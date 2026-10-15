//! age subprocess実行(1pass stream)・復号検証(設計5章crypto.rs・6章)。

use crate::errors::{BackupError, BackupResult};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// 読込・書込チャンクサイズ(6.2。平文・暗号文とも4MiB)。
const CHUNK: usize = 4 * 1024 * 1024;

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// PATH上の`age`を検索する(不在は設定エラー。`age --version`は毎回叩かない)。
pub fn find_age() -> BackupResult<PathBuf> {
    let path_env = std::env::var_os("PATH").ok_or_else(|| {
        BackupError::Config("PATH環境変数が無い(ageバイナリを検索できない)".to_string())
    })?;
    for dir in std::env::split_paths(&path_env) {
        let cand = dir.join("age");
        if is_executable_file(&cand) {
            return Ok(cand);
        }
    }
    Err(BackupError::Config(
        "ageバイナリがPATH上に見つからない(encrypt/verifyにはageが必要)".to_string(),
    ))
}

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(m) => m.is_file() && (m.permissions().mode() & 0o111) != 0,
        Err(_) => false,
    }
}

/// encrypt_stream / encrypt_bytesの共通結果(6.2)。
#[derive(Debug)]
pub struct EncryptResult {
    pub source_size: u64,
    pub age_size: u64,
    /// 平文のSHA-256(hex 64桁・1pass計算)
    pub source_sha256: String,
    /// 暗号文のSHA-256(hex 64桁・書込同時計算)
    pub age_sha256: String,
    /// recipient指紋列(`SHA256:<base64 nopad>`・設定順)
    pub recipients: Vec<String>,
}

/// 1pass stream暗号化の中核(6章)。`.part`書込→`std::fs::rename`。
/// 平文sha256・暗号文sha256を同時計算する。age異常終了時は`BackupError::Process`
/// (`.part`削除・既存`.age`は保持)。
pub fn encrypt_stream(
    src: &Path,
    dest: &Path,
    public_keys: &[PathBuf],
) -> BackupResult<EncryptResult> {
    encrypt_impl(&EncryptInput::File(src), dest, public_keys)
}

/// manifest暗号化用(6章)。stdin投入源のみ`&[u8]`に差し替えた同じ下請け構成。
/// 戻り値はage_sha256。
pub fn encrypt_bytes(data: &[u8], dest: &Path, public_keys: &[PathBuf]) -> BackupResult<String> {
    Ok(encrypt_impl(&EncryptInput::Bytes(data), dest, public_keys)?.age_sha256)
}

