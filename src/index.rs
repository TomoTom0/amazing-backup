//! TASK-18索引(設計3.1・5.4・tmp/tasks/tm-18/design_key-uuid-versioning.md)。
//! 索引JSONLの構築(registry reduceから・冪等)・暗号化(age)・S3配置(hot=STANDARD)・
//! 読込(災害時の逆引き正本)。schema=2・実体(entity=uuid)単位entry。
//! schema詳細は条件書 tests/design/index.toml。

use crate::crypto;
use crate::errors::{BackupError, BackupResult};
use crate::registry::{self, Reduced};
use crate::s3::S3Backend;
use std::path::{Path, PathBuf};

/// 索引schema版数(TASK-18 3.1: 1→2。旧schema=1索引の読込・変換は行わない)
pub const INDEX_SCHEMA: i64 = 2;

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

/// 索引entity entry(2行目以降・`record_type: "file"`)。実体(uuid)単位。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexFileEntry {
    pub schema: i64,
    pub record_type: String,
    /// 実体UUID(v7 hyphenated)
    pub uuid: String,
    /// 実体のtitle(人間用識別子。一意性はuuidが担う)
    pub title: String,
    /// date降順(先頭=最新版)
    pub versions: Vec<IndexVersion>,
    /// ピン留め版(write-once別key)
    pub pins: Vec<IndexPin>,
    /// 所在一覧(source_path辞書順)
    pub locations: Vec<IndexLocation>,
}

/// 索引versions[]の1件(供給源はregistryのfile_upload記録・version_tsは記録自身の値)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexVersion {
    /// `YYYY-MM-DD`(JST暦日・表示と--version filter用)
    pub date: String,
    /// 版の暗号化時刻(ISO8601 JST・同一日内複数版の一意化)
    pub version_ts: String,
    pub batch_id: String,
    /// 実key(`data/{uuid}/default.age`記録値)
    pub s3_key: String,
    /// S3 VersionId。fixture編集で欠損する場合は空(現行version取得へfall back)
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

/// 索引locations[]の1件(TASK-18 3.1新設。所在=ローカル側の実在path)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexLocation {
    /// realpath絶対path(記録表現のまま)
    pub source_path: String,
    /// 分類解決結果(記録値)
    pub category: String,
    /// 正規化済み(空=なし。記録値)
    pub rel_path: String,
    /// sha256(realpath)先頭8hex(所在の突合キー)
    pub identity_hash: String,
    /// YYYY-MM-DD その所在の最終backup日
    pub last_backup: String,
}

/// location_removeによる所在除外(TASK-18 3.2): 同identity_hashのlocation_remove(lr)が
/// 存在し`lr.recorded_at >= w.recorded_at`なら当該file_writeは所在として除外する。
/// 除去後に同pathを再backupすると新file_write(時刻が後)が勝って所在が復活する。
fn location_removed(reduced: &Reduced, w: &registry::FileWriteRecord) -> bool {
    reduced
        .location_removes
        .iter()
        .any(|lr| lr.identity_hash == w.identity_hash && lr.recorded_at >= w.recorded_at)
}

