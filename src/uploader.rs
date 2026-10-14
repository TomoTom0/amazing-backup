//! S3転送統括(HeadObject skip判定・PutObject(multipart)・README配置・dry-run。
//! 設計5章uploader.rs・2.3)。S3 API呼び出しは`S3Backend` trait(src/s3.rs)経由
//! (TASK-9 AWS SDK移行でaws CLI(subprocess)呼び出しを全面置換)。

use crate::config::AwsConfig;
use crate::errors::{BackupError, BackupResult};
use crate::registry::UploadRecord;
use crate::s3::{HeadInfo, S3Backend};
use std::path::{Path, PathBuf};

/// skip判定(2.3-3の行列)。bucket一致の前回成功記録がある対象はpending列挙
/// (`pending_uploads_for_bucket`)の時点で除外されるため、判定に流入する対象は
/// 常に前回成功記録なし(クラッシュ再開ケース)で、照合はhead Sizeのみ行う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipDecision {
    /// 行1: head 404(NotFound) → upload実行
    Upload,
    /// 行2: head Size一致 → skip + 「sizeのみ検証」警告
    SkipWithSizeOnlyWarn,
    /// 行3: head Size不一致 → 再upload(上書き)
    Reupload,
}

/// 2.3-3判定表(3行)の実装。local_sizeはローカル`.age`のsize。
/// 比較対象の前回成功記録は判定に流入しないためhead `ChecksumSHA256`との照合は
/// 行わない(`s3_checksum_sha256`記録は4.1のとおりupload成功時のS3側実測値として保持)。
pub fn decide_skip(local_size: u64, head: Option<&HeadInfo>) -> SkipDecision {
    let Some(head) = head else {
        return SkipDecision::Upload; // 行1: head 404
    };
    let size_match = head.size >= 0 && head.size as u64 == local_size;
    if size_match {
        SkipDecision::SkipWithSizeOnlyWarn // 行2
    } else {
        SkipDecision::Reupload // 行3
    }
}

/// upload成功時の記録用実測値(HeadObject取得)。
/// 実効bucket/region(4.1の`s3_bucket`/`s3_region`記録元)を含む。
#[derive(Debug)]
pub struct UploadedInfo {
    /// 当該実行の実効bucket(2.3のCLI一時上書き適用後の値)
    pub s3_bucket: String,
    /// 同じく実効region
    pub s3_region: String,
    pub s3_size: i64,
    pub s3_checksum_sha256: String,
    pub s3_checksum_type: String,
}

/// PutObject(multipart判定はBackend側)実行 + 成功後`head`で実測記録を取得する
/// (2.3-4・2.3-5)。storage_class・checksum_algorithm(SHA256)指定はBackendが担う。
pub fn upload_object(
    backend: &dyn S3Backend,
    cfg: &AwsConfig,
    local_path: &Path,
    key: &str,
) -> BackupResult<UploadedInfo> {
    backend.put(&cfg.bucket, key, local_path, &cfg.storage_class)?;
    // 成功後HeadObjectでS3側実測値を取得(7.4。multipartのCOMPOSITE対応)
    let head = backend.head(&cfg.bucket, key)?.ok_or_else(|| {
        BackupError::Transfer(format!(
            "upload成功後にHeadObjectで対象が見つからない(key={key})"
        ))
    })?;
    // head応答にChecksumSHA256が無い場合は空文字でなく"UNKNOWN"マーカーを記録し警告する
    // (空文字では「照合不能」と「値無し」の区別がつかず、skip判定が黙って再uploadに傾く)
    let checksum_sha256 = match head.checksum_sha256 {
        Some(c) => c,
        None => {
            eprintln!(
                "WARNING: HeadObject応答にChecksumSHA256が含まれない(key={key})。registry記録はUNKNOWNとする"
            );
            "UNKNOWN".to_string()
        }
    };
    Ok(UploadedInfo {
        s3_bucket: cfg.bucket.clone(),
        s3_region: cfg.region.clone(),
        s3_size: head.size,
        s3_checksum_sha256: checksum_sha256,
        s3_checksum_type: head.checksum_type.unwrap_or_else(|| "UNKNOWN".to_string()),
    })
}

/// upload統括の実行options(2.3)。
#[derive(Debug)]
pub struct UploadOptions {
    /// 実効aws設定(cmd_uploadが「load_aws読込 → require_ready(toml値の正検査+credential
    /// ファイル検証) → CLI一時上書き適用」の順でmergeした値。5章cli.rs・2.3 TASK-6節)
    pub aws: AwsConfig,
    /// aws.toml正本のbucket値(記録`s3_bucket`欠損の既存記録互換判定のみに使用。4.1)
    pub canonical_bucket: String,
    pub registry_path: PathBuf,
    /// `--date`指定時は当該batch_date分に限定
    pub date: Option<String>,
    /// HeadObject/PutObjectを実行せず実行予定API呼び出しとskip想定を表示
    pub dry_run: bool,
    /// per-object失敗でも継続
    pub keep_going: bool,
}

