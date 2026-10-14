//! clap定義・サブコマンドdispatch・終了コード集約(設計5章cli.rs・2章)。

use crate::errors::{BackupError, BackupResult};
use std::path::{Path, PathBuf};

/// 既定のpipeline設定path(2章各表)。
const DEFAULT_PIPELINE_CONFIG: &str = "configs/pipeline.toml";
/// 既定のaws設定path(2.3)。
const DEFAULT_AWS_CONFIG: &str = "configs/aws.toml";

/// サブコマンド引数定義(2章各表)。
#[derive(clap::Parser)]
#[command(name = "backup-remote", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// 対象解決と事前検査(暗号化・uploadを行わない)
    Plan(PlanArgs),
    /// 暗号化とregistry・manifest記録
    Encrypt(EncryptArgs),
    /// S3 Deep Archiveへの転送
    Upload(UploadArgs),
    /// ローカル整合検証(明示オプションで復号roundtrip)
    Verify(VerifyArgs),
    /// S3 Deep Archiveからの復元(発行・状態確認・取得+検証+復号)
    Restore(RestoreArgs),
}

#[derive(clap::Args)]
pub struct PlanArgs {
    /// pipeline設定(既定 configs/pipeline.toml)
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// 対象リストtoml(位置引数と併用可・和集合)
    #[arg(long)]
    pub from_list: Option<PathBuf>,
    /// 対象アーカイブ(zip/7z)
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
}

#[derive(clap::Args)]
pub struct EncryptArgs {
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub from_list: Option<PathBuf>,
    /// 成果物dir(既定は設定[output].dir)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// registry path(既定は設定[output].registry)
    #[arg(long)]
    pub registry: Option<PathBuf>,
    /// per-file失敗でも継続
    #[arg(long)]
    pub keep_going: bool,
    /// 対象アーカイブ(zip/7z)
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
}

/// `--date`の形式検証(2.3・2.4): `YYYY-MM-DD`(半角数字4-2-2)以外はusage error 64。
/// 誤記(例: `2026-1-1`)が対象なしexit 0で静かな未upload/未検証になるのを防ぐ。
/// 暦日としての実在性(13月等)は検証しない(形式のみ)。
fn parse_date_arg(s: &str) -> Result<String, String> {
    let parts: Vec<&str> = s.split('-').collect();
    let ok = parts.len() == 3
        && [parts[0].len(), parts[1].len(), parts[2].len()] == [4, 2, 2]
        && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if ok {
        Ok(s.to_string())
    } else {
        Err(format!("YYYY-MM-DD形式(半角数字4-2-2)で指定してください: {s}"))
    }
}

#[derive(clap::Args)]
pub struct UploadArgs {
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub aws_config: Option<PathBuf>,
    #[arg(long)]
    pub aws_profile: Option<String>,
    /// 当該batch_date分に限定(YYYY-MM-DD形式。形式不正はusage error 64)
    #[arg(long, value_parser = parse_date_arg)]
    pub date: Option<String>,
    /// head-object・cpを実行せず実行予定とskip想定を表示
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub keep_going: bool,
    /// storage classの当該実行限りの上書き(既定はaws.toml値。空文字・空白のみは未指定扱い)
    ///
    /// 優先順位: CLI指定値 > aws.toml値・aws.tomlが正本(aws.tomlへの書戻しはしない)
    #[arg(long, verbatim_doc_comment)]
    pub storage_class: Option<String>,
    /// S3 bucketの当該実行限りの上書き(既定はaws.toml値。空文字・空白のみ・前後空白付きは設定エラー)
    ///
    /// 優先順位: CLI指定値 > aws.toml値・aws.tomlが正本(aws.tomlへの書戻しはしない)
    #[arg(long, verbatim_doc_comment)]
    pub bucket: Option<String>,
    /// regionの当該実行限りの上書き(既定はaws.toml値。空文字・空白のみ・前後空白付きは設定エラー)
    ///
    /// 優先順位: CLI指定値 > aws.toml値・aws.tomlが正本(aws.tomlへの書戻しはしない)
    #[arg(long, verbatim_doc_comment)]
    pub region: Option<String>,
}

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

/// `restore`配下の3サブコマンド(2.7・TASK-5 3.1)。
#[derive(clap::Args)]
pub struct RestoreArgs {
    #[command(subcommand)]
    pub command: RestoreCommand,
}

#[derive(clap::Subcommand)]
pub enum RestoreCommand {
    /// 復元job発行(Deep Archive取り出し要求)
    Request(RestoreRequestArgs),
    /// 復元状態一覧(head-objectのRestoreヘッダ)
    Status(RestoreStatusArgs),
    /// 取得・検証・復号(manifest正本のsha256照合)
    Download(RestoreDownloadArgs),
}

/// restore系共通のCLI一時上書きoptions(3.1。--storage-class・--profileは持たない。
/// --profileはTASK-9で廃止)。
fn restore_aws_overrides(
    bucket: &Option<String>,
    region: &Option<String>,
) -> crate::config::AwsOverrides {
    crate::config::AwsOverrides {
        storage_class: None,
        bucket: bucket.clone(),
        region: region.clone(),
    }
}

