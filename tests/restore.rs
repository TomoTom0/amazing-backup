//! restore系3サブコマンド(request/status/download)の結合test(設計本編2.7・
//! tmp/tasks/tm-5/design_restore.md 3章・8章RES-001〜024)。
//! bin backup-remoteをsubprocess実行し、実ageでの暗号化とfake S3 backend seam
//! (env BACKUP_REMOTE_FAKE_S3_STATEで差し替わる状態を持つS3 simulator・tests/common)での
//! 復元を検証する(実networkなし)。テスト先行のためsrc/restore.rs実装前は
//! restore系サブコマンドがunknown扱いとなり各testはfailする(実装後に通う)。

mod common;

use common::*;
use std::path::{Path, PathBuf};

/// download方向のAPI呼び出し行(GetObject。upload方向はPutObjectのため区別は不要)。
fn is_download_get(line: &str) -> bool {
    line.starts_with("s3 GetObject")
}

/// restore系testの共通fixture: 2ファイル(a.zip・b.7z)をencrypt→upload済みの
/// fake S3(objects/<date>/{a.zip.age,b.7z.age,manifest.jsonl.age,
/// RESTORE_README.txt})とaws.tomlを用意する。復元状態(restore/<key>)は空。
struct RestoreFixture {
    /// tmp dirの生存保持(Dropで削除されるため読み出しはfx.tmp.path()で行う)
    #[allow(dead_code)]
    tmp: TestDir,
    aws_toml: PathBuf,
    state: PathBuf,
    secret: PathBuf,
    /// 改変manifest作成用のrecipient公開鍵(RES-012)
    public: PathBuf,
    /// encrypt実行時のbatch_date(JST暦日)
    date: String,
    plain_a: Vec<u8>,
    plain_b: Vec<u8>,
}

