//! restore系3サブコマンド(request/status/download)の統括(設計本編2.7・
//! tmp/tasks/tm-5/design_restore.md 3章)。S3側を正本とする対象列挙
//! (ListObjectsV2)・復元job発行(RestoreObject)・Restoreヘッダparse・
//! download+検証+復号のflow。ローカルregistry・pipeline.tomlは参照しない
//! (P1作成環境資産への非依存。downloadの--key既定値解決のみcli.rs経由で例外)。
//! S3 API呼び出しは`S3Backend` trait(src/s3.rs)経由(TASK-9 AWS SDK移行。
//! 実networkなしtestはBackendのfake差替え・結合seamで行う)。

use crate::config::AwsConfig;
use crate::errors::{BackupError, BackupResult};
use crate::s3::S3Backend;
use std::path::{Path, PathBuf};

/// READMEのS3キーのファイル名部分(uploader.rsと同一の固定文名)。
const RESTORE_README_NAME: &str = "RESTORE_README.txt";

// ---------------------------------------------------------------------------
// Restoreヘッダparse(3.8)

/// HeadObject応答`Restore`ヘッダの3状態(3.8)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreState {
    /// 復元済み(`ongoing-request="false"`。expiry-dateはRFC 7231 HTTP-dateのraw文字列)
    Restored { expiry_date: String },
    /// 復元進行中(`ongoing-request="true"`)
    Ongoing,
    /// 復元job未発行・期限切れ(ヘッダなし)
    NotRequested,
}

/// `Restore`ヘッダraw文字列を3状態へ分類する(3.8)。
/// カンマ+`key="value"`形式の簡易parse(expiry-date値の中のカンマを壊さない
/// quote考慮split)。時刻の解釈・変換は行わない(原文保持・JST変換しない)。
pub fn parse_restore_state(raw: Option<&str>) -> RestoreState {
    let Some(raw) = raw else {
        return RestoreState::NotRequested;
    };
    let mut ongoing: Option<String> = None;
    let mut expiry: Option<String> = None;
    for segment in split_outside_quotes(raw) {
        let Some((k, v)) = segment.split_once('=') else {
            continue;
        };
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(v);
        match k.trim() {
            "ongoing-request" => ongoing = Some(v.to_string()),
            "expiry-date" => expiry = Some(v.to_string()),
            _ => {}
        }
    }
    match ongoing.as_deref() {
        // ongoing=false自体が取得可能の正本のため、expiry-date欠損は表示上の欠落に留める
        Some("false") => RestoreState::Restored {
            expiry_date: expiry.unwrap_or_else(|| "<no expiry-date>".to_string()),
        },
        Some("true") => RestoreState::Ongoing,
        // ongoing-requestキー不在・値不正は未発行扱い(fail-closed)
        _ => RestoreState::NotRequested,
    }
}

