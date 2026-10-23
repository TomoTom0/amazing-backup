//! TASK-18キー構成の純関数群(UUID発行・key生成・名前規則検証・分類解決)。
//! 設計書 tmp/tasks/tm-18/design_key-uuid-versioning.md 1章・5.1・条件書 tests/design/layout.toml。
//! key構造: `<prefix>data/{uuid}/default.age`(versioning)・
//! `<prefix>data/{uuid}/{pin}.age`(write-once)・`<prefix>data/{uuid}/metadata_{title}.json`(平文)・
//! 予約領域 `<prefix>sys/`(索引・manifest)。キーはpathにも内容にも依存しない発行ID(uuid)基準。

use crate::errors::{BackupError, BackupResult};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// S3 object key長上限(S3仕様。超過は対象エラー3)
pub const KEY_MAX_BYTES: usize = 1024;
/// 予約領域の第1階層名(`<prefix>sys/`。索引・manifestの配置先)。
/// category名としての使用は拒否される(名前規則 `[a-z0-9][a-z0-9-]` と合わせ、
/// 第1階層の衝突を構造的に防ぐ)
pub const RESERVE_SEGMENT: &str = "sys";
/// 実体領域の第1階層名(`<prefix>data/`)
pub const DATA_SEGMENT: &str = "data";
/// pin名として予約されている語(default.ageとの衝突防止)
pub const RESERVED_PIN_NAME: &str = "default";

/// prefix形式検証(設計1.1): 空でない・`/`終端・先頭`/`禁止・
/// 区切り`/`で結ぶ各要素は`[a-z0-9-]`のみ・空要素禁止。違反は設定エラー2。
pub fn validate_prefix(prefix: &str) -> BackupResult<()> {
    let invalid = |detail: &str| {
        BackupError::Config(format!(
            "prefixの形式が不正です({detail}): {prefix:?}(例: \"amazing-backup/\")"
        ))
    };
    if prefix.is_empty() {
        return Err(invalid("空です"));
    }
    if !prefix.ends_with('/') {
        return Err(invalid("末尾が/でありません"));
    }
    if prefix.starts_with('/') {
        return Err(invalid("先頭に/があります"));
    }
    if prefix == "/" {
        return Err(invalid("要素が空です"));
    }
    for seg in prefix.trim_end_matches('/').split('/') {
        if seg.is_empty() {
            return Err(invalid("空の階層要素があります"));
        }
        if !seg
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(invalid("階層要素に使えるのは小文字英数字と-のみです"));
        }
    }
    Ok(())
}

/// category名規則: `^[a-z0-9][a-z0-9-]{0,63}$`・予約語(sys)拒否。
/// TASK-18: [categories]定義は廃止され、categoryは所在記録値として自由化
/// (文字列検証のみ・5.15)。違反は設定エラー2。
pub fn validate_category_name(name: &str) -> BackupResult<()> {
    let invalid =
        |detail: &str| BackupError::Config(format!("category名が不正です({detail}): {name:?}"));
    if name.is_empty() {
        return Err(invalid("空です"));
    }
    if name.len() > 64 {
        return Err(invalid("64文字を超えています"));
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(invalid("先頭は小文字英数字のみです"));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(invalid("使えるのは小文字英数字と-のみです"));
    }
    if name == RESERVE_SEGMENT {
        return Err(invalid("予約領域名(sys)は使えません"));
    }
    Ok(())
}

/// pin名規則: `^[a-z0-9][a-z0-9-]{0,62}$`(全体63文字以内)・`default`予約。
/// CLI引数形式検証として使う場合の違反は使用法エラー64。
pub fn validate_pin_name(pin: &str) -> BackupResult<()> {
    let invalid = |detail: &str| BackupError::Usage(format!("pin名が不正です({detail}): {pin:?}"));
    if pin.is_empty() {
        return Err(invalid("空です"));
    }
    if pin.len() > 63 {
        return Err(invalid("63文字を超えています"));
    }
    let mut chars = pin.chars();
    let first = chars.next().expect("non-empty");
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(invalid("先頭は小文字英数字のみです"));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(invalid("使えるのは小文字英数字と-のみです"));
    }
    if pin == RESERVED_PIN_NAME {
        return Err(invalid("\'default\'は予約されています"));
    }
    Ok(())
}

