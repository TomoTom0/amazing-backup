//! TASK-10物理配置の純関数群(分類解決・key生成・名前規則検証)。
//! 設計書 tmp/tasks/tm-10/design_s3_storage.md 1章・条件書 tests/design/layout.toml。
//! key構造: `<prefix>/<category>[/<rel_path>]/<name>/default.age`(versioning)・
//! `<prefix>/<category>[/<rel_path>]/<name>/<pin>.age`(write-once)・
//! 予約領域 `<prefix>sys/`(索引・manifest)。

use crate::errors::{BackupError, BackupResult};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// S3 object key長上限(S3仕様。超過は対象エラー3)
pub const KEY_MAX_BYTES: usize = 1024;
/// 予約領域の第1階層名(`<prefix>sys/`。索引・manifestの配置先)。
/// category名としての使用は拒否される(名前規則 `[a-z0-9][a-z0-9-]` と合わせ、
/// 第1階層の衝突を構造的に防ぐ)
pub const RESERVE_SEGMENT: &str = "sys";
/// 予約領域の旧設計表記(`_sys`)。現行では使わないが、旧表記での誤入力を
/// category名・files写像path先頭として弾くために拒否リストへ残す
const RESERVE_REJECTED_LEGACY: &str = "_sys";
/// pin名として予約されている語(default.ageとの衝突防止)
pub const RESERVED_PIN_NAME: &str = "default";

/// prefix形式検証(設計1.1): 空でない・`/`終端・先頭`/`禁止・
/// 区切り`/`で結ぶ各要素は`[a-z0-9-]`のみ・空要素禁止。違反は設定エラー2。
pub fn validate_prefix(prefix: &str) -> BackupResult<()> {
    let invalid = |detail: &str| {
        BackupError::Config(format!(
            "prefixの形式が不正です({detail}): {prefix:?}(例: \"backup-remote/\")"
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

/// category名規則(設計1.2): `^[a-z0-9][a-z0-9-]{0,63}$`・予約語(sys/_sys)拒否。
/// 違反は設定エラー2。
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
    if name == RESERVE_SEGMENT || name == RESERVE_REJECTED_LEGACY {
        return Err(invalid("予約領域名(sys)は使えません"));
    }
    Ok(())
}

/// pin名規則(設計1.3): `^[a-z0-9][a-z0-9-]{0,62}$`(全体63文字以内)・`default`予約。
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

/// rel_path正規化(設計1.2): 先頭`/`禁止・`./`/`../`要素と空要素の拒否・
/// 末尾`/`の除去。空文字列は「rel_pathなし」を表す正当値。
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

/// pathの第1階層が予約領域に衝突するか(files写像の設定検証用)。
pub fn path_collides_reserve(path: &str) -> bool {
    path.split('/')
        .next()
        .is_some_and(|first| first == RESERVE_SEGMENT || first == RESERVE_REJECTED_LEGACY)
}

/// identity_hash: sha256(realpath文字列)先頭8hex(設計1.2・2.1)。
/// realpathはcanonicalize済み絶対pathを想定する(衝突folder名と同一規則)。
pub fn identity_hash(realpath: &Path) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(realpath.to_string_lossy().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..8].to_string()
}

/// 対象の分類解決結果(設計1.2の3段)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub category: String,
    /// 正規化済み。空文字列=rel_pathなし(category直下)
    pub rel_path: String,
}

/// files手動写像の1件(configのschemaと同一形状の純粋参照用)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMapping {
    pub category: String,
    pub path: String,
}

/// 分類解決(設計1.2の優先順位): ①files写像 ②auto_path_roots相対化(最初に一致したroot)
/// ③fallback(category=unfiled・rel_pathなし)。
/// source_canonicalはrealpath解決済み絶対path。source_nameはbasename(拡張子付き)。
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
            // rel_pathは「root配下のdirectory側path」(末尾のファイル名はfolder segmentが
            // 担うため除去。root直下のfileはrel_pathなし)
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

