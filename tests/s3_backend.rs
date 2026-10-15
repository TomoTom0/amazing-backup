//! S3 backend固有の結合test(TASK-9 AWS SDK移行・tmp/tasks/tm-9/design_sdk_migration.md)。
//! fake seam(env BACKUP_REMOTE_FAKE_S3_STATE・設計4章)・multipart(5章)・
//! credentialファイル(3章)・AWS_ENDPOINT_URL系env警告(9.3)をbin subprocess実行で検証する。
//! TASK-11(Phase B)で旧uploadサブコマンドが削除され、S3転送はbackup push経路で検証する。

mod common;

use common::*;

/// 64 MiB(multipart閾値・MULTIPART_THRESHOLD_BYTES。設計5.3)。
const MIB: usize = 1024 * 1024;
const THRESHOLD: usize = 64 * MIB;

/// calls.logを空にする(以降の呼び出しのみを観測する)。
fn clear_calls(fx: &StorageFixture) {
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
}

/// credentialファイルの内容を書き換える(schema検証・非露出検証用)。
fn rewrite_credentials(fx: &StorageFixture, body: &str) {
    write_file(&fx.credentials, body.as_bytes());
}

/// profileを1つだけ含むaws.toml本文(credentials_fileは引数)。
fn aws_toml_with(credentials_file: &str) -> String {
    format!(
        "[aws]\nactive_profile = \"dev\"\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"backup-remote/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{credentials_file}\"\n"
    )
}

/// [covers:cli.fake-s3-seam]
/// fake seam(TASK-9設計4.3案1・12.4): env BACKUP_REMOTE_FAKE_S3_STATE指定でS3 backendが
/// fakeへ差し替わる。ダミーcredential値(実AWSで無効)でもbackup pushが成功する=実network不使用。
/// fake選択時はstderr冒頭へ「WARNING: FAKE S3 MODE」が表示される(誤用防御)。
/// state dir配下へobjects/<key>とAPI呼び出し記録(calls.log)が書かれる。
#[test]
fn fake_seam_uses_fake_backend_with_warning() {
    let Some(fx) = StorageFixture::new("sb-seam", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"seam-plain");
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(
        code, 0,
        "fake seamでbackup push成功(ダミーcredentialでも=実network不使用): {stdout}{stderr}"
    );
    assert!(
        stderr.contains("WARNING: FAKE S3 MODE"),
        "fake選択時のWARNING表示: {stderr}"
    );
    let obj = fx.object_path(&fx.file_key("unfiled", None, "a.zip"));
    assert!(obj.is_file(), "state dirへobject配置: {}", obj.display());
    let calls = fx.calls();
    assert!(
        calls.lines().any(|l| l.starts_with("s3 PutObject")
            && l.contains("key=")
            && l.contains("bucket=it-bucket")),
        "API呼び出し記録(calls.log)の生成: {calls}"
    );
}

/// [covers:config.aws-credentials-file-required]
/// [aws.profiles.X].credentials_fileキー(credentialファイルpath)は必須キー。
/// キー欠落・空値は設定エラー2でS3 API呼び出し前に弾かれる(明示API key要件・書き漏れの即検出)。
#[test]
fn credentials_file_key_required() {
    let Some(fx) = StorageFixture::new("sb-credreq", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"cred-req-plain");
    // キー欠落
    fx.rewrite_aws_toml(
        "[aws]\nactive_profile = \"dev\"\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"backup-remote/\"\nstorage_class = \"STANDARD\"\n",
    );
    clear_calls(&fx);
    let (code, _, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 2, "credentials_fileキー欠落は設定エラー2: {stderr}");
    assert_eq!(fx.calls(), "", "S3 API呼び出しは行われない");
    // 空値
    fx.rewrite_aws_toml(&aws_toml_with(""));
    let (code, _, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 2, "credentials_file空値は設定エラー2: {stderr}");
}

