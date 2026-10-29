//! config initのtemplate置換・escape符号化・生成処理seam(TASK-1・
//! tmp/tasks/tm-1/design_config_init.md 3.2/3.3)のunit test。
//! design doc 3.8のunit観測(template置換・escape符号化・失敗注入)を、src/config.rsの
//! 実装関数をlib API(amazing_backup::config)として直接呼んで行う。テストが仮定する
//! API(design doc 3.8のtarget_functionとgiven「生成関数の書込を差し替え可能な構成」の
//! 具現化。実装工程でsignatureを調整した場合は本テストを追従させる):
//!   - render_init_template(template: &str, credentials_file: &Path) -> BackupResult<String>
//!     (aws.toml生成内容の組立。置換前後2段検査+TOML basic string符号化を担う)
//!   - cmd_config_init_with_writer(config_dir: &Path,
//!       write_file: &mut dyn FnMut(&Path, &[u8], bool) -> std::io::Result<()>)
//!     -> BackupResult<()>
//!     (3fileの事前check・排他的作成の委譲・失敗時rollbackまでの生成処理本体。
//!     write_fileの第3引数はcredential雛形(0600指定)かの旗)
//! テスト先行のため実装関数未実装の段階ではcompile error(赤)を許容する。

mod common;

use amazing_backup::config::{cmd_config_init_with_writer, render_init_template};
use amazing_backup::errors::BackupError;
use common::*;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// marker入りtest template(design doc 3.2の例示形式。全profileのcredentials_fileが
/// marker `__CREDENTIALS_FILE__`)。現行configs/aws.example.toml相当の2profile構成。
const TEMPLATE: &str = "# aws.toml template(テスト用・marker入り)\n\
[aws]\nactive_profile = \"dev\"\n\n\
[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n\
[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"<dev-bucket>\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"__CREDENTIALS_FILE__\"\n\n\
[aws.profiles.family]\npurpose = \"prod\"\nbucket = \"<prod-bucket-1>\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"DEEP_ARCHIVE\"\ncredentials_file = \"__CREDENTIALS_FILE__\"\n";

/// 1file分の排他的作成+書込(本物の書込相当。create_new・private=trueで0600)。
fn real_create(path: &Path, contents: &[u8], private: bool) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    if private {
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents)
}

/// render結果をTOML parseし、全profileのcredentials_fileが期待pathと完全一致することを
/// 検証する(marker残存・escape崩れはparse復元後の完全一致で排斥される)。
fn assert_profiles_credentials_file(rendered: &str, expected: &Path) {
    let v: toml::Value = toml::from_str(rendered)
        .unwrap_or_else(|e| panic!("render結果はTOMLとしてparse可能: {e}\n{rendered}"));
    let profiles = v
        .get("aws")
        .and_then(|a| a.get("profiles"))
        .and_then(|p| p.as_table())
        .expect("[aws.profiles]テーブル");
    assert!(!profiles.is_empty(), "profileが1つ以上ある: {rendered}");
    for (name, profile) in profiles {
        let cf = profile
            .get("credentials_file")
            .and_then(|x| x.as_str())
            .unwrap_or_default();
        assert_eq!(
            cf,
            expected.display().to_string(),
            "profile({name})のcredentials_file: {rendered}"
        );
    }
}

/// 劣化templateが内部エラー1(BackupError::Internal)で弾かれることを検証する。
fn assert_degraded(tag: &str, degraded: &str, cred: &Path) {
    let err =
        render_init_template(degraded, cred).expect_err(&format!("{tag}: 劣化templateはErrのはず"));
    assert!(
        matches!(err, BackupError::Internal(_)),
        "{tag}: template正本の劣化は内部エラー1(exit 1相当): {err}"
    );
}

