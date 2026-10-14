//! バッチmanifest(JSONL)生成・暗号化(設計5章manifest.rs・4.3)。

use crate::errors::{BackupError, BackupResult};
use crate::registry::EncryptRecord;
use std::path::Path;

/// manifestバッチヘッダの補助情報(4.3。file_count・合計sizeはrecordsから導出)。
#[derive(Debug)]
pub struct BatchMeta {
    pub batch_id: String,
    /// `YYYY-MM-DD`(JST暦日)
    pub batch_date: String,
    /// ISO8601 JST(manifest生成時刻)
    pub created_at: String,
    /// バッチ共通のrecipient指紋リスト(設定順)
    pub recipients: Vec<String>,
}

/// manifestの1レコードを`"key": value`区切り(`": "`・`", "`)の1行JSONへserializeする。
/// serde_json標準のcompact形式(`":"`・`","`)だと、平文manifestを文字列照合する
/// 検証・テストinteropとの整合が取れないため、スペース入り区切りの単一行形式とする。
fn json_line(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        serde_json::to_string(k).unwrap_or_default(),
                        serde_json::to_string(v).unwrap_or_default()
                    )
                })
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// 4.3のJSONL(バッチヘッダ+ファイルレコード)をメモリ上で組み立てる。
/// ファイルレコードはencrypt記録から`batch_id`・`recorded_at`・`manifest_path`を除いた
/// 各フィールドに`encrypted_at`を加えたもの。
pub fn build_manifest(records: &[EncryptRecord], batch_meta: &BatchMeta) -> BackupResult<Vec<u8>> {
    let total_source_size: u64 = records.iter().map(|r| r.source_size).sum();
    let total_age_size: u64 = records.iter().map(|r| r.age_size).sum();
    let header = serde_json::json!({
        "schema": 1,
        "record_type": "batch",
        "batch_id": batch_meta.batch_id,
        "batch_date": batch_meta.batch_date,
        "created_at": batch_meta.created_at,
        "file_count": records.len(),
        "total_source_size": total_source_size,
        "total_age_size": total_age_size,
        "recipients": batch_meta.recipients,
    });
    let mut out = String::new();
    out.push_str(&json_line(&header));
    out.push('\n');
    for rec in records {
        let file_record = serde_json::json!({
            "schema": 1,
            "record_type": "file",
            "source_path": rec.source_path,
            "source_name": rec.source_name,
            "source_size": rec.source_size,
            "source_mtime": rec.source_mtime,
            "source_sha256": rec.source_sha256,
            "age_path": rec.age_path,
            "age_size": rec.age_size,
            "age_sha256": rec.age_sha256,
            "recipients": rec.recipients,
            "encrypted_at": rec.recorded_at,
        });
        out.push_str(&json_line(&file_record));
        out.push('\n');
    }
    Ok(out.into_bytes())
}

/// `crypto::encrypt_bytes`でdataを暗号化し`manifest.jsonl.age`として配置する。
/// 戻り値はage_sha256。
pub fn write_manifest(
    dest: &Path,
    data: &[u8],
    public_keys: &[std::path::PathBuf],
) -> BackupResult<String> {
    crate::crypto::encrypt_bytes(data, dest, public_keys).map_err(|e| {
        BackupError::Process(format!(
            "manifestの暗号化に失敗 ({}): {e}",
            dest.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, source_size: u64, age_size: u64) -> EncryptRecord {
        EncryptRecord {
            schema: 1,
            record_type: "encrypt".into(),
            recorded_at: "2026-10-08T10:00:00+09:00".into(),
            batch_id: "20261008-abcdef01".into(),
            batch_date: "2026-10-08".into(),
            source_path: format!("/data/{name}"),
            source_name: name.into(),
            source_size,
            source_mtime: "2026-10-01T09:00:00+09:00".into(),
            source_sha256: "a".repeat(64),
            age_path: format!("2026-10-08/{name}.age"),
            age_size,
            age_sha256: "b".repeat(64),
            recipients: vec!["SHA256:testfingerprint".into()],
            manifest_path: "2026-10-08/manifest.jsonl.age".into(),
        }
    }

    #[test]
    fn build_manifest_layout() {
        let records = vec![record("a.zip", 100, 140), record("b.7z", 200, 260)];
        let meta = BatchMeta {
            batch_id: "20261008-abcdef01".into(),
            batch_date: "2026-10-08".into(),
            created_at: "2026-10-08T12:00:00+09:00".into(),
            recipients: vec!["SHA256:testfingerprint".into()],
        };

        let data = build_manifest(&records, &meta).expect("manifest組み立て");
        let text = String::from_utf8(data).expect("manifestはUTF-8 JSONL");
        let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.len(), 3, "バッチヘッダ1行+ファイルレコード2行");

        let header: serde_json::Value = serde_json::from_str(lines[0]).expect("header JSON");
        assert_eq!(header["record_type"], "batch");
        assert_eq!(header["schema"], 1);
        assert_eq!(header["batch_id"], "20261008-abcdef01");
        assert_eq!(header["batch_date"], "2026-10-08");
        assert_eq!(header["created_at"], "2026-10-08T12:00:00+09:00");
        assert_eq!(header["file_count"], 2);
        assert_eq!(header["total_source_size"], 300);
        assert_eq!(header["total_age_size"], 400);
        assert_eq!(header["recipients"][0], "SHA256:testfingerprint");

        for (i, line) in lines[1..].iter().enumerate() {
            let fr: serde_json::Value = serde_json::from_str(line).expect("file record JSON");
            assert_eq!(fr["record_type"], "file");
            assert_eq!(fr["source_name"], records[i].source_name);
            assert_eq!(fr["source_sha256"], records[i].source_sha256);
            assert_eq!(fr["age_sha256"], records[i].age_sha256);
            assert!(fr.get("batch_id").is_none(), "fileレコードにbatch_idは含めない");
            assert!(fr.get("manifest_path").is_none(), "fileレコードにmanifest_pathは含めない");
            assert!(fr.get("recorded_at").is_none(), "fileレコードにrecorded_atは含めない");
            assert!(fr.get("encrypted_at").is_some(), "fileレコードはencrypted_atを持つ");
        }
    }
}
