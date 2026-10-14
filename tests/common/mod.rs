//! 結合test共用の補助(設計1章: ageバイナリ有無判定・鍵pair生成・tmp dir fixture)。
//! 実ageが不在の環境では当該testを早期returnでskip扱いにする(test failureとしない)。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// テスト終了時に再帰削除する一時dir(std::env::temp_dir配下・repo外)。
pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(tag: &str) -> TestDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("backup-remote-it-{tag}-{}-{n}", std::process::id()));
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
        .map(|path| {
            std::env::split_paths(&path).any(|dir| is_executable(&dir.join("ssh-keygen")))
        })
        .unwrap_or(false)
}

/// throwawayのed25519鍵pairをdirに生成し(pub, secret)を返す。
pub fn generate_ed25519_keypair(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    let stem = dir.join(name);
    let secret = stem.clone();
    let public = dir.join(format!("{name}.pub"));
    let out = std::process::Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-C", "backup-remote-it", "-f"])
        .arg(&stem)
        .output()
        .expect("ssh-keygen実行");
    assert!(out.status.success(), "ssh-keygen失敗: {}", String::from_utf8_lossy(&out.stderr));
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
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::inherit()).spawn().expect("age spawn");
    child.stdin.take().unwrap().write_all(data).expect("age stdin write");
    let status = child.wait().expect("age wait");
    assert!(status.success(), "age暗号化(fixture)失敗");
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(data);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_b64(data: &[u8]) -> String {
    use base64::Engine as _;
    use sha2::Digest;
    let d = sha2::Sha256::digest(data);
    base64::engine::general_purpose::STANDARD.encode(d)
}

/// 現在のJST暦日(YYYY-MM-DD)。
pub fn jst_today() -> String {
    let jst = chrono::FixedOffset::east_opt(9 * 3600).expect("JST offset");
    chrono::Utc::now().with_timezone(&jst).format("%Y-%m-%d").to_string()
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

/// bin backup-remoteをsubprocess実行し(exit code, stdout, stderr)を返す。
pub fn run_bin(args: &[&str]) -> (i32, String, String) {
    run_bin_env(args, &[])
}

/// 環境変数を上書きしてbinを実行する(BACKUP_REMOTE_FAKE_S3_STATE seam・PATH制限等)。
pub fn run_bin_env(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    use std::os::unix::process::ExitStatusExt as _;
    let bin = env!("CARGO_BIN_EXE_backup-remote");
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("backup-remote bin 実行");
    (
        out.status.code().unwrap_or_else(|| out.status.signal().unwrap_or(-1)),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// 環境変数に加えて作業dir(cwd)も指定してbinを実行する(restore系の「pipeline.toml不在の
/// 作業dirで実行」等、cwdが関与する経路の検証用。cargo testの並列testは同一process内で
/// 走るため、test process側のcwdは変更せずsubprocessのcwdのみ変更する)。
pub fn run_bin_cwd(args: &[&str], env: &[(&str, &str)], cwd: &Path) -> (i32, String, String) {
    use std::os::unix::process::ExitStatusExt as _;
    let bin = env!("CARGO_BIN_EXE_backup-remote");
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("backup-remote bin 実行");
    (
        out.status.code().unwrap_or_else(|| out.status.signal().unwrap_or(-1)),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// fake S3 backend seam(TASK-9設計4章・10.3)の検証構造。
/// bin側(cli.rs)が`BACKUP_REMOTE_FAKE_S3_STATE=<dir>`環境変数を検出するとS3 backendを
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

/// credential値非露出検証用: 各値へ一意の検出マーカーを埋めたcredentialファイルを書く。
/// (access_key_id等の実際の値ではなくマーカー文字列そのものが出力に現れないことを検証する)
pub fn write_marked_credentials(dir: &Path) -> PathBuf {
    let p = dir.join("aws-credentials.toml");
    write_file(
        &p,
        b"[credentials]\naccess_key_id = \"LEAKMARK-AKID-9f8e7d6c\"\nsecret_access_key = \"LEAKMARK-SECRET-5a4b3c2d1e\"\nsession_token = \"LEAKMARK-TOKEN-0f9e8d7c\"\n",
    );
    p
}

/// fake seam検証用のaws.toml本文(bucket/region/credentials_fileは必須キー。
/// storage_classはOptionで省略可=既定DEEP_ARCHIVE)。credentials_fileのpath解決は
/// cwd基準の相対path仕様だが、テストは絶対pathで指定する(run_bin_envのcwdはrepo root)。
pub fn aws_toml_body(
    bucket: &str,
    region: &str,
    credentials_file: &Path,
    storage_class: Option<&str>,
) -> String {
    let sc = storage_class
        .map(|s| format!("storage_class = \"{s}\"\n"))
        .unwrap_or_default();
    format!(
        "[aws]\nbucket = \"{bucket}\"\nregion = \"{region}\"\ncredentials_file = \"{}\"\n{sc}",
        credentials_file.display()
    )
}

/// 結合test用のpipeline.tomlを書く。
pub fn write_pipeline_toml(
    dir: &Path,
    recipients: &[PathBuf],
    out_dir: &Path,
    registry: &Path,
    decrypt_key: Option<&Path>,
) -> std::path::PathBuf {
    let keys = recipients
        .iter()
        .map(|p| format!("\"{}\"", p.display()))
        .collect::<Vec<_>>()
        .join(", ");
    let body = format!(
        "[recipients]\npublic_keys = [{keys}]\n\
         [output]\ndir = \"{}\"\nregistry = \"{}\"\n\
         [behavior]\nkeep_going = false\n\
         [verify]\ndecrypt_key = \"{}\"\n",
        out_dir.display(),
        registry.display(),
        decrypt_key.map(|p| p.display().to_string()).unwrap_or_default(),
    );
    let p = dir.join("pipeline.toml");
    write_file(&p, body.as_bytes());
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
