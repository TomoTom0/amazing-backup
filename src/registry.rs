//! registry.jsonl追記・読込・reduce(設計5章registry.rs・4.1)。

use crate::errors::{BackupError, BackupResult};
use std::collections::HashMap;
use std::path::Path;

/// `record_type: "encrypt"`のレコード(4.1)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncryptRecord {
    pub schema: i64,
    pub record_type: String,
    /// ISO8601 JST `+09:00`付き
    pub recorded_at: String,
    pub batch_id: String,
    /// `YYYY-MM-DD`(JST暦日)
    pub batch_date: String,
    /// realpath絶対path(記録表現のまま保持)
    pub source_path: String,
    /// 出力名(basenameまたはname override後・拡張子付き)
    pub source_name: String,
    pub source_size: u64,
    /// `YYYY-MM-DDTHH:MM:SS+09:00`に正規化(4.1注記)
    pub source_mtime: String,
    pub source_sha256: String,
    /// 指定out基準の`<date>/<name>.age`を指定表現のまま記録
    pub age_path: String,
    pub age_size: u64,
    pub age_sha256: String,
    /// recipient指紋列(設定順)
    pub recipients: Vec<String>,
    pub manifest_path: String,
}

/// `record_type: "upload"`のレコード(4.1)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadRecord {
    pub schema: i64,
    pub record_type: String,
    pub recorded_at: String,
    pub batch_id: String,
    pub age_path: String,
    /// `<batch_date>/<source_name>.age`
    pub s3_key: String,
    /// upload先の実効bucket(2.3のCLI一時上書き適用後の値。pending列挙・skip判定は
    /// この値と当該実行の実効bucketの一致で照合。欠損時のdefault空文字は追加前の
    /// 既存記録であり正本bucket相当として解釈する。4.1)
    #[serde(default)]
    pub s3_bucket: String,
    /// 同じく実効region(記録用。判定には使わない)
    #[serde(default)]
    pub s3_region: String,
    pub s3_size: i64,
    /// base64(S3側実測値をそのまま記録)
    pub s3_checksum_sha256: String,
    /// `FULL_OBJECT`/`COMPOSITE`等
    pub s3_checksum_type: String,
    pub uploaded_at: String,
}

/// `record_type: "manifest_upload"`のレコード(4.1)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestUploadRecord {
    pub schema: i64,
    pub record_type: String,
    pub recorded_at: String,
    pub batch_id: String,
    pub batch_date: String,
    pub manifest_path: String,
    /// `<batch_date>/manifest.jsonl.age`
    pub s3_key: String,
    /// upload先の実効bucket(2.3のCLI一時上書き適用後の値。欠損時のdefault空文字は
    /// 追加前の既存記録であり正本bucket相当として解釈する。4.1)
    #[serde(default)]
    pub s3_bucket: String,
    /// 同じく実効region(記録用。判定には使わない)
    #[serde(default)]
    pub s3_region: String,
    pub s3_size: i64,
    pub s3_checksum_sha256: String,
    pub s3_checksum_type: String,
    pub uploaded_at: String,
}

/// `record_type: "file_write"`のレコード(TASK-10設計7.3・新経路のencrypt相当)。
/// 旧`encrypt`eventとは種別分離されており、旧upload経路の処理対象にならない
/// (Phase A干渉防止。旧経路はTASK-11 Phase Bで削除済み)。
/// 索引のfile entry源(identity単位・layout情報と実keyを含む)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileWriteRecord {
    pub schema: i64,
    pub record_type: String,
    pub recorded_at: String,
    pub batch_id: String,
    /// `YYYY-MM-DD`(JST暦日・runの日付)
    pub batch_date: String,
    pub source_path: String,
    pub source_name: String,
    pub source_size: u64,
    pub source_mtime: String,
    pub source_sha256: String,
    pub age_path: String,
    pub age_size: u64,
    pub age_sha256: String,
    pub recipients: Vec<String>,
    /// TASK-10 layout: 分類解決結果と衝突hash・実key(規約導出でなく記録値が正本)
    pub category: String,
    /// 正規化済みrel_path(空=なし)
    pub rel_path: String,
    /// sha256(realpath)先頭8hex(identity)
    pub identity_hash: String,
    /// folder名(nameまたはname--hash8)
    pub folder: String,
    pub s3_key: String,
}

