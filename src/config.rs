//! toml読込・厳格schema検証・struct化(設計5章config.rs・3章schema)。
//! TASK-19 XDG配置: 規定path解決(resolve_app_paths)と、[output]廃止に伴う
//! registry・暗号化一時物dir(旧data/out)のXDG規定path化を担う。

use crate::errors::{BackupError, BackupResult};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// 規定pathのapp名(XDG配置のdir名。TASK-19)。
pub const APP_DIR_NAME: &str = "amazing-backup";

/// `pipeline.toml`相当(3.2・TASK-10設計4.1)。registry・cache dirのpathはXDG規定
/// 解決(TASK-19: [output]廃止・設定fileからは指定不可)。
#[derive(Debug)]
pub struct PipelineConfig {
    /// SSH ed25519公開鍵path列挙(1-of-N recipient)
    pub public_keys: Vec<std::path::PathBuf>,
    /// 暗号化一時物の集約dir(XDG cache dir。旧data/out。成功run後に掃除が既定)
    pub output_dir: std::path::PathBuf,
    /// ローカル累積registry(JSONL)path(XDG state dir)
    pub registry_path: std::path::PathBuf,
    /// per-file失敗時も継続する既定値
    pub keep_going: bool,
    /// --roundtrip検証で使う秘密鍵path
    pub decrypt_key: Option<std::path::PathBuf>,
    /// cache dir配下の.age保持policy(TASK-10設計4.1・8.3。TASK-19で[behavior]へ移設)。
    /// 既定Run(成功run後に掃除)
    pub keep_artifacts: KeepArtifacts,
    /// 物理配置の設定(所在記録値の分類解決用。TASK-18 5.15維持前提)
    pub layout: LayoutConfig,
    /// metadataのlatest_source_path掲載形式(TASK-18 2.2)
    pub metadata: MetadataConfig,
    /// 手動写像(basename → category/path)。省略時はauto_path_roots相対化へ
    pub files: std::collections::BTreeMap<String, FileMapping>,
}

/// XDG配置の規定path一式(TASK-19)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// pipeline.tomlの規定path
    pub pipeline_config: PathBuf,
    /// aws.tomlの規定path
    pub aws_config: PathBuf,
    /// ローカルregistryの規定path
    pub registry: PathBuf,
    /// 暗号化一時物dirの規定path(旧data/out)
    pub cache_dir: PathBuf,
}

/// 規定pathの解決(TASK-19 XDG配置)。優先順位:
/// 1. `AMAZING_BACKUP_HOME`(一括上書き・テスト/CI用・空値は無視): すべて`$ABH/`配下へ
///    統合(`$ABH/pipeline.toml`・`$ABH/aws.toml`・`$ABH/registry.jsonl`・`$ABH/cache/`)
/// 2. XDG準拠: 設定dir=`$XDG_CONFIG_HOME/amazing-backup/`(未設定なら
///    `~/.config/amazing-backup/`)・registry=`$XDG_STATE_HOME/amazing-backup/registry.jsonl`
///    (未設定なら`~/.local/state/...`)・cache=`$XDG_CACHE_HOME/amazing-backup/`
///    (未設定なら`~/.cache/...`)。XDG_*_HOMEは絶対pathのみ有効(空値・相対pathは
///    未指定扱い。XDG base directory仕様準拠)
pub fn resolve_app_paths() -> AppPaths {
    resolve_app_paths_from(|k| std::env::var_os(k))
}

/// 規定path解決の本体(env取得を注入可能=unit testでenv競合なしに検証できる)。
fn resolve_app_paths_from(getenv: impl Fn(&str) -> Option<OsString>) -> AppPaths {
    if let Some(abh) = getenv("AMAZING_BACKUP_HOME").filter(|v| !v.is_empty()) {
        let root = PathBuf::from(abh);
        return AppPaths {
            pipeline_config: root.join("pipeline.toml"),
            aws_config: root.join("aws.toml"),
            registry: root.join("registry.jsonl"),
            cache_dir: root.join("cache"),
        };
    }
    let config = xdg_dir_from(&getenv, "XDG_CONFIG_HOME", ".config");
    let state = xdg_dir_from(&getenv, "XDG_STATE_HOME", ".local/state");
    let cache = xdg_dir_from(&getenv, "XDG_CACHE_HOME", ".cache");
    AppPaths {
        pipeline_config: config.join(APP_DIR_NAME).join("pipeline.toml"),
        aws_config: config.join(APP_DIR_NAME).join("aws.toml"),
        registry: state.join(APP_DIR_NAME).join("registry.jsonl"),
        cache_dir: cache.join(APP_DIR_NAME),
    }
}

/// XDG_*_HOMEの解決(絶対pathのみ有効。空値・相対pathはhome配下の既定へ)。
fn xdg_dir_from(
    getenv: &impl Fn(&str) -> Option<OsString>,
    env: &str,
    fallback_rel: &str,
) -> PathBuf {
    match getenv(env) {
        Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home_dir_from(getenv).join(fallback_rel),
    }
}

fn home_dir_from(getenv: &impl Fn(&str) -> Option<OsString>) -> PathBuf {
    getenv("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `[metadata]`相当(TASK-18 2.2・5.15)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataConfig {
    /// latest_source_pathの掲載形式(既定Plain)
    pub latest_source_path: PathMode,
}

/// latest_source_pathの3形態(TASK-18 2.2・承認事項11)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    /// path文字列そのまま(既定)
    Plain,
    /// age暗号化object{format,recipients,sha256,data}
    Encrypted,
    /// null(掲載しない)
    None,
}

/// [aws.lifecycle]相当(TASK-18 7章・5.15)。3キーとも必須(コード内defaultなし)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleConfig {
    /// NCVE NoncurrentDays(Deep Archive最低保存180日と整合)
    pub noncurrent_days: u32,
    /// NCVE NewerNoncurrentVersions(保持noncurrent版数・1..=100)
    pub keep_versions: u32,
    /// AbortIncompleteMultipartUpload DaysAfterInitiation
    pub mpu_abort_days: u32,
}

/// cache dir配下の.age保持policy(TASK-10設計4.1。TASK-19 XDG配置で[behavior]へ移設)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepArtifacts {
    /// run成功後に当該run分の.ageを掃除(既定)
    Run,
    /// 従来挙動(全保持)
    Always,
}

impl KeepArtifacts {
    fn parse(value: &str) -> Option<KeepArtifacts> {
        match value {
            "run" => Some(KeepArtifacts::Run),
            "always" => Some(KeepArtifacts::Always),
            _ => None,
        }
    }
}

/// `[layout]`相当(TASK-10設計4.1)。
#[derive(Debug, Clone)]
pub struct LayoutConfig {
    /// 写像/roots不一致時のcategory(既定"unfiled")
    pub unfiled_category: String,
    /// source_path相対化のroot列挙(順序優先・絶対path)
    pub auto_path_roots: Vec<std::path::PathBuf>,
}

/// `[files."<name>"]`手動写像相当(TASK-10設計4.1)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMapping {
    pub category: String,
    /// rel_path(空文字列=category直下)
    pub path: String,
}

/// `aws.toml`(規定path: XDG配置)相当(3.3・TASK-9 9.2・TASK-10設計4.2)。credential値は含まない
/// (credentialは`credentials_file`が指す外部ファイル明示参照のみ)。
#[derive(Debug)]
pub struct AwsConfig {
    /// 実効profile名(--profile・env・active_profileの解決結果。
    /// 接続先表示=誤接続防止用。TASK-11 Phase B: 3prod bucket構成の混同防止)
    pub profile: String,
    /// 実効profileのpurpose(dev/prod等)。従来はprofile選択時にのみ使われ破棄
    /// されていたが、TASK-18 8章の`manage reset`安全弁(実効profileのpurpose=dev
    /// 限定)のため保持する
    pub purpose: String,
    pub bucket: String,
    pub region: String,
    /// credentialファイル(`configs/secret/aws-credentials.toml`等)のpath。
    /// cwd基準の相対path。必須(fail-closed: credential chainへのfallbackはしない)
    pub credentials_file: std::path::PathBuf,
    /// 既定DEEP_ARCHIVE
    pub storage_class: String,
    /// 名前空間root prefix(TASK-10設計1.1・4.2)。`/`終端。既定"amazing-backup/"
    pub prefix: String,
    /// lifecycle設定(TASK-18 7章。[aws.lifecycle]はprofile横断で同一構成のため
    /// [aws]直下に一元化)
    pub lifecycle: LifecycleConfig,
}

/// credentialファイルの内容(TASK-9設計3章)。`RealS3`構築(明示API key)へ渡す。
/// 値のDebug表示は`***`化する(値非露出・設計3.6)。
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl std::fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsCredentials")
            .field("access_key_id", &"***")
            .field("secret_access_key", &"***")
            .field("session_token", &self.session_token.as_ref().map(|_| "***"))
            .finish()
    }
}