/// 成功経路以外の全exit pathで`.part`を削除する統一入口(6.2)。
/// 内部処理が成功した時点でrename済みのため`.part`は存在せず、削除はno-opになる。
fn encrypt_impl(
    input: &EncryptInput<'_>,
    dest: &Path,
    public_keys: &[PathBuf],
) -> BackupResult<EncryptResult> {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    match encrypt_impl_inner(input, dest, public_keys, &part) {
        Ok(v) => Ok(v),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// 暗号化対象の入力源(6.2。manifestのみ`&[u8]`)。
enum EncryptInput<'a> {
    File(&'a Path),
    Bytes(&'a [u8]),
}

/// 6.2の編成: stdin投入thread(平文read+SHA-256更新+age stdin書込)と
/// stderr drain threadを並行させ、main threadはage stdout読込+.part書込+SHA-256更新。
/// 正常終了時のみflush+sync→ヘッダ照合(6.3)→rename。
/// main threadがstdout読込loopを中断する際はstdout handleをdropしてから`proc.wait()`する
/// (読込端が生存したままwaitすると、ageがstdoutへの書込でblockし続けデッドロックする。
/// dropによりageはSIGPIPE/EPIPEで終了へ向かい、stdin・stderr threadのblockも解放される)。
fn encrypt_impl_inner(
    input: &EncryptInput<'_>,
    dest: &Path,
    public_keys: &[PathBuf],
    part: &Path,
) -> BackupResult<EncryptResult> {
    let age_path = find_age()?;
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                BackupError::Process(format!("出力dir作成失敗 ({}): {e}", parent.display()))
            })?;
        }
    }
    // 残存.partはtruncate上書きで再利用する(7.3)
    let part_file = std::fs::File::create(&part)
        .map_err(|e| BackupError::Process(format!(".part作成失敗 ({}): {e}", part.display())))?;
    let mut part_writer = std::io::BufWriter::with_capacity(CHUNK, part_file);

    let mut cmd = std::process::Command::new(&age_path);
    for k in public_keys {
        cmd.arg("-R").arg(k);
    }
    let mut proc = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| BackupError::Config(format!("age subprocessの起動に失敗: {e}")))?;
    let mut stdin = proc.stdin.take().expect("age stdin");
    let mut stdout = proc.stdout.take().expect("age stdout");
    let stderr = proc.stderr.take().expect("age stderr");

    // 3方向pipeを扱う編成(6.2)。scopeで'staticでない入力源もそのまま渡す。
    let run = std::thread::scope(|scope| -> BackupResult<(u64, String, u64, String)> {
        let stderr_handle = scope.spawn(move || {
            let mut drained: Vec<u8> = Vec::new();
            let _ = std::io::BufReader::new(stderr).read_to_end(&mut drained);
            drained
        });
        let stdin_handle = scope.spawn(move || -> std::io::Result<(u64, String)> {
            let mut hasher = Sha256::new();
            let mut total: u64 = 0;
            let mut buf = vec![0u8; CHUNK];
            match input {
                EncryptInput::File(path) => {
                    let mut f = std::fs::File::open(path)?;
                    loop {
                        let n = f.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        hasher.update(&buf[..n]);
                        total += n as u64;
                        stdin.write_all(&buf[..n])?;
                    }
                }
                EncryptInput::Bytes(data) => {
                    for chunk in data.chunks(CHUNK) {
                        hasher.update(chunk);
                        total += chunk.len() as u64;
                        stdin.write_all(chunk)?;
                    }
                }
            }
            stdin.flush()?;
            drop(stdin); // pipeのwrite側close = EOF通知
            Ok((total, hex(&hasher.finalize())))
        });

        // main thread: age stdoutを.partへ書きながらSHA-256更新
        let mut age_hasher = Sha256::new();
        let mut age_total: u64 = 0;
        let mut read_err: Option<std::io::Error> = None;
        let mut buf = vec![0u8; CHUNK];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    age_hasher.update(&buf[..n]);
                    age_total += n as u64;
                    if let Err(e) = part_writer.write_all(&buf[..n]) {
                        read_err = Some(e);
                        break;
                    }
                }
                Err(e) => {
                    read_err = Some(e);
                    break;
                }
            }
        }
        // 読込loop中断時(.part書込失敗・読込Err含む)もstdoutの読込端を閉じてから
        // waitする(指摘1: 読込端が生存するとageのstdout書込がblockしwaitが返らない)
        drop(stdout);
        let status = proc
            .wait()
            .map_err(|e| BackupError::Process(format!("ageのwaitに失敗: {e}")))?;
        let stdin_res = stdin_handle
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("stdin投入threadがpanicした")));
        let stderr_bytes = stderr_handle.join().unwrap_or_default();
        let stderr_text = String::from_utf8_lossy(&stderr_bytes).trim().to_string();

        let (source_size, source_sha256) = stdin_res.map_err(|e| {
            BackupError::Process(format!(
                "平文の読込/age stdin書込に失敗: {e}; age stderr: {stderr_text}"
            ))
        })?;
        if let Some(e) = read_err {
            return Err(BackupError::Process(format!(
                "暗号文の書込に失敗: {e}; age stderr: {stderr_text}"
            )));
        }
        if !status.success() {
            return Err(BackupError::Process(format!(
                "ageが異常終了した(status={status}); age stderr: {stderr_text}"
            )));
        }
        Ok((
            source_size,
            source_sha256,
            age_total,
            hex(&age_hasher.finalize()),
        ))
    });
    let (source_size, source_sha256, age_size, age_sha256) = run?;

    // 正常終了時のみ: flush+sync → ヘッダ照合(6.3) → 原子rename
    // (以降の失敗経路を含め、.part削除はencrypt_implの統一入口が担う)
    part_writer
        .flush()
        .map_err(|e| BackupError::Process(format!(".partのflushに失敗: {e}")))?;
    part_writer
        .get_ref()
        .sync_all()
        .map_err(|e| BackupError::Process(format!(".partのsyncに失敗: {e}")))?;
    drop(part_writer);

    // recipient鍵のparseと指紋計算(成功経路でのみ行う)
    let mut recipients = Vec::with_capacity(public_keys.len());
    let mut pubkeys = Vec::with_capacity(public_keys.len());
    for k in public_keys {
        let pk = crate::fingerprint::parse_ssh_pubkey(k)?;
        recipients.push(crate::fingerprint::fingerprint(&pk));
        pubkeys.push(pk);
    }
    // 暗号化直後の自己検証(6.3。rename前の.partに対して実施)
    let header = crate::fingerprint::parse_age_header(part)?;
    crate::fingerprint::verify_recipients_for_keys(&header, &pubkeys)?;

    std::fs::rename(part, dest).map_err(|e| {
        BackupError::Process(format!(
            "renameに失敗({} -> {}): {e}",
            part.display(),
            dest.display()
        ))
    })?;
    Ok(EncryptResult {
        source_size,
        age_size,
        source_sha256,
        age_sha256,
        recipients,
    })
}