/// 衝突時のfolder名(設計1.2): primary(無hash保持者)は`<name>`、それ以外は
/// 拡張子を除いたstemへ`--<identity_hash>`を付ける(`<name>.zip` → `<name>--<hash8>`)。
pub fn folder_name(name: &str, identity_hash: &str, primary: bool) -> String {
    if primary {
        return name.to_string();
    }
    let stem = match name.rfind('.') {
        Some(pos) if pos > 0 => &name[..pos],
        _ => name,
    };
    format!("{stem}--{identity_hash}")
}

/// key長検証(S3仕様の1024 byte上限。超過は対象エラー3・設計1.2)。
pub fn check_key_length(key: &str) -> BackupResult<()> {
    if key.len() > KEY_MAX_BYTES {
        return Err(BackupError::Target(format!(
            "生成keyがS3のkey長上限({KEY_MAX_BYTES} byte)を超えています({} byte): {key}。files写像のpath縮約またはauto_path_rootsの見直しで対応してください",
            key.len()
        )));
    }
    Ok(())
}

fn object_dir(prefix: &str, category: &str, rel_path: &str, folder: &str) -> String {
    let mut key = format!("{prefix}{category}/");
    if !rel_path.is_empty() {
        key.push_str(rel_path);
        key.push('/');
    }
    key.push_str(folder);
    key
}

/// file objectのkey: `<prefix>/<category>[/<rel_path>]/<folder>/default.age`。
pub fn file_object_key(
    prefix: &str,
    category: &str,
    rel_path: &str,
    folder: &str,
) -> BackupResult<String> {
    let key = format!(
        "{}/default.age",
        object_dir(prefix, category, rel_path, folder)
    );
    check_key_length(&key)?;
    Ok(key)
}

/// pin objectのkey: `<prefix>/<category>[/<rel_path>]/<folder>/<pin>.age`。
pub fn pin_object_key(
    prefix: &str,
    category: &str,
    rel_path: &str,
    folder: &str,
    pin: &str,
) -> BackupResult<String> {
    let key = format!(
        "{}/{pin}.age",
        object_dir(prefix, category, rel_path, folder)
    );
    check_key_length(&key)?;
    Ok(key)
}

/// 索引objectのkey: `<prefix>sys/index.jsonl.age`(設計1.1・2.1)。
pub fn index_object_key(prefix: &str) -> String {
    format!("{prefix}{RESERVE_SEGMENT}/index.jsonl.age")
}

/// manifest objectのkey: `<prefix>sys/manifests/<batch_id>.jsonl.age`(設計1.1)。
pub fn manifest_object_key(prefix: &str, batch_id: &str) -> BackupResult<String> {
    let key = format!("{prefix}{RESERVE_SEGMENT}/manifests/{batch_id}.jsonl.age");
    check_key_length(&key)?;
    Ok(key)
}

/// 同一slot(category+rel_path+name)に既存identity保持者がいる場合の無hash帰属
/// (設計1.2(a)): 既存identityが自identityと一致するなら自らがprimary。
pub fn existing_holder_primary(existing_identity: &str, own_identity: &str) -> bool {
    existing_identity == own_identity
}

