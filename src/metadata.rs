//! metadata_{title}.json(TASK-18設計2章・5.5)。実体の身元票(平文JSON・STANDARD固定)。
//! 災害時にS3コンソールだけで実体を同定する目的。registry reduceから正規形として
//! 導出でき、書込は現行metadata keyの内容とのbyte比較により変更時のみ行う(2.3)。

use crate::config::{AwsConfig, PathMode, PipelineConfig};
use crate::errors::{BackupError, BackupResult};
use crate::registry::{self, Reduced};
use crate::s3::S3Backend;
use base64::Engine as _;

/// metadataのschema版数。
pub const METADATA_SCHEMA: i64 = 1;

/// metadata_{title}.jsonの1件(設計2.1)。serializeはstruct宣言順で固定
/// (同内容ならbyte同一=差分比較の前提)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MetadataDoc {
    pub schema: i64,
    pub record_type: String,
    /// 実体UUID(v7 hyphenated)
    pub uuid: String,
    /// 現行title
    pub title: String,
    /// 現行版の平文sha256(hex 64字)
    pub source_sha256: String,
    /// 平文byte数
    pub source_size: u64,
    /// 現行版の暗号文sha256(hex 64字)
    pub age_sha256: String,
    /// 暗号文byte数
    pub age_size: u64,
    /// 現行default.age版のS3 version_id
    pub version_id: String,
    /// 現行版の日付`YYYY-MM-DD`(JST)
    pub date: String,
    /// 現行版の暗号化時刻(ISO8601 JST)
    pub version_ts: String,
    /// 現行版暗号化時のrecipient指紋列(`SHA256:<base64 nopad>`・設定順)
    pub recipients: Vec<String>,
    /// `{pin: string, date: "YYYY-MM-DD", source_sha256: hex64}`。pin名辞書順
    pub pins: Vec<MetadataPin>,
    /// 最新所在path(3形態。2.2・承認事項11)
    pub latest_source_path: serde_json::Value,
}

/// metadata pins[]の1件。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MetadataPin {
    pub pin: String,
    pub date: String,
    pub source_sha256: String,
}

/// latest_source_pathのencrypted表現(2.2)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PathCipher {
    pub format: String,
    pub recipients: Vec<String>,
    pub sha256: String,
    pub data: String,
}

