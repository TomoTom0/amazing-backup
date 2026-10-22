//! TASK-10/11 CLI層(list/query/get/diff/edit/backup/pin/manage・設計書3章)。
//! 機能単位へ分割したmodule群(設計8.1のPhase B再編: get.rs/pull.rsほか):
//!   - query: list系(list files/versions/pins/runs/categories/jobs)・query・diff
//!   - get:   取得(単一file・平文衝突保護・事前検査)
//!   - pull:  一括復元(backup pull)
//!   - backup: 書き込み(backup push/verify)
//!   - pin:   ピン留め付与/解除
//!   - manage: 保持・索引・lifecycle・復元job管理
//!   - edit:  設定の編集対象表示
//! 照会系はaws.toml+秘密鍵のみで動作する(索引はS3上にあるため災害時も成立・設計2.4)。

pub mod backup;
pub mod edit;
pub mod get;
pub mod manage;
pub mod pin;
pub mod pull;
pub mod query;

pub use backup::{BackupArgs, BackupSub, cmd_backup};
pub use edit::{EditArgs, cmd_edit};
pub use get::{GetArgs, cmd_get};
pub use manage::{ManageArgs, ManageSub, cmd_manage};
pub use pin::{PinArgs, PinSub, cmd_pin};
pub use query::{DiffArgs, ListArgs, ListSub, QueryArgs, cmd_diff, cmd_list, cmd_query};

use crate::config::{self, AwsConfig, PipelineConfig};
use crate::errors::BackupResult;
use crate::s3::S3Backend;
use std::path::{Path, PathBuf};

/// 既定のaws設定path(2.3)。
const DEFAULT_AWS_CONFIG: &str = "configs/aws.toml";

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
    /// aws.tomlの[aws].active_profileを当該実行限り上書き(aws.tomlへの書戻しはしない)
    #[arg(long)]
    pub profile: Option<String>,
    /// 出力形式(照会系9コマンド。既定human・jsonはstdout全体が単一JSON)
    #[arg(long, value_enum, default_value_t = Format::Human)]
    pub format: Format,
}

/// 照会系の出力形式(設計3章・TASK-11 Phase B)。
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// 人間向けの現行表示形式(既定)
    Human,
    /// stdout全体が単一JSON(警告等の人間向け通知はstderr)
    Json,
}

/// 実行context(設定読込+backend)。
pub(crate) struct Session {
    pub(crate) aws: AwsConfig,
    /// aws.toml正本profile(active_profile)のbucket(H-1: bucket一致規則の正本側)
    pub(crate) canonical_bucket: String,
    pub(crate) cfg: Option<PipelineConfig>,
    pub(crate) backend: Box<dyn S3Backend>,
}

/// 設定読込+backend構築。cfgは--config指定時のみ読む(照会系はpipeline.toml不要)。
/// `--profile`はaws.tomlのactive_profileを当該実行限り上書きする(書戻さない)。
pub(crate) fn open_session(ctx: &CtxArgs) -> Result<Session, crate::errors::BackupError> {
    let cfg = match &ctx.config {
        Some(p) => Some(config::load_pipeline(p)?),
        None => None,
    };
    let aws = load_aws(&ctx.aws_config, ctx.profile.as_deref())?;
    let canonical_bucket = config::canonical_bucket(&aws_config_path(&ctx.aws_config))?;
    let backend = crate::cli::select_backend(&aws)?;
    Ok(Session {
        aws,
        canonical_bucket,
        cfg,
        backend,
    })
}

/// --aws-config指定の解決(None=既定path configs/aws.toml)。
pub(crate) fn aws_config_path(aws_config: &Option<PathBuf>) -> PathBuf {
    aws_config
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_AWS_CONFIG))
}

/// aws.toml読込(共通optionsから。--profileは一時上書き)。
pub(crate) fn load_aws(
    aws_config: &Option<PathBuf>,
    profile: Option<&str>,
) -> Result<AwsConfig, crate::errors::BackupError> {
    let aws_path = aws_config_path(aws_config);
    config::load_aws_profile(&aws_path, profile, None)
}

/// 復号鍵の解決(--key > pipeline.toml [verify].decrypt_key)。
pub(crate) fn resolve_key(
    ctx: &CtxArgs,
    cfg: Option<&PipelineConfig>,
) -> Result<PathBuf, crate::errors::BackupError> {
    if let Some(k) = &ctx.key {
        return Ok(k.clone());
    }
    cfg.and_then(|c| c.decrypt_key.clone()).ok_or_else(|| {
        crate::errors::BackupError::Config(
            "復号用秘密鍵が未指定です(--key または pipeline.toml [verify].decrypt_key)".to_string(),
        )
    })
}

/// 索引の取得・復号・parse(照会系の最初の1 GET・設計2.4)。
pub(crate) fn read_index(
    backend: &dyn S3Backend,
    aws: &AwsConfig,
    key_path: &Path,
) -> BackupResult<(crate::index::IndexHeader, Vec<crate::index::IndexFileEntry>)> {
    crate::index::read_index(backend, &aws.bucket, &aws.prefix, key_path)
}