/// 列挙→判定→実行→registry追記の統括(2.3)。戻り値はexit code。
/// aws設定の事前検査・CLI一時上書きのmergeはcmd_uploadの担当(5章cli.rs)であり、
/// この関数はmerge済みの実効設定を`opts.aws`で受ける。
pub fn run_upload(backend: &dyn S3Backend, opts: &UploadOptions) -> i32 {
    let cfg = &opts.aws;

    let records = match crate::registry::load_registry(&opts.registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    let reduced = crate::registry::reduce_records(&records);
    // pending列挙は「記録のs3_bucket == 当該実行の実効bucket」条件付き(2.3-2・4.1)
    let mut pending = crate::registry::pending_uploads_for_bucket(
        &reduced,
        opts.date.as_deref(),
        &cfg.bucket,
        &opts.canonical_bucket,
    );
    // 前回成功記録で除外された対象もS3側objectの実在をheadで確認し、削除済み(404)
    // のものは再upload対象へ戻す(記録は前回成功の事実であり、S3側で削除された対象は
    // 再送が必要。ローカル実ファイルが既に無い対象は再送不能のため対象外)。
    // dry-runはS3 API呼び出しを行わないためこの確認も行わない(skip想定未確定のまま表示)。
    if !opts.dry_run {
        let candidates = crate::registry::upload_candidates(&reduced, opts.date.as_deref());
        match revive_deleted_objects(backend, cfg, &candidates, &pending) {
            Ok(revived) => {
                if !revived.is_empty() {
                    pending.extend(revived);
                    // 並び順の維持: 対象群(s3_key昇順) → manifest(s3_key昇順)(2.3-2)
                    let mut objects: Vec<_> = pending
                        .iter()
                        .filter(|p| matches!(p.kind, crate::registry::PendingKind::Object { .. }))
                        .cloned()
                        .collect();
                    let mut manifests: Vec<_> = pending
                        .iter()
                        .filter(|p| matches!(p.kind, crate::registry::PendingKind::Manifest { .. }))
                        .cloned()
                        .collect();
                    objects.sort_by(|a, b| a.s3_key.cmp(&b.s3_key));
                    manifests.sort_by(|a, b| a.s3_key.cmp(&b.s3_key));
                    objects.extend(manifests);
                    pending = objects;
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                return e.exit_code();
            }
        }
    }
    // README配置判定対象(2.3-7)。pending空の実行でも配置判定は実行するため
    // 早期returnの前に算出する(7.3のREADME配置前crashリカバリ経路)
    let readme_dates = readme_batch_dates(&reduced, opts.date.as_deref());

    if opts.dry_run {
        for line in dry_run_display_lines(cfg, &pending, &readme_dates) {
            println!("{line}");
        }
        let readme_note = if readme_dates.is_empty() {
            String::new()
        } else {
            format!(" + README配置予定{}件", readme_dates.len())
        };
        println!(
            "-- summary (dry-run): {} objects{readme_note} (HeadObject未実行のためskip想定は未確定・全件upload想定)",
            pending.len()
        );
        return 0;
    }

    let mut uploaded = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut first_err: Option<i32> = None;
    // README配置分は実行summaryでオブジェクト数と区別して数える(2.3-7)
    let mut readme_placed = 0usize;
    let mut readme_skipped = 0usize;
    let mut readme_failed = 0usize;
    if pending.is_empty() {
        println!("upload対象なし(全オブジェクトupload済み、または対象が存在しません)");
    }
    for p in &pending {
        match upload_pending(backend, &cfg, &opts.registry_path, p) {
            Ok(true) => {
                uploaded += 1;
                println!("ok    {}", p.s3_key);
            }
            Ok(false) => {
                skipped += 1;
                println!("skip  {}", p.s3_key);
            }
            Err(e) => {
                failed += 1;
                eprintln!("error: {e}");
                if first_err.is_none() {
                    first_err = Some(e.exit_code());
                }
                if !opts.keep_going {
                    println!("{}", run_summary_line(uploaded, skipped, failed, readme_placed, readme_skipped, readme_failed));
                    return e.exit_code();
                }
            }
        }
    }

    // 各バッチプレフィックス(<date>/)へ平文READMEを配置する(2.3-7・2.6・TASK-8)。
    // manifestより後に配置するため「READMEが存在するプレフィックス=当該バッチの
    // upload完了」の目印を兼ねる(fail-fastで完了した実行の場合の目印であり、
    // --keep-goingで一部オブジェクトが失敗した実行ではこの保証はない)。
    // pending空の実行でも判定は実行し(7.3)、配置でregistryレコードは作らない(4.1注記)。
    // 一時fileは初回配置時に書き出し、このscopeを抜ける時点(Drop)で削除する。
    let mut readme_tmp: Option<ReadmeTempFile> = None;
    for date in &readme_dates {
        let key = format!("{date}/{RESTORE_README_NAME}");
        match place_restore_readme(backend, cfg, &mut readme_tmp, &key) {
            Ok(true) => {
                readme_placed += 1;
                println!("ok    {key}");
            }
            Ok(false) => {
                readme_skipped += 1;
                println!("skip  {key}");
            }
            Err(e) => {
                readme_failed += 1;
                eprintln!("error: {e}");
                if first_err.is_none() {
                    first_err = Some(e.exit_code());
                }
                // 送り元一時fileの書込失敗等の設定・環境エラーは、retryしても成功
                // しないローカル環境の問題のため--keep-goingでも即終了する
                // (7.2「設定エラーは常に即中止」・2.3-7)
                if matches!(e, BackupError::Config(_)) || !opts.keep_going {
                    println!("{}", run_summary_line(uploaded, skipped, failed, readme_placed, readme_skipped, readme_failed));
                    return e.exit_code();
                }
            }
        }
    }
    println!("{}", run_summary_line(uploaded, skipped, failed, readme_placed, readme_skipped, readme_failed));
    first_err.unwrap_or(0)
}

/// 前回成功記録でpending列挙から除外された候補のうち、S3側objectが削除済み
/// (head 404)のものを再upload対象として返す(候補からpendingに既に含まれるものと
/// ローカル実ファイルが不在のものは対象外)。
fn revive_deleted_objects(
    backend: &dyn S3Backend,
    cfg: &AwsConfig,
    candidates: &[crate::registry::PendingUpload],
    pending: &[crate::registry::PendingUpload],
) -> BackupResult<Vec<crate::registry::PendingUpload>> {
    let pending_keys: std::collections::HashSet<&str> =
        pending.iter().map(|p| p.s3_key.as_str()).collect();
    let mut revived = Vec::new();
    for c in candidates {
        if pending_keys.contains(c.s3_key.as_str()) {
            continue; // 未成功(初回・クラッシュ再開)のため既にupload対象
        }
        if !c.local_path.is_file() {
            continue; // ローカル実ファイル不在=再送不能のため記録を正とする
        }
        if backend.head(&cfg.bucket, &c.s3_key)?.is_none() {
            revived.push(c.clone());
        }
    }
    Ok(revived)
}

/// 実行summary行(2.3・2.3-7)。README配置分はオブジェクト数に混入させず
/// 「+ README配置 <n>件」形式で分離表示する(dry-run summaryとの粒度一致。
/// README配置対象の無い実行ではREADME部分を付加しない)。
fn run_summary_line(
    uploaded: usize,
    skipped: usize,
    failed: usize,
    readme_placed: usize,
    readme_skipped: usize,
    readme_failed: usize,
) -> String {
    let readme_note = if readme_placed + readme_skipped + readme_failed == 0 {
        String::new()
    } else {
        format!(" + README配置 {readme_placed}件(skip {readme_skipped}・fail {readme_failed})")
    };
    format!("-- summary: {uploaded} uploaded, {skipped} skipped, {failed} failed{readme_note}")
}

/// pending 1件の処理。Ok(true)=upload実行、Ok(false)=skip、Err=失敗。
fn upload_pending(
    backend: &dyn S3Backend,
    cfg: &AwsConfig,
    registry_path: &Path,
    p: &crate::registry::PendingUpload,
) -> BackupResult<bool> {
    // 対象の実ファイル不在は対象エラー3(2.3-2)
    let meta = std::fs::metadata(&p.local_path).map_err(|e| {
        BackupError::Target(format!(
            "registryが指すファイルが存在しない ({}): {e}",
            p.local_path.display()
        ))
    })?;
    if !meta.is_file() {
        return Err(BackupError::Target(format!(
            "registryが指すpathが通常fileでない ({})",
            p.local_path.display()
        )));
    }
    let local_size = meta.len();
    let head = backend.head(&cfg.bucket, &p.s3_key)?;

    // skip判定は2.3-3の行列どおり(前回成功記録はpending列挙で除外済みのため
    // 照合はhead Sizeのみ。manifestも同一行列)
    let decision = decide_skip(local_size, head.as_ref());
    match decision {
        SkipDecision::SkipWithSizeOnlyWarn => {
            eprintln!(
                "WARNING: {} はsizeのみ検証でskipします(前回成功upload記録がない=クラッシュ再開ケース)",
                p.s3_key
            );
            Ok(false)
        }
        SkipDecision::Upload | SkipDecision::Reupload => {
            let info = upload_object(backend, cfg, &p.local_path, &p.s3_key)?;
            let now = crate::targets::format_jst(&crate::targets::now_jst());
            // upload/manifest_upload記録には当該実行の実効bucket/regionを
            // s3_bucket/s3_regionとして記録する(4.1)
            match &p.kind {
                crate::registry::PendingKind::Object { .. } => {
                    let rec = UploadRecord {
                        schema: 1,
                        record_type: "upload".into(),
                        recorded_at: now.clone(),
                        batch_id: p.batch_id.clone(),
                        age_path: p.local_path.display().to_string(),
                        s3_key: p.s3_key.clone(),
                        s3_bucket: info.s3_bucket.clone(),
                        s3_region: info.s3_region.clone(),
                        s3_size: info.s3_size,
                        s3_checksum_sha256: info.s3_checksum_sha256,
                        s3_checksum_type: info.s3_checksum_type,
                        uploaded_at: now,
                    };
                    crate::registry::append_record(registry_path, &rec)?;
                }
                crate::registry::PendingKind::Manifest { manifest_path } => {
                    let rec = crate::registry::ManifestUploadRecord {
                        schema: 1,
                        record_type: "manifest_upload".into(),
                        recorded_at: now.clone(),
                        batch_id: p.batch_id.clone(),
                        batch_date: p.batch_date.clone(),
                        manifest_path: manifest_path.clone(),
                        s3_key: p.s3_key.clone(),
                        s3_bucket: info.s3_bucket,
                        s3_region: info.s3_region,
                        s3_size: info.s3_size,
                        s3_checksum_sha256: info.s3_checksum_sha256,
                        s3_checksum_type: info.s3_checksum_type,
                        uploaded_at: now,
                    };
                    crate::registry::append_record(registry_path, &rec)?;
                }
            }
            Ok(true)
        }
    }
}

// ---------------------------------------------------------------------------
// RESTORE_README.txt配置(2.3-7・2.6・TASK-8)

/// READMEのS3キーのファイル名部分(2.6。bucket一覧で気づきやすい大文字命名)。
const RESTORE_README_NAME: &str = "RESTORE_README.txt";

/// README配置判定対象のbatch_date全集(2.3-7)。reduce後encrypt記録の`manifest_path`が
/// 持つ`batch_date`プレフィックスの全集(`--date`指定時は当該日付のみ)。昇順・重複除去。
/// pending列挙(2.3-2)へ流出した対象が属する`batch_date`は常にこの集合に含まれる。
fn readme_batch_dates(reduced: &crate::registry::Reduced, date: Option<&str>) -> Vec<String> {
    let mut dates: Vec<String> = reduced
        .encrypts
        .iter()
        .filter(|e| date.map(|d| e.batch_date == d).unwrap_or(true))
        .map(|e| e.batch_date.clone())
        .collect();
    dates.sort();
    dates.dedup();
    dates
}

/// READMEテンプレートをcpの送り元として書き出す一時fileのpath(実行時)。
/// templateはbin埋め込みのため実fileが必要(2.3-7)。実行ごとに同一pathへ
/// `create_new`(O_EXCL)で新規作成する(pathが既に存在する場合は書き換えず
/// 設定・環境エラー2。前回実行のcrash残存・第三者によるsymlink事前配置の
/// いずれも、symlink追従での意図しないfile破壊を起こさない)。
fn readme_temp_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "backup-remote-{}.{RESTORE_README_NAME}",
        std::process::id()
    ))
}