/// `record_type: "file_upload"`のレコード(TASK-10設計7.3・8.1)。
/// 索引versions[]の供給源(version_idはS3Backend::put応答)。自足的な版情報を持つ。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileUploadRecord {
    pub schema: i64,
    pub record_type: String,
    pub recorded_at: String,
    pub batch_id: String,
    /// 版の暗号化時刻(ISO8601 JST。索引versions[].version_ts)
    pub version_ts: String,
    pub name: String,
    pub category: String,
    pub rel_path: String,
    pub identity_hash: String,
    pub s3_key: String,
    pub s3_bucket: String,
    pub s3_region: String,
    /// S3応答のVersionId(versioning前提)
    pub version_id: String,
    pub s3_size: i64,
    pub s3_checksum_sha256: String,
    pub s3_checksum_type: String,
    pub source_sha256: String,
    pub age_sha256: String,
    pub source_size: u64,
    pub age_size: u64,
    pub uploaded_at: String,
}

/// `record_type: "manifest"`のレコード(TASK-10設計7.3・新配置のrun manifest upload)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestV2Record {
    pub schema: i64,
    pub record_type: String,
    pub recorded_at: String,
    pub batch_id: String,
    pub s3_key: String,
    pub s3_bucket: String,
    pub s3_region: String,
    pub version_id: String,
    pub s3_size: i64,
    pub s3_checksum_sha256: String,
    pub s3_checksum_type: String,
    pub uploaded_at: String,
}

/// `record_type: "index_upload"`のレコード(TASK-10設計2.2・完全性突合と冪等skip用)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexUploadRecord {
    pub schema: i64,
    pub record_type: String,
    pub recorded_at: String,
    pub run_id: String,
    pub s3_key: String,
    pub s3_bucket: String,
    pub version_id: String,
    pub s3_size: i64,
    pub s3_checksum_sha256: String,
    pub s3_checksum_type: String,
    pub uploaded_at: String,
}

/// `record_type: "pin_write"`のレコード(TASK-10設計1.3・3.2。索引pins[]の供給源)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinWriteRecord {
    pub schema: i64,
    pub record_type: String,
    pub recorded_at: String,
    pub name: String,
    pub category: String,
    pub rel_path: String,
    pub folder: String,
    pub pin: String,
    /// pin objectの実key
    pub pin_s3_key: String,
    /// 写し元のfile object keyと版
    pub source_s3_key: String,
    pub source_version_id: String,
    /// 写し元版の日付(YYYY-MM-DD)
    pub date: String,
    pub source_sha256: String,
    pub age_sha256: String,
    pub source_size: u64,
    pub s3_bucket: String,
    pub s3_size: i64,
    pub s3_checksum_sha256: String,
    pub uploaded_at: String,
}

/// `record_type: "pin_remove"`のレコード(TASK-10設計3.2。pin rmの実行記録)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinRemoveRecord {
    pub schema: i64,
    pub record_type: String,
    pub recorded_at: String,
    pub name: String,
    pub pin: String,
    pub pin_s3_key: String,
    pub s3_bucket: String,
}

/// `record_type: "prune"`のレコード(TASK-10設計3.2。manage prune --applyの実行記録)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PruneRecord {
    pub schema: i64,
    pub record_type: String,
    pub recorded_at: String,
    pub category: String,
    pub keep: u32,
    /// 削除した版(対象計算の正本記録)
    pub deleted: Vec<PruneDeleted>,
}

/// prune実行の削除対象1件。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PruneDeleted {
    pub s3_key: String,
    pub version_id: String,
}