/// 照会系(list/query/get/diff)の索引読込。索引objectが不在(404)でも、ローカルregistryが
/// 当該prefixへ一度も書込(file_upload記録)をしていない場合は「未書込namespace」として
/// 空の索引扱い(0件・exit 0)にする(TASK-11 Phase B・--profileによる複数namespace運用で
/// 「まだ一度も書いていないprefix」の照会が常態化するため)。書込実績がある・registryを
/// 読めない(災害時等)場合の索引不在は従来どおり設定エラー2+案内(cli.index-missing-error)。
pub(crate) fn read_index_for_query(
    session: &Session,
    key_path: &Path,
) -> BackupResult<(crate::index::IndexHeader, Vec<crate::index::IndexFileEntry>)> {
    let index_key = crate::layout::index_object_key(&session.aws.prefix);
    if session
        .backend
        .head(&session.aws.bucket, &index_key)?
        .is_none()
        && namespace_never_written(session)?
    {
        return Ok((
            crate::index::IndexHeader {
                schema: crate::index::INDEX_SCHEMA,
                record_type: "index_header".into(),
                updated_at: String::new(),
                run_id: String::new(),
                tool_version: env!("CARGO_PKG_VERSION").to_string(),
                entries: 0,
            },
            Vec::new(),
        ));
    }
    read_index(session.backend.as_ref(), &session.aws, key_path)
}

/// ローカルregistryに当該prefix配下へのfile_upload記録が無い(=未書込namespace)か。
/// registry不在はfalse(索引不在を異常扱い)。registryの不正行(parse失敗)はエラー伝播
/// する(黙って空扱いにしない)。
/// 書込実績の判定はbucket一致(registry::record_bucket_matched。H-1)を伴う:
/// 同一prefix・別bucketの他方profileの実績では未書込のまま(0件例外はbucket単位)。
fn namespace_never_written(session: &Session) -> BackupResult<bool> {
    let Some(cfg) = session.cfg.as_ref() else {
        return Ok(false);
    };
    let registry_path = cfg.registry_path.clone();
    if !registry_path.is_file() {
        return Ok(false);
    }
    let records = crate::registry::load_registry(&registry_path)?;
    let reduced = crate::registry::reduce_records(&records);
    let prefix = session.aws.prefix.clone();
    Ok(!reduced.file_uploads.iter().any(|u| {
        u.s3_key.starts_with(&prefix)
            && crate::registry::record_bucket_matched(
                &u.s3_bucket,
                &session.aws.bucket,
                &session.canonical_bucket,
            )
    }))
}

/// 書き込み系コマンドの起動時versioning確認(設計5.1)。無効は設定エラー2+有効化案内。
pub(crate) fn require_versioning(
    backend: &dyn S3Backend,
    aws: &AwsConfig,
) -> Result<(), crate::errors::BackupError> {
    if backend.get_bucket_versioning(&aws.bucket)? {
        return Ok(());
    }
    Err(crate::errors::BackupError::Config(format!(
        "bucket({})のversioningが無効です。有効化してください: aws s3api put-bucket-versioning --bucket {} --versioning-configuration Status=Enabled",
        aws.bucket, aws.bucket
    )))
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn sha256_hex_file(path: &Path) -> BackupResult<String> {
    let bytes = std::fs::read(path).map_err(|e| {
        crate::errors::BackupError::Transfer(format!(
            "取得fileの読込に失敗({}): {e}",
            path.display()
        ))
    })?;
    Ok(sha256_hex(&bytes))
}

/// 平文sha256のstreaming計算(dry-run予測などsize不明の大ファイル対象で
/// 全文readによるメモリ膨張を避ける。読込失敗は対象エラー)。
pub(crate) fn sha256_hex_streaming(path: &Path) -> BackupResult<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| {
        crate::errors::BackupError::Target(format!("対象の読込に失敗({}): {e}", path.display()))
    })?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| {
            crate::errors::BackupError::Target(format!("対象の読込に失敗({}): {e}", path.display()))
        })?;
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

/// 既存平文のsha256(平文衝突保護用。読込失敗は対象エラー)。
pub(crate) fn sha256_of_plain(path: &Path) -> BackupResult<String> {
    let bytes = std::fs::read(path).map_err(|e| {
        crate::errors::BackupError::Target(format!("既存平文の読込に失敗({}): {e}", path.display()))
    })?;
    Ok(sha256_hex(&bytes))
}

/// JST現在時刻(ISO8601 +09:00付き)。
pub(crate) fn now_jst() -> String {
    crate::targets::format_jst(&crate::targets::now_jst())
}

/// source fileのmtimeをJST・サブ秒精度で文字列化(新経路のskip指紋用)。
/// 旧経路(format_jst)は秒精度へ切捨てるため、同秒内の同サイズ変更がskipされて
/// しまう。新経路は指紋にサブ秒を含めることで「同じsize+mtime秒でも内容が
/// 変わり得る」変更を取りこぼさない(push_versions系の結合test要件)。
pub(crate) fn mtime_jst_precise(path: &Path) -> BackupResult<String> {
    let meta = std::fs::metadata(path).map_err(|e| {
        crate::errors::BackupError::Target(format!(
            "対象のmetadata取得に失敗({}): {e}",
            path.display()
        ))
    })?;
    let modified = meta.modified().map_err(|e| {
        crate::errors::BackupError::Target(format!("mtimeの取得に失敗({}): {e}", path.display()))
    })?;
    let dt: chrono::DateTime<chrono::Utc> = modified.into();
    let jst = dt.with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).expect("JST offset"));
    Ok(jst.format("%Y-%m-%dT%H:%M:%S%.f+09:00").to_string())
}

/// `--version`等の日付形式検証(usage 64)。
pub(crate) fn parse_date(s: &str) -> Result<String, String> {
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

pub(crate) fn fmt_size(bytes: u64) -> String {
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

/// 単一JSONをstdoutへ出力する(--format json・TASK-11 Phase B)。
pub(crate) fn print_json(value: &serde_json::Value) {
    match serde_json::to_string(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error: 内部エラー: JSON出力の組み立てに失敗: {e}"),
    }
}