/// quote外のカンマでのみ分割する(`expiry-date="Wed, 14 Oct ..."`内のカンマを保持)。
fn split_outside_quotes(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            ',' if !in_quote => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

// ---------------------------------------------------------------------------
// dry-run表示行(設計10.1のAPI形式)

/// dry-run表示のRestoreObject予定行。
fn dry_run_restore_line(bucket: &str, key: &str, days: u32, tier: &str) -> String {
    format!("[dry-run] s3 RestoreObject bucket={bucket} key={key} days={days} tier={tier}")
}

/// dry-run表示のGetObject予定行(destは`.part`path)。
fn dry_run_get_line(bucket: &str, key: &str, dest_part: &Path) -> String {
    format!(
        "[dry-run] s3 GetObject bucket={bucket} key={key} dest={} checksum_mode=ENABLED",
        dest_part.display()
    )
}

/// ListObjectsV2(paginator)で`<date>/`prefixのkey一覧を取得する(3.2・軸12)。
/// Backend実装がkey昇順・重複除去済みを返す。
fn list_objects(backend: &dyn S3Backend, cfg: &AwsConfig, prefix: &str) -> BackupResult<Vec<String>> {
    backend.list_keys(&cfg.bucket, prefix)
}

/// README(STANDARD即時取得のため復元対象外)の除外判定。
fn is_readme(key: &str) -> bool {
    Path::new(key).file_name().map(|f| f == RESTORE_README_NAME).unwrap_or(false)
}

/// headの`Restore`から状態を得る。head 404(None)の扱いは呼び出し側で決める
/// (requestはkey不在として即転送エラー5・statusは一覧後の消失として未発行表示)。
fn state_of(head_restore: Option<&str>) -> RestoreState {
    parse_restore_state(head_restore)
}

/// download対象の復元済み事前確認(3.4-2・3.4-5b)。未了・未発行は転送エラー5
/// (InvalidObjectState相当を事前head判定で明示的に報告)。
fn ensure_restored(head_restore: Option<&str>, key: &str) -> BackupResult<()> {
    match state_of(head_restore) {
        RestoreState::Restored { .. } => Ok(()),
        RestoreState::Ongoing | RestoreState::NotRequested => Err(BackupError::Transfer(format!(
            "restore requestが未完了または未発行のため取得不能(key={key})。`restore status`で確認してください"
        ))),
    }
}

/// ファイルのsha256(hex)をストリーム計算する(cli.rsのverify --checksumと共用)。
pub(crate) fn sha256_file(path: &Path) -> BackupResult<String> {
    use sha2::Digest;
    let mut f = std::fs::File::open(path)
        .map_err(|e| BackupError::Process(format!("{}: {e}", path.display())))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        use std::io::Read;
        let n = f
            .read(&mut buf)
            .map_err(|e| BackupError::Process(format!("{}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// sha256計算(読込失敗・不在はNone。ローカル照合のskip判定用)。
fn sha256_file_or_none(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    sha256_file(path).ok()
}

// ---------------------------------------------------------------------------
// restore request(3.2)

/// request統括の実行options(3.1)。
#[derive(Debug)]
pub struct RestoreRequestOptions {
    /// 実効aws設定(cli.rsのmerge済み値)
    pub aws: AwsConfig,
    /// 対象batch_date(S3プレフィックス)
    pub date: String,
    /// --name(空=バッチ全体)
    pub names: Vec<String>,
    /// --days値(未指定既定"7"。検証はrun_restore_request内で実施)
    pub days: String,
    /// --tier値(未指定既定"Standard")
    pub tier: String,
    pub dry_run: bool,
    pub keep_going: bool,
}

/// `restore request`統括(3.2)。戻り値はexit code。
pub fn run_restore_request(backend: &dyn S3Backend, opts: &RestoreRequestOptions) -> i32 {
    // 1. 事前検査(S3 API呼び出しはまだ行わない)
    let days: u32 = match opts.days.parse() {
        Ok(d) => d,
        Err(_) => {
            eprintln!(
                "error: 設定・環境エラー: --daysは1〜365の整数で指定してください: {}",
                opts.days
            );
            return 2;
        }
    };
    if !(1..=365).contains(&days) {
        eprintln!(
            "error: 設定・環境エラー: --daysは1〜365の範囲外です: {days}"
        );
        return 2;
    }
    // Deep Archiveで選択不可なExpeditedはallowlistに含めない(軸3)
    if !["Standard", "Bulk"].contains(&opts.tier.as_str()) {
        eprintln!(
            "error: 設定・環境エラー: --tierがallowlist外です: {}(許容値: Standard/Bulk)",
            opts.tier
        );
        return 2;
    }

    // --dry-run: ListObjectsV2/head/RestoreObjectすべて実行せず予定行を表示(軸7)。
    // --name指定時は当該name+manifestの予定行(download側のdry-run表示と対称)
    if opts.dry_run {
        println!(
            "[dry-run] 対象key一覧は未確定(--dateバッチ全件想定・--name指定時は当該name+manifest想定・ListObjectsV2/HeadObject未実行)"
        );
        let names: Vec<String> = if opts.names.is_empty() {
            vec!["<name>".to_string()]
        } else {
            opts.names.clone()
        };
        for n in &names {
            let key = format!("{}/{n}.age", opts.date);
            println!(
                "{}",
                dry_run_restore_line(&opts.aws.bucket, &key, days, &opts.tier)
            );
        }
        // manifestは--nameに関わらず対象へ追加される(`--name manifest.jsonl`指定時は
        // name行と同一keyのため重複表示しない。3.2-2の対象選択と同じ規則)
        if !opts.names.iter().any(|n| n == "manifest.jsonl") {
            let manifest_key = format!("{}/manifest.jsonl.age", opts.date);
            println!(
                "{}",
                dry_run_restore_line(&opts.aws.bucket, &manifest_key, days, &opts.tier)
            );
        }
        println!(
            "-- summary (dry-run): RestoreObject予定(対象key一覧は未確定・RESTORE_README.txtは除外)"
        );
        return 0;
    }

    // 2. 対象列挙(README除外)
    let prefix = format!("{}/", opts.date);
    let keys = match list_objects(backend, &opts.aws, &prefix) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    let keys: Vec<String> = keys.into_iter().filter(|k| !is_readme(k)).collect();
    if keys.is_empty() {
        println!("WARNING: 当該prefixにobjectなし。--date・bucketを確認してください (prefix: {prefix})");
        return 0;
    }
    // --name突合(発行前。RES-023)
    let targets = match select_request_targets(&opts.date, &keys, &opts.names) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };

    // 3. per-key処理(事前head → 復元済み/進行中はskip・未発行のみ発行)
    let mut requested = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut first_err: Option<i32> = None;
    for key in &targets {
        // head 404(None)はkey不在として即転送エラー5(download側の不在報告と対称。
        // NotRequested扱いで発行へ進むとRestoreObjectが再度404で失敗する二段階失敗になる)
        let head = match backend.head(&opts.aws.bucket, key).and_then(|h| {
            h.ok_or_else(|| {
                BackupError::Transfer(format!(
                    "対象keyがS3に存在しない(key={key}。ListObjectsV2結果との不一致)"
                ))
            })
        }) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: {e}");
                failed += 1;
                if first_err.is_none() {
                    first_err = Some(e.exit_code());
                }
                if !opts.keep_going {
                    println!("-- summary: {requested} requested, {skipped} skipped (restored/ongoing), {failed} failed");
                    return e.exit_code();
                }
                continue;
            }
        };
        match state_of(head.restore.as_deref()) {
            RestoreState::Restored { .. } => {
                skipped += 1;
                println!("skip  {key} (restored)");
            }
            RestoreState::Ongoing => {
                skipped += 1;
                println!("skip  {key} (ongoing)");
            }
            RestoreState::NotRequested => {
                // RestoreObject(RestoreRequest{days, glacier_job_parameters{tier}}組立はBackend側)
                match backend.restore(&opts.aws.bucket, key, days, &opts.tier) {
                    Ok(()) => {
                        requested += 1;
                        println!("ok    {key}");
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        failed += 1;
                        if first_err.is_none() {
                            first_err = Some(e.exit_code());
                        }
                        if !opts.keep_going {
                            println!("-- summary: {requested} requested, {skipped} skipped (restored/ongoing), {failed} failed");
                            return e.exit_code();
                        }
                    }
                }
            }
        }
    }
    // 4. summary
    println!("-- summary: {requested} requested, {skipped} skipped (restored/ongoing), {failed} failed");
    first_err.unwrap_or(0)
}

/// request対象keyの選択(3.2-2)。--name指定時はlist結果と突合し不在・重複は対象エラー3。
/// 対象は`<date>/<name>.age`と`<date>/manifest.jsonl.age`に限定。
fn select_request_targets(
    date: &str,
    keys: &[String],
    names: &[String],
) -> BackupResult<Vec<String>> {
    let mut seen = std::collections::HashSet::new();
    for n in names {
        if !seen.insert(n.clone()) {
            return Err(BackupError::Target(format!(
                "--name {n} が重複指定されています(重複指定は対象エラー)"
            )));
        }
    }
    let mut targets: Vec<String>;
    if names.is_empty() {
        targets = keys.to_vec();
    } else {
        targets = Vec::new();
        for n in names {
            let key = format!("{date}/{n}.age");
            if !keys.contains(&key) {
                return Err(BackupError::Target(format!(
                    "--name {n} がバッチ内に存在しません(想定key: {key}。ListObjectsV2結果と不一致)"
                )));
            }
            targets.push(key);
        }
        let manifest_key = format!("{date}/manifest.jsonl.age");
        // `--name manifest.jsonl`指定時はname側で同一keyが既に含まれるため重複追加しない
        // (重複すると同keyへRestoreObjectを2回発行しうる)
        if keys.contains(&manifest_key) && !targets.contains(&manifest_key) {
            targets.push(manifest_key);
        }
        targets.sort();
    }
    Ok(targets)
}

// ---------------------------------------------------------------------------
// restore status(3.3)

/// status統括の実行options(3.1)。
#[derive(Debug)]
pub struct RestoreStatusOptions {
    pub aws: AwsConfig,
    pub date: String,
    pub keep_going: bool,
}

/// `restore status`統括(3.3)。戻り値はexit code。
/// aws設定のmerge・事前検査はcli.rsの担当(この関数はmerge済みの実効設定を受ける)。
pub fn run_restore_status(backend: &dyn S3Backend, opts: &RestoreStatusOptions) -> i32 {
    // 1. 対象列挙(READMEは平文として区別表示)
    let prefix = format!("{}/", opts.date);
    let listed = match list_objects(backend, &opts.aws, &prefix) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    if listed.is_empty() {
        println!("WARNING: 当該prefixにobjectなし。--date・bucketを確認してください (prefix: {prefix})");
        return 0;
    }
    let mut keys: Vec<String> = Vec::new();
    let mut readmes: Vec<String> = Vec::new();
    for k in listed {
        if is_readme(&k) {
            readmes.push(k);
        } else {
            keys.push(k);
        }
    }

    // 2. 各keyのhead → Restore parse → 3状態表示
    println!("== restore status ({}) ==", opts.date);
    let mut restored = 0usize;
    let mut ongoing = 0usize;
    let mut not_requested = 0usize;
    let mut failed = 0usize;
    let mut first_err: Option<i32> = None;
    for key in &keys {
        let head = match backend.head(&opts.aws.bucket, key) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("error: {e}");
                failed += 1;
                if first_err.is_none() {
                    first_err = Some(e.exit_code());
                }
                if !opts.keep_going {
                    println!(
                        "-- {restored} restored, {ongoing} ongoing, {not_requested} not-requested / {} objects (+{} README), {failed} failed",
                        keys.len(),
                        readmes.len()
                    );
                    return e.exit_code();
                }
                continue;
            }
        };
        // head 404(None)は一覧後の消失のため未発行扱いで表示する
        let state = state_of(head.as_ref().and_then(|h| h.restore.as_deref()));
        match state {
            RestoreState::Restored { expiry_date } => {
                restored += 1;
                println!("  restored        {key}  expiry-date=\"{expiry_date}\"");
            }
            RestoreState::Ongoing => {
                ongoing += 1;
                println!("  ongoing         {key}");
            }
            RestoreState::NotRequested => {
                not_requested += 1;
                println!("  not-requested   {key}");
            }
        }
    }
    for key in &readmes {
        println!("  --              {key}  (平文・復元不要)");
    }
    println!(
        "-- {restored} restored, {ongoing} ongoing, {not_requested} not-requested / {} objects (+{} README), {failed} failed",
        keys.len(),
        readmes.len()
    );
    first_err.unwrap_or(0)
}

