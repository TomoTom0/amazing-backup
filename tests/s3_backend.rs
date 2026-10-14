//! S3 backend固有の結合test(TASK-9 AWS SDK移行・tmp/tasks/tm-9/design_sdk_migration.md)。
//! fake seam(env BACKUP_REMOTE_FAKE_S3_STATE・設計4章)・multipart(5章)・
//! credentialファイル(3章)・--profile廃止(3.7・9.1)・AWS_ENDPOINT_URL系env警告(9.3)を
//! bin subprocess実行で検証する。テスト先行のためsrc/s3.rs・load_credentials実装前は
//! 各testはfailする(実装後に通る)。libの未実装要素(S3Backend等)は直接参照しない。

mod common;

use common::*;
use std::path::{Path, PathBuf};

/// S3 backend系testの共通fixture: 1ファイルencrypt済みのregistryと
/// fake seam環境(state dir・credentialファイル・aws.toml)を用意する。
/// aws.toml・credentialファイルはテスト側で書き換え可能(rewrite_*で上書き)。
struct S3BackendFixture {
    /// tmp dirの生存保持(Dropで削除されるため読み出しはfx.tmp.path()で行う)
    #[allow(dead_code)]
    tmp: TestDir,
    cfg: PathBuf,
    aws_toml: PathBuf,
    credentials: PathBuf,
    state: PathBuf,
}

