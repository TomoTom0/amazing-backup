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
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
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
    for (p, title) in [(&a, "a.zip"), (&b, "b.zip"), (&c, "c.zip")] {
        let Some((code, stdout)) = push_titled_pty(&fx, &[p], title, &[]) else {
            return;
        };
        assert_eq!(code, 0, "初回push({title}): {stdout}");
    }
    let key_a = fx.data_key(&uuid_of_path(&fx, &a));
    let key_b = fx.data_key(&uuid_of_path(&fx, &b));
    let key_c = fx.data_key(&uuid_of_path(&fx, &c));
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
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
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
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
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

/// [covers:restore.pull-plain-conflict-unreadable-protected]
/// get/backup pullの平文衝突で既存平文の読込が失敗する(sha256検証不能)場合(L-5+L-R5):
/// --yesなしでは上書きせず対象エラー3として保護する。--yesは明示的上書き指示のため
/// get/pullとも読込不能な既存平文を上書きして続行する(get側をpull側へ統一)。
/// pullの確認ガードはTTY(yes入力)で通して検証する。読込不能化はchmod 0200
/// (読込不可・書込可)で行い、--yes付きの上書き(書込)は成功する形で検証する。
#[test]
fn plain_conflict_unreadable_existing_file_protected_get_and_pull() {
    let Some(fx) = StorageFixture::new("rs-pullread", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pull-unreadable-v1-longer");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "v1 push: {stdout}");
    let uuid1 = uuid_of_path(&fx, &a);
    let out = fx.elsewhere.join("pull-out");
    let out_s = out.display().to_string();
    // 1度取得して既存平文を配置
    assert_eq!(fx.get(&uuid1, &["--out", &out_s]).0, 0);
    // 新版(内容変更は別entity発行・同一title。設計4.3「移動+内容変更」と同経路)
    write_file(&a, b"pull-unreadable-v2-long");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "v2 push(同一title別entity): {stdout}");
    let uuid2 = uuid_of_path(&fx, &a);
    // v2 runのみを対象範囲にする(pull --run。全対象だと同一title別entityの復元先が
    // 衝突するため)
    let run2 = fx
        .registry_values()
        .into_iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid2.as_str())
        })
        .and_then(|r| r.get("batch_id").and_then(|b| b.as_str()).map(String::from))
        .expect("v2のbatch_id");
    let dest = out.join("a.zip");
    use std::os::unix::fs::PermissionsExt as _;
    let make_unreadable = || {
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o200))
            .expect("既存平文を読込不能化(読込不可・書込可)")
    };
    let make_readable = || {
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644))
            .expect("既存平文を読込可能化")
    };

    // get --yesなし(uuid探索でv2 entityを指定): 読込失敗=検証不能でも上書きしない(対象エラー3)
    make_unreadable();
    let (code, _, stderr) = fx.get(&uuid2, &["--out", &out_s]);
    assert_eq!(
        code, 3,
        "getは読込不能な既存平文を--yesなしで対象エラー3保護(上書きしない): {stderr}"
    );
    assert!(
        stderr.contains("a.zip"),
        "衝突nameの表示(読込失敗の案内): {stderr}"
    );
    make_readable();
    assert_eq!(
        std::fs::read(&dest).expect("既存平文(再読)"),
        b"pull-unreadable-v1-longer".to_vec(),
        "get --yesなしでは既存平文は不変"
    );

    // get --yes: 明示的上書き指示のため読込不能な既存平文も上書きして続行
    make_unreadable();
    let (code, stdout, stderr) = fx.get(&uuid2, &["--out", &out_s, "--yes"]);
    assert_eq!(
        code, 0,
        "get --yesは読込不能な既存平文を上書きしてexit 0: {stdout}{stderr}"
    );
    make_readable();
    assert_eq!(
        std::fs::read(&dest).expect("上書き後の平文"),
        b"pull-unreadable-v2-long".to_vec(),
        "get --yesは取得対象の内容で上書きする"
    );

    // さらに新版push(pull対象を変えて上書きの観測性を保つ)
    write_file(&a, b"pull-unreadable-v3-longest");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "v3 push: {stdout}");
    let uuid3 = uuid_of_path(&fx, &a);
    let run3 = fx
        .registry_values()
        .into_iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid3.as_str())
        })
        .and_then(|r| r.get("batch_id").and_then(|b| b.as_str()).map(String::from))
        .expect("v3のbatch_id");

    // pull --yesなし(確認はTTY入力で通す・範囲はv3 run): 読込失敗=検証不能でも上書きしない
    make_unreadable();
    let state_s = fx.state.display().to_string();
    let cfg_s = fx.cfg.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let key_s = fx.secret.display().to_string();
    let Some((code, output)) = run_bin_pty_stdin(
        &[
            "backup",
            "pull",
            "--run",
            run3.as_str(),
            "--config",
            &cfg_s,
            "--aws-config",
            &aws_s,
            "--key",
            &key_s,
            "--out",
            &out_s,
        ],
        &[("BACKUP_REMOTE_FAKE_S3_STATE", state_s.as_str())],
        "yes",
    ) else {
        eprintln!("skipping: script(pty)が利用できない");
        return;
    };
    assert_eq!(
        code, 3,
        "pullは読込不能な既存平文を--yesなしで対象エラー3保護(上書きしない): {output}"
    );
    assert!(output.contains("a.zip"), "衝突nameの表示: {output}");
    make_readable();
    assert_eq!(
        std::fs::read(&dest).expect("既存平文(pull --yesなし後)"),
        b"pull-unreadable-v2-long".to_vec(),
        "pull --yesなしでは既存平文は不変"
    );

    // pull --yes(範囲はv3 run): 上書きして続行
    make_unreadable();
    let (code, stdout, stderr) =
        fx.backup_pull(&["--run", run3.as_str(), "--yes", "--out", &out_s]);
    assert_eq!(
        code, 0,
        "pull --yesは読込不能な既存平文を上書きしてexit 0: {stdout}{stderr}"
    );
    make_readable();
    assert_eq!(
        std::fs::read(&dest).expect("上書き後の平文(pull --yes)"),
        b"pull-unreadable-v3-longest".to_vec(),
        "pull --yesは取得対象の内容で上書きする"
    );
}
