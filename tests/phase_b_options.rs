//! TASK-11(Phase B)のCLI option系結合test: --profile(aws.toml active_profile一時上書き)・
//! --aws-config既定path・索引GET転送エラーのexit 5分離・manage jobs reissue。
//! fake seam(AMAZING_BACKUP_FAKE_S3_STATE)のbin subprocessで検証する
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
        "[aws]\nactive_profile = \"dev\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n\n[aws.profiles.prod]\npurpose = \"prod\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"prod-remote/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n",
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
    // (新規発行はtitle必須のためpty prompt・設計6.1)
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--profile", "prod"]) else {
        return;
    };
    assert_eq!(code, 0, "backup push --profile prodはexit 0: {stdout}");
    let prod_uuid = uuid_of_bucket_upload(&fx, "it-bucket");
    let prod_key = format!("prod-remote/data/{prod_uuid}/default.age");
    assert!(
        fx.object_path(&prod_key).is_file(),
        "object keyはprod側prefix: {}",
        fx.object_path(&prod_key).display()
    );
    assert!(
        fx.object_path(&prod_key).is_file(),
        "object keyはprod側prefix: {}",
        fx.object_path(&prod_key).display()
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
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--profile", "prod"]) else {
        return;
    };
    assert_eq!(code, 0, "prod push: {stdout}");
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
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");

    // 作業dirへ configs/aws.toml を配置(fake stateとcredentialを共用)
    let work = fx.elsewhere.join("awsdef-work");
    let configs_dir = work.join("configs");
    std::fs::create_dir_all(&configs_dir).expect("configs dir");
    std::fs::copy(&fx.aws_toml, configs_dir.join("aws.toml")).expect("aws.toml配置");
    let state_s = fx.state.display().to_string();
    let key_s = fx.secret.display().to_string();
    let env = [("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str())];

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

    // --aws-config未指定のbackup push(配置した作業dir側。新規発行のtitle必須のため
    // from-listのtitle欄で与え、positionalにはskip対象のa.zipを添える・設計6.1)
    let b_src = work.join("b.zip");
    write_file(&b_src, b"aws-default-b");
    let list = write_from_list(&work, &[(&b_src, Some("b-title"))]);
    let (code, stdout, stderr) = run_bin_cwd(
        &[
            "backup",
            "push",
            "--config",
            fx.cfg.display().to_string().as_str(),
            a.display().to_string().as_str(),
            "--from-list",
            list.display().to_string().as_str(),
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
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
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

/// dev(active_profile)とprodが同一prefix・別bucketの2profile aws.toml本文
/// (cli.profile-namespace-isolated-by-bucket検証用。prefixが同一である点が本質)。
fn same_prefix_diff_bucket_aws_toml(fx: &StorageFixture) -> String {
    format!(
        "[aws]\nactive_profile = \"dev\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"bucket-dev\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n\n[aws.profiles.prod]\npurpose = \"prod\"\nbucket = \"bucket-prod\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n",
        creds = fx.credentials.display()
    )
}

/// 同一prefix・別bucketのfile_upload記録(version_id)をregistryから読む。
fn upload_ids_for_bucket(fx: &StorageFixture, s3_key: &str, bucket: &str) -> Vec<String> {
    fx.registry_values()
        .iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                && r.get("s3_key").and_then(|k| k.as_str()) == Some(s3_key)
                && r.get("s3_bucket").and_then(|b| b.as_str()) == Some(bucket)
        })
        .filter_map(|r| {
            r.get("version_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect()
}

/// [covers:cli.profile-namespace-isolated-by-bucket]
/// 同一prefix・別bucketの複数profile構成(H-1): namespace分離はregistry記録の
/// s3_bucket一致で行われ、prefix一致のみの他方bucket実績には(a)push skip判定
/// (b)索引versions構築 (c)索引不在時の未書込namespace例外 のいずれも惑わされない。
/// bucket一致規則はregistry::record_bucket_matchedと同一(記録s3_bucket欠損の旧記録は
/// 正本bucket=active_profile相当として扱う)。
#[test]
fn profile_namespace_isolated_by_bucket() {
    let Some(fx) = StorageFixture::new("pb-nsbkt", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_aws_toml(&same_prefix_diff_bucket_aws_toml(&fx));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"ns-by-bucket-plain");

    // (c) 索引不在時の未書込namespace例外はbucket単位で機能する:
    // devの書込実績(同一prefix)があってもprodから見て未書込なら0件+exit 0
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "dev push: {stdout}");
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    let (code, stdout, stderr) = fx.list_env("files", &["--profile", "prod"], &[]);
    assert_eq!(
        code, 0,
        "prod照会は未書込namespaceとして0件+exit 0: {stdout}{stderr}"
    );
    assert!(!stdout.contains("a.zip"), "prod側の対象は0件: {stdout}");
    let (code, _, stderr) = fx.list("files", &[]);
    assert_eq!(
        code, 2,
        "dev(書込実績ありbucket)の照会は索引不在で設定エラー2: {stderr}"
    );

    // (a) dev実績(prefix一致・内容同一)ではprod側pushはskipされない
    // (bucket横断で1byteもuploadされないrunを作らない。file_write一致でもbucket一致の
    // file_uploadが無いため(2.5)再PUT経路でuploadされる・設計4.1(1)/(2.5))
    clear_calls(&fx);
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--profile", "prod"]) else {
        return;
    };
    assert_eq!(code, 0, "prod pushはexit 0: {stdout}");
    assert!(
        !stdout.lines().any(|l| l.starts_with("skip")),
        "dev実績によるskipは起こらない(末尾集計のskip=0は除外): {stdout}"
    );
    let key = fx.data_key(&uuid_of_path(&fx, &a));
    assert!(
        fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}"))),
        "file keyへのPUTが行われる: {}",
        fx.calls()
    );

    // (b) 索引versionsにはprod bucketのfile_upload記録のみが載る(dev版の混入なし)
    let dev_ids = upload_ids_for_bucket(&fx, &key, "bucket-dev");
    let prod_ids = upload_ids_for_bucket(&fx, &key, "bucket-prod");
    assert_eq!(dev_ids.len(), 1, "dev実績1件: {dev_ids:?}");
    assert_eq!(prod_ids.len(), 1, "prod実績1件: {prod_ids:?}");
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert_eq!(vids.len(), 1, "索引versionsはprod版のみ: {vids:?}");
    assert!(
        vids.contains(&prod_ids[0]),
        "prod版は索引へ載る: {vids:?} vs {prod_ids:?}"
    );
    assert!(
        !vids.contains(&dev_ids[0]),
        "他方bucket(dev)の版は索引へ混入しない: {vids:?} vs {dev_ids:?}"
    );
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
    for p in [&a, &b] {
        let Some((code, stdout)) =
            push_titled_pty(&fx, &[p], &p.file_name().unwrap().to_string_lossy(), &[])
        else {
            return;
        };
        assert_eq!(code, 0, "初回push: {stdout}");
    }
    let key_a = fx.data_key(&uuid_of_path(&fx, &a));
    let key_b = fx.data_key(&uuid_of_path(&fx, &b));
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

/// registry.jsonlのfile_upload記録のs3_bucketを空文字へ書き換える
/// (フィールド追加前の旧形式記録の模倣。L-R3/L-R4検証用)。
fn rewrite_file_upload_buckets_empty(fx: &StorageFixture) {
    let mut body = String::new();
    for r in fx.registry_values() {
        if r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload") {
            let mut v = r.clone();
            v["s3_bucket"] = serde_json::json!("");
            body.push_str(&serde_json::to_string(&v).expect("registry行serialize"));
        } else {
            body.push_str(&serde_json::to_string(&r).expect("registry行serialize"));
        }
        body.push('\n');
    }
    write_file(&fx.registry, body.as_bytes());
}

/// [covers:cli.profile-namespace-isolated-by-bucket]
/// 旧形式file_upload記録(s3_bucket空文字=フィールド追加前の記録相当)は正本bucket
/// (active_profile)相当として扱われる(L-R3/L-R4: 旧記録分岐の結合test検証)。
/// 実効bucket=正本の再pushはskip判定に採用され(旧記録は正本bucket扱い)、
/// 実効bucket≠正本(--profile prod)のpushではskipされずuploadされる。
#[test]
fn legacy_empty_bucket_upload_record_treated_as_canonical() {
    let Some(fx) = StorageFixture::new("pb-legacybkt", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_aws_toml(&same_prefix_diff_bucket_aws_toml(&fx));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"legacy-record-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "dev初回push: {stdout}");
    let key = fx.data_key(&uuid_of_path(&fx, &a));
    rewrite_file_upload_buckets_empty(&fx);

    // 実効bucket=正本(bucket-dev): 旧記録は正本bucket扱いのためskipされる
    clear_calls(&fx);
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "dev再pushはexit 0: {stdout}{stderr}");
    assert!(
        stdout.lines().any(|l| l.starts_with("skip")),
        "旧記録(正本bucket扱い)でskipされる: {stdout}"
    );
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}"))),
        "file keyへのPUTは行われない: {}",
        fx.calls()
    );

    // 実効bucket≠正本(--profile prod): 旧記録は正本bucket扱いのため一致せずuploadされる
    // (file_write一致でもbucket一致のfile_uploadが無いため(2.5)再PUT経路・設計4.1)
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--profile", "prod"]) else {
        return;
    };
    assert_eq!(code, 0, "prod pushはexit 0: {stdout}");
    assert!(
        !stdout.lines().any(|l| l.starts_with("skip")),
        "旧記録ではprod側はskipされない: {stdout}"
    );
    assert!(
        fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}"))),
        "file keyへのPUTが行われる: {}",
        fx.calls()
    );
}