/// pipeline.tomlのschema(3.2・TASK-10設計4.1。未知キー・型不一致拒否の厳格検証)。
/// TASK-10追加分([layout]・[files])は省略可能。TASK-19で[output]は廃止
/// (存在したら未知キー拒否・registry/cache dir pathはXDG規定path固定)。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineFile {
    recipients: RecipientsFile,
    #[serde(default)]
    behavior: BehaviorFile,
    #[serde(default)]
    verify: VerifyFile,
    #[serde(default)]
    layout: LayoutFile,
    /// [categories]はTASK-18で廃止(存在したら未知キー拒否・静的に廃止を確定させる)
    #[serde(default)]
    metadata: Option<MetadataFile>,
    #[serde(default)]
    files: std::collections::BTreeMap<String, FileMappingFile>,
}

/// `[metadata]`のschema(TASK-18 2.2)。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataFile {
    /// "plain"|"encrypted"|"none"(省略時plain)
    #[serde(default)]
    latest_source_path: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipientsFile {
    public_keys: Vec<String>,
}

/// `[layout]`のschema(TASK-10設計4.1)。
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct LayoutFile {
    #[serde(default)]
    unfiled_category: String,
    #[serde(default)]
    auto_path_roots: Vec<String>,
}

/// `[files."<name>"]`のschema(category必須・path省略可=category直下)。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMappingFile {
    category: String,
    #[serde(default)]
    path: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BehaviorFile {
    keep_going: bool,
    /// .age保持policy "run"|"always"(省略時run)。TASK-19 XDG配置で[output]廃止に伴い
    /// [behavior]へ移設
    #[serde(default)]
    keep_artifacts: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct VerifyFile {
    decrypt_key: String,
}

/// aws.tomlのschema(3.3・TASK-9 9.2。`credentials_file`を除くcredential系キーは
/// 未知キーとして拒否される=値直書き不可)。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AwsFile {
    aws: AwsSection,
}

/// profile単位のS3設定(複数profileを1つのaws.tomlに持つためのサブテーブル)。
#[derive(serde::Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct AwsProfile {
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    region: String,
    /// 省略時は`[aws].credentials_file`(共通)を採用
    #[serde(default)]
    credentials_file: String,
    #[serde(default)]
    storage_class: String,
    /// 名前空間root prefix(TASK-10設計4.2。None=省略時の既定"amazing-backup/"。
    /// 明示空文字は形式違反として拒否)
    #[serde(default)]
    prefix: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AwsSection {
    /// 起動時に使うprofile名(省略時は"default")。
    #[serde(default)]
    active_profile: String,
    /// profile名 -> profile定義。最低1つ必須。
    profiles: std::collections::BTreeMap<String, AwsProfile>,
    /// lifecycle設定(TASK-18 7章。3キーとも必須・省略は設定エラー2)
    lifecycle: LifecycleFile,
}

/// `[aws.lifecycle]`のschema(TASK-18 7章)。
#[derive(serde::Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct LifecycleFile {
    noncurrent_days: u32,
    keep_versions: u32,
    mpu_abort_days: u32,
}

/// credentialファイルのschema(TASK-9設計3.1・3.2。[credentials]テーブル配下は
/// access_key_id/secret_access_key必須・session_token省略可・未知キー拒否)。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialsFile {
    credentials: CredentialsSection,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialsSection {
    access_key_id: String,
    secret_access_key: String,
    #[serde(default)]
    session_token: Option<String>,
}

/// pipeline.tomlを読み、厳格schema検証(未知キー・型不一致拒否)・
/// 空検査・公開鍵の存在とed25519形式検証・指紋重複検査(3.2)を行う。
/// registry・cache dirのpathはXDG規定解決(TASK-19: [output]廃止・設定fileから指定不可)。
/// 規定pathにfileが無い場合はcopy案内を添える(config.default-config-missing-guidance)。
pub fn load_pipeline(path: &Path) -> BackupResult<PipelineConfig> {
    let is_default_path = *path == resolve_app_paths().pipeline_config;
    let text = std::fs::read_to_string(path).map_err(|e| {
        let mut msg = format!("pipeline.tomlを読めない ({}): {e}", path.display());
        if is_default_path {
            msg.push_str(&format!(
                "\n規定path({})にpipeline.tomlがありません。初回セットアップ: repoのconfigs/pipeline.example.tomlをこのpathへcopyして記述してください(--config PATHで別pathの明示指定も可)",
                path.display()
            ));
        }
        BackupError::Config(msg)
    })?;
    let parsed: PipelineFile = toml::from_str(&text).map_err(|e| {
        BackupError::Config(format!(
            "pipeline.tomlのparseに失敗 ({}): {e}",
            path.display()
        ))
    })?;

    if parsed.recipients.public_keys.is_empty() {
        return Err(BackupError::Config(
            "[recipients].public_keysが空です(1つ以上のSSH ed25519公開鍵を指定してください)"
                .to_string(),
        ));
    }

    let mut public_keys = Vec::with_capacity(parsed.recipients.public_keys.len());
    let mut fingerprints = Vec::with_capacity(parsed.recipients.public_keys.len());
    for key in &parsed.recipients.public_keys {
        let key_path = std::path::PathBuf::from(key);
        if !key_path.is_file() {
            return Err(BackupError::Config(format!(
                "recipient公開鍵ファイルが存在しない: {}",
                key_path.display()
            )));
        }
        let pubkey = crate::fingerprint::parse_ssh_pubkey(&key_path)?;
        let fp = crate::fingerprint::fingerprint(&pubkey);
        if fingerprints.contains(&fp) {
            return Err(BackupError::Config(format!(
                "public_keys内の公開鍵指紋が重複しています: {fp} ({})",
                key_path.display()
            )));
        }
        fingerprints.push(fp);
        public_keys.push(key_path);
    }

    let decrypt_key = if parsed.verify.decrypt_key.trim().is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(parsed.verify.decrypt_key))
    };

    // TASK-10設計4.1・4.2: keep_artifacts列挙値(TASK-19 XDG配置で[behavior]へ移設)
    let keep_artifacts = match parsed.behavior.keep_artifacts.as_deref() {
        None => KeepArtifacts::Run,
        Some(v) => KeepArtifacts::parse(v).ok_or_else(|| {
            BackupError::Config(format!(
                "[behavior].keep_artifactsが不正です: {v:?}(許容値: run/always)"
            ))
        })?,
    };

    // TASK-10設計4.2: layout・categories・filesの検証
    let unfiled_category = match parsed.layout.unfiled_category.trim() {
        "" => "unfiled".to_string(),
        t => t.to_string(),
    };
    crate::layout::validate_category_name(&unfiled_category)
        .map_err(|e| BackupError::Config(format!("[layout].unfiled_categoryが不正です: {e}")))?;

    let mut auto_path_roots = Vec::with_capacity(parsed.layout.auto_path_roots.len());
    for root in &parsed.layout.auto_path_roots {
        let p = std::path::PathBuf::from(root);
        if !p.is_absolute() {
            return Err(BackupError::Config(format!(
                "[layout].auto_path_rootsは絶対pathで指定してください: {root:?}"
            )));
        }
        auto_path_roots.push(p);
    }

    // [metadata](TASK-18 2.2): latest_source_pathは3値。省略時plain
    let latest_source_path = match parsed
        .metadata
        .as_ref()
        .and_then(|m| m.latest_source_path.as_deref())
    {
        None => PathMode::Plain,
        Some("plain") => PathMode::Plain,
        Some("encrypted") => PathMode::Encrypted,
        Some("none") => PathMode::None,
        Some(v) => {
            return Err(BackupError::Config(format!(
                "[metadata].latest_source_pathが不正です: {v:?}(許容値: plain/encrypted/none)"
            )));
        }
    };

    // TASK-18 5.15: files.categoryの定義存在検証は廃止(category語彙は記録値として
    // 自由化)しvalidate_category_nameによる文字列検証のみへ置換
    let mut files = std::collections::BTreeMap::new();
    for (name, mapping) in &parsed.files {
        if name.is_empty() || name.contains('/') {
            return Err(BackupError::Config(format!(
                "[files]のkeyはbasename(拡張子付き・/を含まない)を指定してください: {name:?}"
            )));
        }
        crate::layout::validate_category_name(&mapping.category)
            .map_err(|e| BackupError::Config(format!("[files.{name}].categoryが不正です: {e}")))?;
        let rel = crate::layout::normalize_rel_path(&mapping.path)
            .map_err(|e| BackupError::Config(format!("[files.{name}].pathが不正です: {e}")))?;
        files.insert(
            name.clone(),
            FileMapping {
                category: mapping.category.clone(),
                path: rel,
            },
        );
    }

    // registry・cache dirはXDG規定解決(TASK-19: [output]廃止・env上書きのみ)
    let paths = resolve_app_paths();
    Ok(PipelineConfig {
        public_keys,
        output_dir: paths.cache_dir,
        registry_path: paths.registry,
        keep_going: parsed.behavior.keep_going,
        decrypt_key,
        keep_artifacts,
        layout: LayoutConfig {
            unfiled_category,
            auto_path_roots,
        },
        metadata: MetadataConfig { latest_source_path },
        files,
    })
}