#[derive(clap::Args)]
pub struct RestoreRequestArgs {
    #[arg(long)]
    pub aws_config: Option<PathBuf>,
    #[arg(long)]
    pub aws_profile: Option<String>,
    /// 対象batch_date(YYYY-MM-DD形式・必須。形式不正はusage error 64)
    #[arg(long, value_parser = parse_date_arg)]
    pub date: String,
    /// 対象source_name(basename・repeatable。省略時バッチ全体。重複・不在は対象エラー3)
    #[arg(long)]
    pub name: Vec<String>,
    /// 復元保持日数(1〜365の整数・既定7。範囲外・非整数は設定エラー2)
    #[arg(long)]
    pub days: Option<String>,
    /// 復元Tier(Standard/Bulk・既定Standard。allowlist外は設定エラー2)
    #[arg(long)]
    pub tier: Option<String>,
    /// S3 bucketの当該実行限りの上書き(既定はaws.toml値。空文字・空白のみ・前後空白付きは設定エラー)
    #[arg(long, verbatim_doc_comment)]
    pub bucket: Option<String>,
    /// regionの当該実行限りの上書き(既定はaws.toml値。空文字・空白のみ・前後空白付きは設定エラー)
    #[arg(long, verbatim_doc_comment)]
    pub region: Option<String>,
    /// aws実行なし(ListObjectsV2/head/RestoreObject)。予定行を表示
    #[arg(long)]
    pub dry_run: bool,
    /// per-key失敗でも継続(設定エラー2は常に即中止)
    #[arg(long)]
    pub keep_going: bool,
}

#[derive(clap::Args)]
pub struct RestoreStatusArgs {
    #[arg(long)]
    pub aws_config: Option<PathBuf>,
    #[arg(long)]
    pub aws_profile: Option<String>,
    /// 対象batch_date(YYYY-MM-DD形式・必須。形式不正はusage error 64)
    #[arg(long, value_parser = parse_date_arg)]
    pub date: String,
    /// S3 bucketの当該実行限りの上書き(既定はaws.toml値。空文字・空白のみ・前後空白付きは設定エラー)
    #[arg(long, verbatim_doc_comment)]
    pub bucket: Option<String>,
    /// regionの当該実行限りの上書き(既定はaws.toml値。空文字・空白のみ・前後空白付きは設定エラー)
    #[arg(long, verbatim_doc_comment)]
    pub region: Option<String>,
    /// head失敗でも継続(設定エラー2は常に即中止)
    #[arg(long)]
    pub keep_going: bool,
}

#[derive(clap::Args)]
pub struct RestoreDownloadArgs {
    #[arg(long)]
    pub aws_config: Option<PathBuf>,
    #[arg(long)]
    pub aws_profile: Option<String>,
    /// 対象batch_date(YYYY-MM-DD形式・必須。形式不正はusage error 64)
    #[arg(long, value_parser = parse_date_arg)]
    pub date: String,
    /// 対象source_name(basename・repeatable。省略時manifest全件。重複・不在は対象エラー3)
    #[arg(long)]
    pub name: Vec<String>,
    /// 復元先dir(既定 data/restore。配下に<date>/を作る)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// 復号用SSH ed25519秘密鍵(既定はpipeline.tomlの[verify].decrypt_key)
    #[arg(long)]
    pub key: Option<PathBuf>,
    /// S3 bucketの当該実行限りの上書き(既定はaws.toml値。空文字・空白のみ・前後空白付きは設定エラー)
    #[arg(long, verbatim_doc_comment)]
    pub bucket: Option<String>,
    /// regionの当該実行限りの上書き(既定はaws.toml値。空文字・空白のみ・前後空白付きは設定エラー)
    #[arg(long, verbatim_doc_comment)]
    pub region: Option<String>,
    /// aws実行なし(head/get)・復号もしない。GetObject予定行を表示
    #[arg(long)]
    pub dry_run: bool,
    /// per-file失敗でも継続(設定エラー2は常に即中止)
    #[arg(long)]
    pub keep_going: bool,
}

/// plan結果の表示組み立て(2.1)。cmd_planがstdoutへ出力する文字列。
/// (fingerprint, 公開鍵path)の組でrecipientを表示する。
pub(crate) fn format_plan_report(
    targets: &[crate::targets::Target],
    recipients: &[(String, PathBuf)],
    out_dir: &Path,
    batch_date: &str,
) -> String {
    let mut s = String::from("== plan ==\n");
    let mut total: u64 = 0;
    for (i, t) in targets.iter().enumerate() {
        total += t.size;
        s.push_str(&format!(
            "  {:>2}. {}  {:>12}  mtime {} JST\n",
            i + 1,
            t.source_path.display(),
            fmt_size(t.size),
            t.mtime.format("%Y-%m-%d %H:%M"),
        ));
    }
    s.push_str(&format!(
        "targets: {} files, {} total\n",
        targets.len(),
        fmt_size(total)
    ));
    s.push_str(&format!(
        "recipients (1-of-N, N={}):\n",
        recipients.len()
    ));
    for (fp, path) in recipients {
        s.push_str(&format!("  - {fp}   {}\n", path.display()));
    }
    let mut listing: Vec<String> = targets
        .iter()
        .map(|t| format!("{}.age", t.source_name))
        .collect();
    listing.push("manifest.jsonl.age".to_string());
    s.push_str(&format!(
        "output: {}/{}/{{{}}}\n",
        out_dir.display(),
        batch_date,
        listing.join(", ")
    ));
    s.push_str(&format!("s3 prefix: {batch_date}/\n"));
    s
}

/// サイズの表示(二進接頭辞・小数1桁)。
fn fmt_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 5] = [
        ("PiB", 1 << 50),
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {}", bytes as f64 / scale as f64, unit);
        }
    }
    format!("{bytes} B")
}

/// バッチ識別子`<YYYYMMDD>-<8桁hex乱数>`(4.1)。
fn new_batch_id(batch_date: &str) -> String {
    let mut rand = [0u8; 4];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut rand));
    if ok.is_err() {
        // fallback: 高精度時刻から導出(実運用で/dev/urandomが開けないことはほぼ無い)
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        rand = (d.as_nanos() as u32).wrapping_mul(2654435761).to_le_bytes();
    }
    let hex: String = rand.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{hex}", batch_date.replace('-', ""))
}

