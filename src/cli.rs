//! clap定義・サブコマンドdispatch・終了コード集約(設計5章cli.rs・2章)。
//! TASK-11(Phase B): 旧サブコマンド(plan/encrypt/upload/verify/restore)は削除し、
//! 機能は新体系(backup push/pull/verify・get・list jobs等)へ集約(設計7.3)。
//! ローカル整合検査(backup verify)の実装も本moduleにある(旧verifyの引数構造を継承)。

use crate::errors::BackupResult;
use std::path::{Path, PathBuf};

/// サブコマンド引数定義(2章各表)。
#[derive(clap::Parser)]
#[command(name = "amazing-backup", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// 一覧(照会のみ・正引き)
    List(crate::storage::ListArgs),
    /// 探索(逆引き・名前/path部分一致)
    Query(crate::storage::QueryArgs),
    /// 取得(単一file中心。未復元なら復元job発行を内包)
    Get(crate::storage::GetArgs),
    /// 完全性(索引↔S3実態の突合)
    Diff(crate::storage::DiffArgs),
    /// 書き込み(push)/一括復元(pull)/ローカル整合検査(verify)
    Backup(crate::storage::BackupArgs),
    /// ピン留め版の付与/解除
    Pin(crate::storage::PinArgs),
    /// 保持・索引・lifecycle・復元jobの管理
    Manage(crate::storage::ManageArgs),
    /// 設定の編集対象表示(検証付き)
    Edit(crate::storage::EditArgs),
}

/// `--date`の形式検証(backup verifyの対象日filter): `YYYY-MM-DD`(半角数字4-2-2)以外は
/// usage error 64。暦日としての実在性(13月等)は検証しない(形式のみ)。
fn parse_date_arg(s: &str) -> Result<String, String> {
    let parts: Vec<&str> = s.split('-').collect();
    let ok = parts.len() == 3
        && [parts[0].len(), parts[1].len(), parts[2].len()] == [4, 2, 2]
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if ok {
        Ok(s.to_string())
    } else {
        Err(format!(
            "YYYY-MM-DD形式(半角数字4-2-2)で指定してください: {s}"
        ))
    }
}

/// 設定読込の共通部。失敗時はstderrへ出力してNone。
/// --config省略時はXDG規定path(TASK-19: resolve_app_paths)を読む。
fn load_config_or_exit(config: &Option<PathBuf>) -> Option<crate::config::PipelineConfig> {
    let path = config
        .clone()
        .unwrap_or_else(|| crate::config::resolve_app_paths().pipeline_config);
    match crate::config::load_pipeline(&path) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            eprintln!("error: {e}");
            None
        }
    }
}

/// S3 backendの選択(TASK-9設計4.3・9.3)。
/// debug build(`#[cfg(debug_assertions)]`)ではenv `AMAZING_BACKUP_FAKE_S3_STATE`指定時に
/// fake seam(`FakeS3`)へ差し替える(release buildにはseam・fakeともcompileされず
/// 実S3経路になる。cargo testはdev profileのためseam有効)。fake選択時はstderr冒頭へ
/// 「WARNING: FAKE S3 MODE」を必ず表示する(誤用防御)。
/// 実S3 backend(`RealS3`)はcredentialファイル(明示API key)・regionから構築する
/// (credential chain IMDS/env/~/.awsへのfallbackなし・構築失敗は設定エラー2)。
pub(crate) fn select_backend(
    aws: &crate::config::AwsConfig,
) -> BackupResult<Box<dyn crate::s3::S3Backend>> {
    warn_endpoint_url_env();
    #[cfg(debug_assertions)]
    {
        if let Some(state) = std::env::var_os("AMAZING_BACKUP_FAKE_S3_STATE") {
            eprintln!("WARNING: FAKE S3 MODE");
            return Ok(Box::new(crate::s3::FakeS3::from_env(PathBuf::from(state))));
        }
    }
    let creds = crate::config::load_credentials(&aws.credentials_file)?;
    let region = aws.region.clone();
    let real = crate::s3::RealS3::new(&creds, &region)?;
    Ok(Box::new(real))
}

