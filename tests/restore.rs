//! restore エリアの結合test。TASK-11(Phase B)で旧restoreサブコマンド
//! (request/status/download)が削除され、復元job系の共通経路(tier/days検証・
//! Restore header parse・--key事前検査・平文衝突保護)は get / backup pull / list jobs
//! (src/storage.rs)経路で検証する。fake seam(BACKUP_REMOTE_FAKE_S3_STATE)のbin
//! subprocess実行(実networkなし)。平文衝突保護はPhase B実装前のため実行failを許容。

mod common;

use common::*;

/// calls.logを空にする(以降の呼び出しのみを観測する)。
fn clear_calls(fx: &StorageFixture) {
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
}

/// [covers:restore.request-tier-days-validation]
/// get/backup pull共通の事前検査(resolve_tier_days): --tierがallowlist
/// (Standard/Bulk)外はusage error 64・--daysが1〜365外は設定エラー2。
/// いずれもS3 API呼び出し(索引GET含む)より前に検出される。
#[test]
fn tier_days_validation_before_s3_calls() {
    let Some(fx) = StorageFixture::new("rs-tierdays", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"tier-days-plain");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    clear_calls(&fx);

    for (tag, extra, want) in [
        ("tier Expedited", vec!["--tier", "Expedited"], 64),
        ("days 0", vec!["--days", "0"], 2),
        ("days 366", vec!["--days", "366"], 2),
    ] {
        // get側
        let (code, stdout, stderr) = fx.get("a.zip", &extra);
        assert_eq!(
            code, want,
            "get {tag}は事前検査でexit {want}: {stdout}{stderr}"
        );
        // backup pull側(--all --yes付き=確認より後・S3 APIより前に検出)
        let mut pull_extra: Vec<&str> = vec!["--all", "--yes"];
        pull_extra.extend(extra.iter().copied());
        let (code, stdout, stderr) = fx.backup_pull(&pull_extra);
        assert_eq!(
            code, want,
            "backup pull {tag}は事前検査でexit {want}: {stdout}{stderr}"
        );
    }
    assert_eq!(
        read_calls(&fx.state),
        "",
        "S3 API呼び出しなし(索引GET前に検出): {}",
        read_calls(&fx.state)
    );
}

