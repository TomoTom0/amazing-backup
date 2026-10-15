//! TASK-10索引(設計2章・tmp/tasks/tm-10/design_s3_storage.md)。
//! 索引JSONLの構築(registry reduceから・冪等)・暗号化(age)・S3配置(hot=STANDARD)・
//! 読込(災害時の逆引き正本)。schema詳細は条件書 tests/design/index.toml。

use crate::crypto;
use crate::errors::{BackupError, BackupResult};
use crate::registry::{self, Reduced};
use crate::s3::S3Backend;
use std::path::{Path, PathBuf};

/// 索引schema版数
pub const INDEX_SCHEMA: i64 = 1;

/// 索引header(1行目・`record_type: "index_header"`)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexHeader {
    pub schema: i64,
    pub record_type: String,
    /// ISO8601 JST
    pub updated_at: String,
    /// 最終更新runのbatch_id
    pub run_id: String,
    pub tool_version: String,
    pub entries: i64,
}

/// 索引file entry(2行目以降・`record_type: "file"`)。identity(=realpath由来)単位。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexFileEntry {
    pub schema: i64,
    pub record_type: String,
    pub name: String,
    pub category: String,
    /// 正規化済みrel_path(空=なし)
    pub rel_path: String,
    /// 元の絶対path(記録表現のまま)
    pub source_path: String,
    /// sha256(realpath)先頭8hex
    pub identity_hash: String,
    /// folder名(nameまたはname--hash8。pin key生成に使う。旧索引は空)
    #[serde(default)]
    pub folder: String,
    /// date降順(先頭=最新版)
    pub versions: Vec<IndexVersion>,
    /// ピン留め版(write-once別key)
    pub pins: Vec<IndexPin>,
}

/// 索引versions[]の1件(設計2.1。供給源はregistryのfile_upload記録)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexVersion {
    /// `YYYY-MM-DD`(JST暦日・表示と--version filter用)
    pub date: String,
    /// 版の暗号化時刻(ISO8601 JST・同一日内複数版の一意化)
    pub version_ts: String,
    pub batch_id: String,
    /// 実key(規約導出でなく記録値)
    pub s3_key: String,
    /// S3 VersionId。旧索引・fixture編集で欠損する場合は空(現行version取得へfall back)
    #[serde(default)]
    pub version_id: String,
    pub source_sha256: String,
    pub age_sha256: String,
    pub source_size: u64,
    pub age_size: u64,
}

/// 索引pins[]の1件。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexPin {
    pub pin: String,
    pub s3_key: String,
    /// ISO8601 JST
    pub created_at: String,
    /// 写し元版の日付
    pub date: String,
    pub source_sha256: String,
    pub age_sha256: String,
    pub source_size: u64,
}

