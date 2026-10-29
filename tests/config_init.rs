//! config init コマンド(TASK-1・tmp/tasks/tm-1/design_config_init.md 3章)の結合test。
//! bin subprocessで`config init`を実行し、AMAZING_BACKUP_HOME(ABH)で生成先をtmp dirへ
//! 向けて生成3file・終了コード・stdout・file権限を観測する。
//! template置換・escape符号化と失敗注入(排他的作成・rollback)のunit観測は
//! tests/config_init_unit.rs(lib API直呼び)で行う。
//! テスト先行のため実装前の実行failを許容する(cargo test --no-runは通る)。

mod common;

use common::*;
use std::path::Path;

/// ABH=<abh>で`config init`を実行する。
fn run_config_init(abh: &str) -> (i32, String, String) {
    run_bin_env(&["config", "init"], &[("AMAZING_BACKUP_HOME", abh)])
}

/// config initで3fileを生成済みのABH環境を返す(生成確認まで済ませたsetup)。
fn init_generated_home(tag: &str) -> (TestDir, std::path::PathBuf) {
    let tmp = TestDir::new(tag);
    let abh = tmp.path().join("abh");
    let (code, stdout, stderr) = run_config_init(&abh.display().to_string());
    assert_eq!(code, 0, "config initはexit 0: {stdout}{stderr}");
    (tmp, abh)
}

/// toml fileを読んで値を書き換える(コメントは失われるが読み込み側の検証には影響しない)。
fn rewrite_toml(path: &Path, edit: impl FnOnce(&mut toml::Value)) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("toml読込({}): {e}", path.display()));
    let mut v: toml::Value = toml::from_str(&text).expect("toml parse");
    edit(&mut v);
    write_file(
        path,
        toml::to_string(&v).expect("toml serialize").as_bytes(),
    );
}

/// 生成3file(pipeline.toml・aws.toml・aws-credentials.toml)の存在を一括検証。
fn assert_three_files_exist(dir: &Path) {
    for f in ["pipeline.toml", "aws.toml", "aws-credentials.toml"] {
        assert!(
            dir.join(f).is_file(),
            "{f}が生成されている: {}",
            dir.display()
        );
    }
}

/// [covers:config.init-generates-under-resolved-paths]
/// config initはresolve_app_paths解決のconfig dir配下へ3fileを生成する(design doc 3.1)。
/// ABH絶対path(存在しないdir指定)は$ABH配下へ生成しconfig dirも作成する。
/// ABH相対pathは実行時cwd基準で絶対化した<cwd>/<相対path>配下へ生成する
/// (resolve_app_paths自体の解決仕様は不変のまま、init側の絶対化のみ作用)。
#[test]
fn config_init_generates_under_resolved_paths() {
    // (a) ABH絶対path(存在しないdir): $ABH配下の3file生成・config dirも作成される
    let tmp = TestDir::new("cfg-init-abs");
    let abh = tmp.path().join("abh");
    assert!(!abh.exists(), "前提: ABH dirは存在しない");
    let (code, stdout, stderr) = run_config_init(&abh.display().to_string());
    assert_eq!(
        code, 0,
        "ABH絶対path指定のconfig initはexit 0: {stdout}{stderr}"
    );
    assert_three_files_exist(&abh);

    // (b) ABH相対path: cwdを固定して実行すると<cwd>/<相対path>配下へ生成(絶対化)
    let tmp2 = TestDir::new("cfg-init-rel");
    let cwd = tmp2.path().join("work");
    std::fs::create_dir_all(&cwd).expect("cwd dir作成");
    let (code, stdout, stderr) = run_bin_cwd(
        &["config", "init"],
        &[("AMAZING_BACKUP_HOME", "rel-home")],
        &cwd,
    );
    assert_eq!(
        code, 0,
        "ABH相対path指定のconfig initはexit 0: {stdout}{stderr}"
    );
    let resolved = cwd.join("rel-home");
    assert_three_files_exist(&resolved);
}