/// 設定読込の共通部。失敗時はstderrへ出力してNone。
fn load_config_or_exit(config: &Option<PathBuf>) -> Option<crate::config::PipelineConfig> {
    let path = config
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PIPELINE_CONFIG));
    match crate::config::load_pipeline(&path) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            eprintln!("error: {e}");
            None
        }
    }
}

/// plan統括(2.1)。戻り値はexit code。
pub fn cmd_plan(args: &PlanArgs) -> i32 {
    let Some(cfg) = load_config_or_exit(&args.config) else {
        return 2;
    };
    let targets = match crate::targets::resolve_targets(&args.paths, args.from_list.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    // 表示に必要なrecipient指紋(設定読込時に検証済み。ここで再計算)
    let mut recipients: Vec<(String, PathBuf)> = Vec::new();
    for key in &cfg.public_keys {
        match crate::fingerprint::parse_ssh_pubkey(key) {
            Ok(pk) => recipients.push((crate::fingerprint::fingerprint(&pk), key.clone())),
            Err(e) => {
                eprintln!("error: {e}");
                return e.exit_code();
            }
        }
    }
    let batch_date = crate::targets::jst_today();
    print!(
        "{}",
        format_plan_report(&targets, &recipients, &cfg.output_dir, &batch_date)
    );
    0
}

/// encrypt統括(2.2)。戻り値はexit code。
pub fn cmd_encrypt(args: &EncryptArgs) -> i32 {
    let Some(cfg) = load_config_or_exit(&args.config) else {
        return 2;
    };
    let out_dir = args.out.clone().unwrap_or_else(|| cfg.output_dir.clone());
    let registry_path = args
        .registry
        .clone()
        .unwrap_or_else(|| cfg.registry_path.clone());
    let keep_going = args.keep_going || cfg.keep_going;

    let targets = match crate::targets::resolve_targets(&args.paths, args.from_list.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };

    let records = match crate::registry::load_registry(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    let reduced = crate::registry::reduce_records(&records);

    let batch_date = crate::targets::jst_today();
    let date_dir = out_dir.join(&batch_date);
    let manifest_path = date_dir.join("manifest.jsonl.age");
    let batch_id = new_batch_id(&batch_date);

    let mut ok_records: Vec<crate::registry::EncryptRecord> = Vec::new();
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut first_err: Option<i32> = None;

    for t in &targets {
        let mtime_str = crate::targets::format_jst(&t.mtime);
        let age_path = date_dir.join(format!("{}.age", t.source_name));
        // skip判定(2.2-1): registryに同一source(path・size・mtime一致)のencrypt記録があり、
        // その記録が指す出力.ageが存在する場合は再読込せずskip
        if let Some(rec) =
            crate::registry::find_match(&reduced, &t.source_path, t.size, &mtime_str)
        {
            if Path::new(&rec.age_path).is_file() {
                println!("skip  {} (already encrypted: registry match)", t.source_name);
                skipped += 1;
                continue;
            }
        }
        match crate::crypto::encrypt_stream(&t.source_path, &age_path, &cfg.public_keys) {
            Ok(res) => {
                println!(
                    "ok    {}  {} -> {}  sha256:{}",
                    t.source_name,
                    fmt_size(t.size),
                    fmt_size(res.age_size),
                    &res.age_sha256.chars().take(16).collect::<String>(),
                );
                let rec = crate::registry::EncryptRecord {
                    schema: 1,
                    record_type: "encrypt".into(),
                    recorded_at: crate::targets::format_jst(&crate::targets::now_jst()),
                    batch_id: batch_id.clone(),
                    batch_date: batch_date.clone(),
                    source_path: t.source_path.display().to_string(),
                    source_name: t.source_name.clone(),
                    source_size: t.size,
                    source_mtime: mtime_str,
                    source_sha256: res.source_sha256,
                    age_path: age_path.display().to_string(),
                    age_size: res.age_size,
                    age_sha256: res.age_sha256,
                    recipients: res.recipients,
                    manifest_path: manifest_path.display().to_string(),
                };
                if let Err(e) = crate::registry::append_record(&registry_path, &rec) {
                    eprintln!("error: {e}");
                    failed += 1;
                    if first_err.is_none() {
                        first_err = Some(e.exit_code());
                    }
                    if !keep_going {
                        return e.exit_code();
                    }
                    continue;
                }
                ok_records.push(rec);
            }
            Err(e) => {
                eprintln!("error: {}: {e}", t.source_path.display());
                failed += 1;
                // 設定エラーは常に即中止(7.2)
                if matches!(e, BackupError::Config(_)) {
                    return e.exit_code();
                }
                if first_err.is_none() {
                    first_err = Some(e.exit_code());
                }
                if !keep_going {
                    return e.exit_code();
                }
            }
        }
    }

    // バッチmanifestの再構築(2.2-6・4.3)。内容はmanifest_path単位のreduce後全encrypt記録
    // (当該実行の成功分 + 過去実行のskip済み分)。生成条件は
    // (i) 当該実行で成功した対象が1件以上、または (ii) reduce後encrypt記録が存在するのに
    // manifestファイルが不在(前回のmanifest生成失敗からの復旧)。
    // 全skipかつmanifest既存の場合は再構築しない(skip実行のage不要を維持。ENC-009)。
    // reduce後encrypt記録が0件(全対象失敗)の場合は生成しない。
    let mut manifest_ok = false;
    {
        let manifest_path_str = manifest_path.display().to_string();
        let batch_records: Vec<crate::registry::EncryptRecord> = {
            // 処理後のregistryを再読込し、このmanifest_pathに属する記録を集める
            // (当該実行の成功分はappend済み。失敗分は記録されていないため含まれない)
            let records_now = match crate::registry::load_registry(&registry_path) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e}");
                    if first_err.is_none() {
                        first_err = Some(e.exit_code());
                    }
                    if !keep_going {
                        return e.exit_code();
                    }
                    Vec::new()
                }
            };
            let reduced_now = crate::registry::reduce_records(&records_now);
            reduced_now
                .encrypts
                .iter()
                .filter(|e| e.manifest_path == manifest_path_str)
                .cloned()
                .collect()
        };
        let need_manifest = !ok_records.is_empty() || !manifest_path.is_file();
        if !batch_records.is_empty() && need_manifest {
            // バッチヘッダのrecipientsはmanifest再構築時の現在設定の指紋列(4.3)
            let mut current_fingerprints = Vec::with_capacity(cfg.public_keys.len());
            let fingerprints_result: BackupResult<()> = (|| {
                for k in &cfg.public_keys {
                    let pk = crate::fingerprint::parse_ssh_pubkey(k)?;
                    current_fingerprints.push(crate::fingerprint::fingerprint(&pk));
                }
                Ok(())
            })();
            match fingerprints_result {
                Err(e) => {
                    eprintln!("error: {e}");
                    if first_err.is_none() {
                        first_err = Some(e.exit_code());
                    }
                    if !keep_going {
                        return e.exit_code();
                    }
                }
                Ok(()) => {
                    let meta = crate::manifest::BatchMeta {
                        batch_id: batch_id.clone(),
                        batch_date: batch_date.clone(),
                        created_at: crate::targets::format_jst(&crate::targets::now_jst()),
                        recipients: current_fingerprints,
                    };
                    let built = crate::manifest::build_manifest(&batch_records, &meta).and_then(
                        |data| {
                            crate::manifest::write_manifest(&manifest_path, &data, &cfg.public_keys)
                        },
                    );
                    match built {
                        Ok(_) => manifest_ok = true,
                        Err(e) => {
                            eprintln!("error: {e}");
                            if first_err.is_none() {
                                first_err = Some(e.exit_code());
                            }
                            if !keep_going {
                                return e.exit_code();
                            }
                        }
                    }
                }
            }
        }
    }

    if manifest_ok {
        println!(
            "-- summary: {} encrypted, {} skipped, {} failed / out: {} / manifest: {}",
            ok_records.len(),
            skipped,
            failed,
            date_dir.display(),
            manifest_path.display()
        );
    } else {
        println!(
            "-- summary: {} encrypted, {} skipped, {} failed / out: {}",
            ok_records.len(),
            skipped,
            failed,
            date_dir.display()
        );
    }
    first_err.unwrap_or(0)
}