/// registry reduce結果から索引entriesを全再構築する(冪等・設計3.1/5.4)。
/// - entity源: file_writes(所在単位reduce済み)をuuidでgrouping。location_removeによる
///   除外を適用した残りがlocations(並びはsource_path辞書順)
/// - versions: file_uploadsを`u.uuid == entity.uuid && bucket一致 && !prune/sweep削除`で
///   収集し、記録順の反転でdate降順へ。version_tsはfile_upload記録自身のversion_ts
/// - pins: pin_write(未削除)をuuid照合のみで接続(採用・削除済み判定ともbucket一致。
///   M-R1: 他方bucketのpin混入防止)
/// - title: entity_title導出(uuid直近file_writeのtitle)
/// - 当該bucket一致のversions[]・pins[]とも無いentityは載せない(L-R1)
pub fn build_entries(
    reduced: &Reduced,
    effective_bucket: &str,
    canonical_bucket: &str,
) -> Vec<IndexFileEntry> {
    // prune/sweep済みの版は索引へ載せない(H-1: 再構築で幽霊版が復活しないよう
    // 削除記録集合で一律除外する)
    let deleted: std::collections::BTreeSet<(String, String)> = reduced
        .prunes
        .iter()
        .flat_map(|p| p.deleted.iter())
        .chain(reduced.sweeps.iter().flat_map(|p| p.deleted.iter()))
        .map(|d| (d.s3_key.clone(), d.version_id.clone()))
        .collect();
    // uuidの出現順(冪等: file_writesはidentity単位reduce済みの挿入順)。
    // entity源はfile_writes(所在単位reduce済み)をuuidでgroupingしたものに加え、
    // 所在が全てlocation_removeで除去されたuuid(bucket一致の版が残る実体)も
    // locations空のentityとして載せる(6.2: 1所在のみのentityのrmはlocationsが空になる)
    let mut order: Vec<String> = Vec::new();
    let mut by_uuid: std::collections::BTreeMap<String, Vec<&registry::FileWriteRecord>> =
        std::collections::BTreeMap::new();
    for w in &reduced.file_writes {
        if location_removed(reduced, w) {
            continue;
        }
        if !by_uuid.contains_key(&w.uuid) {
            order.push(w.uuid.clone());
        }
        by_uuid.entry(w.uuid.clone()).or_default().push(w);
    }
    for u in &reduced.file_uploads {
        if !by_uuid.contains_key(&u.uuid)
            && registry::record_bucket_matched(&u.s3_bucket, effective_bucket, canonical_bucket)
            && !order.contains(&u.uuid)
        {
            order.push(u.uuid.clone());
        }
    }
    let mut entries: Vec<IndexFileEntry> = order
        .iter()
        .map(|uuid| {
            let writes = by_uuid.get(uuid).cloned().unwrap_or_default();
            // title: recorded_at最大(同値ならVec後方=後勝ち)。所在が全て除去された
            // entityはlocation_remove適用外のfile_write群から導出する
            let title = writes
                .iter()
                .max_by(|a, b| a.recorded_at.cmp(&b.recorded_at))
                .map(|w| w.title.clone())
                .or_else(|| registry::entity_title(reduced, uuid))
                .unwrap_or_default();
            // locations: source_path辞書順
            let mut locations: Vec<IndexLocation> = writes
                .iter()
                .map(|w| IndexLocation {
                    source_path: w.source_path.clone(),
                    category: w.category.clone(),
                    rel_path: w.rel_path.clone(),
                    identity_hash: w.identity_hash.clone(),
                    last_backup: w.recorded_at.get(..10).unwrap_or("").to_string(),
                })
                .collect();
            locations.sort_by(|a, b| a.source_path.cmp(&b.source_path));
            IndexFileEntry {
                schema: INDEX_SCHEMA,
                record_type: "file".into(),
                uuid: uuid.clone(),
                title,
                versions: Vec::new(),
                pins: Vec::new(),
                locations,
            }
        })
        .collect();
    for entry in entries.iter_mut() {
        // versions: uuid+bucket一致・削除記録除外。記録順昇順→反転でdate降順
        let mut versions: Vec<IndexVersion> = reduced
            .file_uploads
            .iter()
            .filter(|u| {
                u.uuid == entry.uuid
                    && registry::record_bucket_matched(
                        &u.s3_bucket,
                        effective_bucket,
                        canonical_bucket,
                    )
                    && !deleted.contains(&(u.s3_key.clone(), u.version_id.clone()))
            })
            .map(|u| IndexVersion {
                date: u.version_ts.get(..10).unwrap_or("").to_string(),
                version_ts: u.version_ts.clone(),
                batch_id: u.batch_id.clone(),
                s3_key: u.s3_key.clone(),
                version_id: u.version_id.clone(),
                source_sha256: u.source_sha256.clone(),
                age_sha256: u.age_sha256.clone(),
                source_size: u.source_size,
                age_size: u.age_size,
            })
            .collect();
        versions.reverse();
        entry.versions = versions;
        // pins: pin_write(未削除)をuuid照合で接続。採用・削除済み判定ともbucket一致
        // (M-R1)。pin_writes/pin_removesのmap keyは(pin_s3_key, s3_bucket)組(TASK-15)
        for pin in reduced.pin_writes.values() {
            if pin.uuid != entry.uuid {
                continue;
            }
            if !registry::record_bucket_matched(&pin.s3_bucket, effective_bucket, canonical_bucket)
            {
                continue;
            }
            if let Some(remove) = reduced
                .pin_removes
                .get(&(pin.pin_s3_key.clone(), pin.s3_bucket.clone()))
                && registry::record_bucket_matched(
                    &remove.s3_bucket,
                    effective_bucket,
                    canonical_bucket,
                )
            {
                continue;
            }
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
    // 索引の意味論は「当該namespace(bucket)で管理される実体」: 当該bucket一致の
    // versions[]もpins[]も無いentityはlist/query/diffの対象外として落とす(L-R1)
    entries.retain(|e| !e.versions.is_empty() || !e.pins.is_empty());
    // pinsの並びをs3_keyで確定する: 採用源がHashMapのiteration order(process毎に不定)
    // のため、sortしないと同一registryからの再構築でも索引内容(バイト列)が変わる
    // =冪等を満たさない
    for e in entries.iter_mut() {
        e.pins.sort_by(|a, b| a.s3_key.cmp(&b.s3_key));
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
/// 1行目はindex_header必須。parse失敗時のerror文には`manage index rebuild`による
/// 索引再構築の案内を含める(L-6。終了コードは検証エラー4のまま)。
/// TASK-18: header.schema != 2(旧schema=1索引)は設定エラー2+rebuild案内で拒否
/// (旧索引の誤読防止・設計5.4/9章)。
pub fn parse_index(plain: &str) -> BackupResult<(IndexHeader, Vec<IndexFileEntry>)> {
    let guidance = "索引の再構築は `backup-remote manage index rebuild` で行えます";
    let mut header: Option<IndexHeader> = None;
    let mut entries: Vec<IndexFileEntry> = Vec::new();
    for (idx, line) in plain.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            BackupError::Process(format!("索引の{}行目が不正JSON: {e}。{guidance}", idx + 1))
        })?;
        match value.get("record_type").and_then(|v| v.as_str()) {
            Some("index_header") => {
                let h: IndexHeader = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!("索引headerがschema違反: {e}。{guidance}"))
                })?;
                if h.schema != INDEX_SCHEMA {
                    return Err(BackupError::Config(format!(
                        "索引のschema版数が不正です(schema={}・期待={INDEX_SCHEMA})。旧形式の索引です。{guidance}",
                        h.schema
                    )));
                }
                header = Some(h);
            }
            Some("file") => {
                let e: IndexFileEntry = serde_json::from_value(value).map_err(|e| {
                    BackupError::Process(format!(
                        "索引の{}行目のfile entryがschema違反: {e}。{guidance}",
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
    let header = header
        .ok_or_else(|| BackupError::Process(format!("索引にindex_header行がない。{guidance}")))?;
    Ok((header, entries))
}

/// 索引を暗号化してS3へ配置する(hot=STANDARD固定)。
/// 成功時にregistryへ`index_upload`eventを記録する(冪等skip・完全性突合用)。
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
/// 索引不在(404)・復号失敗・schema不一致は設定・環境エラー2として
/// `manage index rebuild`への案内を添える(F4・TASK-18 schema=2)。
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
        uuid: &str,
        title: &str,
        path: &str,
        category: &str,
        rel: &str,
    ) -> FileWriteRecord {
        FileWriteRecord {
            schema: 2,
            record_type: "file_write".into(),
            recorded_at: "2026-10-14T10:00:00+09:00".into(),
            batch_id: "20261014-abcdef01".into(),
            batch_date: "2026-10-14".into(),
            uuid: uuid.into(),
            title: title.into(),
            source_path: path.into(),
            source_name: "a.zip".into(),
            source_size: 100,
            source_mtime: "2026-10-14T09:00:00+09:00".into(),
            source_sha256: "a".repeat(64),
            age_path: "out/a.zip.age".into(),
            age_size: 140,
            age_sha256: "b".repeat(64),
            recipients: vec!["SHA256:test".into()],
            category: category.into(),
            rel_path: rel.into(),
            identity_hash: crate::layout::identity_hash(std::path::Path::new(path)),
            s3_key: format!("backup-remote/data/{uuid}/default.age"),
        }
    }

    fn file_upload(uuid: &str, s3_key: &str, version_id: &str, ts: &str) -> FileUploadRecord {
        FileUploadRecord {
            schema: 2,
            record_type: "file_upload".into(),
            recorded_at: format!("{ts}+09:00"),
            batch_id: "20261014-abcdef01".into(),
            version_ts: format!("{ts}+09:00"),
            uuid: uuid.into(),
            title: "a.zip".into(),
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
    /// serialize→parse往復で全fieldが保たれる(header・entity・versions降順・pins・locations)。
    #[test]
    fn serialize_parse_roundtrip() {
        let uuid = "0192a4f0-3c1b-7a2e-9f10-112233445566";
        let w = file_write(
            uuid,
            "旅のしおり",
            "/data/sub/旅のしおり.zip",
            "unfiled",
            "sub",
        );
        let key = w.s3_key.clone();
        let reduced = reduced_with(
            vec![w],
            vec![
                file_upload(uuid, &key, "v1", "2026-10-13T10:00:00"),
                file_upload(uuid, &key, "v2", "2026-10-14T21:14:05"),
            ],
        );
        let entries = build_entries(&reduced, "b", "b");
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.uuid, uuid);
        assert_eq!(e.title, "旅のしおり");
        assert_eq!(e.locations.len(), 1);
        assert_eq!(e.locations[0].source_path, "/data/sub/旅のしおり.zip");
        assert_eq!(e.locations[0].rel_path, "sub");
        assert_eq!(e.versions.len(), 2);
        assert_eq!(e.versions[0].version_id, "v2", "date降順(先頭=最新)");
        assert_eq!(e.versions[0].date, "2026-10-14");
        assert_eq!(e.versions[0].version_ts, "2026-10-14T21:14:05+09:00");

        let body = serialize_index("2026-10-14T22:00:00+09:00", "20261014-x", &entries)
            .expect("serialize");
        let (h, parsed) = parse_index(&body).expect("parse");
        assert_eq!(h.record_type, "index_header");
        assert_eq!(h.schema, 2);
        assert_eq!(h.entries, 1);
        assert_eq!(parsed, entries, "往復で全fieldが保たれる");
    }

    /// [covers:index.entity-grouping-by-uuid]
    /// 同一uuidの所在複数はlocations(source_path辞書順)へgrouping・entityは1件。
    #[test]
    fn entities_grouped_by_uuid_with_sorted_locations() {
        let uuid = "0192a4f0-3c1b-7a2e-9f10-112233445566";
        let w1 = file_write(uuid, "t", "/data/b/a.zip", "unfiled", "b");
        let w2 = file_write(uuid, "t", "/data/a/a.zip", "unfiled", "a");
        let key = w1.s3_key.clone();
        let reduced = reduced_with(
            vec![w1, w2],
            vec![file_upload(uuid, &key, "v1", "2026-10-14T10:00:00")],
        );
        let entries = build_entries(&reduced, "b", "b");
        assert_eq!(entries.len(), 1, "同一uuidは1entityへgrouping");
        let paths: Vec<&str> = entries[0]
            .locations
            .iter()
            .map(|l| l.source_path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["/data/a/a.zip", "/data/b/a.zip"],
            "source_path辞書順"
        );
    }

    /// [covers:index.rebuild-from-registry-per-run]
    /// registry reduceからの冪等再構築(同一入力→同一結果・pin_write/pin_remove反映)。
    #[test]
    fn rebuild_is_idempotent_and_reflects_pins() {
        let uuid = "0192a4f0-3c1b-7a2e-9f10-112233445566";
        let w = file_write(uuid, "a.zip", "/data/a.zip", "unfiled", "");
        let key = w.s3_key.clone();
        let pin_key = format!("backup-remote/data/{uuid}/pin-1.age");
        let records = vec![
            registry::Record::FileWrite(w.clone()),
            registry::Record::FileUpload(file_upload(uuid, &key, "v1", "2026-10-14T10:00:00")),
            registry::Record::PinWrite(PinWriteRecord {
                schema: 2,
                record_type: "pin_write".into(),
                recorded_at: "2026-10-14T23:00:00+09:00".into(),
                uuid: uuid.into(),
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
        let entries1 = build_entries(&reduced, "b", "b");
        let entries2 = build_entries(&reduced, "b", "b");
        assert_eq!(entries1, entries2, "冪等");
        assert_eq!(entries1[0].pins.len(), 1);
        assert_eq!(entries1[0].pins[0].pin, "pin-1");

        // pin_remove後はpinsが空へ戻る
        let mut records2 = records.clone();
        records2.push(registry::Record::PinRemove(PinRemoveRecord {
            schema: 2,
            record_type: "pin_remove".into(),
            recorded_at: "2026-10-15T09:00:00+09:00".into(),
            uuid: uuid.into(),
            pin: "pin-1".into(),
            pin_s3_key: pin_key,
            s3_bucket: "b".into(),
        }));
        let reduced2 = registry::reduce_records(&records2);
        let entries3 = build_entries(&reduced2, "b", "b");
        assert!(entries3[0].pins.is_empty(), "pin rm後はpinsが空");
    }

    /// [covers:index.pins-bucket-isolated]
    /// [covers:index.entries-without-bucket-versions-excluded]
    /// pins[]の採用・pin_removeの削除済み判定ともbucket一致規則に従う(M-R1)。
    /// 当該bucketのversions[]・pins[]とも無いentityは索引へ載らない(L-R1)。
    #[test]
    fn pins_and_orphan_entries_filtered_by_recorded_bucket() {
        let uuid_a = "0192a4f0-3c1b-7a2e-9f10-112233445566";
        let uuid_b = "0192a4f0-3c1b-7a2e-9f10-112233445577";
        let wa = file_write(uuid_a, "a.zip", "/data/a.zip", "unfiled", "");
        let wb = file_write(uuid_b, "b.zip", "/data/b.zip", "unfiled", "");
        let key_a = wa.s3_key.clone();
        let key_b = wb.s3_key.clone();
        let mut up_b = file_upload(uuid_b, &key_b, "v9", "2026-10-14T10:00:00");
        up_b.s3_bucket = "other".into(); // b.zipは他方bucketのみへupload
        let pin_rec = |pin: &str, bucket: &str| PinWriteRecord {
            schema: 2,
            record_type: "pin_write".into(),
            recorded_at: "2026-10-14T23:00:00+09:00".into(),
            uuid: uuid_a.into(),
            pin: pin.into(),
            pin_s3_key: format!("backup-remote/data/{uuid_a}/{pin}.age"),
            source_s3_key: key_a.clone(),
            source_version_id: "v1".into(),
            date: "2026-10-14".into(),
            source_sha256: "a".repeat(64),
            age_sha256: "b".repeat(64),
            source_size: 100,
            s3_bucket: bucket.into(),
            s3_size: 140,
            s3_checksum_sha256: "c".repeat(43),
            uploaded_at: "2026-10-14T23:00:00+09:00".into(),
        };
        let pin_remove = |pin: &str, bucket: &str| PinRemoveRecord {
            schema: 2,
            record_type: "pin_remove".into(),
            recorded_at: "2026-10-15T09:00:00+09:00".into(),
            uuid: uuid_a.into(),
            pin: pin.into(),
            pin_s3_key: format!("backup-remote/data/{uuid_a}/{pin}.age"),
            s3_bucket: bucket.into(),
        };
        let base = || {
            vec![
                registry::Record::FileWrite(wa.clone()),
                registry::Record::FileWrite(wb.clone()),
                registry::Record::FileUpload(file_upload(
                    uuid_a,
                    &key_a,
                    "v1",
                    "2026-10-14T10:00:00",
                )),
                registry::Record::FileUpload(up_b.clone()),
                registry::Record::PinWrite(pin_rec("pin-1", "b")),
                registry::Record::PinWrite(pin_rec("pin-2", "other")),
            ]
        };
        let pin_names = |entries: &[IndexFileEntry], uuid: &str| -> Vec<String> {
            entries
                .iter()
                .find(|e| e.uuid == uuid)
                .map(|e| e.pins.iter().map(|p| p.pin.clone()).collect())
                .unwrap_or_default()
        };

        // 実効=正本=b: a.zip(版あり+pin-1)のみ載る。b.zip(他方bucketのみの版)と
        // pin-2(他方bucketのpin_write)は載らない
        let reduced = registry::reduce_records(&base());
        let entries = build_entries(&reduced, "b", "b");
        assert_eq!(entries.len(), 1, "当該bucketの版もpinも無いentityは落とす");
        assert_eq!(entries[0].uuid, uuid_a);
        assert_eq!(pin_names(&entries, uuid_a), vec!["pin-1"]);

        // 他方bucketのpin_removeではpin-1は消えない
        let mut records = base();
        records.push(registry::Record::PinRemove(pin_remove("pin-1", "other")));
        let entries = build_entries(&registry::reduce_records(&records), "b", "b");
        assert_eq!(pin_names(&entries, uuid_a), vec!["pin-1"]);

        // 正bucket(実効と一致)のpin_removeで消える(版はあるためentryは残る)
        let mut records = base();
        records.push(registry::Record::PinRemove(pin_remove("pin-1", "b")));
        let entries = build_entries(&registry::reduce_records(&records), "b", "b");
        assert_eq!(entries.len(), 1, "版があるentityはpinsが空でも残る");
        assert!(
            pin_names(&entries, uuid_a).is_empty(),
            "正bucketのpin_removeで消える"
        );

        // 実効=other(正本=b): pin-2のみ採用。a.zipは版が無いがpin-2がありentry残置、
        // b.zipは当該bucketの版が載る。旧記録(s3_bucket空)のpin_writeは正本bucket相当
        let mut records = base();
        records.push(registry::Record::PinWrite(pin_rec("pin-3", "")));
        let entries = build_entries(&registry::reduce_records(&records), "other", "b");
        assert_eq!(entries.len(), 2, "a.zip(pinsのみ)+b.zip(版あり)");
        assert_eq!(pin_names(&entries, uuid_a), vec!["pin-2"]);
        assert!(
            entries.iter().any(|e| e.uuid == uuid_b),
            "実効bucket側のb.zip版は載る"
        );
        let entries = build_entries(&registry::reduce_records(&records), "b", "b");
        assert_eq!(
            pin_names(&entries, uuid_a),
            vec!["pin-1", "pin-3"],
            "s3_bucket欠損(空文字)の旧記録は正本bucket扱いで実効=正本なら採用"
        );
    }

    /// [covers:index.swept-versions-not-revived]
    /// prune/sweep記録の版はversionsへ復活しない。
    #[test]
    fn swept_versions_not_revived() {
        let uuid = "0192a4f0-3c1b-7a2e-9f10-112233445566";
        let w = file_write(uuid, "a.zip", "/data/a.zip", "unfiled", "");
        let key = w.s3_key.clone();
        let records = vec![
            registry::Record::FileWrite(w),
            registry::Record::FileUpload(file_upload(uuid, &key, "v1", "2026-10-13T10:00:00")),
            registry::Record::FileUpload(file_upload(uuid, &key, "v2", "2026-10-14T10:00:00")),
            registry::Record::Sweep(registry::SweepRecord {
                schema: 1,
                record_type: "sweep".into(),
                recorded_at: "2999-01-01T00:00:00+09:00".into(),
                reason: "lifecycle".into(),
                deleted: vec![registry::PruneDeleted {
                    s3_key: key.clone(),
                    version_id: "v1".into(),
                }],
            }),
        ];
        let reduced = registry::reduce_records(&records);
        let entries = build_entries(&reduced, "b", "b");
        let vids: Vec<&str> = entries[0]
            .versions
            .iter()
            .map(|v| v.version_id.as_str())
            .collect();
        assert_eq!(vids, vec!["v2"], "sweep済み版は除外");
    }

    /// [covers:index.location-remove-excludes-and-revives]
    /// location_removeの所在除外(lr.recorded_at >= w.recorded_at)と再backup復活。
    #[test]
    fn location_remove_excludes_and_revives() {
        let uuid = "0192a4f0-3c1b-7a2e-9f10-112233445566";
        let w1 = file_write(uuid, "a.zip", "/data/loc1/a.zip", "unfiled", "loc1");
        let w2 = file_write(uuid, "a.zip", "/data/loc2/a.zip", "unfiled", "loc2");
        let key = w1.s3_key.clone();
        let base = || {
            vec![
                registry::Record::FileWrite(w1.clone()),
                registry::Record::FileWrite(w2.clone()),
                registry::Record::FileUpload(file_upload(uuid, &key, "v1", "2026-10-14T10:00:00")),
            ]
        };
        let lr = registry::LocationRemoveRecord {
            schema: 1,
            record_type: "location_remove".into(),
            recorded_at: "2999-01-01T00:00:00+09:00".into(),
            uuid: uuid.into(),
            identity_hash: w1.identity_hash.clone(),
            source_path: "/data/loc1/a.zip".into(),
        };
        let mut records = base();
        records.push(registry::Record::LocationRemove(lr));
        let entries = build_entries(&registry::reduce_records(&records), "b", "b");
        assert_eq!(entries[0].locations.len(), 1);
        assert_eq!(entries[0].locations[0].source_path, "/data/loc2/a.zip");

        // 除去後にrecorded_atが後の新file_writeで当該所在が復活する
        let mut revived = records;
        let mut w3 = w1.clone();
        w3.recorded_at = "3999-01-01T00:00:00+09:00".into();
        revived.push(registry::Record::FileWrite(w3));
        let entries = build_entries(&registry::reduce_records(&revived), "b", "b");
        assert_eq!(
            entries[0].locations.len(),
            2,
            "新file_writeが勝って所在復活"
        );
    }

    /// [covers:index.version-ts-from-file-write]
    /// versions[].version_tsの供給源はfile_upload記録自身のversion_ts。
    #[test]
    fn version_ts_comes_from_file_upload_record() {
        let uuid = "0192a4f0-3c1b-7a2e-9f10-112233445566";
        let w = file_write(uuid, "a.zip", "/data/a.zip", "unfiled", "");
        let key = w.s3_key.clone();
        let reduced = reduced_with(
            vec![w],
            vec![
                file_upload(uuid, &key, "v1", "2026-10-13T10:00:00"),
                file_upload(uuid, &key, "v2", "2026-10-14T21:14:05"),
            ],
        );
        let entries = build_entries(&reduced, "b", "b");
        assert_eq!(
            entries[0].versions[0].version_ts,
            "2026-10-14T21:14:05+09:00"
        );
        assert_eq!(
            entries[0].versions[1].version_ts,
            "2026-10-13T10:00:00+09:00"
        );
    }

    /// [covers:index.parse-failure-guides-rebuild]
    /// schema違反平文は検証エラー4+rebuild案内。schema=1索引は設定エラー2+rebuild案内。
    #[test]
    fn parse_failure_error_guides_index_rebuild() {
        let body = concat!(
            "{\"schema\":2,\"record_type\":\"index_header\",\"updated_at\":\"t\",\"run_id\":\"r\",\"tool_version\":\"v\",\"entries\":0}\n",
            "{\"schema\":2,\"record_type\":\"future_kind\",\"x\":1}\n"
        );
        let (h, entries) = parse_index(body).expect("未知行は無視してparse");
        assert_eq!(h.run_id, "r");
        assert!(entries.is_empty());

        // 旧schema=1索引は設定エラー2
        let v1 = concat!(
            "{\"schema\":1,\"record_type\":\"index_header\",\"updated_at\":\"t\",\"run_id\":\"r\",\"tool_version\":\"v\",\"entries\":0}\n"
        );
        let e = parse_index(v1).unwrap_err();
        assert_eq!(e.exit_code(), 2, "旧schema=1は設定エラー2: {e}");
        assert!(format!("{e}").contains("manage index rebuild"));
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
            "{\"schema\":2,\"record_type\":\"index_header\",\"updated_at\":\"t\",\"run_id\":\"r\",\"tool_version\":\"v\",\"entries\":0}\n",
            "{\"schema\":2,\"record_type\":\"future_kind\",\"x\":1}\n"
        );
        let (h, entries) = parse_index(body).expect("未知行は無視してparse");
        assert_eq!(h.run_id, "r");
        assert!(entries.is_empty());
    }
}
