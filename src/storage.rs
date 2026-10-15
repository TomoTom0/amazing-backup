//! TASK-10 CLI層(list/query/get/diff/edit/backup/pin/manage・設計書3章)。
//! 旧サブコマンド(plan/encrypt/upload/verify/restore)はPhase A案aで現行挙動凍結
//! (設計7.3)。照会系はaws.toml+秘密鍵のみで動作(索引はS3上にあるため災害時も成立)。

use crate::config::{self, AwsConfig, PipelineConfig};
use crate::errors::{BackupError, BackupResult};
use crate::index::{self, IndexFileEntry, IndexHeader, IndexPin, IndexVersion};
use crate::layout;
use crate::registry::{self, FileUploadRecord, FileWriteRecord, Reduced};
use crate::s3::{HeadInfo, S3Backend};
use std::collections::BTreeMap;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

/// 既定のaws設定path(cli.rsと同一値・旧側はcli.rsの定数を使う)
const DEFAULT_AWS_CONFIG: &str = "configs/aws.toml";

// ---------------------------------------------------------------------------
// 引数定義(clap)

/// 新体系コマンド共通の設定option群(各subcommandの末尾に置かれる)。
#[derive(clap::Args, Debug)]
pub struct CtxArgs {
    /// pipeline設定(照会系は省略可)
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// aws設定(既定 configs/aws.toml)
    #[arg(long)]
    pub aws_config: Option<PathBuf>,
    /// 復号用SSH ed25519秘密鍵(省略時はpipeline.toml [verify].decrypt_key)
    #[arg(long)]
    pub key: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct ListArgs {
    #[command(subcommand)]
    pub sub: ListSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum ListSub {
    /// 全ファイルの目録
    Files {
        #[command(flatten)]
        ctx: CtxArgs,
        /// 階層表示
        #[arg(long)]
        tree: bool,
        /// category絞り込み
        #[arg(long)]
        category: Option<String>,
    },
    /// ファイル毎の版一覧
    Versions {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        name: Option<String>,
        /// category絞り込み
        #[arg(long)]
        category: Option<String>,
    },
    /// ピン留め版一覧
    Pins {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        name: Option<String>,
    },
    /// run(batch)一覧
    Runs {
        #[command(flatten)]
        ctx: CtxArgs,
    },
    /// 管理区分一覧
    Categories {
        #[command(flatten)]
        ctx: CtxArgs,
    },
    /// 復元job状況(全namespace)
    Jobs {
        #[command(flatten)]
        ctx: CtxArgs,
    },
}

#[derive(clap::Args, Debug)]
pub struct QueryArgs {
    #[command(flatten)]
    pub ctx: CtxArgs,
    /// 探索の手がかり(名前/path部分一致)
    pub term: String,
    #[arg(long)]
    pub category: Option<String>,
    /// 版一覧も表示
    #[arg(long)]
    pub versions: bool,
}

#[derive(clap::Args, Debug)]
pub struct GetArgs {
    #[command(flatten)]
    pub ctx: CtxArgs,
    /// 対象ファイル名(basename)
    pub name: String,
    /// 同名候補のpath特定
    #[arg(long)]
    pub path: Option<String>,
    /// 版の日付(YYYY-MM-DD)
    #[arg(long, value_parser = parse_date)]
    pub version: Option<String>,
    /// 版のversion_id指定
    #[arg(long)]
    pub version_id: Option<String>,
    /// ピン留め版の取得
    #[arg(long = "pin")]
    pub pin: Option<String>,
    /// 取得先dir(既定 data/restore)
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[arg(long)]
    pub tier: Option<String>,
    #[arg(long)]
    pub days: Option<u32>,
}

/// `--version`等の日付形式検証(旧cli.rs parse_date_argと同一規則・usage 64)。
fn parse_date(s: &str) -> Result<String, String> {
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

#[derive(clap::Args, Debug)]
pub struct DiffArgs {
    #[command(flatten)]
    pub ctx: CtxArgs,
    /// run(batch)単位の完否監査
    #[arg(long)]
    pub run: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct BackupArgs {
    #[command(subcommand)]
    pub sub: BackupSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum BackupSub {
    /// 書き込み(暗号化→upload→manifest→索引更新を1実行で)
    Push {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        from_list: Option<PathBuf>,
        /// 対象アーカイブ(zip/7z)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// 予定表示のみ(旧plan相当)
        #[arg(long)]
        dry_run: bool,
        /// backup時ピン(uploadと同時に版名でも上げる)
        #[arg(long)]
        pin: Option<String>,
        #[arg(long)]
        keep_going: bool,
    },
    /// 一括復元(確認付き)
    Pull {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        tier: Option<String>,
        #[arg(long)]
        days: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        /// 確認を省略(非TTYでは必須)
        #[arg(long)]
        yes: bool,
    },
}

#[derive(clap::Args, Debug)]
pub struct PinArgs {
    #[command(subcommand)]
    pub sub: PinSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum PinSub {
    /// ピン留め付与(write-once写し)
    Add {
        #[command(flatten)]
        ctx: CtxArgs,
        name: String,
        #[arg(long = "latest", group = "pin_version")]
        latest: bool,
        #[arg(long, group = "pin_version", value_parser = parse_date)]
        version: Option<String>,
        #[arg(long = "as")]
        as_pin: Option<String>,
    },
    /// ピン留め解除
    Rm {
        #[command(flatten)]
        ctx: CtxArgs,
        name: String,
        #[arg(long = "pin")]
        pin: String,
    },
}

#[derive(clap::Args, Debug)]
pub struct ManageArgs {
    #[command(subcommand)]
    pub sub: ManageSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum ManageSub {
    /// 世代管理の即時実行(dry-run既定)
    Prune {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long, required = true)]
        category: String,
        /// 当該実行のみのkeep上書き
        #[arg(long)]
        keep: Option<u32>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
    },
    /// bucket側保持設定の確認・適用
    Lifecycle {
        #[command(subcommand)]
        sub: LifecycleSub,
    },
    /// 索引の再構築(manifestから)
    Index {
        #[command(subcommand)]
        sub: IndexSub,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum LifecycleSub {
    /// 現在の設定表示
    Show {
        #[command(flatten)]
        ctx: CtxArgs,
    },
    /// rule一式の適用(置換)
    Apply {
        #[command(flatten)]
        ctx: CtxArgs,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum IndexSub {
    /// manifest(DA)から索引を再構築
    Rebuild {
        #[command(flatten)]
        ctx: CtxArgs,
    },
}

#[derive(clap::Args, Debug)]
pub struct EditArgs {
    #[command(flatten)]
    pub ctx: CtxArgs,
    pub target: String,
}

// ---------------------------------------------------------------------------
// 共通helper

struct Session {
    aws: AwsConfig,
    cfg: Option<PipelineConfig>,
    backend: Box<dyn S3Backend>,
}

/// 設定読込+backend構築。cfgは--config指定時のみ読む(照会系はpipeline.toml不要)。
fn open_session(ctx: &CtxArgs) -> Result<Session, BackupError> {
    let cfg = match &ctx.config {
        Some(p) => Some(config::load_pipeline(p)?),
        None => None,
    };
    let aws_path = ctx
        .aws_config
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_AWS_CONFIG));
    let aws = config::load_aws_profile(&aws_path, None, None)?;
    let backend = crate::cli::select_backend(&aws)?;
    Ok(Session { aws, cfg, backend })
}

/// 復号鍵の解決(--key > pipeline.toml [verify].decrypt_key)。
fn resolve_key(ctx: &CtxArgs, cfg: Option<&PipelineConfig>) -> Result<PathBuf, BackupError> {
    if let Some(k) = &ctx.key {
        return Ok(k.clone());
    }
    cfg.and_then(|c| c.decrypt_key.clone()).ok_or_else(|| {
        BackupError::Config(
            "復号用秘密鍵が未指定です(--key または pipeline.toml [verify].decrypt_key)".to_string(),
        )
    })
}

/// 索引の取得・復号・parse(照会系の最初の1 GET・設計2.4)。
fn read_index(
    backend: &dyn S3Backend,
    aws: &AwsConfig,
    key_path: &Path,
) -> BackupResult<(IndexHeader, Vec<IndexFileEntry>)> {
    index::read_index(backend, &aws.bucket, &aws.prefix, key_path)
}

/// 書き込み系コマンドの起動時versioning確認(設計5.1)。無効は設定エラー2+有効化案内。
fn require_versioning(backend: &dyn S3Backend, aws: &AwsConfig) -> Result<(), BackupError> {
    if backend.get_bucket_versioning(&aws.bucket)? {
        return Ok(());
    }
    Err(BackupError::Config(format!(
        "bucket({})のversioningが無効です。有効化してください: aws s3api put-bucket-versioning --bucket {} --versioning-configuration Status=Enabled",
        aws.bucket, aws.bucket
    )))
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex_file(path: &Path) -> BackupResult<String> {
    let bytes = std::fs::read(path).map_err(|e| {
        BackupError::Transfer(format!("取得fileの読込に失敗({}): {e}", path.display()))
    })?;
    Ok(sha256_hex(&bytes))
}

/// JST現在時刻(ISO8601 +09:00付き)。
fn now_jst() -> String {
    crate::targets::format_jst(&crate::targets::now_jst())
}

/// source fileのmtimeをJST・サブ秒精度で文字列化(新経路のskip指紋用)。
/// 旧経路(format_jst)は秒精度へ切捨てるため、同秒内の同サイズ変更がskipされて
/// しまう。新経路は指紋にサブ秒を含めることで「同じsize+mtime秒でも内容が
/// 変わり得る」変更を取りこぼさない(push_versions系の結合test要件)。
fn mtime_jst_precise(path: &Path) -> BackupResult<String> {
    let meta = std::fs::metadata(path).map_err(|e| {
        BackupError::Target(format!("対象のmetadata取得に失敗({}): {e}", path.display()))
    })?;
    let modified = meta
        .modified()
        .map_err(|e| BackupError::Target(format!("mtimeの取得に失敗({}): {e}", path.display())))?;
    let dt: chrono::DateTime<chrono::Utc> = modified.into();
    let jst = dt.with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).expect("JST offset"));
    Ok(jst.format("%Y-%m-%dT%H:%M:%S%.f+09:00").to_string())
}

/// 復元要否の判定と発行(get/pull/pin add共通)。
enum RestoreCheck {
    /// 取得可能(非archive・または復元済み)
    Ready,
    /// 復元jobをこの実行で発行した(案内してexit 0)
    Issued,
    /// 復元jobが進行中(案内してexit 0)
    Ongoing,
}

fn ensure_restored(
    backend: &dyn S3Backend,
    aws: &AwsConfig,
    key: &str,
    version_id: Option<&str>,
    days: u32,
    tier: &str,
) -> BackupResult<RestoreCheck> {
    let head: Option<HeadInfo> = match version_id.filter(|v| !v.is_empty()) {
        Some(vid) => backend.head_version(&aws.bucket, key, vid)?,
        None => backend.head(&aws.bucket, key)?,
    };
    let Some(h) = head else {
        return Err(BackupError::Transfer(format!(
            "対象keyがS3に存在しない(key={key})"
        )));
    };
    let archived = matches!(
        h.storage_class.as_deref(),
        Some("DEEP_ARCHIVE") | Some("GLACIER") | Some("GLACIER_IR")
    );
    if !archived {
        return Ok(RestoreCheck::Ready);
    }
    match h.restore.as_deref() {
        None => {
            match version_id.filter(|v| !v.is_empty()) {
                Some(vid) => backend.restore_version(&aws.bucket, key, vid, days, tier)?,
                None => backend.restore(&aws.bucket, key, days, tier)?,
            };
            Ok(RestoreCheck::Issued)
        }
        Some(r) if r.contains(r#"ongoing-request="true""#) => Ok(RestoreCheck::Ongoing),
        Some(_) => Ok(RestoreCheck::Ready),
    }
}

fn restore_guidance(what: &str) -> String {
    format!("復元jobを発行しました(進行中・約12〜48h)。完了後に再実行してください: {what}")
}

/// 復元job状態の文言(headのRestore headerから)。
fn restore_state(head: Option<&HeadInfo>) -> &'static str {
    match head.and_then(|h| h.restore.as_deref()) {
        None => "not-requested",
        Some(r) if r.contains(r#"ongoing-request="true""#) => "ongoing",
        Some(_) => "restored",
    }
}

/// 取得対象1件(get/pull共通)。
struct FetchTarget {
    s3_key: String,
    version_id: Option<String>,
    age_sha256: String,
    source_sha256: String,
    dest_name: String,
    rel_path: String,
}

impl FetchTarget {
    fn from_version(entry: &IndexFileEntry, v: &IndexVersion) -> FetchTarget {
        FetchTarget {
            s3_key: v.s3_key.clone(),
            version_id: Some(v.version_id.clone()),
            age_sha256: v.age_sha256.clone(),
            source_sha256: v.source_sha256.clone(),
            dest_name: entry.name.clone(),
            rel_path: entry.rel_path.clone(),
        }
    }

    fn from_pin(entry: &IndexFileEntry, p: &IndexPin) -> FetchTarget {
        FetchTarget {
            s3_key: p.s3_key.clone(),
            version_id: None,
            age_sha256: p.age_sha256.clone(),
            source_sha256: p.source_sha256.clone(),
            dest_name: entry.name.clone(),
            rel_path: entry.rel_path.clone(),
        }
    }

    /// 取得→sha検証(索引正本)→復号→<out>/<rel_path>/<name>へ配置。
    fn fetch_verify_decrypt(
        &self,
        backend: &dyn S3Backend,
        aws: &AwsConfig,
        out_dir: &Path,
        key_path: &Path,
    ) -> BackupResult<PathBuf> {
        let tmp_age = std::env::temp_dir().join(format!(
            "backup-remote-get-{}-{}.age",
            std::process::id(),
            sha256_hex(self.s3_key.as_bytes())
                .chars()
                .take(16)
                .collect::<String>()
        ));
        let result = (|| -> BackupResult<PathBuf> {
            match self.version_id.as_deref().filter(|v| !v.is_empty()) {
                Some(vid) => {
                    backend.get_version_to_file(&aws.bucket, &self.s3_key, vid, &tmp_age)?
                }
                None => backend.get_to_file(&aws.bucket, &self.s3_key, &tmp_age)?,
            }
            let age_sha = sha256_hex_file(&tmp_age)?;
            if age_sha != self.age_sha256 {
                return Err(BackupError::Process(format!(
                    "検証エラー: 取得objectのsha256が索引記録と不一致(key={})",
                    self.s3_key
                )));
            }
            let plain = crate::crypto::decrypt_bytes(&tmp_age, key_path)?;
            let source_sha = sha256_hex(&plain);
            if source_sha != self.source_sha256 {
                return Err(BackupError::Process(format!(
                    "検証エラー: 復号平文のsha256が索引記録と不一致(key={})",
                    self.s3_key
                )));
            }
            let mut dest = out_dir.to_path_buf();
            if !self.rel_path.is_empty() {
                for seg in self.rel_path.split('/') {
                    dest.push(seg);
                }
            }
            dest.push(&self.dest_name);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| BackupError::Process(format!("出力先dirの作成に失敗: {e}")))?;
            }
            std::fs::write(&dest, &plain).map_err(|e| {
                BackupError::Process(format!("復元先への書込に失敗({}): {e}", dest.display()))
            })?;
            Ok(dest)
        })();
        let _ = std::fs::remove_file(&tmp_age);
        result
    }
}

/// tier/daysの検証(get/pull共通・旧restore仕様と同一)。
fn resolve_tier_days(tier: Option<&str>, days: Option<u32>) -> Result<(String, u32), BackupError> {
    let tier = tier.unwrap_or("Standard").to_string();
    if !["Standard", "Bulk"].contains(&tier.as_str()) {
        return Err(BackupError::Usage(format!(
            "--tierはStandard/Bulkのいずれかです: {tier}"
        )));
    }
    let days = days.unwrap_or(7);
    if !(1..=365).contains(&days) {
        return Err(BackupError::Config(format!(
            "--daysは1〜365で指定してください: {days}"
        )));
    }
    Ok((tier, days))
}

/// 索引からnameでentryを引き、候補が複数なら--path特定を求める(対象エラー3)。
fn find_entry<'a>(
    entries: &'a [IndexFileEntry],
    name: &str,
    path: Option<&str>,
) -> Result<&'a IndexFileEntry, BackupError> {
    let mut matched = entries.iter().filter(|e| e.name == name);
    if let Some(p) = path {
        return matched
            .find(|e| e.rel_path == p)
            .ok_or_else(|| {
                BackupError::Target(format!(
                    "name={name} path={p} の組は索引にありません(--pathの確認またはqueryで候補を表示してください)"
                ))
            });
    }
    let first = matched.next().ok_or_else(|| {
        BackupError::Target(format!(
            "索引にname={name} がありません(queryで確認してください)"
        ))
    })?;
    if matched.next().is_some() {
        return Err(BackupError::Target(format!(
            "name={name} の候補が複数あります。--path で特定してください(query {name} で候補一覧)"
        )));
    }
    Ok(first)
}

/// 版選択(--version-id > --version日付filter > 既定=最新)。同日複数hitは対象エラー3。
fn select_version<'a>(
    entry: &'a IndexFileEntry,
    version: Option<&str>,
    version_id: Option<&str>,
) -> Result<&'a IndexVersion, BackupError> {
    if let Some(vid) = version_id {
        return entry
            .versions
            .iter()
            .find(|v| v.version_id == vid)
            .ok_or_else(|| {
                BackupError::Target(format!(
                    "version_id={vid} の版は索引にありません(name={})",
                    entry.name
                ))
            });
    }
    if let Some(date) = version {
        let hits: Vec<&IndexVersion> = entry.versions.iter().filter(|v| v.date == date).collect();
        return match hits.as_slice() {
            [one] => Ok(one),
            [] => Err(BackupError::Target(format!(
                "date={date} の版はありません(name={}。候補: {})",
                entry.name,
                entry
                    .versions
                    .iter()
                    .map(|v| v.date.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            many => Err(BackupError::Target(format!(
                "date={date} に複数版があります。--version-id で特定してください(候補: {})",
                many.iter()
                    .map(|v| format!("{} {}", v.version_ts, v.version_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        };
    }
    entry
        .versions
        .first()
        .ok_or_else(|| BackupError::Target(format!("索引に版がありません(name={})", entry.name)))
}

fn fmt_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// 照会系: query / list / get / diff

pub fn cmd_query(args: &QueryArgs) -> i32 {
    match run_query(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

fn run_query(args: &QueryArgs) -> BackupResult<i32> {
    let session = open_session(&args.ctx)?;
    let key_path = resolve_key(&args.ctx, session.cfg.as_ref())?;
    let (_header, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
    let term = args.term.as_str();
    let matched: Vec<&IndexFileEntry> = entries
        .iter()
        .filter(|e| {
            e.name.contains(term) || e.rel_path.contains(term) || e.source_path.contains(term)
        })
        .filter(|e| args.category.as_deref().is_none_or(|c| e.category == c))
        .collect();
    if matched.is_empty() {
        println!("0件 (一致するfileがありません。query <手がかり> で名前/path部分一致を探せます)");
        return Ok(0);
    }
    for e in &matched {
        let latest = e.versions.first();
        println!(
            "{}  {}/{}  {}版  latest {} ({}){}",
            e.name,
            e.category,
            if e.rel_path.is_empty() {
                "-".to_string()
            } else {
                e.rel_path.clone()
            },
            e.versions.len(),
            latest.map(|v| v.date.as_str()).unwrap_or("-"),
            latest
                .map(|v| fmt_size(v.source_size))
                .unwrap_or_else(|| "-".into()),
            if e.pins.is_empty() {
                String::new()
            } else {
                format!("  pins={}", e.pins.len())
            },
        );
        if args.versions {
            for v in &e.versions {
                println!(
                    "    {} {} {} {}",
                    v.date,
                    v.version_ts,
                    v.version_id,
                    fmt_size(v.source_size)
                );
            }
        }
    }
    // 同名候補が複数ある場合はget --path特定の案内
    let mut names: Vec<&str> = matched.iter().map(|e| e.name.as_str()).collect();
    names.sort();
    names.dedup();
    let duplicated = names
        .iter()
        .any(|n| matched.iter().filter(|e| e.name == *n).count() > 1);
    if duplicated {
        println!("同名の候補が複数あります。候補(rel_path / folder)で特定してください:");
        for e in &matched {
            let dup = matched.iter().filter(|o| o.name == e.name).count() > 1;
            if dup {
                println!(
                    "  {}  --path {}  (folder: {})",
                    e.name,
                    if e.rel_path.is_empty() {
                        "\"\""
                    } else {
                        &e.rel_path
                    },
                    if e.folder.is_empty() { "-" } else { &e.folder }
                );
            }
        }
        println!("get <name> --path <rel_path> で特定できます");
    }
    Ok(0)
}

pub fn cmd_list(args: &ListArgs) -> i32 {
    match run_list(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

fn run_list(args: &ListArgs) -> BackupResult<i32> {
    match &args.sub {
        ListSub::Files {
            ctx,
            tree,
            category,
        } => list_files(ctx, *tree, category.as_deref()),
        ListSub::Versions {
            ctx,
            name,
            category,
        } => list_versions(ctx, name.as_deref(), category.as_deref()),
        ListSub::Pins { ctx, name } => list_pins(ctx, name.as_deref()),
        ListSub::Runs { ctx } => list_runs(ctx),
        ListSub::Categories { ctx } => list_categories(ctx),
        ListSub::Jobs { ctx } => list_jobs(ctx),
    }
}

fn list_files(ctx: &CtxArgs, tree: bool, category_filter: Option<&str>) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
    if entries.is_empty() {
        println!("0件 (索引は空です)");
        return Ok(0);
    }
    let mut by_category: BTreeMap<&str, Vec<&IndexFileEntry>> = BTreeMap::new();
    for e in &entries {
        if category_filter.is_some_and(|c| e.category != c) {
            continue;
        }
        by_category.entry(e.category.as_str()).or_default().push(e);
    }
    if by_category.is_empty() {
        println!("0件 (該当categoryのfileがありません)");
        return Ok(0);
    }
    for (category, list) in &by_category {
        let noncurrent: usize = list
            .iter()
            .map(|e| e.versions.len().saturating_sub(1))
            .sum();
        let latest_size: u64 = list
            .iter()
            .filter_map(|e| e.versions.first())
            .map(|v| v.source_size)
            .sum();
        let latest_date = list
            .iter()
            .filter_map(|e| e.versions.first())
            .map(|v| v.date.as_str())
            .max()
            .unwrap_or("-");
        let last_run = list
            .iter()
            .flat_map(|e| e.versions.iter())
            .map(|v| v.batch_id.as_str())
            .max()
            .unwrap_or("-");
        println!(
            "{category}  files={}  noncurrent={noncurrent}  最新日={latest_date}  last run={last_run}  latest計size={}  pins={}",
            list.len(),
            fmt_size(latest_size),
            list.iter().map(|e| e.pins.len()).sum::<usize>(),
        );
    }
    if tree {
        for (category, list) in &by_category {
            for e in list {
                println!(
                    "{}/{}  {}版  latest {}{}",
                    category,
                    if e.rel_path.is_empty() {
                        e.name.clone()
                    } else {
                        format!("{}/{}", e.rel_path, e.name)
                    },
                    e.versions.len(),
                    e.versions.first().map(|v| v.date.as_str()).unwrap_or("-"),
                    if e.pins.is_empty() {
                        String::new()
                    } else {
                        format!("  pins={}", e.pins.len())
                    },
                );
            }
        }
    }
    Ok(0)
}

fn list_versions(
    ctx: &CtxArgs,
    name: Option<&str>,
    category_filter: Option<&str>,
) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
    let targets: Vec<&IndexFileEntry> = match name {
        Some(n) => {
            let e = find_entry(&entries, n, None)?;
            if category_filter.is_some_and(|c| e.category != c) {
                println!("0件 (該当categoryのfileがありません)");
                return Ok(0);
            }
            vec![e]
        }
        None => entries
            .iter()
            .filter(|e| category_filter.is_none_or(|c| e.category == c))
            .collect(),
    };
    if targets.is_empty() {
        println!("0件 (該当するfileがありません)");
        return Ok(0);
    }
    for e in targets {
        println!("{}  ({}/{})", e.name, e.category, print_rel(&e.rel_path));
        let current_vid = e.versions.first().map(|f| f.version_id.as_str());
        for v in &e.versions {
            println!(
                "  {}  {}  {}  {}  {}",
                v.date,
                v.version_ts,
                v.version_id,
                fmt_size(v.source_size),
                if Some(v.version_id.as_str()) == current_vid {
                    "現行"
                } else {
                    "noncurrent"
                },
            );
        }
        for p in &e.pins {
            println!("  pin {}  {}  {}", p.pin, p.date, fmt_size(p.source_size));
        }
    }
    Ok(0)
}

fn print_rel(rel: &str) -> String {
    if rel.is_empty() {
        "-".to_string()
    } else {
        rel.to_string()
    }
}

fn list_pins(ctx: &CtxArgs, name: Option<&str>) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
    let targets: Vec<&IndexFileEntry> = match name {
        Some(n) => vec![find_entry(&entries, n, None)?],
        None => entries.iter().collect(),
    };
    let mut count = 0;
    for e in targets {
        for p in &e.pins {
            println!(
                "{}  {}  {}  {}",
                e.name,
                p.pin,
                p.date,
                fmt_size(p.source_size)
            );
            count += 1;
        }
    }
    if count == 0 {
        println!("0件 (ピン留め版はありません)");
    }
    Ok(0)
}

fn list_runs(ctx: &CtxArgs) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
    let mut runs: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &entries {
        for v in &e.versions {
            *runs.entry(v.batch_id.as_str()).or_insert(0) += 1;
        }
    }
    if runs.is_empty() {
        println!("0件 (索引は空です)");
        return Ok(0);
    }
    for (run, files) in &runs {
        println!("{run}  files={files}");
    }
    Ok(0)
}

fn list_categories(ctx: &CtxArgs) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
    let mut cats: BTreeMap<String, usize> = BTreeMap::new();
    for e in &entries {
        *cats.entry(e.category.clone()).or_insert(0) += 1;
    }
    // 設定のcategories(索引に実体が無くても表示。keepは設定値・災害時設定なしは"-")
    if let Some(cfg) = session.cfg.as_ref() {
        for name in cfg.categories.keys() {
            cats.entry(name.clone()).or_insert(0);
        }
    }
    if cats.is_empty() {
        println!("0件 (索引は空です)");
        return Ok(0);
    }
    for (c, n) in &cats {
        let keep = session
            .cfg
            .as_ref()
            .and_then(|cfg| cfg.categories.get(c))
            .map(|cat| cat.keep.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!("{c}  files={n}  keep={keep}");
    }
    Ok(0)
}

fn list_jobs(ctx: &CtxArgs) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
    let mut keys: Vec<String> = Vec::new();
    for e in &entries {
        for v in &e.versions {
            if !keys.contains(&v.s3_key) {
                keys.push(v.s3_key.clone());
            }
        }
        for p in &e.pins {
            if !keys.contains(&p.s3_key) {
                keys.push(p.s3_key.clone());
            }
        }
    }
    if keys.is_empty() {
        println!("0件 (索引は空です)");
        return Ok(0);
    }
    keys.sort();
    for key in &keys {
        let head = session.backend.head(&session.aws.bucket, key)?;
        println!("{}  {}", key, restore_state(head.as_ref()));
    }
    Ok(0)
}

pub fn cmd_get(args: &GetArgs) -> i32 {
    match run_get(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

fn run_get(args: &GetArgs) -> BackupResult<i32> {
    let (tier, days) = resolve_tier_days(args.tier.as_deref(), args.days)?;
    let session = open_session(&args.ctx)?;
    let key_path = resolve_key(&args.ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
    let entry = find_entry(&entries, &args.name, args.path.as_deref())?;
    let target = if let Some(pin_name) = args.pin.as_deref() {
        let p = entry
            .pins
            .iter()
            .find(|p| p.pin == pin_name)
            .ok_or_else(|| {
                BackupError::Target(format!(
                    "pin名({pin_name})はname={} の索引にありません(list pinsで確認してください)",
                    entry.name
                ))
            })?;
        FetchTarget::from_pin(entry, p)
    } else {
        let v = select_version(entry, args.version.as_deref(), args.version_id.as_deref())?;
        FetchTarget::from_version(entry, v)
    };
    match ensure_restored(
        session.backend.as_ref(),
        &session.aws,
        &target.s3_key,
        target.version_id.as_deref(),
        days,
        &tier,
    )? {
        RestoreCheck::Issued | RestoreCheck::Ongoing => {
            println!(
                "{}",
                restore_guidance(&format!("backup-remote get {}", args.name))
            );
            return Ok(0);
        }
        RestoreCheck::Ready => {}
    }
    let out_dir = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("data/restore"));
    let dest =
        target.fetch_verify_decrypt(session.backend.as_ref(), &session.aws, &out_dir, &key_path)?;
    println!("ok: {}", dest.display());
    Ok(0)
}

pub fn cmd_diff(args: &DiffArgs) -> i32 {
    match run_diff(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

fn run_diff(args: &DiffArgs) -> BackupResult<i32> {
    let session = open_session(&args.ctx)?;
    let key_path = resolve_key(&args.ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
    let versions = session
        .backend
        .list_versions(&session.aws.bucket, &session.aws.prefix)?;
    // 予約領域(sys)は突合対象外(索引・manifestは索引の管理外)
    let reserve_prefix = format!("{}{}/", session.aws.prefix, layout::RESERVE_SEGMENT);
    let is_reserved = |k: &str| k.starts_with(reserve_prefix.as_str());
    let present: std::collections::BTreeSet<&str> = versions
        .iter()
        .map(|v| v.key.as_str())
        .filter(|k| !is_reserved(k))
        .collect();

    if let Some(run) = args.run.as_deref() {
        // 三方突合(索引・manifest・S3実態)でrunの完否を監査(UC-8)
        let mut run_keys: Vec<String> = Vec::new();
        for e in &entries {
            for v in &e.versions {
                if v.batch_id == run && !run_keys.contains(&v.s3_key) {
                    run_keys.push(v.s3_key.clone());
                }
            }
        }
        let mut missing = 0;
        for k in &run_keys {
            if !present.contains(k.as_str()) {
                println!("missing: {k} (run={run})");
                missing += 1;
            }
        }
        let manifest_key = layout::manifest_object_key(&session.aws.prefix, run)?;
        let manifest_ok = session
            .backend
            .head(&session.aws.bucket, &manifest_key)?
            .is_some();
        if !manifest_ok {
            println!("missing: manifest {manifest_key} (run={run})");
        } else {
            println!("manifest: {manifest_key} OK");
        }
        println!(
            "run {run}: files={} missing={missing} manifest={}",
            run_keys.len(),
            if manifest_ok { "OK" } else { "missing" }
        );
        return Ok(0);
    }

    let mut index_keys = std::collections::BTreeSet::new();
    for e in &entries {
        for v in &e.versions {
            index_keys.insert(v.s3_key.clone());
        }
        for p in &e.pins {
            index_keys.insert(p.s3_key.clone());
        }
    }
    let mut missing = 0;
    for k in &index_keys {
        if !present.contains(k.as_str()) {
            println!("missing: {k}");
            missing += 1;
        }
    }
    let mut unexpected = 0;
    for k in &present {
        if !index_keys.contains(*k) {
            println!("unexpected: {k}");
            unexpected += 1;
        }
    }
    println!(
        "diff: entries={} missing={missing} unexpected={unexpected}",
        entries.len()
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// 操作系: backup push / pull

pub fn cmd_backup(args: &BackupArgs) -> i32 {
    match &args.sub {
        BackupSub::Push {
            ctx,
            from_list,
            paths,
            dry_run,
            pin,
            keep_going,
        } => cmd_backup_push(ctx, from_list, paths, *dry_run, pin.as_deref(), *keep_going),
        BackupSub::Pull {
            ctx,
            category,
            run,
            all,
            tier,
            days,
            out,
            dry_run,
            yes,
        } => cmd_backup_pull(
            ctx,
            category.as_deref(),
            run.as_deref(),
            *all,
            tier.as_deref(),
            days.to_owned(),
            out.as_deref(),
            *dry_run,
            *yes,
        ),
    }
}

/// pushの対象1件(解決・分類・衝突folder確定済み)。
struct Planned {
    target: crate::targets::Target,
    category: String,
    rel_path: String,
    identity: String,
    folder: String,
    s3_key: String,
}

fn plan_push(
    targets: &[crate::targets::Target],
    cfg: &PipelineConfig,
    aws_prefix: &str,
    reduced: &Reduced,
) -> Result<Vec<Planned>, BackupError> {
    let files: BTreeMap<String, layout::FileMapping> = cfg
        .files
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                layout::FileMapping {
                    category: v.category.clone(),
                    path: v.path.clone(),
                },
            )
        })
        .collect();
    let unfiled = cfg.layout.unfiled_category.clone();
    let roots = cfg.layout.auto_path_roots.clone();

    struct Resolved {
        idx: usize,
        category: String,
        rel_path: String,
        identity: String,
    }
    let mut resolved: Vec<Resolved> = Vec::with_capacity(targets.len());
    for (idx, t) in targets.iter().enumerate() {
        let c = layout::classify(&t.source_path, &t.source_name, &files, &unfiled, &roots)?;
        resolved.push(Resolved {
            idx,
            category: c.category,
            rel_path: c.rel_path,
            identity: layout::identity_hash(&t.source_path),
        });
    }

    // 衝突folderの決定論的帰属(設計1.2):
    // (1) registryに同一slot(category+rel_path+name)の無hash保持者があれば維持
    // (2) 自identityに前回folderがあればそれを再利用(安定)
    // (3) 新規slotは同一run内の辞書順最小realpathのみ無hash
    let mut planned = Vec::with_capacity(targets.len());
    for r in &resolved {
        let t = &targets[r.idx];
        let slot_plain_holder = reduced.file_writes.iter().find_map(|w| {
            (w.category == r.category
                && w.rel_path == r.rel_path
                && w.source_name == t.source_name
                && w.folder == t.source_name)
                .then_some(w.identity_hash.as_str())
        });
        let folder = if let Some(holder) = slot_plain_holder {
            // 既存の無hash保持者の維持判定はlayout::existing_holder_primaryへ統一(L-12)
            let primary = layout::existing_holder_primary(holder, &r.identity);
            layout::folder_name(&t.source_name, &r.identity, primary)
        } else if let Some(prev) = reduced
            .file_writes
            .iter()
            .find(|w| w.identity_hash == r.identity)
        {
            prev.folder.clone()
        } else {
            // 同一run内の同slot候補で辞書順最小が無hash
            let candidates: Vec<String> = resolved
                .iter()
                .filter(|o| {
                    o.category == r.category
                        && o.rel_path == r.rel_path
                        && targets[o.idx].source_name == t.source_name
                })
                .map(|o| targets[o.idx].source_path.to_string_lossy().into_owned())
                .collect();
            let own = t.source_path.to_string_lossy().into_owned();
            let primary = layout::lexicographic_primary(
                &candidates.iter().map(String::as_str).collect::<Vec<_>>(),
            )
            .is_some_and(|m| m == own);
            layout::folder_name(&t.source_name, &r.identity, primary)
        };
        let key = layout::file_object_key(aws_prefix, &r.category, &r.rel_path, &folder)?;
        planned.push(Planned {
            target: clone_target(t),
            category: r.category.clone(),
            rel_path: r.rel_path.clone(),
            identity: r.identity.clone(),
            folder,
            s3_key: key,
        });
    }
    Ok(planned)
}

/// targets::Targetの簡易clone(derive Cloneが無いため手写し)。
fn clone_target(t: &crate::targets::Target) -> crate::targets::Target {
    crate::targets::Target {
        source_path: t.source_path.clone(),
        source_name: t.source_name.clone(),
        size: t.size,
        mtime: t.mtime,
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_backup_push(
    ctx: &CtxArgs,
    from_list: &Option<PathBuf>,
    paths: &[PathBuf],
    dry_run: bool,
    pin: Option<&str>,
    keep_going: bool,
) -> i32 {
    // pin名は引数形式検証として最初に(usage 64)
    if let Some(pin_name) = pin
        && let Err(e) = layout::validate_pin_name(pin_name)
    {
        eprintln!("error: {e}");
        return e.exit_code();
    }
    let run = (|| -> BackupResult<i32> {
        let cfg = match &ctx.config {
            Some(p) => config::load_pipeline(p)?,
            None => {
                return Err(BackupError::Config(
                    "backup pushには--config(pipeline.toml)が必要です".to_string(),
                ));
            }
        };
        let aws_path = ctx
            .aws_config
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_AWS_CONFIG));
        let aws = config::load_aws_profile(&aws_path, None, None)?;

        let targets = crate::targets::resolve_targets_storage(paths, from_list.as_deref())?;
        let registry_path = cfg.registry_path.clone();
        let records = registry::load_registry(&registry_path)?;
        let reduced = registry::reduce_records(&records);
        let planned = plan_push(&targets, &cfg, &aws.prefix, &reduced)?;

        if dry_run {
            // 旧plan相当: 対象・分類・衝突・size集計・予定keyの表示(S3 API呼び出しなし)
            let total: u64 = planned.iter().map(|p| p.target.size).sum();
            for p in &planned {
                println!(
                    "push  {}  {}/{}  {}  ->  {}",
                    p.target.source_name,
                    p.category,
                    if p.rel_path.is_empty() {
                        "-".to_string()
                    } else {
                        p.rel_path.clone()
                    },
                    fmt_size(p.target.size),
                    p.s3_key
                );
            }
            println!(
                "plan: {}件  計 {}  (dry-run: 暗号化・S3 API呼び出しは行いません)",
                planned.len(),
                fmt_size(total)
            );
            return Ok(0);
        }

        let backend = crate::cli::select_backend(&aws)?;
        require_versioning(backend.as_ref(), &aws)?;

        let batch_date = crate::targets::jst_today();
        let batch_id = format!(
            "{}-{}",
            batch_date.replace('-', ""),
            &sha256_hex(format!("{:?}", std::time::SystemTime::now()).as_bytes())[..8]
        );
        let run_dir = cfg.output_dir.join(&batch_id);
        std::fs::create_dir_all(&run_dir)
            .map_err(|e| BackupError::Process(format!("run dirの作成に失敗: {e}")))?;

        let mut uploaded: Vec<UploadItem> = Vec::new();
        let mut skipped = 0usize;
        let mut skipped_identities: Vec<String> = Vec::new();
        let mut failed = 0usize;
        let mut first_err: Option<i32> = None;

        for p in &planned {
            let mtime_str = mtime_jst_precise(&p.target.source_path)?;
            // skip判定(registry基準・8.3): file_write成功記録+指紋一致で.age不在でもskip
            // skip判定(registry基準・8.3+M-1): file_write成功記録+指紋一致に加え、
            // 同一identityのfile_upload記録(=S3へ実際に載った版)が存在することも確認する
            // (file_write記録後・file_upload記録前にcrashした対象は再uploadする)
            let skip = reduced.file_writes.iter().any(|w| {
                w.identity_hash == p.identity
                    && w.source_size == p.target.size
                    && w.source_mtime == mtime_str
            }) && reduced
                .file_uploads
                .iter()
                .any(|u| u.identity_hash == p.identity && u.s3_key == p.s3_key);
            if skip {
                println!("skip  {} (registry match)", p.target.source_name);
                skipped += 1;
                skipped_identities.push(p.identity.clone());
                continue;
            }
            let age_path = run_dir.join(format!("{}.age", p.target.source_name));
            let res = match crate::crypto::encrypt_stream(
                &p.target.source_path,
                &age_path,
                &cfg.public_keys,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {}: {e}", p.target.source_path.display());
                    failed += 1;
                    if matches!(e, BackupError::Config(_)) {
                        return Err(e);
                    }
                    first_err.get_or_insert(e.exit_code());
                    if !keep_going {
                        return Err(e);
                    }
                    continue;
                }
            };
            let recorded_at = now_jst();
            let version_id =
                match backend.put(&aws.bucket, &p.s3_key, &age_path, &aws.storage_class) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("error: {e}");
                        failed += 1;
                        first_err.get_or_insert(e.exit_code());
                        if !keep_going {
                            return Err(e);
                        }
                        continue;
                    }
                };
            let write = FileWriteRecord {
                schema: 1,
                record_type: "file_write".into(),
                recorded_at: recorded_at.clone(),
                batch_id: batch_id.clone(),
                batch_date: batch_date.clone(),
                source_path: p.target.source_path.display().to_string(),
                source_name: p.target.source_name.clone(),
                source_size: p.target.size,
                source_mtime: mtime_str.clone(),
                source_sha256: res.source_sha256.clone(),
                age_path: age_path.display().to_string(),
                age_size: res.age_size,
                age_sha256: res.age_sha256.clone(),
                recipients: res.recipients.clone(),
                category: p.category.clone(),
                rel_path: p.rel_path.clone(),
                identity_hash: p.identity.clone(),
                folder: p.folder.clone(),
                s3_key: p.s3_key.clone(),
            };
            let upload = FileUploadRecord {
                schema: 1,
                record_type: "file_upload".into(),
                recorded_at: recorded_at.clone(),
                batch_id: batch_id.clone(),
                version_ts: recorded_at.clone(),
                name: p.target.source_name.clone(),
                category: p.category.clone(),
                rel_path: p.rel_path.clone(),
                identity_hash: p.identity.clone(),
                s3_key: p.s3_key.clone(),
                s3_bucket: aws.bucket.clone(),
                s3_region: aws.region.clone(),
                version_id: version_id.clone(),
                s3_size: res.age_size as i64,
                s3_checksum_sha256: res.age_sha256.clone(),
                s3_checksum_type: "FULL_OBJECT".into(),
                source_sha256: res.source_sha256.clone(),
                age_sha256: res.age_sha256.clone(),
                source_size: p.target.size,
                age_size: res.age_size,
                uploaded_at: recorded_at.clone(),
            };
            if let Err(e) = registry::append_record(&registry_path, &write)
                .and_then(|()| registry::append_record(&registry_path, &upload))
            {
                eprintln!("error: {e}");
                failed += 1;
                first_err.get_or_insert(e.exit_code());
                if !keep_going {
                    return Err(e);
                }
                continue;
            }
            uploaded.push(UploadItem {
                name: p.target.source_name.clone(),
                category: p.category.clone(),
                rel_path: p.rel_path.clone(),
                source_path: p.target.source_path.display().to_string(),
                identity: p.identity.clone(),
                folder: p.folder.clone(),
                s3_key: p.s3_key.clone(),
                version_id: version_id.clone(),
                date: batch_date.clone(),
                version_ts: recorded_at.clone(),
                source_sha256: res.source_sha256.clone(),
                age_sha256: res.age_sha256.clone(),
                source_size: p.target.size,
                age_size: res.age_size,
                age_path: age_path.display().to_string(),
            });
            println!(
                "ok    {}  ->  {} (version {})",
                p.target.source_name, p.s3_key, version_id
            );
        }

        // backup時ピン: upload済み.ageを版名keyへも直接upload(復元job不要・CopyObject制約なし)
        if let Some(pin_name) = pin {
            for item in &uploaded {
                let pin_key = layout::pin_object_key(
                    &aws.prefix,
                    &item.category,
                    &item.rel_path,
                    &item.folder,
                    pin_name,
                )?;
                if backend.head(&aws.bucket, &pin_key)?.is_some() {
                    return Err(BackupError::Target(format!(
                        "pin名({pin_name})は既に存在します(write-once)。pin rm後に再付与してください: {pin_key}"
                    )));
                }
                let pin_vid = backend.put(
                    &aws.bucket,
                    &pin_key,
                    Path::new(&item.age_path),
                    &aws.storage_class,
                )?;
                let recorded = registry::PinWriteRecord {
                    schema: 1,
                    record_type: "pin_write".into(),
                    recorded_at: now_jst(),
                    name: item.name.clone(),
                    category: item.category.clone(),
                    rel_path: item.rel_path.clone(),
                    folder: item.folder.clone(),
                    pin: pin_name.to_string(),
                    pin_s3_key: pin_key.clone(),
                    source_s3_key: item.s3_key.clone(),
                    source_version_id: item.version_id.clone(),
                    date: item.date.clone(),
                    source_sha256: item.source_sha256.clone(),
                    age_sha256: item.age_sha256.clone(),
                    source_size: item.source_size,
                    s3_bucket: aws.bucket.clone(),
                    s3_size: item.age_size as i64,
                    s3_checksum_sha256: item.age_sha256.clone(),
                    uploaded_at: now_jst(),
                };
                registry::append_record(&registry_path, &recorded)?;
                println!("ok    pin {pin_name} -> {pin_key} (version {pin_vid})");
            }

            // skip対象(変更なし)への--pin指定: 警告表示のうえ最新版へpin付与(M-6。
            // 当該runに新規.ageが無いためCopyObjectで写す)
            if !skipped_identities.is_empty() {
                println!(
                    "WARNING: --pin指定の対象に変更なし(skip)のfileが{}件あります。最新版へpinを付与します",
                    skipped_identities.len()
                );
                for identity in &skipped_identities {
                    let Some(write) = reduced
                        .file_writes
                        .iter()
                        .find(|w| &w.identity_hash == identity)
                    else {
                        continue;
                    };
                    // 当該identityの最新版(registry記録順の最後)
                    let Some(version) = reduced
                        .file_uploads
                        .iter()
                        .rfind(|u| &u.identity_hash == identity)
                    else {
                        continue;
                    };
                    let pin_key = layout::pin_object_key(
                        &aws.prefix,
                        &write.category,
                        &write.rel_path,
                        &write.folder,
                        pin_name,
                    )?;
                    if backend.head(&aws.bucket, &pin_key)?.is_some() {
                        eprintln!(
                            "WARNING: pin名({pin_name})は既に存在します(write-once)。skip対象のpin付与を飛ばします: {pin_key}"
                        );
                        continue;
                    }
                    match ensure_restored(
                        backend.as_ref(),
                        &aws,
                        &version.s3_key,
                        Some(&version.version_id),
                        7,
                        "Standard",
                    )? {
                        RestoreCheck::Issued | RestoreCheck::Ongoing => {
                            println!(
                                "{}",
                                restore_guidance(&format!(
                                    "pin add {} --as {pin_name}",
                                    write.source_name
                                ))
                            );
                            continue;
                        }
                        RestoreCheck::Ready => {}
                    }
                    backend.copy_object(
                        &aws.bucket,
                        &version.s3_key,
                        Some(&version.version_id),
                        &pin_key,
                        &aws.storage_class,
                    )?;
                    registry::append_record(
                        &registry_path,
                        &registry::PinWriteRecord {
                            schema: 1,
                            record_type: "pin_write".into(),
                            recorded_at: now_jst(),
                            name: write.source_name.clone(),
                            category: write.category.clone(),
                            rel_path: write.rel_path.clone(),
                            folder: write.folder.clone(),
                            pin: pin_name.to_string(),
                            pin_s3_key: pin_key.clone(),
                            source_s3_key: version.s3_key.clone(),
                            source_version_id: version.version_id.clone(),
                            date: version.recorded_at.get(..10).unwrap_or("").to_string(),
                            source_sha256: version.source_sha256.clone(),
                            age_sha256: version.age_sha256.clone(),
                            source_size: version.source_size,
                            s3_bucket: aws.bucket.clone(),
                            s3_size: version.age_size as i64,
                            s3_checksum_sha256: version.age_sha256.clone(),
                            uploaded_at: now_jst(),
                        },
                    )?;
                    println!("ok    pin {pin_name} -> {pin_key} (skip対象の最新版)");
                }
            }
        }

        // run manifest(F8: file upload後に構築。file recordに実s3_key+version_idを記録)
        if !uploaded.is_empty() {
            let manifest_body = crate::manifest::build_manifest_v2(
                &batch_id,
                &now_jst(),
                &uploaded
                    .iter()
                    .map(|u| crate::manifest::ManifestV2File {
                        name: u.name.clone(),
                        category: u.category.clone(),
                        rel_path: u.rel_path.clone(),
                        source_path: u.source_path.clone(),
                        identity_hash: u.identity.clone(),
                        folder: u.folder.clone(),
                        s3_key: u.s3_key.clone(),
                        version_id: u.version_id.clone(),
                        date: u.date.clone(),
                        version_ts: u.version_ts.clone(),
                        batch_id: batch_id.clone(),
                        source_sha256: u.source_sha256.clone(),
                        age_sha256: u.age_sha256.clone(),
                        source_size: u.source_size,
                        age_size: u.age_size,
                    })
                    .collect::<Vec<_>>(),
            )?;
            let manifest_local = run_dir.join("manifest.jsonl.age");
            crate::crypto::encrypt_bytes(
                manifest_body.as_bytes(),
                &manifest_local,
                &cfg.public_keys,
            )?;
            let manifest_key = layout::manifest_object_key(&aws.prefix, &batch_id)?;
            let m_vid = backend.put(
                &aws.bucket,
                &manifest_key,
                &manifest_local,
                &aws.storage_class,
            )?;
            let m_size = std::fs::metadata(&manifest_local)
                .map(|m| m.len() as i64)
                .unwrap_or(-1);
            registry::append_record(
                &registry_path,
                &registry::ManifestV2Record {
                    schema: 1,
                    record_type: "manifest".into(),
                    recorded_at: now_jst(),
                    batch_id: batch_id.clone(),
                    s3_key: manifest_key.clone(),
                    s3_bucket: aws.bucket.clone(),
                    s3_region: aws.region.clone(),
                    version_id: m_vid,
                    s3_size: m_size,
                    s3_checksum_sha256: String::new(),
                    s3_checksum_type: "FULL_OBJECT".into(),
                    uploaded_at: now_jst(),
                },
            )?;
        }

        // 索引の再構築+配置(成功run末尾で常時・冪等)。前回runがregistry記録後に失敗した
        // 場合の再実行が全skipでも、ここで未反映分を拾って索引が更新される(H-2)
        {
            let records_now = registry::load_registry(&registry_path)?;
            let reduced_now = registry::reduce_records(&records_now);
            let entries = index::build_entries(&reduced_now);
            index::upload_index(
                backend.as_ref(),
                &aws.bucket,
                &aws.prefix,
                &cfg.public_keys,
                &entries,
                &batch_id,
                &now_jst(),
                &registry_path,
            )?;
        }

        // data/out掃除(keep_artifacts="run"・run成功時のみ)。掃除対象は当該run分
        // (<batch_id> dir)のみ(Phase A併存で旧経路産物・失敗runの残りを消さない)
        if failed == 0 && matches!(cfg.keep_artifacts, config::KeepArtifacts::Run) {
            let _ = std::fs::remove_dir_all(&run_dir);
        }

        println!(
            "run {batch_id}: ok={} skip={} failed={}",
            uploaded.len(),
            skipped,
            failed
        );
        if let Some(code) = first_err {
            return Ok(code);
        }
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

/// pushでuploadした対象1件(manifest・pin組み立て用)。
struct UploadItem {
    name: String,
    category: String,
    rel_path: String,
    source_path: String,
    identity: String,
    folder: String,
    s3_key: String,
    version_id: String,
    date: String,
    version_ts: String,
    source_sha256: String,
    age_sha256: String,
    source_size: u64,
    age_size: u64,
    age_path: String,
}

#[allow(clippy::too_many_arguments)]
fn cmd_backup_pull(
    ctx: &CtxArgs,
    category: Option<&str>,
    run: Option<&str>,
    all: bool,
    tier: Option<&str>,
    days: Option<u32>,
    out: Option<&Path>,
    dry_run: bool,
    yes: bool,
) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let scope_count =
            usize::from(all) + usize::from(category.is_some()) + usize::from(run.is_some());
        if scope_count == 0 {
            return Err(BackupError::Usage(
                "backup pullには範囲指定(--all / --category C / --run ID)が必要です".to_string(),
            ));
        }
        if scope_count > 1 {
            return Err(BackupError::Usage(
                "--all / --category / --run は併用できません(いずれか1つを指定してください)"
                    .to_string(),
            ));
        }
        // 確認ガード(設計3.2・事故防止はscope確認)。索引GETの前に判定する
        if !yes {
            use std::io::Write as _;
            if !std::io::stdin().is_terminal() {
                return Err(BackupError::Usage(
                    "backup pullは一括復元です。非TTY実行では --yes が必要です(対話確認はTTYのみ)"
                        .to_string(),
                ));
            }
            print!("backup pullは対象を一括で復元します。続行しますか (yes/no): ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| BackupError::Config(format!("確認入力の読込に失敗: {e}")))?;
            let answer = line.trim().to_ascii_lowercase();
            if answer != "yes" && answer != "y" {
                println!("中止しました (no)");
                return Ok(0);
            }
        }
        let (tier, days) = resolve_tier_days(tier, days)?;
        let session = open_session(ctx)?;
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;

        let mut targets: Vec<FetchTarget> = Vec::new();
        if let Some(run_id) = run {
            for e in &entries {
                for v in &e.versions {
                    if v.batch_id == run_id {
                        targets.push(FetchTarget::from_version(e, v));
                    }
                }
            }
            if targets.is_empty() {
                println!(
                    "0件 (run={run_id} の対象は索引にありません。list runsで確認してください)"
                );
                return Ok(0);
            }
        } else {
            let want_category = category;
            let want_all = all;
            for e in &entries {
                if !want_all && !want_category.is_some_and(|c| e.category == c) {
                    continue;
                }
                if let Some(v) = e.versions.first() {
                    targets.push(FetchTarget::from_version(e, v));
                }
                for p in &e.pins {
                    targets.push(FetchTarget::from_pin(e, p));
                }
            }
            if targets.is_empty() {
                println!("0件 (対象がありません。list files / query で確認してください)");
                return Ok(0);
            }
        }
        if dry_run {
            for t in &targets {
                println!("pull  {}  {}", t.dest_name, t.s3_key);
            }
            println!("plan: {}件 (dry-run)", targets.len());
            return Ok(0);
        }
        let out_dir = out
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data/restore"));
        let mut pending = 0usize;
        let mut ok = 0usize;
        let mut first_err: Option<i32> = None;
        for t in &targets {
            match ensure_restored(
                session.backend.as_ref(),
                &session.aws,
                &t.s3_key,
                t.version_id.as_deref(),
                days,
                &tier,
            ) {
                Ok(RestoreCheck::Issued) | Ok(RestoreCheck::Ongoing) => {
                    pending += 1;
                    continue;
                }
                Ok(RestoreCheck::Ready) => {}
                Err(e) => {
                    eprintln!("error: {e}");
                    first_err.get_or_insert(e.exit_code());
                    continue;
                }
            }
            match t.fetch_verify_decrypt(
                session.backend.as_ref(),
                &session.aws,
                &out_dir,
                &key_path,
            ) {
                Ok(dest) => {
                    println!("ok: {}", dest.display());
                    ok += 1;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    first_err.get_or_insert(e.exit_code());
                }
            }
        }
        if pending > 0 {
            println!(
                "{}",
                restore_guidance("backup pull (復元job発行済みの対象は約12〜48h後に再実行)")
            );
        }
        println!(
            "pull: ok={ok} pending={pending} failed={}",
            targets.len() - ok - pending
        );
        if let Some(code) = first_err {
            return Ok(code);
        }
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

// ---------------------------------------------------------------------------
// pin add / rm

pub fn cmd_pin(args: &PinArgs) -> i32 {
    match &args.sub {
        PinSub::Add {
            ctx,
            name,
            version,
            as_pin,
            ..
        } => cmd_pin_add(ctx, name, version.as_deref(), as_pin.as_deref()),
        PinSub::Rm { ctx, name, pin } => cmd_pin_rm(ctx, name, pin),
    }
}

fn cmd_pin_add(ctx: &CtxArgs, name: &str, version: Option<&str>, as_pin: Option<&str>) -> i32 {
    // pin名は引数形式検証として最初に(usage 64)
    let Some(pin_name) = as_pin else {
        eprintln!("error: pin名の指定(--as)が必要です");
        return 64;
    };
    if let Err(e) = layout::validate_pin_name(pin_name) {
        eprintln!("error: {e}");
        return e.exit_code();
    }
    let run = (|| -> BackupResult<i32> {
        // registryへの記録を要するため--config必須。S3変更(copy_object)より前に解決して
        // おく(M-4: copy後にregistry解決失敗するとwrite-once滞留で手詰まりになる)
        if ctx.config.is_none() {
            return Err(BackupError::Config(
                "pin addには--config(pipeline.toml・registry記録先)が必要です".to_string(),
            ));
        }
        let session = open_session(ctx)?;
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        let registry_path = session
            .cfg
            .as_ref()
            .map(|c| c.registry_path.clone())
            .expect("configは必須検査済み");
        require_versioning(session.backend.as_ref(), &session.aws)?;
        let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
        let entry = find_entry(&entries, name, None)?;
        let v = if let Some(date) = version {
            select_version(entry, Some(date), None)?
        } else {
            // 既定=最新
            select_version(entry, None, None)?
        };
        // write-once確認(既存pin keyへの再付与拒否)
        let pin_key = layout::pin_object_key(
            &session.aws.prefix,
            &entry.category,
            &entry.rel_path,
            &entry.folder,
            pin_name,
        )?;
        if session
            .backend
            .head(&session.aws.bucket, &pin_key)?
            .is_some()
        {
            return Err(BackupError::Target(format!(
                "pin名({pin_name})は既に存在します(write-once)。上書きする場合は pin rm {name} --pin {pin_name} 後に再付与してください: {pin_key}"
            )));
        }
        match ensure_restored(
            session.backend.as_ref(),
            &session.aws,
            &v.s3_key,
            Some(&v.version_id),
            7,
            "Standard",
        )? {
            RestoreCheck::Issued | RestoreCheck::Ongoing => {
                println!(
                    "{}",
                    restore_guidance(&format!("pin add {name} --as {pin_name}"))
                );
                return Ok(0);
            }
            RestoreCheck::Ready => {}
        }
        session.backend.copy_object(
            &session.aws.bucket,
            &v.s3_key,
            Some(&v.version_id),
            &pin_key,
            &session.aws.storage_class,
        )?;
        let head = session
            .backend
            .head(&session.aws.bucket, &pin_key)?
            .ok_or_else(|| BackupError::Transfer(format!("pin objectの確認に失敗: {pin_key}")))?;
        let recorded_at = now_jst();
        registry::append_record(
            &registry_path,
            &registry::PinWriteRecord {
                schema: 1,
                record_type: "pin_write".into(),
                recorded_at: recorded_at.clone(),
                name: entry.name.clone(),
                category: entry.category.clone(),
                rel_path: entry.rel_path.clone(),
                folder: entry.folder.clone(),
                pin: pin_name.to_string(),
                pin_s3_key: pin_key.clone(),
                source_s3_key: v.s3_key.clone(),
                source_version_id: v.version_id.clone(),
                date: v.date.clone(),
                source_sha256: v.source_sha256.clone(),
                age_sha256: v.age_sha256.clone(),
                source_size: v.source_size,
                s3_bucket: session.aws.bucket.clone(),
                s3_size: head.size,
                s3_checksum_sha256: head.checksum_sha256.clone().unwrap_or_default(),
                uploaded_at: recorded_at,
            },
        )?;
        rebuild_and_upload_index(&session, &registry_path, "pin-add")?;
        println!("ok: pin {pin_name} -> {pin_key}");
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

fn cmd_pin_rm(ctx: &CtxArgs, name: &str, pin_name: &str) -> i32 {
    let run = (|| -> BackupResult<i32> {
        // registryへの記録を要するため--config必須(S3変更より前に解決・M-4)
        if ctx.config.is_none() {
            return Err(BackupError::Config(
                "pin rmには--config(pipeline.toml・registry記録先)が必要です".to_string(),
            ));
        }
        let session = open_session(ctx)?;
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        let registry_path = session
            .cfg
            .as_ref()
            .map(|c| c.registry_path.clone())
            .expect("configは必須検査済み");
        require_versioning(session.backend.as_ref(), &session.aws)?;
        let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
        let entry = find_entry(&entries, name, None)?;
        let pin = entry
            .pins
            .iter()
            .find(|p| p.pin == pin_name)
            .ok_or_else(|| {
                BackupError::Target(format!(
                    "pin名({pin_name})はname={name} の索引にありません(list pinsで確認してください)"
                ))
            })?;
        // DA配置のpinも復元jobなしで削除できる(deleteはarchive objectへ可能)
        session
            .backend
            .delete_object(&session.aws.bucket, &pin.s3_key)?;
        registry::append_record(
            &registry_path,
            &registry::PinRemoveRecord {
                schema: 1,
                record_type: "pin_remove".into(),
                recorded_at: now_jst(),
                name: name.to_string(),
                pin: pin_name.to_string(),
                pin_s3_key: pin.s3_key.clone(),
                s3_bucket: session.aws.bucket.clone(),
            },
        )?;
        rebuild_and_upload_index(&session, &registry_path, "pin-rm")?;
        println!("ok: pin {pin_name} を削除しました ({})", pin.s3_key);
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

/// registryから索引を再構築してS3へ配置する(pin/prune等の索引影響操作後)。
fn rebuild_and_upload_index(
    session: &Session,
    registry_path: &Path,
    run_tag: &str,
) -> BackupResult<()> {
    let cfg = session
        .cfg
        .as_ref()
        .ok_or_else(|| BackupError::Config("pipeline.tomlが必要です".to_string()))?;
    let records = registry::load_registry(registry_path)?;
    let reduced = registry::reduce_records(&records);
    let entries = index::build_entries(&reduced);
    index::upload_index(
        session.backend.as_ref(),
        &session.aws.bucket,
        &session.aws.prefix,
        &cfg.public_keys,
        &entries,
        run_tag,
        &now_jst(),
        registry_path,
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// manage系

pub fn cmd_manage(args: &ManageArgs) -> i32 {
    match &args.sub {
        ManageSub::Prune {
            ctx,
            category,
            keep,
            dry_run,
            apply,
        } => cmd_manage_prune(ctx, category, *keep, *dry_run, *apply),
        ManageSub::Lifecycle { sub } => match sub {
            LifecycleSub::Show { ctx } => cmd_manage_lifecycle(ctx, &LifecycleOp::Show),
            LifecycleSub::Apply { ctx } => cmd_manage_lifecycle(ctx, &LifecycleOp::Apply),
        },
        ManageSub::Index { sub } => match sub {
            IndexSub::Rebuild { ctx } => cmd_manage_index_rebuild(ctx),
        },
    }
}

fn cmd_manage_prune(
    ctx: &CtxArgs,
    category: &str,
    keep_override: Option<u32>,
    dry_run: bool,
    apply: bool,
) -> i32 {
    let run = (|| -> BackupResult<i32> {
        if dry_run && apply {
            return Err(BackupError::Usage(
                "--dry-runと--applyは併用できません(既定はdry-run・実行は--apply)".to_string(),
            ));
        }
        let session = open_session(ctx)?;
        let cfg = session.cfg.as_ref().ok_or_else(|| {
            BackupError::Config("manage pruneには--config(pipeline.toml)が必要です".to_string())
        })?;
        // categoryの検証(M-2: 予約語(sys)・旧表記(_sys)・形式違反・未定義categoryを
        // ListObjectVersionsより前に弾く)
        layout::validate_category_name(category)
            .map_err(|e| BackupError::Config(format!("--categoryが不正です: {e}")))?;
        if !cfg.categories.contains_key(category) {
            return Err(BackupError::Config(format!(
                "category({category})は[categories]に定義されていません"
            )));
        }
        let keep = match keep_override {
            Some(k) => {
                if !(1..=100).contains(&k) {
                    return Err(BackupError::Config(format!(
                        "--keepは1以上100以下で指定してください: {k}"
                    )));
                }
                k
            }
            None => cfg
                .categories
                .get(category)
                .map(|c| c.keep)
                .ok_or_else(|| {
                    BackupError::Config(format!(
                        "category({category})は[categories]に定義されていません"
                    ))
                })?,
        };
        // 対象計算: ListObjectVersions。同一keyのnoncurrent版を新しい順に並べ、
        // newer noncurrent数がkeep以上となった版を削除対象とする(NCVEと同一定義・5.2)。
        // 現行version(is_latest)・pin key(default.age以外)は対象外(構造的保護)。
        let prefix = format!("{}{}/", session.aws.prefix, category);
        let versions = session
            .backend
            .list_versions(&session.aws.bucket, &prefix)?;
        let mut by_key: BTreeMap<String, Vec<&crate::s3::VersionInfo>> = BTreeMap::new();
        for v in &versions {
            by_key.entry(v.key.clone()).or_default().push(v);
        }
        let mut deletions: Vec<(String, String)> = Vec::new();
        for (key, mut vs) in by_key {
            if !key.ends_with("/default.age") {
                continue; // pin key等は構造的保護
            }
            // listは新しい順に揃える(実装差を吸収するため明示sort・安定)。
            // 現行(is_latest)を除外した残り=noncurrent(新しい順)
            vs.sort_by(|a, b| b.last_modified.cmp(&a.last_modified));
            vs.retain(|v| !v.is_latest && !v.is_delete_marker);
            for (i, v) in vs.iter().enumerate() {
                if i >= keep as usize {
                    deletions.push((key.clone(), v.version_id.clone()));
                }
            }
        }
        if dry_run || !apply {
            for (key, vid) in &deletions {
                println!("[dry-run] delete {key} version_id={vid}");
            }
            println!(
                "[dry-run] manage prune: category={category} keep={keep} 削除対象={}版 (実行は --apply)",
                deletions.len()
            );
            return Ok(0);
        }
        let registry_path = cfg.registry_path.clone();
        for (key, vid) in &deletions {
            session
                .backend
                .delete_object_version(&session.aws.bucket, key, vid)?;
            println!("delete {key} version_id={vid}");
        }
        registry::append_record(
            &registry_path,
            &registry::PruneRecord {
                schema: 1,
                record_type: "prune".into(),
                recorded_at: now_jst(),
                category: category.to_string(),
                keep,
                deleted: deletions
                    .iter()
                    .map(|(k, v)| registry::PruneDeleted {
                        s3_key: k.clone(),
                        version_id: v.clone(),
                    })
                    .collect(),
            },
        )?;
        // 索引の再構築・配置(削除済み版の除外はbuild_entriesがprune記録で行う)
        let records = registry::load_registry(&registry_path)?;
        let reduced = registry::reduce_records(&records);
        let entries = index::build_entries(&reduced);
        index::upload_index(
            session.backend.as_ref(),
            &session.aws.bucket,
            &session.aws.prefix,
            &cfg.public_keys,
            &entries,
            "prune",
            &now_jst(),
            &registry_path,
        )?;
        println!("deleted {} version(s)", deletions.len());
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

/// lifecycle操作(show|apply)。引数構造の都合でleafで受けてopへ写像する。
enum LifecycleOp {
    Show,
    Apply,
}

fn cmd_manage_lifecycle(ctx: &CtxArgs, op: &LifecycleOp) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let session = open_session(ctx)?;
        let cfg = session.cfg.as_ref().ok_or_else(|| {
            BackupError::Config("manage lifecycleには--config(pipeline.toml)が必要です".to_string())
        })?;
        match op {
            LifecycleOp::Show => {
                let rules = session.backend.get_lifecycle(&session.aws.bucket)?;
                match rules {
                    None => println!("未設定 (manage lifecycle apply で適用できます)"),
                    Some(rules) => {
                        for r in &rules {
                            println!("{}", format_spec(r));
                        }
                    }
                }
                Ok(0)
            }
            LifecycleOp::Apply => {
                // category毎NCVE rule(prefix filterで予約領域は構造的に対象外)+MPU abort
                let mut rules: Vec<crate::s3::LifecycleSpec> = Vec::new();
                for (name, cat) in &cfg.categories {
                    rules.push(crate::s3::LifecycleSpec::Noncurrent {
                        prefix: format!("{}{}/", session.aws.prefix, name),
                        noncurrent_days: 180,
                        newer_noncurrent_versions: cat.keep as i32,
                    });
                }
                rules.push(crate::s3::LifecycleSpec::MpuAbort {
                    prefix: session.aws.prefix.clone(),
                    days_after_initiation: 7,
                });
                for r in &rules {
                    println!("{}", format_spec(r));
                }
                session.backend.put_lifecycle(&session.aws.bucket, &rules)?;
                println!("lifecycle設定を適用しました(rule一式の置換・冪等)");
                Ok(0)
            }
        }
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

fn format_spec(r: &crate::s3::LifecycleSpec) -> String {
    match r {
        crate::s3::LifecycleSpec::Noncurrent {
            prefix,
            noncurrent_days,
            newer_noncurrent_versions,
        } => format!(
            "NCVE {prefix} NoncurrentDays={noncurrent_days} NewerNoncurrentVersions={newer_noncurrent_versions}"
        ),
        crate::s3::LifecycleSpec::MpuAbort {
            prefix,
            days_after_initiation,
        } => format!("AbortIncompleteMultipartUpload {prefix} {days_after_initiation}日"),
    }
}

fn cmd_manage_index_rebuild(ctx: &CtxArgs) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let session = open_session(ctx)?;
        let cfg = session.cfg.as_ref().ok_or_else(|| {
            BackupError::Config(
                "manage index rebuildには--config(pipeline.toml)が必要です".to_string(),
            )
        })?;
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        println!("索引をmanifest(sys/manifests/)から再構築します");
        println!("復元費用の目安: Standard ≈$0.10/GB(最大12時間) / Bulk ≈$0.02/GB(最大48時間)");

        let manifests_prefix = format!(
            "{}{}/manifests/",
            session.aws.prefix,
            layout::RESERVE_SEGMENT
        );
        let manifest_keys = session
            .backend
            .list_keys(&session.aws.bucket, &manifests_prefix)?;
        if manifest_keys.is_empty() {
            println!("0件 (manifestがありません。push実行後に再実行してください)");
            return Ok(0);
        }

        // 未復元manifestへの復元job発行(Bulk既定)。発行したら案内してexit 0(再実行で継続)
        let mut issued = 0usize;
        for key in &manifest_keys {
            match ensure_restored(session.backend.as_ref(), &session.aws, key, None, 7, "Bulk")? {
                RestoreCheck::Issued => issued += 1,
                RestoreCheck::Ongoing => issued += 1,
                RestoreCheck::Ready => {}
            }
        }
        if issued > 0 {
            println!("{}", restore_guidance("manage index rebuild"));
            return Ok(0);
        }

        // manifest取得・復号・parse
        struct Rec {
            name: String,
            category: String,
            rel_path: String,
            source_path: String,
            identity_hash: String,
            folder: String,
            s3_key: String,
            version_id: Option<String>,
            date: String,
            version_ts: String,
            batch_id: String,
            source_sha256: String,
            age_sha256: String,
            source_size: u64,
            age_size: u64,
        }
        let mut all: Vec<Rec> = Vec::new();
        for key in &manifest_keys {
            let tmp = std::env::temp_dir().join(format!(
                "backup-remote-manifest-{}-{}.age",
                std::process::id(),
                sha256_hex(key.as_bytes())
                    .chars()
                    .take(12)
                    .collect::<String>()
            ));
            session
                .backend
                .get_to_file(&session.aws.bucket, key, &tmp)?;
            let plain = crate::crypto::decrypt_bytes(&tmp, &key_path);
            let _ = std::fs::remove_file(&tmp);
            let plain = plain?;
            let text = String::from_utf8(plain).map_err(|e| {
                BackupError::Process(format!("manifest平文がUTF-8でない({key}): {e}"))
            })?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let v: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                    BackupError::Process(format!("manifest行のparse失敗({key}): {e}"))
                })?;
                if v.get("record_type").and_then(|t| t.as_str()) != Some("file") {
                    continue;
                }
                let g = |f: &str| {
                    v.get(f)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                all.push(Rec {
                    name: g("name"),
                    category: g("category"),
                    rel_path: g("rel_path"),
                    source_path: g("source_path"),
                    identity_hash: g("identity_hash"),
                    folder: {
                        // manifestのfolder記録を使う(欠損時はnameから導出・H-3:
                        // folderが落ちるとrebuild後のpin key生成が壊れる)
                        let f = g("folder");
                        if f.is_empty() { g("name") } else { f }
                    },
                    s3_key: g("s3_key"),
                    version_id: v
                        .get("version_id")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    date: g("date"),
                    version_ts: g("version_ts"),
                    batch_id: g("batch_id"),
                    source_sha256: g("source_sha256"),
                    age_sha256: g("age_sha256"),
                    source_size: v.get("source_size").and_then(|x| x.as_u64()).unwrap_or(0),
                    age_size: v.get("age_size").and_then(|x| x.as_u64()).unwrap_or(0),
                });
            }
        }
        // version_id欠損の補完: ListObjectVersionsで当該keyの版(古い順)から
        // 未claimのものを記録順(古い順)へ割当てる
        let keys_missing: std::collections::BTreeSet<String> = all
            .iter()
            .filter(|r| r.version_id.is_none())
            .map(|r| r.s3_key.clone())
            .collect();
        for key in keys_missing {
            let missing_idx: Vec<usize> = all
                .iter()
                .enumerate()
                .filter(|(_, r)| r.version_id.is_none() && r.s3_key == key)
                .map(|(i, _)| i)
                .collect();
            let claimed: Vec<String> = all
                .iter()
                .filter(|r| r.s3_key == key)
                .filter_map(|r| r.version_id.clone())
                .collect();
            let mut versions = session.backend.list_versions(&session.aws.bucket, &key)?;
            versions.retain(|v| !v.is_delete_marker);
            // listは新しい順→古い順へ反転
            versions.reverse();
            let mut unclaimed = versions
                .into_iter()
                .filter(|v| !claimed.contains(&v.version_id) && !v.version_id.is_empty())
                .map(|v| v.version_id)
                .collect::<Vec<_>>();
            for i in missing_idx {
                if let Some(vid) = unclaimed.first().cloned() {
                    all[i].version_id = Some(vid);
                    unclaimed.remove(0);
                }
            }
        }

        // identity単位のentry組立(versionsは新しい順)
        let mut order: Vec<String> = Vec::new();
        let mut entries: BTreeMap<String, IndexFileEntry> = BTreeMap::new();
        for r in &all {
            if !entries.contains_key(&r.identity_hash) {
                order.push(r.identity_hash.clone());
                entries.insert(
                    r.identity_hash.clone(),
                    IndexFileEntry {
                        schema: index::INDEX_SCHEMA,
                        record_type: "file".into(),
                        name: r.name.clone(),
                        category: r.category.clone(),
                        rel_path: r.rel_path.clone(),
                        source_path: r.source_path.clone(),
                        identity_hash: r.identity_hash.clone(),
                        folder: r.folder.clone(),
                        versions: Vec::new(),
                        pins: Vec::new(),
                    },
                );
            }
        }
        let mut sorted = all;
        sorted.sort_by(|a, b| a.version_ts.cmp(&b.version_ts));
        for r in sorted {
            if let Some(e) = entries.get_mut(&r.identity_hash) {
                e.versions.push(IndexVersion {
                    date: r.date,
                    version_ts: r.version_ts,
                    batch_id: r.batch_id,
                    s3_key: r.s3_key,
                    version_id: r.version_id.unwrap_or_default(),
                    source_sha256: r.source_sha256,
                    age_sha256: r.age_sha256,
                    source_size: r.source_size,
                    age_size: r.age_size,
                });
            }
        }
        for e in entries.values_mut() {
            e.versions.reverse(); // date降順(先頭=最新)
        }
        let mut entry_list: Vec<IndexFileEntry> = order
            .into_iter()
            .filter_map(|id| entries.remove(&id))
            .collect();
        // pins復元(H-3・設計2.3): ローカルregistryのpin_write記録(未削除)を該当entryへ
        // 接続する。registry喪失時(完全被災の新環境)はpin情報の補完は行われない
        // (manifestにpin実体の記録が無いため。list_versions走査での補完はPhase B課題)
        let registry_path = cfg.registry_path.clone();
        if registry_path.is_file() {
            let records = registry::load_registry(&registry_path)?;
            let reduced_now = registry::reduce_records(&records);
            for pin in reduced_now.pin_writes.values() {
                if reduced_now.pin_removes.contains_key(&pin.pin_s3_key) {
                    continue;
                }
                if let Some(entry) = entry_list.iter_mut().find(|e| {
                    e.name == pin.name
                        && e.category == pin.category
                        && e.rel_path == pin.rel_path
                        && e.folder == pin.folder
                }) {
                    entry.pins.push(IndexPin {
                        pin: pin.pin.clone(),
                        s3_key: pin.pin_s3_key.clone(),
                        created_at: pin.recorded_at.clone(),
                        date: pin.date.clone(),
                        source_sha256: pin.source_sha256.clone(),
                        age_sha256: pin.age_sha256.clone(),
                        source_size: pin.source_size,
                    });
                }
            }
        }
        let run_id = entry_list
            .first()
            .and_then(|e| e.versions.first().map(|v| v.batch_id.clone()))
            .unwrap_or_else(|| "rebuild".to_string());
        index::upload_index(
            session.backend.as_ref(),
            &session.aws.bucket,
            &session.aws.prefix,
            &cfg.public_keys,
            &entry_list,
            &run_id,
            &now_jst(),
            &registry_path,
        )?;
        println!(
            "索引を再構築しました: entries={} (manifests={})",
            entry_list.len(),
            manifest_keys.len()
        );
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

// ---------------------------------------------------------------------------
// edit(設定の検証付きラッパ)

pub fn cmd_edit(args: &EditArgs) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let cfg_path = args
            .ctx
            .config
            .clone()
            .unwrap_or_else(|| PathBuf::from("configs/pipeline.toml"));
        let cfg = config::load_pipeline(&cfg_path)?;
        match args.target.as_str() {
            "categories" => {
                println!("現在の[categories]({}):", cfg_path.display());
                for (name, cat) in &cfg.categories {
                    println!("  [categories.{name}] keep = {}", cat.keep);
                }
                println!("編集はpipeline.tomlを直接編集してください(保存後に検証されます)");
            }
            "files" => {
                println!("現在の[files]手動写像({}):", cfg_path.display());
                for (name, m) in &cfg.files {
                    println!(
                        "  [files.{name:?}] category = {:?} path = {:?}",
                        m.category, m.path
                    );
                }
                if cfg.files.is_empty() {
                    println!("  (なし:auto_path_roots相対化またはfallbackへ分類)");
                }
            }
            "layout" => {
                println!(
                    "[layout] unfiled_category={} auto_path_roots={}",
                    cfg.layout.unfiled_category,
                    cfg.layout
                        .auto_path_roots
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            "policy" => {
                println!(
                    "[output] keep_artifacts = {:?}",
                    match cfg.keep_artifacts {
                        config::KeepArtifacts::Run => "run",
                        config::KeepArtifacts::Always => "always",
                    }
                );
                for (name, cat) in &cfg.categories {
                    println!("[categories.{name}] keep = {}", cat.keep);
                }
            }
            other => {
                return Err(BackupError::Usage(format!(
                    "editの対象は categories|files|layout|policy です: {other}"
                )));
            }
        }
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}