/// `age -d -i <key>`のstdoutをストリーム読みして平文sha256を返す(平文を保持しない)。
pub fn decrypt_sha256(age_path: &Path, key: &Path) -> BackupResult<String> {
    let (_, _, sha) = decrypt_impl(age_path, key, false, None)?;
    sha.ok_or_else(|| BackupError::Internal("decrypt_sha256: sha256未計算".to_string()))
}

/// verify --roundtripのmanifest復号読込用(2.4-3・4章注記)。
/// `decrypt_sha256`と同じstream構成で、内容を`Vec<u8>`として返す。
pub fn decrypt_bytes(age_path: &Path, key: &Path) -> BackupResult<Vec<u8>> {
    let (bytes, _, _) = decrypt_impl(age_path, key, true, None)?;
    bytes.ok_or_else(|| BackupError::Internal("decrypt_bytes: 平文未収集".to_string()))
}

/// 復号のファイル書き出し結果(restore download・3.8)。
#[derive(Debug)]
pub struct DecryptFileResult {
    /// 平文のbyte数
    pub plain_size: u64,
    /// 平文のSHA-256(hex 64桁・書込同時計算)
    pub plain_sha256: String,
}

/// 復号して`dest`へ書き出す(restore download・3.4・3.8)。
/// `.part`書込→`std::fs::rename`の原子性はencryptと対称(6.2)。失敗経路では
/// `.part`を削除し既存`dest`は保持する。戻り値は平文sizeとsha256。
pub fn decrypt_to_file(
    age_path: &Path,
    key: &Path,
    dest: &Path,
) -> BackupResult<DecryptFileResult> {
    let part = PathBuf::from(format!("{}.part", dest.display()));
    match decrypt_impl(age_path, key, false, Some((&part, dest))) {
        Ok((_, size, sha)) => Ok(DecryptFileResult {
            plain_size: size,
            plain_sha256: sha.expect("decrypt_to_file: sha256未計算"),
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// 復号の中核(6章末尾)。stdin threadで.ageをageへ流し、main threadでstdoutを読む。
/// `collect`がtrueなら平文を`Vec<u8>`へ集積する(sha256は常に計算)。
/// `dest`指定時は平文を`<dest>.part`へ書き出し、成功時に`dest`へrenameする
/// (3.8のdecrypt_to_file経路。collectとの併用はしない)。
fn decrypt_impl(
    age_path: &Path,
    key: &Path,
    collect: bool,
    dest: Option<(&Path, &Path)>,
) -> BackupResult<(Option<Vec<u8>>, u64, Option<String>)> {
    let age_bin = find_age()?;
    let file = std::fs::File::open(age_path).map_err(|e| {
        BackupError::Process(format!(".ageを開けない ({}): {e}", age_path.display()))
    })?;
    // part書き出し経路: 親dir作成→truncate上書き(残存.partの再利用は7.3と同じ扱い)
    let mut part_writer = match dest {
        Some((p, _)) => {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        BackupError::Process(format!("出力dir作成失敗 ({}): {e}", parent.display()))
                    })?;
                }
            }
            let f = std::fs::File::create(p).map_err(|e| {
                BackupError::Process(format!(".part作成失敗 ({}): {e}", p.display()))
            })?;
            Some(std::io::BufWriter::with_capacity(CHUNK, f))
        }
        None => None,
    };
    let mut cmd = std::process::Command::new(&age_bin);
    cmd.arg("-d").arg("-i").arg(key);
    let mut proc = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| BackupError::Config(format!("age subprocessの起動に失敗: {e}")))?;
    let mut stdin = proc.stdin.take().expect("age stdin");
    let mut stdout = proc.stdout.take().expect("age stdout");
    let stderr = proc.stderr.take().expect("age stderr");

    let (collected, total, sha) = std::thread::scope(
        |scope| -> BackupResult<(Option<Vec<u8>>, u64, Option<String>)> {
            let stderr_handle = scope.spawn(move || {
                let mut drained: Vec<u8> = Vec::new();
                let _ = std::io::BufReader::new(stderr).read_to_end(&mut drained);
                drained
            });
            // stdin投入thread: .ageをageへ流す(平文hashはしない)
            let stdin_handle = scope.spawn(move || -> std::io::Result<()> {
                let mut reader = std::io::BufReader::with_capacity(CHUNK, file);
                let mut buf = vec![0u8; CHUNK];
                loop {
                    let n = reader.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    stdin.write_all(&buf[..n])?;
                }
                stdin.flush()?;
                drop(stdin);
                Ok(())
            });

            // main thread: 復号平文のsha256更新(と必要なら集積・part書き出し)
            let mut hasher = Sha256::new();
            let mut total: u64 = 0;
            let mut collected: Option<Vec<u8>> = if collect { Some(Vec::new()) } else { None };
            let mut read_err: Option<std::io::Error> = None;
            let mut buf = vec![0u8; CHUNK];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        hasher.update(&buf[..n]);
                        total += n as u64;
                        if let Some(v) = collected.as_mut() {
                            v.extend_from_slice(&buf[..n]);
                        }
                        if let Some(w) = part_writer.as_mut() {
                            if let Err(e) = w.write_all(&buf[..n]) {
                                read_err = Some(e);
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        read_err = Some(e);
                        break;
                    }
                }
            }
            // 読込loop中断時もstdoutの読込端を閉じてからwaitする(encrypt側と同じ理由。
            // 読込端が生存するとage -dのstdout書込がblockしwaitが返らない)
            drop(stdout);
            let status = proc
                .wait()
                .map_err(|e| BackupError::Process(format!("ageのwaitに失敗: {e}")))?;
            let stdin_res = stdin_handle
                .join()
                .unwrap_or_else(|_| Err(std::io::Error::other("stdin投入threadがpanicした")));
            let stderr_bytes = stderr_handle.join().unwrap_or_default();
            let stderr_text = String::from_utf8_lossy(&stderr_bytes).trim().to_string();
            // stdin投入threadエラーにもage stderrを添える(encrypt側と対称の形式)
            stdin_res.map_err(|e| {
                BackupError::Process(format!(
                    ".ageのage stdin書込に失敗: {e}; age stderr: {stderr_text}"
                ))
            })?;
            if let Some(e) = read_err {
                return Err(BackupError::Process(format!(
                    "復号平文の書込に失敗: {e}; age stderr: {stderr_text}"
                )));
            }
            if !status.success() {
                return Err(BackupError::Process(format!(
                    "age -dが異常終了した(status={status}); age stderr: {stderr_text}"
                )));
            }
            Ok((collected, total, Some(hex(&hasher.finalize()))))
        },
    )?;
    // 正常終了時のみ: flush+sync → rename(part書き出し経路。以降の失敗経路の
    // .part削除はdecrypt_to_fileの統一入口が担う)
    if let Some(w) = part_writer.as_mut() {
        w.flush()
            .map_err(|e| BackupError::Process(format!(".partのflushに失敗: {e}")))?;
        w.get_ref()
            .sync_all()
            .map_err(|e| BackupError::Process(format!(".partのsyncに失敗: {e}")))?;
    }
    drop(part_writer);
    if let Some((p, dest)) = dest {
        std::fs::rename(p, dest).map_err(|e| {
            BackupError::Process(format!(
                "renameに失敗({} -> {}): {e}",
                p.display(),
                dest.display()
            ))
        })?;
    }
    Ok((collected, total, sha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::BackupError;
    use crate::testutil::{TempDir, path_env_lock, set_path_env};

    /// [covers:encrypt.age-binary-missing-config-error]
    /// PATH上にageが無い状態では設定エラー2。
    #[test]
    fn find_age_missing_binary_is_config_error() {
        // PATH書換えは他testの外部コマンドspawnと競合するためlockして直列化
        let _lock = path_env_lock();
        let tmp = TempDir::new("crypto-noage");
        let empty_dir = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&empty_dir).expect("empty bin dir");

        let _guard = set_path_env(empty_dir.to_str().unwrap());
        match find_age() {
            Err(BackupError::Config(msg)) => {
                assert!(msg.contains("age"), "messageがage不在を示す: {msg}");
            }
            Err(other) => panic!("期待した区分はConfig、実際は {other:?}"),
            Ok(p) => panic!("空PATHでageが見つかった: {}", p.display()),
        }
        let e = { find_age().unwrap_err() };
        assert_eq!(e.exit_code(), 2);
    }
}