/// 書き出したREADME一時file。Dropで削除する(fail-fastの早期return経路でも残さない)。
#[derive(Debug)]
struct ReadmeTempFile(PathBuf);

impl ReadmeTempFile {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ReadmeTempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// READMEテンプレートを一時fileへ書き出す(配置が必要になった時点で初回のみ呼ぶ)。
fn write_readme_temp() -> BackupResult<ReadmeTempFile> {
    write_readme_temp_at(&readme_temp_path())
}

/// `path`へREADMEテンプレートを`create_new`(O_EXCL)で書き出す。pathが既に存在
/// する場合(第三者によるfile・symlinkの事前配置、前回実行のcrash残存)は
/// リトライせず設定・環境エラー2とする(2.3-7。予測可能pathへのsymlinkを経由
/// した標的file破壊の攻撃耐性。送り元は非機密の固定文面だが同一耐性を入れる。
/// 失敗はローカル環境の問題のため転送エラーでなく設定・環境エラー)。
fn write_readme_temp_at(path: &Path) -> BackupResult<ReadmeTempFile> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            BackupError::Config(format!(
                "READMEテンプレートの一時file作成に失敗 ({}): {e}",
                path.display()
            ))
        })?;
    file.write_all(crate::RESTORE_README_TEMPLATE.as_bytes())
        .map_err(|e| {
            BackupError::Config(format!(
                "READMEテンプレートの一時file書込に失敗 ({}): {e}",
                path.display()
            ))
        })?;
    Ok(ReadmeTempFile(path.to_path_buf()))
}

/// README 1バッチ分の配置判定と配置(2.3-7)。Ok(true)=配置(再配置)実行・
/// Ok(false)=head `Size`一致でskip・Err=失敗(配置PutObject失敗は転送エラー5。
/// 送り元一時fileの作成・書込失敗は設定・環境エラー2)。
/// READMEはregistryレコードを作らない(2.6・4.1注記)ため冪等判定はhead `Size`のみ
/// (2.3-3と同一の3行構造)。`readme_tmp`は一時fileのlazy書き出し用(呼び出し側で保持)。
/// 配置PutObjectのstorage_classは設定`[aws].storage_class`・CLI一時上書き
/// `--storage-class`のいずれにもよらず`STANDARD`を常に明示する(README本文の取得に
/// Glacier復元jobを要する鶏卵問題の回避。2.6)。
fn place_restore_readme(
    backend: &dyn S3Backend,
    cfg: &AwsConfig,
    readme_tmp: &mut Option<ReadmeTempFile>,
    key: &str,
) -> BackupResult<bool> {
    // 配置判定はregistry記録を参照せず毎回HeadObjectで行う(2.3-7)
    let local_size = crate::RESTORE_README_TEMPLATE.len() as u64;
    let head = backend.head(&cfg.bucket, key)?;
    match decide_skip(local_size, head.as_ref()) {
        SkipDecision::SkipWithSizeOnlyWarn => {
            eprintln!(
                "WARNING: {key} はsizeのみ検証でskipします(README配置の冪等skip・registry記録は持たない)"
            );
            Ok(false)
        }
        SkipDecision::Upload | SkipDecision::Reupload => {
            if readme_tmp.is_none() {
                *readme_tmp = Some(write_readme_temp()?);
            }
            let tmp_path = readme_tmp
                .as_ref()
                .map(|t| t.path().to_path_buf())
                .unwrap_or_else(|| readme_temp_path());
            backend.put(&cfg.bucket, key, &tmp_path, "STANDARD")?;
            Ok(true)
        }
    }
}

/// dry-run表示のPutObject予定行(設計10.1のAPI形式)。
/// 64MiB超対象はmultipart予定の注記を付ける(Backend側判定と同一閾値)。
fn dry_run_put_line(bucket: &str, key: &str, src: &Path, storage_class: &str) -> String {
    let multipart_note = match std::fs::metadata(src) {
        Ok(m) if m.len() > crate::s3::MULTIPART_THRESHOLD_BYTES => {
            format!(" [multipart: 64MiB x {}]", crate::s3::multipart_part_count(m.len()))
        }
        _ => String::new(),
    };
    format!(
        "[dry-run] s3 PutObject bucket={bucket} key={key} src={} storage_class={storage_class} checksum=SHA256{multipart_note}",
        src.display()
    )
}