// ---------------------------------------------------------------------------
// restore download(3.4)

/// download統括の実行options(3.1)。
#[derive(Debug)]
pub struct RestoreDownloadOptions {
    pub aws: AwsConfig,
    pub date: String,
    /// --name(空=バッチ全体)
    pub names: Vec<String>,
    /// 復元先dir(配下に`<date>/`を作る)
    pub out: PathBuf,
    /// 復号用SSH ed25519秘密鍵(cli.rsで--keyまたは[verify].decrypt_keyから解決済み)
    pub key: PathBuf,
    pub dry_run: bool,
    pub keep_going: bool,
}

/// manifestのファイルレコード(検証正本。P2)。
#[derive(Debug, Clone)]
struct ManifestRecord {
    source_name: String,
    source_sha256: String,
    age_sha256: String,
}

/// `restore download`統括(3.4)。戻り値はexit code。
pub fn run_restore_download(backend: &dyn S3Backend, opts: &RestoreDownloadOptions) -> i32 {
    // 1. 事前検査(S3 API呼び出し前に設定エラー2で即終了。対象処理に紛れない)
    if !opts.key.is_file() {
        eprintln!(
            "error: 設定・環境エラー: 復号用秘密鍵ファイルが存在しない ({})",
            opts.key.display()
        );
        return 2;
    }
    if let Err(e) = crate::crypto::find_age() {
        eprintln!("error: {e}");
        return 2;
    }

    let date_dir = opts.out.join(&opts.date);
    let manifest_key = format!("{}/manifest.jsonl.age", opts.date);
    let manifest_age = date_dir.join("manifest.jsonl.age");
    let manifest_plain = date_dir.join("manifest.jsonl");

    // --dry-run: head/get/ageを一切実行せず予定表示(軸7)
    if opts.dry_run {
        println!("[dry-run] 対象key一覧は未確定(--dateバッチ全件想定・manifestは毎回再取得)");
        let part = part_path(&manifest_age);
        println!("{}", dry_run_get_line(&opts.aws.bucket, &manifest_key, &part));
        let names: Vec<String> = if opts.names.is_empty() {
            vec!["<name>".to_string()]
        } else {
            opts.names.clone()
        };
        for n in &names {
            let key = format!("{}/{n}.age", opts.date);
            let part = part_path(&date_dir.join(format!("{n}.age")));
            println!("{}", dry_run_get_line(&opts.aws.bucket, &key, &part));
        }
        println!("-- summary (dry-run): GetObject予定表示のみ(head/get・復号とも未実行)");
        return 0;
    }

    // 2. manifest取得(検証正本・常に最初。毎回再取得・再復号する)
    let head = match backend.head(&opts.aws.bucket, &manifest_key) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    let Some(head) = head else {
        // head 404 = バッチ不在(RES-024)
        eprintln!(
            "error: 対象エラー: {manifest_key}が存在しない=当該バッチ不在(--date誤り・bucket誤り)。--dateとbucket(--aws-config・CLI一時上書き)を確認してください"
        );
        return 3;
    };
    if let Err(e) = ensure_restored(head.restore.as_deref(), &manifest_key) {
        eprintln!("error: {e}");
        return e.exit_code();
    }
    if let Err(e) = fetch_object(backend, &opts.aws, &manifest_key, &manifest_age) {
        eprintln!("error: {e}");
        return e.exit_code();
    }
    if let Err(e) = crate::crypto::decrypt_to_file(&manifest_age, &opts.key, &manifest_plain) {
        eprintln!("error: {e}");
        return e.exit_code();
    }

    // 3. manifest parse(検証エラー4)
    let records = match parse_manifest(&manifest_plain) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };

    // 4. 対象選択(--name突合・不在・重複は対象エラー3)
    let selected = match select_download_records(&records, &opts.names) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };

    // 5. per-file処理(ローカル.age照合先行→head→get→検証→平文衝突→復号)
    let mut downloaded = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut first_err: Option<i32> = None;
    for rec in &selected {
        match process_file(backend, opts, &date_dir, rec) {
            Ok(Outcome::Done { size, sha256 }) => {
                downloaded += 1;
                println!(
                    "ok    {}  {}  sha256:{}",
                    rec.source_name,
                    size,
                    &sha256.chars().take(16).collect::<String>()
                );
            }
            Ok(Outcome::Skipped { reason }) => {
                skipped += 1;
                println!("skip  {} ({reason})", rec.source_name);
            }
            Err(e) => {
                eprintln!("error: {e}");
                failed += 1;
                if first_err.is_none() {
                    first_err = Some(e.exit_code());
                }
                // 設定エラーは常に即中止(7.2)
                if matches!(e, BackupError::Config(_)) || !opts.keep_going {
                    println!(
                        "-- summary: {downloaded} downloaded, {skipped} skipped, {failed} failed / out: {} / manifest: ok",
                        date_dir.display()
                    );
                    return e.exit_code();
                }
            }
        }
    }
    // 6. summary
    println!(
        "-- summary: {downloaded} downloaded, {skipped} skipped, {failed} failed / out: {} / manifest: ok",
        date_dir.display()
    );
    first_err.unwrap_or(0)
}

