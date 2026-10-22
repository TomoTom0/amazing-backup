//! 対象解決(存在・拡張子・重複・basename一意性)(設計5章targets.rs・3.1解決規則)。

use crate::errors::{BackupError, BackupResult};
use std::path::{Path, PathBuf};

/// JST(+09:00)の固定offset。
pub(crate) fn jst_offset() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(9 * 3600).expect("JST offset")
}

/// 現在時刻(JST)。
pub(crate) fn now_jst() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::Utc::now().with_timezone(&jst_offset())
}

/// 4.1注記の単一serializer: `YYYY-MM-DDTHH:MM:SS+09:00`(秒精度・小数秒なし)。
/// 記録時もskip判定時もこの変換を通した文字列で厳密比較する。
pub(crate) fn format_jst(dt: &chrono::DateTime<chrono::FixedOffset>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S+09:00").to_string()
}

/// 現在のJST暦日(YYYY-MM-DD)。
pub(crate) fn jst_today() -> String {
    now_jst().format("%Y-%m-%d").to_string()
}

/// SystemTimeをJSTのDateTimeへ(秒精度へ切捨て正規化)。
fn systemtime_to_jst(st: std::time::SystemTime) -> chrono::DateTime<chrono::FixedOffset> {
    use chrono::SubsecRound as _;
    let dt: chrono::DateTime<chrono::Utc> = st.into();
    dt.with_timezone(&jst_offset()).trunc_subsecs(0)
}

/// 解決済みの対象アーカイブ(3.1)。TASK-18: title(新規発行時の実体ラベル)を追加。
#[derive(Debug)]
pub struct Target {
    /// realpath解決後の絶対path
    pub source_path: PathBuf,
    /// 出力名(basename。拡張子付き)
    pub source_name: String,
    /// 平文サイズ(bytes)
    pub size: u64,
    /// 平文のmtime(JST・秒精度に切捨て正規化。registry記録は4.1注記の文字列形式)
    pub mtime: chrono::DateTime<chrono::FixedOffset>,
    /// TASK-18: from-listの[[targets]] title欄由来のtitle(位置引数は常にNone)。
    /// 新規uuid発行時のみ必須(合流・再PUT時は現行title継続のため省略可)
    pub title: Option<String>,
}

/// from-list tomlのentry(TASK-18 5.6: title欄追加)。
#[derive(Debug)]
pub struct FromListEntry {
    /// 必須・文字列
    pub path: PathBuf,
    /// 任意・basename形式の出力名override(新経路では拒否)
    pub name: Option<String>,
    /// 任意・実体のtitle(validate_titleはbackup push側で実施)
    pub title: Option<String>,
}

/// from-list tomlのschema(未知キー・path必須を検証。TASK-18: title欄追加)。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FromListEntryFile {
    path: PathBuf,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(serde::Deserialize)]
struct FromListFile {
    #[serde(default)]
    targets: Vec<FromListEntryFile>,
}

/// 3.1規則の全体(plan/encrypt共用)。位置引数とfrom-listの和集合を解決する。
/// エラーは集約して1度に全件表示する(fail-fastは「解決段階で全部見せる」意味)。
pub fn resolve_targets(
    positionals: &[PathBuf],
    from_list: Option<&Path>,
) -> BackupResult<Vec<Target>> {
    resolve_targets_opts(positionals, from_list, true)
}

/// TASK-10新経路(backup push)用の対象解決: 同名basenameを許容する
/// (出力名一意性検査を省く。同名はlayoutの衝突folderで分離する)。
/// name override(from-listのname)は新経路では撤廃(設計1.2)のため拒否する。
pub fn resolve_targets_storage(
    positionals: &[PathBuf],
    from_list: Option<&Path>,
) -> BackupResult<Vec<Target>> {
    if let Some(list_path) = from_list {
        for entry in parse_from_list(list_path)? {
            if entry.name.is_some() {
                return Err(BackupError::Target(format!(
                    "新経路(backup push)ではfrom-listのname指定(name override)は使えません(basenameは常に元ファイル名。{} )",
                    entry.path.display()
                )));
            }
        }
    }
    resolve_targets_opts(positionals, from_list, false)
}