/// [covers:config.credentials-file-schema]
/// credentialファイルschema(設計3.1・3.2): [credentials]配下access_key_id/
/// secret_access_key必須・session_token省略可・未知キー拒否・空値・前後空白付き値は
/// 設定エラー2。正しい形式(session_token省略)はbackup pushまで成功する。
#[test]
fn credentials_file_schema_validation() {
    let Some(fx) = StorageFixture::new("sb-credschema", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"cred-schema-plain");
    let cfg_s = fx.cfg.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let a_s = a.display().to_string();

    // 正しい形式(session_token省略): backup push成功(S3 API呼び出しまで到達=fake seam)
    rewrite_credentials(
        &fx,
        "[credentials]\naccess_key_id = \"AKITEST0000000000000A\"\nsecret_access_key = \"test-secret-value-not-real\"\n",
    );
    let (code, _, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "session_token省略の正しい形式は成功: {stderr}");
    assert!(!fx.calls().is_empty(), "S3 API呼び出しまで到達");

    // 不正系はすべて設定エラー2(load_credentials検査=RealS3構築経路・seam無し。
    // S3 API呼び出し前に設定エラーで停止)
    for (tag, body) in [
        ("不正TOML", "[credentials\nbroken"),
        (
            "[credentials]テーブル不在",
            "[other]\naccess_key_id = \"x\"\n",
        ),
        (
            "access_key_id欠落",
            "[credentials]\nsecret_access_key = \"s\"\n",
        ),
        (
            "secret_access_key欠落",
            "[credentials]\naccess_key_id = \"a\"\n",
        ),
        (
            "access_key_id空値",
            "[credentials]\naccess_key_id = \"\"\nsecret_access_key = \"s\"\n",
        ),
        (
            "secret_access_key空値",
            "[credentials]\naccess_key_id = \"a\"\nsecret_access_key = \"\"\n",
        ),
        (
            "前後空白付き値",
            "[credentials]\naccess_key_id = \" AKI1 \"\nsecret_access_key = \"s\"\n",
        ),
        (
            "未知キー",
            "[credentials]\naccess_key_id = \"a\"\nsecret_access_key = \"s\"\ntoken = \"x\"\n",
        ),
    ] {
        rewrite_credentials(&fx, body);
        let (code, _, stderr) = run_bin(&[
            "backup",
            "push",
            "--config",
            &cfg_s,
            "--aws-config",
            &aws_s,
            &a_s,
        ]);
        assert_eq!(code, 2, "credentialファイル{tag}は設定エラー2: {stderr}");
    }
}

/// [covers:config.credentials-fail-closed]
/// fail-closed(設計3.5): credentials_fileが指すファイル不在は設定エラー2で、
/// credential chain(IMDS/env/~/.aws)へのfallbackは行われない(別環境credentialでの
/// 意図しない接続を構造的に防止)。
#[test]
fn credentials_fail_closed_no_fallback() {
    let Some(fx) = StorageFixture::new("sb-credno", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"fail-closed-plain");
    fx.rewrite_aws_toml(&aws_toml_with("/nonexistent/aws-credentials.toml"));
    clear_calls(&fx);
    // fake seam無し(=RealS3構築経路)でload_credentials検査を駆動
    let (code, _, stderr) = run_bin(&[
        "backup",
        "push",
        "--config",
        fx.cfg.display().to_string().as_str(),
        "--aws-config",
        fx.aws_toml.display().to_string().as_str(),
        a.display().to_string().as_str(),
    ]);
    assert_eq!(
        code, 2,
        "credentialファイル不在は設定エラー2(fallbackしない): {stderr}"
    );
    assert_eq!(fx.calls(), "", "S3 API呼び出しは行われない");
}

/// [covers:config.credentials-value-not-leaked]
/// credential値非露出(設計3.6・12.4): 設定エラー時・正常完了時・転送エラー時の
/// いずれのstdout/stderrにもcredential値(検出マーカー)が現れない。
/// error文はcredentialファイルのpathのみを表示する。
#[test]
fn credentials_values_never_leaked_in_output() {
    let Some(fx) = StorageFixture::new("sb-credleak", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"leak-check-plain");

    // (a) 設定エラー経路(未知キーでexit 2・RealS3構築経路): 値が出力に現れない
    rewrite_credentials(
        &fx,
        "[credentials]\naccess_key_id = \"LEAKMARK-AKID-9f8e7d6c\"\nsecret_access_key = \"LEAKMARK-SECRET-5a4b3c2d1e\"\nunknown_key = \"LEAKMARK-UNKNOWN-11223344\"\n",
    );
    let (code, _, stderr) = run_bin(&[
        "backup",
        "push",
        "--config",
        fx.cfg.display().to_string().as_str(),
        "--aws-config",
        fx.aws_toml.display().to_string().as_str(),
        a.display().to_string().as_str(),
    ]);
    assert_eq!(code, 2, "未知キーで設定エラー2: {stderr}");
    assert!(
        !stderr.contains("LEAKMARK"),
        "設定エラー時のstderrへcredential値を出さない: {stderr}"
    );

    // (b) 正常完了経路(fake seam): 値が出力に現れない
    rewrite_credentials(
        &fx,
        "[credentials]\naccess_key_id = \"LEAKMARK-AKID-9f8e7d6c\"\nsecret_access_key = \"LEAKMARK-SECRET-5a4b3c2d1e\"\n",
    );
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "正常backup push: {stdout}{stderr}");
    assert!(
        !stdout.contains("LEAKMARK") && !stderr.contains("LEAKMARK"),
        "正常時の出力へcredential値を出さない: {stdout}{stderr}"
    );
    assert!(
        !fx.calls().contains("LEAKMARK"),
        "API呼び出し記録へcredential値を書かない: {}",
        fx.calls()
    );

    // (c) 転送エラー経路(失敗注入でexit 5): 値が出力に現れない(map_sdk_errorのmessage含む)
    let b = fx.data_root.join("b.zip");
    write_file(&b, b"leak-check-fail");
    let fail_key = fx.file_key("unfiled", None, "b.zip");
    let (code, _, stderr) = fx.push(&[&b], &[], &[("FAKE_S3_FAIL_CP_FOR", fail_key.as_str())]);
    assert_eq!(code, 5, "失敗注入で転送エラー5: {stderr}");
    assert!(
        !stderr.contains("LEAKMARK"),
        "転送エラー時のstderrへcredential値を出さない: {stderr}"
    );
}