/// [covers:restore.restore-header-parse]
/// Restore header parse(list jobs表示経由): 値なし=not-requested・
/// ongoing-request="true"=ongoing・ongoing-request="false"=restoredへ分類される。
/// 判定はRestore値の原文部分一致で行われ、expiry-dateの時刻変換を前提としない。
#[test]
fn restore_header_parse_via_list_jobs() {
    let Some(fx) = StorageFixture::new("rs-hdrparse", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.data_root.join("b.zip");
    let c = fx.data_root.join("c.zip");
    write_file(&a, b"hdrparse-a");
    write_file(&b, b"hdrparse-b");
    write_file(&c, b"hdrparse-c");
    for p in [&a, &b, &c] {
        assert_eq!(fx.push(&[p], &[], &[]).0, 0);
    }
    let key_a = fx.file_key("unfiled", None, "a.zip");
    let key_b = fx.file_key("unfiled", None, "b.zip");
    let key_c = fx.file_key("unfiled", None, "c.zip");
    // fake backendのHeadObject応答のRestore状態: a=復元済み・b=進行中・c=未発行(不在)
    fx.seed_restore(&key_a, "restored");
    fx.seed_restore(&key_b, "ongoing");

    let (code, stdout, stderr) = fx.list("jobs", &[]);
    assert_eq!(code, 0, "list jobsはexit 0: {stdout}{stderr}");
    for (key, state) in [
        (&key_a, "restored"),
        (&key_b, "ongoing"),
        (&key_c, "not-requested"),
    ] {
        let line = stdout
            .lines()
            .find(|l| l.contains(key.as_str()))
            .unwrap_or_else(|| panic!("対象key({key})の行: {stdout}"));
        assert!(
            line.contains(state),
            "Restore headerからの{state}分類: {line}"
        );
    }
}

/// [covers:restore.download-preflight-errors]
/// get事前検査: --key未指定かつ[verify].decrypt_keyも解決不能(pipeline.toml不在の
/// 作業dir)・--keyの鍵ファイル不在・ageバイナリ不在は、いずれもS3 API呼び出し
/// (索引GET含む)より前に設定エラー2。
#[test]
fn get_preflight_key_and_age_errors() {
    let Some(fx) = StorageFixture::new("rs-preflight", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"preflight-plain");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    clear_calls(&fx);
    let state_dir = fx.state.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let cfg_s = fx.cfg.display().to_string();

    // (a) --key未指定・pipeline.toml不在の作業dir(default解決不能。
    // --configも渡さない=decrypt_keyの解決先が無い)
    let empty_cwd = fx.elsewhere.join("empty-cwd");
    std::fs::create_dir_all(&empty_cwd).expect("empty cwd");
    let (code, _, stderr) = run_bin_cwd(
        &["get", "a.zip", "--aws-config", &aws_s],
        &[("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())],
        &empty_cwd,
    );
    assert_eq!(
        code, 2,
        "--key未指定かつdecrypt_key解決不能は設定エラー2: {stderr}"
    );

    // (b) --keyに不在path(get helperは--key自動付与のため直接実行)
    let bad_key = "/nonexistent/secret-key";
    let (code, _, stderr) = run_bin_env(
        &[
            "get",
            "a.zip",
            "--config",
            &cfg_s,
            "--aws-config",
            &aws_s,
            "--key",
            bad_key,
        ],
        &[("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())],
    );
    assert_eq!(code, 2, "鍵ファイル不在は設定エラー2: {stderr}");

    // (c) ageバイナリ不在(PATHからageを除外)
    let (code, _, stderr) = fx.get_env("a.zip", &[], &[("PATH", "/nonexistent-bin-dir")]);
    assert_eq!(code, 2, "ageバイナリ不在は設定エラー2: {stderr}");

    assert_eq!(
        read_calls(&fx.state),
        "",
        "S3 API呼び出しなし(索引GET等の対象処理前に検出): {}",
        read_calls(&fx.state)
    );
}

/// [covers:restore.download-plain-conflict]
/// 平文衝突保護(get/pull出力先・旧download仕様の継承):
/// 既存平文のsha256が取得対象のsource_sha256と不一致なら上書きせず対象エラー3。
/// 一致ならskip(冪等)。--yes指定時のみ上書きする。
#[test]
fn get_plain_conflict_protection() {
    let Some(fx) = StorageFixture::new("rs-plain", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"plain-conflict-original");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let out = fx.elsewhere.join("plain-out");
    let out_s = out.display().to_string();

    // 一致case: 事前に取得済みの平文が置かれた状態での再getはskip(冪等)
    assert_eq!(fx.get("a.zip", &["--out", &out_s]).0, 0);
    clear_calls(&fx);
    let (code, stdout, stderr) = fx.get("a.zip", &["--out", &out_s]);
    assert_eq!(code, 0, "sha一致の既存平文はskipでexit 0: {stdout}{stderr}");
    assert!(stdout.to_lowercase().contains("skip"), "skip表示: {stdout}");
    assert!(
        !read_calls(&fx.state)
            .lines()
            .any(|l| l.starts_with("s3 GetObject")),
        "一致caseは対象の再取得を行わない: {}",
        read_calls(&fx.state)
    );
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("既存平文"),
        b"plain-conflict-original".to_vec(),
        "既存平文は保持される"
    );

    // 不一致case: 改変済み平文(sha不一致)は上書きせずexit 3
    std::fs::write(out.join("a.zip"), b"tampered-plain-content").expect("改変平文");
    let (code, _, stderr) = fx.get("a.zip", &["--out", &out_s]);
    assert_eq!(
        code, 3,
        "sha不一致の既存平文は対象エラー3(上書きしない): {stderr}"
    );
    assert!(
        stderr.contains("a.zip"),
        "衝突nameの案内(手動リネーム促し): {stderr}"
    );
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("既存平文(再読)"),
        b"tampered-plain-content".to_vec(),
        "既存平文は不変(上書きしない)"
    );

    // --yes指定時のみ上書きする
    let (code, stdout, stderr) = fx.get("a.zip", &["--out", &out_s, "--yes"]);
    assert_eq!(code, 0, "--yesは上書きを許可してexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("上書き後の平文"),
        b"plain-conflict-original".to_vec(),
        "--yes指定時は取得対象の内容で上書きされる"
    );
}