/// 既知のS3 storage classのallowlist(3.3。打ち間違いによる誤configを設定エラーで弾く)。
const KNOWN_STORAGE_CLASSES: [&str; 7] = [
    "STANDARD",
    "STANDARD_IA",
    "ONEZONE_IA",
    "INTELLIGENT_TIERING",
    "GLACIER",
    "DEEP_ARCHIVE",
    "GLACIER_IR",
];

/// aws.tomlを読み、厳格schema検証を行う(credential系キーの値直書きは未知キーとして拒否。
/// credentialは`credentials_file`による外部ファイル明示参照のみ)。
/// `storage_class`は既知S3 storage classのallowlist検証を行う(3.3)。
/// `bucket`/`region`/`credentials_file`の前後空白付き値・`credentials_file`空値は
/// (値を trimせず使う仕様のため)設定エラーとする(3.3・TASK-9 3.3)。
/// `aws.toml`を読み、`profile_name`(Someなら指定profile、Noneなら
/// `active_profile`/直下fieldsによる既定選択)に対応する設定を返す。
/// 旧形式(`[aws]`直下にbucket/region/...を書く)は単一profileとして扱い後方互換を維持する。
pub fn load_aws(path: &Path) -> BackupResult<AwsConfig> {
    load_aws_profile(path, None, None)
}

pub fn load_aws_profile(
    path: &Path,
    profile_name: Option<&str>,
    purpose: Option<&str>,
) -> BackupResult<AwsConfig> {
    let is_default_path = *path == resolve_app_paths().aws_config;
    let text = std::fs::read_to_string(path).map_err(|e| {
        let mut msg = format!("aws.tomlを読めない ({}): {e}", path.display());
        if is_default_path {
            msg.push_str(&format!(
                "\n規定path({})にaws.tomlがありません。初回セットアップ: repoのconfigs/aws.example.tomlをこのpathへcopyして実際の値(bucket等)を記述してください(--aws-config PATHで別pathの明示指定も可)",
                path.display()
            ));
        }
        BackupError::Config(msg)
    })?;
    let parsed: AwsFile = toml::from_str(&text).map_err(|e| {
        BackupError::Config(format!("aws.tomlのparseに失敗 ({}): {e}", path.display()))
    })?;

    if parsed.aws.profiles.is_empty() {
        return Err(BackupError::Config(format!(
            "[aws].profilesが空です(最低1つのprofileを定義してください。{})",
            path.display()
        )));
    }

    let env_profile = std::env::var("AMAZING_BACKUP_AWS_PROFILE")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let env_purpose = std::env::var("AMAZING_BACKUP_AWS_PURPOSE")
        .ok()
        .filter(|v| !v.trim().is_empty());
    // fake seam中(=テスト環境)は dev目的のprofileへ自動解決(手動切替・環境変数設定不要)。
    // TASK-10設計4.2: test profile廃止に伴い自動解決先をdevへ変更。
    let test_env = std::env::var("AMAZING_BACKUP_FAKE_S3_STATE")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let explicit_profile = profile_name
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty());
    let requested_purpose = purpose
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .or(env_purpose);

    let auto_test_purpose = test_env.map(|_| "dev".to_string());

    let selected_name = if let Some(n) = explicit_profile.or(env_profile) {
        n
    } else if let Some(pur) = requested_purpose.or(auto_test_purpose) {
        parsed
            .aws
            .profiles
            .iter()
            .find_map(|(name, prof)| {
                if prof.purpose == pur {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                if !parsed.aws.active_profile.trim().is_empty() {
                    parsed.aws.active_profile.clone()
                } else if parsed.aws.profiles.contains_key("default") {
                    "default".to_string()
                } else {
                    parsed
                        .aws
                        .profiles
                        .keys()
                        .next()
                        .cloned()
                        .unwrap_or_default()
                }
            })
    } else if !parsed.aws.active_profile.trim().is_empty() {
        parsed.aws.active_profile.clone()
    } else if parsed.aws.profiles.contains_key("default") {
        "default".to_string()
    } else {
        parsed
            .aws
            .profiles
            .keys()
            .next()
            .cloned()
            .unwrap_or_default()
    };

    let prof = parsed.aws.profiles.get(&selected_name).ok_or_else(|| {
        BackupError::Config(format!(
            "指定profileが見つからない: {selected_name} ({}。利用可能: {})",
            path.display(),
            parsed
                .aws
                .profiles
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join("/")
        ))
    })?;

    let credentials_file = &prof.credentials_file;
    // whitespace + non-empty checks
    for (name, value) in [
        ("bucket", &prof.bucket),
        ("region", &prof.region),
        ("credentials_file", credentials_file),
    ] {
        if value.trim() != value {
            return Err(BackupError::Config(format!(
                "[aws.profiles.{selected_name}].{name}の値に前後の空白が含まれる: {value:?}"
            )));
        }
    }
    if prof.bucket.trim().is_empty() || prof.region.trim().is_empty() {
        return Err(BackupError::Config(format!(
            "[aws.profiles.{selected_name}].bucket/regionが空です ({})",
            path.display()
        )));
    }
    if credentials_file.trim().is_empty() {
        return Err(BackupError::Config(format!(
            "[aws.profiles.{selected_name}].credentials_fileが空です ({})",
            path.display()
        )));
    }
    let storage_class = if prof.storage_class.trim().is_empty() {
        "DEEP_ARCHIVE".to_string()
    } else {
        prof.storage_class.clone()
    };
    if !KNOWN_STORAGE_CLASSES.contains(&storage_class.as_str()) {
        return Err(BackupError::Config(format!(
            "[aws.profiles.{selected_name}].storage_classが既知のS3 storage classでない: {storage_class}(許容値: {})",
            KNOWN_STORAGE_CLASSES.join("/")
        )));
    }
    // TASK-10設計1.1・4.2: root prefix(None=省略時の既定値。明示値は形式検証)
    let prefix = match &prof.prefix {
        None => DEFAULT_ROOT_PREFIX.to_string(),
        Some(v) => v.clone(),
    };
    crate::layout::validate_prefix(&prefix)
        .map_err(|e| BackupError::Config(format!("[aws.profiles.{selected_name}].prefix: {e}")))?;
    // TASK-18 7章: [aws.lifecycle]の検証(3キーとも必須=parse時に強制・範囲検証。
    // DEEP_ARCHIVE profileでnoncurrent_days<180はDeep Archive最低保存180日未満の
    // 早期削除となるため構造的に排除)
    let lc = parsed.aws.lifecycle;
    if !(1..=100).contains(&lc.keep_versions) {
        return Err(BackupError::Config(format!(
            "[aws.lifecycle].keep_versionsは1以上100以下で指定してください: {}",
            lc.keep_versions
        )));
    }
    if lc.noncurrent_days < 1 {
        return Err(BackupError::Config(format!(
            "[aws.lifecycle].noncurrent_daysは1以上で指定してください: {}",
            lc.noncurrent_days
        )));
    }
    if lc.mpu_abort_days < 1 {
        return Err(BackupError::Config(format!(
            "[aws.lifecycle].mpu_abort_daysは1以上で指定してください: {}",
            lc.mpu_abort_days
        )));
    }
    if storage_class == "DEEP_ARCHIVE" && lc.noncurrent_days < 180 {
        return Err(BackupError::Config(format!(
            "DEEP_ARCHIVE profileの[aws.lifecycle].noncurrent_daysは180以上必要です(Deep Archive最低保存180日): {}",
            lc.noncurrent_days
        )));
    }
    Ok(AwsConfig {
        profile: selected_name,
        purpose: prof.purpose.clone(),
        bucket: prof.bucket.clone(),
        region: prof.region.clone(),
        credentials_file: std::path::PathBuf::from(credentials_file.clone()),
        storage_class,
        prefix,
        lifecycle: LifecycleConfig {
            noncurrent_days: lc.noncurrent_days,
            keep_versions: lc.keep_versions,
            mpu_abort_days: lc.mpu_abort_days,
        },
    })
}

/// aws.tomlの正本profile([aws].active_profile。空なら既定選択)のbucket
/// (TASK-11 Phase BレビューH-1)。--profile一時上書き・AMAZING_BACKUP_AWS_PROFILE等の
/// env経路の解決を介さない「正本」のbucketで、registry記録`s3_bucket`欠損の旧記録を
/// 正本bucket相当と扱うbucket一致規則(registry::record_bucket_matched)の正本側引数。
pub fn canonical_bucket(path: &Path) -> BackupResult<String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        BackupError::Config(format!("aws.tomlを読めない ({}): {e}", path.display()))
    })?;
    let parsed: AwsFile = toml::from_str(&text).map_err(|e| {
        BackupError::Config(format!("aws.tomlのparseに失敗 ({}): {e}", path.display()))
    })?;
    canonical_bucket_from(&parsed, path)
}