/// title規則(TASK-18 5.1): 空拒否・64文字(char)超過拒否・`\n`/`\r`拒否・`/`拒否
/// (key階層の誤読とdiff除外判定の確実性のため)・制御文字拒否。違反はUsage 64
/// (validate_pin_nameと同一区分)。UTF-8(日本語等)は許容。
pub fn validate_title(title: &str) -> BackupResult<()> {
    let invalid =
        |detail: &str| BackupError::Usage(format!("titleが不正です({detail}): {title:?}"));
    if title.is_empty() {
        return Err(invalid("空です"));
    }
    if title.chars().count() > 64 {
        return Err(invalid("64文字を超えています"));
    }
    if title.contains('\n') || title.contains('\r') {
        return Err(invalid("改行を含みます"));
    }
    if title.contains('/') {
        return Err(invalid("スラッシュを含みます"));
    }
    if title.chars().any(|c| c.is_control()) {
        return Err(invalid("制御文字を含みます"));
    }
    Ok(())
}

/// rel_path正規化: 先頭`/`禁止・`./`/`../`要素と空要素の拒否・末尾`/`の除去。
/// 空文字列は「rel_pathなし」を表す正当値。
/// 違反は呼び出し側の区分(設定値は設定エラー2・対象解決由来は対象エラー3)に
/// 帰着させるため、ここではmessage付きエラーのみ返す。
pub fn normalize_rel_path(path: &str) -> BackupResult<String> {
    let invalid = |detail: &str| BackupError::Config(format!("path({path:?})が不正です: {detail}"));
    if path.starts_with('/') {
        return Err(invalid("先頭に/があります"));
    }
    // `.`/`..`要素と空要素は排除(除去)する。`a/./b/`も`a//b`も`a/b`へ正規化される
    let segments: Vec<&str> = path
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        .collect();
    if segments.is_empty() {
        return Ok(String::new());
    }
    Ok(segments.join("/"))
}

/// identity_hash: sha256(realpath文字列)先頭8hex。所在(location)の突合キー。
/// realpathはcanonicalize済み絶対pathを想定する。
pub fn identity_hash(realpath: &Path) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(realpath.to_string_lossy().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..8].to_string()
}

/// 対象の分類解決結果(所在記録値。key構成には関与しない)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub category: String,
    /// 正規化済み。空文字列=rel_pathなし
    pub rel_path: String,
}

/// files手動写像の1件(configのschemaと同一形状の純粋参照用)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMapping {
    pub category: String,
    pub path: String,
}

/// 分類解決(優先順位): ①files写像 ②auto_path_roots相対化(最初に一致したroot)
/// ③fallback(category=unfiled・rel_pathなし)。
/// source_canonicalはrealpath解決済み絶対path。source_nameはbasename(拡張子付き)。
/// 解決結果はIndexLocation・manifest file recordの所在記録値として記録される
/// (TASK-18: キーはUUID体系のため分類はキー構成に影響しない)。
pub fn classify(
    source_canonical: &Path,
    source_name: &str,
    files: &BTreeMap<String, FileMapping>,
    unfiled_category: &str,
    auto_path_roots: &[PathBuf],
) -> BackupResult<Classification> {
    if let Some(mapping) = files.get(source_name) {
        return Ok(Classification {
            category: mapping.category.clone(),
            rel_path: normalize_rel_path(&mapping.path)?,
        });
    }
    for root in auto_path_roots {
        if let Ok(rel) = source_canonical.strip_prefix(root) {
            // rel_pathは「root配下のdirectory側path」(末尾のファイル名は除く。
            // root直下のfileはrel_pathなし)
            let rel_path = rel
                .parent()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            return Ok(Classification {
                category: unfiled_category.to_string(),
                rel_path: normalize_rel_path(&rel_path)?,
            });
        }
    }
    Ok(Classification {
        category: unfiled_category.to_string(),
        rel_path: String::new(),
    })
}

