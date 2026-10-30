//! 結合test共用の補助(設計1章: ageバイナリ有無判定・鍵pair生成・tmp dir fixture)。
//! 実ageが不在の環境では当該testを早期returnでskip扱いにする(test failureとしない)。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// テスト終了時に再帰削除する一時dir(repo内`tmp/test/`配下。cargo test実行cwd=
/// package root基準で、cleanは`rm -rf ./tmp/test/`で完結する)。
pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(tag: &str) -> TestDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // 生成先はcwd(package root)基準の相対path「tmp/test」。StorageFixtureがこの
        // pathをpipeline.tomlのauto_path_rootsへ書くため、絶対path検証(設定エラー2)に
        // 合わせて絶対化して返す(symlink解決なし・cwdのみ前置)。
        let path = std::path::absolute(Path::new("tmp/test").join(format!(
            "amazing-backup-it-{tag}-{}-{n}",
            std::process::id()
        )))
        .expect("test temp dirの絶対path化");
        std::fs::create_dir_all(&path).expect("integration test temp dir create");
        TestDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn write_file(path: &Path, data: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent dir create");
    }
    std::fs::write(path, data).expect("test file write");
}

/// PATH上の`age`バイナリを探す。不在ならNone(呼び出し側testはskip扱い)。
pub fn find_age() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join("age");
        if is_executable(&cand) {
            return Some(cand);
        }
    }
    None
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111) != 0,
        Err(_) => false,
    }
}

/// ssh-keygenがPATH上にあるか。
pub fn ssh_keygen_available() -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| is_executable(&dir.join("ssh-keygen"))))
        .unwrap_or(false)
}