/// parse済みaws.tomlから正本profileのbucketを解決する。
fn canonical_bucket_from(parsed: &AwsFile, path: &Path) -> BackupResult<String> {
    let name = if !parsed.aws.active_profile.trim().is_empty() {
        parsed.aws.active_profile.trim().to_string()
    } else if parsed.aws.profiles.contains_key("default") {
        "default".to_string()
    } else {
        parsed
            .aws
            .profiles
            .keys()
            .next()
            .cloned()
            .unwrap_or_default()
    };
    let prof = parsed.aws.profiles.get(&name).ok_or_else(|| {
        BackupError::Config(format!(
            "正本profileが見つからない: {name} ({})",
            path.display()
        ))
    })?;
    Ok(prof.bucket.clone())
}

/// root prefixの既定値(TASK-10設計4.2)。
pub const DEFAULT_ROOT_PREFIX: &str = "amazing-backup/";

/// credentialファイルを読み、厳格schema検証を行う(TASK-9設計3.1〜3.5)。
/// 不在・TOML parse失敗・`[credentials]`テーブル不在・必須キー欠落・空値・
/// 前後空白付き値は設定エラー2。credential chain(IMDS/env/~/.aws)へのfallbackは
/// 一切行わない(fail-closed。設計3.5)。
/// error文にはcredential値を含めずpathのみを表示する(設計3.6。toml errorの
/// source行snippetに値が現れ得るためparse失敗の詳細は意図的に伏せる)。
pub fn load_credentials(path: &Path) -> BackupResult<AwsCredentials> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        BackupError::Config(format!(
            "credentialファイルを読めない ({}): {e}",
            path.display()
        ))
    })?;
    let parsed: CredentialsFile = toml::from_str(&text).map_err(|_| {
        BackupError::Config(format!(
            "credentialファイルのparseに失敗 ({})。schema: [credentials]配下にaccess_key_id/secret_access_key(必須)・session_token(省略可)。未知キーは拒否(値の詳細は表示しない)",
            path.display()
        ))
    })?;
    let c = parsed.credentials;
    for (name, value) in [
        ("access_key_id", &c.access_key_id),
        ("secret_access_key", &c.secret_access_key),
    ] {
        if value.trim().is_empty() {
            return Err(BackupError::Config(format!(
                "credentialファイルの[credentials].{name}が空です ({})",
                path.display()
            )));
        }
        if value.trim() != value {
            return Err(BackupError::Config(format!(
                "credentialファイルの[credentials].{name}の値に前後の空白が含まれる ({})",
                path.display()
            )));
        }
    }
    if let Some(token) = &c.session_token {
        if token.trim().is_empty() || token.trim() != token {
            return Err(BackupError::Config(format!(
                "credentialファイルの[credentials].session_tokenが空または前後空白付きです ({})",
                path.display()
            )));
        }
    }
    Ok(AwsCredentials {
        access_key_id: c.access_key_id,
        secret_access_key: c.secret_access_key,
        session_token: c.session_token,
    })
}

/// bucket/regionが空なら設定エラー(upload開始時に呼ぶ。案内文付き)。
/// TASK-9 9.2: credentialファイルの存在・parse・値検査まで含めて事前検査とする
/// (S3 API呼び出し前に設定エラー2で弾く。aws CLIのPATH検査は廃止)。
pub fn require_ready(cfg: &AwsConfig) -> BackupResult<()> {
    if cfg.bucket.trim().is_empty() || cfg.region.trim().is_empty() {
        return Err(BackupError::Config(
            "aws.toml未設定(bucket/regionが空)です。plan/encrypt/verifyはawsなしで実行可能"
                .to_string(),
        ));
    }
    load_credentials(&cfg.credentials_file)?;
    Ok(())
}

/// uploadサブコマンドのCLI一時上書きオプション値(2.3 TASK-6節・3.3)。
/// 各フィールド`None` = 未指定(aws.toml値のまま)。aws.tomlが正本であり、
/// CLI指定は当該実行限りの例外(aws.tomlへの書戻しは一切行わない)。
#[derive(Debug, Default, Clone)]
pub struct AwsOverrides {
    pub storage_class: Option<String>,
    pub bucket: Option<String>,
    pub region: Option<String>,
}