impl S3BackendFixture {
    /// age/ssh-keygenが使えない環境ではNone(呼び出し側testはskip扱い)。
    fn new(tag: &str) -> Option<Self> {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new(tag);
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let registry = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &registry, None);
        let a = tmp.path().join("a.zip");
        write_file(&a, b"s3-backend-it-plain");
        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
        ]);
        assert_eq!(code, 0, "encrypt: {stderr}");

        let state = tmp.path().join("s3state");
        std::fs::create_dir_all(&state).expect("state dir");
        let credentials = write_fake_credentials(tmp.path());
        let aws_toml = tmp.path().join("aws.toml");
        write_file(
            &aws_toml,
            aws_toml_body("it-bucket", "ap-northeast-1", &credentials, Some("DEEP_ARCHIVE"))
                .as_bytes(),
        );
        Some(Self { tmp, cfg, aws_toml, credentials, state })
    }

    /// credentialファイルの内容を書き換える(schema検証・非露出検証用)。
    fn rewrite_credentials(&self, body: &str) {
        write_file(&self.credentials, body.as_bytes());
    }

    /// aws.tomlの内容を書き換える(credentials_file必須キー検証等)。
    fn rewrite_aws_toml(&self, body: &str) {
        write_file(&self.aws_toml, body.as_bytes());
    }

    /// fake seam環境(BACKUP_REMOTE_FAKE_S3_STATE・追加env)でuploadを実行し
    /// (exit code, stdout, stderr)を返す。
    fn upload_env(
        &self,
        cli_args: &[&str],
        extra_env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let cfg_s = self.cfg.display().to_string();
        let aws_s = self.aws_toml.display().to_string();
        let state_dir = self.state.display().to_string();
        let mut envs: Vec<(&str, &str)> = vec![("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())];
        envs.extend_from_slice(extra_env);
        let mut args: Vec<&str> = vec!["upload", "--config", &cfg_s, "--aws-config", &aws_s];
        args.extend_from_slice(cli_args);
        run_bin_env(&args, &envs)
    }

    fn upload(&self, cli_args: &[&str]) -> (i32, String, String) {
        self.upload_env(cli_args, &[])
    }

    /// fake backendのAPI呼び出し記録(calls.log。未実行なら空文字列)。
    fn calls(&self) -> String {
        read_calls(&self.state)
    }
}

/// 64 MiB(multipart閾値・MULTIPART_THRESHOLD_BYTES。設計5.3)。
const MIB: usize = 1024 * 1024;
const THRESHOLD: usize = 64 * MIB;

/// [covers:cli.fake-s3-seam]
/// fake seam(TASK-9設計4.3案1・12.4): env BACKUP_REMOTE_FAKE_S3_STATE指定でS3 backendが
/// fakeへ差し替わる。ダミーcredential値(実AWSで無効)でもuploadが成功する=実network不使用。
/// fake選択時はstderr冒頭へ「WARNING: FAKE S3 MODE」が表示される(誤用防御)。
/// state dir配下へobjects/<key>とAPI呼び出し記録(calls.log)が書かれる。
#[test]
fn fake_seam_uses_fake_backend_with_warning() {
    let Some(fx) = S3BackendFixture::new("sb-seam") else {
        return;
    };
    let (code, stdout, stderr) = fx.upload(&[]);
    assert_eq!(code, 0, "fake seamでupload成功(ダミーcredentialでも=実network不使用): {stdout}{stderr}");
    assert!(
        stderr.contains("WARNING: FAKE S3 MODE"),
        "fake選択時のWARNING表示: {stderr}"
    );
    let date = jst_today();
    let obj = fx.state.join("objects").join(&date).join("a.zip.age");
    assert!(obj.is_file(), "state dirへobject配置: {}", obj.display());
    let calls = fx.calls();
    assert!(
        calls.lines().any(|l| l.starts_with("s3 PutObject") && l.contains("key=") && l.contains("bucket=it-bucket")),
        "API呼び出し記録(calls.log)の生成: {calls}"
    );
}

/// [covers:cli.profile-option-removed]
/// --profile廃止(設計3.7・9.1): upload/restore全系で--profileは未知オプション扱いの
/// usage error 64。--helpに--profileが現れず--region/--bucketは維持される。
/// aws.tomlのprofileキー残存はdeny_unknown_fieldsで設定エラー2。
#[test]
fn profile_option_is_usage_error() {
    let Some(fx) = S3BackendFixture::new("sb-prof") else {
        return;
    };
    let (code, _, _) = fx.upload(&["--profile", "x"]);
    assert_eq!(code, 64, "upload --profileは未知オプションでusage error 64");
    let state_dir = fx.state.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let date = jst_today();
    let (code, _, _) = run_bin_env(
        &["restore", "request", "--date", &date, "--aws-config", &aws_s, "--profile", "x"],
        &[("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())],
    );
    assert_eq!(code, 64, "restore request --profileは未知オプションでusage error 64");

    // --help文言: --profile無し・--region/--bucketあり
    let (code, stdout, stderr) = run_bin(&["upload", "--help"]);
    assert_eq!(code, 0, "upload --helpはexit 0: {stderr}");
    assert!(!stdout.contains("--profile"), "--profileはhelp文言に現れない: {stdout}");
    assert!(stdout.contains("--region"), "--regionは維持: {stdout}");
    assert!(stdout.contains("--bucket"), "--bucketは維持: {stdout}");

    // 旧aws.tomlのprofile残存は未知キーで設定エラー2(移行検出)
    let creds_s = fx.credentials.display().to_string();
    fx.rewrite_aws_toml(&format!(
        "[aws]\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"{creds_s}\"\nprofile = \"backup\"\n"
    ));
    let (code, _, stderr) = fx.upload(&[]);
    assert_eq!(code, 2, "aws.tomlのprofile残存は設定エラー2: {stderr}");
}

/// [covers:config.aws-credentials-file-required]
/// [aws].credentials_fileキー必須(設計3.3・9.2): キー欠落・空値は設定エラー2で
/// S3 API呼び出し前に弾かれる(明示API key要件・書き漏れの即検出)。
#[test]
fn credentials_file_key_required() {
    let Some(fx) = S3BackendFixture::new("sb-credreq") else {
        return;
    };
    // キー欠落
    fx.rewrite_aws_toml("[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\n");
    let (code, _, stderr) = fx.upload(&[]);
    assert_eq!(code, 2, "credentials_fileキー欠落は設定エラー2: {stderr}");
    assert_eq!(fx.calls(), "", "S3 API呼び出しは行われない");
    // 空値
    fx.rewrite_aws_toml(
        "[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"\"\n",
    );
    let (code, _, stderr) = fx.upload(&[]);
    assert_eq!(code, 2, "credentials_file空値は設定エラー2: {stderr}");
}

/// [covers:config.credentials-file-schema]
/// credentialファイルschema(設計3.1・3.2): [credentials]配下access_key_id/
/// secret_access_key必須・session_token省略可・未知キー拒否・空値・前後空白付き値は
/// 設定エラー2。正しい形式(session_token省略)はuploadまで成功する。
#[test]
fn credentials_file_schema_validation() {
    let Some(fx) = S3BackendFixture::new("sb-credschema") else {
        return;
    };
    // 正しい形式(session_token省略): upload成功(S3 API呼び出しまで到達)
    fx.rewrite_credentials(
        "[credentials]\naccess_key_id = \"AKITEST0000000000000A\"\nsecret_access_key = \"test-secret-value-not-real\"\n",
    );
    let (code, _, stderr) = fx.upload(&[]);
    assert_eq!(code, 0, "session_token省略の正しい形式は成功: {stderr}");
    assert!(!fx.calls().is_empty(), "S3 API呼び出しまで到達");

    // 不正系はすべて設定エラー2(credentialファイルの検証)。
    // 1件ずつ実際のuploadで検証すると2件目以降はupload済みskipとなるため、
    // 設定検査(require_ready)がdry-runより先に行われることを利用しdry-runで検査する。
    for (tag, body) in [
        ("不正TOML", "[credentials\nbroken"),
        ("[credentials]テーブル不在", "[other]\naccess_key_id = \"x\"\n"),
        ("access_key_id欠落", "[credentials]\nsecret_access_key = \"s\"\n"),
        ("secret_access_key欠落", "[credentials]\naccess_key_id = \"a\"\n"),
        ("access_key_id空値", "[credentials]\naccess_key_id = \"\"\nsecret_access_key = \"s\"\n"),
        ("secret_access_key空値", "[credentials]\naccess_key_id = \"a\"\nsecret_access_key = \"\"\n"),
        ("前後空白付き値", "[credentials]\naccess_key_id = \" AKI1 \"\nsecret_access_key = \"s\"\n"),
        ("未知キー", "[credentials]\naccess_key_id = \"a\"\nsecret_access_key = \"s\"\ntoken = \"x\"\n"),
    ] {
        fx.rewrite_credentials(body);
        let (code, _, stderr) = fx.upload(&["--dry-run"]);
        assert_eq!(code, 2, "credentialファイル{tag}は設定エラー2: {stderr}");
    }
}

/// [covers:config.credentials-fail-closed]
/// fail-closed(設計3.5): credentials_fileが指すファイル不在は設定エラー2で、
/// credential chain(IMDS/env/~/.aws)へのfallbackは行われない(別環境credentialでの
/// 意図しない接続を構造的に防止)。
#[test]
fn credentials_fail_closed_no_fallback() {
    let Some(fx) = S3BackendFixture::new("sb-credno") else {
        return;
    };
    // 存在しないpathへ指す(ファイル不在)
    fx.rewrite_aws_toml(
        "[aws]\nactive_profile = \"default\"\n\n[aws.profiles.default]\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\ncredentials_file = \"/nonexistent/aws-credentials.toml\"\n",
    );
    let (code, _, stderr) = fx.upload(&[]);
    assert_eq!(code, 2, "credentialファイル不在は設定エラー2(fallbackしない): {stderr}");
    assert_eq!(fx.calls(), "", "S3 API呼び出しは行われない");
}

/// [covers:config.credentials-value-not-leaked]
/// credential値非露出(設計3.6・12.4): 設定エラー時・正常完了時・転送エラー時の
/// いずれのstdout/stderrにもcredential値(検出マーカー)が現れない。
/// error文はcredentialファイルのpathのみを表示する。
#[test]
fn credentials_values_never_leaked_in_output() {
    let Some(fx) = S3BackendFixture::new("sb-credleak") else {
        return;
    };
    fx.rewrite_credentials(
        "[credentials]\naccess_key_id = \"LEAKMARK-AKID-9f8e7d6c\"\nsecret_access_key = \"LEAKMARK-SECRET-5a4b3c2d1e\"\nsession_token = \"LEAKMARK-TOKEN-0f9e8d7c\"\n",
    );

    // (a) 設定エラー経路(未知キーでexit 2): 値が出力に現れない
    fx.rewrite_credentials(
        "[credentials]\naccess_key_id = \"LEAKMARK-AKID-9f8e7d6c\"\nsecret_access_key = \"LEAKMARK-SECRET-5a4b3c2d1e\"\nunknown_key = \"LEAKMARK-UNKNOWN-11223344\"\n",
    );
    let (code, _, stderr) = fx.upload(&[]);
    assert_eq!(code, 2, "未知キーで設定エラー2: {stderr}");
    assert!(!stderr.contains("LEAKMARK"), "設定エラー時のstderrへcredential値を出さない: {stderr}");

    // (b) 正常完了経路(fake seam): 値が出力に現れない
    fx.rewrite_credentials(
        "[credentials]\naccess_key_id = \"LEAKMARK-AKID-9f8e7d6c\"\nsecret_access_key = \"LEAKMARK-SECRET-5a4b3c2d1e\"\n",
    );
    let (code, stdout, stderr) = fx.upload(&[]);
    assert_eq!(code, 0, "正常upload: {stdout}{stderr}");
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
    let date = jst_today();
    let fail_key = format!("{date}/a.zip.age");
    // 対象を未uploadへ戻す(object実体を削除)して失敗注入
    let _ = std::fs::remove_file(fx.state.join("objects").join(&date).join("a.zip.age"));
    let (code, _, stderr) = fx.upload_env(&[], &[("FAKE_S3_FAIL_CP_FOR", fail_key.as_str())]);
    assert_eq!(code, 5, "失敗注入で転送エラー5: {stderr}");
    assert!(!stderr.contains("LEAKMARK"), "転送エラー時のstderrへcredential値を出さない: {stderr}");
}

/// [covers:upload.multipart-threshold]
/// [covers:upload.multipart-checksum-per-part]
/// multipart閾値(設計5.3)とchecksum指定(設計5.4): 64 MiB超の対象は
/// CreateMultipartUpload→UploadPart×N→CompleteMultipartUploadでuploadされ
/// (単発PutObjectは使わない)、CreateMultipartUploadと各UploadPartの両方へ
/// checksum_algorithm SHA256が指定される。閾値未満(manifest・README)は単発PutObject。
/// 完成objectはローカルの.ageとbyte一致する。
#[test]
fn multipart_upload_above_threshold() {
    let Some(()) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("sb-multipart");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let registry = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &registry, None);
        // 閾値(64MiB)を確実に超える対象(平文64MiB+1MiB → .ageも64MiB超)
        let big = tmp.path().join("big.zip");
        make_archive(&big, THRESHOLD + MIB);
        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &big.display().to_string(),
        ]);
        assert_eq!(code, 0, "encrypt(big): {stderr}");

        let state = tmp.path().join("s3state");
        std::fs::create_dir_all(&state).expect("state dir");
        let creds = write_fake_credentials(tmp.path());
        let aws_toml = tmp.path().join("aws.toml");
        write_file(
            &aws_toml,
            aws_toml_body("it-bucket", "ap-northeast-1", &creds, Some("DEEP_ARCHIVE")).as_bytes(),
        );
        let (code, stdout, stderr) = upload_with_seam(&cfg, &aws_toml, &state, &[]);
        assert_eq!(code, 0, "multipart upload成功: {stdout}{stderr}");

        let date = jst_today();
        let local_age = out.join(&date).join("big.zip.age");
        let age_bytes = std::fs::read(&local_age).unwrap();
        assert!(
            age_bytes.len() > THRESHOLD,
            "fixture対象は64MiB超であること: {} bytes",
            age_bytes.len()
        );
        let calls = read_calls(&state);
        let key = format!("{date}/big.zip.age");

        // 閾値超keyはmultipart(Create→UploadPart×N→Complete)で・単発PutObjectは無し
        let create_line = calls
            .lines()
            .find(|l| l.starts_with("s3 CreateMultipartUpload") && l.contains(&format!("key={key}")))
            .unwrap_or_else(|| panic!("CreateMultipartUpload行: {calls}"));
        assert!(
            create_line.contains("storage_class=DEEP_ARCHIVE") && create_line.contains("checksum=SHA256"),
            "CreateMultipartUploadのstorage_class・checksum指定: {create_line}"
        );
        let parts: Vec<&str> = calls
            .lines()
            .filter(|l| l.starts_with("s3 UploadPart") && l.contains(&format!("key={key}")))
            .collect();
        assert!(parts.len() >= 2, "UploadPartは2回以上: {calls}");
        for p in &parts {
            assert!(p.contains("checksum=SHA256"), "各UploadPartのchecksum指定: {p}");
        }
        assert!(
            calls
                .lines()
                .any(|l| l.starts_with("s3 CompleteMultipartUpload") && l.contains(&format!("key={key}"))),
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
            std::fs::read(state.join("objects").join(&key)).unwrap(),
            age_bytes,
            "multipart完成objectの結合一致"
        );

        // 閾値未満(manifest・README)は単発PutObject(CreateMultipartUpload無し)
        let mkey = format!("{date}/manifest.jsonl.age");
        assert!(
            calls
                .lines()
                .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={mkey}"))),
            "manifestは単発PutObject: {calls}"
        );
        assert!(
            !calls
                .lines()
                .any(|l| l.starts_with("s3 CreateMultipartUpload") && l.contains(&format!("key={mkey}"))),
            "manifestへCreateMultipartUploadは無い: {calls}"
        );
        Some(())
    })() else {
        return;
    };
}

