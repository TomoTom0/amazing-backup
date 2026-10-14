//! CLI entrypoint系の結合test(設計8章CLI-002/CLI-004)。
//! env!("CARGO_BIN_EXE_backup-remote")でbinを直接実行して検証する。

mod common;

use common::*;

/// [covers:cli.version-flag]
/// --versionでversion表示・exit 0となる。
#[test]
fn version_flag_prints_and_exits_zero() {
    let (code, stdout, stderr) = run_bin(&["--version"]);
    assert_eq!(code, 0, "exit 0: {stderr}");
    assert!(
        stdout.contains("backup-remote"),
        "version文字列にpackage名を含む: {stdout}"
    );
    assert!(!stdout.trim().is_empty(), "stdoutへ表示: {stdout}");
}

/// [covers:cli.module-entrypoint-parity]
/// 同一引数をlib経由(cli::run直接呼出)とbin backup-remoteで実行すると同一の終了コードを返す。
#[test]
fn bin_and_lib_entrypoint_parity() {
    let tmp = TestDir::new("cli-parity");
    let cfg = tmp.path().join("pipeline.toml");
    write_file(
        &cfg,
        b"[recipients]\npublic_keys = []\n[output]\ndir = \"out\"\nregistry = \"reg.jsonl\"\n[behavior]\nkeep_going = false\n",
    );

    let cases: Vec<Vec<String>> = vec![
        // 設定エラー2(不正なpipeline.toml: public_keys空)
        vec!["plan".into(), "--config".into(), cfg.display().to_string()],
        // 対象エラー3(不在ファイル)
        vec![
            "plan".into(),
            "--config".into(),
            tmp.path().join("absent.toml").display().to_string(),
        ],
        // usage誤り64
        vec!["--no-such-option".into()],
        // version(0)
        vec!["--version".into()],
    ];

    for case in cases {
        let mut bin_args: Vec<&str> = vec!["backup-remote"];
        bin_args.extend(case.iter().map(|s| s.as_str()));
        let (bin_code, stdout, stderr) = run_bin(&bin_args[1..]);

        let mut lib_argv: Vec<std::ffi::OsString> = vec!["backup-remote".into()];
        lib_argv.extend(case.iter().map(|s| std::ffi::OsString::from(s)));
        let lib_code = backup_remote::cli::run(lib_argv);

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