/// [covers:config.init-templates-from-example]
/// aws.tomlの生成内容は全profileのcredentials_file値(marker)をcredential雛形絶対pathの
/// TOML basic string符号化(\→\\・"→\"・制御文字は\uXXXX)へ置換した内容となる
/// (design doc 3.2)。path文字列中の「\"」と「\」(「\n」2文字を含む)・実改行・Unicode
/// 制御文字は符号化でescapeされ、parse復元後に元pathと完全一致する(単純文字列挿入なら
/// TOML破壊・誤解釈となる境界)。path自身がmarker文字列を含む正常ケースはmarker残存
/// 検査をせずparse一致のみで受理する(marker残存検査が誤検出する境界)。
/// 置換対象は`credentials_file = "MARKER"`値行のみで、コメント中のquoted markerは
/// 保持される。非UTF-8 pathは設定エラー2。置換前に全profile値がmarkerと一致しない
/// template(劣化: 別値・キー欠落・型不正・空profiles・TOML parse失敗)は内部エラー1として
/// 生成しない(template正本の劣化検出)。
#[test]
fn render_init_template_encodes_path_and_rejects_degraded_template() {
    let tmp = TestDir::new("cfg-init-esc");

    // (a) 「"」と「\」を含むpath(「\」+「n」の2文字が改行escapeと解釈される境界を含む)
    let odd = tmp.path().join("qu\"ote\\nd");
    std::fs::create_dir_all(&odd).expect("escape境界dir作成");
    let odd_cred = odd.join("aws-credentials.toml");
    let rendered = render_init_template(TEMPLATE, &odd_cred)
        .unwrap_or_else(|e| panic!("marker入りtemplateの置換は成功: {e}"));
    assert!(
        !rendered.contains("__CREDENTIALS_FILE__"),
        "marker残存なし: {rendered}"
    );
    assert_profiles_credentials_file(&rendered, &odd_cred);

    // (b) escape不要な通常path(ASCIIのみ)も同一の一致検査を通る
    let plain = tmp.path().join("plain-home");
    std::fs::create_dir_all(&plain).expect("通常dir作成");
    let plain_cred = plain.join("aws-credentials.toml");
    let rendered_plain = render_init_template(TEMPLATE, &plain_cred)
        .unwrap_or_else(|e| panic!("通常pathの置換は成功: {e}"));
    assert_profiles_credentials_file(&rendered_plain, &plain_cred);

    // (c) 実改行(LF)を含むpath: 符号化で\u000Aへescapeされparse復元で完全一致する
    let newline = tmp.path().join("line\nbreak");
    std::fs::create_dir_all(&newline).expect("実改行dir作成");
    let newline_cred = newline.join("aws-credentials.toml");
    let rendered_nl = render_init_template(TEMPLATE, &newline_cred)
        .unwrap_or_else(|e| panic!("実改行pathの置換は成功: {e}"));
    assert!(
        rendered_nl.contains("\\u000A"),
        "path内の実改行は\\u000Aへescapeされる: {rendered_nl:?}"
    );
    assert_profiles_credentials_file(&rendered_nl, &newline_cred);

    // (d) Unicode制御文字(U+0007 BEL・U+007F DEL)を含むpath: \uXXXX符号化で往復一致
    let ctrl = tmp.path().join("c\u{7}trl\u{7f}");
    std::fs::create_dir_all(&ctrl).expect("制御文字dir作成");
    let ctrl_cred = ctrl.join("aws-credentials.toml");
    let rendered_ctrl = render_init_template(TEMPLATE, &ctrl_cred)
        .unwrap_or_else(|e| panic!("制御文字pathの置換は成功: {e}"));
    assert!(
        !rendered_ctrl.contains('\u{7}') && !rendered_ctrl.contains('\u{7f}'),
        "制御文字は\\uXXXXへescapeされる: {rendered_ctrl:?}"
    );
    assert_profiles_credentials_file(&rendered_ctrl, &ctrl_cred);

    // (e) path自身がmarker文字列を含む正常ケース: marker残存検査は誤検出するため
    //     行わず(design doc 3.2の廃止理由)、parse一致のみで受理する
    let markerish = tmp.path().join("m__CREDENTIALS_FILE__x");
    std::fs::create_dir_all(&markerish).expect("marker含みdir作成");
    let markerish_cred = markerish.join("aws-credentials.toml");
    let rendered_m = render_init_template(TEMPLATE, &markerish_cred)
        .unwrap_or_else(|e| panic!("marker含みpathの置換は成功: {e}"));
    assert_profiles_credentials_file(&rendered_m, &markerish_cred);

    // (f) 置換対象はcredentials_file値行のみ: コメント中のquoted markerは保持される
    let comment_line = "# memo: \"__CREDENTIALS_FILE__\" は置換後もそのまま残る";
    let with_comment = format!("{comment_line}\n{TEMPLATE}");
    let rendered_c = render_init_template(&with_comment, &plain_cred)
        .unwrap_or_else(|e| panic!("コメント付きtemplateの置換は成功: {e}"));
    assert!(
        rendered_c.contains(comment_line),
        "コメント中のquoted markerは保持される: {rendered_c}"
    );
    assert_profiles_credentials_file(&rendered_c, &plain_cred);

    // (g) 非UTF-8 path(OsStrがUTF-8で表現できない): 設定エラー2で生成しない
    use std::os::unix::ffi::OsStrExt as _;
    let non_utf8 = tmp.path().join(std::ffi::OsStr::from_bytes(b"bad\xffname"));
    let err = render_init_template(TEMPLATE, &non_utf8).expect_err("非UTF-8 pathは設定エラー");
    assert!(
        matches!(err, BackupError::Config(_)),
        "非UTF-8 pathは設定エラー2: {err}"
    );

    // (h) 劣化template群: いずれも内部エラー1で生成しない
    // (h-1) markerが消えた(全profile別値)
    assert_degraded(
        "marker不在",
        &TEMPLATE.replace("__CREDENTIALS_FILE__", "configs/secret/x.toml"),
        &plain_cred,
    );
    // (h-2) 特定profileだけmarkerで無い
    assert_degraded(
        "部分劣化(1profileだけ別値)",
        &TEMPLATE.replacen("__CREDENTIALS_FILE__", "other.toml", 1),
        &plain_cred,
    );
    // (h-3) credentials_fileキー欠落
    let no_key: String = TEMPLATE
        .lines()
        .filter(|l| !l.trim_start().starts_with("credentials_file"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_degraded("credentials_fileキー欠落", &no_key, &plain_cred);
    // (h-4) credentials_fileの型不正(文字列でない)
    assert_degraded(
        "credentials_file型不正(整数値)",
        &TEMPLATE.replacen(
            "credentials_file = \"__CREDENTIALS_FILE__\"",
            "credentials_file = 123",
            1,
        ),
        &plain_cred,
    );
    // (h-5) 空profiles
    let empty_profiles = "[aws]\nactive_profile = \"dev\"\n\
        [aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n\
        [aws.profiles]\n";
    assert_degraded("空profiles", empty_profiles, &plain_cred);
    // (h-6) TOMLとしてparse失敗するtemplate
    assert_degraded("TOML parse失敗", "[aws\nbroken", &plain_cred);
}

/// 失敗注入の種類(rollback観測)。
enum Fail {
    /// Nfile目のopen(create_new)がEEXISTに失敗する競合(事前check後に他processがfileを
    /// 作った状況の模倣: writerが競合fileを実際に作ってからAlreadyExistsを返す。
    /// 競合fileは他processの所有物のためrollbackで削除してはならない)
    Eexist(usize),
    /// Nfile目の排他的作成成功後に部分書込して書込失敗を返す(write失敗の模倣。
    /// fileを残したままErrを返すことで、実装側の部分書込file後始末を観測する)
    WriteFail(usize),
}

/// rollback後のconfig dirの期待。
enum DirAfter {
    /// 削除される(実行前に無かったdirがrollback後空になる)
    Removed,
    /// 残る(実行前から存在したdir・または競合file等が中に残る)
    Kept,
}

/// [covers:config.init-exclusive-create-and-rollback]
/// 生成はcreate_new(true)による排他的作成で行い、失敗時は生成済みfileを削除して
/// 原状回復する(design doc 3.3手順3〜4):
/// - EEXIST競合(事前check後に他processが作った競合fileを実際に配置): 競合fileの中身は
///   保持され(誤削除の検出)・writer成功分のみrollbackで削除される
/// - 書込途中失敗(排他的作成成功後に部分書込して失敗): 失敗対象の部分書込fileも
///   残さず除去される
/// - initがこの実行で作成したconfig dirはrollback後に空になるなら削除され、実行前から
///   存在したdir(無関係file入り・空のいずれも)は残る
#[test]
fn init_write_rolls_back_generated_files_on_failure() {
    // (tag, 失敗注入, config dirを実行前に作るか, 無関係fileを置くか, dirの事後期待)
    let cases: &[(&str, Fail, bool, bool, DirAfter)] = &[
        // EEXIST競合: 競合file(2file目)は残り、1file目はrollback削除。dirは競合fileの
        // ために空にならないので残る
        (
            "EEXIST競合(競合fileは誤削除しない)",
            Fail::Eexist(2),
            false,
            false,
            DirAfter::Kept,
        ),
        // EEXIST競合・実行前から存在したdir内: 無関係fileも不変
        (
            "EEXIST競合・既存dirの無関係file不変",
            Fail::Eexist(2),
            true,
            true,
            DirAfter::Kept,
        ),
        // 書込途中失敗(部分書込fileも残さない): 新規dirはrollback後に空となり削除
        (
            "書込途中失敗(部分書込fileも残さない)",
            Fail::WriteFail(2),
            false,
            false,
            DirAfter::Removed,
        ),
        // 1file目での書込失敗: 生成物ゼロ・dirも削除
        (
            "1file目で書込失敗(生成物ゼロ)",
            Fail::WriteFail(1),
            false,
            false,
            DirAfter::Removed,
        ),
        // 3file目(credential雛形)での書込失敗: 2file成功分もrollback
        (
            "3file目で書込失敗(成功分2fileもrollback)",
            Fail::WriteFail(3),
            false,
            false,
            DirAfter::Removed,
        ),
        // 実行前から存在した空dirはrollbackでも削除せず保持
        (
            "書込途中失敗・既存空dirは保持",
            Fail::WriteFail(2),
            true,
            false,
            DirAfter::Kept,
        ),
    ];
    let rival_content = b"RIVAL-CONTENT-BY-OTHER-PROCESS";
    for (i, (tag, fail, precreate, with_unrelated, dir_after)) in cases.iter().enumerate() {
        let tmp = TestDir::new(&format!("cfg-init-rb-{i}"));
        let dir = tmp.path().join("cfg");
        let unrelated = dir.join("unrelated.txt");
        if *precreate {
            std::fs::create_dir_all(&dir).expect("config dir事前作成");
            if *with_unrelated {
                write_file(&unrelated, b"UNRELATED");
            }
        }
        // 失敗対象file名(Eexist/WriteFailはいずれもNfile目)
        let fail_name = match fail {
            Fail::Eexist(n) | Fail::WriteFail(n) => match n {
                1 => "pipeline.toml",
                2 => "aws.toml",
                _ => "aws-credentials.toml",
            },
        };
        let fail_path = dir.join(fail_name);
        let mut written: Vec<PathBuf> = Vec::new();
        let mut n = 0usize;
        let result = cmd_config_init_with_writer(&dir, &mut |path, contents, private| {
            n += 1;
            let hit = match fail {
                Fail::Eexist(k) | Fail::WriteFail(k) => n == *k,
            };
            if !hit {
                real_create(path, contents, private)?;
                written.push(path.to_path_buf());
                return Ok(());
            }
            match fail {
                // 事前check通過後に他processがfileを作った状況の模倣
                Fail::Eexist(_) => {
                    write_file(path, rival_content);
                    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
                }
                // 排他的作成成功→部分書込→書込失敗(fileは残したまま返す)
                Fail::WriteFail(_) => {
                    use std::os::unix::fs::OpenOptionsExt as _;
                    let mut opts = std::fs::OpenOptions::new();
                    opts.write(true).create_new(true);
                    if private {
                        opts.mode(0o600);
                    }
                    let mut f = opts.open(path)?;
                    f.write_all(&contents[..contents.len() / 2])?;
                    Err(std::io::Error::other("書込失敗の注入"))
                }
            }
        });
        let err = result.expect_err(&format!("{tag}: 失敗注入時はErr"));
        assert_eq!(
            err.exit_code(),
            2,
            "{tag}: 生成系の失敗は設定エラー2(rollback実行): {err}"
        );
        // writer成功分のfileはrollbackで削除される
        for p in &written {
            assert!(
                !p.exists(),
                "{tag}: 生成済みfileはrollbackで削除: {}",
                p.display()
            );
        }
        // EEXIST競合fileは他processの所有物のため内容とも保持される(誤削除の検出)
        if matches!(fail, Fail::Eexist(_)) {
            assert!(
                fail_path.is_file(),
                "{tag}: 競合fileはrollbackで削除しない: {}",
                fail_path.display()
            );
            assert_eq!(
                std::fs::read(&fail_path).expect("競合file読込"),
                rival_content,
                "{tag}: 競合fileの内容は不変"
            );
        } else {
            // 書込途中失敗の部分書込fileは残さない(実装側の後始末観測)
            assert!(
                !fail_path.exists(),
                "{tag}: 部分書込fileもrollbackで削除: {}",
                fail_path.display()
            );
        }
        // 部分生成しない: 競合file以外の3対象は残らない
        for f in ["pipeline.toml", "aws.toml", "aws-credentials.toml"] {
            if matches!(fail, Fail::Eexist(_)) && f == fail_name {
                continue;
            }
            assert!(
                !dir.join(f).exists(),
                "{tag}: 3対象({f})は残っていない(部分生成しない)"
            );
        }
        if *with_unrelated {
            assert!(
                unrelated.is_file(),
                "{tag}: 実行前から存在した無関係fileは不変"
            );
        }
        match dir_after {
            DirAfter::Removed => {
                assert!(
                    !dir.exists(),
                    "{tag}: initがこの実行で作成したconfig dirは除去される: {}",
                    dir.display()
                );
            }
            DirAfter::Kept => {
                assert!(
                    dir.is_dir(),
                    "{tag}: config dirは残る(実行前から存在したdir・または競合file残存): {}",
                    dir.display()
                );
            }
        }
    }
}