/// path文字列(UTF-8)をageで暗号化しencrypted表現を組み立てる(2.2)。
/// 鍵はpipeline.tomlのpublic_keysと同一(recipients指紋列は鍵特定用に一緒に記録)。
pub(crate) fn encrypt_path(
    path: &str,
    public_keys: &[std::path::PathBuf],
) -> BackupResult<PathCipher> {
    let mut recipients = Vec::with_capacity(public_keys.len());
    for k in public_keys {
        let pk = crate::fingerprint::parse_ssh_pubkey(k)?;
        recipients.push(crate::fingerprint::fingerprint(&pk));
    }
    let tmp = std::env::temp_dir().join(format!(
        "amazing-backup-meta-path-{}-{}.age",
        std::process::id(),
        crate::storage::sha256_hex(path.as_bytes())
            .chars()
            .take(16)
            .collect::<String>()
    ));
    let result = (|| -> BackupResult<PathCipher> {
        crate::crypto::encrypt_bytes(path.as_bytes(), &tmp, public_keys)?;
        let blob = std::fs::read(&tmp)
            .map_err(|e| BackupError::Process(format!("暗号化path blobの読込に失敗: {e}")))?;
        Ok(PathCipher {
            format: "age".into(),
            recipients,
            sha256: crate::storage::sha256_hex(&blob),
            data: base64::engine::general_purpose::STANDARD.encode(&blob),
        })
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

/// registry reduceからMetadataDocの正規形を導出する(2.1・2.3.1)。
/// - 現行title=uuid直近file_writeのtitle
/// - 現行版=bucket一致するuuid直近file_upload
/// - pins=未削除・bucket一致pin_writeから{pin,date,source_sha256}(pin名辞書順)
/// - recipients=現行版のbatch_idとuuidが一致するfile_writeのrecipients
///   (欠損時はuuid直近file_writeの値へfall back)
/// - latest_source_path=last_backup最新のlocation([metadata]設定の3形態)
pub fn build_metadata(
    reduced: &Reduced,
    uuid: &str,
    effective_bucket: &str,
    canonical_bucket: &str,
    cfg: &PipelineConfig,
) -> BackupResult<Option<MetadataDoc>> {
    let Some(current) = reduced.file_uploads.iter().rfind(|u| {
        u.uuid == uuid
            && registry::record_bucket_matched(&u.s3_bucket, effective_bucket, canonical_bucket)
    }) else {
        // 当該bucketに現行版が無い実体はmetadata対象外(書込しない)
        return Ok(None);
    };
    let title = registry::entity_title(reduced, uuid).unwrap_or_else(|| current.title.clone());
    // recipients: 現行版のbatch_idとuuidが一致するfile_write(同一run内は同一設定のため一意。
    // 欠損時はuuid直近file_writeの値へfall back)
    let recipients = reduced
        .file_writes
        .iter()
        .rfind(|w| w.uuid == uuid && w.batch_id == current.batch_id)
        .or_else(|| reduced.file_writes.iter().rfind(|w| w.uuid == uuid))
        .map(|w| w.recipients.clone())
        .unwrap_or_default();
    // pins: 未削除・bucket一致・uuid一致(並びはpin名辞書順で確定=冪等)
    let mut pins: Vec<MetadataPin> = Vec::new();
    for pin in reduced.pin_writes.values() {
        if pin.uuid != uuid {
            continue;
        }
        if !registry::record_bucket_matched(&pin.s3_bucket, effective_bucket, canonical_bucket) {
            continue;
        }
        if reduced
            .pin_removes
            .contains_key(&(pin.pin_s3_key.clone(), pin.s3_bucket.clone()))
        {
            continue;
        }
        pins.push(MetadataPin {
            pin: pin.pin.clone(),
            date: pin.date.clone(),
            source_sha256: pin.source_sha256.clone(),
        });
    }
    pins.sort_by(|a, b| a.pin.cmp(&b.pin));
    // latest_source_path: location_remove除外適用後のrecorded_at最大file_writeの所在
    let latest_write = reduced
        .file_writes
        .iter()
        .filter(|w| {
            w.uuid == uuid && {
                let removed = reduced.location_removes.iter().any(|lr| {
                    lr.identity_hash == w.identity_hash && lr.recorded_at >= w.recorded_at
                });
                !removed
            }
        })
        .max_by(|a, b| a.recorded_at.cmp(&b.recorded_at));
    let latest_source_path = match latest_write {
        None => serde_json::Value::Null,
        Some(w) => match cfg.metadata.latest_source_path {
            PathMode::None => serde_json::Value::Null,
            PathMode::Plain => serde_json::Value::String(w.source_path.clone()),
            PathMode::Encrypted => {
                let cipher = encrypt_path(&w.source_path, &cfg.public_keys)?;
                serde_json::to_value(&cipher).map_err(|e| {
                    BackupError::Internal(format!("latest_source_pathのserializeに失敗: {e}"))
                })?
            }
        },
    };
    Ok(Some(MetadataDoc {
        schema: METADATA_SCHEMA,
        record_type: "metadata".into(),
        uuid: uuid.to_string(),
        title,
        source_sha256: current.source_sha256.clone(),
        source_size: current.source_size,
        age_sha256: current.age_sha256.clone(),
        age_size: current.age_size,
        version_id: current.version_id.clone(),
        date: current.version_ts.get(..10).unwrap_or("").to_string(),
        version_ts: current.version_ts.clone(),
        recipients,
        pins,
        latest_source_path,
    }))
}

/// metadata objectのGET。不在(404)はNone・404以外の失敗はエラー伝播(2.3低3)。
/// head→getの二段でなくget_to_file_checked一段で判定する(head/get間の削除競合も解消)。
pub fn read_metadata(
    backend: &dyn S3Backend,
    bucket: &str,
    key: &str,
) -> BackupResult<Option<MetadataDoc>> {
    let tmp = std::env::temp_dir().join(format!(
        "amazing-backup-meta-dl-{}-{}.json",
        std::process::id(),
        crate::storage::sha256_hex(key.as_bytes())
            .chars()
            .take(16)
            .collect::<String>()
    ));
    let result = (|| -> BackupResult<Option<MetadataDoc>> {
        if !backend.get_to_file_checked(bucket, key, &tmp)? {
            return Ok(None);
        }
        let bytes = std::fs::read(&tmp).map_err(|e| {
            BackupError::Process(format!("metadata objectの読込に失敗({key}): {e}"))
        })?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| BackupError::Process(format!("metadata objectのparseに失敗({key}): {e}")))
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

/// 導出docを現行metadata keyの内容(平文)とbyte比較し、内容不一致または不在のときのみ
/// PUTする(STANDARD固定)。戻り値はPUTしたか。同一内容なら書かない(非現行版の
/// 無意味な増加を防ぐ)。
pub fn put_metadata_if_changed(
    backend: &dyn S3Backend,
    bucket: &str,
    prefix: &str,
    uuid: &str,
    doc: &MetadataDoc,
) -> BackupResult<bool> {
    let key = crate::layout::metadata_object_key(prefix, uuid, &doc.title)?;
    let body = serde_json::to_vec(doc)
        .map_err(|e| BackupError::Internal(format!("metadataのserializeに失敗: {e}")))?;
    if let Some(existing) = read_metadata(backend, bucket, &key)? {
        let existing_body = serde_json::to_vec(&existing)
            .map_err(|e| BackupError::Internal(format!("metadataのserializeに失敗: {e}")))?;
        if existing_body == body {
            return Ok(false);
        }
    }
    let tmp = std::env::temp_dir().join(format!(
        "amazing-backup-meta-put-{}-{}.json",
        std::process::id(),
        crate::storage::sha256_hex(key.as_bytes())
            .chars()
            .take(16)
            .collect::<String>()
    ));
    let result = (|| -> BackupResult<()> {
        std::fs::write(&tmp, &body)
            .map_err(|e| BackupError::Process(format!("metadataの一時file書込に失敗: {e}")))?;
        backend.put(bucket, &key, &tmp, "STANDARD")?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    result.map(|()| true)
}

/// 対象entity1件のmetadata同期(2.3): build→put_if_changed→旧titleキー掃除。
/// 旧titleキーの特定はlist_keysで`{prefix}data/{uuid}/`配下の`metadata_`接頭辞キーを
/// 列挙し、現行titleキー以外をすべてDELETE(delete marker付与)する(冪等・複数回の
/// title変更やdelete失敗の残骸を掃除。list_keys呼び出しはuuidごとに1回)。
/// 戻り値は書込(PUT)を行ったか。
pub fn sync_entity_metadata(
    backend: &dyn S3Backend,
    aws: &AwsConfig,
    canonical_bucket: &str,
    cfg: &PipelineConfig,
    reduced: &Reduced,
    uuid: &str,
) -> BackupResult<bool> {
    let Some(doc) = build_metadata(reduced, uuid, &aws.bucket, canonical_bucket, cfg)? else {
        return Ok(false);
    };
    let written = put_metadata_if_changed(backend, &aws.bucket, &aws.prefix, uuid, &doc)?;
    // 旧titleキー掃除(現行titleキー以外のmetadata_接頭辞キーをDELETE)
    let current_key = crate::layout::metadata_object_key(&aws.prefix, uuid, &doc.title)?;
    let list_prefix = format!("{}{uuid}/", crate::layout::data_prefix(&aws.prefix));
    for key in backend.list_keys(&aws.bucket, &list_prefix)? {
        let basename = key.rsplit('/').next().unwrap_or("");
        if basename.starts_with("metadata_") && key != current_key {
            backend.delete_object(&aws.bucket, &key)?;
        }
    }
    Ok(written)
}