/// registry reduce結果から索引entriesを全再構築する(冪等・設計2.2)。
/// - entryはfile_write(identity_hash単位・後勝ち)から組む
/// - versionsはidentity_hashで突き合わせたfile_upload記録(記録順=昇順 → date降順へ反転。
///   分類変更でs3_keyが変わったidentityも版履歴が続く)
/// - pinsはpin_writeから(pin_remove済みのpin_s3_keyは除外)。folder+category+rel_path+
///   nameで該当entryへ接続する
pub fn build_entries(reduced: &Reduced) -> Vec<IndexFileEntry> {
    // prune --apply済みの版は索引へ載せない(H-1: 再構築で幽霊版が復活しないよう
    // 削除記録(PruneDeleted)集合で一律除外する)
    let deleted: std::collections::BTreeSet<(String, String)> = reduced
        .prunes
        .iter()
        .flat_map(|p| p.deleted.iter())
        .map(|d| (d.s3_key.clone(), d.version_id.clone()))
        .collect();
    let mut entries: Vec<IndexFileEntry> = Vec::new();
    let mut folders: Vec<String> = Vec::new();
    for w in &reduced.file_writes {
        entries.push(IndexFileEntry {
            schema: INDEX_SCHEMA,
            record_type: "file".into(),
            name: w.source_name.clone(),
            category: w.category.clone(),
            rel_path: w.rel_path.clone(),
            source_path: w.source_path.clone(),
            identity_hash: w.identity_hash.clone(),
            folder: w.folder.clone(),
            versions: Vec::new(),
            pins: Vec::new(),
        });
        folders.push(w.folder.clone());
    }
    // versions: file_uploadをidentity_hashでentryへ接続(記録順昇順→反転でdate降順)。
    // version_tsの時刻源は暗号化時刻=file_write記録のrecorded_atとする(TASK-11 Phase B。
    // upload記録の時刻とは独立)。file_write↔file_uploadはregistry上でid直結しないため、
    // 同一(identity+batch)の組内で記録順に対応付ける(組内のwrite数を超えるuploadは
    // 旧記録・不正dataとしてfile_upload記録のversion_tsへfall back)
    let mut write_ts_by_group: std::collections::HashMap<(String, String), Vec<String>> =
        std::collections::HashMap::new();
    for w in &reduced.file_writes {
        write_ts_by_group
            .entry((w.identity_hash.clone(), w.batch_id.clone()))
            .or_default()
            .push(w.recorded_at.clone());
    }
    let mut upload_ordinal: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for (entry, folder) in entries.iter_mut().zip(&folders) {
        let mut versions: Vec<IndexVersion> = reduced
            .file_uploads
            .iter()
            .filter(|u| {
                u.identity_hash == entry.identity_hash
                    && !deleted.contains(&(u.s3_key.clone(), u.version_id.clone()))
            })
            .map(|u| {
                let group = (u.identity_hash.clone(), u.batch_id.clone());
                let ordinal = upload_ordinal.entry(group.clone()).or_insert(0);
                let encrypt_ts = write_ts_by_group
                    .get(&group)
                    .and_then(|ws| ws.get(*ordinal).cloned())
                    .unwrap_or_else(|| u.version_ts.clone());
                *ordinal += 1;
                IndexVersion {
                    date: encrypt_ts.get(..10).unwrap_or("").to_string(),
                    version_ts: encrypt_ts,
                    batch_id: u.batch_id.clone(),
                    s3_key: u.s3_key.clone(),
                    version_id: u.version_id.clone(),
                    source_sha256: u.source_sha256.clone(),
                    age_sha256: u.age_sha256.clone(),
                    source_size: u.source_size,
                    age_size: u.age_size,
                }
            })
            .collect();
        versions.reverse();
        entry.versions = versions;
        // pins: pin_write(未削除)をfolder位置へ接続
        for pin in reduced.pin_writes.values() {
            if reduced.pin_removes.contains_key(&pin.pin_s3_key) {
                continue;
            }
            if pin.name == entry.name
                && pin.category == entry.category
                && pin.rel_path == entry.rel_path
                && &pin.folder == folder
            {
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
    entries
}

/// 索引のJSONL平文化(header+entries)。
pub fn serialize_index(
    updated_at: &str,
    run_id: &str,
    entries: &[IndexFileEntry],
) -> BackupResult<String> {
    let header = IndexHeader {
        schema: INDEX_SCHEMA,
        record_type: "index_header".into(),
        updated_at: updated_at.to_string(),
        run_id: run_id.to_string(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        entries: entries.len() as i64,
    };
    let mut lines = Vec::with_capacity(entries.len() + 1);
    let header_line = serde_json::to_string(&header)
        .map_err(|e| BackupError::Internal(format!("索引headerのserializeに失敗: {e}")))?;
    lines.push(header_line);
    for entry in entries {
        let line = serde_json::to_string(entry)
            .map_err(|e| BackupError::Internal(format!("索引entryのserializeに失敗: {e}")))?;
        lines.push(line);
    }
    let mut body = lines.join("\n");
    body.push('\n');
    Ok(body)
}

/// 索引JSONL平文のparse(読込側)。未知record_type行は警告のうえ無視(前方互換)。
/// 1行目はindex_header必須。
pub fn parse_index(plain: &str) -> BackupResult<(IndexHeader, Vec<IndexFileEntry>)> {
    let mut header: Option<IndexHeader> = None;
    let mut entries: Vec<IndexFileEntry> = Vec::new();
    for (idx, line) in plain.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| BackupError::Process(format!("索引の{}行目が不正JSON: {e}", idx + 1)))?;
        match value.get("record_type").and_then(|v| v.as_str()) {
            Some("index_header") => {
                let h: IndexHeader = serde_json::from_value(value)
                    .map_err(|e| BackupError::Process(format!("索引headerがschema違反: {e}")))?;
                header = Some(h);
            }
            Some("file") => {
                let e: IndexFileEntry = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "索引の{}行目のfile entryがschema違反: {e}",
                        idx + 1
                    ))
                })?;
                entries.push(e);
            }
            other => {
                // 未知record_typeの警告はrecord_type文字列をそのまま表示する(L-5。
                // registry側の未知record_type警告と表示形式を揃える)
                let type_name = other.unwrap_or("record_typeなし");
                eprintln!("WARNING: 索引の{type_name}行は未知のrecord_typeのため無視します");
            }
        }
    }
    let header =
        header.ok_or_else(|| BackupError::Process("索引にindex_header行がない".to_string()))?;
    Ok((header, entries))
}