/// CLI一時上書きをaws.toml読込済みの`AwsConfig`へ適用する(2.3 TASK-6節。検証基準は3.3と同一)。
/// 呼出側は「`load_aws`読込 → `require_ready`(toml値の正検査) → 本関数」の順で呼ぶこと
/// (CLI上書き値はtoml値が正である前提の上書きであり、tomlのbucket/region空をCLI値で
/// 補完する使い方はしない。空値はCLI指定の有無にかかわらず設定エラー2)。
/// - `--storage-class`: 空文字・空白のみは未指定扱い(aws.toml値を採用)。allowlist外は設定エラー2
/// - `--bucket` / `--region`: 空文字・空白のみ・前後空白付きは設定エラー2(値のtrimは行わない)
/// - `--profile`はTASK-9で廃止(3.7・9.1)
pub fn apply_cli_overrides(base: AwsConfig, ovr: &AwsOverrides) -> BackupResult<AwsConfig> {
    let mut cfg = base;
    if let Some(sc) = ovr.storage_class.as_deref() {
        if sc.trim().is_empty() {
            // 空文字・空白のみは未指定扱い: aws.toml値のまま
        } else if KNOWN_STORAGE_CLASSES.contains(&sc) {
            cfg.storage_class = sc.to_string();
        } else {
            return Err(BackupError::Config(format!(
                "--storage-classが既知のS3 storage classでない: {sc}(許容値: {})",
                KNOWN_STORAGE_CLASSES.join("/")
            )));
        }
    }
    if let Some(b) = ovr.bucket.as_deref() {
        if b.trim().is_empty() {
            return Err(BackupError::Config(
                "--bucketの値が空です(toml値が正である前提の一時上書きのため、空値は指定できません)"
                    .to_string(),
            ));
        }
        if b.trim() != b {
            return Err(BackupError::Config(
                "--bucketの値に前後の空白が含まれる(空白付き値はtrimせず設定エラーとする)"
                    .to_string(),
            ));
        }
        cfg.bucket = b.to_string();
    }
    if let Some(r) = ovr.region.as_deref() {
        if r.trim().is_empty() {
            return Err(BackupError::Config(
                "--regionの値が空です(toml値が正である前提の一時上書きのため、空値は指定できません)"
                    .to_string(),
            ));
        }
        if r.trim() != r {
            return Err(BackupError::Config(
                "--regionの値に前後の空白が含まれる(空白付き値はtrimせず設定エラーとする)"
                    .to_string(),
            ));
        }
        cfg.region = r.to_string();
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::BackupError;
    use crate::testutil::{
        TempDir, config_env_lock, set_amazing_backup_home, write_file, write_test_ed25519_pub,
    };

    fn config_error(e: &BackupError) -> &str {
        match e {
            BackupError::Config(m) => m,
            other => panic!("期待した区分はConfig、実際は {other:?}"),
        }
    }

    fn valid_pipeline_toml(dir: &std::path::Path, pub1: &str) -> String {
        format!(
            "[recipients]\npublic_keys = [\"{pub1}\"]\n\
             [behavior]\nkeep_going = false\n\
             [verify]\ndecrypt_key = \"{}\"\n",
            dir.join("nokey").display(),
        )
    }

    #[test]
    fn load_pipeline_valid_parses_fields() {
        let tmp = TempDir::new("conf-valid");
        let pubkey = write_test_ed25519_pub(tmp.path());
        let cfg_path = tmp.path().join("pipeline.toml");
        write_file(
            &cfg_path,
            valid_pipeline_toml(tmp.path(), &pubkey.display().to_string()).as_bytes(),
        );

        let cfg = load_pipeline(&cfg_path).expect("有効なpipeline.tomlは読み込める");
        assert_eq!(cfg.public_keys, vec![pubkey]);
        assert!(!cfg.keep_going);
        assert_eq!(cfg.decrypt_key, Some(tmp.path().join("nokey")));
    }

    /// [covers:config.xdg-default-paths]
    /// 規定path解決(XDG準拠): XDG_*_HOME設定時はその配下のamazing-backup/...へ、
    /// 未設定・空値・相対path時は~/.config等のhome配下の既定へ解決される。
    /// registry・cache dirはload_pipelineが解決結果をPipelineConfigへ載せる
    /// (env注入はclosureで行いprocess envとの競合を避ける)。
    #[test]
    fn resolve_app_paths_follows_xdg_envs() {
        let home = std::path::PathBuf::from("/home/ut");
        let getenv = |vars: &[(&str, &str)]| {
            let owned: Vec<(String, String)> = vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            move |k: &str| {
                owned
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };

        // 全XDG設定: 各env配下のamazing-backup/...へ
        let p = resolve_app_paths_from(getenv(&[
            ("HOME", home.to_str().unwrap()),
            ("XDG_CONFIG_HOME", "/xdg/cfg"),
            ("XDG_STATE_HOME", "/xdg/state"),
            ("XDG_CACHE_HOME", "/xdg/cache"),
        ]));
        assert_eq!(
            p.pipeline_config,
            PathBuf::from("/xdg/cfg/amazing-backup/pipeline.toml")
        );
        assert_eq!(
            p.aws_config,
            PathBuf::from("/xdg/cfg/amazing-backup/aws.toml")
        );
        assert_eq!(
            p.registry,
            PathBuf::from("/xdg/state/amazing-backup/registry.jsonl")
        );
        assert_eq!(p.cache_dir, PathBuf::from("/xdg/cache/amazing-backup"));

        // 全XDG未設定: home配下の既定へ
        let p = resolve_app_paths_from(getenv(&[("HOME", home.to_str().unwrap())]));
        assert_eq!(
            p.pipeline_config,
            home.join(".config/amazing-backup/pipeline.toml")
        );
        assert_eq!(p.aws_config, home.join(".config/amazing-backup/aws.toml"));
        assert_eq!(
            p.registry,
            home.join(".local/state/amazing-backup/registry.jsonl")
        );
        assert_eq!(p.cache_dir, home.join(".cache/amazing-backup"));

        // 空値・相対pathのXDG_*_HOMEは未指定扱い(絶対pathのみ有効)
        for (tag, cfg_home) in [("空値", ""), ("相対path", "relative/cfg")] {
            let p = resolve_app_paths_from(getenv(&[
                ("HOME", home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", cfg_home),
            ]));
            assert_eq!(
                p.pipeline_config,
                home.join(".config/amazing-backup/pipeline.toml"),
                "XDG_CONFIG_HOME({tag})は未指定扱い"
            );
        }

        // load_pipelineは解決結果をregistry/cache dirへ載せる([output]由来ではない)
        let _lock = config_env_lock();
        let tmp = TempDir::new("conf-xdg-paths");
        let _guard = set_amazing_backup_home(tmp.path());
        let pubkey = write_test_ed25519_pub(tmp.path());
        let cfg_path = tmp.path().join("p.toml");
        write_file(
            &cfg_path,
            valid_pipeline_toml(tmp.path(), &pubkey.display().to_string()).as_bytes(),
        );
        let cfg = load_pipeline(&cfg_path).expect("読み込める");
        assert_eq!(cfg.registry_path, tmp.path().join("registry.jsonl"));
        assert_eq!(cfg.output_dir, tmp.path().join("cache"));
    }

    /// [covers:config.abh-home-override]
    /// AMAZING_BACKUP_HOME指定時は規定pathをすべて$ABH配下へ統合(XDG_*_HOMEより優先)。
    /// 空値は未指定扱い。
    #[test]
    fn resolve_app_paths_abh_overrides_all() {
        let getenv = |vars: &[(&str, &str)]| {
            let owned: Vec<(String, String)> = vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            move |k: &str| {
                owned
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };

        // XDG系envの併設定でもABHが優先する
        let p = resolve_app_paths_from(getenv(&[
            ("AMAZING_BACKUP_HOME", "/abh"),
            ("XDG_CONFIG_HOME", "/xdg/cfg"),
            ("XDG_STATE_HOME", "/xdg/state"),
            ("XDG_CACHE_HOME", "/xdg/cache"),
        ]));
        assert_eq!(p.pipeline_config, PathBuf::from("/abh/pipeline.toml"));
        assert_eq!(p.aws_config, PathBuf::from("/abh/aws.toml"));
        assert_eq!(p.registry, PathBuf::from("/abh/registry.jsonl"));
        assert_eq!(p.cache_dir, PathBuf::from("/abh/cache"));

        // 空値は未指定扱い(XDG規定へ解決される)
        let p =
            resolve_app_paths_from(getenv(&[("AMAZING_BACKUP_HOME", ""), ("HOME", "/home/ut")]));
        assert_eq!(
            p.pipeline_config,
            PathBuf::from("/home/ut/.config/amazing-backup/pipeline.toml")
        );
    }

    /// [covers:config.output-section-rejected]
    /// [output]は廃止: 存在したら未知キー扱いの設定エラー2。
    #[test]
    fn load_pipeline_rejects_output_section() {
        let tmp = TempDir::new("conf-out-reject");
        let pubkey = write_test_ed25519_pub(tmp.path());
        let write_cfg = |name: &str, section: &str| {
            let p = tmp.path().join(name);
            write_file(
                &p,
                (valid_pipeline_toml(tmp.path(), &pubkey.display().to_string()) + section)
                    .as_bytes(),
            );
            p
        };
        for (name, section) in [
            (
                "out-dir.toml",
                "[output]\ndir = \"out\"\nregistry = \"reg.jsonl\"\n",
            ),
            ("out-keep.toml", "[output]\nkeep_artifacts = \"run\"\n"),
        ] {
            let e = load_pipeline(&write_cfg(name, section)).unwrap_err();
            let msg = config_error(&e);
            assert_eq!(e.exit_code(), 2, "[output]存在({name})は設定エラー2");
            assert!(msg.contains("output"), "未知キー名を含む: {msg}");
        }
    }

    /// [covers:config.pipeline-toml-invalid-error]
    /// 不在・TOML parseエラー・未知キー・型不一致(public_keysに文字列等)の4ケース。
    #[test]
    fn load_pipeline_rejects_invalid_inputs() {
        let tmp = TempDir::new("conf-invalid");

        // 1. ファイル不在
        let e = load_pipeline(&tmp.path().join("absent.toml")).unwrap_err();
        config_error(&e);
        assert_eq!(e.exit_code(), 2);

        // 2. TOML parseエラー
        let bad = tmp.path().join("bad.toml");
        write_file(&bad, b"[recipients\npublic_keys = [\n");
        let e = load_pipeline(&bad).unwrap_err();
        let msg = config_error(&e);
        assert!(
            msg.contains("bad.toml") || !msg.is_empty(),
            "parseエラーの詳細: {msg}"
        );

        // 3. 未知キー
        let pubkey = write_test_ed25519_pub(tmp.path());
        let unknown = tmp.path().join("unknown.toml");
        let body = valid_pipeline_toml(tmp.path(), &pubkey.display().to_string())
            + "[extra_section]\nfoo = 1\n";
        write_file(&unknown, body.as_bytes());
        let e = load_pipeline(&unknown).unwrap_err();
        config_error(&e);

        // 4. 型不一致(public_keysに文字列)
        let typebad = tmp.path().join("typebad.toml");
        let body = valid_pipeline_toml(tmp.path(), &pubkey.display().to_string()).replace(
            &format!("public_keys = [\"{}\"]", pubkey.display()),
            "public_keys = \"not-an-array\"",
        );
        write_file(&typebad, body.as_bytes());
        let e = load_pipeline(&typebad).unwrap_err();
        config_error(&e);
    }

    /// [covers:config.recipient-keys-validation]
    /// public_keys空配列・鍵ファイル不在・ssh-ed25519以外の形式。
    #[test]
    fn load_pipeline_rejects_bad_recipient_keys() {
        let tmp = TempDir::new("conf-keys");
        let pubkey = write_test_ed25519_pub(tmp.path());

        // 1. 空配列
        let empty = tmp.path().join("empty.toml");
        let body = valid_pipeline_toml(tmp.path(), &pubkey.display().to_string()).replace(
            &format!("public_keys = [\"{}\"]", pubkey.display()),
            "public_keys = []",
        );
        write_file(&empty, body.as_bytes());
        let e = load_pipeline(&empty).unwrap_err();
        config_error(&e);

        // 2. 鍵ファイル不在
        let absent_key = tmp.path().join("absent.pub");
        let body = valid_pipeline_toml(tmp.path(), &absent_key.display().to_string());
        let missing = tmp.path().join("missingkey.toml");
        write_file(&missing, body.as_bytes());
        let e = load_pipeline(&missing).unwrap_err();
        let msg = config_error(&e);
        assert!(
            msg.contains("absent.pub"),
            "不在鍵pathがmessageに含まれる: {msg}"
        );

        // 3. ssh-rsa形式(対象外)
        let rsa_path = tmp.path().join("rsa.pub");
        write_file(
            &rsa_path,
            format!("{}\n", crate::testutil::TEST_RSA_PUB).as_bytes(),
        );
        let body = valid_pipeline_toml(tmp.path(), &rsa_path.display().to_string());
        let rsa_cfg = tmp.path().join("rsakey.toml");
        write_file(&rsa_cfg, body.as_bytes());
        let e = load_pipeline(&rsa_cfg).unwrap_err();
        config_error(&e);
    }

    /// [covers:config.recipient-fingerprint-duplicate-error]
    /// 同一pathの重複指定と、異pathで同一鍵blob(copy)の両方を検出する。
    #[test]
    fn load_pipeline_rejects_duplicate_fingerprints() {
        let tmp = TempDir::new("conf-dup");
        let pubkey = write_test_ed25519_pub(tmp.path());

        // 1. 同一pathを2回列挙
        let same = tmp.path().join("same.toml");
        let body = valid_pipeline_toml(tmp.path(), &pubkey.display().to_string()).replace(
            &format!("public_keys = [\"{}\"]", pubkey.display()),
            &format!("public_keys = [\"{0}\", \"{0}\"]", pubkey.display()),
        );
        write_file(&same, body.as_bytes());
        let e = load_pipeline(&same).unwrap_err();
        config_error(&e);

        // 2. 同一鍵blobを異path(copy)に置いて列挙
        let copy = tmp.path().join("copy.pub");
        std::fs::copy(&pubkey, &copy).expect("鍵copy");
        let diff = tmp.path().join("diffpath.toml");
        let body = valid_pipeline_toml(tmp.path(), &pubkey.display().to_string()).replace(
            &format!("public_keys = [\"{}\"]", pubkey.display()),
            &format!(
                "public_keys = [\"{0}\", \"{1}\"]",
                pubkey.display(),
                copy.display()
            ),
        );
        write_file(&diff, body.as_bytes());
        let e = load_pipeline(&diff).unwrap_err();
        config_error(&e);
    }

    /// [covers:config.aws-credential-keys-rejected]
    /// credentials_fileを除くcredential系キー(値直書き)はschemaに存在しないため
    /// 未知キーエラーで拒否される(credentialは外部ファイル明示参照のみ)。
    #[test]
    fn load_aws_rejects_credential_keys() {
        let tmp = TempDir::new("conf-aws-cred");
        let p = tmp.path().join("aws.toml");
        write_file(
            &p,
            b"[aws]\nactive_profile = \"default\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"configs/secret/aws-credentials.toml\"\naws_access_key_id = \"AKIAxxxxxxxxxxxxxxxx\"\n",
        );
        let e = load_aws(&p).unwrap_err();
        let msg = config_error(&e);
        assert!(
            msg.contains("aws_access_key_id"),
            "未知キー名がmessageに含まれる: {msg}"
        );
    }

    #[test]
    fn load_aws_valid_parses_fields() {
        let tmp = TempDir::new("conf-aws-valid");
        let p = tmp.path().join("aws.toml");
        write_file(
            &p,
            b"[aws]\nactive_profile = \"default\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.default]\nbucket = \"my-bucket\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"configs/secret/aws-credentials.toml\"\nstorage_class = \"DEEP_ARCHIVE\"\n",
        );
        let cfg = load_aws(&p).expect("有効なaws.tomlは読み込める");
        assert_eq!(cfg.bucket, "my-bucket");
        assert_eq!(cfg.region, "ap-northeast-1");
        assert_eq!(
            cfg.credentials_file,
            std::path::PathBuf::from("configs/secret/aws-credentials.toml")
        );
        assert_eq!(cfg.storage_class, "DEEP_ARCHIVE");
    }

    /// [covers:config.aws-credentials-file-required]
    /// credentials_fileキーは必須(欠落はparseエラー)。
    #[test]
    fn load_aws_requires_credentials_file_key() {
        let tmp = TempDir::new("conf-aws-credreq");
        let p = tmp.path().join("aws.toml");
        write_file(&p, b"[aws]\nactive_profile = \"default\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"ap-northeast-1\"\n");
        let e = load_aws(&p).unwrap_err();
        config_error(&e);
        assert_eq!(e.exit_code(), 2);
    }

    /// [covers:config.storage-class-allowlist]
    /// storage_classは既知S3 storage classのallowlist検証: allowlist外
    /// (打ち間違い)は設定エラー2。許容値はそのまま・空文字は既定DEEP_ARCHIVE。
    #[test]
    fn load_aws_validates_storage_class_allowlist() {
        let tmp = TempDir::new("conf-aws-sc");

        // 1. 打ち間違いは設定エラー
        let typo = tmp.path().join("typo.toml");
        write_file(
            &typo,
            b"[aws]\nactive_profile = \"default\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"r\"\ncredentials_file = \"c.toml\"\nstorage_class = \"DEEP_ARCHIV\"\n",
        );
        let e = load_aws(&typo).unwrap_err();
        let msg = config_error(&e);
        assert_eq!(e.exit_code(), 2);
        assert!(
            msg.contains("DEEP_ARCHIV"),
            "対象値がmessageに含まれる: {msg}"
        );
        assert!(
            msg.contains("STANDARD"),
            "許容値一覧がmessageに含まれる: {msg}"
        );

        // 2. 許容値(代表: GLACIER・GLACIER_IR)は読み込める
        for ok_value in ["GLACIER", "GLACIER_IR", "STANDARD"] {
            let p = tmp
                .path()
                .join(format!("ok-{}.toml", ok_value.to_lowercase()));
            write_file(
                &p,
                format!("[aws]\nactive_profile = \"default\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"r\"\ncredentials_file = \"c.toml\"\nstorage_class = \"{ok_value}\"\n").as_bytes(),
            );
            let cfg = load_aws(&p).unwrap_or_else(|e| panic!("{ok_value}は許容値: {e}"));
            assert_eq!(cfg.storage_class, ok_value);
        }

        // 3. 空文字は既定DEEP_ARCHIVE
        let empty = tmp.path().join("empty-sc.toml");
        write_file(&empty, b"[aws]\nactive_profile = \"default\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"r\"\ncredentials_file = \"c.toml\"\nstorage_class = \"\"\n");
        let cfg = load_aws(&empty).expect("空文字は既定値扱い");
        assert_eq!(cfg.storage_class, "DEEP_ARCHIVE");
    }

    /// credentialファイルfixture(検証用の非実在値)を書き、そのpathを返す。
    fn write_credentials(tmp: &std::path::Path) -> std::path::PathBuf {
        let p = tmp.join("aws-credentials.toml");
        write_file(
            &p,
            b"[credentials]\naccess_key_id = \"AKITEST0000000000000A\"\nsecret_access_key = \"unit-test-not-a-real-secret\"\n",
        );
        p
    }

    /// [covers:config.aws-required-on-upload]
    /// bucket/region空はupload開始時の設定エラー2(案内文付き)。
    /// require_readyはcredentialファイルの検証まで含む(TASK-9 9.2。
    /// credentialファイル検証の詳細はconfig.credentials-file-schema系testが担う)。
    #[test]
    fn require_ready_rejects_empty_bucket_or_region() {
        let tmp = TempDir::new("conf-ready");
        let creds = write_credentials(tmp.path());
        let empty_both = AwsConfig {
            profile: "dev".into(),
            purpose: "dev".into(),
            bucket: String::new(),
            region: String::new(),
            credentials_file: creds.clone(),
            storage_class: "DEEP_ARCHIVE".into(),
            prefix: "amazing-backup/".into(),
            lifecycle: LifecycleConfig {
                noncurrent_days: 180,
                keep_versions: 10,
                mpu_abort_days: 7,
            },
        };
        let e = require_ready(&empty_both).unwrap_err();
        let msg = config_error(&e);
        assert!(
            msg.contains("plan") && msg.contains("encrypt") && msg.contains("verify"),
            "plan/encrypt/verify案内文が含まれる: {msg}"
        );

        let empty_region = AwsConfig {
            region: String::new(),
            ..empty_both
        };
        require_ready(&empty_region).unwrap_err();

        let ready = AwsConfig {
            profile: "dev".into(),
            purpose: "dev".into(),
            bucket: "b".into(),
            region: "r".into(),
            credentials_file: creds,
            storage_class: "DEEP_ARCHIVE".into(),
            prefix: "amazing-backup/".into(),
            lifecycle: LifecycleConfig {
                noncurrent_days: 180,
                keep_versions: 10,
                mpu_abort_days: 7,
            },
        };
        require_ready(&ready).expect("bucket/region・credentialファイルがあれば準備完了");
    }

    /// [covers:config.credentials-file-schema]
    /// credentialファイルschemaの単位検証(結合側はtests/s3_backend.rs):
    /// 正しい形式(session_token省略)は読み込める・[credentials]テーブル不在・
    /// 必須キー欠落・空値・前後空白付き値・未知キー・不正TOMLは設定エラー2。
    /// error文にはcredential値を含まない(pathのみ。検出用の値marker V-AKID等で検証)。
    #[test]
    fn load_credentials_schema_validation() {
        let tmp = TempDir::new("conf-credschema");
        let cases: &[(&str, &str, bool)] = &[
            (
                "正しい形式(session_token省略)",
                "[credentials]\naccess_key_id = \"V-AKID\"\nsecret_access_key = \"V-SECR\"\n",
                true,
            ),
            (
                "session_tokenあり",
                "[credentials]\naccess_key_id = \"V-AKID\"\nsecret_access_key = \"V-SECR\"\nsession_token = \"V-TOKN\"\n",
                true,
            ),
            ("不正TOML", "[credentials\nbroken", false),
            (
                "[credentials]テーブル不在",
                "[other]\naccess_key_id = \"V-AKID\"\n",
                false,
            ),
            (
                "access_key_id欠落",
                "[credentials]\nsecret_access_key = \"V-SECR\"\n",
                false,
            ),
            (
                "secret_access_key欠落",
                "[credentials]\naccess_key_id = \"V-AKID\"\n",
                false,
            ),
            (
                "access_key_id空値",
                "[credentials]\naccess_key_id = \"\"\nsecret_access_key = \"V-SECR\"\n",
                false,
            ),
            (
                "secret_access_key空値",
                "[credentials]\naccess_key_id = \"V-AKID\"\nsecret_access_key = \"\"\n",
                false,
            ),
            (
                "前後空白付き値",
                "[credentials]\naccess_key_id = \" V-AKID \"\nsecret_access_key = \"V-SECR\"\n",
                false,
            ),
            (
                "未知キー",
                "[credentials]\naccess_key_id = \"V-AKID\"\nsecret_access_key = \"V-SECR\"\ntoken = \"V-TOKN\"\n",
                false,
            ),
        ];
        for (i, (tag, body, ok)) in cases.iter().enumerate() {
            let p = tmp.path().join(format!("cred-{i}.toml"));
            write_file(&p, body.as_bytes());
            let result = load_credentials(&p);
            if *ok {
                result.unwrap_or_else(|e| panic!("{tag}は読み込めるべき: {e}"));
            } else {
                let e = result.err().unwrap_or_else(|| panic!("{tag}は設定エラー"));
                config_error(&e);
                assert_eq!(e.exit_code(), 2, "{tag}");
                // error文はcredential値を含まない(値の検出markerで検証)
                let msg = format!("{e}");
                for marker in ["V-AKID", "V-SECR", "V-TOKN"] {
                    assert!(
                        !msg.contains(marker),
                        "{tag}: 値({marker})を含まない: {msg}"
                    );
                }
            }
        }
    }

    /// [covers:config.credentials-fail-closed]
    /// credentialファイル不在は設定エラー2(credential chainへのfallbackなし)。
    #[test]
    fn load_credentials_missing_file_is_config_error() {
        let tmp = TempDir::new("conf-credno");
        let e = load_credentials(&tmp.path().join("absent-credentials.toml")).unwrap_err();
        let msg = config_error(&e);
        assert!(
            msg.contains("absent-credentials.toml"),
            "pathがmessageに含まれる: {msg}"
        );
        assert_eq!(e.exit_code(), 2);
    }

    /// [covers:config.aws-bucket-region-whitespace-rejected]
    /// [aws].bucket/[aws].regionの前後空白付き値は設定エラー2(値はtrimせず
    /// そのまま使う仕様のため、打ち間違いを拾う)。空白なしの値はそのまま読み込まれる。
    #[test]
    fn load_aws_rejects_whitespace_padded_bucket_region() {
        let tmp = TempDir::new("conf-aws-ws");

        // 1. 前後空白付きbucket/regionは設定エラー
        for (name, body) in [
            (
                "bucket",
                "[aws]\nactive_profile = \"default\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.default]\nbucket = \" my-bucket \"\nregion = \"ap-northeast-1\"\ncredentials_file = \"c.toml\"\n",
            ),
            (
                "region",
                "[aws]\nactive_profile = \"default\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.default]\nbucket = \"my-bucket\"\nregion = \" ap-northeast-1\"\ncredentials_file = \"c.toml\"\n",
            ),
        ] {
            let p = tmp.path().join(format!("ws-{}.toml", name));
            write_file(&p, body.as_bytes());
            let e = load_aws(&p).unwrap_err();
            let msg = config_error(&e);
            assert_eq!(e.exit_code(), 2);
            assert!(msg.contains(name), "対象キー名がmessageに含まれる: {msg}");
        }

        // 2. 空白なしの値はそのまま読み込まれる(trimされない)
        let p = tmp.path().join("ws-ok.toml");
        write_file(&p, b"[aws]\nactive_profile = \"default\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.default]\nbucket = \"my-bucket\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"c.toml\"\n");
        let cfg = load_aws(&p).expect("空白なしは読み込める");
        assert_eq!(cfg.bucket, "my-bucket");
        assert_eq!(cfg.region, "ap-northeast-1");
    }

    /// TASK-10設計4.1の拡張schema([layout]/[files]/keep_artifacts)を含むpipeline.toml
    /// 本文。keep_artifactsは[behavior]配下(TASK-19 XDG配置で[output]廃止に伴う移設)。
    /// 以降に[layout]と追加断片(extra)を繋ぐ。
    fn task10_pipeline_toml(
        dir: &std::path::Path,
        pub1: &str,
        keep_artifacts: &str,
        extra: &str,
    ) -> String {
        let keep_line = if keep_artifacts.is_empty() {
            String::new()
        } else {
            format!("keep_artifacts = \"{keep_artifacts}\"\n")
        };
        format!(
            "[recipients]\npublic_keys = [\"{pub1}\"]\n\
             [behavior]\nkeep_going = false\n{keep_line}\
             [verify]\ndecrypt_key = \"{}\"\n\
             [layout]\nunfiled_category = \"unfiled\"\nauto_path_roots = [\"/home/tomo/data\"]\n\
             {extra}",
            dir.join("nokey").display(),
        )
    }

    /// [covers:config.layout-and-categories-validation]
    /// [layout]/[files]の検証(TASK-18 5.15改訂): auto_path_rootsは絶対path・
    /// files.categoryと[layout].unfiled_categoryはvalidate_category_nameによる
    /// 文字列検証のみ([categories]定義存在検証は廃止・category語彙は記録値として自由化)。
    /// files写像path先頭のsys衝突検証は廃止。適正な拡張schemaは読み込める。
    #[test]
    fn load_pipeline_validates_layout_and_categories() {
        let tmp = TempDir::new("conf-t10-layout");
        let pubkey = write_test_ed25519_pub(tmp.path());
        let pub_s = pubkey.display().to_string();
        let write_cfg = |name: &str, body: String| {
            let p = tmp.path().join(name);
            write_file(&p, body.as_bytes());
            p
        };

        // 適正: 拡張schema([files]写像込み)は読み込める
        let valid = write_cfg(
            "t10-valid.toml",
            task10_pipeline_toml(
                tmp.path(),
                &pub_s,
                "run",
                "[files.\"travel.zip\"]\ncategory = \"photos\"\npath = \"2020-2023/travel\"\n",
            ),
        );
        load_pipeline(&valid).expect("適正な拡張schemaは読み込める");

        // files.categoryの[categories]未定義値は受理される(記録値として自由化)
        let p = write_cfg(
            "t10-undefined.toml",
            task10_pipeline_toml(
                tmp.path(),
                &pub_s,
                "run",
                "[files.\"x.zip\"]\ncategory = \"ghost\"\npath = \"a\"\n",
            ),
        );
        load_pipeline(&p).expect("未定義category値は文字列検査のみで受理");

        // files写像path先頭の_sys(旧予約衝突検査)は受理される(検証廃止)
        let p = write_cfg(
            "t10-sysprefix.toml",
            task10_pipeline_toml(
                tmp.path(),
                &pub_s,
                "run",
                "[files.\"x.zip\"]\ncategory = \"unfiled\"\npath = \"_sys/x\"\n",
            ),
        );
        load_pipeline(&p).expect("_sys先頭のfiles写像pathは受理される");

        // files.categoryの名前規則違反(_sys・BadName)は設定エラー2
        for (name, extra) in [
            (
                "cat-sys",
                "[files.\"x.zip\"]\ncategory = \"_sys\"\npath = \"a\"\n",
            ),
            (
                "cat-uppercase",
                "[files.\"x.zip\"]\ncategory = \"BadName\"\npath = \"a\"\n",
            ),
        ] {
            let p = write_cfg(
                &format!("t10-{name}.toml"),
                task10_pipeline_toml(tmp.path(), &pub_s, "run", extra),
            );
            let e = load_pipeline(&p).unwrap_err();
            config_error(&e);
            assert_eq!(e.exit_code(), 2, "category名違反({name})は設定エラー2");
        }

        // [categories]セクション自体の存在は設定エラー2(廃止・未知キー拒否)
        let p = write_cfg(
            "t10-categories.toml",
            task10_pipeline_toml(
                tmp.path(),
                &pub_s,
                "run",
                "[categories.unfiled]\nkeep = 5\n",
            ),
        );
        let e = load_pipeline(&p).unwrap_err();
        config_error(&e);
        assert_eq!(e.exit_code(), 2, "[categories]存在は設定エラー2");

        // auto_path_rootsに相対path
        let body = task10_pipeline_toml(tmp.path(), &pub_s, "run", "").replace(
            "auto_path_roots = [\"/home/tomo/data\"]",
            "auto_path_roots = [\"relative/path\"]",
        );
        let p = write_cfg("t10-relroot.toml", body);
        let e = load_pipeline(&p).unwrap_err();
        config_error(&e);
        assert_eq!(e.exit_code(), 2, "auto_path_roots相対pathは設定エラー2");

        // unfiled_categoryの名前規則違反
        let body = task10_pipeline_toml(tmp.path(), &pub_s, "run", "").replace(
            "unfiled_category = \"unfiled\"",
            "unfiled_category = \"Unfiled\"",
        );
        let p = write_cfg("t10-badunfiled.toml", body);
        let e = load_pipeline(&p).unwrap_err();
        config_error(&e);
        assert_eq!(e.exit_code(), 2, "unfiled_category規則違反は設定エラー2");
    }

    /// [covers:config.metadata-latest-source-path-values]
    /// [metadata].latest_source_pathは列挙値 plain(既定)|encrypted|none。
    /// その他の値・キー・セクション省略時の挙動。
    #[test]
    fn load_pipeline_validates_metadata_section() {
        let tmp = TempDir::new("conf-t18-meta");
        let pubkey = write_test_ed25519_pub(tmp.path());
        let pub_s = pubkey.display().to_string();
        let write_cfg = |name: &str, extra: &str| {
            let p = tmp.path().join(name);
            write_file(
                &p,
                task10_pipeline_toml(tmp.path(), &pub_s, "run", extra).as_bytes(),
            );
            p
        };
        // セクション・キー省略はplain既定
        let cfg = load_pipeline(&write_cfg("meta-default.toml", "")).expect("省略はplain");
        assert_eq!(cfg.metadata.latest_source_path, PathMode::Plain);
        for (v, want) in [
            ("plain", PathMode::Plain),
            ("encrypted", PathMode::Encrypted),
            ("none", PathMode::None),
        ] {
            let cfg = load_pipeline(&write_cfg(
                &format!("meta-{v}.toml"),
                &format!("[metadata]\nlatest_source_path = \"{v}\"\n"),
            ))
            .unwrap_or_else(|e| panic!("{v}は許容: {e}"));
            assert_eq!(cfg.metadata.latest_source_path, want);
        }
        // 不正値
        let e = load_pipeline(&write_cfg(
            "meta-monthly.toml",
            "[metadata]\nlatest_source_path = \"monthly\"\n",
        ))
        .unwrap_err();
        config_error(&e);
        assert_eq!(e.exit_code(), 2, "不正値は設定エラー2");
    }

    /// [covers:config.aws-profile-prefix]
    /// aws.tomlのprofile毎prefix(設計4.2・1.1): 形式違反(空・末尾スラッシュなし・
    /// 先頭スラッシュ・使用文字違反)は設定エラー2。適正値は読み込める。
    #[test]
    fn load_aws_validates_profile_prefix() {
        let tmp = TempDir::new("conf-t10-prefix");
        let base = "[aws]\nactive_profile = \"dev\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.dev]\nbucket = \"b\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"c.toml\"\n";

        // 適正なprefixは読み込める
        let p = tmp.path().join("prefix-ok.toml");
        write_file(
            &p,
            (base.to_string() + "prefix = \"amazing-backup/\"\n").as_bytes(),
        );
        load_aws(&p).expect("適正なprefixは読み込める");

        for (name, prefix) in [
            ("空", ""),
            ("末尾スラッシュなし", "amazing-backup"),
            ("先頭スラッシュ", "/amazing-backup/"),
            ("使用文字違反", "Backup_Remote/"),
        ] {
            let p = tmp.path().join(format!("prefix-{}.toml", name.len()));
            write_file(
                &p,
                (base.to_string() + &format!("prefix = \"{prefix}\"\n")).as_bytes(),
            );
            let e = load_aws(&p).unwrap_err();
            config_error(&e);
            assert_eq!(e.exit_code(), 2, "prefix形式違反({name})は設定エラー2");
        }
    }

    /// [covers:config.aws-lifecycle-required-and-ranged]
    /// [aws.lifecycle]の3キーは必須(欠落はparseエラー)・範囲検証
    /// (keep_versions 1..=100・noncurrent_days>=1・mpu_abort_days>=1)・
    /// DEEP_ARCHIVEでnoncurrent_days<180は設定エラー2。
    #[test]
    fn load_aws_validates_lifecycle() {
        let tmp = TempDir::new("conf-t18-lyf");
        let base = |lc: &str, sc: &str| {
            format!(
                "[aws]\nactive_profile = \"dev\"\n{lc}\n[aws.profiles.dev]\nbucket = \"b\"\nregion = \"r\"\ncredentials_file = \"c.toml\"\nstorage_class = \"{sc}\"\n"
            )
        };
        let lifecycle = |n: &str, k: &str, m: &str| {
            format!(
                "[aws.lifecycle]\nnoncurrent_days = {n}\nkeep_versions = {k}\nmpu_abort_days = {m}\n"
            )
        };
        let write = |name: &str, body: String| {
            let p = tmp.path().join(name);
            write_file(&p, body.as_bytes());
            p
        };

        // 適正値(STANDARD・DEEP_ARCHIVEとも180以上は可)
        for (name, sc) in [("ok-std", "STANDARD"), ("ok-da", "DEEP_ARCHIVE")] {
            let p = write(name, base(&lifecycle("180", "10", "7"), sc));
            load_aws(&p).unwrap_or_else(|e| panic!("{name}は許容: {e}"));
        }
        // 欠落(キー・セクション)
        for (name, lc) in [
            (
                "no-keep",
                "[aws.lifecycle]\nnoncurrent_days = 180\nmpu_abort_days = 7\n",
            ),
            (
                "no-ncd",
                "[aws.lifecycle]\nkeep_versions = 10\nmpu_abort_days = 7\n",
            ),
            (
                "no-mpu",
                "[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\n",
            ),
            ("no-section", ""),
        ] {
            let p = write(name, base(lc, "STANDARD"));
            let e = load_aws(&p).unwrap_err();
            config_error(&e);
            assert_eq!(e.exit_code(), 2, "{name}の欠落は設定エラー2");
        }
        // 範囲外
        for (name, lc) in [
            ("keep0", &lifecycle("180", "0", "7")),
            ("keep101", &lifecycle("180", "101", "7")),
            ("ncd0", &lifecycle("0", "10", "7")),
            ("mpu0", &lifecycle("180", "10", "0")),
        ] {
            let p = write(name, base(lc, "STANDARD"));
            let e = load_aws(&p).unwrap_err();
            config_error(&e);
            assert_eq!(e.exit_code(), 2, "{name}は設定エラー2");
        }
        // DEEP_ARCHIVEでnoncurrent_days<180
        let p = write("da179", base(&lifecycle("179", "10", "7"), "DEEP_ARCHIVE"));
        let e = load_aws(&p).unwrap_err();
        let msg = config_error(&e);
        assert_eq!(e.exit_code(), 2);
        assert!(msg.contains("180"), "Deep Archive最低保存の案内: {msg}");
    }

    /// [covers:config.keep-artifacts-enum]
    /// [behavior].keep_artifactsは列挙値 "run"|"always"(省略時はrun既定)。
    /// TASK-19 XDG配置で[output]廃止に伴い[behavior]へ移設。その他の値は設定エラー2。
    #[test]
    fn load_pipeline_validates_keep_artifacts_enum() {
        let tmp = TempDir::new("conf-t10-keepart");
        let pubkey = write_test_ed25519_pub(tmp.path());
        let pub_s = pubkey.display().to_string();
        let write_cfg = |name: &str, keep_artifacts: &str| {
            let body = task10_pipeline_toml(tmp.path(), &pub_s, keep_artifacts, "");
            let p = tmp.path().join(name);
            write_file(&p, body.as_bytes());
            p
        };

        for value in ["run", "always"] {
            let p = write_cfg(&format!("keepart-{value}.toml"), value);
            load_pipeline(&p)
                .unwrap_or_else(|e| panic!("keep_artifacts={value}は読み込めるべき: {e}"));
        }
        // キー省略は既定run扱いで読み込める
        let p = write_cfg("keepart-default.toml", "");
        load_pipeline(&p).expect("keep_artifacts省略は既定値扱い");

        // 不正値
        let p = write_cfg("keepart-monthly.toml", "monthly");
        let e = load_pipeline(&p).unwrap_err();
        config_error(&e);
        assert_eq!(e.exit_code(), 2, "keep_artifacts不正値は設定エラー2");
    }
}
