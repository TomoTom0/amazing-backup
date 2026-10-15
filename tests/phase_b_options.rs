//! TASK-11(Phase B)のCLI option系結合test: --profile(aws.toml active_profile一時上書き)・
//! --aws-config既定path・索引GET転送エラーのexit 5分離・manage jobs reissue。
//! fake seam(BACKUP_REMOTE_FAKE_S3_STATE)のbin subprocessで検証する
//! (Phase B実装前のため--profile等は未知オプション扱いとなり各testはfailを許容)。

mod common;

use common::*;

/// calls.logを空にする(以降の呼び出しのみを観測する)。
fn clear_calls(fx: &StorageFixture) {
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
}

/// dev(active_profile)とprodの2profileを含むaws.toml本文(dev/prodでprefixが異なる)。
fn two_profile_aws_toml(fx: &StorageFixture) -> String {
    format!(
        "[aws]\nactive_profile = \"dev\"\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"backup-remote/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n\n[aws.profiles.prod]\npurpose = \"prod\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"prod-remote/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n",
        creds = fx.credentials.display()
    )
}

/// [covers:cli.profile-option-overrides-active]
/// --profile <name>(全コマンド共通・TASK-11 Phase B): aws.tomlの[aws].active_profileを
/// 当該実行限り上書きする。指定profileのbucket/prefix/storage_class等で動作する
/// (object key・索引keyが指定profile側prefixで発行・参照される)。
#[test]
fn profile_option_overrides_active_profile() {
    let Some(fx) = StorageFixture::new("pb-profile", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_aws_toml(&two_profile_aws_toml(&fx));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"profile-override-plain");

    // 書き込み系: backup push --profile prod はprod側prefixで発行される
    let (code, stdout, stderr) = fx.push(&[&a], &["--profile", "prod"], &[]);
    assert_eq!(
        code, 0,
        "backup push --profile prodはexit 0: {stdout}{stderr}"
    );
    let prod_key = "prod-remote/unfiled/a.zip/default.age";
    assert!(
        fx.object_path(prod_key).is_file(),
        "object keyはprod側prefix: {}",
        fx.object_path(prod_key).display()
    );
    assert!(
        !fx.object_path(&fx.file_key("unfiled", None, "a.zip"))
            .is_file(),
        "dev側(既定active_profile)prefixへは書かない"
    );
    assert!(
        fx.object_path("prod-remote/sys/index.jsonl.age").is_file(),
        "索引もprod側prefixへ配置"
    );

    // 照会系: list files --profile prod はprod側索引を参照する
    let (code, stdout, stderr) = fx.list_env("files", &["--profile", "prod"], &[]);
    assert_eq!(
        code, 0,
        "list files --profile prodはexit 0: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("a.zip") || stdout.contains("unfiled"),
        "prod側索引の内容: {stdout}"
    );
    // 既定(active_profile=dev)のlist filesはdev側(空)を参照し対象0件
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "既定のlist filesもexit 0: {stdout}{stderr}");
    assert!(
        !stdout.contains("a.zip"),
        "既定はdev側索引(対象0件): {stdout}"
    );
}

/// [covers:cli.profile-unknown-config-error]
/// --profileにaws.tomlへ存在しないprofile名を指定した場合は設定エラー2。
/// S3 API呼び出しは行われない。
#[test]
fn profile_unknown_name_is_config_error() {
    let Some(fx) = StorageFixture::new("pb-profghost", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_aws_toml(&two_profile_aws_toml(&fx));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"profile-ghost-plain");
    clear_calls(&fx);

    let (code, _, stderr) = fx.list_env("files", &["--profile", "ghost"], &[]);
    assert_eq!(code, 2, "list files --profile ghostは設定エラー2: {stderr}");
    let (code, _, stderr) = fx.push(&[&a], &["--profile", "ghost"], &[]);
    assert_eq!(
        code, 2,
        "backup push --profile ghostは設定エラー2: {stderr}"
    );
    assert_eq!(
        fx.calls(),
        "",
        "S3 API呼び出し前に検出(calls.log空): {}",
        fx.calls()
    );
}