/// throwawayのed25519鍵pairをdirに生成し(pub, secret)を返す。
pub fn generate_ed25519_keypair(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    let stem = dir.join(name);
    let secret = stem.clone();
    let public = dir.join(format!("{name}.pub"));
    let out = std::process::Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-C", "amazing-backup-it", "-f"])
        .arg(&stem)
        .output()
        .expect("ssh-keygen実行");
    assert!(
        out.status.success(),
        "ssh-keygen失敗: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (public, secret)
}

/// 実ageで.ageを復号し平文bytesを返す(独立oracle。lib実装を使わない)。
pub fn age_decrypt(age: &Path, age_file: &Path, secret_key: &Path) -> Vec<u8> {
    let out = std::process::Command::new(age)
        .args(["-d", "-i"])
        .arg(secret_key)
        .arg(age_file)
        .output()
        .expect("age -d 実行");
    assert!(
        out.status.success(),
        "age -d 失敗: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// 実ageでbytesを暗号化しdestへ書き出す(fixture組み立て用の独立経路)。
pub fn age_encrypt_bytes(age: &Path, recipients: &[PathBuf], data: &[u8], dest: &Path) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(age);
    for r in recipients {
        cmd.arg("-R").arg(r);
    }
    cmd.arg("-o").arg(dest);
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("age spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(data)
        .expect("age stdin write");
    let status = child.wait().expect("age wait");
    assert!(status.success(), "age暗号化(fixture)失敗");
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(data);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// 現在のJST暦日(YYYY-MM-DD)。
pub fn jst_today() -> String {
    let jst = chrono::FixedOffset::east_opt(9 * 3600).expect("JST offset");
    chrono::Utc::now()
        .with_timezone(&jst)
        .format("%Y-%m-%d")
        .to_string()
}

/// 空でない実行可能ファイル(対象アーカイブの代用)を作る。
pub fn make_archive(path: &Path, size_bytes: usize) {
    assert!(size_bytes > 0, "テストアーカイブは空にしない");
    let mut data = Vec::with_capacity(size_bytes);
    let mut x: u32 = 0x12345678;
    while data.len() < size_bytes {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        let chunk = x.to_le_bytes();
        let need = (size_bytes - data.len()).min(4);
        data.extend_from_slice(&chunk[..need]);
    }
    write_file(path, &data);
}

/// bin amazing-backupをsubprocess実行し(exit code, stdout, stderr)を返す。
pub fn run_bin(args: &[&str]) -> (i32, String, String) {
    run_bin_env(args, &[])
}

/// 環境変数を上書きしてbinを実行する(AMAZING_BACKUP_FAKE_S3_STATE seam・PATH制限等)。
pub fn run_bin_env(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    use std::os::unix::process::ExitStatusExt as _;
    let bin = env!("CARGO_BIN_EXE_amazing-backup");
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("amazing-backup bin 実行");
    (
        out.status
            .code()
            .unwrap_or_else(|| out.status.signal().unwrap_or(-1)),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// 環境変数に加えて作業dir(cwd)も指定してbinを実行する(restore系の「pipeline.toml不在の
/// 作業dirで実行」等、cwdが関与する経路の検証用。cargo testの並列testは同一process内で
/// 走るため、test process側のcwdは変更せずsubprocessのcwdのみ変更する)。
pub fn run_bin_cwd(args: &[&str], env: &[(&str, &str)], cwd: &Path) -> (i32, String, String) {
    use std::os::unix::process::ExitStatusExt as _;
    let bin = env!("CARGO_BIN_EXE_amazing-backup");
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("amazing-backup bin 実行");
    (
        out.status
            .code()
            .unwrap_or_else(|| out.status.signal().unwrap_or(-1)),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// fake S3 backend seam(TASK-9設計4章・10.3)の検証構造。
/// bin側(cli.rs)が`AMAZING_BACKUP_FAKE_S3_STATE=<dir>`環境変数を検出するとS3 backendを
/// fakeへ差し替える(gateは#[cfg(debug_assertions)]・release buildにはseamが存在しない)。
/// fakeはstate dir配下へ次の状態を書く(旧fake aws scriptと同一の状態構造・API呼び出し記録は
/// 設計10.1のAPI形式行):
///   - objects/<key>          配置済みobjectの実体(PutObject配置・GetObject取得ともここを扱う)
///   - restore/<key>          復元job状態のmeta(HeadObject応答のRestoreの正本)。
///                            内容が"ongoing"なら ongoing-request="true"、それ以外(例:"restored")
///                            なら ongoing-request="false", expiry-date=<固定文> で応答。
///                            ファイル不在=復元job未発行(Restoreなし)。
///   - calls.log              API呼び出し記録(1呼び出し1行・key=value形式。設計10.1と同構造):
///     s3 HeadObject bucket=<b> key=<k>
///     s3 PutObject bucket=<b> key=<k> src=<path> storage_class=<sc> checksum=SHA256
///     s3 CreateMultipartUpload bucket=<b> key=<k> storage_class=<sc> checksum=SHA256
///     s3 UploadPart bucket=<b> key=<k> part_number=<n> checksum=SHA256
///     s3 CompleteMultipartUpload bucket=<b> key=<k>
///     s3 AbortMultipartUpload bucket=<b> key=<k>
///     s3 GetObject bucket=<b> key=<k> dest=<path> checksum_mode=ENABLED
///     s3 ListObjectsV2 bucket=<b> prefix=<p>
///     s3 RestoreObject bucket=<b> key=<k> days=<n> tier=<tier>
/// 失敗注入(テストから環境変数で指定・旧fake aws scriptと同一のenv名):
///   - FAKE_S3_FAIL_CP_FOR=<key>          当該keyのput(PutObject・UploadPart)・getのみ失敗
///   - FAKE_S3_FAIL_RESTORE_FOR=<key>     当該keyのRestoreObjectのみ失敗
///   - FAKE_S3_FAIL_HEAD_FOR=<key>        当該keyのHeadObjectのみ異常失敗(404以外)
///   - FAKE_S3_FAIL_DELVER_FOR=<key>[:N]  当該keyのDeleteObjectVersionのみ失敗(先頭N回は成功)
///   - FAKE_S3_HEAD_404_FOR=<key>         当該keyのHeadObjectのみ404扱い(object実体は保持)
/// fake選択時はstderr冒頭へ「WARNING: FAKE S3 MODE」が表示される(誤用防御・設計4.3)。

/// fake S3 backendのAPI呼び出し記録(state dirのcalls.log。未実行なら空文字列)。
pub fn read_calls(state: &Path) -> String {
    std::fs::read_to_string(state.join("calls.log")).unwrap_or_default()
}

/// fake seam検証用のcredentialファイルを書く(値は実AWSで無効なダミー値。
/// fake backendはこの値でも成功する=実network不使用の担保)。
pub fn write_fake_credentials(dir: &Path) -> PathBuf {
    let p = dir.join("aws-credentials.toml");
    write_file(
        &p,
        b"[credentials]\naccess_key_id = \"AKIAFAKESEAMONLY00000A\"\nsecret_access_key = \"fake-seam-only-not-a-real-secret\"\n",
    );
    p
}

/// registry.jsonlの行をJSON Value列で読む。
pub fn read_registry_lines(path: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("registry行はJSON"))
        .collect()
}

/// JST暦日を日数offsetした日付(YYYY-MM-DD)。get --versionの版日付操作等で使う。
pub fn jst_date_offset(days: i64) -> String {
    let jst = chrono::FixedOffset::east_opt(9 * 3600).expect("JST offset");
    (chrono::Utc::now() + chrono::Duration::days(days))
        .with_timezone(&jst)
        .format("%Y-%m-%d")
        .to_string()
}

/// sha256(path文字列)先頭8hex(layoutのidentity_hash・衝突hash8と同一規則)。
pub fn hash8_of_path(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    sha256_hex(canonical.to_string_lossy().as_bytes())[..8].to_string()
}

/// pty経由でbinを実行しstdinへ1行入力する(backup pullのyes/no確認を非subprocess-pipeで
/// 行う検証用。`script`バイナリが不在ならNone=呼び出し側testはskip扱い)。
pub fn run_bin_pty_stdin(
    args: &[&str],
    env: &[(&str, &str)],
    stdin_line: &str,
) -> Option<(i32, String)> {
    use std::io::Write as _;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::Stdio;
    let bin = env!("CARGO_BIN_EXE_amazing-backup");
    let mut cmdstr = format!("'{bin}'");
    for a in args {
        cmdstr.push_str(&format!(" '{a}'"));
    }
    let mut cmd = std::process::Command::new("script");
    cmd.arg("-qec").arg(&cmdstr).arg("/dev/null");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = match cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => {
            eprintln!("skipping: script(pty)が利用できない");
            return None;
        }
    };
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{stdin_line}\n").as_bytes())
        .expect("pty stdin write");
    let out = child.wait_with_output().expect("script(pty) 実行");
    let code = out
        .status
        .code()
        .unwrap_or_else(|| out.status.signal().unwrap_or(-1));
    Some((code, String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// TASK-10(S3ストレージ設計・tmp/tasks/tm-10/design_s3_storage.md)結合testの共用fixture。
/// 新体系コマンド(backup push/pull・pin add/rm・manage・list/query/get/diff)を
/// bin subprocess + fake seam(AMAZING_BACKUP_FAKE_S3_STATE)で実行する。
///
/// fake seam側の拡張解釈(設計8.2のversioning模擬を先取り。実装taskでsrc/s3.rsのFakeS3へ
/// 反映される想定の仕掛け。テストはstate dirへの直接読書きでfixtureを組む):
///   - env `FAKE_S3_VERSIONING_DISABLED=1`  GetBucketVersioning応答を無効化する
///     (cli.versioning-disabled-error検証用の注入。未設定はversioning有効)
///   - env `FAKE_S3_OBJECT_SIZE_FOR=<key>:<bytes>`  当該keyのHeadObject応答sizeを上書きする
///     (pin.copy-oversize-uses-uploadpartcopy検証用の注入。object実体は数byteのままで
///     CopyObject 5GB上限のsize分岐だけを駆動する。未設定は実file長)
///   - 新API呼び出しのcalls.log行(実装でFakeS3に追加・既存行と同形式):
///     s3 GetBucketVersioning bucket=<b>
///     s3 CopyObject bucket=<b> key=<k> ...(後付けpin写し)
///     s3 UploadPartCopy bucket=<b> key=<k> part_number=<n>
///     s3 ListObjectVersions bucket=<b> prefix=<p>
///     s3 DeleteObject bucket=<b> key=<k>
///     s3 DeleteObjectVersion bucket=<b> key=<k> version_id=<vid>
///     s3 PutBucketLifecycleConfiguration bucket=<b>
///   - versioning有効時のPutObjectはobjects/<key>へ最新版を置き、版情報(VersionId)は
///     fake内部で管理する(テストはregistryのfile_upload記録経由でversion_idを参照する)
pub struct StorageFixture {
    /// tmp dirの生存保持(Dropで削除されるためパスは各fieldから参照)
    #[allow(dead_code)]
    tmp: TestDir,
    /// AMAZING_BACKUP_HOME(TASK-19 XDG配置: 規定path一括上書きの統合root。
    /// pipeline.toml・aws.toml・registry.jsonl・cache/がこの配下へ解決される)
    pub home: PathBuf,
    /// pipeline.toml(新schema: [layout]/[files]/[metadata]/[behavior]込み)
    pub cfg: PathBuf,
    pub aws_toml: PathBuf,
    /// fake S3 state dir
    pub state: PathBuf,
    /// recipient公開鍵(索引再暗号化用)
    pub public: PathBuf,
    /// 復号用秘密鍵(get・索引読取用)
    pub secret: PathBuf,
    /// ageバイナリ(read_index/rewrite_indexで使用)
    pub age: PathBuf,
    /// 暗号化一時物dir($ABH/cache。XDG配置のcache dir相当・旧data/out)
    pub out_dir: PathBuf,
    pub registry: PathBuf,
    /// auto_path_rootsの既定root
    pub data_root: PathBuf,
    /// auto_path_roots配下に置かない対象の置き場(fallback・衝突構成等)
    pub elsewhere: PathBuf,
    /// fake seam用credentialファイルのpath(dummy値)
    pub credentials: PathBuf,
    /// aws.tomlのroot prefix(既定 "amazing-backup/")
    pub prefix: String,
}

/// StorageFixtureの組立option(未指定は既定値)。
pub struct StorageOpts {
    /// profileのstorage_class(既定 STANDARD=get即時可能。DA経路は DEEP_ARCHIVE)
    pub storage_class: &'static str,
    /// [output].keep_artifacts(既定 "run"=成功後cache dir掃除)
    pub keep_artifacts: &'static str,
    /// [categories]へ追記するtoml断片(unfiledは常に入る)
    pub categories: String,
    /// [files.<name>]手動写像のtoml断片
    pub files: String,
    /// auto_path_rootsへdata_rootの後に追記するroot(順序優先の検証用)
    pub extra_auto_path_roots: Vec<PathBuf>,
    /// [metadata]セクションとしてpipeline.tomlへ追記するtoml断片
    /// (TASK-18: latest_source_pathの3値指定。既定空=セクション省略=plain)
    pub metadata_section: String,
    /// [aws.lifecycle]としてaws.tomlへ追記するtoml断片
    /// (TASK-18: noncurrent_days/keep_versions/mpu_abort_days。既定空=セクション無し)
    pub aws_lifecycle: String,
    /// [categories]セクション自体を出力しない(TASK-18で[categories]は廃止・
    /// 存在時設定エラー2となるための新体系向け組立。既定false=現行実装向け)
    pub no_categories: bool,
}

impl Default for StorageOpts {
    fn default() -> Self {
        // TASK-18後の既定: [categories]は廃止(存在時は設定エラー2)のため出力せず、
        // [aws.lifecycle]3キーは必須のため既定で含める(設計5.15・7章)
        StorageOpts {
            storage_class: "STANDARD",
            keep_artifacts: "run",
            categories: String::new(),
            files: String::new(),
            extra_auto_path_roots: Vec::new(),
            metadata_section: String::new(),
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            no_categories: true,
        }
    }
}

impl StorageFixture {
    /// age/ssh-keygenが使えない環境ではNone(呼び出し側testはskip扱い)。
    pub fn new(tag: &str, opts: StorageOpts) -> Option<Self> {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new(tag);
        // TASK-19 XDG配置: 規定pathはAMAZING_BACKUP_HOMEで$ABH配下へ統合されるため、
        // fixtureはhome dirへ pipeline.toml・aws.toml・registry.jsonl・cache/ を配置する
        // (bin実行には常時AMAZING_BACKUP_HOME=<home>を渡す=registry/cacheの解決先を固定)
        let home = tmp.path().join("abh");
        std::fs::create_dir_all(&home).expect("ABH home dir");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out_dir = home.join("cache");
        let registry = home.join("registry.jsonl");
        let data_root = tmp.path().join("data");
        std::fs::create_dir_all(&data_root).expect("data root dir");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("elsewhere dir");

        // auto_path_roots = [data_root, extra...](data_rootが常に最初に一致するroot)
        let mut roots = vec![data_root.clone()];
        roots.extend(opts.extra_auto_path_roots);
        let roots_toml = roots
            .iter()
            .map(|r| format!("\"{}\"", r.display()))
            .collect::<Vec<_>>()
            .join(", ");
        // [categories]はTASK-18で廃止(存在時設定エラー2)。no_categories=trueの組立では
        // 出力しない(既定false=現行実装向けに[categories.unfiled]を含める)
        let categories_part = if opts.no_categories {
            String::new()
        } else {
            format!("[categories.unfiled]\nkeep = 5\n{}", opts.categories)
        };
        // [output]はTASK-19(XDG配置)で廃止(存在時設定エラー2)。registry・暗号化一時物dir
        // のpathはXDG規定(AMAZING_BACKUP_HOME)解決となりpipeline.tomlからは指定しない。
        // keep_artifactsは[behavior].keep_artifactsへ移設。
        let keep_line = if opts.keep_artifacts.is_empty() {
            String::new()
        } else {
            format!("keep_artifacts = \"{}\"\n", opts.keep_artifacts)
        };
        let cfg_body = format!(
            "[recipients]\npublic_keys = [\"{}\"]\n\
             [behavior]\nkeep_going = false\n{keep_line}\
             [verify]\ndecrypt_key = \"{}\"\n\
             [layout]\nunfiled_category = \"unfiled\"\nauto_path_roots = [{roots_toml}]\n\
             {categories_part}{}{}",
            public.display(),
            secret.display(),
            opts.files,
            opts.metadata_section,
        );
        let cfg = home.join("pipeline.toml");
        write_file(&cfg, cfg_body.as_bytes());

        let credentials = write_fake_credentials(tmp.path());
        let aws_toml = home.join("aws.toml");
        let prefix = "amazing-backup/".to_string();
        let mut body =
            storage_aws_toml_body("it-bucket", &prefix, opts.storage_class, &credentials);
        body.push_str(&opts.aws_lifecycle);
        write_file(&aws_toml, body.as_bytes());

        let state = tmp.path().join("s3state");
        std::fs::create_dir_all(&state).expect("state dir");
        Some(Self {
            tmp,
            home,
            cfg,
            aws_toml,
            state,
            public,
            secret,
            age,
            out_dir,
            registry,
            data_root,
            elsewhere,
            credentials,
            prefix,
        })
    }

    /// aws.tomlを書き換える(prefix形式検証等)。
    pub fn rewrite_aws_toml(&self, body: &str) {
        write_file(&self.aws_toml, body.as_bytes());
    }

    /// pipeline.tomlを書き換える(keep値の上書き検証等)。
    pub fn rewrite_pipeline_toml(&self, body: &str) {
        write_file(&self.cfg, body.as_bytes());
    }

    pub fn pipeline_body(&self) -> String {
        std::fs::read_to_string(&self.cfg).unwrap_or_default()
    }

    /// aws.tomlの現在の本文(書換えの基準取得用)。
    pub fn aws_toml_body(&self) -> String {
        std::fs::read_to_string(&self.aws_toml).unwrap_or_default()
    }

    /// fake seam環境でbinを実行する(共通envに追加envを合わせる)。
    /// TASK-19 XDG配置: 常にAMAZING_BACKUP_HOME=<home>を渡し規定path解決を固定する
    /// (registry/cacheがhome配下へ解決される)。
    pub fn bin(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        let state_s = self.state.display().to_string();
        let home_s = self.home.display().to_string();
        let mut envs: Vec<(&str, &str)> = vec![
            ("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str()),
            ("AMAZING_BACKUP_HOME", home_s.as_str()),
        ];
        envs.extend_from_slice(env);
        run_bin_env(args, &envs)
    }

    /// `backup push <paths...>`(+追加options/env)。
    pub fn push(
        &self,
        paths: &[&Path],
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        self.push_env(paths, extra, env)
    }

    /// pushのenv指定版(失敗注入等)。
    pub fn push_env(
        &self,
        paths: &[&Path],
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut args: Vec<String> = vec![
            "backup".into(),
            "push".into(),
            "--config".into(),
            self.cfg.display().to_string(),
            "--aws-config".into(),
            self.aws_toml.display().to_string(),
        ];
        for p in paths {
            args.push(p.display().to_string());
        }
        args.extend(extra.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.bin(&refs, env)
    }

    /// `backup push <paths...> --from-list <toml>`(TASK-18のtitle入力経路。
    /// PATH位置引数1個以上必須・positionalとfrom-listのpath重複は解決エラーのため、
    /// positionalにはskip対象等の別pathを渡す)。
    pub fn push_from_list(
        &self,
        positional: &[&Path],
        entries: &[(&Path, Option<&str>)],
        extra: &[&str],
    ) -> (i32, String, String) {
        let list = write_from_list(&self.tmp_path(), entries);
        let mut extra_owned: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        extra_owned.push("--from-list".into());
        extra_owned.push(list.display().to_string());
        let refs: Vec<&str> = extra_owned.iter().map(|s| s.as_str()).collect();
        self.push(positional, &refs, &[])
    }

    /// `backup push <paths...> --from-list <toml>`(+追加options/env付き。
    /// 失敗注入などenvを伴う実行用)。
    pub fn push_from_list_env(
        &self,
        positional: &[&Path],
        entries: &[(&Path, Option<&str>)],
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let list = write_from_list(&self.tmp_path(), entries);
        let mut extra_owned: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        extra_owned.push("--from-list".into());
        extra_owned.push(list.display().to_string());
        let refs: Vec<&str> = extra_owned.iter().map(|s| s.as_str()).collect();
        self.push_env(positional, &refs, env)
    }

    /// TestDirのpath(内部保持。tmpはprivateのための参照用)。
    fn tmp_path(&self) -> PathBuf {
        self.cfg
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default()
    }

    /// `get <name>`(+追加options)。
    pub fn get(&self, name: &str, extra: &[&str]) -> (i32, String, String) {
        self.get_env(name, extra, &[])
    }

    pub fn get_env(
        &self,
        name: &str,
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut args: Vec<String> = vec![
            "get".into(),
            name.into(),
            "--config".into(),
            self.cfg.display().to_string(),
            "--aws-config".into(),
            self.aws_toml.display().to_string(),
            "--key".into(),
            self.secret.display().to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.bin(&refs, env)
    }

    /// `query <term>`(+追加options)。
    pub fn query(&self, term: &str, extra: &[&str]) -> (i32, String, String) {
        let mut args: Vec<String> = vec![
            "query".into(),
            term.into(),
            "--config".into(),
            self.cfg.display().to_string(),
            "--aws-config".into(),
            self.aws_toml.display().to_string(),
            "--key".into(),
            self.secret.display().to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.bin(&refs, &[])
    }

    /// `list <sub>`(+追加options)。
    pub fn list(&self, sub: &str, extra: &[&str]) -> (i32, String, String) {
        self.list_env(sub, extra, &[])
    }

    pub fn list_env(
        &self,
        sub: &str,
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut args: Vec<String> = vec![
            "list".into(),
            sub.into(),
            "--config".into(),
            self.cfg.display().to_string(),
            "--aws-config".into(),
            self.aws_toml.display().to_string(),
            "--key".into(),
            self.secret.display().to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.bin(&refs, env)
    }

    /// `diff`(+追加options)。
    pub fn diff(&self, extra: &[&str]) -> (i32, String, String) {
        let mut args: Vec<String> = vec![
            "diff".into(),
            "--config".into(),
            self.cfg.display().to_string(),
            "--aws-config".into(),
            self.aws_toml.display().to_string(),
            "--key".into(),
            self.secret.display().to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.bin(&refs, &[])
    }

    /// `pin add <name>` / `pin rm <name>`(+追加options)。
    pub fn pin(&self, sub: &str, name: &str, extra: &[&str]) -> (i32, String, String) {
        self.pin_env(sub, name, extra, &[])
    }

    pub fn pin_env(
        &self,
        sub: &str,
        name: &str,
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        let mut args: Vec<String> = vec![
            "pin".into(),
            sub.into(),
            name.into(),
            "--config".into(),
            self.cfg.display().to_string(),
            "--aws-config".into(),
            self.aws_toml.display().to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.bin(&refs, env)
    }

    /// `manage <sub>`(+追加options)。
    pub fn manage(&self, sub: &str, extra: &[&str]) -> (i32, String, String) {
        self.manage_env(sub, extra, &[])
    }

    pub fn manage_env(
        &self,
        sub: &str,
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> (i32, String, String) {
        // `manage index rebuild`等のnested subcommandは共通optionsの前に置く
        let mut args: Vec<String> = vec!["manage".into(), sub.into()];
        args.extend(extra.iter().map(|s| s.to_string()));
        args.push("--config".into());
        args.push(self.cfg.display().to_string());
        args.push("--aws-config".into());
        args.push(self.aws_toml.display().to_string());
        args.push("--key".into());
        args.push(self.secret.display().to_string());
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.bin(&refs, env)
    }

    /// `backup pull`(+追加options)。
    pub fn backup_pull(&self, extra: &[&str]) -> (i32, String, String) {
        let mut args: Vec<String> = vec![
            "backup".into(),
            "pull".into(),
            "--config".into(),
            self.cfg.display().to_string(),
            "--aws-config".into(),
            self.aws_toml.display().to_string(),
            "--key".into(),
            self.secret.display().to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.bin(&refs, &[])
    }

    /// fake backendのAPI呼び出し記録(calls.log。未実行なら空文字列)。
    pub fn calls(&self) -> String {
        read_calls(&self.state)
    }

    /// file objectのkey(<prefix>/<category>[/<rel_path>]/<name>/default.age)。
    pub fn file_key(&self, category: &str, rel_path: Option<&str>, name: &str) -> String {
        match rel_path {
            Some(rel) if !rel.is_empty() => {
                format!("{}{}/{}/{}/default.age", self.prefix, category, rel, name)
            }
            _ => format!("{}{}/{}/default.age", self.prefix, category, name),
        }
    }

    /// pin objectのkey(<prefix>/<category>[/<rel_path>]/<name>/<pin>.age)。
    pub fn pin_key(&self, category: &str, rel_path: Option<&str>, name: &str, pin: &str) -> String {
        match rel_path {
            Some(rel) if !rel.is_empty() => {
                format!("{}{}/{}/{}/{}.age", self.prefix, category, rel, name, pin)
            }
            _ => format!("{}{}/{}/{}.age", self.prefix, category, name, pin),
        }
    }

    /// TASK-18 data領域のfile object key({prefix}data/{uuid}/default.age)。
    pub fn data_key(&self, uuid: &str) -> String {
        format!("{}data/{}/default.age", self.prefix, uuid)
    }

    /// TASK-18 data領域のpin object key({prefix}data/{uuid}/{pin}.age)。
    pub fn data_pin_key(&self, uuid: &str, pin: &str) -> String {
        format!("{}data/{}/{}.age", self.prefix, uuid, pin)
    }

    /// TASK-18 metadata object key({prefix}data/{uuid}/metadata_{title}.json)。
    pub fn metadata_key(&self, uuid: &str, title: &str) -> String {
        format!("{}data/{}/metadata_{}.json", self.prefix, uuid, title)
    }

    /// metadata objectの内容をJSONで読む(不在はNone)。
    pub fn read_metadata_doc(&self, uuid: &str, title: &str) -> Option<serde_json::Value> {
        let bytes = std::fs::read(self.object_path(&self.metadata_key(uuid, title))).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// 索引objectのkey。
    pub fn index_key(&self) -> String {
        format!("{}sys/index.jsonl.age", self.prefix)
    }

    /// state dir内のobject実体のpath。
    pub fn object_path(&self, key: &str) -> PathBuf {
        let mut p = self.state.join("objects");
        for seg in key.split('/') {
            p = p.join(seg);
        }
        p
    }

    /// state dirへobject実体を直接配置する(diffのunexpected対象等のfixture)。
    pub fn seed_object(&self, key: &str, data: &[u8]) {
        write_file(&self.object_path(key), data);
    }

    /// 復元job状態のmetaを直接書く("ongoing"=進行中・それ以外=復元済み)。
    pub fn seed_restore(&self, key: &str, state: &str) {
        let mut p = self.state.join("restore");
        for seg in key.split('/') {
            p = p.join(seg);
        }
        write_file(&p, state.as_bytes());
    }

    /// 索引objectを復号してJSONL行をJSON Value列で読む。
    pub fn read_index(&self) -> Vec<serde_json::Value> {
        let plain = age_decrypt(
            &self.age,
            &self.object_path(&self.index_key()),
            &self.secret,
        );
        let text = String::from_utf8(plain).expect("索引平文はUTF-8");
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("索引行はJSON"))
            .collect()
    }

    /// JSONL行を再暗号化して索引objectへ書き戻す(版日付操作等のfixture編集)。
    pub fn rewrite_index(&self, lines: &[serde_json::Value]) {
        let mut text = String::new();
        for l in lines {
            text.push_str(&serde_json::to_string(l).expect("索引行serialize"));
            text.push('\n');
        }
        age_encrypt_bytes(
            &self.age,
            &[self.public.clone()],
            text.as_bytes(),
            &self.object_path(&self.index_key()),
        );
    }

    /// registry.jsonlの行をJSON Value列で読む。
    pub fn registry_values(&self) -> Vec<serde_json::Value> {
        read_registry_lines(&self.registry)
    }
}

/// TASK-10形式のaws.toml本文([aws.profiles.dev]にprefix付き)。
pub fn storage_aws_toml_body(
    bucket: &str,
    prefix: &str,
    storage_class: &str,
    credentials_file: &Path,
) -> String {
    format!(
        "[aws]\nactive_profile = \"dev\"\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"{bucket}\"\nregion = \"ap-northeast-1\"\nprefix = \"{prefix}\"\nstorage_class = \"{storage_class}\"\ncredentials_file = \"{}\"\n",
        credentials_file.display()
    )
}

/// 索引のfile entryをnameで引く。
pub fn index_file_entry<'a>(lines: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    lines
        .iter()
        .find(|v| {
            v.get("record_type").and_then(|r| r.as_str()) == Some("file")
                && v.get("name").and_then(|n| n.as_str()) == Some(name)
        })
        .unwrap_or_else(|| panic!("索引にfile entry({name})が無い: {lines:?}"))
}

/// 索引file entryのversions[].version_id(格納順=新しい順)。
pub fn index_version_ids(entry: &serde_json::Value) -> Vec<String> {
    entry
        .get("versions")
        .and_then(|v| v.as_array())
        .expect("versions配列")
        .iter()
        .map(|v| {
            v.get("version_id")
                .and_then(|i| i.as_str())
                .expect("version_id")
                .to_string()
        })
        .collect()
}

/// calls.logから`version_id=<vid>`を取り出す(DeleteObjectVersion等の行)。
pub fn version_ids_in_calls(calls: &str, op_prefix: &str, key: &str) -> Vec<String> {
    calls
        .lines()
        .filter(|l| l.starts_with(op_prefix) && l.contains(&format!("key={key}")))
        .filter_map(|l| {
            l.split_whitespace()
                .find(|f| f.starts_with("version_id="))
                .map(|f| f.trim_start_matches("version_id=").to_string())
        })
        .collect()
}

/// registryのfile_upload記録から当該s3_keyのversion_idを記録順(古い順)に読む。
pub fn registry_upload_version_ids(registry: &[serde_json::Value], s3_key: &str) -> Vec<String> {
    registry
        .iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                && r.get("s3_key").and_then(|k| k.as_str()) == Some(s3_key)
        })
        .filter_map(|r| {
            r.get("version_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

/// registryのfile_write記録からsource_pathでuuidを引く(同一pathに複数記録がある
/// 場合は最新=最後の記録。TASK-18では内容変更で別entityが発行されるため)。
pub fn uuid_of_path(fx: &StorageFixture, path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).expect("realpath");
    fx.registry_values()
        .into_iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("source_path").and_then(|p| p.as_str())
                    == Some(canonical.to_string_lossy().as_ref())
        })
        .last()
        .and_then(|r| r.get("uuid").and_then(|u| u.as_str()).map(String::from))
        .unwrap_or_else(|| panic!("file_write.uuid({})", canonical.display()))
}

/// 当該bucketのfile_upload記録からuuidを引く(複数bucket構成でのkey組立用)。
pub fn uuid_of_bucket_upload(fx: &StorageFixture, bucket: &str) -> String {
    fx.registry_values()
        .into_iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                && r.get("s3_bucket").and_then(|b| b.as_str()) == Some(bucket)
        })
        .and_then(|r| r.get("uuid").and_then(|u| u.as_str()).map(String::from))
        .unwrap_or_else(|| panic!("file_upload.uuid(bucket={bucket})"))
}

/// crash残存(file_upload記録の除去)→再pushのre-put経路(設計4.4(ii)(iii))で同一entityの
/// default.ageへ版を重ねたのち、除去した記録を元の位置へ復元する(各版のfile_upload記録が
/// 残る通常運用相当のregistry状態。S3には全版が実在する)。extraは再pushへの追加option
/// (--pin等)。戻り値は時系列順(古い方から)のversion_id。
pub fn add_version_via_reput(fx: &StorageFixture, path: &Path, extra: &[&str]) -> Vec<String> {
    let uuid = uuid_of_path(fx, path);
    let key = fx.data_key(&uuid);
    let before = read_registry_lines(&fx.registry);
    let kept: Vec<serde_json::Value> = before
        .iter()
        .filter(|r| {
            !(r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                && r.get("s3_key").and_then(|k| k.as_str()) == Some(key.as_str()))
        })
        .cloned()
        .collect();
    let mut text = String::new();
    for r in &kept {
        text.push_str(&serde_json::to_string(r).expect("registry行serialize"));
        text.push('\n');
    }
    write_file(&fx.registry, text.as_bytes());
    let (code, stdout, stderr) = fx.push(&[path], extra, &[]);
    assert_eq!(code, 0, "re-put(版追加): {stdout}{stderr}");
    // re-putの追記分(kept行数以降)を退避済み記録の後ろへ繋いで復元
    let after = read_registry_lines(&fx.registry);
    let tail: Vec<serde_json::Value> = after[kept.len()..].to_vec();
    let mut restored = before;
    restored.extend(tail);
    let mut text = String::new();
    for r in &restored {
        text.push_str(&serde_json::to_string(r).expect("registry行serialize"));
        text.push('\n');
    }
    write_file(&fx.registry, text.as_bytes());
    // 復元後のregistry(各版のfile_upload記録)を索引へ反映させるため、all-skip runで
    // run末尾の索引再構築を走らせる(成功run末尾で常時再構築・設計2.2/H-2)
    let (code, stdout, stderr) = fx.push(&[path], &[], &[]);
    assert_eq!(code, 0, "復元後のall-skip再構築push: {stdout}{stderr}");
    registry_upload_version_ids(&read_registry_lines(&fx.registry), &key)
}

/// registryのrecord_typeの記録数。
pub fn registry_count(registry: &[serde_json::Value], record_type: &str) -> usize {
    registry
        .iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some(record_type))
        .count()
}

// ---------------------------------------------------------------------------
// TASK-18(S3キー設計: UUID+versioning・tmp/tasks/tm-18/design_key-uuid-versioning.md)
// のテスト先行で追加したhelper群。

/// [aws.lifecycle]のtoml断片(3キー)。
pub fn aws_lifecycle_toml(noncurrent_days: u32, keep_versions: u32, mpu_abort_days: u32) -> String {
    format!(
        "[aws.lifecycle]\nnoncurrent_days = {noncurrent_days}\nkeep_versions = {keep_versions}\nmpu_abort_days = {mpu_abort_days}\n"
    )
}

/// from-list tomlを書いてpathを返す(TASK-18: [[targets]]のtitle欄。Noneはtitle省略)。
pub fn write_from_list(dir: &Path, entries: &[(&Path, Option<&str>)]) -> PathBuf {
    let mut body = String::new();
    for (path, title) in entries {
        body.push_str(&format!("[[targets]]\npath = \"{}\"\n", path.display()));
        if let Some(t) = title {
            body.push_str(&format!("title = \"{t}\"\n"));
        }
    }
    let p = dir.join("from-list.toml");
    write_file(&p, body.as_bytes());
    p
}

/// pty経由でbackup pushを実行し、新規発行対象のtitleをstdinへ与える
/// (TASK-18: 新規uuid発行はtitle必須・prompt入力。非ttyの新規発行は対象エラー3の
/// ため、testからの新規発行setupはこの経路で行う。stdin_linesはprompt出現順に
/// 改行区切りで並べる。`script`が不在ならNone=呼び出し側testはskip扱い)。
pub fn push_titled_pty(
    fx: &StorageFixture,
    paths: &[&Path],
    stdin_lines: &str,
    extra: &[&str],
) -> Option<(i32, String)> {
    let mut args: Vec<String> = vec![
        "backup".into(),
        "push".into(),
        "--config".into(),
        fx.cfg.display().to_string(),
        "--aws-config".into(),
        fx.aws_toml.display().to_string(),
    ];
    for p in paths {
        args.push(p.display().to_string());
    }
    args.extend(extra.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let state_s = fx.state.display().to_string();
    let home_s = fx.home.display().to_string();
    run_bin_pty_stdin(
        &refs,
        &[
            ("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str()),
            ("AMAZING_BACKUP_HOME", home_s.as_str()),
        ],
        stdin_lines,
    )
}

/// 索引のentity entryをuuid(またはtitle)で引く(TASK-18 schema=2索引の実体単位entry)。
pub fn index_entity_by<'a>(
    lines: &'a [serde_json::Value],
    field: &str,
    value: &str,
) -> &'a serde_json::Value {
    lines
        .iter()
        .find(|v| {
            v.get("record_type").and_then(|r| r.as_str()) == Some("file")
                && v.get(field).and_then(|n| n.as_str()) == Some(value)
        })
        .unwrap_or_else(|| panic!("索引にentity entry({field}={value})が無い: {lines:?}"))
}

/// entity entryのlocations配列(索引にlocationsが無い場合は空)。
pub fn entity_locations(entry: &serde_json::Value) -> Vec<serde_json::Value> {
    entry
        .get("locations")
        .and_then(|l| l.as_array())
        .cloned()
        .unwrap_or_default()
}

/// entity entryのpins配列(索引にpinsが無い場合は空)。
pub fn entity_pins(entry: &serde_json::Value) -> Vec<serde_json::Value> {
    entry
        .get("pins")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default()
}

/// registry.jsonlへ1行追記する(fixture構築・crash残存やsweep/location_removeの模擬)。
pub fn append_registry_line(path: &Path, value: &serde_json::Value) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("registry.jsonl({})へ追記: {e}", path.display()));
    writeln!(f, "{value}").expect("registry行の追記");
}

/// registry.jsonlを行filterで書き換える(当該recordの除去等のfixture編集)。
pub fn rewrite_registry_lines(path: &Path, keep: impl Fn(&serde_json::Value) -> bool) {
    let kept: Vec<serde_json::Value> = read_registry_lines(path)
        .into_iter()
        .filter(|r| keep(r))
        .collect();
    let mut text = String::new();
    for r in kept {
        text.push_str(&serde_json::to_string(&r).expect("registry行serialize"));
        text.push('\n');
    }
    write_file(path, text.as_bytes());
}

/// versions state fileから指定版の行を除去する(lifecycleで版が消滅した状態の模擬。
/// 現行版を行除去した場合はobjects/<key>実体も削除する)。
pub fn drop_version_state(state_dir: &Path, key: &str, version_id: &str) {
    let versions_path = state_dir.join("versions").join(key);
    let text = std::fs::read_to_string(&versions_path)
        .unwrap_or_else(|e| panic!("versions state({})の読込: {e}", versions_path.display()));
    let mut out = String::new();
    let mut removed_current = false;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if line.split('\t').next() == Some(version_id) {
            removed_current = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    assert!(
        removed_current,
        "対象版({version_id})がversions stateに無い: {text}"
    );
    write_file(&versions_path, out.as_bytes());
    let _ = std::fs::remove_file(state_dir.join("object-versions").join(key).join(version_id));
}

/// registryのrecordのうちfield=valueのものを取り出す。
pub fn registry_records<'a>(
    registry: &'a [serde_json::Value],
    record_type: &str,
    field: &str,
    value: &str,
) -> Vec<&'a serde_json::Value> {
    registry
        .iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some(record_type)
                && r.get(field).and_then(|v| v.as_str()) == Some(value)
        })
        .collect()
}