/// 読込後の既知レコード(未知`record_type`は警告のうえ無視される)。
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Encrypt(EncryptRecord),
    Upload(UploadRecord),
    ManifestUpload(ManifestUploadRecord),
    FileWrite(FileWriteRecord),
    FileUpload(FileUploadRecord),
    ManifestV2(ManifestV2Record),
    IndexUpload(IndexUploadRecord),
    PinWrite(PinWriteRecord),
    PinRemove(PinRemoveRecord),
    Prune(PruneRecord),
}

/// reduce後のregistry状態(4.1 reduce規則 + TASK-10新event)。
/// encryptは`age_path`キーの後勝ち(挿入順保持)・upload状態は`age_path`/`manifest_path`ごとの最新。
/// TASK-10新event: file_writeはidentity_hashキーの後勝ち・file_uploadは全件保持
/// (各記録が1版に対応)・pin_write/pin_removeはpin_s3_keyキーの後勝ち。
#[derive(Debug, Clone, Default)]
pub struct Reduced {
    /// reduce後のencrypt記録(age_path後勝ち・挿入順保持)
    pub encrypts: Vec<EncryptRecord>,
    /// age_pathごとの最新upload記録
    pub uploads: HashMap<String, UploadRecord>,
    /// manifest_pathごとの最新manifest_upload記録
    pub manifest_uploads: HashMap<String, ManifestUploadRecord>,
    /// identity_hashごとの最新file_write記録(挿入順保持)
    pub file_writes: Vec<FileWriteRecord>,
    /// file_upload記録の全件(記録順=時系列。索引versions[]の源)
    pub file_uploads: Vec<FileUploadRecord>,
    /// s3_keyごとの最新manifest記録(新配置)
    pub manifest_v2: HashMap<String, ManifestV2Record>,
    /// 最新のindex_upload記録
    pub index_upload: Option<IndexUploadRecord>,
    /// pin_s3_keyごとの最新pin_write記録
    pub pin_writes: HashMap<String, PinWriteRecord>,
    /// pin_s3_keyごとの最新pin_remove記録(存在すればpinは削除済み)
    pub pin_removes: HashMap<String, PinRemoveRecord>,
    /// prune実行記録(全件)
    pub prunes: Vec<PruneRecord>,
}

/// 1行JSON + `\n`を単一write、flush、fsync(親dirも初回作成)(4.1)。
pub fn append_record<T: serde::Serialize>(path: &Path, record: &T) -> BackupResult<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                BackupError::Process(format!("registry親dir作成失敗 ({}): {e}", parent.display()))
            })?;
        }
    }
    let mut line = serde_json::to_string(record)
        .map_err(|e| BackupError::Internal(format!("registry recordのserializeに失敗: {e}")))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|e| {
            BackupError::Process(format!("registryを開けない ({}): {e}", path.display()))
        })?;
    use std::io::Write as _;
    file.write_all(line.as_bytes())
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|e| {
            BackupError::Process(format!("registryへの書込に失敗 ({}): {e}", path.display()))
        })?;
    Ok(())
}