/// [covers:cli.profile-no-writeback]
/// --profileは一時上書きでありaws.tomlファイルへは書戻さない
/// (実行後も[aws].active_profileの値は不変。正本はaws.toml)。
#[test]
fn profile_option_does_not_write_back_to_aws_toml() {
    let Some(fx) = StorageFixture::new("pb-profnowrite", StorageOpts::default()) else {
        return;
    };
    let body = two_profile_aws_toml(&fx);
    fx.rewrite_aws_toml(&body);
    let before = std::fs::read(&fx.aws_toml).expect("aws.toml読込");

    let a = fx.data_root.join("a.zip");
    write_file(&a, b"profile-nowrite-plain");
    assert_eq!(fx.push(&[&a], &["--profile", "prod"], &[]).0, 0);
    let _ = fx.list("files", &["--profile", "prod"]);

    let after = std::fs::read(&fx.aws_toml).expect("aws.toml再読込");
    assert_eq!(before, after, "aws.tomlの内容はbyte単位で同一(書戻さない)");
}

/// [covers:cli.aws-config-default-path]
/// --aws-config省略時の既定pathは configs/aws.toml(cwd基準の相対path・L-10)。
/// 作業dirに配置した場合は読み込まれて動作し、配置しない作業dirでは設定エラー2。
#[test]
fn aws_config_default_path_is_configs_aws_toml() {
    let Some(fx) = StorageFixture::new("pb-awsdef", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"aws-default-plain");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);

    // 作業dirへ configs/aws.toml を配置(fake stateとcredentialを共用)
    let work = fx.elsewhere.join("awsdef-work");
    let configs_dir = work.join("configs");
    std::fs::create_dir_all(&configs_dir).expect("configs dir");
    std::fs::copy(&fx.aws_toml, configs_dir.join("aws.toml")).expect("aws.toml配置");
    let state_s = fx.state.display().to_string();
    let key_s = fx.secret.display().to_string();
    let env = [("BACKUP_REMOTE_FAKE_S3_STATE", state_s.as_str())];

    // --aws-config未指定のlist files: configs/aws.tomlを読み込んで索引参照まで動作
    let (code, stdout, stderr) = run_bin_cwd(&["list", "files", "--key", &key_s], &env, &work);
    assert_eq!(
        code, 0,
        "--aws-config省略時はconfigs/aws.tomlを読み込む: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("a.zip") || stdout.contains("unfiled"),
        "索引の内容が表示される: {stdout}"
    );

    // --aws-config未指定のbackup push(配置した作業dir側)
    let b_src = work.join("b.zip");
    write_file(&b_src, b"aws-default-b");
    let (code, stdout, stderr) = run_bin_cwd(
        &[
            "backup",
            "push",
            "--config",
            fx.cfg.display().to_string().as_str(),
            "b.zip",
        ],
        &env,
        &work,
    );
    assert_eq!(
        code, 0,
        "backup pushもconfigs/aws.tomlで動作: {stdout}{stderr}"
    );

    // configs/aws.tomlを配置しない作業dirでは設定エラー2
    let bare = fx.elsewhere.join("awsdef-bare");
    std::fs::create_dir_all(&bare).expect("bare dir");
    let (code, _, stderr) = run_bin_cwd(&["list", "files", "--key", &key_s], &env, &bare);
    assert_eq!(
        code, 2,
        "configs/aws.toml不在の作業dirは設定エラー2: {stderr}"
    );
}

