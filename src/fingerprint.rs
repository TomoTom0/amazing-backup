//! SSH公開鍵指紋・age stanza tag計算(純Rust ChaCha20)・ageヘッダparse(設計4.4・5章)。

use crate::errors::{BackupError, BackupResult};
use base64::Engine as _;
use sha2::{Digest, Sha256};
use std::io::BufRead;
use std::path::Path;

/// parse済みSSH公開鍵。
#[derive(Debug)]
pub struct PubKey {
    /// アルゴリズム名(`ssh-ed25519`等)
    pub algo: String,
    /// SSH wire-format公開鍵blob
    pub blob: Vec<u8>,
}

/// OpenSSH形式の指紋文字列 `SHA256:<base64 paddingなし>`(4.4)。
pub fn fingerprint(pub_key: &PubKey) -> String {
    let digest = Sha256::digest(&pub_key.blob);
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
    format!("SHA256:{b64}")
}

/// age v1の`-> ssh-ed25519 <tag>`引数tag(4.4の導出。SHA-256(blob)を鍵・nonce零として
/// ChaCha20でSHA-256(blob)自身を暗号化した32byte)。
pub fn stanza_tag(pub_key: &PubKey) -> [u8; 32] {
    let sum = Sha256::digest(&pub_key.blob);
    let key: [u8; 32] = sum.into();
    let mut tag = key;
    chacha20::keystream_xor(&key, 0, &[0u8; 12], &mut tag);
    tag
}

/// 公開鍵1行形式(`ssh-ed25519 <base64> [comment]`)のparse。
/// algoが`ssh-ed25519`以外は設定エラー。
pub fn parse_ssh_pubkey(path: &Path) -> BackupResult<PubKey> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        BackupError::Config(format!(
            "公開鍵ファイルを読めない ({}): {e}",
            path.display()
        ))
    })?;
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| BackupError::Config(format!("公開鍵ファイルが空 ({})", path.display())))?;
    let mut fields = line.split_whitespace();
    let algo = fields.next().unwrap_or_default().to_string();
    let b64 = fields.next().unwrap_or_default().to_string();
    if algo.is_empty() || b64.is_empty() {
        return Err(BackupError::Config(format!(
            "SSH公開鍵形式としてparseできない ({})",
            path.display()
        )));
    }
    if algo != "ssh-ed25519" {
        return Err(BackupError::Config(format!(
            "recipient公開鍵はssh-ed25519形式のみ対象 ({}: {algo})",
            path.display()
        )));
    }
    let blob = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| {
            BackupError::Config(format!(
                "公開鍵blobのbase64 decodeに失敗 ({}): {e}",
                path.display()
            ))
        })?;
    // SSH wire-format検証: string "ssh-ed25519" + string(32byte公開鍵)
    let (name, rest) = read_ssh_string(&blob).ok_or_else(|| {
        BackupError::Config(format!("公開鍵blobのwire形式が不正 ({})", path.display()))
    })?;
    if name != b"ssh-ed25519" {
        return Err(BackupError::Config(format!(
            "公開鍵blobのalgorithmがssh-ed25519でない ({})",
            path.display()
        )));
    }
    let (point, tail) = read_ssh_string(rest).ok_or_else(|| {
        BackupError::Config(format!("公開鍵blobのwire形式が不正 ({})", path.display()))
    })?;
    if point.len() != 32 || !tail.is_empty() {
        return Err(BackupError::Config(format!(
            "ed25519公開鍵blobの長さが不正 ({})",
            path.display()
        )));
    }
    Ok(PubKey { algo, blob })
}

/// SSH wire-formatのlength前置stringを1つ読む。成功時(内容, 残り)を返す。
fn read_ssh_string(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    if buf.len() < 4 {
        return None;
    }
    let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + n {
        return None;
    }
    Some((&buf[4..4 + n], &buf[4 + n..]))
}

/// ageヘッダのstanza 1つ(`-> <type> <args...>`行)。
#[derive(Debug)]
pub struct Stanza {
    pub stanza_type: String,
    pub args: Vec<String>,
}

/// parse済みageヘッダ。
#[derive(Debug)]
pub struct AgeHeader {
    pub stanzas: Vec<Stanza>,
}