/// UUIDv7の発行(TASK-18 5.1): hyphenated lowercase 36字・時刻由来の単調増加傾向。
pub fn generate_uuid() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// key長検証(S3仕様の1024 byte上限。超過は対象エラー3)。
pub fn check_key_length(key: &str) -> BackupResult<()> {
    if key.len() > KEY_MAX_BYTES {
        return Err(BackupError::Target(format!(
            "生成keyがS3のkey長上限({KEY_MAX_BYTES} byte)を超えています({} byte): {key}。prefix長またはtitle長を確認してください",
            key.len()
        )));
    }
    Ok(())
}

/// 実体領域のprefix: `{prefix}data/`。
pub fn data_prefix(prefix: &str) -> String {
    format!("{prefix}{DATA_SEGMENT}/")
}

/// 実体本体のkey: `{prefix}data/{uuid}/default.age`(versioningで版蓄積)。
pub fn data_object_key(prefix: &str, uuid: &str) -> BackupResult<String> {
    let key = format!("{}{uuid}/default.age", data_prefix(prefix));
    check_key_length(&key)?;
    Ok(key)
}

/// pin objectのkey: `{prefix}data/{uuid}/{pin}.age`(write-once)。
/// validate_pin_nameは呼び出し側で実施済み(現行どおり)。
pub fn pin_object_key(prefix: &str, uuid: &str, pin: &str) -> BackupResult<String> {
    let key = format!("{}{uuid}/{pin}.age", data_prefix(prefix));
    check_key_length(&key)?;
    Ok(key)
}

/// metadata objectのkey: `{prefix}data/{uuid}/metadata_{title}.json`(平文JSON)。
/// validate_titleは呼び出し側で実施済み。
pub fn metadata_object_key(prefix: &str, uuid: &str, title: &str) -> BackupResult<String> {
    let key = format!("{}{uuid}/metadata_{title}.json", data_prefix(prefix));
    check_key_length(&key)?;
    Ok(key)
}

/// 索引objectのkey: `{prefix}sys/index.jsonl.age`。
pub fn index_object_key(prefix: &str) -> String {
    format!("{prefix}{RESERVE_SEGMENT}/index.jsonl.age")
}