/// [covers:upload.multipart-threshold]
/// [covers:upload.multipart-checksum-per-part]
/// multipart閾値(設計5.3)とchecksum指定(設計5.4): 64 MiB超の対象(暗号化済み.age)は
/// CreateMultipartUpload→UploadPart×N→CompleteMultipartUploadでuploadされ
/// (単発PutObjectは使わない)、CreateMultipartUploadと各UploadPartの両方へ
/// checksum_algorithm SHA256が指定される。閾値未満(索引・manifest)は単発PutObject。
/// 完成objectはローカルの.ageとbyte一致する。
#[test]
fn multipart_upload_above_threshold() {
    let Some(fx) = StorageFixture::new(
        "sb-multipart",
        StorageOpts {
            keep_artifacts: "always",
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    // 閾値(64MiB)を確実に超える対象(平文64MiB+1MiB → .ageも64MiB超)
    let big = fx.data_root.join("big.zip");
    make_archive(&big, THRESHOLD + MIB);
    let (code, stdout, stderr) = fx.push(&[&big], &[], &[]);
    assert_eq!(code, 0, "multipart backup push成功: {stdout}{stderr}");

    let key = fx.file_key("unfiled", None, "big.zip");
    // runの.age(keep_artifacts=alwaysで残る)
    let writes: Vec<_> = fx
        .registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .collect();
    assert_eq!(writes.len(), 1);
    let local_age = std::path::PathBuf::from(writes[0]["age_path"].as_str().unwrap());
    let age_bytes = std::fs::read(&local_age).unwrap();
    assert!(
        age_bytes.len() > THRESHOLD,
        "fixture対象は64MiB超であること: {} bytes",
        age_bytes.len()
    );
    let calls = fx.calls();

    // 閾値超keyはmultipart(Create→UploadPart×N→Complete)で・単発PutObjectは無し
    let create_line = calls
        .lines()
        .find(|l| l.starts_with("s3 CreateMultipartUpload") && l.contains(&format!("key={key}")))
        .unwrap_or_else(|| panic!("CreateMultipartUpload行: {calls}"));
    assert!(
        create_line.contains("storage_class=DEEP_ARCHIVE")
            && create_line.contains("checksum=SHA256"),
        "CreateMultipartUploadのstorage_class・checksum指定: {create_line}"
    );
    let parts: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 UploadPart") && l.contains(&format!("key={key}")))
        .collect();
    assert!(parts.len() >= 2, "UploadPartは2回以上: {calls}");
    for p in &parts {
        assert!(
            p.contains("checksum=SHA256"),
            "各UploadPartのchecksum指定: {p}"
        );
    }
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 CompleteMultipartUpload")
                && l.contains(&format!("key={key}"))),
        "CompleteMultipartUpload行: {calls}"
    );
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}"))),
        "閾値超keyへの単発PutObjectは無い: {calls}"
    );

    // 完成objectはローカル.ageとbyte一致(結合結果)
    assert_eq!(
        std::fs::read(fx.object_path(&key)).unwrap(),
        age_bytes,
        "multipart完成objectの結合一致"
    );

    // 閾値未満(索引)は単発PutObject(CreateMultipartUpload無し)
    let index_key = fx.index_key();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={index_key}"))),
        "索引は単発PutObject: {calls}"
    );
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("s3 CreateMultipartUpload")
                && l.contains(&format!("key={index_key}"))),
        "索引へCreateMultipartUploadは無い: {calls}"
    );
}