/// ヘッダ1行(改行込み)の長さ上限(64KB)。正常なageヘッダ行は最大でも数百byte程度であり、
/// これを超える行は破損・改竄データのみ。`read_until`は改行が現れるまで行全体をメモリに
/// 載せるため、改行なし巨大データでメモリを使い切らないための上限。
const MAX_HEADER_LINE: usize = 64 * 1024;

/// `read_bounded_line`の読み取り結果。
enum LineRead {
    /// 1行(改行込み)を`line`へ読み込んだ。
    Line,
    /// EOFに達した。`line`には改行を含まない最終部分行が入る(改行終端ファイルなら空)。
    Eof,
    /// 行長が`MAX_HEADER_LINE`を超えた。読込を打ち切り、`line`は上限近辺までの部分行。
    TooLong,
}

/// `reader`から1行(改行込み)を`line`へ読む。行長が`MAX_HEADER_LINE`を超えた時点で
/// 読込を打ち切り`LineRead::TooLong`を返す(`read_until`と違い改行の無い巨大データを
/// 行全体でメモリに載せない。`line`は上限+内部buffer分までしか成長しない)。
fn read_bounded_line(
    reader: &mut std::io::BufReader<std::fs::File>,
    line: &mut Vec<u8>,
) -> std::io::Result<LineRead> {
    line.clear();
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(LineRead::Eof);
        }
        let newline = buf.iter().position(|&b| b == b'\n');
        let take = newline.map_or(buf.len(), |pos| pos + 1);
        line.extend_from_slice(&buf[..take]);
        reader.consume(take);
        if line.len() > MAX_HEADER_LINE {
            return Ok(LineRead::TooLong);
        }
        if newline.is_some() {
            return Ok(LineRead::Line);
        }
    }
}