/// [covers:config.init-existing-files-not-overwritten]
/// 生成対象fileが既に存在する場合(通常file・dangling symlinkを含む。存在判定は
/// symlink_metadata)は、存在file毎のerror表示+復旧手順案内でexit 2し、いずれのfileも
/// 作成・変更しない(all-or-nothing。design doc 3.3手順1〜2)。
#[test]
fn config_init_existing_files_not_overwritten() {
    /// 事前配置の種類。
    enum Pre {
        File,
        DanglingSymlink,
    }
    // (tag, 事前配置対象と方法)
    let cases: &[(&str, &[(&str, Pre)])] = &[
        ("一部のみ通常file", &[("pipeline.toml", Pre::File)]),
        (
            "全部通常file",
            &[
                ("pipeline.toml", Pre::File),
                ("aws.toml", Pre::File),
                ("aws-credentials.toml", Pre::File),
            ],
        ),
        ("dangling symlink", &[("aws.toml", Pre::DanglingSymlink)]),
    ];
    for (i, (tag, pre)) in cases.iter().enumerate() {
        let tmp = TestDir::new(&format!("cfg-init-exist-{i}"));
        let abh = tmp.path().join("abh");
        std::fs::create_dir_all(&abh).expect("config dir作成");
        let existing_content = b"EXISTING-CONTENT-DO-NOT-TOUCH";
        let mut expected_names: Vec<&str> = Vec::new();
        for (name, how) in *pre {
            let p = abh.join(name);
            match how {
                Pre::File => {
                    write_file(&p, existing_content);
                }
                Pre::DanglingSymlink => {
                    std::os::unix::fs::symlink(
                        "/nonexistent/amazing-backup-it-dangling-target",
                        &p,
                    )
                    .expect("dangling symlink作成");
                }
            }
            expected_names.push(name);
        }
        let (code, _stdout, stderr) = run_config_init(&abh.display().to_string());
        assert_eq!(code, 2, "{tag}: 既存file存在はexit 2: {stderr}");
        // 存在対象毎のerror行(対象fileのpathがstderrに現れる)
        for name in &expected_names {
            assert!(
                stderr.contains(name),
                "{tag}: 存在対象({name})のerror行: {stderr}"
            );
        }
        // 復旧手順の案内(退避して再実行・削除して再実行)
        assert!(
            stderr.contains("退避") && stderr.contains("再実行"),
            "{tag}: 復旧案内(退避して再実行)を含む: {stderr}"
        );
        // 既存fileの内容・種別は不変
        for (name, how) in *pre {
            let p = abh.join(name);
            match how {
                Pre::File => {
                    assert_eq!(
                        std::fs::read(&p).expect("既存file読込"),
                        existing_content,
                        "{tag}: 既存file({name})の内容は不変"
                    );
                }
                Pre::DanglingSymlink => {
                    assert!(
                        p.is_symlink(),
                        "{tag}: dangling symlink({name})はsymlinkのまま"
                    );
                    assert_eq!(
                        std::fs::read_link(&p).expect("symlink先読込"),
                        std::path::PathBuf::from("/nonexistent/amazing-backup-it-dangling-target"),
                        "{tag}: symlink先は不変"
                    );
                }
            }
        }
        // 新規fileは1つも作成されない(config dirのentry数は事前配置分のみ)
        let entries: Vec<String> = std::fs::read_dir(&abh)
            .expect("config dir読み")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries.len(),
            expected_names.len(),
            "{tag}: all-or-nothing(新規fileなし): {entries:?}"
        );
    }
}