/// [covers:upload.multipart-abort-on-part-failure]
/// part失敗時abort(設計5.7・12.4): multipart送信中のpart失敗(失敗注入FAKE_S3_FAIL_CP_FOR)は
/// 転送エラー5とし、AbortMultipartUploadが発火する(不完全MPUの放置防止。
/// CompleteMultipartUploadは実行されない)。
#[test]
fn multipart_abort_on_part_failure() {
    let Some(fx) = StorageFixture::new(
        "sb-abort",
        StorageOpts {
            keep_artifacts: "always",
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let big = fx.data_root.join("big.zip");
    make_archive(&big, THRESHOLD + MIB);
    let key = fx.file_key("unfiled", None, "big.zip");
    let (code, stdout, stderr) = fx.push(&[&big], &[], &[("FAKE_S3_FAIL_CP_FOR", key.as_str())]);
    assert_eq!(code, 5, "part失敗は転送エラー5: {stdout}{stderr}");
    let calls = fx.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 AbortMultipartUpload") && l.contains(&format!("key={key}"))),
        "part失敗でAbortMultipartUpload発火: {calls}"
    );
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("s3 CompleteMultipartUpload")
                && l.contains(&format!("key={key}"))),
        "CompleteMultipartUploadは実行されない: {calls}"
    );
}

/// [covers:upload.endpoint-url-env-warning]
/// AWS_ENDPOINT_URL系env検出警告(設計9.3): AWS_ENDPOINT_URL_S3(またはAWS_ENDPOINT_URL)が
/// 設定された環境での実行は処理を継続し(exit 0)、stderrへ警告表示が出る
/// (SDKが当該envを解釈して接続先endpointを切替え得るための検出通知)。
#[test]
fn endpoint_url_env_warns_on_stderr() {
    for env_name in ["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"] {
        let Some(fx) = StorageFixture::new("sb-endpoint", StorageOpts::default()) else {
            return;
        };
        let a = fx.data_root.join("a.zip");
        write_file(&a, b"endpoint-plain");
        let (code, stdout, stderr) = fx.push(&[&a], &[], &[(env_name, "http://127.0.0.1:19000")]);
        assert_eq!(
            code, 0,
            "{env_name}設定時も処理継続(exit 0): {stdout}{stderr}"
        );
        assert!(
            stderr.contains("AWS_ENDPOINT_URL"),
            "{env_name}検出の警告表示: {stderr}"
        );
    }
}

/// [covers:cli.fake-s3-restore-validates-key-and-state]
/// fakeのRestoreObjectは実S3と同様の事前検査を行う(L-3):
/// (1)不在keyはNoSuchKey相当の転送エラー5 (2)archive系(GLACIER/DEEP_ARCHIVE/GLACIER_IR)
/// 以外のstorage_classはInvalidObjectState相当の転送エラー5 (3)archive系objectへの
/// 発行は従来どおり成功する。manage jobs reissue(restoreを直接駆動する経路)で検証する。
#[test]
fn fake_s3_restore_validates_key_and_archive_state() {
    // (1)(2) STORAGE_CLASS=STANDARDでpushしたobjectと不在key
    let Some(fx) = StorageFixture::new("sb-rstval", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"restore-validate-std");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let std_key = fx.file_key("unfiled", None, "a.zip");

    // 不在keyへの復元job発行はNoSuchKey相当の転送エラー5
    let ghost_key = "backup-remote/unfiled/ghost.zip/default.age";
    let (code, _, stderr) = fx.manage("jobs", &["reissue", ghost_key]);
    assert_eq!(code, 5, "不在keyへのreissueは転送エラー5: {stderr}");
    assert!(
        stderr.contains("存在しない") || stderr.contains("NoSuchKey"),
        "NoSuchKey相当の文言: {stderr}"
    );

    // STANDARD objectへの復元job発行はInvalidObjectState相当の転送エラー5
    let (code, _, stderr) = fx.manage("jobs", &["reissue", &std_key]);
    assert_eq!(code, 5, "STANDARD objectへのreissueは転送エラー5: {stderr}");
    assert!(
        stderr.contains("InvalidObjectState") || stderr.contains("archive"),
        "InvalidObjectState相当の文言: {stderr}"
    );

    // (3) DEEP_ARCHIVE objectへの発行は従来どおり成功する
    let Some(fx_da) = StorageFixture::new(
        "sb-rstval-da",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let b = fx_da.data_root.join("b.zip");
    write_file(&b, b"restore-validate-da");
    assert_eq!(fx_da.push(&[&b], &[], &[]).0, 0);
    let da_key = fx_da.file_key("unfiled", None, "b.zip");
    let (code, stdout, stderr) = fx_da.manage("jobs", &["reissue", &da_key]);
    assert_eq!(
        code, 0,
        "DEEP_ARCHIVE objectへのreissueはexit 0: {stdout}{stderr}"
    );
    assert!(
        fx_da
            .calls()
            .lines()
            .any(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={da_key}"))),
        "RestoreObject発行: {}",
        fx_da.calls()
    );
}