/// 索引file entryのpins[]のpin名一覧。
fn index_pin_names(entry: &serde_json::Value) -> Vec<String> {
    entry
        .get("pins")
        .and_then(|p| p.as_array())
        .map(|a| {
            a.iter()
                .map(|p| {
                    p.get("pin")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// [covers:pin.pin-resolution-bucket-isolated]
/// [covers:index.pins-bucket-isolated]
/// 同一prefix・別bucket構成でのpinのnamespace分離(M-R1): 索引pins[]はbucket一致する
/// pin_write/pin_remove記録のみで構築され、get --pin・pin rmの解決も当該bucketの
/// 索引に限る(誤profileではpin名不在のexit 3・正bucket側の索引pins[]とpin objectは不変)。
#[test]
fn pin_namespace_isolated_by_bucket() {
    let Some(fx) = StorageFixture::new("pb-pinbkt", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_aws_toml(&same_prefix_diff_bucket_aws_toml(&fx));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-bucket-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "dev初回push: {stdout}");
    let dev_uuid = uuid_of_path(&fx, &a);
    assert_eq!(fx.pin("add", "a.zip", &["--as", "pm1"]).0, 0);
    assert_eq!(
        index_pin_names(index_entity_by(&fx.read_index(), "title", "a.zip")),
        vec!["pm1"],
        "dev側索引はpm1を保持"
    );

    // prod側push(bucket分離で別entity発行・title必須)で再構築された索引にdev側pinは混入しない
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--profile", "prod"]) else {
        return;
    };
    assert_eq!(code, 0, "prod push: {stdout}");
    let pins = index_pin_names(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert!(
        pins.is_empty(),
        "prod側索引pins[]に他方bucketのpinは混入しない: {pins:?}"
    );

    // get --pin(誤profile): pin名不在の対象エラー3・pin objectへの取得も行わない
    let pin_key = fx.data_pin_key(&dev_uuid, "pm1");
    let out = fx.elsewhere.join("pin-ns-out");
    clear_calls(&fx);
    let (code, _, stderr) = fx.get_env(
        "a.zip",
        &[
            "--pin",
            "pm1",
            "--profile",
            "prod",
            "--out",
            out.display().to_string().as_str(),
        ],
        &[],
    );
    assert_eq!(
        code, 3,
        "get --pin pm1 --profile prodはpin名不在の対象エラー3: {stderr}"
    );
    assert!(
        stderr.contains("pm1") && stderr.contains("索引にありません"),
        "pin名不在の表示: {stderr}"
    );
    assert!(
        !fx.calls().lines().any(|l| l.contains(&pin_key)),
        "pin objectへのS3 API呼び出し(取得)は行わない: {}",
        fx.calls()
    );

    // pin rm(誤profile): pin名不在の対象エラー3・削除も行わない
    clear_calls(&fx);
    let (code, _, stderr) = fx.pin_env("rm", "a.zip", &["--pin", "pm1", "--profile", "prod"], &[]);
    assert_eq!(
        code, 3,
        "pin rm --pin pm1 --profile prodはpin名不在の対象エラー3: {stderr}"
    );
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 DeleteObjectVersion")),
        "pin objectの削除は行わない: {}",
        fx.calls()
    );
    assert!(
        fx.object_path(&pin_key).is_file(),
        "pin objectは不変(誤profile操作では削除されない)"
    );

    // dev側(正bucket)はpm1を保持したまま(all-skip pushでの再構築でも消えない)
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    assert_eq!(
        index_pin_names(index_entity_by(&fx.read_index(), "title", "a.zip")),
        vec!["pm1"],
        "dev側索引のpm1は誤profile操作後も保持される"
    );
}

/// [covers:index.entries-without-bucket-versions-excluded]
/// 当該bucketのversions[]・pins[]とも無いentry(他方bucketにしかuploadされていない
/// file)は索引へ載らない(L-R1): prod側のlist/query/get/diffにb.zip(devのみ)は
/// 現れず、dev側(正本bucket)の照会は従来どおり双方が現れる。
#[test]
fn entries_without_bucket_versions_excluded_from_query() {
    let Some(fx) = StorageFixture::new("pb-orphan", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_aws_toml(&same_prefix_diff_bucket_aws_toml(&fx));
    let a = fx.data_root.join("a.zip");
    let b = fx.data_root.join("b.zip");
    write_file(&a, b"orphan-a-plain");
    write_file(&b, b"orphan-b-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a, &b], "a.zip\nb.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "dev push: {stdout}");
    // a.zipのみprod側へもpush(prod索引が再構築される。bucket分離で別entity発行のため
    // title必須・設計6.1)
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--profile", "prod"]) else {
        return;
    };
    assert_eq!(code, 0, "prod push: {stdout}");

    // prod側: a.zipのみ現れ、b.zip(devのみ)は現れない(--treeは廃止・設計6.5)
    let (code, stdout, stderr) = fx.list_env("files", &["--profile", "prod"], &[]);
    assert_eq!(code, 0, "prod側list filesはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("a.zip"),
        "prod側にもa.zipは現れる: {stdout}"
    );
    assert!(
        !stdout.contains("b.zip"),
        "版の無いb.zipは表示されない: {stdout}"
    );

    let (code, stdout, stderr) = fx.query("b.zip", &["--profile", "prod"]);
    assert_eq!(code, 0, "prod側queryはexit 0: {stdout}{stderr}");
    assert!(stdout.contains("0件"), "prod側queryはb.zipに0件: {stdout}");

    let (code, _, stderr) = fx.get_env("b.zip", &["--profile", "prod"], &[]);
    assert_eq!(code, 3, "prod側get b.zipはtitle不在の対象エラー3: {stderr}");
    assert!(
        stderr.contains("索引にtitle/uuid=b.zip がありません"),
        "title不在の表示: {stderr}"
    );

    let (code, stdout, stderr) = fx.diff(&["--profile", "prod"]);
    assert_eq!(code, 0, "prod側diffはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("entries=1"),
        "diffのentries数は当該namespaceのfileのみ: {stdout}"
    );

    // dev側(正本bucket): 従来どおり双方が現れる(all-skip pushでdev索引を再構築)
    assert_eq!(fx.push(&[&a, &b], &[], &[]).0, 0);
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "dev側list filesはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("a.zip") && stdout.contains("b.zip"),
        "dev側は双方が現れる: {stdout}"
    );
}

/// dev(active_profile)とprod系family/bakedが同一prefix・別bucketの3profile aws.toml本文
/// (cli.profile-multi-bucket-isolation・cli.backup-push-connection-dest-shown検証用。
/// prod系3bucket構成(your-family-bucket/baked/atara)で同一prefix運用を模した配置)。
fn three_bucket_aws_toml(fx: &StorageFixture) -> String {
    let creds = fx.credentials.display();
    format!(
        "[aws]\nactive_profile = \"dev\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n\
         [aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"bucket-dev\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n\n\
         [aws.profiles.family]\npurpose = \"prod\"\nbucket = \"bucket-family\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"DEEP_ARCHIVE\"\ncredentials_file = \"{creds}\"\n\n\
         [aws.profiles.baked]\npurpose = \"prod\"\nbucket = \"bucket-baked\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"DEEP_ARCHIVE\"\ncredentials_file = \"{creds}\"\n"
    )
}

/// [covers:cli.profile-multi-bucket-isolation]
/// 3bucket以上のprofile構成(dev + prod系family/baked。同一prefix・別bucket): bucket単位の
/// namespace分離はprofile数に依存せず機能する(H-1のbucket一致規則の3bucket版)。
/// (a) 他bucket実績(prefix一致・内容同一)ではskipされない (b) 索引versionsには
/// 当該bucketのfile_upload記録のみが載る(3bucket分の実績が同一registryに混在しても
/// dev/family版はbaked索引へ混入しない)。
#[test]
fn multi_bucket_profiles_isolated() {
    let Some(fx) = StorageFixture::new("pb-3bkt", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_aws_toml(&three_bucket_aws_toml(&fx));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"three-bucket-plain");

    // dev(正本) -> family -> baked の順に同一内容をpush(新規発行はtitle必須のためpty。
    // 2回目以降はfile_write一致でもbucket一致のfile_uploadが無いため(2.5)再PUT経路で
    // uploadされる・設計4.1(1)/(2.5)。bucket横断で1byteもuploadされないrunを作らない)
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "dev push: {stdout}");
    for profile in ["family", "baked"] {
        let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--profile", profile])
        else {
            return;
        };
        assert_eq!(code, 0, "{profile} pushはexit 0: {stdout}");
        assert!(
            !stdout.lines().any(|l| l.starts_with("skip")),
            "{profile}側は前bucket実績によりskipされない: {stdout}"
        );
    }

    // registry: 3bucketそれぞれへfile_upload記録1件ずつ(同一uuidへbucket毎の版)
    let key = fx.data_key(&uuid_of_path(&fx, &a));
    let dev_ids = upload_ids_for_bucket(&fx, &key, "bucket-dev");
    let family_ids = upload_ids_for_bucket(&fx, &key, "bucket-family");
    let baked_ids = upload_ids_for_bucket(&fx, &key, "bucket-baked");
    assert_eq!(dev_ids.len(), 1, "dev実績1件: {dev_ids:?}");
    assert_eq!(family_ids.len(), 1, "family実績1件: {family_ids:?}");
    assert_eq!(baked_ids.len(), 1, "baked実績1件: {baked_ids:?}");
    assert_ne!(
        dev_ids[0], family_ids[0],
        "dev版とfamily版は別(同一内容でもbucket毎にupload)"
    );
    assert_ne!(
        family_ids[0], baked_ids[0],
        "family版とbaked版は別(prod系相互でもbucket毎にupload)"
    );

    // 索引versionsはbaked(最終再構築)版のみ。dev/family版は混入しない
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert_eq!(vids.len(), 1, "索引versionsはbaked版のみ: {vids:?}");
    assert!(
        vids.contains(&baked_ids[0]),
        "baked版は索引へ載る: {vids:?} vs {baked_ids:?}"
    );
    assert!(
        !vids.contains(&dev_ids[0]) && !vids.contains(&family_ids[0]),
        "他bucket(dev/family)の版は索引へ混入しない: {vids:?}"
    );
}

/// [covers:cli.backup-push-connection-dest-shown]
/// backup pushは実行開始時に接続先(実効profile・bucket・prefix)を表示する(3prod bucket
/// 構成の混同防止: 誤profile・誤bucketへの書き込みに気づかせる)。--dry-runでも
/// 表示され(S3 API呼び出しに先行)、dry-runではS3 API呼び出しは行われない。
#[test]
fn backup_push_shows_connection_destination() {
    let Some(fx) = StorageFixture::new("pb-dest", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_aws_toml(&three_bucket_aws_toml(&fx));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"connection-dest-plain");

    // dry-run: 接続先行が表示され、S3 API呼び出しは行われない
    clear_calls(&fx);
    let (code, stdout, stderr) = fx.push(&[&a], &["--dry-run", "--profile", "baked"], &[]);
    assert_eq!(code, 0, "dry-runはexit 0: {stdout}{stderr}");
    assert!(
        stdout.lines().any(|l| l.contains("接続先")
            && l.contains("profile=baked")
            && l.contains("bucket=bucket-baked")
            && l.contains("prefix=amazing-backup/")),
        "dry-runにも接続先行(実効profile/bucket/prefix)が表示される: {stdout}"
    );
    assert!(
        fx.calls().is_empty(),
        "dry-runはS3 API呼び出しを行わない: {}",
        fx.calls()
    );

    // 通常実行(--profile指定): 指定profileが表示される(新規発行はtitle必須のためpty)
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--profile", "family"])
    else {
        return;
    };
    assert_eq!(code, 0, "family pushはexit 0: {stdout}");
    assert!(
        stdout.lines().any(|l| l.contains("接続先")
            && l.contains("profile=family")
            && l.contains("bucket=bucket-family")
            && l.contains("prefix=amazing-backup/")),
        "通常実行にも接続先行が表示される: {stdout}"
    );

    // 通常実行(profile省略): active_profile(dev)が表示される(all-skip)
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "既定profile pushはexit 0: {stdout}{stderr}");
    assert!(
        stdout.lines().any(|l| l.contains("接続先")
            && l.contains("profile=dev")
            && l.contains("bucket=bucket-dev")
            && l.contains("prefix=amazing-backup/")),
        "profile省略時はactive_profile(dev)の接続先が表示される: {stdout}"
    );
}