/// 同一run内で同時新規の衝突の場合の無hash帰属(設計1.2(b)):
/// realpath辞書順最小の1つがprimary。candidatesが空ならNone(全員hash付きには
/// ならない運用上、呼び出し側は1件以上を渡す)。
pub fn lexicographic_primary<'a>(candidates: &[&'a str]) -> Option<&'a str> {
    candidates.iter().min().copied()
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

        let key = file_object_key("backup-remote/", &c.category, &c.rel_path, "travel.zip")
            .expect("key生成");
        assert_eq!(
            key,
            "backup-remote/photos/2020-2023/travel/travel.zip/default.age"
        );
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

        let key = file_object_key("backup-remote/", "unfiled", "", "travel.zip").expect("key");
        assert_eq!(key, "backup-remote/unfiled/travel.zip/default.age");
    }

    /// [covers:layout.key-generation-collision-hash-suffix]
    /// 衝突folder名と無hash帰属の決定論的規則(既存保持者維持・同時新規は辞書順最小)。
    #[test]
    fn collision_folder_and_primary_rules() {
        let a = Path::new("/data/a/travel.zip");
        let b = Path::new("/data/b/travel.zip");
        let ha = identity_hash(a);
        let hb = identity_hash(b);
        assert_ne!(ha, hb);
        assert_eq!(ha.len(), 8);

        // 既存保持者(a)がいる: aは無hash維持・bはhash付き
        assert!(existing_holder_primary(&ha, &ha));
        assert!(!existing_holder_primary(&ha, &hb));
        assert_eq!(folder_name("travel.zip", &ha, true), "travel.zip");
        assert_eq!(
            folder_name("travel.zip", &hb, false),
            format!("travel--{hb}")
        );

        // 同時新規: 辞書順最小(/data/a/...)が無hash
        assert_eq!(
            lexicographic_primary(&[b.to_string_lossy().as_ref(), a.to_string_lossy().as_ref()]),
            Some(a.to_string_lossy().as_ref())
        );

        let key = file_object_key(
            "backup-remote/",
            "unfiled",
            "",
            &format!("travel.zip--{hb}"),
        )
        .expect("key");
        assert_eq!(
            key,
            format!("backup-remote/unfiled/travel.zip--{hb}/default.age")
        );
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
        validate_prefix("backup-remote/").expect("適正");
        validate_prefix("backup-remote/v2/x-1/").expect("多段も適正");
        for ng in [
            "",
            "backup-remote",
            "/backup-remote/",
            "Backup_Remote/",
            "backup-remote//x/",
        ] {
            assert!(validate_prefix(ng).is_err(), "{ng:?}は拒否されるべき");
        }
    }

    /// [covers:layout.key-length-limit-1024]
    /// 生成keyの1024 byte上限(超過は対象エラー3)。
    #[test]
    fn key_length_limit() {
        let long_name = "x".repeat(1100);
        let e = file_object_key("backup-remote/", "unfiled", "", &long_name).unwrap_err();
        assert_eq!(e.exit_code(), 3, "key長超過は対象エラー3: {e}");
        // 境界内(1024 byteちょうど)は通る
        let folder: String =
            "y".repeat(KEY_MAX_BYTES - "backup-remote/unfiled/".len() - "/default.age".len());
        file_object_key("backup-remote/", "unfiled", "", &folder).expect("境界内は通る");
    }

    /// 予約領域key・rel_path正規化・reserve衝突の単位確認。
    #[test]
    fn reserve_keys_and_rel_path_normalization() {
        assert_eq!(
            index_object_key("backup-remote/"),
            "backup-remote/sys/index.jsonl.age"
        );
        assert_eq!(
            manifest_object_key("backup-remote/", "20261014-abcd0001").unwrap(),
            "backup-remote/sys/manifests/20261014-abcd0001.jsonl.age"
        );
        assert_eq!(
            pin_object_key(
                "backup-remote/",
                "photos",
                "t",
                "travel.zip",
                "pre-migration"
            )
            .unwrap(),
            "backup-remote/photos/t/travel.zip/pre-migration.age"
        );
        assert_eq!(normalize_rel_path("a/b/").unwrap(), "a/b");
        assert_eq!(normalize_rel_path("a/./b/").unwrap(), "a/b");
        assert_eq!(normalize_rel_path("a//b").unwrap(), "a/b");
        assert_eq!(normalize_rel_path("../a").unwrap(), "a");
        assert_eq!(normalize_rel_path("").unwrap(), "");
        assert!(normalize_rel_path("/abs").is_err());
        assert!(path_collides_reserve("_sys/x"));
        assert!(path_collides_reserve("sys/x"));
        assert!(!path_collides_reserve("a/sys"));
    }
}