/// [covers:upload.multipart-abort-on-part-failure]
/// part失敗時abort(設計5.7・12.4): multipart送信中のpart失敗(失敗注入FAKE_S3_FAIL_CP_FOR)は
/// 転送エラー5とし、AbortMultipartUploadが発火する(不完全MPUの放置防止。
/// CompleteMultipartUploadは実行されない)。
#[test]
fn multipart_abort_on_part_failure() {
    let Some(()) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("sb-abort");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let registry = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &registry, None);
        let big = tmp.path().join("big.zip");
        make_archive(&big, THRESHOLD + MIB);
        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &big.display().to_string(),
        ]);
        assert_eq!(code, 0, "encrypt(big): {stderr}");

        let state = tmp.path().join("s3state");
        std::fs::create_dir_all(&state).expect("state dir");
        let creds = write_fake_credentials(tmp.path());
        let aws_toml = tmp.path().join("aws.toml");
        write_file(
            &aws_toml,
            aws_toml_body("it-bucket", "ap-northeast-1", &creds, Some("DEEP_ARCHIVE")).as_bytes(),
        );
        let date = jst_today();
        let key = format!("{date}/big.zip.age");
        let (code, stdout, stderr) = upload_with_seam(
            &cfg,
            &aws_toml,
            &state,
            &[("FAKE_S3_FAIL_CP_FOR", key.as_str())],
        );
        assert_eq!(code, 5, "part失敗は転送エラー5: {stdout}{stderr}");
        let calls = read_calls(&state);
        assert!(
            calls
                .lines()
                .any(|l| l.starts_with("s3 AbortMultipartUpload") && l.contains(&format!("key={key}"))),
            "part失敗でAbortMultipartUpload発火: {calls}"
        );
        assert!(
            !calls
                .lines()
                .any(|l| l.starts_with("s3 CompleteMultipartUpload") && l.contains(&format!("key={key}"))),
            "CompleteMultipartUploadは実行されない: {calls}"
        );
        Some(())
    })() else {
        return;
    };
}