/// per-file処理の結果。
enum Outcome {
    /// 取得・復号のいずれかを実行した
    Done { size: u64, sha256: String },
    /// .age・平文ともsha256一致で全skip
    Skipped { reason: String },
}

/// getの送り先`.part`path。
fn part_path(dest: &Path) -> PathBuf {
    PathBuf::from(format!("{}.part", dest.display()))
}

/// オブジェクト1件の取得(get→rename。3.4-5c)。失敗時は既存ファイルを保持する。
/// destの親dirは旧`aws s3 cp`の暗黙作成に合わせて明示作成する。
fn fetch_object(backend: &dyn S3Backend, cfg: &AwsConfig, key: &str, dest: &Path) -> BackupResult<()> {
    let part = part_path(dest);
    if let Some(parent) = part.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            BackupError::Transfer(format!(
                "S3 GetObject失敗(key={key}, dest={}): 取得先dirの作成に失敗: {e}",
                part.display()
            ))
        })?;
    }
    if let Err(e) = backend.get_to_file(&cfg.bucket, key, &part) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, dest).map_err(|e| {
        BackupError::Process(format!(
            "renameに失敗({} -> {}): {e}",
            part.display(),
            dest.display()
        ))
    })
}

/// manifest平文のparse(3.4-3)。1行目batchヘッダ・以降ファイルレコード。
/// parse失敗・batchでない・レコード不在は検証エラー4。
fn parse_manifest(path: &Path) -> BackupResult<Vec<ManifestRecord>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        BackupError::Process(format!("manifest平文の読込に失敗 ({}): {e}", path.display()))
    })?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Err(BackupError::Process("manifestが空です".to_string()));
    }
    let header: serde_json::Value = serde_json::from_str(lines[0])
        .map_err(|e| BackupError::Process(format!("manifestヘッダのparseに失敗: {e}")))?;
    if header.get("record_type").and_then(|v| v.as_str()) != Some("batch") {
        return Err(BackupError::Process(
            "manifestヘッダのrecord_typeがbatchでない".to_string(),
        ));
    }
    let mut records = Vec::new();
    for line in &lines[1..] {
        let v: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| BackupError::Process(format!("manifestレコードのparseに失敗: {e}")))?;
        let name = v
            .get("source_name")
            .and_then(|x| x.as_str())
            .ok_or_else(|| BackupError::Process("manifestレコードにsource_nameがない".to_string()))?;
        let source_sha256 = v.get("source_sha256").and_then(|x| x.as_str()).ok_or_else(|| {
            BackupError::Process(format!("manifestレコードにsource_sha256がない ({name})"))
        })?;
        let age_sha256 = v.get("age_sha256").and_then(|x| x.as_str()).ok_or_else(|| {
            BackupError::Process(format!("manifestレコードにage_sha256がない ({name})"))
        })?;
        records.push(ManifestRecord {
            source_name: name.to_string(),
            source_sha256: source_sha256.to_string(),
            age_sha256: age_sha256.to_string(),
        });
    }
    if records.is_empty() {
        return Err(BackupError::Process(
            "manifestにファイルレコードがない".to_string(),
        ));
    }
    Ok(records)
}