impl RestoreFixture {
    /// age/ssh-keygenが使えない環境ではNone(呼び出し側testはskip扱い)。
    fn new(tag: &str) -> Option<Self> {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new(tag);
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let enc_out = tmp.path().join("enc-out");
        let registry = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public.clone()], &enc_out, &registry, None);
        let plain_a = b"restore-it-plain-A-0123456789abcdef".to_vec();
        let plain_b = b"restore-it-plain-B-zyxwvutsrqponmlk".to_vec();
        let a = tmp.path().join("a.zip");
        let b = tmp.path().join("b.7z");
        write_file(&a, &plain_a);
        write_file(&b, &plain_b);
        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
            &b.display().to_string(),
        ]);
        assert_eq!(code, 0, "encrypt: {stderr}");

        let state = tmp.path().join("s3state");
        std::fs::create_dir_all(&state).expect("state dir");
        let creds = write_fake_credentials(tmp.path());
        let aws_toml = tmp.path().join("aws.toml");
        write_file(
            &aws_toml,
            aws_toml_body("it-bucket", "ap-northeast-1", &creds, None).as_bytes(),
        );
        let date = jst_today();
        let state_dir = state.display().to_string();
        let (code, _, stderr) = run_bin_env(
            &[
                "upload",
                "--config",
                &cfg.display().to_string(),
                "--aws-config",
                &aws_toml.display().to_string(),
            ],
            &[("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())],
        );
        assert_eq!(code, 0, "upload: {stderr}");
        Some(Self {
            tmp,
            aws_toml,
            state,
            secret,
            public,
            date,
            plain_a,
            plain_b,
        })
    }

    /// restore系サブコマンドをfake S3 backend seam環境(BACKUP_REMOTE_FAKE_S3_STATE)で実行し
    /// (exit code, stdout, stderr)を返す。subにはサブコマンド動詞以降を渡す
    /// (--aws-configは末尾に自動付与)。
    fn run_env(&self, sub: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
        let state_dir = self.state.display().to_string();
        let mut envs: Vec<(&str, &str)> = vec![("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())];
        envs.extend_from_slice(extra_env);
        let aws_s = self.aws_toml.display().to_string();
        let mut args: Vec<&str> = vec!["restore"];
        args.extend_from_slice(sub);
        args.extend_from_slice(&["--aws-config", aws_s.as_str()]);
        run_bin_env(&args, &envs)
    }

    fn run(&self, sub: &[&str]) -> (i32, String, String) {
        self.run_env(sub, &[])
    }

    /// `restore request --date <date>`(追加args・追加env付き)。
    fn request_env(&self, extra: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        let d = self.date.clone();
        let mut args: Vec<&str> = vec!["request", "--date", &d];
        args.extend_from_slice(extra);
        self.run_env(&args, env)
    }

    fn request(&self, extra: &[&str]) -> (i32, String, String) {
        self.request_env(extra, &[])
    }

    /// 全keyへrequest発行(成功を期待する前提。fake awsが全keyをrestored状態にする)。
    fn request_all(&self) {
        let (code, _, stderr) = self.request(&[]);
        assert_eq!(code, 0, "request_all: {stderr}");
    }

    /// `restore download --date <date> --out <out> --key <secret>`(追加args・追加env付き)。
    fn download_env(
        &self,
        out: &Path,
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let d = self.date.clone();
        let k = self.secret.display().to_string();
        let o = out.display().to_string();
        let mut args: Vec<&str> = vec!["download", "--date", &d, "--out", &o, "--key", &k];
        args.extend_from_slice(extra);
        self.run_env(&args, env)
    }

    fn download(&self, out: &Path, extra: &[&str]) -> (i32, String, String) {
        self.download_env(out, extra, &[])
    }

    /// fake backendのAPI呼び出し記録(calls.log。未実行なら空文字列)。
    fn calls(&self) -> String {
        read_calls(&self.state)
    }

    fn calls_count(&self) -> usize {
        self.calls().lines().count()
    }

    /// S3キー(<date>/<name>)を組み立てる。
    fn key(&self, name: &str) -> String {
        format!("{}/{}", self.date, name)
    }

    /// fake S3 simulator上のobject実体のpath。
    fn obj(&self, key: &str) -> PathBuf {
        self.state.join("objects").join(key)
    }

    /// head-object応答のRestore状態を事前設定する("restored"/"ongoing"。
    /// 3.8のNotRequestedはseed fileの不在で表現)。
    fn seed(&self, key: &str, st: &str) {
        let p = self.state.join("restore").join(key);
        std::fs::create_dir_all(p.parent().expect("seed parent")).expect("seed parent dir");
        std::fs::write(p, st).expect("seed write");
    }

    /// seedしたRestore状態を解除する(未発行へ戻す)。
    fn unseed(&self, key: &str) {
        let _ = std::fs::remove_file(self.state.join("restore").join(key));
    }
}

/// [covers:restore.request-restore-object-argv]
/// request発行(RES-001): --dateバッチ内の全key(manifest含む・README除外)へ
/// RestoreObjectが発行され、RestoreRequestはdays=<n>・tier=<tier>参数で組立られる
/// (SDK型RestoreRequest{days, glacier_job_parameters{tier}})。
/// --days/--tier指定値が反映され、省略時はdays=7・tier=Standard。
/// 列挙0件は警告「当該prefixにobjectなし」+exit 0(3.2)。
/// fakeのcalls.logは>>追記専用の累積logのため、同一fixtureでの再実行後の検証は
/// 全行への一括assertでなく既定値発行の件数countで行う。
#[test]
fn request_issues_restore_object_for_all_batch_keys() {
    let Some(fx) = RestoreFixture::new("rs-reqargv") else {
        return;
    };
    let (code, stdout, stderr) = fx.request(&["--days", "3", "--tier", "Bulk"]);
    assert_eq!(code, 0, "request成功: {stdout}{stderr}");
    let calls = fx.calls();
    let ro: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 RestoreObject"))
        .collect();
    assert_eq!(ro.len(), 3, "対象3key(manifest含む・README除外): {calls}");
    for name in ["a.zip.age", "b.7z.age", "manifest.jsonl.age"] {
        let k = fx.key(name);
        assert!(
            ro.iter().any(|l| l.contains(&format!("key={k}"))),
            "対象key {k}: {calls}"
        );
    }
    assert!(
        ro.iter()
            .all(|l| l.contains("days=3") && l.contains("tier=Bulk")),
        "RestoreRequest参数(days・glacier_job_parameters.tier): {calls}"
    );
    assert!(
        ro.iter().all(|l| l.contains("bucket=it-bucket")),
        "bucket反映: {calls}"
    );
    assert!(
        calls.lines().any(|l| {
            l.starts_with("s3 ListObjectsV2") && l.contains(&format!("prefix={}/", fx.date))
        }),
        "ListObjectsV2のprefix指定: {calls}"
    );

    // 省略時の既定値(days=7・tier=Standard)。復元状態をクリアしてから再実行
    // (calls.logは累積されるため、既定値発行の件数で検証する)
    std::fs::remove_dir_all(fx.state.join("restore")).expect("restore状態クリア");
    let (code, _, stderr) = fx.request(&[]);
    assert_eq!(code, 0, "既定値request成功: {stderr}");
    let calls = fx.calls();
    assert_eq!(
        calls
            .lines()
            .filter(|l| {
                l.starts_with("s3 RestoreObject")
                    && l.contains("days=7")
                    && l.contains("tier=Standard")
            })
            .count(),
        3,
        "省略時既定(days=7・Standard)の発行は3件: {calls}"
    );

    // 列挙0件(空prefixの--date)は警告+exit 0
    let (code, stdout, stderr) = fx.run(&["request", "--date", "2099-01-01"]);
    assert_eq!(code, 0, "空バッチはexit 0: {stdout}{stderr}");
    assert!(stdout.contains("objectなし"), "警告表示: {stdout}");
    assert!(
        stdout.contains("--date") && stdout.contains("bucket"),
        "確認案内: {stdout}"
    );
}

/// [covers:restore.request-idempotent-skip]
/// requestの冪等(RES-002): HeadObjectのRestoreが復元済み・進行中のkeyはskipして再発行せず、
/// 未発行(Restore無し)のkeyのみ発行する。
#[test]
fn request_skips_restored_and_ongoing_keys() {
    let Some(fx) = RestoreFixture::new("rs-reqskip") else {
        return;
    };
    fx.seed(&fx.key("a.zip.age"), "restored");
    fx.seed(&fx.key("manifest.jsonl.age"), "ongoing");
    // b.7z.ageは未発行(seedなし)

    let (code, stdout, stderr) = fx.request(&[]);
    assert_eq!(code, 0, "request成功: {stdout}{stderr}");
    let calls = fx.calls();
    let ro: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 RestoreObject"))
        .collect();
    assert_eq!(ro.len(), 1, "発行は未発行keyのみ: {calls}");
    let kb = fx.key("b.7z.age");
    assert!(
        ro[0].contains(&format!("key={kb}")),
        "発行先はb.7z.age: {calls}"
    );
    assert!(stdout.contains("skip"), "skip表示: {stdout}");
    // 事前HeadObjectは全keyに対して実行される
    for name in ["a.zip.age", "b.7z.age", "manifest.jsonl.age"] {
        let k = fx.key(name);
        assert!(
            calls
                .lines()
                .any(|l| l.starts_with("s3 HeadObject") && l.contains(&format!("key={k}"))),
            "事前HeadObject({k}): {calls}"
        );
    }
}

/// [covers:restore.request-dry-run]
/// request --dry-run(RES-003): S3 API呼び出しなし(ListObjectsV2・HeadObject・RestoreObject)・
/// 予定行表示(RestoreObject API形式)・exit 0。--name指定時は当該name+manifestの予定行のみを
/// 表示する(download側dry-run表示と対称)。
#[test]
fn request_dry_run_no_aws_execution() {
    let Some(fx) = RestoreFixture::new("rs-reqdry") else {
        return;
    };
    let before = fx.calls_count();
    let (code, stdout, stderr) = fx.request(&["--dry-run"]);
    assert_eq!(code, 0, "dry-runはexit 0: {stdout}{stderr}");
    assert_eq!(
        fx.calls_count(),
        before,
        "S3 API呼び出しなし(list/head/RestoreObject)"
    );
    assert!(stdout.contains("[dry-run]"), "dry-run表示: {stdout}");
    assert!(
        stdout.contains("RestoreObject"),
        "RestoreObject予定表示: {stdout}"
    );

    // --name指定時: 当該name+manifestの予定行のみ(未指定name・placeholderは出さない)
    let before = fx.calls_count();
    let (code, stdout, stderr) = fx.request(&["--name", "a.zip", "--dry-run"]);
    assert_eq!(code, 0, "--name付きdry-runはexit 0: {stdout}{stderr}");
    assert_eq!(fx.calls_count(), before, "--name指定でもS3 API呼び出しなし");
    assert!(
        stdout.contains(&fx.key("a.zip.age")),
        "指定nameの予定行: {stdout}"
    );
    assert!(
        stdout.contains(&fx.key("manifest.jsonl.age")),
        "manifestの予定行: {stdout}"
    );
    assert!(
        !stdout.contains(&fx.key("b.7z.age")),
        "未指定nameの予定行は出さない: {stdout}"
    );
    assert!(
        !stdout.contains("<name>.age"),
        "placeholder表示は使わない: {stdout}"
    );

    // --name manifest.jsonl単独: name行とmanifest自動追加行が同一keyのため1行のみ
    let (code, stdout, stderr) = fx.request(&["--name", "manifest.jsonl", "--dry-run"]);
    assert_eq!(code, 0, "manifest.jsonl指定dry-run: {stdout}{stderr}");
    let km = fx.key("manifest.jsonl.age");
    assert_eq!(
        stdout.lines().filter(|l| l.contains(&km)).count(),
        1,
        "manifestの予定行は1回のみ(重複表示しない): {stdout}"
    );
}

/// [covers:restore.request-tier-days-validation]
/// [covers:restore.exit-codes-integration]
/// --tier allowlist外・--days範囲外・非整数(RES-004): 設定エラー2・S3 API呼び出しなし。
#[test]
fn request_tier_days_validation() {
    let Some(fx) = RestoreFixture::new("rs-reqvalid") else {
        return;
    };
    let before = fx.calls_count();
    for extra in [
        vec!["--tier", "Expedited"],
        vec!["--days", "0"],
        vec!["--days", "366"],
        vec!["--days", "abc"],
    ] {
        let (code, stdout, stderr) = fx.request(&extra);
        assert_eq!(
            code, 2,
            "--tier/--days検証不正は設定エラー2({extra:?}): {stdout}{stderr}"
        );
    }
    assert_eq!(
        fx.calls_count(),
        before,
        "S3 API呼び出しなし(ListObjectsV2含め)"
    );
}

/// [covers:restore.status-three-states]
/// status(RES-005): Restoreヘッダ3状態(restored+expiry-date/ongoing/not-requested)が
/// 正しく表示されexit 0。READMEは「平文・復元不要」の区別表示。空prefixは警告+exit 0。
#[test]
fn status_lists_three_restore_states() {
    let Some(fx) = RestoreFixture::new("rs-status3") else {
        return;
    };
    fx.seed(&fx.key("a.zip.age"), "restored");
    fx.seed(&fx.key("b.7z.age"), "ongoing");
    // manifest.jsonl.ageは未発行

    let (code, stdout, stderr) = fx.run(&["status", "--date", &fx.date]);
    assert_eq!(code, 0, "status成功: {stdout}{stderr}");
    assert!(stdout.contains("restored"), "restored表示: {stdout}");
    assert!(stdout.contains("ongoing"), "ongoing表示: {stdout}");
    assert!(
        stdout.contains("not-requested"),
        "not-requested表示: {stdout}"
    );
    let ka = fx.key("a.zip.age");
    assert!(stdout.contains(&ka), "key表示: {stdout}");
    // 空prefixの--dateは警告+exit 0(空一覧)
    let (code, stdout, stderr) = fx.run(&["status", "--date", "2099-01-01"]);
    assert_eq!(code, 0, "空バッチはexit 0: {stdout}{stderr}");
    assert!(stdout.contains("objectなし"), "警告表示: {stdout}");
}

/// [covers:restore.restore-header-parse]
/// Restoreヘッダparse(RES-006・3.8): 3状態へ正しく分類し、expiry-dateはRFC 7231
/// HTTP-dateのraw文字列をそのまま保持する(時刻解析・JST変換をしない)。
#[test]
fn restore_header_raw_expiry_date_retained() {
    let Some(fx) = RestoreFixture::new("rs-hdrparse") else {
        return;
    };
    fx.seed(&fx.key("a.zip.age"), "restored");
    fx.seed(&fx.key("b.7z.age"), "ongoing");
    // manifest.jsonl.ageは未発行

    let (code, stdout, stderr) = fx.run(&["status", "--date", &fx.date]);
    assert_eq!(code, 0, "status成功: {stdout}{stderr}");
    // raw文字列の保持(fake awsが返す固定expiry-dateが原文どおり出る)
    assert!(
        stdout.contains("Wed, 14 Oct 2026 00:00:00 GMT"),
        "expiry-date raw文字列: {stdout}"
    );
    assert!(!stdout.contains("+09:00"), "JST変換しない: {stdout}");
    // 3状態の分類(per-line)
    let ka = fx.key("a.zip.age");
    let la = stdout
        .lines()
        .find(|l| l.contains(&ka))
        .expect("a.zip.age行");
    assert!(la.contains("restored"), "restored分類: {la}");
    let kb = fx.key("b.7z.age");
    let lb = stdout
        .lines()
        .find(|l| l.contains(&kb))
        .expect("b.7z.age行");
    assert!(lb.contains("ongoing"), "ongoing分類: {lb}");
    let km = fx.key("manifest.jsonl.age");
    let lm = stdout
        .lines()
        .find(|l| l.contains(&km))
        .expect("manifest行");
    assert!(lm.contains("not-requested"), "not-requested分類: {lm}");
}

/// [covers:restore.download-full-roundtrip]
/// [covers:restore.download-output-structure]
/// [covers:restore.exit-codes-integration]
/// download一気通貫(RES-007・RES-022・実age統合): manifest取得→復号→parse→
/// 全.age取得→age_sha256照合→復号→source_sha256照合まで成功し、<out>/<date>/配下に
/// .age・平文・manifest平文が生成される。GetObject呼び出しはchecksum_mode=ENABLED付きで
/// checksum_algorithm(作成用参数)を含まない。
#[test]
fn download_full_roundtrip_with_real_age() {
    let Some(fx) = RestoreFixture::new("rs-dlfull") else {
        return;
    };
    fx.request_all();
    let out = fx.tmp.path().join("restore-out");
    let (code, stdout, stderr) = fx.download(&out, &[]);
    assert_eq!(code, 0, "download成功: {stdout}{stderr}");
    let dir = out.join(&fx.date);

    // 出力構造(3.6): .age・平文・manifest暗号文・manifest平文
    for name in [
        "a.zip.age",
        "a.zip",
        "b.7z.age",
        "b.7z",
        "manifest.jsonl.age",
        "manifest.jsonl",
    ] {
        assert!(dir.join(name).is_file(), "成果物 {name}: {}", dir.display());
    }
    // 復号平文はオリジナルとbyte一致(実age・実鍵でのroundtrip)
    assert_eq!(
        std::fs::read(dir.join("a.zip")).unwrap(),
        fx.plain_a,
        "a.zip平文"
    );
    assert_eq!(
        std::fs::read(dir.join("b.7z")).unwrap(),
        fx.plain_b,
        "b.7z平文"
    );
    // 取得.ageはS3側objectとbyte一致
    for name in ["a.zip.age", "b.7z.age", "manifest.jsonl.age"] {
        assert_eq!(
            std::fs::read(dir.join(name)).unwrap(),
            std::fs::read(fx.obj(&fx.key(name))).unwrap(),
            "取得.age {name}"
        );
    }
    // manifest平文のparse(batchヘッダ+ファイルレコード)
    let mtext = std::fs::read_to_string(dir.join("manifest.jsonl")).unwrap();
    let mlines: Vec<&str> = mtext.trim_end_matches('\n').split('\n').collect();
    assert_eq!(mlines.len(), 3, "ヘッダ1+レコード2: {mtext}");
    let header: serde_json::Value = serde_json::from_str(mlines[0]).unwrap();
    assert_eq!(header["record_type"], "batch");
    assert_eq!(header["file_count"], 2);

    // GetObject呼び出し: checksum_mode=ENABLED付き・checksum_algorithm(作成用)なし
    let calls = fx.calls();
    let dl_get: Vec<&str> = calls.lines().filter(|l| is_download_get(l)).collect();
    assert_eq!(
        dl_get.len(),
        3,
        "download GetObjectは3件(a/b/manifest): {calls}"
    );
    assert!(
        dl_get.iter().all(|l| l.contains("checksum_mode=ENABLED")),
        "checksum_mode ENABLED: {calls}"
    );
    assert!(
        !dl_get.iter().any(|l| l.contains("checksum_algorithm")),
        "download GetObjectへchecksum_algorithmは使わない(P5): {calls}"
    );
}

/// [covers:restore.download-name-filter]
/// [covers:restore.exit-codes-integration]
/// download --name絞り込み(RES-008): 指定nameのみ処理(manifest+当該name)。
/// manifestに存在しないnameは対象エラー3。--name重複指定は対象エラー3。
#[test]
fn download_name_filter_and_missing() {
    let Some(fx) = RestoreFixture::new("rs-dlname") else {
        return;
    };
    fx.request_all();

    // 絞り込み: a.zipのみ(manifestも処理される)
    let out1 = fx.tmp.path().join("out-name1");
    let (code, stdout, stderr) = fx.download(&out1, &["--name", "a.zip"]);
    assert_eq!(code, 0, "絞り込みdownload成功: {stdout}{stderr}");
    let dir1 = out1.join(&fx.date);
    for name in ["a.zip.age", "a.zip", "manifest.jsonl.age", "manifest.jsonl"] {
        assert!(dir1.join(name).is_file(), "処理対象 {name}");
    }
    assert!(!dir1.join("b.7z.age").exists(), "b.7z.ageは未処理");
    assert!(!dir1.join("b.7z").exists(), "b.7zは未処理");
    assert_eq!(
        std::fs::read(dir1.join("a.zip")).unwrap(),
        fx.plain_a,
        "平文一致"
    );

    // 不在nameは対象エラー3
    let out2 = fx.tmp.path().join("out-name2");
    let (code, _, stderr) = fx.download(&out2, &["--name", "ghost.zip"]);
    assert_eq!(code, 3, "不在nameは対象エラー3: {stderr}");
    assert!(stderr.contains("ghost.zip"), "不在nameの案内: {stderr}");

    // 重複指定は対象エラー3
    let out3 = fx.tmp.path().join("out-name3");
    let (code, _, stderr) = fx.download(&out3, &["--name", "a.zip", "--name", "a.zip"]);
    assert_eq!(code, 3, "重複--nameは対象エラー3: {stderr}");
}

/// [covers:restore.download-idempotent-skip]
/// downloadの冪等(RES-009): 2回目実行は対象.age・平文ともsha256一致でskip
/// (対象keyのHeadObject・GetObject再実行なし。処理順はローカル.age照合が先行)。
/// manifest.jsonl.ageは毎回再取得・再復号する。
#[test]
fn download_idempotent_second_run() {
    let Some(fx) = RestoreFixture::new("rs-dlidem") else {
        return;
    };
    fx.request_all();
    let out = fx.tmp.path().join("restore-out");
    let (code, _, stderr) = fx.download(&out, &[]);
    assert_eq!(code, 0, "1回目: {stderr}");

    let get_for = |calls: &str, key: &str| {
        calls
            .lines()
            .filter(|l| is_download_get(l) && l.contains(&format!("{key}.part")))
            .count()
    };
    let head_for = |calls: &str, key: &str| {
        calls
            .lines()
            .filter(|l| l.starts_with("s3 HeadObject") && l.contains(&format!("key={key}")))
            .count()
    };
    let calls1 = fx.calls();
    let ka = fx.key("a.zip.age");
    let kb = fx.key("b.7z.age");
    let km = fx.key("manifest.jsonl.age");
    assert_eq!(get_for(&calls1, &ka), 1, "1回目a GetObject: {calls1}");
    assert_eq!(get_for(&calls1, &kb), 1, "1回目b GetObject: {calls1}");
    assert_eq!(
        get_for(&calls1, &km),
        1,
        "1回目manifest GetObject: {calls1}"
    );

    let (code, stdout, stderr) = fx.download(&out, &[]);
    assert_eq!(code, 0, "2回目exit 0: {stdout}{stderr}");
    assert!(stdout.contains("skip"), "skip表示: {stdout}");
    let calls2 = fx.calls();
    // 対象keyのGetObject・HeadObjectは再実行されない(ローカル.age照合一致→headのRestore確認をskip)
    assert_eq!(get_for(&calls2, &ka), 1, "a GetObject不変: {calls2}");
    assert_eq!(get_for(&calls2, &kb), 1, "b GetObject不変: {calls2}");
    assert_eq!(
        head_for(&calls2, &ka),
        head_for(&calls1, &ka),
        "a head不変: {calls2}"
    );
    assert_eq!(
        head_for(&calls2, &kb),
        head_for(&calls1, &kb),
        "b head不変: {calls2}"
    );
    // manifestは毎回再取得・headも再実行
    assert_eq!(
        get_for(&calls2, &km),
        2,
        "manifest GetObjectは毎回: {calls2}"
    );
    assert_eq!(
        head_for(&calls2, &km),
        head_for(&calls1, &km) + 1,
        "manifest headは毎回: {calls2}"
    );
    // 平文も再復号せず成果物は保持されたまま
    let dir = out.join(&fx.date);
    assert_eq!(
        std::fs::read(dir.join("a.zip")).unwrap(),
        fx.plain_a,
        "平文保持"
    );
}

/// [covers:restore.download-age-refetch]
/// .age再取得(RES-010): 既存.ageのsha256がage_sha256と不一致なら再取得し.part→renameで
/// 上書きする。再取得(GetObject)失敗時は既存(不一致).ageを保持する。
#[test]
fn download_refetches_mismatched_age() {
    let Some(fx) = RestoreFixture::new("rs-dlrefetch") else {
        return;
    };
    fx.request_all();
    let out = fx.tmp.path().join("restore-out");
    let (code, _, stderr) = fx.download(&out, &[]);
    assert_eq!(code, 0, "1回目: {stderr}");
    let dir = out.join(&fx.date);
    let orig = std::fs::read(dir.join("a.zip.age")).unwrap();

    // ローカル.ageを改変→再取得でS3側正本へ戻る
    std::fs::write(
        dir.join("a.zip.age"),
        [orig.as_slice(), b"CORRUPT-JUNK"].concat(),
    )
    .unwrap();
    let (code, _, stderr) = fx.download(&out, &[]);
    assert_eq!(code, 0, "改変後再実行: {stderr}");
    assert_eq!(
        std::fs::read(dir.join("a.zip.age")).unwrap(),
        orig,
        "再取得で正本の内容へ上書き"
    );

    // 再取得GetObject失敗時は既存(不一致).ageを保持する
    let corrupted = [orig.as_slice(), b"CORRUPT-JUNK-2"].concat();
    std::fs::write(dir.join("a.zip.age"), &corrupted).unwrap();
    let ka = fx.key("a.zip.age");
    let (code, _, stderr) = fx.download_env(&out, &[], &[("FAKE_S3_FAIL_CP_FOR", ka.as_str())]);
    assert_eq!(code, 5, "GetObject失敗は転送エラー5: {stderr}");
    assert_eq!(
        std::fs::read(dir.join("a.zip.age")).unwrap(),
        corrupted,
        "失敗時の既存.ageは保持(削除しない)"
    );
}

/// [covers:restore.download-plain-conflict]
/// [covers:restore.exit-codes-integration]
/// 平文衝突(RES-011): 既存平文のsha256がsource_sha256と不一致なら対象エラー3で
/// 当該ファイルを処理せず上書きしない。
#[test]
fn download_plain_conflict_is_target_error() {
    let Some(fx) = RestoreFixture::new("rs-dlplain") else {
        return;
    };
    fx.request_all();
    let out = fx.tmp.path().join("restore-out");
    let dir = out.join(&fx.date);
    std::fs::create_dir_all(&dir).expect("out dir");
    std::fs::write(dir.join("a.zip"), b"tampered-plain-content").expect("既存平文");

    let (code, _, stderr) = fx.download(&out, &[]);
    assert_eq!(code, 3, "平文衝突は対象エラー3: {stderr}");
    assert!(
        stderr.contains("a.zip"),
        "衝突nameの案内(手動リネーム促し): {stderr}"
    );
    assert_eq!(
        std::fs::read(dir.join("a.zip")).unwrap(),
        b"tampered-plain-content",
        "既存平文は不変(上書きしない)"
    );
    // 処理順(3.4-5): .age取得(a〜c)の後の平文衝突判定(d)のため .age は取得済み
    assert!(
        dir.join("a.zip.age").is_file(),
        ".ageは取得後に平文衝突で停止"
    );
}

/// [covers:restore.download-verification-mismatch]
/// [covers:restore.exit-codes-integration]
/// 検証不一致(RES-012): 取得.ageのsha256 ≠ manifest記録age_sha256は検証エラー4。
/// 復号平文sha256 ≠ source_sha256も検証エラー4(実age統合・改変fixture)。
#[test]
fn download_verification_mismatch_errors() {
    // case1: S3側の.ageを改変→取得物のsha256がmanifest記録と不一致
    let Some(fx) = RestoreFixture::new("rs-dlver1") else {
        return;
    };
    fx.request_all();
    let obj_a = fx.obj(&fx.key("a.zip.age"));
    let good = std::fs::read(&obj_a).unwrap();
    std::fs::write(&obj_a, [good.as_slice(), b"CORRUPTED-TRANSFER"].concat()).unwrap();
    let out = fx.tmp.path().join("restore-out");
    let (code, _, stderr) = fx.download(&out, &[]);
    assert_eq!(code, 4, ".age sha256不一致は検証エラー4: {stderr}");
    assert!(stderr.contains("a.zip"), "当該nameの切り分け表示: {stderr}");

    // case2: source_sha256を不正値に改変したmanifestを配置→平文sha256不一致
    let Some(fx) = RestoreFixture::new("rs-dlver2") else {
        return;
    };
    fx.request_all();
    let age = find_age().expect("このtestはage必須");
    let age_bytes = std::fs::read(fx.obj(&fx.key("a.zip.age"))).unwrap();
    let age_sha = sha256_hex(&age_bytes);
    let header = serde_json::json!({
        "schema": 1, "record_type": "batch", "batch_id": "tampered",
        "batch_date": fx.date, "created_at": "2026-10-11T00:00:00+09:00",
        "file_count": 1, "total_source_size": 0, "total_age_size": 0,
        "recipients": [],
    })
    .to_string();
    let rec = serde_json::json!({
        "schema": 1, "record_type": "file", "source_path": "/x/a.zip",
        "source_name": "a.zip", "source_size": fx.plain_a.len(),
        "source_mtime": "2026-10-11T00:00:00+09:00",
        "source_sha256": "f".repeat(64),
        "age_path": "/x/a.zip.age", "age_size": age_bytes.len(),
        "age_sha256": age_sha, "recipients": [],
        "encrypted_at": "2026-10-11T00:00:00+09:00",
    })
    .to_string();
    let mplain = format!("{header}\n{rec}\n");
    age_encrypt_bytes(
        &age,
        &[fx.public.clone()],
        mplain.as_bytes(),
        &fx.obj(&fx.key("manifest.jsonl.age")),
    );
    let out = fx.tmp.path().join("restore-out");
    let (code, _, stderr) = fx.download(&out, &[]);
    assert_eq!(code, 4, "平文sha256不一致は検証エラー4: {stderr}");
}

/// [covers:restore.download-requires-restored]
/// [covers:restore.exit-codes-integration]
/// 復元未了でのdownload(RES-013): HeadObjectのRestoreがongoing・未発行(Restore無し)の対象は
/// GetObjectを実行せず転送エラー5+案内文「restore statusで確認してください」。
#[test]
fn download_blocked_when_not_restored() {
    let Some(fx) = RestoreFixture::new("rs-dlblock") else {
        return;
    };
    // manifestのみ復元済み・a.zip.ageは進行中
    fx.seed(&fx.key("manifest.jsonl.age"), "restored");
    fx.seed(&fx.key("a.zip.age"), "ongoing");
    let out1 = fx.tmp.path().join("out-ongoing");
    let (code, _, stderr) = fx.download(&out1, &[]);
    assert_eq!(code, 5, "進行中対象のdownloadは転送エラー5: {stderr}");
    assert!(stderr.contains("restore status"), "案内文: {stderr}");
    let ka = fx.key("a.zip.age");
    let calls = fx.calls();
    assert!(
        !calls.lines().any(|l| is_download_get(l) && l.contains(&ka)),
        "対象keyへのGetObjectは実行しない: {calls}"
    );

    // 未発行(Restore無し)
    fx.unseed(&fx.key("a.zip.age"));
    let out2 = fx.tmp.path().join("out-notreq");
    let (code, _, stderr) = fx.download(&out2, &[]);
    assert_eq!(code, 5, "未発行対象のdownloadは転送エラー5: {stderr}");
    assert!(stderr.contains("restore status"), "案内文: {stderr}");
}

/// [covers:restore.download-dry-run]
/// download --dry-run(RES-014): S3 API呼び出しなし(HeadObject/GetObject)・予定表示・exit 0。
#[test]
fn download_dry_run_no_aws_execution() {
    let Some(fx) = RestoreFixture::new("rs-dldry") else {
        return;
    };
    let before = fx.calls_count();
    let out = fx.tmp.path().join("restore-out");
    let (code, stdout, stderr) = fx.download(&out, &["--dry-run"]);
    assert_eq!(code, 0, "dry-runはexit 0: {stdout}{stderr}");
    assert_eq!(fx.calls_count(), before, "S3 API呼び出しなし(head/getとも)");
    assert!(stdout.contains("[dry-run]"), "dry-run表示: {stdout}");
    assert!(stdout.contains("s3 GetObject"), "GetObject予定行: {stdout}");
    assert!(
        !out.join(&fx.date).join("manifest.jsonl").exists(),
        "復号も実行しない"
    );
}

/// [covers:restore.keep-going-continues]
/// keep-going(RES-015): per-object/per-file失敗でも継続し、exitは最初のエラー区分(5)。
/// 成功分の成果物・復元状態は残る(request/download共通)。
#[test]
fn keep_going_continues_after_failures() {
    // request側: 1 keyのみRestoreObject失敗注入
    let Some(fx) = RestoreFixture::new("rs-kg") else {
        return;
    };
    let ka = fx.key("a.zip.age");
    let (code, _, stderr) = fx.request_env(
        &["--keep-going"],
        &[("FAKE_S3_FAIL_RESTORE_FOR", ka.as_str())],
    );
    assert_eq!(code, 5, "request keep-goingはexit 5: {stderr}");
    let calls = fx.calls();
    let ro = calls
        .lines()
        .filter(|l| l.starts_with("s3 RestoreObject"))
        .count();
    assert_eq!(ro, 3, "全keyへ発行を試みる: {calls}");
    assert!(
        !fx.state.join("restore").join(&ka).is_file(),
        "失敗keyは復元状態なし"
    );
    assert!(
        fx.state.join("restore").join(fx.key("b.7z.age")).is_file()
            && fx
                .state
                .join("restore")
                .join(fx.key("manifest.jsonl.age"))
                .is_file(),
        "成功分の復元状態は残る"
    );

    // download側: 1 keyのみGetObject失敗注入(失敗keyを手動で復元済みにしてから)
    fx.seed(&ka, "restored");
    let out = fx.tmp.path().join("restore-out");
    let (code, _, stderr) = fx.download_env(
        &out,
        &["--keep-going"],
        &[("FAKE_S3_FAIL_CP_FOR", ka.as_str())],
    );
    assert_eq!(code, 5, "download keep-goingはexit 5: {stderr}");
    let dir = out.join(&fx.date);
    assert!(
        dir.join("b.7z").is_file(),
        "成功分の平文は残る: {}",
        dir.display()
    );
    assert!(dir.join("b.7z.age").is_file(), "成功分の.ageは残る");
    assert!(!dir.join("a.zip").is_file(), "失敗keyの平文は無い");
}

/// [covers:restore.download-preflight-errors]
/// [covers:restore.exit-codes-integration]
/// download事前検査(RES-016): --key未指定かつ[verify].decrypt_keyも解決不能
/// (pipeline.toml不在)・--keyの鍵ファイル不在・ageバイナリ不在は、いずれも
/// S3 API呼び出し前に設定エラー2(対象処理に紛れない)。
#[test]
fn download_preflight_key_and_age_errors() {
    let Some(fx) = RestoreFixture::new("rs-preflight") else {
        return;
    };
    let before = fx.calls_count();
    let state_dir = fx.state.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let out_s = fx.tmp.path().join("restore-out").display().to_string();
    let date = fx.date.clone();

    // (a) --key未指定・pipeline.toml不在の作業dir(default解決不能)
    let empty_cwd = fx.tmp.path().join("empty-cwd");
    std::fs::create_dir_all(&empty_cwd).expect("empty cwd");
    let (code, _, stderr) = run_bin_cwd(
        &[
            "restore",
            "download",
            "--date",
            &date,
            "--aws-config",
            &aws_s,
            "--out",
            &out_s,
        ],
        &[("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())],
        &empty_cwd,
    );
    assert_eq!(
        code, 2,
        "--key未指定かつdecrypt_key解決不能は設定エラー2: {stderr}"
    );

    // (b) --keyに不在path
    let bad_key = "/nonexistent/secret-key".to_string();
    let (code, _, stderr) = fx.run_env(
        &[
            "download", "--date", &date, "--out", &out_s, "--key", &bad_key,
        ],
        &[],
    );
    assert_eq!(code, 2, "鍵ファイル不在は設定エラー2: {stderr}");

    // (c) ageバイナリ不在(PATHからageを除外。aws CLI不要のためPATHはage不在で足りる)
    let bin_only = "/nonexistent-bin-dir";
    let key_s = fx.secret.display().to_string();
    let (code, _, stderr) = fx.run_env(
        &[
            "download", "--date", &date, "--out", &out_s, "--key", &key_s,
        ],
        &[("PATH", bin_only)],
    );
    assert_eq!(code, 2, "ageバイナリ不在は設定エラー2: {stderr}");

    assert_eq!(
        fx.calls_count(),
        before,
        "S3 API呼び出しなし(manifest head等の対象処理前に検出)"
    );
}

/// [covers:restore.download-registry-independent]
/// registry非依存(RES-017・P1): registry.jsonl・pipeline.tomlが不在の作業dirでも
/// --key指定によりdownloadが完結する。
#[test]
fn download_works_without_registry_and_pipeline() {
    let Some(fx) = RestoreFixture::new("rs-noreg") else {
        return;
    };
    fx.request_all();
    // registry.jsonl・pipeline.tomlとも不在の作業dir(災害時の新環境相当)
    let bare_cwd = fx.tmp.path().join("bare-cwd");
    std::fs::create_dir_all(&bare_cwd).expect("bare cwd");
    let state_dir = fx.state.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let out = fx.tmp.path().join("restore-out");
    let out_s = out.display().to_string();
    let key_s = fx.secret.display().to_string();
    let date = fx.date.clone();
    let (code, stdout, stderr) = run_bin_cwd(
        &[
            "restore",
            "download",
            "--date",
            &date,
            "--aws-config",
            &aws_s,
            "--out",
            &out_s,
            "--key",
            key_s.as_str(),
        ],
        &[("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())],
        &bare_cwd,
    );
    assert_eq!(code, 0, "registry非依存でdownload完結: {stdout}{stderr}");
    let dir = out.join(&fx.date);
    for name in ["a.zip.age", "a.zip", "manifest.jsonl"] {
        assert!(dir.join(name).is_file(), "成果物 {name}");
    }
    assert_eq!(
        std::fs::read(dir.join("b.7z")).unwrap(),
        fx.plain_b,
        "平文一致"
    );
}

/// [covers:restore.date-required]
/// [covers:restore.exit-codes-integration]
/// --date必須(RES-019): 3サブコマンドとも--date指定なしはusage error 64。
/// --date形式不正も64(RES-018のusage error代表)。
#[test]
fn date_required_and_format_validation() {
    let Some(fx) = RestoreFixture::new("rs-date") else {
        return;
    };
    for sub in ["request", "status", "download"] {
        let (code, _, _) = fx.run(&[sub]);
        assert_eq!(code, 64, "{sub}は--date必須(usage error 64)");
    }
    let (code, _, stderr) = fx.run(&["request", "--date", "2026-10-8"]);
    assert_eq!(code, 64, "--date形式不正(0埋め無し)は64: {stderr}");
    let (code, _, stderr) = fx.run(&["request", "--date", "20261008"]);
    assert_eq!(code, 64, "--date形式不正(区切り無し)は64: {stderr}");
}

/// [covers:restore.cli-override-argv]
/// CLI一時上書き(RES-020): --bucket/--regionがListObjectsV2・HeadObject・
/// RestoreObject・GetObjectの各API呼び出しへ反映される(--profileはTASK-9で廃止)。
#[test]
fn cli_overrides_reflected_in_restore_argv() {
    let Some(fx) = RestoreFixture::new("rs-override") else {
        return;
    };
    let ov = ["--bucket", "ov-bucket", "--region", "us-east-1"];

    // request側: ListObjectsV2・HeadObject・RestoreObject
    let (code, stdout, stderr) = fx.request(&ov);
    assert_eq!(code, 0, "上書きrequest成功: {stdout}{stderr}");
    let calls = fx.calls();
    let list_line = calls
        .lines()
        .find(|l| l.starts_with("s3 ListObjectsV2"))
        .expect("list行");
    assert!(
        list_line.contains("bucket=ov-bucket"),
        "ListObjectsV2へ上書きbucket反映: {list_line}"
    );
    for line in calls.lines().filter(|l| l.starts_with("s3 RestoreObject")) {
        assert!(
            line.contains("bucket=ov-bucket"),
            "RestoreObjectへ上書きbucket反映: {line}"
        );
    }
    for line in calls
        .lines()
        .filter(|l| l.starts_with("s3 HeadObject") && l.contains("ov-bucket"))
    {
        assert!(
            line.contains("bucket=ov-bucket"),
            "HeadObjectへ上書きbucket反映: {line}"
        );
    }

    // download側: HeadObject・GetObject
    let out = fx.tmp.path().join("restore-out");
    let (code, stdout, stderr) = fx.download(&out, &ov);
    assert_eq!(code, 0, "上書きdownload成功: {stdout}{stderr}");
    let calls = fx.calls();
    let dl_get: Vec<&str> = calls
        .lines()
        .filter(|l| is_download_get(l) && l.contains("bucket=ov-bucket"))
        .collect();
    assert!(!dl_get.is_empty(), "GetObjectの上書きbucket: {calls}");
    let dl_head: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 HeadObject") && l.contains("bucket=ov-bucket"))
        .collect();
    assert!(!dl_head.is_empty(), "download側のHeadObject: {calls}");
}

/// [covers:restore.request-name-cross-check]
/// request --name不在検出(RES-023): --name指定時はListObjectsV2結果と突合し、
/// バッチ内に存在しないnameはRestoreObject発行前に対象エラー3。
/// --name重複指定も対象エラー3。--name絞り込み時の対象は当該name+manifest。
#[test]
fn request_name_cross_check() {
    let Some(fx) = RestoreFixture::new("rs-namecheck") else {
        return;
    };
    // 不在name: 発行前に対象エラー3
    let (code, _, stderr) = fx.request(&["--name", "ghost.zip"]);
    assert_eq!(code, 3, "不在nameは対象エラー3: {stderr}");
    assert!(stderr.contains("ghost.zip"), "不在nameの案内: {stderr}");
    let calls = fx.calls();
    assert!(
        !calls.lines().any(|l| l.starts_with("s3 RestoreObject")),
        "RestoreObject発行なし: {calls}"
    );
    assert!(
        calls.lines().any(|l| l.starts_with("s3 ListObjectsV2")),
        "ListObjectsV2は実行(突合): {calls}"
    );

    // 重複指定: 対象エラー3
    let (code, _, stderr) = fx.request(&["--name", "a.zip", "--name", "a.zip"]);
    assert_eq!(code, 3, "重複--nameは対象エラー3: {stderr}");
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 RestoreObject")),
        "発行なし"
    );

    // 絞り込みhappy path: 当該name+manifestのみへ発行
    let (code, _, stderr) = fx.request(&["--name", "a.zip"]);
    assert_eq!(code, 0, "絞り込みrequest成功: {stderr}");
    let calls = fx.calls();
    let ro: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 RestoreObject"))
        .collect();
    assert_eq!(ro.len(), 2, "対象はa.zip.age+manifest: {calls}");
    assert!(
        ro.iter()
            .any(|l| l.contains(&format!("key={}", fx.key("a.zip.age")))),
        "a.zip.age: {calls}"
    );
    assert!(
        ro.iter()
            .any(|l| l.contains(&format!("key={}", fx.key("manifest.jsonl.age")))),
        "manifest: {calls}"
    );
    assert!(
        !ro.iter()
            .any(|l| l.contains(&format!("key={}", fx.key("b.7z.age")))),
        "b.7z.ageへは発行しない: {calls}"
    );

    // `--name manifest.jsonl`単独指定: name解決keyとmanifest自動追加keyが同一keyでも
    // RestoreObject発行は1回に限られる(同keyへの重複発行をしない)
    fx.unseed(&fx.key("manifest.jsonl.age"));
    let (code, _, stderr) = fx.request(&["--name", "manifest.jsonl"]);
    assert_eq!(code, 0, "manifest.jsonl単独指定は成功: {stderr}");
    let calls = fx.calls();
    let km = fx.key("manifest.jsonl.age");
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={km}")))
            .count(),
        2,
        "manifestへの発行は累積2回(絞り込みhappy pathの1回+今回の1回のみ・重複発行しない): {calls}"
    );
}

/// [covers:restore.download-manifest-missing]
/// manifest不在(RES-024): download時のmanifest.jsonl.ageのHeadObject 404は
/// 「バッチ不在(--date誤り・bucket誤り)」として対象エラー3+専用案内文。
#[test]
fn manifest_head_404_is_batch_missing() {
    let Some(fx) = RestoreFixture::new("rs-no404") else {
        return;
    };
    // 当該--dateのprefixにobjectが存在しない(空のfake S3)
    let out = fx.tmp.path().join("restore-out");
    let d = "2099-12-31".to_string();
    let k = fx.secret.display().to_string();
    let o = out.display().to_string();
    let (code, _, stderr) = fx.run(&["download", "--date", &d, "--out", &o, "--key", &k]);
    assert_eq!(
        code, 3,
        "manifestのHeadObject 404はバッチ不在の対象エラー3: {stderr}"
    );
    assert!(
        stderr.contains("manifest.jsonl.age") && stderr.contains("バッチ不在"),
        "専用案内文: {stderr}"
    );
}

/// [covers:restore.exit-codes-integration]
/// request時の対象key head 404(RES-018転送エラー5の代表case追加): head 404はkey不在
/// として即転送エラー5にする(未発行扱いでRestoreObject発行に進まず二段階失敗を回避。
/// download側のkey不在報告と対称)。
#[test]
fn request_head_404_is_key_missing_transfer_error() {
    let Some(fx) = RestoreFixture::new("rs-req404") else {
        return;
    };
    let ka = fx.key("a.zip.age");
    let (code, stdout, stderr) = fx.request_env(&[], &[("FAKE_S3_HEAD_404_FOR", ka.as_str())]);
    assert_eq!(code, 5, "head 404はkey不在の転送エラー5: {stdout}{stderr}");
    assert!(
        stderr.contains("存在しない") && stderr.contains(&ka),
        "key不在の案内: {stderr}"
    );
    let calls = fx.calls();
    assert!(
        !calls.lines().any(|l| l.starts_with("s3 RestoreObject")),
        "head 404対象へRestoreObjectは発行しない: {calls}"
    );
    assert!(stdout.contains("-- summary:"), "summary表示: {stdout}");
}

/// [covers:restore.exit-codes-integration]
/// statusのhead失敗fail-fast(RES-018転送エラー5の代表case追加): HeadObject失敗で
/// 即終了する場合も他サブコマンドと同形式のsummary行を出力する。
#[test]
fn status_head_failure_outputs_summary() {
    let Some(fx) = RestoreFixture::new("rs-stathd") else {
        return;
    };
    let ka = fx.key("a.zip.age");
    let d = fx.date.clone();
    let (code, stdout, stderr) = fx.run_env(
        &["status", "--date", &d],
        &[("FAKE_S3_FAIL_HEAD_FOR", ka.as_str())],
    );
    assert_eq!(code, 5, "head失敗は転送エラー5: {stdout}{stderr}");
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("-- ") && l.contains("restored") && l.contains("failed")),
        "fail-fast時も同形式のsummary行: {stdout}"
    );
}