/// 全行parse(4.1注記)。不正JSON行は行番号付きで`Process`エラー。
/// 改行終端でない最終行はtorn writeとして警告のうえ除外。
/// 未知`record_type`の行は警告のうえ無視する。ファイル不在(NotFound)のみ空として読み、
/// 他のread error(directory指定・権限不足等)はProcessエラーとして伝播する。
pub fn load_registry(path: &Path) -> BackupResult<Vec<Record>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new()); // 初回実行(ファイル不在)のみ空として読む
        }
        Err(e) => {
            return Err(BackupError::Process(format!(
                "registryを読めない ({}) : {e}",
                path.display()
            )));
        }
    };
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8(bytes).map_err(|e| {
        BackupError::Process(format!("registryがUTF-8でない ({}): {e}", path.display()))
    })?;
    // 改行終端でない最終行はtorn writeとして除外(4.1注記)
    let (body, torn_tail) = if text.ends_with('\n') {
        (text.as_str(), None)
    } else {
        match text.rfind('\n') {
            Some(pos) => (&text[..pos], Some(&text[pos + 1..])),
            None => ("", Some(text.as_str())),
        }
    };
    if let Some(tail) = torn_tail {
        eprintln!(
            "WARNING: registryの最終行が改行終端していないためtorn writeとして除外します ({})",
            path.display()
        );
        let _ = tail;
    }
    let mut records = Vec::new();
    for (idx, line) in body.split('\n').enumerate() {
        let lineno = idx + 1;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            BackupError::Process(format!(
                "registryの{lineno}行目が不正JSON ({}) : {e}",
                path.display()
            ))
        })?;
        let record_type = value
            .get("record_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match record_type {
            "encrypt" => {
                let rec: EncryptRecord = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のencrypt記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::Encrypt(rec));
            }
            "upload" => {
                let rec: UploadRecord = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のupload記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::Upload(rec));
            }
            "manifest_upload" => {
                let rec: ManifestUploadRecord = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のmanifest_upload記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::ManifestUpload(rec));
            }
            "file_write" => {
                let rec: FileWriteRecord = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のfile_write記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::FileWrite(rec));
            }
            "file_upload" => {
                let rec: FileUploadRecord = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のfile_upload記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::FileUpload(rec));
            }
            "manifest" => {
                let rec: ManifestV2Record = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のmanifest記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::ManifestV2(rec));
            }
            "index_upload" => {
                let rec: IndexUploadRecord = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のindex_upload記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::IndexUpload(rec));
            }
            "pin_write" => {
                let rec: PinWriteRecord = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のpin_write記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::PinWrite(rec));
            }
            "pin_remove" => {
                let rec: PinRemoveRecord = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のpin_remove記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::PinRemove(rec));
            }
            "prune" => {
                let rec: PruneRecord = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "registryの{lineno}行目のprune記録がschema違反 ({}): {e}",
                        path.display()
                    ))
                })?;
                records.push(Record::Prune(rec));
            }
            other => {
                eprintln!(
                    "WARNING: registryの{lineno}行目は未知のrecord_type({other})のため無視します ({})",
                    path.display()
                );
            }
        }
    }
    Ok(records)
}

/// reduce規則の適用(4.1 + TASK-10新eventのreduce規則)。
pub fn reduce_records(records: &[Record]) -> Reduced {
    let mut reduced = Reduced::default();
    let mut encrypt_index: HashMap<String, usize> = HashMap::new();
    let mut file_write_index: HashMap<String, usize> = HashMap::new();
    for record in records {
        match record {
            Record::Encrypt(e) => match encrypt_index.get(&e.age_path) {
                Some(&i) => reduced.encrypts[i] = e.clone(),
                None => {
                    encrypt_index.insert(e.age_path.clone(), reduced.encrypts.len());
                    reduced.encrypts.push(e.clone());
                }
            },
            Record::Upload(u) => {
                reduced.uploads.insert(u.age_path.clone(), u.clone());
            }
            Record::ManifestUpload(m) => {
                reduced
                    .manifest_uploads
                    .insert(m.manifest_path.clone(), m.clone());
            }
            Record::FileWrite(w) => match file_write_index.get(&w.identity_hash) {
                Some(&i) => reduced.file_writes[i] = w.clone(),
                None => {
                    file_write_index.insert(w.identity_hash.clone(), reduced.file_writes.len());
                    reduced.file_writes.push(w.clone());
                }
            },
            Record::FileUpload(u) => {
                reduced.file_uploads.push(u.clone());
            }
            Record::ManifestV2(m) => {
                reduced.manifest_v2.insert(m.s3_key.clone(), m.clone());
            }
            Record::IndexUpload(i) => {
                reduced.index_upload = Some(i.clone());
            }
            Record::PinWrite(w) => {
                reduced.pin_writes.insert(w.pin_s3_key.clone(), w.clone());
            }
            Record::PinRemove(r) => {
                reduced.pin_removes.insert(r.pin_s3_key.clone(), r.clone());
            }
            Record::Prune(p) => {
                reduced.prunes.push(p.clone());
            }
        }
    }
    reduced
}