/// [covers:upload.endpoint-url-env-warning]
/// AWS_ENDPOINT_URL系env検出警告(設計9.3): AWS_ENDPOINT_URL_S3(またはAWS_ENDPOINT_URL)が
/// 設定された環境での実行は処理を継続し(exit 0)、stderrへ警告表示が出る
/// (SDKが当該envを解釈して接続先endpointを切替え得るための検出通知)。
#[test]
fn endpoint_url_env_warns_on_stderr() {
    for env_name in ["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"] {
        let Some(fx) = S3BackendFixture::new("sb-endpoint") else {
            return;
        };
        let (code, stdout, stderr) =
            fx.upload_env(&[], &[(env_name, "http://127.0.0.1:19000")]);
        assert_eq!(code, 0, "{env_name}設定時も処理継続(exit 0): {stdout}{stderr}");
        assert!(
            stderr.contains("AWS_ENDPOINT_URL"),
            "{env_name}検出の警告表示: {stderr}"
        );
    }
}

/// fake seam環境でuploadをbin実行する(multipart系testの共用。S3BackendFixtureを使わない
/// 軽量版: cfg・aws.toml・stateのみ指定)。
fn upload_with_seam(
    cfg: &Path,
    aws_toml: &Path,
    state: &Path,
    extra_env: &[(&str, &str)],
) -> (i32, String, String) {
    let cfg_s = cfg.display().to_string();
    let aws_s = aws_toml.display().to_string();
    let state_dir = state.display().to_string();
    let mut envs: Vec<(&str, &str)> = vec![("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())];
    envs.extend_from_slice(extra_env);
    run_bin_env(
        &["upload", "--config", &cfg_s, "--aws-config", &aws_s],
        &envs,
    )
}