/// aws設定の共通merge(2.3・TASK-5 3.1): 「`load_aws`読込 → `require_ready`(toml値の
/// 正検査) → CLI一時上書き適用」。upload/restore系の各サブコマンドが共用する。
/// 戻り値は(実効設定, 正本bucket値)。失敗時はstderrへ出力してNone(いずれも設定エラー2)。
fn merge_aws_config(
    aws_config: &Option<PathBuf>,
    aws_profile: &Option<String>,
    overrides: &crate::config::AwsOverrides,
) -> Option<(crate::config::AwsConfig, String)> {
    let aws_config_path = aws_config
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_AWS_CONFIG));
    // 1. load_awsによるtoml読込
    let aws = match crate::config::load_aws_profile(&aws_config_path, aws_profile.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return None;
        }
    };
    // 2. require_ready: toml値の正検査(bucket/region空はCLI指定の有無にかかわらず設定エラー2)
    if let Err(e) = crate::config::require_ready(&aws) {
        eprintln!("error: {e}");
        return None;
    }
    let canonical_bucket = aws.bucket.clone();
    // 3. CLI一時上書き適用(優先順位: CLI指定値 > aws.toml値・aws.tomlが正本)
    match crate::config::apply_cli_overrides(aws, overrides) {
        Ok(c) => Some((c, canonical_bucket)),
        Err(e) => {
            eprintln!("error: {e}");
            None
        }
    }
}