/// encrypt skip判定用の一致検索(2.2-1)。mtime比較は4.1注記の正規化文字列同士の厳密比較。
pub fn find_match<'a>(
    reduced: &'a Reduced,
    source_path: &Path,
    size: u64,
    mtime: &str,
) -> Option<&'a EncryptRecord> {
    reduced.encrypts.iter().find(|rec| {
        Path::new(&rec.source_path) == source_path
            && rec.source_size == size
            && rec.source_mtime == mtime
    })
}

/// 記録のs3_bucketと当該実行の実効bucketの一致判定(4.1)。
/// 記録の`s3_bucket`と当該実行の実効bucketの一致判定(2.3-2・4.1)。
/// `s3_bucket`欠損(フィールド追加前の既存記録)は正本bucket相当として解釈し、
/// 実効bucketが正本と一致する実行でのみ一致扱いとする。
/// H-1(TASK-11 Phase Bレビュー): backup push skip判定・索引versions構築・
/// 未書込namespace判定でも同一規則を使う(namespace分離はbucket単位)。
/// M-R1(TASK-11 Phase B再レビュー): 索引pins[]の構築・復元(pin_write採用・
/// pin_remove照合)でも同一規則を使う。
pub(crate) fn record_bucket_matched(record_bucket: &str, effective: &str, canonical: &str) -> bool {
    if record_bucket.is_empty() {
        // 欠損は正本bucket相当(4.1互換): 実効bucketが正本と一致する実行でのみ成功済み扱い
        effective == canonical
    } else {
        record_bucket == effective
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::BackupError;
    use crate::testutil::{TempDir, write_file};

    fn encrypt_rec(age_path: &str, source_path: &str, batch_date: &str) -> EncryptRecord {
        EncryptRecord {
            schema: 1,
            record_type: "encrypt".into(),
            recorded_at: format!("{batch_date}T10:00:00+09:00"),
            batch_id: format!("{}-abcdef01", batch_date.replace('-', "")),
            batch_date: batch_date.into(),
            source_path: source_path.into(),
            source_name: Path::new(source_path)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string(),
            source_size: 100,
            source_mtime: "2026-10-01T09:00:00+09:00".into(),
            source_sha256: "a".repeat(64),
            age_path: age_path.into(),
            age_size: 140,
            age_sha256: "b".repeat(64),
            recipients: vec!["SHA256:testfingerprint".into()],
            manifest_path: format!("{}/manifest.jsonl.age", batch_date),
        }
    }

    fn upload_rec(age_path: &str, s3_key: &str, checksum: &str) -> UploadRecord {
        UploadRecord {
            schema: 1,
            record_type: "upload".into(),
            recorded_at: "2026-10-02T11:00:00+09:00".into(),
            batch_id: "20261001-abcdef01".into(),
            age_path: age_path.into(),
            s3_key: s3_key.into(),
            s3_bucket: "test-bucket".into(),
            s3_region: "ap-northeast-1".into(),
            s3_size: 140,
            s3_checksum_sha256: checksum.into(),
            s3_checksum_type: "FULL_OBJECT".into(),
            uploaded_at: "2026-10-02T11:00:01+09:00".into(),
        }
    }

    fn manifest_upload_rec(manifest_path: &str, s3_key: &str) -> ManifestUploadRecord {
        ManifestUploadRecord {
            schema: 1,
            record_type: "manifest_upload".into(),
            recorded_at: "2026-10-02T12:00:00+09:00".into(),
            batch_id: "20261001-abcdef01".into(),
            batch_date: "2026-10-01".into(),
            manifest_path: manifest_path.into(),
            s3_key: s3_key.into(),
            s3_bucket: "test-bucket".into(),
            s3_region: "ap-northeast-1".into(),
            s3_size: 200,
            s3_checksum_sha256: "m".repeat(43),
            s3_checksum_type: "FULL_OBJECT".into(),
            uploaded_at: "2026-10-02T12:00:01+09:00".into(),
        }
    }

    #[test]
    fn append_and_load_roundtrip() {
        let tmp = TempDir::new("reg-rt");
        let reg = tmp.path().join("registry.jsonl");

        // ファイル不在は空として読める(初回encrypt実行のため)
        assert!(load_registry(&reg).expect("不在registryは空").is_empty());

        append_record(
            &reg,
            &encrypt_rec("2026-10-01/a.zip.age", "/data/a.zip", "2026-10-01"),
        )
        .expect("append 1");
        append_record(
            &reg,
            &upload_rec(
                "2026-10-01/a.zip.age",
                "2026-10-01/a.zip.age",
                "checksumvalue",
            ),
        )
        .expect("append 2");
        let records = load_registry(&reg).expect("読込");
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0],
            Record::Encrypt(encrypt_rec(
                "2026-10-01/a.zip.age",
                "/data/a.zip",
                "2026-10-01"
            ))
        );
    }

    /// [covers:verify.registry-read-error-propagates]
    /// registry読込はNotFoundのみ空扱い。他のread error(directory指定による
    /// EISDIR等)はProcessエラーとして伝播する(黙って空扱いにしない)。
    #[test]
    fn read_error_other_than_not_found_propagates() {
        let tmp = TempDir::new("reg-rderr");
        // directoryをregistry pathに指定 → readがEISDIRで失敗
        let dir_path = tmp.path().join("not-a-file");
        std::fs::create_dir_all(&dir_path).expect("dir create");
        let e = load_registry(&dir_path).unwrap_err();
        match &e {
            BackupError::Process(msg) => {
                assert!(msg.contains("registryを読めない"), "message: {msg}");
            }
            other => panic!("期待した区分はProcess、実際は {other:?}"),
        }
        assert_eq!(e.exit_code(), 4);

        // NotFound(不在file)は引き続き空配列(初回実行)
        let absent = tmp.path().join("absent.jsonl");
        assert!(
            load_registry(&absent).expect("不在は空").is_empty(),
            "NotFoundのみ空扱い"
        );
    }

    /// [covers:verify.torn-write-and-bad-json-lines]
    /// 不正JSON行は行番号付きProcessError。改行終端しない最終行はtorn writeとして
    /// 除外され残り行の読込が継続する。
    #[test]
    fn torn_write_excluded_and_bad_json_detected() {
        let tmp = TempDir::new("reg-torn");
        let good = serde_json::to_string(&encrypt_rec(
            "2026-10-01/a.zip.age",
            "/data/a.zip",
            "2026-10-01",
        ))
        .unwrap();

        // 1. 改行終端しない最終行(torn write)は除外され、前行は読み込まれる
        let torn = tmp.path().join("torn.jsonl");
        let body = format!("{good}\n{{\"schema\":1,\"record_type\":\"enc"); // 末尾改行なし
        write_file(&torn, body.as_bytes());
        let records = load_registry(&torn).expect("torn writeは読込errorにしない");
        assert_eq!(records.len(), 1, "改行終端しない最終行のみ除外");

        // 2. 中途の不正JSON行(改行終端あり)は行番号付きProcessError
        let bad = tmp.path().join("bad.jsonl");
        write_file(
            &bad,
            format!("{good}\nnot-json-at-all\n{good}\n").as_bytes(),
        );
        let e = load_registry(&bad).unwrap_err();
        match &e {
            BackupError::Process(msg) => {
                assert!(msg.contains('2'), "行番号がmessageに含まれる: {msg}");
            }
            other => panic!("期待した区分はProcess、実際は {other:?}"),
        }
        assert_eq!(e.exit_code(), 4);
    }

    /// [covers:verify.unknown-record-type-skipped]
    /// 未知record_typeの行は警告のうえ無視され、既知レコードの読込・reduceが継続する。
    #[test]
    fn unknown_record_type_is_ignored() {
        let tmp = TempDir::new("reg-unknown");
        let reg = tmp.path().join("registry.jsonl");
        let e1 = serde_json::to_string(&encrypt_rec(
            "2026-10-01/a.zip.age",
            "/data/a.zip",
            "2026-10-01",
        ))
        .unwrap();
        let u1 = serde_json::to_string(&upload_rec(
            "2026-10-01/a.zip.age",
            "2026-10-01/a.zip.age",
            "checksumvalue",
        ))
        .unwrap();
        write_file(
            &reg,
            format!(
                "{e1}\n{{\"schema\":2,\"record_type\":\"future_event\",\"detail\":\"x\"}}\n{u1}\n"
            )
            .as_bytes(),
        );

        let records = load_registry(&reg).expect("未知record_typeは読込errorにしない");
        assert_eq!(records.len(), 2, "未知record_type行は結果から除外");
        assert!(matches!(records[0], Record::Encrypt(_)));
        assert!(matches!(records[1], Record::Upload(_)));

        let reduced = reduce_records(&records);
        assert_eq!(reduced.encrypts.len(), 1);
        assert!(reduced.uploads.contains_key("2026-10-01/a.zip.age"));
    }

    /// [covers:encrypt.reencrypt-on-source-change]
    /// reduce規則: 同一age_pathのencrypt記録は後勝ち(新記録が有効・旧記録は参照されない)。
    #[test]
    fn reduce_encrypt_latest_wins() {
        let mut old = encrypt_rec("2026-10-01/a.zip.age", "/data/a.zip", "2026-10-01");
        old.age_sha256 = "o".repeat(64);
        old.recorded_at = "2026-10-01T10:00:00+09:00".into();
        let mut new = old.clone();
        new.age_sha256 = "n".repeat(64);
        new.recorded_at = "2026-10-01T11:00:00+09:00".into();
        new.source_size = 999;

        let records = vec![Record::Encrypt(old), Record::Encrypt(new)];
        let reduced = reduce_records(&records);
        assert_eq!(reduced.encrypts.len(), 1, "同一age_pathは1件にreduce");
        assert_eq!(reduced.encrypts[0].age_sha256, "n".repeat(64));
        assert_eq!(reduced.encrypts[0].source_size, 999);

        // upload/manifest_uploadもそれぞれ最新勝ち
        let mut u_old = upload_rec("p", "k", "old");
        u_old.s3_size = 1;
        let mut u_new = upload_rec("p", "k", "new");
        u_new.s3_size = 2;
        let reduced = reduce_records(&[
            Record::Upload(u_old),
            Record::Upload(u_new),
            Record::ManifestUpload(manifest_upload_rec("m", "mk")),
        ]);
        assert_eq!(reduced.uploads["p"].s3_checksum_sha256, "new");
        assert!(reduced.manifest_uploads.contains_key("m"));
    }

    /// s3_bucket/s3_regionフィールドを持たない既存記録(追加前の記録)は
    /// `#[serde(default)]`で読み込め、s3_bucket空文字=正本bucket相当扱いとなる(4.1互換)。
    /// (bucket一致規則自体の結合test検証はcli.profile-namespace-isolated-by-bucket:
    /// 旧upload経路のpending_uploads_for_bucket/upload_candidatesはPhase Bで
    /// デッドコードとして削除済み)
    #[test]
    fn upload_record_without_bucket_fields_deserializes() {
        let tmp = TempDir::new("reg-oldrec");
        let reg = tmp.path().join("registry.jsonl");
        write_file(
            &reg,
            format!(
                "{{\"schema\":1,\"record_type\":\"upload\",\"recorded_at\":\"2026-10-02T11:00:00+09:00\",\"batch_id\":\"20261001-abcdef01\",\"age_path\":\"2026-10-01/a.zip.age\",\"s3_key\":\"2026-10-01/a.zip.age\",\"s3_size\":140,\"s3_checksum_sha256\":\"c\",\"s3_checksum_type\":\"FULL_OBJECT\",\"uploaded_at\":\"2026-10-02T11:00:01+09:00\"}}\n"
            )
            .as_bytes(),
        );
        let records = load_registry(&reg).expect("欠損フィールド記録も読み込める");
        let Record::Upload(u) = &records[0] else {
            panic!("upload記録: {records:?}");
        };
        assert_eq!(
            u.s3_bucket, "",
            "欠損はdefault空文字(正本bucket相当として解釈)"
        );
        assert_eq!(u.s3_region, "");
    }
}