/// [covers:cli.index-get-transfer-error]
/// 照会系の最初の索引GET(GetObject)時のS3転送エラーは転送エラー5(TASK-11 Phase B)。
/// 索引404(不在)・復号失敗の設定エラー2(cli.index-missing-error)と分離する。
#[test]
fn index_get_transfer_error_is_exit_5() {
    let Some(fx) = StorageFixture::new("pb-idxget", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"index-get-plain");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let index_key = fx.index_key();

    // 索引objectは存在する状態で索引GETへ失敗注入
    let (code, _, stderr) =
        fx.list_env("files", &[], &[("FAKE_S3_FAIL_CP_FOR", index_key.as_str())]);
    assert_eq!(
        code, 5,
        "索引GET転送エラーは転送エラー5(list files): {stderr}"
    );
    let out = fx.elsewhere.join("idxget-out");
    let (code, _, stderr) = fx.get_env(
        "a.zip",
        &["--out", out.display().to_string().as_str()],
        &[("FAKE_S3_FAIL_CP_FOR", index_key.as_str())],
    );
    assert_eq!(code, 5, "索引GET転送エラーは転送エラー5(get): {stderr}");

    // 索引不在(404)は設定エラー2と区別される
    std::fs::remove_file(fx.object_path(&index_key)).expect("索引object削除");
    let (code, _, stderr) = fx.list("files", &[]);
    assert_eq!(code, 2, "索引不在(404)は設定エラー2: {stderr}");
}

/// [covers:cli.manage-jobs-reissue]
/// manage jobs reissue <対象>(TASK-11 Phase B): 対象objectの復元jobを再発行する
/// (tier変更可。--tier指定時は当該tier・省略時は既定Standard)。tierはStandard/Bulkの
/// allowlist検査(allowlist外はusage error 64)。復元済み・進行中の対象にも再発行できる。
#[test]
fn manage_jobs_reissue_reissues_restore_job() {
    let Some(fx) = StorageFixture::new(
        "pb-reissue",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.data_root.join("b.zip");
    write_file(&a, b"reissue-a");
    write_file(&b, b"reissue-b");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    assert_eq!(fx.push(&[&b], &[], &[]).0, 0);
    let key_a = fx.file_key("unfiled", None, "a.zip");
    let key_b = fx.file_key("unfiled", None, "b.zip");
    // aは復元済み・bは進行中
    fx.seed_restore(&key_a, "restored");
    fx.seed_restore(&key_b, "ongoing");
    clear_calls(&fx);

    // --tier Bulk: 復元済み対象へtier=Bulkで再発行
    let (code, stdout, stderr) = fx.manage("jobs", &["reissue", &key_a, "--tier", "Bulk"]);
    assert_eq!(
        code, 0,
        "manage jobs reissue --tier Bulkはexit 0: {stdout}{stderr}"
    );
    let calls = fx.calls();
    let ro_a = calls
        .lines()
        .find(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={key_a}")))
        .unwrap_or_else(|| panic!("a.zipへのRestoreObject再発行: {calls}"));
    assert!(
        ro_a.contains("tier=Bulk"),
        "tier=Bulkで再発行(保持期間延長・tier切替): {ro_a}"
    );

    // 進行中対象にも再発行できる
    let (code, stdout, stderr) = fx.manage("jobs", &["reissue", &key_b]);
    assert_eq!(code, 0, "進行中対象のreissueもexit 0: {stdout}{stderr}");
    let calls = fx.calls();
    let ro_b = calls
        .lines()
        .find(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={key_b}")))
        .unwrap_or_else(|| panic!("b.zipへのRestoreObject再発行: {calls}"));
    assert!(
        ro_b.contains("tier=Standard"),
        "--tier省略は既定Standard: {ro_b}"
    );

    // --tier Expedited(allowlist外)はusage error 64・RestoreObject発行なし
    clear_calls(&fx);
    let (code, _, stderr) = fx.manage("jobs", &["reissue", &key_a, "--tier", "Expedited"]);
    assert_eq!(
        code, 64,
        "--tier Expeditedはallowlist外でusage error 64: {stderr}"
    );
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 RestoreObject")),
        "allowlist外はRestoreObjectを発行しない: {}",
        fx.calls()
    );
}