/// [covers:config.init-credential-template-empty-and-private]
/// credential雛形は空値・schemaコメント付きで生成される。credential以外の設定が
/// 全て有効な状態では、空値のままのcredentialが空値検査固有のエラー
/// (access_key_idが空)でexit 2となりS3 API呼び出しに到達しない(design doc 3.4。
/// fake seam未使用の観測。permissionの観測はumask明示の別test
/// config_init_credential_permission_under_umask_022が担う)。
#[test]
fn config_init_credential_template_empty_and_private() {
    if !ssh_keygen_available() {
        eprintln!("skipping: ssh-keygenが利用できない");
        return;
    }
    let (tmp, abh) = init_generated_home("cfg-init-cred");
    let cred = abh.join("aws-credentials.toml");

    // 値は空値([credentials]テーブルのaccess_key_id/secret_access_key)
    let text = std::fs::read_to_string(&cred).expect("credential雛形読込");
    let v: toml::Value = toml::from_str(&text).expect("credential雛形はTOMLとしてparse可能");
    let c = v
        .get("credentials")
        .and_then(|c| c.as_table())
        .expect("[credentials]テーブル");
    assert_eq!(
        c.get("access_key_id").and_then(|x| x.as_str()),
        Some(""),
        "access_key_idは空値: {text}"
    );
    assert_eq!(
        c.get("secret_access_key").and_then(|x| x.as_str()),
        Some(""),
        "secret_access_keyは空値: {text}"
    );

    // credential以外を有効化(public_keys=実在鍵・bucket=記述済み値)してbackup push:
    // 空値credentialは固有のエラー文でexit 2(S3 API呼び出し前に停止)
    let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
    rewrite_toml(&abh.join("pipeline.toml"), |v| {
        v["recipients"]["public_keys"] =
            toml::Value::Array(vec![toml::Value::String(public.display().to_string())]);
    });
    rewrite_toml(&abh.join("aws.toml"), |v| {
        if let Some(profiles) = v
            .get_mut("aws")
            .and_then(|a| a.get_mut("profiles"))
            .and_then(|p| p.as_table_mut())
        {
            for (_name, profile) in profiles.iter_mut() {
                if let Some(t) = profile.as_table_mut() {
                    t.insert(
                        "bucket".into(),
                        toml::Value::String("init-cred-it-bucket".into()),
                    );
                }
            }
        }
    });
    let target = tmp.path().join("data.zip");
    write_file(&target, b"cred-empty-plain");
    let (code, _stdout, stderr) = run_bin_env(
        &["backup", "push", &target.display().to_string()],
        &[("AMAZING_BACKUP_HOME", &abh.display().to_string())],
    );
    assert_eq!(
        code, 2,
        "空値credentialのままのbackup pushは設定エラー2: {stderr}"
    );
    assert!(
        stderr.contains("access_key_id") && stderr.contains("空"),
        "credential空値固有のエラー文(access_key_idが空): {stderr}"
    );
}

/// [covers:config.init-stdout-artifacts-and-next-steps]
/// 成功時のstdoutに生成3fileの絶対path(絶対化後)と次ステップ案内
/// (credential値記述・bucket・public_keys)・docs参照(`amazing-backup docs usage`案内。
/// binary埋め込みのためrepo未cloneの利用者にも当該binary versionの手順書へ到達可能)
/// が表示される(design doc 3.6。TASK-4でGitHub URL参照からdocsコマンド参照へ改訂)。
#[test]
fn config_init_stdout_artifacts_and_next_steps() {
    let tmp = TestDir::new("cfg-init-out");
    let abh = tmp.path().join("abh");
    let (code, stdout, stderr) = run_config_init(&abh.display().to_string());
    assert_eq!(code, 0, "config initはexit 0: {stderr}");
    // 生成3fileの絶対path(ABH=tmp配下の絶対path=絶対化後の表示)
    for f in ["pipeline.toml", "aws.toml", "aws-credentials.toml"] {
        assert!(
            stdout.contains(&abh.join(f).display().to_string()),
            "stdoutに生成fileの絶対path({f}): {stdout}"
        );
    }
    // 次ステップ案内(編集必須箇所の優先度順)
    assert!(
        stdout.contains("access_key_id"),
        "案内: credential値記述: {stdout}"
    );
    assert!(stdout.contains("bucket"), "案内: bucket記述: {stdout}");
    assert!(
        stdout.contains("public_keys"),
        "案内: public_keys記述: {stdout}"
    );
    // docs参照はamazing-backup docs usage案内(binary埋め込みのためrepo未cloneでも到達可能)
    assert!(
        stdout.contains("amazing-backup docs usage"),
        "docs参照のdocs usage案内: {stdout}"
    );
}