fn resolve_targets_opts(
    positionals: &[PathBuf],
    from_list: Option<&Path>,
    enforce_unique_names: bool,
) -> BackupResult<Vec<Target>> {
    // 和集合の構築(位置引数 → from-listの順。TASK-18: title欄を伴う)
    let mut candidates: Vec<(PathBuf, Option<String>, Option<String>)> = Vec::new();
    for p in positionals {
        candidates.push((p.clone(), None, None));
    }
    if let Some(list_path) = from_list {
        for entry in parse_from_list(list_path)? {
            candidates.push((entry.path, entry.name, entry.title));
        }
    }

    // 3.1-1/2: realpath解決・通常file検査・拡張子検査(エラー集約)
    let mut errors: Vec<String> = Vec::new();
    let mut resolved: Vec<(PathBuf, String, Option<String>)> = Vec::new();
    for (raw, name_override, title) in &candidates {
        let abs = match raw.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                errors.push(format!("対象ファイルが存在しない: {}", raw.display()));
                continue;
            }
        };
        let meta = match std::fs::metadata(&abs) {
            Ok(m) => m,
            Err(_) => {
                errors.push(format!(
                    "対象ファイルのmetadata取得に失敗: {}",
                    abs.display()
                ));
                continue;
            }
        };
        if !meta.is_file() {
            errors.push(format!(
                "対象は通常fileでない(directory等): {}",
                abs.display()
            ));
            continue;
        }
        let ext_ok = ["zip", "7z"].iter().any(|e| {
            abs.extension()
                .map(|x| x.to_string_lossy().eq_ignore_ascii_case(e))
                .unwrap_or(false)
        });
        if !ext_ok {
            errors.push(format!("拡張子が対象外(.zip/.7zのみ): {}", abs.display()));
            continue;
        }
        let basename = abs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let source_name = match name_override {
            Some(n) => {
                // name overrideはbasename形式のみ許容(2.5・3.1。出力path・S3キーの
                // traversalや空nameを防止する対象エラー)
                if !is_valid_basename(n) {
                    errors.push(format!(
                        "from-listのnameはbasename形式のみ許容される(空・パス区切り・\".\"・\"..\"は不可): {n:?} ({})",
                        raw.display()
                    ));
                    continue;
                }
                n.clone()
            }
            None => basename,
        };
        resolved.push((abs, source_name, title.clone()));
    }

    // 3.1-3: realpath重複検査
    let mut seen_paths = std::collections::HashSet::new();
    for (abs, _, _) in &resolved {
        if !seen_paths.insert(abs.clone()) {
            errors.push(format!("realpathが重複している: {}", abs.display()));
        }
    }
    // 3.1-4: 出力名の一意性検査(旧経路のみ。TASK-10新経路は同名を許容し
    // layoutの衝突folder(name--hash8)で分離するためこの検査をスキップする)
    if enforce_unique_names {
        let mut seen_names = std::collections::HashSet::new();
        for (abs, name, _) in &resolved {
            if !seen_names.insert(name.clone()) {
                errors.push(format!("出力名が重複している: {name} ({})", abs.display()));
            }
        }
    }

    if !errors.is_empty() {
        return Err(BackupError::Target(errors.join("\n")));
    }

    let mut targets = Vec::with_capacity(resolved.len());
    for (abs, source_name, title) in resolved {
        let meta = std::fs::metadata(&abs).map_err(|e| {
            BackupError::Target(format!("対象のmetadata取得に失敗 ({}): {e}", abs.display()))
        })?;
        let mtime = systemtime_to_jst(meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH));
        targets.push(Target {
            size: meta.len(),
            source_path: abs,
            source_name,
            mtime,
            title,
        });
    }
    Ok(targets)
}