/// dry-run表示行の組立(UPL-008・2.3-7)。pending各対象のPutObject予定行(実ファイル
/// 不在は「対象エラー想定」の注記)に加え、README配置予定行(STANDARD固定)を
/// 他オブジェクトと同形式で含める。配置判定のHeadObjectも実行しないため全件配置想定。
fn dry_run_display_lines(
    cfg: &AwsConfig,
    pending: &[crate::registry::PendingUpload],
    readme_dates: &[String],
) -> Vec<String> {
    let mut lines = Vec::new();
    for p in pending {
        // dry-runでも実ファイルの存在を検査し、不在対象はPutObject予定の代わりに
        // 「対象エラー想定」の注記を表示する(2.3。実行時に対象エラー3になる事前通知)
        if !p.local_path.is_file() {
            lines.push(format!(
                "[dry-run] 対象エラー想定: {} (registryが指す実ファイルが不在: {})",
                p.s3_key,
                p.local_path.display()
            ));
            continue;
        }
        lines.push(dry_run_put_line(&cfg.bucket, &p.s3_key, &p.local_path, &cfg.storage_class));
    }
    // README配置予定(2.3-7): 送り元は実行時に書き出す一時file
    let tmp = readme_temp_path();
    for date in readme_dates {
        let key = format!("{date}/{RESTORE_README_NAME}");
        lines.push(dry_run_put_line(&cfg.bucket, &key, &tmp, "STANDARD"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{EncryptRecord, ManifestUploadRecord};
    use crate::s3::FakeS3;
    use crate::testutil::{write_file, TempDir};
    use base64::Engine as _;

    fn sha256_b64(data: &[u8]) -> String {
        use sha2::Digest;
        let d = sha2::Sha256::digest(data);
        base64::engine::general_purpose::STANDARD.encode(d)
    }

    fn head(size: i64, checksum: Option<&str>) -> HeadInfo {
        HeadInfo {
            size,
            checksum_sha256: checksum.map(|s| s.to_string()),
            checksum_type: Some("FULL_OBJECT".into()),
            restore: None,
        }
    }

    /// [covers:upload.head-404-proceeds]
    /// HeadObject 404(NotFound・Keyなし)はupload実行に進む(行列行1)。
    #[test]
    fn head_404_proceeds_to_upload() {
        assert_eq!(decide_skip(140, None), SkipDecision::Upload);
    }

    /// [covers:upload.head-match-skip]
    /// headのSizeがローカル`.age`と一致 → skip(行列行2・sizeのみ検証)。
    /// bucket一致の前回成功記録がある対象はpending列挙で除外されるため、判定に
    /// 流入するのは前回記録なし(クラッシュ再開)ケースのみ。
    #[test]
    fn head_size_match_skips() {
        let h = head(140, Some("checksumAAA"));
        assert_eq!(decide_skip(140, Some(&h)), SkipDecision::SkipWithSizeOnlyWarn);
    }

    /// [covers:upload.skip-decision-matrix]
    /// 判定行列3行(404 × head Size一致)が2.3-3の仕様どおり。前回成功記録は
    /// pending列挙で除外されるためchecksum照合は行わない。
    #[test]
    fn skip_decision_matrix_rows() {
        // 行1: 404 → upload
        assert_eq!(decide_skip(140, None), SkipDecision::Upload);

        // 行2: Size一致 → skip + sizeのみ検証警告
        let h = head(140, Some("irrelevant"));
        assert_eq!(decide_skip(140, Some(&h)), SkipDecision::SkipWithSizeOnlyWarn);

        // 行3: Size不一致 → 再upload
        let h = head(139, Some("irrelevant"));
        assert_eq!(decide_skip(140, Some(&h)), SkipDecision::Reupload);
    }

    fn aws_cfg() -> AwsConfig {
        AwsConfig {
            bucket: "test-bucket".into(),
            region: "ap-northeast-1".into(),
            credentials_file: "configs/secret/aws-credentials.toml".into(),
            storage_class: "DEEP_ARCHIVE".into(),
        }
    }

    /// FakeS3の呼び出し記録からPutObject行のみを取り出す。
    fn put_calls(fake: &FakeS3) -> Vec<String> {
        fake.calls()
            .lines()
            .filter(|l| l.starts_with("s3 PutObject"))
            .map(str::to_string)
            .collect()
    }

    /// [covers:upload.cp-command-options]
    /// PutObject呼び出しにstorage_class(設定値)とchecksum_algorithm SHA256が
    /// 指定される(bucketは呼び出しのbucket参数。SDKはURIでなくbucket/key参数)。
    #[test]
    fn put_call_includes_required_options() {
        let tmp = TempDir::new("upl-cpopts");
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);
        let local = tmp.path().join("a.zip.age");
        write_file(&local, b"age-bytes");

        let info = upload_object(&fake, &aws_cfg(), &local, "2026-10-01/a.zip.age")
            .expect("upload成功");
        let puts = put_calls(&fake);
        assert_eq!(puts.len(), 1, "PutObject 1回(成功後HeadObject 1回): {}", fake.calls());
        let put = &puts[0];
        assert!(put.contains("bucket=test-bucket"), "bucket参数: {put}");
        assert!(put.contains("key=2026-10-01/a.zip.age"), "key参数: {put}");
        assert!(put.contains("storage_class=DEEP_ARCHIVE"), "storage_class指定: {put}");
        assert!(put.contains("checksum=SHA256"), "checksum_algorithm SHA256: {put}");
        assert!(
            fake.calls()
                .lines()
                .any(|l| l.starts_with("s3 HeadObject") && l.contains("key=2026-10-01/a.zip.age")),
            "成功後のHeadObject: {}",
            fake.calls()
        );

        // 成功後HeadObjectの実測値が返る(fakeはfileのsha256を返す)。
        // UploadedInfoには実効bucket/regionも含まれる(4.1のs3_bucket/s3_region記録元)
        assert_eq!(info.s3_bucket, "test-bucket");
        assert_eq!(info.s3_region, "ap-northeast-1");
        assert_eq!(info.s3_size, b"age-bytes".len() as i64);
        assert_eq!(info.s3_checksum_sha256, sha256_b64(b"age-bytes"));
        assert_eq!(info.s3_checksum_type, "FULL_OBJECT");
    }

    /// registry + .age + manifestのfixture。戻り値はregistry path。
    fn fixture_batch(tmp: &Path, names: &[&str]) -> PathBuf {
        let date = "2026-10-01";
        let out = tmp.join("out").join(date);
        let manifest_path = out.join("manifest.jsonl.age");
        write_file(&manifest_path, b"manifest-age-bytes");
        let mut lines = String::new();
        for name in names {
            let age = out.join(format!("{name}.age"));
            write_file(&age, format!("age-of-{name}").as_bytes());
            let rec = EncryptRecord {
                schema: 1,
                record_type: "encrypt".into(),
                recorded_at: "2026-10-01T10:00:00+09:00".into(),
                batch_id: "20261001-abcdef01".into(),
                batch_date: date.into(),
                source_path: format!("/data/{name}"),
                source_name: name.to_string(),
                source_size: 10,
                source_mtime: "2026-09-30T09:00:00+09:00".into(),
                source_sha256: "a".repeat(64),
                age_path: age.display().to_string(),
                age_size: format!("age-of-{name}").len() as u64,
                age_sha256: "b".repeat(64),
                recipients: vec!["SHA256:testfingerprint".into()],
                manifest_path: manifest_path.display().to_string(),
            };
            lines.push_str(&serde_json::to_string(&rec).unwrap());
            lines.push('\n');
        }
        let registry = tmp.join("registry.jsonl");
        write_file(&registry, lines.as_bytes());
        registry
    }

    fn opts(registry: &Path) -> UploadOptions {
        // run_uploadはcmd_uploadでmerge済みの実効設定を受ける(5章cli.rs)。
        // unit testでは実効値を直接構成する(正本bucket = 実効bucket)
        let aws = aws_cfg();
        UploadOptions {
            canonical_bucket: aws.bucket.clone(),
            aws,
            registry_path: registry.to_path_buf(),
            date: None,
            dry_run: false,
            keep_going: false,
        }
    }

    fn registry_lines(path: &Path) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(path).expect("registry read");
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("registry line JSON"))
            .collect()
    }

    /// [covers:upload.s3-key-layout-and-manifest-last]
    /// S3キーは<batch_date>/<source_name>.age・manifestは<batch_date>/manifest.jsonl.ageで
    /// manifestのPutObjectがオブジェクト群より後に行われる。README配置(2.3-7)は
    /// さらにその後(put呼び出しのstorage_class=STANDARD固定)。
    #[test]
    fn s3_key_layout_and_manifest_uploaded_last() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-layout");
        let registry = fixture_batch(tmp.path(), &["b.zip", "a.7z"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);

        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 0, "全upload成功");
        let puts = put_calls(&fake);
        assert_eq!(puts.len(), 4, "オブジェクト2 + manifest 1 + README 1: {}", fake.calls());
        assert!(puts[0].contains("key=2026-10-01/a.7z.age"), "s3_key昇順: {}", puts[0]);
        assert!(puts[1].contains("key=2026-10-01/b.zip.age"), "s3_key昇順: {}", puts[1]);
        assert!(puts[2].contains("key=2026-10-01/manifest.jsonl.age"), "manifestはオブジェクトより後: {}", puts[2]);
        assert!(
            puts[3].contains("key=2026-10-01/RESTORE_README.txt")
                && puts[3].contains("storage_class=STANDARD"),
            "READMEはmanifestより後にSTANDARD固定で配置: {}",
            puts[3]
        );
        // simulator上のobject配置も確認
        assert_eq!(fake.object("2026-10-01/a.7z.age").unwrap(), b"age-of-a.7z".to_vec());
        assert_eq!(fake.object("2026-10-01/manifest.jsonl.age").unwrap(), b"manifest-age-bytes".to_vec());
        // README objectは埋め込みtemplateとbyte一致(完全固定文面)
        assert_eq!(
            fake.object("2026-10-01/RESTORE_README.txt").unwrap(),
            crate::RESTORE_README_TEMPLATE.as_bytes()
        );
    }

    /// [covers:upload.success-record-append]
    /// 成功後registryへupload記録(HeadObject実測値)とmanifest_upload記録がappendされる。
    #[test]
    fn success_records_appended() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-rec");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);

        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 0);
        let lines = registry_lines(&registry);
        assert_eq!(lines.len(), 3, "encrypt 1 + upload 1 + manifest_upload 1");

        let up = &lines[1];
        assert_eq!(up["record_type"], "upload");
        assert_eq!(up["s3_key"], "2026-10-01/a.zip.age");
        assert_eq!(up["s3_size"].as_i64(), Some("age-of-a.zip".len() as i64));
        let want_sum = sha256_b64(b"age-of-a.zip");
        assert_eq!(up["s3_checksum_sha256"].as_str(), Some(want_sum.as_str()));
        assert_eq!(up["s3_checksum_type"], "FULL_OBJECT");
        // 4.1: 実効bucket/regionをs3_bucket/s3_regionとして記録
        assert_eq!(up["s3_bucket"], "test-bucket");
        assert_eq!(up["s3_region"], "ap-northeast-1");
        assert!(up["uploaded_at"].as_str().unwrap().ends_with("+09:00"));

        let mu = &lines[2];
        assert_eq!(mu["record_type"], "manifest_upload");
        assert_eq!(mu["s3_key"], "2026-10-01/manifest.jsonl.age");
        let want_msum = sha256_b64(b"manifest-age-bytes");
        assert_eq!(mu["s3_checksum_sha256"].as_str(), Some(want_msum.as_str()));
        assert_eq!(mu["s3_bucket"], "test-bucket");
        assert_eq!(mu["s3_region"], "ap-northeast-1");
    }

    /// [covers:upload.dry-run-no-aws-exec]
    /// --dry-runはS3 API呼び出し(HeadObject/PutObject)を一切実行せずexit 0。
    /// 表示行(UPL-008)は対象のPutObject予定に加えてREADME配置予定(2.3-7)を含み、
    /// README行は他オブジェクトと同形式でstorage_class=STANDARD固定。
    #[test]
    fn dry_run_executes_nothing() {
        let tmp = TempDir::new("upl-dry");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);

        let mut o = opts(&registry);
        o.dry_run = true;
        let code = run_upload(&fake, &o);
        assert_eq!(code, 0, "dry-runはexit 0");
        assert_eq!(fake.calls(), "", "S3 API呼び出しは一切起きない: {}", fake.calls());
        assert_eq!(registry_lines(&registry).len(), 1, "registryへの追記も無い");

        // dry-run表示行: PutObject予定(対象+manifest)とREADME配置予定を含む(2.3-7・UPL-008)
        let records = crate::registry::load_registry(&registry).expect("registry読込");
        let reduced = crate::registry::reduce_records(&records);
        let aws = aws_cfg();
        let pending =
            crate::registry::pending_uploads_for_bucket(&reduced, None, &aws.bucket, &aws.bucket);
        let dates = readme_batch_dates(&reduced, None);
        let lines = dry_run_display_lines(&aws, &pending, &dates);
        let put_planned: Vec<&String> =
            lines.iter().filter(|l| l.contains("s3 PutObject")).collect();
        assert_eq!(put_planned.len(), 3, "対象1 + manifest 1 + README 1: {lines:?}");
        let readme_line = put_planned
            .iter()
            .find(|l| l.contains("RESTORE_README.txt"))
            .expect("README配置予定行");
        assert!(
            readme_line.starts_with("[dry-run] s3 PutObject bucket=test-bucket")
                && readme_line.contains("key=2026-10-01/RESTORE_README.txt")
                && readme_line.contains("storage_class=STANDARD")
                && readme_line.contains("checksum=SHA256"),
            "README配置予定は同形式でSTANDARD固定: {readme_line}"
        );
    }

    /// [covers:upload.cp-failure-transfer-error]
    /// PutObject失敗は転送エラー5。messageは操作名・bucket・keyを含み
    /// credential値を含まない(SDK error写像と同一形式。失敗注入はFakeS3)。
    #[test]
    fn put_failure_is_transfer_error() {
        let tmp = TempDir::new("upl-cpfail");
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state).fail_cp("2026-10-01/a.zip.age");
        let local = tmp.path().join("a.zip.age");
        write_file(&local, b"age-bytes");

        let e = upload_object(&fake, &aws_cfg(), &local, "2026-10-01/a.zip.age").unwrap_err();
        match &e {
            crate::errors::BackupError::Transfer(msg) => {
                assert!(msg.contains("PutObject"), "操作名: {msg}");
                assert!(msg.contains("bucket=test-bucket"), "bucket: {msg}");
                assert!(msg.contains("key=2026-10-01/a.zip.age"), "key: {msg}");
            }
            other => panic!("期待した区分はTransfer、実際は {other:?}"),
        }
        assert_eq!(e.exit_code(), 5);
    }

    /// [covers:upload.missing-checksum-unknown-marker]
    /// upload成功後のhead応答にChecksumSHA256が無い場合、s3_checksum_sha256は
    /// 空文字でなく"UNKNOWN"マーカーとしてregistryに記録される。
    #[test]
    fn missing_checksum_records_unknown_marker() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-unknown");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state).omitting_checksum();

        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 0, "upload自体は成功");
        let lines = registry_lines(&registry);
        let uploads: Vec<&serde_json::Value> =
            lines.iter().filter(|l| l["record_type"] == "upload").collect();
        assert_eq!(uploads.len(), 1);
        assert_eq!(
            uploads[0]["s3_checksum_sha256"],
            serde_json::json!("UNKNOWN"),
            "ChecksumSHA256無し応答はUNKNOWNマーカー記録(空文字でない): {:?}",
            uploads[0]
        );
        let mus: Vec<&serde_json::Value> =
            lines.iter().filter(|l| l["record_type"] == "manifest_upload").collect();
        assert_eq!(mus.len(), 1);
        assert_eq!(mus[0]["s3_checksum_sha256"], serde_json::json!("UNKNOWN"));
    }

    /// [covers:upload.missing-age-target-error]
    /// registryが指す対象.ageが不在の場合は対象エラー3。
    #[test]
    fn missing_age_file_is_target_error() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-miss");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        // encrypt記録が指す.ageを削除(manifestは残す)
        std::fs::remove_file(tmp.path().join("out/2026-10-01/a.zip.age")).expect("remove .age");
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);

        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 3, "対象エラー");
        assert_eq!(put_calls(&fake).len(), 0, "在らない対象はputしない: {}", fake.calls());
    }

    /// [covers:upload.no-pending-exit-zero]
    /// アップロード対象が無い(全対象upload済み・S3側にも実在)の場合exit 0で「対象なし」。
    /// 前回成功記録のある対象もS3側実在確認のheadは行われるが、実在すればputは再実行
    /// されない(S3側から削除済みの場合は再upload対象へ戻る = tests/s3_backend.rsの
    /// credential値非露出test(c)と同一挙動)。
    #[test]
    fn no_pending_exits_zero() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-none");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);

        // 1回目: 全対象upload(記録append・S3側object配置)
        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 0, "1回目はupload成功");
        let registry_after_first = registry_lines(&registry).len();
        // 呼び出し記録を初期化(2回目の呼び出しのみ検証する)
        let _ = std::fs::remove_file(state.join("calls.log"));

        // 2回目: 全対象upload済み → exit 0で「対象なし」
        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 0, "対象なしはexit 0");
        // 前回成功記録のある対象のS3側実在確認headは行われるが、putは再実行されない。
        // pending空でもREADME配置判定のheadは実行され(7.3リカバリ経路)、Size一致でskip
        let calls = fake.calls();
        assert!(
            put_calls(&fake)
                .iter()
                .all(|c| c.contains("RESTORE_README.txt")),
            "対象/manifestのputは再実行しない: {calls}"
        );
        assert!(
            calls.lines().any(|c| c.starts_with("s3 HeadObject")
                && c.contains("key=2026-10-01/a.zip.age")),
            "前回成功記録のある対象もS3側実在確認のheadは実行: {calls}"
        );
        assert!(
            calls.lines().any(|c| c.starts_with("s3 HeadObject")
                && c.contains("key=2026-10-01/RESTORE_README.txt")),
            "pending空でもREADME配置判定のheadは実行: {calls}"
        );
        assert_eq!(
            registry_lines(&registry).len(),
            registry_after_first,
            "2回目の実行でregistry追記なし"
        );
    }

    /// [covers:upload.no-pending-exit-zero]
    /// 前回成功記録があってもS3側objectが削除済み(head 404)の対象は再upload対象へ
    /// 戻る(記録は前回成功の事実であり、S3側で削除された対象は再送が必要)。
    #[test]
    fn s3_deleted_object_is_reuploaded_despite_record() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-revive");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);

        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 0, "1回目はupload成功");
        assert!(fake.object("2026-10-01/a.zip.age").is_some());

        // S3側objectを削除(記録は残る)して再実行 → 再uploadされる
        let _ = std::fs::remove_file(state.join("objects").join("2026-10-01").join("a.zip.age"));
        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 0, "削除済み対象の再uploadでexit 0");
        assert!(
            fake.object("2026-10-01/a.zip.age").is_some(),
            "削除済みobjectは再配置される"
        );
        let calls = fake.calls();
        assert!(
            calls.lines().any(|c| c.starts_with("s3 PutObject")
                && c.contains("key=2026-10-01/a.zip.age")),
            "削除済み対象へのput再実行: {calls}"
        );
    }

    /// [covers:upload.keep-going-continues]
    /// --keep-goingでper-object失敗でも継続し、exitは最初のエラー区分(5)。
    /// 成功分のupload記録とmanifest_upload記録がregistryにappendされる。
    #[test]
    fn keep_going_continues_after_put_failure() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-keep");
        let registry = fixture_batch(tmp.path(), &["a.zip", "b.7z"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state).fail_cp("2026-10-01/a.zip.age");

        let mut o = opts(&registry);
        o.keep_going = true;
        let code = run_upload(&fake, &o);
        assert_eq!(code, 5, "最初のエラー区分は転送エラー");

        let lines = registry_lines(&registry);
        let uploads: Vec<&serde_json::Value> = lines.iter().filter(|l| l["record_type"] == "upload").collect();
        let manifests: Vec<&serde_json::Value> =
            lines.iter().filter(|l| l["record_type"] == "manifest_upload").collect();
        assert_eq!(uploads.len(), 1, "成功分b.7zのみupload記録");
        assert_eq!(uploads[0]["s3_key"], "2026-10-01/b.7z.age");
        assert_eq!(manifests.len(), 1, "manifestは成功扱いで記録");
    }

    /// [covers:config.aws-required-on-upload]
    /// aws.toml不在・bucket/region空でupload実行(cmd_upload)すると設定エラー2。
    /// bucket/region空はCLI一時上書き(--bucket)指定の有無にかかわらず設定エラー2
    /// (CLI上書き値はtoml値が正である前提の上書き)。事前検査はcmd_uploadの
    /// merge順「load_aws読込→require_ready→CLI一時上書き適用」の中で行われるため
    /// run_upload(S3 API呼び出し)には到達せず、S3 API呼び出しは一切行われない。
    #[test]
    fn aws_config_missing_or_empty_is_config_error_without_s3_calls() {
        let tmp = TempDir::new("upl-noaws");
        let _registry = fixture_batch(tmp.path(), &["a.zip"]);
        // cmd_uploadはpipeline.tomlも読む(registryのみ参照すればよい)
        let pubkey = crate::testutil::write_test_ed25519_pub(tmp.path());
        let pipeline = tmp.path().join("pipeline.toml");
        write_file(
            &pipeline,
            format!(
                "[recipients]\npublic_keys = [\"{}\"]\n[output]\ndir = \"{}\"\nregistry = \"{}\"\n",
                pubkey.display(),
                tmp.path().join("out").display(),
                tmp.path().join("registry.jsonl").display(),
            )
            .as_bytes(),
        );
        let creds = tmp.path().join("aws-credentials.toml");
        write_file(
            &creds,
            b"[credentials]\naccess_key_id = \"AKITEST0000000000000A\"\nsecret_access_key = \"unit-test-not-a-real-secret\"\n",
        );
        let args = |aws_cfg_path: std::path::PathBuf, bucket: Option<&str>| crate::cli::UploadArgs {
            config: Some(pipeline.clone()),
            aws_config: Some(aws_cfg_path),
            date: None,
            dry_run: false,
            keep_going: false,
            storage_class: None,
            bucket: bucket.map(|s| s.to_string()),
            region: None,
            aws_profile: None,
            aws_purpose: None,
        };

        // 1. aws.toml不在
        let absent = tmp.path().join("absent-aws.toml");
        assert_eq!(crate::cli::cmd_upload(&args(absent, None)), 2, "aws.toml不在は設定エラー2");

        // 2. bucket/region空(CLIの--bucket指定の有無にかかわらず設定エラー2)
        let empty = tmp.path().join("empty-aws.toml");
        write_file(
            &empty,
            format!("[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"\"\nregion = \"\"\ncredentials_file = \"{}\"\n", creds.display()).as_bytes(),
        );
        assert_eq!(crate::cli::cmd_upload(&args(empty.clone(), None)), 2, "bucket/region空は設定エラー2");
        assert_eq!(
            crate::cli::cmd_upload(&args(empty, Some("cli-bucket"))),
            2,
            "toml空値はCLI上書きで補完しない(正本主義)"
        );
    }

    /// [covers:upload.cli-override-reflected-in-aws-argv]
    /// 実効aws設定(cmd_uploadのCLI一時上書きmerge結果)が当該実行のS3 API呼び出し
    /// (HeadObject・PutObjectの両方)へ反映される(bucketは両呼び出しのbucket参数。
    /// regionはBackend構築(Region::new)へ反映されるためregistry記録で検証)。
    /// upload/manifest_upload記録には実効bucket/regionがs3_bucket/s3_regionとして
    /// 記録される(4.1)。
    #[test]
    fn cli_override_reflected_in_backend_calls_and_records() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-ovargv");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);
        let aws = AwsConfig {
            bucket: "alt-bucket".into(),
            region: "us-east-1".into(),
            credentials_file: "configs/secret/aws-credentials.toml".into(),
            storage_class: "GLACIER".into(),
        };
        let o = UploadOptions {
            canonical_bucket: "test-bucket".into(), // 正本(aws.toml)のbucket
            aws,
            ..opts(&registry)
        };
        let code = run_upload(&fake, &o);
        assert_eq!(code, 0, "上書き実効値でのupload成功");

        let calls = fake.calls();
        for l in calls.lines() {
            if l.starts_with("s3 HeadObject") || l.starts_with("s3 PutObject") {
                assert!(l.contains("bucket=alt-bucket"), "HeadObject/PutObjectのbucketへ反映: {l}");
            }
        }
        let puts = put_calls(&fake);
        // README配置(2.3-7)のput行(STANDARD固定)はUPL-020系で検証するため除外
        let put_no_readme: Vec<&String> =
            puts.iter().filter(|l| !l.contains("RESTORE_README.txt")).collect();
        assert!(!put_no_readme.is_empty(), "対象/manifestのput実行: {puts:?}");
        assert!(put_no_readme.iter().all(|l| l.contains("storage_class=GLACIER")), "putのstorage_class: {puts:?}");
        assert!(
            puts.iter().any(|l| l.contains("RESTORE_README.txt")
                && l.contains("storage_class=STANDARD")
                && l.contains("bucket=alt-bucket")),
            "README putはCLI上書きによらずSTANDARD固定(bucket等は実効値): {puts:?}"
        );

        // registry記録のs3_bucket/s3_regionは当該実行の実効値
        let lines = registry_lines(&registry);
        let recs: Vec<&serde_json::Value> = lines
            .iter()
            .filter(|r| r["record_type"] == "upload" || r["record_type"] == "manifest_upload")
            .collect();
        assert_eq!(recs.len(), 2, "upload 1 + manifest_upload 1");
        for rec in &recs {
            assert_eq!(rec["s3_bucket"], "alt-bucket", "実効bucket記録: {rec}");
            assert_eq!(rec["s3_region"], "us-east-1", "実効region記録: {rec}");
        }
    }

    /// [covers:upload.restore-readme-placement]
    /// 埋め込みtemplate(2.6)は内容要件(バケット目的・age v1形式・入手先URL・
    /// SSH ed25519秘密鍵・復元コマンド例・manifest取得例・sha256sum検証・
    /// backup-remote restoreコマンドによる復元の1行)を満たす完全固定文面で機密を含まない
    /// (実行時埋め込みなし・bucket名等を含まない)。内容要件6のrestore行はTASK-5で追記済み
    /// (RES-021・UPL-020期待値と相互参照)。
    #[test]
    fn restore_readme_template_content_requirements() {
        let t = crate::RESTORE_README_TEMPLATE;
        assert!(!t.trim().is_empty(), "空templateでない");
        assert!(t.contains("バックアップ"), "バケットの目的: {t}");
        assert!(t.contains("age v1"), "age v1形式: {t}");
        assert!(t.contains(".age"), "拡張子.age: {t}");
        assert!(t.contains("https://age-encryption.org"), "age入手先URL: {t}");
        assert!(t.contains("https://github.com/FiloSottile/age"), "age入手先URL(github): {t}");
        assert!(t.contains("SSH"), "SSH ed25519秘密鍵の必要性: {t}");
        assert!(t.contains("ed25519"), "SSH ed25519秘密鍵の必要性: {t}");
        assert!(t.contains("aws s3 cp"), "復元コマンド例(取得): {t}");
        assert!(t.contains("age -d"), "復元コマンド例(復号): {t}");
        assert!(t.contains("manifest.jsonl.age"), "manifest取得例: {t}");
        assert!(t.contains("sha256sum"), "checksum検証手順: {t}");
        assert!(t.contains("source_name"), "manifest.jsonl内の該当レコード特定方法: {t}");
        assert!(t.contains("photo.zip.age"), "<名前>定義の具体例: {t}");
        // 内容要件6(TASK-5実装で追記・RES-021/UPL-020): restoreコマンドの1行
        assert_eq!(
            t.lines().filter(|l| l.contains("backup-remote restore")).count(),
            1,
            "backup-remote restoreコマンドの行は1行のみ: {t}"
        );
        // 実行時埋め込みなし(bucket名等の環境依存値を含まない)
        assert!(!t.contains("test-bucket"), "bucket名を含まない: {t}");
        assert!(!t.contains("it-bucket"), "bucket名を含まない: {t}");
    }

    /// [covers:upload.restore-readme-placement]
    /// README配置判定対象はreduce後encrypt記録のbatch_date全集(昇順・重複除去)で、
    /// --date指定時は当該日付のみ(2.3-7)。
    #[test]
    fn readme_batch_dates_full_set_and_date_filter() {
        let tmp = TempDir::new("upl-rdates");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        // 別batch_date(2026-09-30)のencrypt記録を2件(age_pathは別)追記し、
        // 全集・昇順・重複除去を検証する
        let mut text = std::fs::read_to_string(&registry).unwrap();
        let mut rec: crate::registry::EncryptRecord =
            serde_json::from_str(text.lines().next().unwrap()).expect("encrypt記録parse");
        for n in 0..2 {
            rec.batch_date = "2026-09-30".into();
            rec.age_path = format!("/data/out/2026-09-30/extra{n}.age");
            text.push_str(&serde_json::to_string(&rec).unwrap());
            text.push('\n');
        }
        write_file(&registry, text.as_bytes());
        let records = crate::registry::load_registry(&registry).expect("registry読込");
        let reduced = crate::registry::reduce_records(&records);

        assert_eq!(
            readme_batch_dates(&reduced, None),
            vec!["2026-09-30".to_string(), "2026-10-01".to_string()],
            "全集を昇順・重複除去で返す"
        );
        assert_eq!(
            readme_batch_dates(&reduced, Some("2026-10-01")),
            vec!["2026-10-01".to_string()],
            "--date指定時は当該日付のみ"
        );
        assert!(
            readme_batch_dates(&reduced, Some("2099-12-31")).is_empty(),
            "--date不一致時は空"
        );
    }

    /// [covers:upload.restore-readme-idempotency]
    /// README配置の冪等(unit版・FakeS3): 2回目の実行(pending空)でもREADME配置判定の
    /// headは実行され、head Size一致でskip(putは再実行しない)。README配置でregistryへ
    /// レコードはappendされない(2.3-7・2.6・4.1注記・7.3)。
    #[test]
    fn restore_readme_idempotent_skip_unit() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-rridem");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);
        let readme_put_count = |f: &FakeS3| {
            put_calls(f)
                .iter()
                .filter(|c| c.contains("RESTORE_README.txt"))
                .count()
        };

        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 0, "初回成功");
        assert_eq!(readme_put_count(&fake), 1, "初回はREADME put 1回");
        let lines_after_first = registry_lines(&registry).len();

        // 2回目(pending空): head Size一致でskip・registry追記なし
        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 0, "pending空の再実行もexit 0");
        assert_eq!(readme_put_count(&fake), 1, "README putは再実行しない");
        assert_eq!(
            registry_lines(&registry).len(),
            lines_after_first,
            "README由来のregistry追記なし"
        );
        assert_eq!(
            fake.object("2026-10-01/RESTORE_README.txt").unwrap(),
            crate::RESTORE_README_TEMPLATE.as_bytes(),
            "配置objectはtemplateとbyte一致"
        );
    }

    /// [covers:upload.restore-readme-cp-failure]
    /// README配置のPutObject失敗は転送エラー5(2.3-7・7.2): fail-fastは即終了し、
    /// READMEより前の対象・manifestの成功分とそのregistry記録は残る。
    #[test]
    fn restore_readme_put_failure_is_transfer_error_unit() {
        // README一時fileのpathはprocess内共有のためrun_upload系testと直列化
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-rrfail");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state).fail_cp("2026-10-01/RESTORE_README.txt");

        let code = run_upload(&fake, &opts(&registry));
        assert_eq!(code, 5, "README put失敗は転送エラー5");
        assert!(fake.object("2026-10-01/a.zip.age").is_some(), "対象はupload済み(READMEより前)");
        assert!(fake.object("2026-10-01/manifest.jsonl.age").is_some(), "manifestもupload済み");
        assert!(fake.object("2026-10-01/RESTORE_README.txt").is_none(), "READMEは未配置");
        let lines = registry_lines(&registry);
        let ups = lines
            .iter()
            .filter(|r| r["record_type"] == "upload" || r["record_type"] == "manifest_upload")
            .count();
        assert_eq!(ups, 2, "README失敗前の成功分のregistry記録");
    }

    /// test内で作成したfile/symlinkを、panic時も含めて確実に削除するguard。
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// [covers:upload.restore-readme-temp-file]
    /// 送り元一時fileの作成はcreate_new(O_EXCL): 事前配置されたsymlink・通常fileは
    /// リトライなしで拒否され(設定・環境エラー2)、symlink先の標的fileは破壊されない。
    /// 新規pathへは書き出せ、Dropで削除される。
    #[test]
    fn readme_temp_file_create_new_rejects_preexisting_path() {
        let tmp = TempDir::new("upl-rrnew");

        // 1. symlink事前配置: 拒否され標的fileは無傷(予測可能pathへの第三者による
        //    symlink配置を経由した標的file破壊が成立しない)
        let victim = tmp.path().join("victim.txt");
        write_file(&victim, b"victim-bytes");
        let link = tmp.path().join("readme-tmp-link");
        std::os::unix::fs::symlink(&victim, &link).expect("symlink事前配置");
        let e = write_readme_temp_at(&link).unwrap_err();
        assert!(matches!(e, BackupError::Config(_)), "設定・環境エラー2: {e:?}");
        assert_eq!(e.exit_code(), 2);
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"victim-bytes".to_vec(),
            "symlink先の標的fileは無傷"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), victim, "symlink自体は置き換えない");

        // 2. 通常fileの事前配置も同様に拒否(前回実行のcrash残存相当)
        let plain = tmp.path().join("readme-tmp-plain");
        write_file(&plain, b"preexisting");
        assert!(matches!(
            write_readme_temp_at(&plain),
            Err(BackupError::Config(_))
        ));
        assert_eq!(std::fs::read(&plain).unwrap(), b"preexisting".to_vec(), "既存fileは無傷");

        // 3. 新規pathへは書き出せる。内容は埋め込みtemplateと一致し、Dropで削除される
        let fresh = tmp.path().join("readme-tmp-fresh");
        {
            let t = write_readme_temp_at(&fresh).expect("新規pathへの書き出し成功");
            assert_eq!(
                std::fs::read(t.path()).unwrap(),
                crate::RESTORE_README_TEMPLATE.as_bytes()
            );
        }
        assert!(!fresh.exists(), "Dropで削除される");
    }

    /// [covers:upload.restore-readme-temp-file]
    /// 一時file作成失敗(設定・環境エラー)は--keep-goingでも即終了する(7.2・2.3-7)。
    /// 実装と同一規則の予測可能path(実temp path)へのsymlink事前配置で発生させる。
    #[test]
    fn readme_temp_failure_stops_immediately_even_with_keep_going() {
        // 実temp pathへ触る他のrun_upload系testとの直列化(実temp pathはprocess内共有)
        let _lock = crate::testutil::path_env_lock();
        let tmp = TempDir::new("upl-rrcfg");
        let registry = fixture_batch(tmp.path(), &["a.zip"]);
        let victim = tmp.path().join("victim.txt");
        write_file(&victim, b"victim-bytes");
        let link = readme_temp_path();
        let _cleanup = RemoveOnDrop(link.clone());
        std::os::unix::fs::symlink(&victim, &link).expect("symlink事前配置");

        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);
        let mut o = opts(&registry);
        o.keep_going = true;
        let code = run_upload(&fake, &o);
        assert_eq!(code, 2, "一時file作成失敗は設定・環境エラー2で即終了(keep-goingでも継続しない)");
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"victim-bytes".to_vec(),
            "symlink先の標的fileは無傷"
        );
        let puts = put_calls(&fake);
        assert!(
            puts.iter().all(|c| !c.contains("RESTORE_README.txt")),
            "READMEのputは実行されない: {puts:?}"
        );
    }

    /// [covers:upload.run-summary-readme-separated]
    /// 実行summaryはREADME配置分をオブジェクト数と区別して分離表示する
    /// (dry-run summaryとの粒度一致。README配置対象の無い実行は従来形式)。
    #[test]
    fn run_summary_separates_readme_counts() {
        assert_eq!(
            run_summary_line(2, 1, 0, 1, 0, 0),
            "-- summary: 2 uploaded, 1 skipped, 0 failed + README配置 1件(skip 0・fail 0)"
        );
        assert_eq!(
            run_summary_line(1, 0, 1, 0, 1, 1),
            "-- summary: 1 uploaded, 0 skipped, 1 failed + README配置 0件(skip 1・fail 1)"
        );
        assert_eq!(
            run_summary_line(3, 0, 0, 0, 0, 0),
            "-- summary: 3 uploaded, 0 skipped, 0 failed",
            "README配置対象なしではREADME部分を付加しない"
        );
    }
}