unsafe extern "C" {
    fn umask(mask: u32) -> u32;
}

/// [covers:config.init-credential-template-empty-and-private]
/// credential雛形のpermission観測(mode & 0o077 == 0)。umask 022下で生成することで
/// 検出力を担保する(umask 077環境ではmode(0o600)指定の欠落が0666&~077=0600に隠れて
/// 検出不能なため、寛容な022では0644=group readableとなり欠落を検出できる)。
/// umaskはtest process全体に作用するため復元までの間に並行testの作るfile権限が
/// 緩まり得るが、permissionを検証するtestは他に無い(権限観測は本testのみ)。
/// 鍵生成(ssh-keygen)とは独立しているため不在環境でも実行される。
#[test]
fn config_init_credential_permission_under_umask_022() {
    let tmp = TestDir::new("cfg-init-perm");
    let abh = tmp.path().join("abh");
    let prev = unsafe { umask(0o022) };
    let (code, stdout, stderr) = run_config_init(&abh.display().to_string());
    unsafe { umask(prev) };
    assert_eq!(
        code, 0,
        "umask 022下のconfig initはexit 0: {stdout}{stderr}"
    );
    let cred = abh.join("aws-credentials.toml");
    let md = std::fs::symlink_metadata(&cred).expect("credential雛形のmeta");
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        md.permissions().mode() & 0o077,
        0,
        "credential雛形はgroup/other権限なし(mode 0600指定。umask 022でも緩まない): {:o}",
        md.permissions().mode()
    );
}

/// [covers:config.init-templates-from-example]
/// 生成されるpipeline.tomlは埋め込みtemplate(configs/pipeline.example.toml)とbyte一致。
/// 生成aws.tomlはmarker残存なし・TOMLとしてparse可能・parse後の全profile
/// credentials_fileは生成したcredential雛形の絶対pathと完全一致(design doc 3.2の
/// 統合側観測。escape境界・劣化template検出のunit観測はtests/config_init_unit.rs)。
#[test]
fn config_init_templates_match_embedded_example() {
    let (_tmp, abh) = init_generated_home("cfg-init-tmpl");

    // pipeline.tomlは埋め込みtemplateとbyte一致(変換なし書き出し)
    let generated = std::fs::read(abh.join("pipeline.toml")).expect("pipeline.toml読込");
    let template: &[u8] = include_bytes!("../configs/pipeline.example.toml");
    assert_eq!(
        generated, template,
        "pipeline.tomlは埋め込みtemplateとbyte一致"
    );

    // aws.toml: marker残存なし・parse後の全profile credentials_fileは雛形pathと完全一致
    let text = std::fs::read_to_string(abh.join("aws.toml")).expect("aws.toml読込");
    assert!(
        !text.contains("__CREDENTIALS_FILE__"),
        "marker残存なし: {text}"
    );
    let v: toml::Value = toml::from_str(&text).expect("生成aws.tomlはTOMLとしてparse可能");
    let cred_s = abh.join("aws-credentials.toml").display().to_string();
    let profiles = v
        .get("aws")
        .and_then(|a| a.get("profiles"))
        .and_then(|p| p.as_table())
        .expect("[aws.profiles]");
    assert!(!profiles.is_empty(), "profileが1つ以上ある: {text}");
    for (name, profile) in profiles {
        let cf = profile
            .get("credentials_file")
            .and_then(|x| x.as_str())
            .unwrap_or_default();
        assert_eq!(
            cf, cred_s,
            "profile({name})のcredentials_fileは雛形の絶対path: {text}"
        );
    }
}