/// name overrideがbasename形式か(空でない・`/`区切りを含まない・`.`/`..`でない)。
/// `Path::file_name`が入力全体と一致することで「単一のファイル名成分」を検証する。
fn is_valid_basename(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && Path::new(name).file_name() == Some(std::ffi::OsStr::new(name))
}

/// from-list tomlの読込とschema検査(path必須・name任意・未知キー拒否)(2.5)。
pub fn parse_from_list(path: &Path) -> BackupResult<Vec<FromListEntry>> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        BackupError::Config(format!(
            "from-listファイルを読めない ({}): {e}",
            path.display()
        ))
    })?;
    let parsed: FromListFile = toml::from_str(&text).map_err(|e| {
        BackupError::Config(format!(
            "from-list tomlのparseに失敗 ({}): {e}",
            path.display()
        ))
    })?;
    Ok(parsed
        .targets
        .into_iter()
        .map(|t| FromListEntry {
            path: t.path,
            name: t.name,
            title: t.title,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::BackupError;
    use crate::testutil::{TempDir, write_file};

    fn target_error(e: &BackupError) -> &str {
        match e {
            BackupError::Target(m) => m,
            other => panic!("期待した区分はTarget、実際は {other:?}"),
        }
    }

    fn config_error(e: &BackupError) -> &str {
        match e {
            BackupError::Config(m) => m,
            other => panic!("期待した区分はConfig、実際は {other:?}"),
        }
    }

    fn write_zip(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let p = dir.join(name);
        write_file(&p, contents);
        p
    }

    /// [covers:plan.resolves-positional-targets]
    /// 位置引数のみで対象解決され、path・サイズ・mtimeが取得される(表示はcli.rsのformat_plan_report検証)。
    #[test]
    fn resolves_positional_targets() {
        let tmp = TempDir::new("plan-pos");
        let a = write_zip(tmp.path(), "photo.zip", b"zip-bytes-A");
        let b = write_zip(tmp.path(), "docs.7z", b"7z-bytes-BB");

        let targets = resolve_targets(&[a.clone(), b.clone()], None).expect("2件とも解決される");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].source_path, a.canonicalize().unwrap());
        assert_eq!(targets[0].source_name, "photo.zip");
        assert_eq!(targets[0].size, b"zip-bytes-A".len() as u64);
        assert_eq!(targets[1].source_name, "docs.7z");
        assert_eq!(targets[1].size, b"7z-bytes-BB".len() as u64);
        // mtimeはJST(+09:00)・秒精度に正規化された文字列と往復一致する
        for t in &targets {
            let s = t.mtime.to_rfc3339();
            assert!(s.ends_with("+09:00"), "JST固定: {s}");
            assert_eq!(
                t.mtime.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
                s
            );
        }
    }

    /// [covers:plan.from-list-union]
    /// 位置引数1件とfrom-list 1件の和集合2件が解決される。
    #[test]
    fn from_list_union_with_positionals() {
        let tmp = TempDir::new("plan-union");
        let a = write_zip(tmp.path(), "a.zip", b"A");
        let b = write_zip(tmp.path(), "b.7z", b"B");
        let list = tmp.path().join("targets.toml");
        write_file(
            &list,
            format!("[[targets]]\npath = \"{}\"\n", b.display()).as_bytes(),
        );

        let targets = resolve_targets(&[a], Some(&list)).expect("和集合が解決される");
        let mut names: Vec<&str> = targets.iter().map(|t| t.source_name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["a.zip", "b.7z"]);
    }

    /// [covers:plan.missing-or-dir-target-error]
    /// 対象不在・directory指定は対象エラー。解決段階のエラーは全件列挙後に一度に失敗する。
    #[test]
    fn missing_or_dir_target_is_target_error() {
        let tmp = TempDir::new("plan-miss");
        let absent = tmp.path().join("absent.zip");
        let dir = tmp.path().join("subdir");
        std::fs::create_dir_all(&dir).expect("dir create");

        let e = resolve_targets(&[absent.clone(), dir.clone()], None).unwrap_err();
        let msg = target_error(&e);
        assert_eq!(e.exit_code(), 3);
        // 全件列挙: 両方の対象pathが同一のerror messageに現れる
        assert!(msg.contains("absent.zip"), "不在pathが含まれる: {msg}");
        assert!(msg.contains("subdir"), "directory pathが含まれる: {msg}");
    }

    /// [covers:plan.extension-whitelist]
    /// .zip/.7zのみ許容(大文字小文字無視)。.tar.gz・.7z.001は対象エラー。
    #[test]
    fn extension_whitelist() {
        let tmp = TempDir::new("plan-ext");
        let upper = write_zip(tmp.path(), "ARCHIVE.ZIP", b"u");
        let tar_gz = write_zip(tmp.path(), "x.tar.gz", b"t");
        let split = write_zip(tmp.path(), "y.7z.001", b"s");

        // 大文字.ZIPは許容
        let ok = resolve_targets(&[upper], None).expect("大文字.ZIPは解決される");
        assert_eq!(ok[0].source_name, "ARCHIVE.ZIP");

        // .tar.gz / .7z.001は対象エラー(両方同時指定で全件列挙)
        let e = resolve_targets(&[tar_gz.clone(), split], None).unwrap_err();
        let msg = target_error(&e);
        assert_eq!(e.exit_code(), 3);
        assert!(
            msg.contains("x.tar.gz") && msg.contains("y.7z.001"),
            "対象外2件が列挙: {msg}"
        );
    }

    /// [covers:plan.realpath-duplicate-error]
    /// realpath重複(位置引数×from-list間含む)は対象エラー。
    #[test]
    fn realpath_duplicate_is_target_error() {
        let tmp = TempDir::new("plan-dup");
        let f = write_zip(tmp.path(), "same.zip", b"S");
        let abs = f.canonicalize().unwrap();
        // 冗長なpath表現でもrealpath解決後に同一になる
        let dotted = tmp.path().join("./same.zip");
        let list = tmp.path().join("targets.toml");
        write_file(
            &list,
            format!("[[targets]]\npath = \"{}\"\n", abs.display()).as_bytes(),
        );

        let e = resolve_targets(&[f.clone(), dotted], None).unwrap_err();
        target_error(&e);

        let e = resolve_targets(&[f], Some(&list)).unwrap_err();
        let msg = target_error(&e);
        assert_eq!(e.exit_code(), 3);
        assert!(msg.contains("same.zip"), "重複対象が特定される: {msg}");
    }

    /// [covers:config.from-list-schema-validation]
    /// from-list tomlのpath欠落・未知キーは設定エラー。
    #[test]
    fn from_list_schema_validation() {
        let tmp = TempDir::new("plan-fromlist");

        // 1. path欠落
        let no_path = tmp.path().join("nopath.toml");
        write_file(&no_path, b"[[targets]]\nname = \"x.zip\"\n");
        let e = parse_from_list(&no_path).unwrap_err();
        config_error(&e);

        // 2. 未知キー
        let unknown = tmp.path().join("unknown.toml");
        write_file(
            &unknown,
            b"[[targets]]\npath = \"/tmp/a.zip\"\nextra_key = 1\n",
        );
        let e = parse_from_list(&unknown).unwrap_err();
        let msg = config_error(&e);
        assert!(
            msg.contains("extra_key"),
            "未知キー名がmessageに含まれる: {msg}"
        );

        // 正常系(TASK-18): path必須・title欄追加・省略可
        let ok = tmp.path().join("ok.toml");
        write_file(
            &ok,
            format!(
                "[[targets]]\npath = \"/tmp/a.zip\"\n\n[[targets]]\npath = \"/tmp/b.7z\"\ntitle = \"旅のしおり\"\n"
            )
            .as_bytes(),
        );
        let entries = parse_from_list(&ok).expect("正常なfrom-listは読み込める");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, PathBuf::from("/tmp/a.zip"));
        assert_eq!(entries[0].name, None);
        assert_eq!(entries[0].title, None, "title省略可");
        assert_eq!(entries[1].title.as_deref(), Some("旅のしおり"), "title欄");
    }
}
