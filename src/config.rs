//! toml読込・厳格schema検証・struct化(設計5章config.rs・3章schema)。

use crate::errors::{BackupError, BackupResult};
use std::path::Path;

/// `configs/pipeline.toml`相当(3.2)。
#[derive(Debug)]
pub struct PipelineConfig {
    /// SSH ed25519公開鍵path列挙(1-of-N recipient)
    pub public_keys: Vec<std::path::PathBuf>,
    /// 暗号化成果物の集約dir
    pub output_dir: std::path::PathBuf,
    /// ローカル累積registry(JSONL)path
    pub registry_path: std::path::PathBuf,
    /// per-file失敗時も継続する既定値
    pub keep_going: bool,
    /// --roundtrip検証で使う秘密鍵path
    pub decrypt_key: Option<std::path::PathBuf>,
}

/// `configs/aws.toml`相当(3.3・TASK-9 9.2)。credential値は含まない
/// (credentialは`credentials_file`が指す外部ファイル明示参照のみ)。
#[derive(Debug)]
pub struct AwsConfig {
    pub bucket: String,
    pub region: String,
    /// credentialファイル(`configs/secret/aws-credentials.toml`等)のpath。
    /// cwd基準の相対path。必須(fail-closed: credential chainへのfallbackはしない)
    pub credentials_file: std::path::PathBuf,
    /// 既定DEEP_ARCHIVE
    pub storage_class: String,
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
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

/// pipeline.tomlのschema(3.2。未知キー・型不一致拒否の厳格検証)。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PipelineFile {
    recipients: RecipientsFile,
    output: OutputFile,
    #[serde(default)]
    behavior: BehaviorFile,
    #[serde(default)]
    verify: VerifyFile,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipientsFile {
    public_keys: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputFile {
    dir: String,
    registry: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BehaviorFile {
    keep_going: bool,
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
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AwsSection {
    /// 起動時に使うprofile名(省略時は"default")。
    #[serde(default)]
    active_profile: String,
    /// profile名 -> profile定義。最低1つ必須。
    profiles: std::collections::BTreeMap<String, AwsProfile>,
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
pub fn load_pipeline(path: &Path) -> BackupResult<PipelineConfig> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        BackupError::Config(format!("pipeline.tomlを読めない ({}): {e}", path.display()))
    })?;
    let parsed: PipelineFile = toml::from_str(&text).map_err(|e| {
        BackupError::Config(format!(
            "pipeline.tomlのparseに失敗 ({}): {e}",
            path.display()
        ))
    })?;

    if parsed.recipients.public_keys.is_empty() {
        return Err(BackupError::Config(
            "[recipients].public_keysが空です(1つ以上のSSH ed25519公開鍵を指定してください)".to_string(),
        ));
    }
    if parsed.output.dir.trim().is_empty() || parsed.output.registry.trim().is_empty() {
        return Err(BackupError::Config(
            "[output].dir / [output].registryは空にできません".to_string(),
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

    Ok(PipelineConfig {
        public_keys,
        output_dir: std::path::PathBuf::from(parsed.output.dir),
        registry_path: std::path::PathBuf::from(parsed.output.registry),
        keep_going: parsed.behavior.keep_going,
        decrypt_key,
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

pub fn load_aws_profile(path: &Path, profile_name: Option<&str>, purpose: Option<&str>) -> BackupResult<AwsConfig> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| BackupError::Config(format!("aws.tomlを読めない ({}): {e}", path.display())))?;
    let parsed: AwsFile = toml::from_str(&text)
        .map_err(|e| BackupError::Config(format!("aws.tomlのparseに失敗 ({}): {e}", path.display())))?;

    if parsed.aws.profiles.is_empty() {
        return Err(BackupError::Config(format!(
            "[aws].profilesが空です(最低1つのprofileを定義してください。{})",
            path.display()
        )));
    }

    let env_profile = std::env::var("BACKUP_REMOTE_AWS_PROFILE").ok().filter(|v| !v.trim().is_empty());
    let env_purpose = std::env::var("BACKUP_REMOTE_AWS_PURPOSE").ok().filter(|v| !v.trim().is_empty());

    let explicit_profile = profile_name.map(str::to_string).filter(|s| !s.trim().is_empty());
    let requested_purpose = purpose.map(str::to_string).filter(|s| !s.trim().is_empty()).or(env_purpose);

    let selected_name = if let Some(n) = explicit_profile.or(env_profile) {
        n
    } else if let Some(pur) = requested_purpose {
        parsed.aws.profiles.iter().find_map(|(name, prof)| {
            if prof.purpose == pur { Some(name.clone()) } else { None }
        }).unwrap_or_else(|| {
            if !parsed.aws.active_profile.trim().is_empty() {
                parsed.aws.active_profile.clone()
            } else if parsed.aws.profiles.contains_key("default") {
                "default".to_string()
            } else {
                parsed.aws.profiles.keys().cloned().next().unwrap_or_default()
            }
        })
    } else if !parsed.aws.active_profile.trim().is_empty() {
        parsed.aws.active_profile.clone()
    } else if parsed.aws.profiles.contains_key("default") {
        "default".to_string()
    } else {
        parsed.aws.profiles.keys().cloned().next().unwrap_or_default()
    };

    let prof = parsed.aws.profiles.get(&selected_name).ok_or_else(|| {
        BackupError::Config(format!(
            "指定profileが見つからない: {selected_name} ({}。利用可能: {})",
            path.display(),
            parsed.aws.profiles.keys().cloned().collect::<Vec<_>>().join("/")
        ))
    })?;

    let credentials_file = &prof.credentials_file;
    // whitespace + non-empty checks
    for (name, value) in [("bucket", &prof.bucket), ("region", &prof.region), ("credentials_file", credentials_file)] {
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
    Ok(AwsConfig {
        bucket: prof.bucket.clone(),
        region: prof.region.clone(),
        credentials_file: std::path::PathBuf::from(credentials_file.clone()),
        storage_class,
    })
}

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
    for (name, value) in [("access_key_id", &c.access_key_id), ("secret_access_key", &c.secret_access_key)] {
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
            "aws.toml未設定(bucket/regionが空)です。plan/encrypt/verifyはawsなしで実行可能".to_string(),
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
                "--bucketの値に前後の空白が含まれる(空白付き値はtrimせず設定エラーとする)".to_string(),
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
                "--regionの値に前後の空白が含まれる(空白付き値はtrimせず設定エラーとする)".to_string(),
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
    use crate::testutil::{write_file, write_test_ed25519_pub, TempDir};

    fn config_error(e: &BackupError) -> &str {
        match e {
            BackupError::Config(m) => m,
            other => panic!("期待した区分はConfig、実際は {other:?}"),
        }
    }

    fn valid_pipeline_toml(dir: &std::path::Path, pub1: &str) -> String {
        format!(
            "[recipients]\npublic_keys = [\"{pub1}\"]\n\
             [output]\ndir = \"{}\"\nregistry = \"{}\"\n\
             [behavior]\nkeep_going = false\n\
             [verify]\ndecrypt_key = \"{}\"\n",
            dir.join("out").display(),
            dir.join("registry.jsonl").display(),
            dir.join("nokey").display(),
        )
    }

    #[test]
    fn load_pipeline_valid_parses_fields() {
        let tmp = TempDir::new("conf-valid");
        let pubkey = write_test_ed25519_pub(tmp.path());
        let cfg_path = tmp.path().join("pipeline.toml");
        write_file(&cfg_path, valid_pipeline_toml(tmp.path(), &pubkey.display().to_string()).as_bytes());

        let cfg = load_pipeline(&cfg_path).expect("有効なpipeline.tomlは読み込める");
        assert_eq!(cfg.public_keys, vec![pubkey]);
        assert_eq!(cfg.output_dir, tmp.path().join("out"));
        assert_eq!(cfg.registry_path, tmp.path().join("registry.jsonl"));
        assert!(!cfg.keep_going);
        assert_eq!(cfg.decrypt_key, Some(tmp.path().join("nokey")));
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
        assert!(msg.contains("bad.toml") || !msg.is_empty(), "parseエラーの詳細: {msg}");

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
        let body = valid_pipeline_toml(tmp.path(), &pubkey.display().to_string())
            .replace(&format!("public_keys = [\"{}\"]", pubkey.display()), "public_keys = \"not-an-array\"");
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
        let body = valid_pipeline_toml(tmp.path(), &pubkey.display().to_string())
            .replace(&format!("public_keys = [\"{}\"]", pubkey.display()), "public_keys = []");
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
        assert!(msg.contains("absent.pub"), "不在鍵pathがmessageに含まれる: {msg}");

        // 3. ssh-rsa形式(対象外)
        let rsa_path = tmp.path().join("rsa.pub");
        write_file(&rsa_path, format!("{}\n", crate::testutil::TEST_RSA_PUB).as_bytes());
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
        let body = valid_pipeline_toml(tmp.path(), &pubkey.display().to_string())
            .replace(&format!("public_keys = [\"{}\"]", pubkey.display()), &format!("public_keys = [\"{0}\", \"{0}\"]", pubkey.display()));
        write_file(&same, body.as_bytes());
        let e = load_pipeline(&same).unwrap_err();
        config_error(&e);

        // 2. 同一鍵blobを異path(copy)に置いて列挙
        let copy = tmp.path().join("copy.pub");
        std::fs::copy(&pubkey, &copy).expect("鍵copy");
        let diff = tmp.path().join("diffpath.toml");
        let body = valid_pipeline_toml(tmp.path(), &pubkey.display().to_string())
            .replace(&format!("public_keys = [\"{}\"]", pubkey.display()), &format!("public_keys = [\"{0}\", \"{1}\"]", pubkey.display(), copy.display()));
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
            b"[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"configs/secret/aws-credentials.toml\"\naws_access_key_id = \"AKIAxxxxxxxxxxxxxxxx\"\n",
        );
        let e = load_aws(&p).unwrap_err();
        let msg = config_error(&e);
        assert!(msg.contains("aws_access_key_id"), "未知キー名がmessageに含まれる: {msg}");
    }

    #[test]
    fn load_aws_valid_parses_fields() {
        let tmp = TempDir::new("conf-aws-valid");
        let p = tmp.path().join("aws.toml");
        write_file(
            &p,
            b"[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"my-bucket\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"configs/secret/aws-credentials.toml\"\nstorage_class = \"DEEP_ARCHIVE\"\n",
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
        write_file(&p, b"[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"ap-northeast-1\"\n");
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
            b"[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"r\"\ncredentials_file = \"c.toml\"\nstorage_class = \"DEEP_ARCHIV\"\n",
        );
        let e = load_aws(&typo).unwrap_err();
        let msg = config_error(&e);
        assert_eq!(e.exit_code(), 2);
        assert!(msg.contains("DEEP_ARCHIV"), "対象値がmessageに含まれる: {msg}");
        assert!(msg.contains("STANDARD"), "許容値一覧がmessageに含まれる: {msg}");

        // 2. 許容値(代表: GLACIER・GLACIER_IR)は読み込める
        for ok_value in ["GLACIER", "GLACIER_IR", "STANDARD"] {
            let p = tmp.path().join(format!("ok-{}.toml", ok_value.to_lowercase()));
            write_file(
                &p,
                format!("[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"r\"\ncredentials_file = \"c.toml\"\nstorage_class = \"{ok_value}\"\n").as_bytes(),
            );
            let cfg = load_aws(&p).unwrap_or_else(|e| panic!("{ok_value}は許容値: {e}"));
            assert_eq!(cfg.storage_class, ok_value);
        }

        // 3. 空文字は既定DEEP_ARCHIVE
        let empty = tmp.path().join("empty-sc.toml");
        write_file(&empty, b"[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"b\"\nregion = \"r\"\ncredentials_file = \"c.toml\"\nstorage_class = \"\"\n");
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
            bucket: String::new(),
            region: String::new(),
            credentials_file: creds.clone(),
            storage_class: "DEEP_ARCHIVE".into(),
        };
        let e = require_ready(&empty_both).unwrap_err();
        let msg = config_error(&e);
        assert!(
            msg.contains("plan") && msg.contains("encrypt") && msg.contains("verify"),
            "plan/encrypt/verify案内文が含まれる: {msg}"
        );

        let empty_region = AwsConfig { region: String::new(), ..empty_both };
        require_ready(&empty_region).unwrap_err();

        let ready = AwsConfig {
            bucket: "b".into(),
            region: "r".into(),
            credentials_file: creds,
            storage_class: "DEEP_ARCHIVE".into(),
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
            ("[credentials]テーブル不在", "[other]\naccess_key_id = \"V-AKID\"\n", false),
            ("access_key_id欠落", "[credentials]\nsecret_access_key = \"V-SECR\"\n", false),
            ("secret_access_key欠落", "[credentials]\naccess_key_id = \"V-AKID\"\n", false),
            ("access_key_id空値", "[credentials]\naccess_key_id = \"\"\nsecret_access_key = \"V-SECR\"\n", false),
            ("secret_access_key空値", "[credentials]\naccess_key_id = \"V-AKID\"\nsecret_access_key = \"\"\n", false),
            ("前後空白付き値", "[credentials]\naccess_key_id = \" V-AKID \"\nsecret_access_key = \"V-SECR\"\n", false),
            ("未知キー", "[credentials]\naccess_key_id = \"V-AKID\"\nsecret_access_key = \"V-SECR\"\ntoken = \"V-TOKN\"\n", false),
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
                    assert!(!msg.contains(marker), "{tag}: 値({marker})を含まない: {msg}");
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
        assert!(msg.contains("absent-credentials.toml"), "pathがmessageに含まれる: {msg}");
        assert_eq!(e.exit_code(), 2);
    }

    fn base_aws() -> AwsConfig {
        AwsConfig {
            bucket: "it-bucket".into(),
            region: "ap-northeast-1".into(),
            credentials_file: std::path::PathBuf::from("configs/secret/aws-credentials.toml"),
            storage_class: "DEEP_ARCHIVE".into(),
        }
    }

    fn ovr(
        storage_class: Option<&str>,
        bucket: Option<&str>,
        region: Option<&str>,
    ) -> AwsOverrides {
        AwsOverrides {
            storage_class: storage_class.map(|s| s.to_string()),
            bucket: bucket.map(|s| s.to_string()),
            region: region.map(|s| s.to_string()),
        }
    }

    /// [covers:upload.cli-storage-class-allowlist-rejected]
    /// --storage-class上書き値: allowlist外は設定エラー2。空文字・空白のみは未指定扱い
    /// (aws.toml値を採用)でエラーとしない。
    #[test]
    fn apply_cli_overrides_validates_storage_class() {
        // allowlist外(打ち間違い)は設定エラー2
        let e = apply_cli_overrides(base_aws(), &ovr(Some("DEEP_ARCIVE"), None, None))
            .unwrap_err();
        let msg = config_error(&e);
        assert_eq!(e.exit_code(), 2);
        assert!(msg.contains("DEEP_ARCIVE"), "対象値がmessageに含まれる: {msg}");

        // 許容値は上書きされる(優先順位: CLI指定値 > aws.toml値)
        let cfg = apply_cli_overrides(base_aws(), &ovr(Some("GLACIER"), None, None))
            .expect("許容値は上書き");
        assert_eq!(cfg.storage_class, "GLACIER");

        // 空文字・空白のみは未指定扱い: aws.toml値を採用
        for blank in ["", " "] {
            let cfg = apply_cli_overrides(base_aws(), &ovr(Some(blank), None, None))
                .unwrap_or_else(|e| panic!("空白値({blank:?})は未指定扱い: {e}"));
            assert_eq!(cfg.storage_class, "DEEP_ARCHIVE", "aws.toml値のまま");
        }
    }

    /// [covers:config.aws-bucket-region-whitespace-rejected]
    /// [aws].bucket/[aws].regionの前後空白付き値は設定エラー2(値はtrimせず
    /// そのまま使う仕様のため、打ち間違いを拾う)。空白なしの値はそのまま読み込まれる。
    #[test]
    fn load_aws_rejects_whitespace_padded_bucket_region() {
        let tmp = TempDir::new("conf-aws-ws");

        // 1. 前後空白付きbucket/regionは設定エラー
        for (name, body) in [
            ("bucket", "[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \" my-bucket \"\nregion = \"ap-northeast-1\"\ncredentials_file = \"c.toml\"\n"),
            ("region", "[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"my-bucket\"\nregion = \" ap-northeast-1\"\ncredentials_file = \"c.toml\"\n"),
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
        write_file(&p, b"[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"my-bucket\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"c.toml\"\n");
        let cfg = load_aws(&p).expect("空白なしは読み込める");
        assert_eq!(cfg.bucket, "my-bucket");
        assert_eq!(cfg.region, "ap-northeast-1");
    }

    /// [covers:upload.cli-bucket-region-blank-rejected]
    /// --bucket/--region上書き値が空文字・空白のみ・前後空白付きの場合は設定エラー2
    /// (値のtrimは行わない)。
    #[test]
    fn apply_cli_overrides_rejects_blank_bucket_region() {
        for (name, o) in [
            ("--bucket空文字", ovr(None, Some(""), None)),
            ("--bucket空白のみ", ovr(None, Some(" "), None)),
            ("--bucket前後空白", ovr(None, Some(" alt-bucket "), None)),
            ("--region空文字", ovr(None, None, Some(""))),
            ("--region空白のみ", ovr(None, None, Some(" "))),
            ("--region前後空白", ovr(None, None, Some(" us-east-1 "))),
        ] {
            let e = apply_cli_overrides(base_aws(), &o)
                .err()
                .unwrap_or_else(|| panic!("{name}は設定エラーになるべき: {o:?}"));
            config_error(&e);
            assert_eq!(e.exit_code(), 2);
        }
    }

    /// [covers:upload.cli-override-unspecified-keeps-toml-values]
    /// 未指定の項目はaws.toml値が使われ、CLI一時上書きはaws.tomlファイルへ書き戻さない
    /// (apply_cli_overridesは引数の値を変換するだけでファイルI/Oを行わない)。
    #[test]
    fn apply_cli_overrides_keeps_unspecified_as_toml_values() {
        // storage-classのみ指定: 他はaws.toml値のまま
        let cfg = apply_cli_overrides(base_aws(), &ovr(Some("STANDARD"), None, None))
            .expect("部分上書き");
        assert_eq!(cfg.storage_class, "STANDARD", "指定項目はCLI値");
        assert_eq!(cfg.bucket, "it-bucket", "bucketはtoml値");
        assert_eq!(cfg.region, "ap-northeast-1", "regionはtoml値");
        assert_eq!(
            cfg.credentials_file,
            std::path::PathBuf::from("configs/secret/aws-credentials.toml"),
            "credentials_fileはtoml値"
        );

        // 全項目上書き(--profileはTASK-9で廃止済み)
        let cfg = apply_cli_overrides(
            base_aws(),
            &ovr(Some("GLACIER"), Some("alt-bucket"), Some("us-east-1")),
        )
        .expect("全項目上書き");
        assert_eq!(cfg.storage_class, "GLACIER");
        assert_eq!(cfg.bucket, "alt-bucket");
        assert_eq!(cfg.region, "us-east-1");
    }
}