/// 索引を暗号化してS3へ配置する(hot=STANDARD固定・設計2.2)。
/// 成功時にregistryへ`index_upload`eventを記録する(冪等skip・完全性突合用)。
/// (引数はrun実行の文脈がそのまま並ぶ。6bのCLI層で呼び出し構造が固まった時点で
/// group化を検討する)
#[allow(clippy::too_many_arguments)]
pub fn upload_index(
    backend: &dyn S3Backend,
    bucket: &str,
    prefix: &str,
    recipients: &[PathBuf],
    entries: &[IndexFileEntry],
    run_id: &str,
    updated_at: &str,
    registry_path: &Path,
) -> BackupResult<String> {
    let body = serialize_index(updated_at, run_id, entries)?;
    let key = crate::layout::index_object_key(prefix);
    // 平文をdiskに置かず一時file経由でage化(既存crypto API。tmpはOS一時dir)
    let tmp = std::env::temp_dir().join(format!(
        "backup-remote-index-{}-{}.age",
        std::process::id(),
        updated_at.replace(':', "")
    ));
    let result = (|| -> BackupResult<String> {
        let age_sha256 = crypto::encrypt_bytes(body.as_bytes(), &tmp, recipients)?;
        let version_id = backend.put(bucket, &key, &tmp, "STANDARD")?;
        let record = registry::IndexUploadRecord {
            schema: 1,
            record_type: "index_upload".into(),
            recorded_at: updated_at.to_string(),
            run_id: run_id.to_string(),
            s3_key: key.clone(),
            s3_bucket: bucket.to_string(),
            version_id: version_id.clone(),
            s3_size: std::fs::metadata(&tmp)
                .map(|m| m.len() as i64)
                .unwrap_or(-1),
            s3_checksum_sha256: age_sha256,
            s3_checksum_type: "FULL_OBJECT".into(),
            uploaded_at: updated_at.to_string(),
        };
        registry::append_record(registry_path, &record)?;
        Ok(version_id)
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

/// S3から索引を取得・復号・parseする(照会系の最初の1 GET・設計2.4)。
/// 索引不在(404)・復号失敗は設定・環境エラー2として`manage index rebuild`への
/// 案内を添える(F4)。
pub fn read_index(
    backend: &dyn S3Backend,
    bucket: &str,
    prefix: &str,
    decrypt_key: &Path,
) -> BackupResult<(IndexHeader, Vec<IndexFileEntry>)> {
    let key = crate::layout::index_object_key(prefix);
    let guidance = format!(
        "索引object({key})を取得・復号できません。`backup-remote manage index rebuild`で復旧するか、aws.tomlのbucket/prefixを確認してください"
    );
    if backend.head(bucket, &key)?.is_none() {
        return Err(BackupError::Config(guidance));
    }
    let tmp = std::env::temp_dir().join(format!(
        "backup-remote-index-dl-{}-{}.age",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    let result = (|| -> BackupResult<(IndexHeader, Vec<IndexFileEntry>)> {
        // 索引GETのS3転送エラーは転送エラー5(cli.index-get-transfer-error。
        // 索引不在404・復号失敗の設定エラー2と分離する)
        backend
            .get_to_file(bucket, &key, &tmp)
            .map_err(|e| BackupError::Transfer(format!("{guidance} ({e})")))?;
        let plain = crypto::decrypt_bytes(&tmp, decrypt_key)
            .map_err(|e| BackupError::Config(format!("{guidance} ({e})")))?;
        let text = String::from_utf8(plain)
            .map_err(|e| BackupError::Config(format!("索引平文がUTF-8でない: {e}。{guidance}")))?;
        parse_index(&text)
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{FileUploadRecord, FileWriteRecord, PinRemoveRecord, PinWriteRecord};
    use crate::s3::FakeS3;
    use crate::testutil::TempDir;

    fn file_write(
        identity: &str,
        name: &str,
        category: &str,
        rel: &str,
        folder: &str,
    ) -> FileWriteRecord {
        FileWriteRecord {
            schema: 1,
            record_type: "file_write".into(),
            recorded_at: "2026-10-14T10:00:00+09:00".into(),
            batch_id: "20261014-abcdef01".into(),
            batch_date: "2026-10-14".into(),
            source_path: format!("/data/{name}"),
            source_name: name.into(),
            source_size: 100,
            source_mtime: "2026-10-14T09:00:00+09:00".into(),
            source_sha256: "a".repeat(64),
            age_path: format!("out/{name}.age"),
            age_size: 140,
            age_sha256: "b".repeat(64),
            recipients: vec!["SHA256:test".into()],
            category: category.into(),
            rel_path: rel.into(),
            identity_hash: identity.into(),
            folder: folder.into(),
            s3_key: format!("backup-remote/{category}/{rel}/{folder}/default.age"),
        }
    }

    fn file_upload(s3_key: &str, version_id: &str, ts: &str) -> FileUploadRecord {
        FileUploadRecord {
            schema: 1,
            record_type: "file_upload".into(),
            recorded_at: format!("{ts}+09:00"),
            batch_id: "20261014-abcdef01".into(),
            version_ts: format!("{ts}+09:00"),
            name: "a.zip".into(),
            category: "unfiled".into(),
            rel_path: "".into(),
            identity_hash: "abcd0001".into(),
            s3_key: s3_key.into(),
            s3_bucket: "b".into(),
            s3_region: "ap-northeast-1".into(),
            version_id: version_id.into(),
            s3_size: 140,
            s3_checksum_sha256: "c".repeat(43),
            s3_checksum_type: "FULL_OBJECT".into(),
            source_sha256: "a".repeat(64),
            age_sha256: "b".repeat(64),
            source_size: 100,
            age_size: 140,
            uploaded_at: format!("{ts}+09:00"),
        }
    }

    fn reduced_with(writes: Vec<FileWriteRecord>, uploads: Vec<FileUploadRecord>) -> Reduced {
        let records = writes
            .into_iter()
            .map(registry::Record::FileWrite)
            .chain(uploads.into_iter().map(registry::Record::FileUpload))
            .collect::<Vec<_>>();
        registry::reduce_records(&records)
    }

    /// [covers:index.schema-and-serialization]
    /// serialize→parse往復で全fieldが保たれる(header・versions降順・pins)。
    #[test]
    fn serialize_parse_roundtrip() {
        let w = file_write("abcd0001", "a.zip", "unfiled", "", "a.zip");
        let key = w.s3_key.clone();
        let reduced = reduced_with(
            vec![w],
            vec![
                file_upload(&key, "v1", "2026-10-13T10:00:00"),
                file_upload(&key, "v2", "2026-10-14T21:14:05"),
            ],
        );
        let entries = build_entries(&reduced);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.versions.len(), 2);
        assert_eq!(e.versions[0].version_id, "v2", "date降順(先頭=最新)");
        assert_eq!(e.versions[0].date, "2026-10-14");
        assert_eq!(e.versions[0].version_ts, "2026-10-14T21:14:05+09:00");

        let body = serialize_index("2026-10-14T22:00:00+09:00", "20261014-x", &entries)
            .expect("serialize");
        let (h, parsed) = parse_index(&body).expect("parse");
        assert_eq!(h.record_type, "index_header");
        assert_eq!(h.entries, 1);
        assert_eq!(parsed, entries, "往復で全fieldが保たれる");
    }

    /// [covers:index.rebuild-from-registry-per-run]
    /// registry reduceからの冪等再構築(同一入力→同一結果・pin_write/pin_remove反映)。
    #[test]
    fn rebuild_is_idempotent_and_reflects_pins() {
        let w = file_write("abcd0001", "a.zip", "unfiled", "", "a.zip");
        let key = w.s3_key.clone();
        let pin_key = "backup-remote/unfiled/a.zip/pin-1.age".to_string();
        let records = vec![
            registry::Record::FileWrite(w.clone()),
            registry::Record::FileUpload(file_upload(&key, "v1", "2026-10-14T10:00:00")),
            registry::Record::PinWrite(PinWriteRecord {
                schema: 1,
                record_type: "pin_write".into(),
                recorded_at: "2026-10-14T23:00:00+09:00".into(),
                name: "a.zip".into(),
                category: "unfiled".into(),
                rel_path: "".into(),
                folder: "a.zip".into(),
                pin: "pin-1".into(),
                pin_s3_key: pin_key.clone(),
                source_s3_key: key.clone(),
                source_version_id: "v1".into(),
                date: "2026-10-14".into(),
                source_sha256: "a".repeat(64),
                age_sha256: "b".repeat(64),
                source_size: 100,
                s3_bucket: "b".into(),
                s3_size: 140,
                s3_checksum_sha256: "c".repeat(43),
                uploaded_at: "2026-10-14T23:00:00+09:00".into(),
            }),
        ];
        let reduced = registry::reduce_records(&records);
        let entries1 = build_entries(&reduced);
        let entries2 = build_entries(&reduced);
        assert_eq!(entries1, entries2, "冪等");
        assert_eq!(entries1[0].pins.len(), 1);
        assert_eq!(entries1[0].pins[0].pin, "pin-1");

        // pin_remove後はpinsが空へ戻る
        let mut records2 = records.clone();
        records2.push(registry::Record::PinRemove(PinRemoveRecord {
            schema: 1,
            record_type: "pin_remove".into(),
            recorded_at: "2026-10-15T09:00:00+09:00".into(),
            name: "a.zip".into(),
            pin: "pin-1".into(),
            pin_s3_key: pin_key,
            s3_bucket: "b".into(),
        }));
        let reduced2 = registry::reduce_records(&records2);
        let entries3 = build_entries(&reduced2);
        assert!(entries3[0].pins.is_empty(), "pin rm後はpinsが空");
    }

    /// [covers:index.hot-storage-class-upload]
    /// 索引uploadはSTORAGE CLASS=STANDARDでindex keyへ置かれ、registryへ
    /// index_upload記録(version_id付き)が残る。
    #[test]
    fn upload_index_uses_standard_and_records_event() {
        let tmp = TempDir::new("idx-up");
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);
        let registry_path = tmp.path().join("registry.jsonl");

        // recipient鍵(encryptに必要): 実ageバイナリ不在環境はskip
        if crate::crypto::find_age().is_err() {
            eprintln!("skipping: ageバイナリが利用できない");
            return;
        }
        let keypair = crate::testutil::write_test_ed25519_pub(tmp.path());

        let entries = vec![];
        let vid = upload_index(
            &fake,
            "b",
            "backup-remote/",
            &[keypair],
            &entries,
            "20261014-x",
            "2026-10-14T22:00:00+09:00",
            &registry_path,
        )
        .expect("索引upload");
        assert!(!vid.is_empty(), "version_idを返す");
        let calls = fake.calls();
        assert!(
            calls.contains("s3 PutObject bucket=b key=backup-remote/sys/index.jsonl.age src="),
            "{calls}"
        );
        assert!(calls.contains("storage_class=STANDARD"), "{calls}");

        // registryへindex_upload記録
        let records = registry::load_registry(&registry_path).expect("registry読込");
        assert!(matches!(
            records.last(),
            Some(registry::Record::IndexUpload(_))
        ));
    }

    /// [covers:index.rebuild-from-manifests]の素(unit側): read_indexの
    /// 不在案内(404→設定エラー2+manage index rebuild案内)。
    #[test]
    fn read_index_missing_returns_config_error_with_guidance() {
        let tmp = TempDir::new("idx-miss");
        let fake = FakeS3::new(tmp.path().join("s3state"));
        let e = read_index(&fake, "b", "backup-remote/", Path::new("nokey")).unwrap_err();
        assert_eq!(e.exit_code(), 2, "索引不在は設定・環境エラー2: {e}");
        let msg = format!("{e}");
        assert!(msg.contains("manage index rebuild"), "案内を含む: {msg}");
    }

    /// parse: 未知record_type行は警告のうえ無視される(前方互換)。
    #[test]
    fn parse_skips_unknown_record_types() {
        let body = concat!(
            "{\"schema\":1,\"record_type\":\"index_header\",\"updated_at\":\"t\",\"run_id\":\"r\",\"tool_version\":\"v\",\"entries\":0}\n",
            "{\"schema\":2,\"record_type\":\"future_kind\",\"x\":1}\n"
        );
        let (h, entries) = parse_index(body).expect("未知行は無視してparse");
        assert_eq!(h.run_id, "r");
        assert!(entries.is_empty());
    }
}