/// manifest objectのkey: `{prefix}sys/manifests/<batch_id>.jsonl.age`。
pub fn manifest_object_key(prefix: &str, batch_id: &str) -> BackupResult<String> {
    let key = format!("{prefix}{RESERVE_SEGMENT}/manifests/{batch_id}.jsonl.age");
    check_key_length(&key)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files_map(entries: &[(&str, &str, &str)]) -> BTreeMap<String, FileMapping> {
        entries
            .iter()
            .map(|(name, category, path)| {
                (
                    name.to_string(),
                    FileMapping {
                        category: category.to_string(),
                        path: path.to_string(),
                    },
                )
            })
            .collect()
    }

    /// [covers:layout.key-generation-manual-mapping]
    /// files写像が最優先(auto_path_roots配下でも写像のcategory/pathが勝つ)。
    #[test]
    fn files_mapping_takes_priority() {
        let files = files_map(&[("travel.zip", "photos", "2020-2023/travel")]);
        let roots = vec![PathBuf::from("/data")];
        let c = classify(
            Path::new("/data/2020-2023/travel/travel.zip"),
            "travel.zip",
            &files,
            "unfiled",
            &roots,
        )
        .expect("写像優先で解決");
        assert_eq!(c.category, "photos");
        assert_eq!(c.rel_path, "2020-2023/travel");
    }

    /// [covers:layout.key-generation-auto-relativize]
    /// auto_path_roots相対化: 最初に一致したrootから相対化しcategoryはunfiled。
    /// どのrootにも一致しない場合はrel_pathなし(fallback)。
    #[test]
    fn auto_roots_relativize_and_fallback() {
        let roots = vec![PathBuf::from("/data"), PathBuf::from("/data/2020-2023")];
        let c = classify(
            Path::new("/data/2020-2023/travel/travel.zip"),
            "travel.zip",
            &BTreeMap::new(),
            "unfiled",
            &roots,
        )
        .expect("相対化");
        assert_eq!(c.category, "unfiled");
        assert_eq!(c.rel_path, "2020-2023/travel", "先頭root基準の相対path");

        let c = classify(
            Path::new("/tmp/elsewhere/travel.zip"),
            "travel.zip",
            &BTreeMap::new(),
            "unfiled",
            &roots,
        )
        .expect("fallback");
        assert_eq!(c.category, "unfiled");
        assert_eq!(c.rel_path, "", "roots不一致はrel_pathなし");
    }

    /// [covers:layout.uuid-v7-generation]
    /// generate_uuid: UUIDv7のhyphenated lowercase 36字(8-4-4-4-12・version nibble=7)・
    /// 連続呼び出しの辞書順は呼び出し順と一致(単調増加傾向)。
    #[test]
    fn generated_uuid_is_v7_and_monotonic() {
        let mut prev: Option<String> = None;
        for _ in 0..5 {
            let u = generate_uuid();
            assert_eq!(u.len(), 36, "36字: {u}");
            let parts: Vec<&str> = u.split('-').collect();
            assert_eq!(
                parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
                vec![8, 4, 4, 4, 12],
                "8-4-4-4-12形式: {u}"
            );
            assert!(u.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
            assert!(u.chars().all(|c| !c.is_ascii_uppercase()), "lowercase: {u}");
            assert!(parts[2].starts_with('7'), "version nibble=7: {u}");
            if let Some(p) = prev {
                assert!(u >= p, "単調増加: prev={p} cur={u}");
            }
            prev = Some(u);
        }
    }

    /// [covers:layout.data-area-object-keys]
    /// data領域key生成: data/pin/metadata object keyのuuid体系構成。
    #[test]
    fn data_area_keys_follow_uuid_layout() {
        let uuid = "0192a4f0-3c1b-7a2e-9f10-112233445566";
        assert_eq!(
            data_object_key("amazing-backup/", uuid).unwrap(),
            "amazing-backup/data/0192a4f0-3c1b-7a2e-9f10-112233445566/default.age"
        );
        assert_eq!(
            pin_object_key("amazing-backup/", uuid, "keep-1").unwrap(),
            "amazing-backup/data/0192a4f0-3c1b-7a2e-9f10-112233445566/keep-1.age"
        );
        assert_eq!(
            metadata_object_key("amazing-backup/", uuid, "旅のしおり2026").unwrap(),
            "amazing-backup/data/0192a4f0-3c1b-7a2e-9f10-112233445566/metadata_旅のしおり2026.json"
        );
        assert_eq!(
            data_prefix("amazing-backup/"),
            "amazing-backup/data/",
            "data領域prefix"
        );
    }

    /// [covers:layout.title-validation]
    /// validate_title: 空/65文字超過/改行/スラッシュ/制御文字は拒否(Usage 64)。
    /// 64文字ちょうど・UTF-8日本語は許容。
    #[test]
    fn title_validation() {
        validate_title("旅のしおり2026").expect("日本語は許容");
        validate_title(&"t".repeat(64)).expect("64文字ちょうどは許容");
        validate_title(&"あ".repeat(64)).expect("日本語64文字も許容(char数基準)");
        for ng in ["", &"t".repeat(65), "a\nb", "a\rb", "a/b", "a\u{1}b"] {
            let e = validate_title(ng).unwrap_err();
            assert_eq!(e.exit_code(), 64, "{ng:?}は拒否(Usage 64): {e}");
        }
    }

    /// [covers:layout.category-name-validation]
    /// category名規則(先頭英数小文字・[a-z0-9-]・64文字内・sys予約拒否)。
    #[test]
    fn category_name_validation() {
        for ok in ["main", "photos2", "a", &"x".repeat(64)] {
            validate_category_name(ok).unwrap_or_else(|e| panic!("{ok}は許容: {e}"));
        }
        for ng in [
            "",
            "_sys",
            "sys",
            "BadName",
            "-lead",
            "under_score",
            &"x".repeat(65),
            "スパン",
        ] {
            assert!(
                validate_category_name(ng).is_err(),
                "{ng:?}は拒否されるべき"
            );
        }
    }

    /// [covers:layout.pin-name-validation-and-default-reserved]
    /// pin名規則と`default`予約拒否(境界: 63文字まで許容・64文字は拒否)。
    #[test]
    fn pin_name_validation() {
        for ok in ["pre-migration", "2026-10-14", "v1", &"a".repeat(63)] {
            validate_pin_name(ok).unwrap_or_else(|e| panic!("{ok}は許容: {e}"));
        }
        for ng in ["", "default", "Bad", "a_b", &"x".repeat(64)] {
            assert!(validate_pin_name(ng).is_err(), "{ng:?}は拒否されるべき");
        }
    }

    /// [covers:layout.prefix-format-validation]
    /// prefix形式(空・末尾/なし・先頭/・使用文字違反を拒否)。
    #[test]
    fn prefix_format_validation() {
        validate_prefix("amazing-backup/").expect("適正");
        validate_prefix("amazing-backup/v2/x-1/").expect("多段も適正");
        for ng in [
            "",
            "amazing-backup",
            "/amazing-backup/",
            "Backup_Remote/",
            "amazing-backup//x/",
        ] {
            assert!(validate_prefix(ng).is_err(), "{ng:?}は拒否されるべき");
        }
    }

    /// [covers:layout.key-length-limit-1024]
    /// 生成keyの1024 byte上限(超過は対象エラー3)。境界内(1024 byteちょうど)は通る。
    /// metadata key = prefix長 + 5 + 36 + 1 + 9 + title長 + 5。
    #[test]
    fn key_length_limit() {
        let uuid = generate_uuid();
        let title64 = "t".repeat(64);
        // metadata key = prefix + "data/" + uuid + "/" + "metadata_" + title + ".json"
        let meta_fixed = "data/".len() + uuid.len() + 1 + "metadata_".len() + ".json".len();
        let just_prefix = format!("{}/", "a".repeat(1024 - meta_fixed - title64.len() - 1));
        validate_prefix(&just_prefix).expect("prefix形式は適正");
        let key = metadata_object_key(&just_prefix, &uuid, &title64).expect("境界内は通る");
        assert_eq!(key.len(), 1024, "境界内(1024 byteちょうど)は受理");
        let over_prefix = format!("{}/", "a".repeat(1024 - meta_fixed - title64.len()));
        let e = metadata_object_key(&over_prefix, &uuid, &title64).unwrap_err();
        assert_eq!(e.exit_code(), 3, "metadata key長超過は対象エラー3: {e}");
        // data key = prefix + "data/" + uuid + "/" + "default.age"
        let data_fixed = "data/".len() + uuid.len() + 1 + "default.age".len();
        let data_over = format!("{}/", "a".repeat(1024 - data_fixed + 1));
        let e2 = data_object_key(&data_over, &uuid).unwrap_err();
        assert_eq!(e2.exit_code(), 3, "data key長超過も対象エラー3: {e2}");
    }

    /// 予約領域key・rel_path正規化の単位確認。
    #[test]
    fn reserve_keys_and_rel_path_normalization() {
        assert_eq!(
            index_object_key("amazing-backup/"),
            "amazing-backup/sys/index.jsonl.age"
        );
        assert_eq!(
            manifest_object_key("amazing-backup/", "20261014-abcd0001").unwrap(),
            "amazing-backup/sys/manifests/20261014-abcd0001.jsonl.age"
        );
        assert_eq!(normalize_rel_path("a/b/").unwrap(), "a/b");
        assert_eq!(normalize_rel_path("a/./b/").unwrap(), "a/b");
        assert_eq!(normalize_rel_path("a//b").unwrap(), "a/b");
        assert_eq!(normalize_rel_path("../a").unwrap(), "a");
        assert_eq!(normalize_rel_path("").unwrap(), "");
        assert!(normalize_rel_path("/abs").is_err());
    }
}
