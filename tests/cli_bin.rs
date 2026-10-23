//! CLI entrypoint系の結合test(設計8章CLI-002/004・TASK-11 Phase B)。
//! env!("CARGO_BIN_EXE_amazing-backup")でbinを直接実行して検証する。
//! TASK-11(Phase B)で旧サブコマンド(plan/encrypt/upload/verify/restore)が削除される
//! (実装前のため旧サブコマンド削除のtestはfailを許容)。

mod common;

use common::*;

/// [covers:cli.version-flag]
/// --versionでversion表示・exit 0となる。
#[test]
fn version_flag_prints_and_exits_zero() {
    let (code, stdout, stderr) = run_bin(&["--version"]);
    assert_eq!(code, 0, "exit 0: {stderr}");
    assert!(
        stdout.contains("amazing-backup"),
        "version文字列にpackage名を含む: {stdout}"
    );
    assert!(!stdout.trim().is_empty(), "stdoutへ表示: {stdout}");
}

/// [covers:cli.module-entrypoint-parity]
/// 同一引数をlib経由(cli::run直接呼出)とbin amazing-backupで実行すると同一の終了コードを返す
/// (TASK-11 Phase B: 旧plan経路の代表ケースはbackup pushの対象指定へ読み替え)。
#[test]
fn bin_and_lib_entrypoint_parity() {
    let tmp = TestDir::new("cli-parity");
    let bad_cfg = tmp.path().join("pipeline.toml");
    write_file(
        &bad_cfg,
        b"[recipients]\npublic_keys = []\n[output]\ndir = \"out\"\nregistry = \"reg.jsonl\"\n[behavior]\nkeep_going = false\n",
    );
    // 対象エラー3用の正常pipeline(実在する鍵)とaws.toml
    let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
    let ok_cfg = tmp.path().join("pipeline-ok.toml");
    write_file(
        &ok_cfg,
        format!(
            "[recipients]\npublic_keys = [\"{}\"]\n[output]\ndir = \"out\"\nregistry = \"reg.jsonl\"\n[behavior]\nkeep_going = false\n",
            public.display()
        )
        .as_bytes(),
    );
    let creds = tmp.path().join("creds.toml");
    write_file(
        &creds,
        b"[credentials]\naccess_key_id = \"AKITEST0000000000000A\"\nsecret_access_key = \"parity-not-real\"\n",
    );
    let aws_toml = tmp.path().join("aws.toml");
    write_file(
        &aws_toml,
        format!(
            "[aws]\nactive_profile = \"dev\"\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{}\"\n",
            creds.display()
        )
        .as_bytes(),
    );

    let cases: Vec<Vec<String>> = vec![
        // 設定エラー2(不正なpipeline.toml: public_keys空でのbackup push)
        vec![
            "backup".into(),
            "push".into(),
            "--config".into(),
            bad_cfg.display().to_string(),
            "x.zip".into(),
        ],
        // 対象エラー3(不在ファイルでのbackup push)
        vec![
            "backup".into(),
            "push".into(),
            "--config".into(),
            ok_cfg.display().to_string(),
            "--aws-config".into(),
            aws_toml.display().to_string(),
            tmp.path().join("absent.zip").display().to_string(),
        ],
        // usage誤り64
        vec!["--no-such-option".into()],
        // version(0)
        vec!["--version".into()],
    ];

    for case in cases {
        let mut bin_args: Vec<&str> = vec!["amazing-backup"];
        bin_args.extend(case.iter().map(|s| s.as_str()));
        let (bin_code, stdout, stderr) = run_bin(&bin_args[1..]);

        let mut lib_argv: Vec<std::ffi::OsString> = vec!["amazing-backup".into()];
        lib_argv.extend(case.iter().map(|s| std::ffi::OsString::from(s)));
        let lib_code = amazing_backup::cli::run(lib_argv);

        assert_eq!(
            bin_code, lib_code,
            "parity: args={case:?} bin_out={stdout} bin_err={stderr}"
        );
        // 出力も(observableなstdoutについて)空/非空の一致を概略検証
        if bin_code == 0 && bin_args[1] == "--version" {
            assert!(!stdout.trim().is_empty());
        }
    }
}

/// [covers:cli.old-subcommands-removed]
/// 旧サブコマンド(plan/encrypt/upload/verify/restore=restore request/status/download含む)は
/// 削除され、未知サブコマンドとしてusage error 64となる。--helpの文言に旧サブコマンドが
/// 現れない(TASK-11 Phase B)。
#[test]
fn old_subcommands_are_removed() {
    for sub in ["plan", "encrypt", "upload", "verify", "restore"] {
        let (code, stdout, stderr) = run_bin(&[sub, "--help"]);
        assert_eq!(
            code, 64,
            "旧サブコマンド{sub}は未知サブコマンド扱いのusage error 64: {stdout}{stderr}"
        );
    }
    for args in [
        vec!["restore", "request", "--date", "2026-10-01"],
        vec!["restore", "status", "--date", "2026-10-01"],
        vec!["restore", "download", "--date", "2026-10-01"],
    ] {
        let (code, stdout, stderr) = run_bin(&args);
        assert_eq!(
            code, 64,
            "旧restore系({args:?})はusage error 64: {stdout}{stderr}"
        );
    }

    // --help文言に旧サブコマンドが現れない
    let (code, stdout, stderr) = run_bin(&["--help"]);
    assert_eq!(code, 0, "--helpはexit 0: {stderr}");
    for sub in ["plan", "encrypt", "upload", "restore"] {
        assert!(
            !stdout.contains(sub),
            "--helpの文言に旧サブコマンド({sub})は現れない: {stdout}"
        );
    }
    // 新体系は現れる(集約先の代表)
    assert!(
        stdout.contains("backup"),
        "backupは--helpに現れる: {stdout}"
    );
    assert!(stdout.contains("get"), "getは--helpに現れる: {stdout}");
}