/// ファイル先頭から`--- `(ヘッダ終端)行までを読み、`-> <type> <args...>`行を列挙する(4.4)。
/// type=`ssh-ed25519`の第1引数がtag。本体(binary payload)は読まない。
/// ヘッダ1行は`MAX_HEADER_LINE`(64KB)上限で読み、行長超過・`--- `終端行に到達しないEOFは
/// いずれも`Process` error(parse失敗=検証エラー4経路。改行なし巨大データでメモリを
/// 使い切らない)。
pub fn parse_age_header(path: &Path) -> BackupResult<AgeHeader> {
    let file = std::fs::File::open(path).map_err(|e| {
        BackupError::Process(format!(".ageヘッダを開けない ({}): {e}", path.display()))
    })?;
    let mut reader = std::io::BufReader::new(file);
    let mut stanzas = Vec::new();
    let mut line = Vec::new();
    loop {
        match read_bounded_line(&mut reader, &mut line) {
            Ok(LineRead::Line) => {}
            Ok(LineRead::Eof) => {
                // 改行なしの最終部分行は1行として処理し、次周回のEOF(空行)で終端判定する
                if line.is_empty() {
                    return Err(BackupError::Process(format!(
                        ".ageヘッダが終端行(`--- `)無しで終わった ({})",
                        path.display()
                    )));
                }
            }
            Ok(LineRead::TooLong) => {
                return Err(BackupError::Process(format!(
                    ".ageヘッダ行が長さ上限({MAX_HEADER_LINE}byte)を超えた ({})",
                    path.display()
                )));
            }
            Err(e) => {
                return Err(BackupError::Process(format!(
                    ".ageヘッダ読込失敗 ({}): {e}",
                    path.display()
                )));
            }
        }
        let trimmed = String::from_utf8_lossy(&line);
        let trimmed = trimmed.trim_end_matches(['\n', '\r']);
        if trimmed.starts_with("--- ") {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("-> ") {
            let mut fields = rest.split_whitespace();
            if let Some(stanza_type) = fields.next() {
                stanzas.push(Stanza {
                    stanza_type: stanza_type.to_string(),
                    args: fields.map(|a| a.to_string()).collect(),
                });
            }
        }
        // 上記以外の行(stanza本体の継続行等)は読み飛ばす
    }
    Ok(AgeHeader { stanzas })
}

/// base64引数をpadding有無両対応でdecodeする。
fn decode_arg(s: &str) -> Option<Vec<u8>> {
    let trimmed = s.trim_end_matches('=');
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(trimmed.as_bytes())
        .ok()
}

/// 暗号化直後検証(4.4・6.3)。ssh-ed25519 stanza数が期待recipient数と一致し、
/// tag集合が期待集合と完全一致することを確認。不一致は`BackupError::Process`。
pub fn verify_recipients(header: &AgeHeader, expected_tags: &[[u8; 32]]) -> BackupResult<()> {
    let ssh_stanzas: Vec<&Stanza> = header
        .stanzas
        .iter()
        .filter(|s| s.stanza_type == "ssh-ed25519")
        .collect();
    if ssh_stanzas.len() != expected_tags.len() {
        return Err(BackupError::Process(format!(
            "ssh-ed25519 stanza数がrecipient数と不一致: header={} / expected={}",
            ssh_stanzas.len(),
            expected_tags.len()
        )));
    }
    let mut actual: Vec<Vec<u8>> = ssh_stanzas
        .iter()
        .map(|s| {
            s.args
                .first()
                .and_then(|a| decode_arg(a))
                .unwrap_or_default()
        })
        .collect();
    let mut expected: Vec<Vec<u8>> = expected_tags.iter().map(|t| t.to_vec()).collect();
    actual.sort();
    expected.sort();
    if actual != expected {
        return Err(BackupError::Process(
            "ageヘッダのssh-ed25519 tag集合が期待recipientと不一致".to_string(),
        ));
    }
    Ok(())
}

/// 暗号化直後検証の実行系版(6.3)。age v1.2以降のssh-ed25519 stanzaは
/// `-> ssh-ed25519 <sha256(blob)先頭4byte> <ephemeral公開鍵>`形式であり
/// 旧形式のChaCha20 tagを含まないため、stanza数一致に加えて
/// 形式に応じてtag(旧)または指紋prefix(v1.2以降)の一致を検証する。
/// 照合は全単射(bijective)でなければならない(4.4): 各stanzaの識別子が
/// 重複しない別々のrecipient鍵と1対1で対応することを保証し、
/// 期待recipientの一部しか対応しない重複stanza(any-matchで全体が通るケース)は拒否する。
pub(crate) fn verify_recipients_for_keys(header: &AgeHeader, keys: &[PubKey]) -> BackupResult<()> {
    let ssh_stanzas: Vec<&Stanza> = header
        .stanzas
        .iter()
        .filter(|s| s.stanza_type == "ssh-ed25519")
        .collect();
    if ssh_stanzas.len() != keys.len() {
        return Err(BackupError::Process(format!(
            "ssh-ed25519 stanza数がrecipient数と不一致: header={} / expected={}",
            ssh_stanzas.len(),
            keys.len()
        )));
    }
    // stanza→鍵の全単射matching: 各stanzaの識別子(tag/prefix)が対応する未割当の鍵へ
    // 1対1で割り当たることを検証する(同一鍵への重複割当・余り鍵を拒否)
    let mut matched = vec![false; keys.len()];
    for stanza in ssh_stanzas {
        let Some(arg0) = stanza.args.first().and_then(|a| decode_arg(a)) else {
            return Err(BackupError::Process(
                "ageヘッダのssh-ed25519 stanzaの第1引数がdecodeできない".to_string(),
            ));
        };
        let old_form = arg0.len() == 32 && stanza.args.len() == 1;
        let v12_form = !old_form && arg0.len() == 4;
        if !old_form && !v12_form {
            return Err(BackupError::Process(
                "ageヘッダのssh-ed25519 stanzaの引数形式が不正".to_string(),
            ));
        }
        let mut assigned: Option<usize> = None;
        for (i, k) in keys.iter().enumerate() {
            if matched[i] {
                continue;
            }
            let hit = if old_form {
                // 旧形式: 第1引数が32byteのChaCha20 tag
                arg0.as_slice() == stanza_tag(k).as_slice()
            } else {
                // v1.2以降: 第1引数がSHA256(blob)先頭4byteの指紋prefix
                arg0.as_slice() == &Sha256::digest(&k.blob)[..4]
            };
            if hit {
                assigned = Some(i);
                break;
            }
        }
        match assigned {
            Some(i) => matched[i] = true,
            None => {
                return Err(BackupError::Process(
                    "ageヘッダのssh-ed25519 stanzaが期待recipientと全単射で対応しない(未対応のstanzaまたは重複)"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// registry記録の指紋文字列(`SHA256:<base64>`・4.4)からv1.2形式指紋prefix
/// (digest先頭4byte)を導出する(2.4-1・VER-013)。指紋は`SHA-256(blob)`のbase64表現
/// であり、stanza第1引数は`SHA-256(blob)`先頭4byteのb64(paddingなし)符号化なので、
/// 指紋のbase64部をdecodeした32byteの先頭4byteがstanza識別子と同一導出元になる。
/// `SHA256:`前置なし・base64 decode不能・decode結果が32byteでない(未満を含む)場合は
/// 導出不能としてNone(fail-closed対象)。
pub(crate) fn fingerprint_prefix(fingerprint: &str) -> Option<[u8; 4]> {
    let b64 = fingerprint.strip_prefix("SHA256:")?;
    let digest = decode_arg(b64)?;
    if digest.len() != 32 {
        return None;
    }
    let mut prefix = [0u8; 4];
    prefix.copy_from_slice(&digest[..4]);
    Some(prefix)
}

/// verify構造検査(2.4-1)のrecipient照合(VER-013)。ageヘッダのssh-ed25519 stanza識別子が
/// registry記録`recipients`指紋から導出したv1.2形式指紋prefix集合と全単射で対応する
/// ことを検証する(全単射要件は`verify_recipients_for_keys`と同じ割当方式)。
/// 旧形式tag(32byte)は指紋から導出不能なため照合に使えず、構造検査は秘密鍵を用いない
/// ためfail-closedに扱う: 第1引数が指紋prefix形式(decode結果4byte)でないstanza・
/// ssh-ed25519 stanzaを1つも持たない`.age`・記録指紋からprefixを導出できない場合は
/// いずれも照合skipではなく検証エラー。不一致は`BackupError::Process`。
pub(crate) fn verify_recipients_for_fingerprints(
    header: &AgeHeader,
    fingerprints: &[String],
) -> BackupResult<()> {
    let ssh_stanzas: Vec<&Stanza> = header
        .stanzas
        .iter()
        .filter(|s| s.stanza_type == "ssh-ed25519")
        .collect();
    if ssh_stanzas.is_empty() {
        return Err(BackupError::Process(
            "ageヘッダがssh-ed25519 stanzaを1つも持たない(照合可能なv1.2形式stanza無し)"
                .to_string(),
        ));
    }
    // 記録指紋→v1.2形式指紋prefixの導出(導出不能な指紋もfail-closedに検証エラー)
    let mut expected: Vec<[u8; 4]> = Vec::with_capacity(fingerprints.len());
    for fp in fingerprints {
        let Some(prefix) = fingerprint_prefix(fp) else {
            return Err(BackupError::Process(format!(
                "記録指紋から指紋prefixを導出できない: {fp}"
            )));
        };
        expected.push(prefix);
    }
    if ssh_stanzas.len() != expected.len() {
        return Err(BackupError::Process(format!(
            "ssh-ed25519 stanza数がrecipient数と不一致: header={} / expected={}",
            ssh_stanzas.len(),
            expected.len()
        )));
    }
    // stanza→指紋prefixの全単射matching: 各stanzaの識別子が未割当の指紋prefixへ
    // 1対1で割り当たることを検証する(同一prefixへの重複割当・余り指紋を拒否)
    let mut matched = vec![false; expected.len()];
    for stanza in ssh_stanzas {
        let Some(arg0) = stanza.args.first().and_then(|a| decode_arg(a)) else {
            return Err(BackupError::Process(
                "ageヘッダのssh-ed25519 stanzaの第1引数がdecodeできない".to_string(),
            ));
        };
        if arg0.len() != 4 {
            return Err(BackupError::Process(format!(
                "ageヘッダのssh-ed25519 stanzaの第1引数が指紋prefix形式(decode結果4byte)でない: {}byte",
                arg0.len()
            )));
        }
        let prefix: [u8; 4] = arg0[..4].try_into().expect("長さ検査済みの4byte");
        match expected
            .iter()
            .enumerate()
            .find(|(i, exp)| !matched[*i] && **exp == prefix)
        {
            Some((i, _)) => matched[i] = true,
            None => {
                return Err(BackupError::Process(
                    "ageヘッダのssh-ed25519 stanzaが記録recipients指紋と全単射で対応しない(誤recipientまたは重複)"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// stanza tag計算専用の最小ChaCha20(32byteのkeystream生成のみ・RFC 8439準拠)。
/// 依存方針(設計0章)により純Rust実装とし、クレートは使わない。
mod chacha20 {
    /// RFC 8439 2.3.2のquarter round。
    fn quarter_round(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
        s[a] = s[a].wrapping_add(s[b]);
        s[d] ^= s[a];
        s[d] = s[d].rotate_left(16);
        s[c] = s[c].wrapping_add(s[d]);
        s[b] ^= s[c];
        s[b] = s[b].rotate_left(12);
        s[a] = s[a].wrapping_add(s[b]);
        s[d] ^= s[a];
        s[d] = s[d].rotate_left(8);
        s[c] = s[c].wrapping_add(s[d]);
        s[b] ^= s[c];
        s[b] = s[b].rotate_left(7);
    }

    /// RFC 8439 2.3.2のblock function。key 32byte・nonce 12byte・counterから64byte keystream blockを返す。
    pub(crate) fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
        let mut state = [0u32; 16];
        state[0] = 0x6170_7865;
        state[1] = 0x3320_646e;
        state[2] = 0x7962_2d32;
        state[3] = 0x6b20_6574;
        for i in 0..8 {
            state[4 + i] =
                u32::from_le_bytes([key[i * 4], key[i * 4 + 1], key[i * 4 + 2], key[i * 4 + 3]]);
        }
        state[12] = counter;
        for i in 0..3 {
            state[13 + i] = u32::from_le_bytes([
                nonce[i * 4],
                nonce[i * 4 + 1],
                nonce[i * 4 + 2],
                nonce[i * 4 + 3],
            ]);
        }
        let mut working = state;
        for _ in 0..10 {
            quarter_round(&mut working, 0, 4, 8, 12);
            quarter_round(&mut working, 1, 5, 9, 13);
            quarter_round(&mut working, 2, 6, 10, 14);
            quarter_round(&mut working, 3, 7, 11, 15);
            quarter_round(&mut working, 0, 5, 10, 15);
            quarter_round(&mut working, 1, 6, 11, 12);
            quarter_round(&mut working, 2, 7, 8, 13);
            quarter_round(&mut working, 3, 4, 9, 14);
        }
        let mut out = [0u8; 64];
        for i in 0..16 {
            let v = working[i].wrapping_add(state[i]);
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        out
    }

    /// dataをkey・counter・nonceから生成したkeystreamでXORする(in-place)。
    pub(crate) fn keystream_xor(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
        for (block_idx, chunk) in data.chunks_mut(64).enumerate() {
            let ks = chacha20_block(key, counter.wrapping_add(block_idx as u32), nonce);
            for (b, k) in chunk.iter_mut().zip(ks.iter()) {
                *b ^= k;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{
        TEST_ED25519_FINGERPRINT, TempDir, path_env_lock, write_file, write_test_ed25519_pub,
    };

    /// [covers:fingerprint.matches-ssh-keygen]
    /// fingerprintが`ssh-keygen -lf`の表示と一致する(独立oracleとの照合)。
    #[test]
    fn fingerprint_matches_ssh_keygen() {
        // ssh-keygenはPATH経由でspawnするため、PATHを書換えるtestとの競合をlockで防ぐ
        let _lock = path_env_lock();
        let tmp = TempDir::new("fpr-keygen");
        let pubkey = write_test_ed25519_pub(tmp.path());

        let out = std::process::Command::new("ssh-keygen")
            .arg("-lf")
            .arg(&pubkey)
            .output()
            .expect("ssh-keygen実行");
        if !out.status.success() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return;
        }
        let text = String::from_utf8(out.stdout).expect("ssh-keygen -lf output utf8");
        // 表示形式: "256 SHA256:xxx comment (ED25519)"
        let shown = text
            .split_whitespace()
            .find(|t| t.starts_with("SHA256:"))
            .unwrap_or_else(|| panic!("ssh-keygen表示に指紋が無い: {text}"));

        let key = parse_ssh_pubkey(&pubkey).expect("embedded pubkeyはparseできる");
        assert_eq!(key.algo, "ssh-ed25519");
        assert_eq!(fingerprint(&key), shown);
        assert_eq!(fingerprint(&key), TEST_ED25519_FINGERPRINT);
    }

    /// [covers:fingerprint.header-parse-multi-stanza]
    /// ヘッダparseは`--- `終端と複数stanzaを正しく扱い、以降の本体は読まない。
    #[test]
    fn header_parse_multi_stanza_and_terminator() {
        let tmp = TempDir::new("fpr-parse");
        let header_path = tmp.path().join("synthetic.age");
        // 複数stanza(X25519 1つ + ssh-ed25519 2つ)と --- 終端行。
        // 終端行より後はbinary本体として読まない(不正UTF-8が含まれていてもparseは成功する)。
        let mut data = b"age-encryption.org/v1\n".to_vec();
        data.extend_from_slice(
            b"-> X25519 SJtilzFHmYMJUyrclM4ptthDPBIi81fitQYQRYkA\nHRRsZw4D8OWQiqcN\n",
        );
        data.extend_from_slice(b"-> ssh-ed25519 AAAAtesttag1\n");
        data.extend_from_slice(b"-> ssh-ed25519 AAAAtesttag2\n");
        data.extend_from_slice(b"--- MacLine123\n");
        data.extend_from_slice(&[0x00, 0x01, 0xff, 0xfe, 0x80, 0x7f, 0xC0, 0x00]);
        write_file(&header_path, &data);

        let header = parse_age_header(&header_path).expect("合成ヘッダはparseできる");
        assert_eq!(header.stanzas.len(), 3, "順序保持で3 stanza");
        assert_eq!(header.stanzas[0].stanza_type, "X25519");
        assert_eq!(header.stanzas[0].args.len(), 1);
        assert_eq!(header.stanzas[1].stanza_type, "ssh-ed25519");
        assert_eq!(header.stanzas[1].args, vec!["AAAAtesttag1".to_string()]);
        assert_eq!(header.stanzas[2].stanza_type, "ssh-ed25519");
        assert_eq!(header.stanzas[2].args, vec!["AAAAtesttag2".to_string()]);
    }

    /// [covers:fingerprint.header-parse-multi-stanza]
    /// ヘッダparseの読込は1行64KB上限: 改行なし巨大データ(終端行なし)・64KB超の1行は
    /// いずれもProcess error(検証エラー4経路)となる。上限超過はEOFまで読まずに行長
    /// 上限の時点で打ち切る(行全体をメモリに載せない。エラー文で終端なしEOFと区別)。
    /// 上限ちょうどの行・改行なしの`--- `終端行は従来どおりparse成功する。
    #[test]
    fn header_parse_line_limit_and_missing_terminator() {
        let tmp = TempDir::new("fpr-limit");

        // (1) 改行なしの巨大データ: EOFまで読まずに行長上限のエラー(旧実装のread_untilは
        //     8MB全体を1行としてメモリに載せた後で終端なしエラーになる)
        let huge = tmp.path().join("no-newline.age");
        write_file(&huge, &vec![b'A'; 8 * 1024 * 1024]);
        let e = parse_age_header(&huge).unwrap_err();
        assert_eq!(e.exit_code(), 4, "改行なし巨大データはProcess error: {e}");
        assert!(
            format!("{e}").contains("長さ上限"),
            "行長上限超過のエラーであること: {e}"
        );

        // (2) 64KB超の1行のあとに終端行が続くデータ: 1行目の時点でエラー
        let long_line = tmp.path().join("long-line.age");
        let mut data = vec![b'B'; 64 * 1024 + 1];
        data.extend_from_slice(b"\n--- MAC\n");
        write_file(&long_line, &data);
        let e = parse_age_header(&long_line).unwrap_err();
        assert_eq!(e.exit_code(), 4, "64KB超の1行はProcess error: {e}");
        assert!(
            format!("{e}").contains("長さ上限"),
            "行長上限超過のエラーであること: {e}"
        );

        // (3) 境界: 上限ちょうど(改行込み64KB)の行は読み飛ばし可能でparseは成功する
        let exact = tmp.path().join("exact-line.age");
        let mut data = vec![b'C'; 64 * 1024 - 1]; // 65535byte + 改行1byte = 64KBちょうど
        data.push(b'\n');
        data.extend_from_slice(b"-> ssh-ed25519 AAAAtesttag1\n--- MAC\n");
        write_file(&exact, &data);
        let header = parse_age_header(&exact).expect("上限ちょうどの行はparse成功");
        assert_eq!(
            header.stanzas.len(),
            1,
            "継続行読み飛ばし後のstanza列挙も不変"
        );

        // (4) 終端行なしでEOF(行長上限には達しないデータ): 終端なしのエラー
        let no_term = tmp.path().join("no-term.age");
        write_file(
            &no_term,
            b"age-encryption.org/v1\n-> ssh-ed25519 AAAAtesttag1\n",
        );
        let e = parse_age_header(&no_term).unwrap_err();
        assert_eq!(e.exit_code(), 4, "終端行なしEOFはProcess error: {e}");
        assert!(
            format!("{e}").contains("終端行"),
            "終端行なしのエラーであること: {e}"
        );

        // (5) 改行なしの最終行が`--- `終端のときは従来どおりparse成功
        let no_nl_term = tmp.path().join("no-nl-term.age");
        write_file(
            &no_nl_term,
            b"age-encryption.org/v1\n-> ssh-ed25519 AAAAtesttag1\n--- MAC",
        );
        let header = parse_age_header(&no_nl_term).expect("改行なし終端行は受け入れる");
        assert_eq!(header.stanzas.len(), 1);
    }

    /// [covers:fingerprint.chacha20-rfc8439-vectors]
    /// 純Rust ChaCha20(keystream生成)がRFC 8439のテストベクトルと一致する。
    #[test]
    fn chacha20_matches_rfc8439_vectors() {
        // RFC 8439 A.1 test vector #1: key=0*32, nonce=0*12, counter=0
        let zero_key = [0u8; 32];
        let zero_nonce = [0u8; 12];
        let block0 = chacha20::chacha20_block(&zero_key, 0, &zero_nonce);
        let expected0 = hex(
            "76b8e0ada0f13d90405d6ae55386bd28bdd219b8a08ded1aa836efcc8b770dc7da41597c5157488d7724e03fb8d84a376a43b8f41518a11cc387b669b2ee6586",
        );
        assert_eq!(&block0[..], &expected0[..], "A.1 #1 block");

        // RFC 8439 2.4.2: key=00..1f, nonce=000000000000004a00000000, counter=1
        let mut key = [0u8; 32];
        for (i, b) in key.iter_mut().enumerate() {
            *b = i as u8;
        }
        let nonce = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4a, 0x00, 0x00, 0x00, 0x00,
        ];
        let plaintext = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let mut buf = plaintext.to_vec();
        chacha20::keystream_xor(&key, 1, &nonce, &mut buf);
        let expected_ct = hex(concat!(
            "6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0b",
            "f91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d80",
            "7ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab77937365a",
            "f90bbf74a35be6b40b8eedf2785e42874d",
        ));
        assert_eq!(&buf[..], &expected_ct[..], "2.4.2 ciphertext");

        // 同一入力から生成したkeystreamのblock単位の検証(2.3.2 block counter=1)
        let block1 = chacha20::chacha20_block(&key, 1, &nonce);
        let expected1 = hex(
            "224f51f3401bd9e12fde276fb8631ded8c131f823d2c06e27e4fcaec9ef3cf788a3b0aa372600a92b57974cded2b9334794cba40c63e34cdea212c4cf07d41b7",
        );
        assert_eq!(&block1[..], &expected1[..], "2.3.2 block counter=1");
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
            .collect()
    }

    /// 指紋prefix(b64(SHA-256(blob)先頭4byte)・paddingなし)。
    fn prefix_b64(k: &PubKey) -> String {
        let sum = Sha256::digest(&k.blob);
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(&sum[..4])
    }

    fn stanza_b64(tag: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(tag)
    }

    fn ssh_stanza(args: Vec<String>) -> Stanza {
        Stanza {
            stanza_type: "ssh-ed25519".into(),
            args,
        }
    }

    /// [covers:fingerprint.bijective-recipient-match]
    /// 照合は全単射: 期待recipientの一部しか対応しない重複stanza(旧形式tag・
    /// v1.2形式指紋prefixの多対多any-matchで全体が通るケース)は拒否される。
    /// 期待鍵2本分の識別子が1回ずつ並ぶ場合は通過する。
    #[test]
    fn verify_recipients_bijective_matching() {
        let tmp = TempDir::new("fpr-bij");
        let pubkey_path = write_test_ed25519_pub(tmp.path());
        let key1 = parse_ssh_pubkey(&pubkey_path).expect("embedded key1 parse");
        // key2: blobを1byte変えた別鍵(PubKeyを直接構築。tag/prefix導出にのみ使う)
        let mut blob2 = key1.blob.clone();
        let last = blob2.len() - 1;
        blob2[last] ^= 0xff;
        let key2 = PubKey {
            algo: "ssh-ed25519".into(),
            blob: blob2,
        };
        let keys = [key1, key2];
        let tag1 = stanza_b64(&stanza_tag(&keys[0]));
        let tag2 = stanza_b64(&stanza_tag(&keys[1]));
        let prefix1 = prefix_b64(&keys[0]);
        let prefix2 = prefix_b64(&keys[1]);
        let ephemeral = "A".repeat(43); // v1.2形式の第2引数(32byteのb64)

        // (a) 旧形式: 鍵1のtagのみ2つ並ぶ(鍵2の分が無い)→ 拒否
        let header = AgeHeader {
            stanzas: vec![
                ssh_stanza(vec![tag1.clone()]),
                ssh_stanza(vec![tag1.clone()]),
            ],
        };
        let e = verify_recipients_for_keys(&header, &keys).unwrap_err();
        assert_eq!(e.exit_code(), 4, "旧形式の重複tagは拒否: {e}");

        // (b) v1.2形式: 鍵1の指紋prefixのみ2つ並ぶ → 拒否
        let header = AgeHeader {
            stanzas: vec![
                ssh_stanza(vec![prefix1.clone(), ephemeral.clone()]),
                ssh_stanza(vec![prefix1.clone(), ephemeral.clone()]),
            ],
        };
        let e = verify_recipients_for_keys(&header, &keys).unwrap_err();
        assert_eq!(e.exit_code(), 4, "v1.2形式の重複prefixは拒否: {e}");

        // (c) 旧形式: 鍵1・鍵2のtagが1回ずつ → 通過
        let header = AgeHeader {
            stanzas: vec![
                ssh_stanza(vec![tag1.clone()]),
                ssh_stanza(vec![tag2.clone()]),
            ],
        };
        verify_recipients_for_keys(&header, &keys).expect("旧形式の全単射は通過");

        // (d) v1.2形式: 鍵1・鍵2のprefixが1回ずつ → 通過
        let header = AgeHeader {
            stanzas: vec![
                ssh_stanza(vec![prefix1, ephemeral.clone()]),
                ssh_stanza(vec![prefix2, ephemeral]),
            ],
        };
        verify_recipients_for_keys(&header, &keys).expect("v1.2形式の全単射は通過");

        // (e) 数不足: 鍵1の分のみ1stanza → 拒否(stanza数不一致)
        let header = AgeHeader {
            stanzas: vec![ssh_stanza(vec![tag1])],
        };
        let e = verify_recipients_for_keys(&header, &keys).unwrap_err();
        assert_eq!(e.exit_code(), 4, "stanza数不一致は拒否: {e}");
    }
}