/// AWS_ENDPOINT_URL系envの検出警告(TASK-9設計9.3)。SDKが当該envを解釈して接続先
/// endpointを切替え得るため、設定の事実を通知して気づかせる(処理は継続し、
/// 解釈の無効化は行わない。env設定が意図した検証用構成か誤設定かは利用者にしか
/// 判別できないため)。
fn warn_endpoint_url_env() {
    for name in ["AWS_ENDPOINT_URL", "AWS_ENDPOINT_URL_S3"] {
        if std::env::var_os(name).is_some() {
            eprintln!(
                "WARNING: 環境変数{name}が設定されています(SDKがこのenvを解釈して接続先endpointを切替え得ます。意図した検証用構成か確認してください)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// backup verify(ローカル整合検査・旧verify相当。設計3.2・2.4)

/// backup verifyの引数(旧verifyサブコマンドの引数構造を継承)。
#[derive(clap::Args)]
pub struct VerifyArgs {
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// 当該batch_date分に限定(YYYY-MM-DD形式。形式不正はusage error 64)
    #[arg(long, value_parser = parse_date_arg)]
    pub date: Option<String>,
    /// .ageのsha256を再計算してregistry記録と照合
    #[arg(long)]
    pub checksum: bool,
    /// 秘密鍵で復号し平文sha256をregistry記録と照合
    #[arg(long)]
    pub roundtrip: bool,
    /// 復号用秘密鍵(--roundtrip時必須)
    #[arg(long)]
    pub key: Option<PathBuf>,
}

/// fileのsha256(旧restore.rsから引き継ぎ・verify --checksumで使用)。
fn sha256_file(path: &Path) -> crate::errors::BackupResult<String> {
    use sha2::Digest;
    let mut f = std::fs::File::open(path)
        .map_err(|e| crate::errors::BackupError::Process(format!("{}: {e}", path.display())))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        use std::io::Read;
        let n = f
            .read(&mut buf)
            .map_err(|e| crate::errors::BackupError::Process(format!("{}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// verify統括(旧2.4。registryの暗号化記録を対象としたローカル整合検査)。
/// 旧経路の`encrypt`記録と新経路(backup push)の`file_write`記録の両方を検査する。
/// 戻り値はexit code。
pub fn cmd_verify(args: &VerifyArgs) -> i32 {
    let Some(cfg) = load_config_or_exit(&args.config) else {
        return 2;
    };
    let registry_path = cfg.registry_path.clone();
    let records = match crate::registry::load_registry(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    let reduced = crate::registry::reduce_records(&records);
    let encrypts: Vec<&crate::registry::EncryptRecord> = reduced
        .encrypts
        .iter()
        .filter(|e| {
            args.date
                .as_deref()
                .map(|d| e.batch_date == d)
                .unwrap_or(true)
        })
        .collect();
    let writes: Vec<&crate::registry::FileWriteRecord> = reduced
        .file_writes
        .iter()
        .filter(|w| {
            args.date
                .as_deref()
                .map(|d| w.batch_date == d)
                .unwrap_or(true)
        })
        .collect();

    // 掃除済みrunの除外(M-1): keep_artifacts="run"既定では成功run末尾にcache dir(TASK-19
    // XDG配置の暗号化一時物dir)配下の<batch_id>/が掃除されるため、file_write記録の
    // age_path実ファイル・run manifest実ファイルは存在しない。当該batchのindex_upload
    // 成功記録(成功run末尾の索引更新が完了した事実)があり・run成果物dirが不在のbatchは
    // 「掃除済み・検査対象外」として構造検査・--checksum・--roundtripの対象から除外する
    // (成功run直後のverifyが必ずexit 4にならないように。run成果物が残っているbatchは
    // 従来どおり検査対象)。
    let index_uploaded_batches: std::collections::HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            crate::registry::Record::IndexUpload(iu) => Some(iu.run_id.as_str()),
            _ => None,
        })
        .collect();
    let (writes, swept_writes): (Vec<&crate::registry::FileWriteRecord>, Vec<_>) =
        writes.into_iter().partition(|w| {
            Path::new(&w.age_path)
                .parent()
                .is_some_and(|run_dir| run_dir.is_dir())
                || !index_uploaded_batches.contains(w.batch_id.as_str())
        });
    if !swept_writes.is_empty() {
        let mut swept_batches: Vec<&str> =
            swept_writes.iter().map(|w| w.batch_id.as_str()).collect();
        swept_batches.sort_unstable();
        swept_batches.dedup();
        println!(
            "verify: 掃除済みrun {}件(keep_artifacts=\"run\"の成功run成果物のため構造検査対象外: {})",
            swept_batches.len(),
            swept_batches.join(", ")
        );
    }

    let mut failures: Vec<String> = Vec::new();

    // 段階1: 構造検査(2.4-1)。旧経路(encrypt)と新経路(file_write)で同一基準
    let structure_age = |source_path: &str,
                         age_path: &str,
                         age_size: u64,
                         recipients: &[String],
                         failures: &mut Vec<String>| {
        let age = Path::new(age_path);
        match std::fs::metadata(age) {
            Err(_) => {
                failures.push(format!("{source_path}: .age不在 ({age_path})"));
            }
            Ok(m) => {
                // age_sizeは実ファイルsizeと完全一致(2.4-1・VER-002): 記録値未満
                // (切り詰め)・超過(追記)のいずれも不一致とする
                let actual = m.len();
                if actual != age_size {
                    failures.push(format!(
                        "{source_path}: age_size不一致 recorded={age_size} actual={actual} ({age_path})"
                    ));
                }
                // recipient照合(2.4-1・4.4・VER-013): .ageのヘッダをparseし、
                // ssh-ed25519 stanza識別子が記録recipients指紋から導出したv1.2形式
                // 指紋prefixと全単射で対応するか照合する(誤recipient検出の
                // --roundtripなし実効化。導出不能・形式不正・stanza0件もfail-closedに4)
                match crate::fingerprint::parse_age_header(age) {
                    Ok(header) => {
                        if let Err(err) = crate::fingerprint::verify_recipients_for_fingerprints(
                            &header, recipients,
                        ) {
                            failures.push(format!(
                                "{source_path}: recipient照合不一致: {err} ({age_path})"
                            ));
                        }
                    }
                    Err(err) => {
                        failures.push(format!(
                            "{source_path}: .ageヘッダのparseに失敗: {err} ({age_path})"
                        ));
                    }
                }
            }
        }
    };
    for e in &encrypts {
        structure_age(
            &e.source_path,
            &e.age_path,
            e.age_size,
            &e.recipients,
            &mut failures,
        );
    }
    for w in &writes {
        structure_age(
            &w.source_path,
            &w.age_path,
            w.age_size,
            &w.recipients,
            &mut failures,
        );
    }
    // manifest存在検査(encrypt記録: manifest_path。file_write記録: run dirのmanifest)
    let mut seen_manifests = std::collections::HashSet::new();
    for e in &encrypts {
        if !seen_manifests.insert(e.manifest_path.clone()) {
            continue;
        }
        if !Path::new(&e.manifest_path).is_file() {
            failures.push(format!(
                "{}: manifest不在 ({})",
                e.source_path, e.manifest_path
            ));
        }
    }
    for w in &writes {
        let Some(run_dir) = Path::new(&w.age_path).parent() else {
            continue;
        };
        let manifest_path = run_dir.join("manifest.jsonl.age");
        let key = manifest_path.display().to_string();
        if !seen_manifests.insert(key.clone()) {
            continue;
        }
        if !manifest_path.is_file() {
            failures.push(format!("{}: manifest不在 ({key})", w.source_path));
        }
    }

    // 段階2: --checksum(.ageのsha256再計算照合。2.4-2)
    if args.checksum {
        for e in &encrypts {
            match sha256_file(Path::new(&e.age_path)) {
                Err(err) => {
                    failures.push(format!("{}: .age読込失敗: {err}", e.source_path));
                }
                Ok(actual) => {
                    if actual != e.age_sha256 {
                        failures.push(format!(
                            "{}: age_sha256不一致 expected={} actual={} ({})",
                            e.source_path, e.age_sha256, actual, e.age_path
                        ));
                    }
                }
            }
        }
        for w in &writes {
            match sha256_file(Path::new(&w.age_path)) {
                Err(err) => {
                    failures.push(format!("{}: .age読込失敗: {err}", w.source_path));
                }
                Ok(actual) => {
                    if actual != w.age_sha256 {
                        failures.push(format!(
                            "{}: age_sha256不一致 expected={} actual={} ({})",
                            w.source_path, w.age_sha256, actual, w.age_path
                        ));
                    }
                }
            }
        }
    }

    // 段階3: --roundtrip(復号sha256照合 + manifest突合。2.4-3)
    if args.roundtrip {
        let key = match args.key.clone().or_else(|| cfg.decrypt_key.clone()) {
            Some(k) => k,
            None => {
                eprintln!(
                    "error: 設定・環境エラー: --roundtripには秘密鍵が必要です(--key または設定[verify].decrypt_key)"
                );
                return 2;
            }
        };
        // roundtrip開始前の環境事前検査(2.4・7.1): 秘密鍵file不在・ageバイナリ不在は
        // 設定・環境エラー2として即終了する(対象ごとの復号失敗に紛れて検証エラー4とならない)
        if !key.is_file() {
            eprintln!(
                "error: 設定・環境エラー: 復号用秘密鍵ファイルが存在しない ({})",
                key.display()
            );
            return 2;
        }
        if let Err(e) = crate::crypto::find_age() {
            eprintln!("error: {e}");
            return 2;
        }
        for e in &encrypts {
            match crate::crypto::decrypt_sha256(Path::new(&e.age_path), &key) {
                Err(err) => {
                    failures.push(format!("{}: 復号に失敗: {err}", e.source_path));
                }
                Ok(actual) => {
                    if actual != e.source_sha256 {
                        failures.push(format!(
                            "{}: source_sha256不一致 expected={} actual={} ({})",
                            e.source_path, e.source_sha256, actual, e.age_path
                        ));
                    }
                }
            }
        }
        for w in &writes {
            match crate::crypto::decrypt_sha256(Path::new(&w.age_path), &key) {
                Err(err) => {
                    failures.push(format!("{}: 復号に失敗: {err}", w.source_path));
                }
                Ok(actual) => {
                    if actual != w.source_sha256 {
                        failures.push(format!(
                            "{}: source_sha256不一致 expected={} actual={} ({})",
                            w.source_path, w.source_sha256, actual, w.age_path
                        ));
                    }
                }
            }
        }
        // manifest復号→registry突合(旧経路: manifest_pathごとのバッチ。
        // 新経路: run dirのmanifest.jsonl.ageをfile_write記録(batch)と突合)
        let mut batches: Vec<(String, Vec<&crate::registry::EncryptRecord>)> = Vec::new();
        for e in &encrypts {
            match batches.iter_mut().find(|(mp, _)| *mp == e.manifest_path) {
                Some(slot) => slot.1.push(e),
                None => batches.push((e.manifest_path.clone(), vec![e])),
            }
        }
        for (mp, recs) in &batches {
            let plain = match crate::crypto::decrypt_bytes(Path::new(mp), &key) {
                Ok(p) => p,
                Err(err) => {
                    failures.push(format!("{mp}: manifest復号に失敗: {err}"));
                    continue;
                }
            };
            let text = String::from_utf8_lossy(&plain);
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            if lines.is_empty() {
                failures.push(format!("{mp}: manifestが空"));
                continue;
            }
            let header: serde_json::Value = match serde_json::from_str(lines[0]) {
                Ok(v) => v,
                Err(err) => {
                    failures.push(format!("{mp}: manifestヘッダのparseに失敗: {err}"));
                    continue;
                }
            };
            if header.get("record_type").and_then(|v| v.as_str()) != Some("batch") {
                failures.push(format!("{mp}: manifestヘッダのrecord_typeがbatchでない"));
            }
            let file_count = header.get("file_count").and_then(|v| v.as_u64());
            if file_count != Some(recs.len() as u64) {
                failures.push(format!(
                    "{mp}: manifestのfile_count不一致 manifest={file_count:?} registry={}",
                    recs.len()
                ));
            }
            if lines.len() - 1 != recs.len() {
                failures.push(format!(
                    "{mp}: manifestのファイルレコード数不一致 manifest={} registry={}",
                    lines.len() - 1,
                    recs.len()
                ));
            }
            for line in &lines[1..] {
                let fr: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(err) => {
                        failures.push(format!("{mp}: manifestレコードのparseに失敗: {err}"));
                        continue;
                    }
                };
                let fr_age_path = fr.get("age_path").and_then(|v| v.as_str()).unwrap_or("");
                let Some(rec) = recs.iter().find(|r| r.age_path == fr_age_path) else {
                    failures.push(format!(
                        "{mp}: manifestにregistry記録の無いファイルがある ({fr_age_path})"
                    ));
                    continue;
                };
                if fr.get("source_sha256").and_then(|v| v.as_str())
                    != Some(rec.source_sha256.as_str())
                {
                    failures.push(format!(
                        "{}: manifestのsource_sha256がregistryと不一致 ({})",
                        rec.source_path, mp
                    ));
                }
                if fr.get("age_sha256").and_then(|v| v.as_str()) != Some(rec.age_sha256.as_str()) {
                    failures.push(format!(
                        "{}: manifestのage_sha256がregistryと不一致 ({})",
                        rec.source_path, mp
                    ));
                }
            }
        }
        // 新経路(file_write)のmanifest突合: name一致でsource_sha256・age_sha256を照合
        let mut run_batches: Vec<(String, Vec<&crate::registry::FileWriteRecord>)> = Vec::new();
        for w in &writes {
            let Some(run_dir) = Path::new(&w.age_path).parent() else {
                continue;
            };
            let mp = run_dir.join("manifest.jsonl.age").display().to_string();
            match run_batches.iter_mut().find(|(m, _)| *m == mp) {
                Some(slot) => slot.1.push(w),
                None => run_batches.push((mp, vec![w])),
            }
        }
        for (mp, recs) in &run_batches {
            let plain = match crate::crypto::decrypt_bytes(Path::new(mp), &key) {
                Ok(p) => p,
                Err(err) => {
                    failures.push(format!("{mp}: manifest復号に失敗: {err}"));
                    continue;
                }
            };
            let text = String::from_utf8_lossy(&plain);
            let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
            if lines.is_empty() {
                failures.push(format!("{mp}: manifestが空"));
                continue;
            }
            let header: serde_json::Value = match serde_json::from_str(lines[0]) {
                Ok(v) => v,
                Err(err) => {
                    failures.push(format!("{mp}: manifestヘッダのparseに失敗: {err}"));
                    continue;
                }
            };
            if header.get("record_type").and_then(|v| v.as_str()) != Some("batch") {
                failures.push(format!("{mp}: manifestヘッダのrecord_typeがbatchでない"));
            }
            let file_count = header.get("file_count").and_then(|v| v.as_u64());
            if file_count != Some(recs.len() as u64) {
                failures.push(format!(
                    "{mp}: manifestのfile_count不一致 manifest={file_count:?} registry={}",
                    recs.len()
                ));
            }
            if lines.len() - 1 != recs.len() {
                failures.push(format!(
                    "{mp}: manifestのファイルレコード数不一致 manifest={} registry={}",
                    lines.len() - 1,
                    recs.len()
                ));
            }
            for line in &lines[1..] {
                let fr: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(err) => {
                        failures.push(format!("{mp}: manifestレコードのparseに失敗: {err}"));
                        continue;
                    }
                };
                let name = fr.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let Some(rec) = recs.iter().find(|r| r.source_name == name) else {
                    failures.push(format!(
                        "{mp}: manifestにregistry記録の無いファイルがある ({name})"
                    ));
                    continue;
                };
                if fr.get("source_sha256").and_then(|v| v.as_str())
                    != Some(rec.source_sha256.as_str())
                {
                    failures.push(format!(
                        "{}: manifestのsource_sha256がregistryと不一致 ({})",
                        rec.source_path, mp
                    ));
                }
                if fr.get("age_sha256").and_then(|v| v.as_str()) != Some(rec.age_sha256.as_str()) {
                    failures.push(format!(
                        "{}: manifestのage_sha256がregistryと不一致 ({})",
                        rec.source_path, mp
                    ));
                }
            }
        }
    }

    let records_total = encrypts.len() + writes.len();
    if failures.is_empty() {
        println!(
            "== verify == ok ({records_total}) records, checksum={}, roundtrip={}",
            args.checksum, args.roundtrip
        );
        0
    } else {
        for f in &failures {
            eprintln!("ng {f}");
        }
        println!(
            "== verify == FAILED: {} 件の不一致({records_total} records)",
            failures.len()
        );
        4
    }
}

/// dispatch(5章)。BackupErrorはmessage+exit code対応をstderrへ出力し、対応exit codeを返す。
/// clapのusage errorは`BackupError::Usage`へ変換しexit 64(7.1)。
pub fn run(argv: impl IntoIterator<Item = std::ffi::OsString>) -> i32 {
    use clap::Parser as _;
    match Cli::try_parse_from(argv) {
        Ok(cli) => match cli.command {
            Command::List(a) => crate::storage::cmd_list(&a),
            Command::Query(a) => crate::storage::cmd_query(&a),
            Command::Get(a) => crate::storage::cmd_get(&a),
            Command::Diff(a) => crate::storage::cmd_diff(&a),
            Command::Backup(a) => crate::storage::cmd_backup(&a),
            Command::Pin(a) => crate::storage::cmd_pin(&a),
            Command::Manage(a) => crate::storage::cmd_manage(&a),
            Command::Edit(a) => crate::storage::cmd_edit(&a),
        },
        Err(e) => {
            use clap::error::ErrorKind;
            match e.kind() {
                ErrorKind::DisplayVersion | ErrorKind::DisplayHelp => {
                    println!("{}", e.render());
                    0
                }
                _ => {
                    eprint!("{}", e.render());
                    64
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{
        TEST_ED25519_FINGERPRINT, TempDir, config_env_lock, path_env_lock, set_amazing_backup_home,
        set_path_env, write_file, write_test_ed25519_pub,
    };

    /// pipeline.toml fixture(TASK-19 XDG配置後のschema: [output]無し。
    /// registry・cache dirはAMAZING_BACKUP_HOME解決のためtest側でset_amazing_backup_home)。
    fn pipeline_toml(dir: &Path) -> PathBuf {
        let pubkey = write_test_ed25519_pub(dir);
        let p = dir.join("pipeline.toml");
        write_file(
            &p,
            format!(
                "[recipients]\npublic_keys = [\"{}\"]\n\
                 [behavior]\nkeep_going = false\n\
                 [verify]\ndecrypt_key = \"{}\"\n",
                pubkey.display(),
                dir.join("unused-key").display(),
            )
            .as_bytes(),
        );
        p
    }

    /// [covers:cli.usage-error-exit-64]
    /// 使用法誤り(不明オプション・サブコマンド不足)はexit 64。
    #[test]
    fn usage_error_exits_64() {
        let code = run(["amazing-backup".into(), "--definitely-not-an-option".into()]);
        assert_eq!(code, 64, "不明オプション");

        let code = run(["amazing-backup".into()]);
        assert_eq!(code, 64, "サブコマンド不足");

        let code = run(["amazing-backup".into(), "no-such-subcommand".into()]);
        assert_eq!(code, 64, "未知サブコマンド");
    }

    /// [covers:cli.exit-code-mapping]
    /// 代表ケースで0/2/3/4を検証(5はuploader.rsの転送エラー系テストが担う)。
    /// TASK-11 Phase B: 旧plan経路の代表ケースはbackup pushの対象指定へ読み替え。
    #[test]
    fn exit_code_mapping_representative_cases() {
        let tmp = TempDir::new("cli-codes");
        let _env_lock = config_env_lock();
        let _abh = set_amazing_backup_home(tmp.path());

        // 2: 設定エラー(不在のpipeline.tomlでbackup push)
        let code = run([
            "amazing-backup".into(),
            "backup".into(),
            "push".into(),
            "--config".into(),
            tmp.path().join("absent.toml").into(),
            "x.zip".into(),
        ]);
        assert_eq!(code, 2, "設定エラー");

        // 3: 対象エラー(不在ファイルでbackup push。aws.tomlは対象解決より前の検証まで通るものを用意)
        let aws_toml = tmp.path().join("aws.toml");
        write_file(
            &aws_toml,
            b"[aws]\nactive_profile = \"dev\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"b\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"creds.toml\"\n",
        );
        let code = run([
            "amazing-backup".into(),
            "backup".into(),
            "push".into(),
            "--config".into(),
            pipeline_toml(tmp.path()).into(),
            "--aws-config".into(),
            aws_toml.into(),
            tmp.path().join("absent.zip").into(),
        ]);
        assert_eq!(code, 3, "対象エラー");

        // 4 + 0: verify(構造検査)の不一致と成功
        let (reg, age, manifest) = verify_fixture(tmp.path());
        let args_ok = VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: false,
            roundtrip: false,
            key: None,
        };
        assert_eq!(cmd_verify(&args_ok), 0, "整合状態の構造検査");

        let _ = reg;
        std::fs::remove_file(&age).expect("remove .age");
        assert_eq!(cmd_verify(&args_ok), 4, ".age欠損は検証エラー4");

        let _ = manifest;
    }

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// verify用fixtureの.age内容: 合成ageヘッダ(v1.2形式ssh-ed25519 stanza+`--- `終端+
    /// 短いpayload。設計8章TASK-2追記節のfixture再構成要件)。prefixesは各stanzaの第1引数
    /// (v1.2形式指紋prefix)。第2引数(ephemeral公開鍵)は照合対象外のため固定値。
    /// payloadはsize不一致(切り詰め)caseでヘッダ部を壊さないための余白。
    fn synthetic_age_bytes(prefixes: &[String]) -> Vec<u8> {
        let ephemeral = "A".repeat(43); // v1.2形式第2引数(32byteのb64)
        let mut text = String::from("age-encryption.org/v1\n");
        for prefix in prefixes {
            text.push_str(&format!("-> ssh-ed25519 {prefix} {ephemeral}\n"));
        }
        text.push_str("--- bWFjbWFjbWFjbWFjbWFjbWFjbWFjbWFjbWFjbWFjbWFj\n");
        text.push_str("payload-bytes");
        text.into_bytes()
    }

    /// 記録指紋(SHA256:<b64 paddingなし32byte>)からv1.2形式指紋prefix(b64部をdecodeした
    /// 32byteの先頭4byteをb64 paddingなし符号化)を導出する(構造検査recipient照合と同じ
    /// 導出。設計2.4-1・4.4)。
    fn prefix_of_fingerprint(fingerprint: &str) -> String {
        use base64::Engine as _;
        let b64 = fingerprint.strip_prefix("SHA256:").expect("SHA256:前置");
        let digest = base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(b64)
            .expect("指紋b64 decode");
        assert_eq!(digest.len(), 32, "実形式指紋は32byte");
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(&digest[..4])
    }

    /// 誤recipient検出用の別鍵指紋(実形式。TEST_ED25519_FINGERPRINTとは異なる)。
    fn other_ed25519_fingerprint() -> String {
        use base64::Engine as _;
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(b"another-ed25519-recipient-blob");
        format!(
            "SHA256:{}",
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
        )
    }

    /// verify用fixture: registry 1行 + .age + manifestを用意し(registry, .age, manifest)を返す。
    /// .ageのage_size・age_sha256は実際に書き込むbyte列から導く(記録値と実sizeの完全一致。
    /// 設計2.4-1)。recipientsは実形式指紋(SHA256:<b64(32byte)>)で記録する。
    fn verify_fixture_with(
        dir: &Path,
        age_bytes: &[u8],
        recipients: &[String],
    ) -> (PathBuf, PathBuf, PathBuf) {
        let date_dir = dir.join("out").join("2026-10-08");
        let age = date_dir.join("a.zip.age");
        let manifest = date_dir.join("manifest.jsonl.age");
        write_file(&age, age_bytes);
        write_file(&manifest, b"manifest-bytes");
        let rec = serde_json::json!({
            "schema": 1,
            "record_type": "encrypt",
            "recorded_at": "2026-10-08T10:00:00+09:00",
            "batch_id": "20261008-abcdef01",
            "batch_date": "2026-10-08",
            "source_path": dir.join("a.zip"),
            "source_name": "a.zip",
            "source_size": 18,
            "source_mtime": "2026-10-08T09:00:00+09:00",
            "source_sha256": "1".repeat(64),
            "age_path": age,
            "age_size": age_bytes.len(),
            "age_sha256": sha256_hex(age_bytes),
            "recipients": recipients,
            "manifest_path": manifest,
        });
        let reg = dir.join("registry.jsonl");
        write_file(&reg, format!("{rec}\n").as_bytes());
        (reg, age, manifest)
    }

    /// verify用fixture(既定): 記録指紋TEST_ED25519_FINGERPRINT(実形式)と、そこから導出した
    /// 指紋prefixのstanza 1つからなる合成ageヘッダで整合状態を構成する。
    fn verify_fixture(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
        verify_fixture_with(
            dir,
            &synthetic_age_bytes(&[prefix_of_fingerprint(TEST_ED25519_FINGERPRINT)]),
            &[TEST_ED25519_FINGERPRINT.to_string()],
        )
    }

    /// [covers:verify.structure-check-ok]
    /// 整合している状態で構造検査(既定段階)が全項目ok・exit 0。
    #[test]
    fn verify_structure_check_ok() {
        let tmp = TempDir::new("cli-verok");
        let _env_lock = config_env_lock();
        let _abh = set_amazing_backup_home(tmp.path());
        let _ = verify_fixture(tmp.path());
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: false,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 0);
    }

    /// [covers:verify.missing-or-size-mismatch-error]
    /// 構造検査で.age欠損・age_size不一致を検出しexit 4(欠損も不一致と同じく4に統一)。
    /// age_sizeは完全一致(設計2.4-1): 実sizeが記録値を超過(追記)・未満(切り詰め)の
    /// いずれもexit 4となる(記録値未満を警告継続にしない)。
    #[test]
    fn verify_detects_missing_and_size_mismatch() {
        let tmp = TempDir::new("cli-vermiss");
        let _env_lock = config_env_lock();
        let _abh = set_amazing_backup_home(tmp.path());
        // case 1: .age欠損
        let (_, age, _) = verify_fixture(tmp.path());
        std::fs::remove_file(&age).expect("remove");
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: false,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 4, ".age欠損");

        // case 2: age_sizeと異なる実sizeの.age(記録値を超過・追記)
        let tmp2 = TempDir::new("cli-versize");
        let _abh2 = set_amazing_backup_home(tmp2.path());
        let (_, age, _) = verify_fixture(tmp2.path());
        let mut appended = std::fs::read(&age).expect("age read");
        appended.push(b'X');
        write_file(&age, &appended);
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp2.path())),
            date: None,
            checksum: false,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 4, "size不一致(超過)");

        // case 3: 実sizeが記録値未満(切り詰め)。ヘッダ部は壊さずpayload末尾を1byte切り詰める
        let tmp3 = TempDir::new("cli-vershort");
        let _abh3 = set_amazing_backup_home(tmp3.path());
        let (_, age, _) = verify_fixture(tmp3.path());
        let bytes = std::fs::read(&age).expect("age read");
        write_file(&age, &bytes[..bytes.len() - 1]);
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp3.path())),
            date: None,
            checksum: false,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 4, "size不一致(未満・切り詰め)も完全一致要件でexit 4");
    }

    /// [covers:verify.structure-recipients-bijection]
    /// 構造検査(既定段階)のrecipient照合(VER-013・設計2.4-1・4.4): .ageヘッダをparseし、
    /// ssh-ed25519 stanza識別子(v1.2形式指紋prefix)がregistry記録recipients指紋から導出した
    /// prefix集合と全単射で対応することを照合する。(a)正recipient(全単射一致)はexit 0、
    /// (b)誤recipient指紋(stanzaは鍵Aのprefix・記録は鍵Bの指紋)・(c)stanza数不一致
    /// (ヘッダ2 stanza vs 記録1指紋)は検証エラー4。
    #[test]
    fn verify_structure_recipient_bijection() {
        let right = TEST_ED25519_FINGERPRINT.to_string();
        let wrong = other_ed25519_fingerprint();
        assert_ne!(wrong, right, "誤recipient検出用指紋は正指紋と異なる");

        // (a) 正recipient: 記録指紋から導出したprefixと一致するstanza 1つ → exit 0
        let tmp = TempDir::new("cli-verbij-a");
        let _env_lock = config_env_lock();
        let _abh = set_amazing_backup_home(tmp.path());
        let _ = verify_fixture(tmp.path());
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: false,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 0, "正recipientは照合通過");

        // (b) 誤recipient指紋: .ageのstanzaはrightのprefix・registry記録はwrongの指紋 → exit 4
        let tmp = TempDir::new("cli-verbij-b");
        // 直列化lockは(a)で取得済み(同一関数内のため再取得するとdeadlockする)。
        // ABHのみ当該subcaseのtmpへ向け直す(名前を分けてshadowing dropを避ける)。
        let _abh_b = set_amazing_backup_home(tmp.path());
        let _ = verify_fixture_with(
            tmp.path(),
            &synthetic_age_bytes(&[prefix_of_fingerprint(&right)]),
            &[wrong.clone()],
        );
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: false,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 4, "誤recipient指紋は検証エラー4");

        // (c) stanza数不一致: ヘッダ2 stanza(right・wrongのprefix) vs 記録1指紋(right) → exit 4
        let tmp = TempDir::new("cli-verbij-c");
        let _abh_c = set_amazing_backup_home(tmp.path());
        let _ = verify_fixture_with(
            tmp.path(),
            &synthetic_age_bytes(&[prefix_of_fingerprint(&right), prefix_of_fingerprint(&wrong)]),
            &[right.clone()],
        );
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: false,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 4, "stanza数不一致は検証エラー4");
    }

    /// [covers:verify.checksum-detects-mutation]
    /// --checksumで.ageの改変(同sizeの内容改変・byte追加)をsha256再計算により検出する(exit 4)。
    #[test]
    fn verify_checksum_detects_mutation() {
        // 同サイズの内容改変(size検査では拾えずchecksum再計算で検出。
        // ヘッダ部は壊さずpayload末尾byteを反転)
        let tmp = TempDir::new("cli-verck");
        let _env_lock = config_env_lock();
        let _abh = set_amazing_backup_home(tmp.path());
        let (_, age, _) = verify_fixture(tmp.path());
        let mut flipped = std::fs::read(&age).expect("age read");
        let last = flipped.len() - 1;
        flipped[last] ^= 0xff;
        write_file(&age, &flipped);
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: true,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 4, "内容改変(同size)をchecksumで検出");

        // byte追加(givenのシナリオ)
        let tmp2 = TempDir::new("cli-verck2");
        let _abh2 = set_amazing_backup_home(tmp2.path());
        let (_, age, _) = verify_fixture(tmp2.path());
        let mut appended = std::fs::read(&age).expect("age read");
        appended.push(b'X');
        write_file(&age, &appended);
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp2.path())),
            date: None,
            checksum: true,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 4, "byte追加を検出");
    }

    /// [covers:verify.runs-without-aws]
    /// verifyはaws CLI不在・aws.toml不在でも完結する(外部バイナリ不要をPATH排除で担保)。
    #[test]
    fn verify_runs_without_aws() {
        // config_env_lockはpath_env_lockと同一のため1回の取得で足りる
        // (PATH・AMAZING_BACKUP_HOME両方の書換えをこのlockで直列化)
        let _lock = path_env_lock();
        let tmp = TempDir::new("cli-vernoaws");
        let _abh = set_amazing_backup_home(tmp.path());
        let _ = verify_fixture(tmp.path());
        let empty_bin = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&empty_bin).expect("empty bin");
        let _guard = set_path_env(empty_bin.to_str().unwrap());

        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: false,
            roundtrip: false,
            key: None,
        });
        assert_eq!(code, 0, "aws不在でも構造検査は完結");
    }

    /// [covers:cli.paths-required-usage-error]
    /// backup push のPATH位置引数0個はusage error(exit 64)。
    /// PATH 1個+--from-list併用はusage errorとならない(TASK-11 Phase B: backup pushの対象指定)。
    #[test]
    fn paths_required_usage_error() {
        // PATH 0個(--from-listの有無にかかわらず)は64
        let code = run([
            "amazing-backup".into(),
            "backup".into(),
            "push".into(),
            "--config".into(),
            "some.toml".into(),
        ]);
        assert_eq!(code, 64, "backup push PATH 0個");

        let code = run([
            "amazing-backup".into(),
            "backup".into(),
            "push".into(),
            "--config".into(),
            "some.toml".into(),
            "--from-list".into(),
            "list.toml".into(),
        ]);
        assert_eq!(code, 64, "backup push PATH 0個(--from-listのみでも不可)");

        // PATH 1個(+--from-list併用)はusage errorとならない(設定エラー等別区分)
        let code = run([
            "amazing-backup".into(),
            "backup".into(),
            "push".into(),
            "--config".into(),
            "absent.toml".into(),
            "a.zip".into(),
        ]);
        assert_ne!(code, 64, "PATH 1個はusage errorでない(実際は設定エラー2)");

        let code = run([
            "amazing-backup".into(),
            "backup".into(),
            "push".into(),
            "--config".into(),
            "absent.toml".into(),
            "--from-list".into(),
            "list.toml".into(),
            "a.zip".into(),
        ]);
        assert_ne!(code, 64, "PATH+--from-list併用はusage errorでない");
    }

    /// [covers:verify.roundtrip-env-error-exit-2]
    /// verify --roundtripはroundtrip開始前に秘密鍵file不在・ageバイナリ不在を
    /// 事前検査しexit 2(検証エラー4に紛れない)。
    #[test]
    fn verify_roundtrip_env_error_is_exit_2() {
        // 1. 秘密鍵file不在
        let tmp = TempDir::new("cli-verenv");
        let _env_lock = config_env_lock();
        let _abh = set_amazing_backup_home(tmp.path());
        let _ = verify_fixture(tmp.path());
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: false,
            roundtrip: true,
            key: Some(tmp.path().join("absent-secret-key")),
        });
        assert_eq!(code, 2, "秘密鍵file不在は設定・環境エラー2");

        // 2. 秘密鍵fileは在るがageバイナリ不在(空PATH。lockは上で取得済み=
        // config_env_lockと同一のため再取得しない)
        let tmp2 = TempDir::new("cli-verenv2");
        let _abh2 = set_amazing_backup_home(tmp2.path());
        let _ = verify_fixture(tmp2.path());
        let key_file = tmp2.path().join("key-file");
        write_file(&key_file, b"dummy-secret");
        let empty_bin = tmp2.path().join("empty-bin");
        std::fs::create_dir_all(&empty_bin).expect("empty bin");
        let _guard = set_path_env(empty_bin.to_str().unwrap());
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp2.path())),
            date: None,
            checksum: false,
            roundtrip: true,
            key: Some(key_file),
        });
        assert_eq!(code, 2, "ageバイナリ不在は設定・環境エラー2");
    }
}