/// S3 backendの選択(TASK-9設計4.3・9.3)。
/// debug build(`#[cfg(debug_assertions)]`)ではenv `BACKUP_REMOTE_FAKE_S3_STATE`指定時に
/// fake seam(`FakeS3`)へ差し替える(release buildにはseam・fakeともcompileされず
/// 実S3経路になる。cargo testはdev profileのためseam有効)。fake選択時はstderr冒頭へ
/// 「WARNING: FAKE S3 MODE」を必ず表示する(誤用防御)。
/// 実S3 backend(`RealS3`)はcredentialファイル(明示API key)・regionから構築する
/// (credential chain IMDS/env/~/.awsへのfallbackなし・構築失敗は設定エラー2)。
fn select_backend(aws: &crate::config::AwsConfig) -> BackupResult<Box<dyn crate::s3::S3Backend>> {
    warn_endpoint_url_env();
    #[cfg(debug_assertions)]
    {
        if let Some(state) = std::env::var_os("BACKUP_REMOTE_FAKE_S3_STATE") {
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

/// S3 backend構築(merge済み実効aws設定から)。失敗時はstderrへ出力してNone
/// (credentialファイル検証エラー・runtime生成失敗等は設定エラー2)。
fn backend_or_exit(aws: &crate::config::AwsConfig) -> Option<Box<dyn crate::s3::S3Backend>> {
    match select_backend(aws) {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!("error: {e}");
            None
        }
    }
}

/// upload統括(2.3)。戻り値はexit code。
/// aws設定のmerge順は「`load_aws`読込 → `require_ready`(toml値の正検査+credential
/// ファイル検証) → CLI一時上書き適用」(5章cli.rs・2.3 TASK-6節)。CLI上書き値は
/// toml値が正である前提の上書きであり、tomlのbucket/region空はCLI指定の有無に
/// かかわらず設定エラー2(aws.tomlへの書戻しは行わない)。
pub fn cmd_upload(args: &UploadArgs) -> i32 {
    let Some(cfg) = load_config_or_exit(&args.config) else {
        return 2;
    };
    let overrides = crate::config::AwsOverrides {
        storage_class: args.storage_class.clone(),
        bucket: args.bucket.clone(),
        region: args.region.clone(),
    };
    let Some((aws, canonical_bucket)) = merge_aws_config(&args.aws_config, &args.aws_profile, &overrides) else {
        return 2;
    };
    let Some(backend) = backend_or_exit(&aws) else {
        return 2;
    };
    let opts = crate::uploader::UploadOptions {
        aws,
        canonical_bucket,
        registry_path: cfg.registry_path.clone(),
        date: args.date.clone(),
        dry_run: args.dry_run,
        keep_going: args.keep_going || cfg.keep_going,
    };
    crate::uploader::run_upload(backend.as_ref(), &opts)
}

/// verify統括(2.4)。戻り値はexit code。
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
        .filter(|e| args.date.as_deref().map(|d| e.batch_date == d).unwrap_or(true))
        .collect();

    let mut failures: Vec<String> = Vec::new();

    // 段階1: 構造検査(2.4-1)
    for e in &encrypts {
        let age = Path::new(&e.age_path);
        match std::fs::metadata(age) {
            Err(_) => {
                failures.push(format!("{}: .age不在 ({})", e.source_path, e.age_path));
            }
            Ok(m) => {
                // age_sizeは実ファイルsizeと完全一致(2.4-1・VER-002): 記録値未満
                // (切り詰め)・超過(追記)のいずれも不一致とする
                let actual = m.len();
                if actual != e.age_size {
                    failures.push(format!(
                        "{}: age_size不一致 recorded={} actual={} ({})",
                        e.source_path, e.age_size, actual, e.age_path
                    ));
                }
                // recipient照合(2.4-1・4.4・VER-013): .age(manifestの.ageは含まない)の
                // ヘッダをparseし、ssh-ed25519 stanza識別子が記録recipients指紋から導出
                // したv1.2形式指紋prefixと全単射で対応するか照合する(誤recipient検出の
                // --roundtripなし実効化。導出不能・形式不正・stanza0件もfail-closedに4)
                match crate::fingerprint::parse_age_header(age) {
                    Ok(header) => {
                        if let Err(err) = crate::fingerprint::verify_recipients_for_fingerprints(
                            &header,
                            &e.recipients,
                        ) {
                            failures.push(format!(
                                "{}: recipient照合不一致: {err} ({})",
                                e.source_path, e.age_path
                            ));
                        }
                    }
                    Err(err) => {
                        failures.push(format!(
                            "{}: .ageヘッダのparseに失敗: {err} ({})",
                            e.source_path, e.age_path
                        ));
                    }
                }
            }
        }
    }
    // manifest存在検査(reduce後のencrypt記録が存在するバッチのみ。2.4-1)
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

    // 段階2: --checksum(.ageのsha256再計算照合。2.4-2)
    if args.checksum {
        for e in &encrypts {
            match crate::restore::sha256_file(Path::new(&e.age_path)) {
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
        // manifest復号→registry突合(manifest_pathごとのバッチ)
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
                if fr.get("source_sha256").and_then(|v| v.as_str()) != Some(rec.source_sha256.as_str()) {
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

    if failures.is_empty() {
        println!(
            "== verify == ok ({}) records, checksum={}, roundtrip={}",
            encrypts.len(),
            args.checksum,
            args.roundtrip
        );
        0
    } else {
        for f in &failures {
            eprintln!("ng {f}");
        }
        println!(
            "== verify == FAILED: {} 件の不一致({} records)",
            failures.len(),
            encrypts.len()
        );
        4
    }
}

/// restore request統括(2.7・TASK-5 3.2)。戻り値はexit code。
/// aws設定のmergeはcmd_uploadと同一の共用関数(読込→require_ready→CLI一時上書き)。
/// `--days`/`--tier`の値検証はrun_restore_request内(aws実行前)。
pub fn cmd_restore_request(args: &RestoreRequestArgs) -> i32 {
    let overrides = restore_aws_overrides(&args.bucket, &args.region);
    let Some((aws, _)) = merge_aws_config(&args.aws_config, &args.aws_profile, &overrides) else {
        return 2;
    };
    let Some(backend) = backend_or_exit(&aws) else {
        return 2;
    };
    let opts = crate::restore::RestoreRequestOptions {
        aws,
        date: args.date.clone(),
        names: args.name.clone(),
        days: args.days.clone().unwrap_or_else(|| "7".to_string()),
        tier: args.tier.clone().unwrap_or_else(|| "Standard".to_string()),
        dry_run: args.dry_run,
        keep_going: args.keep_going,
    };
    crate::restore::run_restore_request(backend.as_ref(), &opts)
}

/// restore status統括(2.7・TASK-5 3.3)。戻り値はexit code。
pub fn cmd_restore_status(args: &RestoreStatusArgs) -> i32 {
    let overrides = restore_aws_overrides(&args.bucket, &args.region);
    let Some((aws, _)) = merge_aws_config(&args.aws_config, &args.aws_profile, &overrides) else {
        return 2;
    };
    let Some(backend) = backend_or_exit(&aws) else {
        return 2;
    };
    let opts = crate::restore::RestoreStatusOptions {
        aws,
        date: args.date.clone(),
        keep_going: args.keep_going,
    };
    crate::restore::run_restore_status(backend.as_ref(), &opts)
}

/// restore download統括(2.7・TASK-5 3.4)。戻り値はexit code。
/// `--key`未指定時のみpipeline.tomlを読み`[verify].decrypt_key`を解決する(3.1注記。
/// ファイル不在・decrypt_key未設定は「--key未指定」と同扱いの設定エラー2)。
pub fn cmd_restore_download(args: &RestoreDownloadArgs) -> i32 {
    let overrides = restore_aws_overrides(&args.bucket, &args.region);
    let Some((aws, _)) = merge_aws_config(&args.aws_config, &args.aws_profile, &overrides) else {
        return 2;
    };
    let Some(backend) = backend_or_exit(&aws) else {
        return 2;
    };
    let key = match args.key.clone() {
        Some(k) => k,
        None => {
            let path = PathBuf::from(DEFAULT_PIPELINE_CONFIG);
            match crate::config::load_pipeline(&path) {
                Ok(cfg) => match cfg.decrypt_key {
                    Some(k) => k,
                    None => {
                        eprintln!(
                            "error: 設定・環境エラー: 復号用秘密鍵が指定されていません(--key またはpipeline.tomlの[verify].decrypt_key)"
                        );
                        return 2;
                    }
                },
                Err(_) => {
                    eprintln!(
                        "error: 設定・環境エラー: 復号用秘密鍵が指定されていません(--key が必要です。pipeline.toml({})が読めないため[verify].decrypt_keyを解決できません)",
                        path.display()
                    );
                    return 2;
                }
            }
        }
    };
    let opts = crate::restore::RestoreDownloadOptions {
        aws,
        date: args.date.clone(),
        names: args.name.clone(),
        out: args
            .out
            .clone()
            .unwrap_or_else(|| PathBuf::from("data/restore")),
        key,
        dry_run: args.dry_run,
        keep_going: args.keep_going,
    };
    crate::restore::run_restore_download(backend.as_ref(), &opts)
}

/// dispatch(5章)。BackupErrorはmessage+exit code対応をstderrへ出力し、対応exit codeを返す。
/// clapのusage errorは`BackupError::Usage`へ変換しexit 64(7.1)。
pub fn run(argv: impl IntoIterator<Item = std::ffi::OsString>) -> i32 {
    use clap::Parser as _;
    match Cli::try_parse_from(argv) {
        Ok(cli) => match cli.command {
            Command::Plan(a) => cmd_plan(&a),
            Command::Encrypt(a) => cmd_encrypt(&a),
            Command::Upload(a) => cmd_upload(&a),
            Command::Verify(a) => cmd_verify(&a),
            Command::Restore(a) => match &a.command {
                RestoreCommand::Request(r) => cmd_restore_request(r),
                RestoreCommand::Status(s) => cmd_restore_status(s),
                RestoreCommand::Download(d) => cmd_restore_download(d),
            },
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
        path_env_lock, set_path_env, write_file, write_test_ed25519_pub, TempDir,
        TEST_ED25519_FINGERPRINT,
    };

    fn pipeline_toml(dir: &Path) -> PathBuf {
        let pubkey = write_test_ed25519_pub(dir);
        let p = dir.join("pipeline.toml");
        write_file(
            &p,
            format!(
                "[recipients]\npublic_keys = [\"{}\"]\n\
                 [output]\ndir = \"{}\"\nregistry = \"{}\"\n\
                 [behavior]\nkeep_going = false\n\
                 [verify]\ndecrypt_key = \"{}\"\n",
                pubkey.display(),
                dir.join("out").display(),
                dir.join("registry.jsonl").display(),
                dir.join("unused-key").display(),
            )
            .as_bytes(),
        );
        p
    }

    fn set_mtime(path: &Path, jst_rfc3339: &str) {
        let dt = chrono::DateTime::parse_from_rfc3339(jst_rfc3339).expect("rfc3339");
        let t = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(dt.timestamp().max(0) as u64);
        let f = std::fs::OpenOptions::new().write(true).open(path).expect("open for set_times");
        f.set_times(std::fs::FileTimes::new().set_modified(t)).expect("set_times");
    }

    /// [covers:cli.usage-error-exit-64]
    /// 使用法誤り(不明オプション・サブコマンド不足)はexit 64。
    #[test]
    fn usage_error_exits_64() {
        let code = run(["backup-remote".into(), "--definitely-not-an-option".into()]);
        assert_eq!(code, 64, "不明オプション");

        let code = run(["backup-remote".into()]);
        assert_eq!(code, 64, "サブコマンド不足");

        let code = run(["backup-remote".into(), "no-such-subcommand".into()]);
        assert_eq!(code, 64, "未知サブコマンド");
    }

    /// [covers:cli.exit-code-mapping]
    /// 代表ケースで0/2/3/4を検証(5はuploader.rsの転送エラー系テストが担う)。
    #[test]
    fn exit_code_mapping_representative_cases() {
        let tmp = TempDir::new("cli-codes");

        // 2: 設定エラー(不在のpipeline.tomlでplan)
        let code = cmd_plan(&PlanArgs {
            config: Some(tmp.path().join("absent.toml")),
            from_list: None,
            paths: vec![tmp.path().join("x.zip")],
        });
        assert_eq!(code, 2, "設定エラー");

        // 3: 対象エラー(不在ファイルでplan)
        let code = cmd_plan(&PlanArgs {
            config: Some(pipeline_toml(tmp.path())),
            from_list: None,
            paths: vec![tmp.path().join("absent.zip")],
        });
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
        sha2::Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
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
    /// テスト先行注記: case 3(未満)はTASK-2実装(完全一致化)前の現状実装では警告継続で
    /// exit 0となるためfailする(実装後にexit 4を検出してpassとなる想定)。
    #[test]
    fn verify_detects_missing_and_size_mismatch() {
        let tmp = TempDir::new("cli-vermiss");
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
    /// テスト先行注記: 本テストはTASK-2実装(構造検査でのヘッダ照合)に先行して追加しており、
    /// 実装前の現状実装はヘッダを照合しないため(b)(c)の誤りcaseもverifyが通過してしまい
    /// 本テストはfailする(実装後にexit 4を検出してpassとなる想定)。
    #[test]
    fn verify_structure_recipient_bijection() {
        let right = TEST_ED25519_FINGERPRINT.to_string();
        let wrong = other_ed25519_fingerprint();
        assert_ne!(wrong, right, "誤recipient検出用指紋は正指紋と異なる");

        // (a) 正recipient: 記録指紋から導出したprefixと一致するstanza 1つ → exit 0
        let tmp = TempDir::new("cli-verbij-a");
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
        let _ = verify_fixture_with(
            tmp.path(),
            &synthetic_age_bytes(&[
                prefix_of_fingerprint(&right),
                prefix_of_fingerprint(&wrong),
            ]),
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
        let _lock = path_env_lock();
        let tmp = TempDir::new("cli-vernoaws");
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

    /// [covers:plan.recipient-fingerprint-display]
    /// recipient指紋(SHA256:base64)が公開鍵から計算され一覧表示される。
    /// age/awsバイナリ不在(空PATH)でも成功する。
    #[test]
    fn plan_displays_recipient_fingerprints() {
        let _lock = path_env_lock();
        let tmp = TempDir::new("cli-planfpr");
        let target = tmp.path().join("photo.zip");
        write_file(&target, b"photo-bytes");
        let cfg = pipeline_toml(tmp.path());
        let empty_bin = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&empty_bin).expect("empty bin");
        let _guard = set_path_env(empty_bin.to_str().unwrap());

        let code = cmd_plan(&PlanArgs {
            config: Some(cfg),
            from_list: None,
            paths: vec![target],
        });
        assert_eq!(code, 0, "age/aws不在でもplanは成功");

        // 表示内容: 指紋と公開鍵pathが並ぶ
        let pubkey = tmp.path().join("test_ed25519.pub");
        let target = crate::targets::Target {
            source_path: tmp.path().join("photo.zip"),
            source_name: "photo.zip".into(),
            size: 12,
            mtime: chrono::DateTime::parse_from_rfc3339("2026-10-08T12:00:00+09:00").unwrap(),
        };
        let report = format_plan_report(
            &[target],
            &[(TEST_ED25519_FINGERPRINT.to_string(), pubkey.clone())],
            &tmp.path().join("out"),
            "2026-10-08",
        );
        assert!(report.contains(TEST_ED25519_FINGERPRINT), "指紋表示: {report}");
        assert!(report.contains(&pubkey.display().to_string()), "公開鍵path表示: {report}");
        assert!(report.contains("recipients"), "recipient一覧セクション: {report}");
    }

    /// [covers:plan.resolves-positional-targets]
    /// plan表示に各対象のpath・サイズ・mtime・合計サイズが含まれる(解決自体はtargets.rsの検証)。
    #[test]
    fn plan_report_contains_paths_sizes_mtime_total() {
        let tmp = TempDir::new("cli-planrep");
        let t1 = crate::targets::Target {
            source_path: tmp.path().join("photo.zip"),
            source_name: "photo.zip".into(),
            size: 12_500_000_000,
            mtime: chrono::DateTime::parse_from_rfc3339("2026-09-30T22:11:00+09:00").unwrap(),
        };
        let t2 = crate::targets::Target {
            source_path: tmp.path().join("docs.7z"),
            source_name: "docs.7z".into(),
            size: 3_200_000_000,
            mtime: chrono::DateTime::parse_from_rfc3339("2026-10-01T09:00:00+09:00").unwrap(),
        };
        let report = format_plan_report(
            &[t1, t2],
            &[(TEST_ED25519_FINGERPRINT.to_string(), tmp.path().join("k.pub"))],
            &tmp.path().join("out"),
            "2026-10-08",
        );
        assert!(report.contains("photo.zip") && report.contains("docs.7z"), "path: {report}");
        assert!(report.contains("mtime"), "mtime表示: {report}");
        assert!(report.contains("2026-09-30"), "t1のmtime(JST): {report}");
        assert!(report.contains("total"), "合計表示: {report}");
        assert!(report.contains("2026-10-08"), "出力日付: {report}");
    }

    /// [covers:plan.runs-without-aws]
    /// planはaws設定(aws.toml・awsバイナリ)が一切なくてもexit 0となる。
    #[test]
    fn plan_runs_without_aws() {
        let _lock = path_env_lock();
        let tmp = TempDir::new("cli-plannoaws");
        let target = tmp.path().join("a.zip");
        write_file(&target, b"a");
        let empty_bin = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&empty_bin).expect("empty bin");
        let _guard = set_path_env(empty_bin.to_str().unwrap());
        // aws.tomlは配置しない

        let code = cmd_plan(&PlanArgs {
            config: Some(pipeline_toml(tmp.path())),
            from_list: None,
            paths: vec![target],
        });
        assert_eq!(code, 0);
    }

    /// [covers:cli.date-format-usage-error]
    /// upload / verify の--dateはYYYY-MM-DD形式(半角数字4-2-2)を検証し、形式不正は
    /// usage error 64(誤記が対象なしexit 0で静かな未upload/未検証になるのを防ぐ)。
    #[test]
    fn date_format_violation_is_usage_error() {
        for bad in ["2026-1-1", "20260101", "2026/10/01", "2026-10-011", "2026-1-01", "x"] {
            let code = run([
                "backup-remote".into(),
                "upload".into(),
                "--date".into(),
                bad.into(),
            ]);
            assert_eq!(code, 64, "upload --date {bad} はusage error 64");
            let code = run([
                "backup-remote".into(),
                "verify".into(),
                "--date".into(),
                bad.into(),
            ]);
            assert_eq!(code, 64, "verify --date {bad} はusage error 64");
        }

        // 形式どおり(数字4-2-2)はusage errorとならず次段(設定読込等)へ進む
        let code = run([
            "backup-remote".into(),
            "upload".into(),
            "--date".into(),
            "2026-10-09".into(),
            "--config".into(),
            "absent.toml".into(),
        ]);
        assert_ne!(code, 64, "形式どおりはusage errorでない(設定エラー等の別区分)");
    }

    /// [covers:cli.paths-required-usage-error]
    /// plan / encrypt のPATH位置引数0個はusage error(exit 64)。
    /// PATH 1個+--from-list併用はusage errorとならない。
    #[test]
    fn paths_required_usage_error() {
        // PATH 0個(--from-listの有無にかかわらず)は64
        let code = run([
            "backup-remote".into(),
            "plan".into(),
            "--config".into(),
            "some.toml".into(),
        ]);
        assert_eq!(code, 64, "plan PATH 0個");

        let code = run([
            "backup-remote".into(),
            "encrypt".into(),
            "--config".into(),
            "some.toml".into(),
            "--from-list".into(),
            "list.toml".into(),
        ]);
        assert_eq!(code, 64, "encrypt PATH 0個(--from-listのみでも不可)");

        // PATH 1個(+--from-list併用)はusage errorとならない(設定エラー等別区分)
        let code = run([
            "backup-remote".into(),
            "plan".into(),
            "--config".into(),
            "absent.toml".into(),
            "a.zip".into(),
        ]);
        assert_ne!(code, 64, "PATH 1個はusage errorでない(実際は設定エラー2)");

        let code = run([
            "backup-remote".into(),
            "plan".into(),
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
        let _ = verify_fixture(tmp.path());
        let code = cmd_verify(&VerifyArgs {
            config: Some(pipeline_toml(tmp.path())),
            date: None,
            checksum: false,
            roundtrip: true,
            key: Some(tmp.path().join("absent-secret-key")),
        });
        assert_eq!(code, 2, "秘密鍵file不在は設定・環境エラー2");

        // 2. 秘密鍵fileは在るがageバイナリ不在(空PATH)
        let _lock = path_env_lock();
        let tmp2 = TempDir::new("cli-verenv2");
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

    /// [covers:encrypt.skip-on-registry-match]
    /// [covers:encrypt.manifest-not-rewritten-on-all-skip]
    /// .age存在+registry記録一致(path・size・mtime)なら再読込せずskip・exit 0。
    /// registry追記なし・.age不変・manifest不変(全skipかつmanifest既存は再構築しない。
    /// ENC-020)・(空PATHで)age実行不要を確認する。
    #[test]
    fn encrypt_skips_on_registry_match() {
        let _lock = path_env_lock();
        let tmp = TempDir::new("cli-encskip");
        let cfg = pipeline_toml(tmp.path());
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let target = tmp.path().join("a.zip");
        write_file(&target, b"plain-bytes");
        set_mtime(&target, "2026-10-08T09:00:00+09:00");
        // 当日バッチのfixture(JST実行日の日付dir。manifestも既存とする)
        let batch_date = crate::targets::jst_today();
        let date_dir = out.join(&batch_date);
        let age = date_dir.join("a.zip.age");
        let manifest = date_dir.join("manifest.jsonl.age");
        write_file(&age, b"previous-age-bytes");
        write_file(&manifest, b"previous-manifest-bytes");
        let rec = serde_json::json!({
            "schema": 1,
            "record_type": "encrypt",
            "recorded_at": format!("{batch_date}T10:00:00+09:00"),
            "batch_id": format!("{}-abcdef01", batch_date.replace('-', "")),
            "batch_date": batch_date,
            "source_path": target,
            "source_name": "a.zip",
            "source_size": 11,
            "source_mtime": "2026-10-08T09:00:00+09:00",
            "source_sha256": "2".repeat(64),
            "age_path": age,
            "age_size": 19,
            "age_sha256": "3".repeat(64),
            "recipients": ["SHA256:testfingerprint"],
            "manifest_path": manifest,
        });
        write_file(&reg, format!("{rec}\n").as_bytes());

        let empty_bin = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&empty_bin).expect("empty bin");
        let _guard = set_path_env(empty_bin.to_str().unwrap());

        let code = cmd_encrypt(&EncryptArgs {
            config: Some(cfg),
            from_list: None,
            out: Some(out),
            registry: Some(reg.clone()),
            keep_going: false,
            paths: vec![target],
        });
        assert_eq!(code, 0, "all skipならage不在でもexit 0");
        // registry追記なし
        let text = std::fs::read_to_string(&reg).expect("registry read");
        assert_eq!(text.lines().count(), 1, "registry追記なし: {text}");
        // .age・manifestとも不変(全skipかつmanifest既存は再構築しない)
        assert_eq!(std::fs::read(&age).unwrap(), b"previous-age-bytes".to_vec());
        assert_eq!(
            std::fs::read(&manifest).unwrap(),
            b"previous-manifest-bytes".to_vec(),
            "全skipかつmanifest既存はmanifestを書き換えない"
        );
    }
}