/// download対象レコードの選択(3.4-4)。--name不在・重複は対象エラー3。省略時は全件昇順。
fn select_download_records(
    records: &[ManifestRecord],
    names: &[String],
) -> BackupResult<Vec<ManifestRecord>> {
    let mut seen = std::collections::HashSet::new();
    for n in names {
        if !seen.insert(n.clone()) {
            return Err(BackupError::Target(format!(
                "--name {n} が重複指定されています(重複指定は対象エラー)"
            )));
        }
    }
    let mut selected: Vec<ManifestRecord> = if names.is_empty() {
        records.to_vec()
    } else {
        let mut out = Vec::new();
        for n in names {
            let Some(rec) = records.iter().find(|r| &r.source_name == n) else {
                return Err(BackupError::Target(format!(
                    "--name {n} がmanifestのsource_nameに存在しません(バッチ内にないnameです)"
                )));
            };
            out.push(rec.clone());
        }
        out
    };
    selected.sort_by(|a, b| a.source_name.cmp(&b.source_name));
    Ok(selected)
}

/// ファイルレコード1件の処理(3.4-5)。処理順は「ローカル`.age`のsha256照合を先行
/// し、一致ならheadの`Restore`確認をskipして復号へ進む」順序。
fn process_file(
    backend: &dyn S3Backend,
    opts: &RestoreDownloadOptions,
    date_dir: &Path,
    rec: &ManifestRecord,
) -> BackupResult<Outcome> {
    let name = &rec.source_name;
    let age_path = date_dir.join(format!("{name}.age"));
    let plain_path = date_dir.join(name);
    let key = format!("{}/{name}.age", opts.date);
    let mut did_work = false;

    // a. 既存.ageのsha256 → age_sha256と一致なら取得skip(headのRestore確認・getを行わない)
    let age_ok = sha256_file_or_none(&age_path).as_deref() == Some(rec.age_sha256.as_str());
    if !age_ok {
        // b. headでRestore確認(復元済みであること)
        let head = backend.head(&opts.aws.bucket, &key)?;
        let Some(head) = head else {
            return Err(BackupError::Transfer(format!(
                "対象keyがS3に存在しない(key={key}。manifestとS3側の不一致)"
            )));
        };
        ensure_restored(head.restore.as_deref(), &key)?;
        // c. .partへget(checksum_mode=ENABLED)→rename→sha256照合(検証エラー4)
        fetch_object(backend, &opts.aws, &key, &age_path)?;
        let actual = sha256_file(&age_path)?;
        if actual != rec.age_sha256 {
            return Err(BackupError::Process(format!(
                "{name}: 取得.ageのsha256がmanifest記録と不一致(転送破損の疑い。再実行で回復できます) expected={} actual={}",
                rec.age_sha256, actual
            )));
        }
        did_work = true;
    }

    // d. 復号前の平文衝突判定(既存平文は保護対象・不一致は対象エラー3)
    if plain_path.is_file() {
        let actual = sha256_file_or_none(&plain_path);
        match actual.as_deref() {
            Some(s) if s == rec.source_sha256 => {
                return if did_work {
                    Ok(Outcome::Done {
                        size: std::fs::metadata(&plain_path).map(|m| m.len()).unwrap_or(0),
                        sha256: rec.source_sha256.clone(),
                    })
                } else {
                    Ok(Outcome::Skipped {
                        reason: "平文sha256一致".to_string(),
                    })
                };
            }
            Some(_) => {
                return Err(BackupError::Target(format!(
                    "{name}: 既存平文のsha256がmanifest記録と不一致のため上書きしません(手動リネーム後の再実行を推奨)"
                )));
            }
            None => {
                // sha256計算のI/O失敗(不在は上のis_fileで除外済み)。ローカル環境側の
                // 問題のため処理エラー4(sha256不一致と同区分。対象の指定ミスではない)
                return Err(BackupError::Process(format!(
                    "{name}: 既存平文のsha256計算I/O失敗のため上書きしません"
                )));
            }
        }
    }

    // e. 復号(.part→rename・sha256同時計算)→ source_sha256照合(検証エラー4)
    let res = crate::crypto::decrypt_to_file(&age_path, &opts.key, &plain_path)?;
    if res.plain_sha256 != rec.source_sha256 {
        return Err(BackupError::Process(format!(
            "{name}: 復号平文のsha256がmanifest記録と不一致 expected={} actual={}",
            rec.source_sha256, res.plain_sha256
        )));
    }
    Ok(Outcome::Done {
        size: res.plain_size,
        sha256: res.plain_sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [covers:restore.restore-header-parse]
    /// Restoreヘッダparse(3.8・RES-006): 3状態への分類とexpiry-date raw文字列の保持。
    /// 時刻解析・JST変換をしないことの単位確認(integration側はstatus表示経由で検証)。
    #[test]
    fn parse_restore_state_three_states() {
        // ヘッダなし = 未発行
        assert_eq!(parse_restore_state(None), RestoreState::NotRequested);

        // ongoing-request="true" = 進行中
        assert_eq!(
            parse_restore_state(Some(r#"ongoing-request="true""#)),
            RestoreState::Ongoing
        );

        // ongoing-request="false" + expiry-date = 復元済み(raw文字列保持・カンマを含む値)
        let raw = r#"ongoing-request="false", expiry-date="Wed, 14 Oct 2026 00:00:00 GMT""#;
        match parse_restore_state(Some(raw)) {
            RestoreState::Restored { expiry_date } => {
                assert_eq!(expiry_date, "Wed, 14 Oct 2026 00:00:00 GMT");
            }
            other => panic!("restoredに分類されるべき: {other:?}"),
        }

        // expiry-date欠損のrestoredは表示上の欠落留め(fail-closedにしない)
        match parse_restore_state(Some(r#"ongoing-request="false""#)) {
            RestoreState::Restored { expiry_date } => {
                assert_eq!(expiry_date, "<no expiry-date>");
            }
            other => panic!("restoredに分類されるべき: {other:?}"),
        }

        // ongoing-requestキー不在・値不正は未発行扱い(fail-closed)
        assert_eq!(
            parse_restore_state(Some(r#"expiry-date="Wed, 14 Oct 2026 00:00:00 GMT""#)),
            RestoreState::NotRequested
        );
    }

    /// README除外判定の単位確認(軸12・3.2。条件単位の検証はtests/restore.rsの
    /// RES-001結合testが担うためcovers tagは付けない)。
    #[test]
    fn readme_key_excluded() {
        assert!(is_readme("2026-10-08/RESTORE_README.txt"));
        assert!(!is_readme("2026-10-08/photo.zip.age"));
        assert!(!is_readme("2026-10-08/manifest.jsonl.age"));
    }

    /// [covers:restore.request-dry-run]
    /// dry-run予定行のAPI形式(設計10.1)の単位確認(結合側はtests/restore.rsが担う)。
    #[test]
    fn dry_run_lines_use_api_form() {
        let line = dry_run_restore_line("it-bucket", "2026-10-08/a.zip.age", 7, "Standard");
        assert_eq!(
            line,
            "[dry-run] s3 RestoreObject bucket=it-bucket key=2026-10-08/a.zip.age days=7 tier=Standard"
        );
        let part = Path::new("/tmp/x/2026-10-08/a.zip.age.part");
        let line = dry_run_get_line("it-bucket", "2026-10-08/a.zip.age", part);
        assert_eq!(
            line,
            "[dry-run] s3 GetObject bucket=it-bucket key=2026-10-08/a.zip.age dest=/tmp/x/2026-10-08/a.zip.age.part checksum_mode=ENABLED"
        );
    }
}
