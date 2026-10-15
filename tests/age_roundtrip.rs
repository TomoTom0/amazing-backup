//! 実ageバイナリによる暗号化→復号・ヘッダ照合の結合test(設計1章・8章)。
//! age不在環境では早期returnでskip扱いとする。

mod common;

use backup_remote::crypto::{decrypt_sha256, encrypt_stream};
use backup_remote::fingerprint::{
    parse_age_header, parse_ssh_pubkey, stanza_tag, verify_recipients,
};
use base64::Engine as _;
use common::*;
use sha2::{Digest, Sha256};

/// [covers:encrypt.roundtrip-byte-identical]
/// 暗号化成果物は実 age -d で復号でき、平文が元ファイルとbyte一致する。
#[test]
fn roundtrip_byte_identical() {
    let Some(()) = (|| {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let plain = b"PK-fake-zip-content-0123456789".to_vec();
        let tmp = TestDir::new("rt-ident");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "key1");
        let src = tmp.path().join("photo.zip");
        write_file(&src, &plain);
        let dest = tmp.path().join("out").join("photo.zip.age");
        encrypt_stream(&src, &dest, &[public]).expect("encrypt_stream成功(実age)");
        let decrypted = age_decrypt(&age, &dest, &secret);
        assert_eq!(decrypted, plain, "復号平文が元ファイルとbyte一致");
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.source-sha256-matches-independent]
/// 平文sha256計算値(1pass)が独立計算(sha2 crate直接)と一致する。
#[test]
fn source_sha256_matches_independent() {
    let Some(_dest) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let plain = b"known-plaintext-for-sha256-vector".to_vec();
        let expected = sha256_hex(&plain);
        let tmp = TestDir::new("rt-srcsha");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "key1");
        let src = tmp.path().join("a.zip");
        write_file(&src, &plain);
        let dest = tmp.path().join("a.zip.age");
        let result = encrypt_stream(&src, &dest, &[public]).expect("encrypt_stream成功");
        assert_eq!(result.source_sha256, expected, "1pass計算の平文sha256");
        assert_eq!(result.source_size, plain.len() as u64);
        Some(dest)
    })() else {
        return;
    };
}

/// [covers:encrypt.age-sha256-matches-file]
/// age_sha256(書込同時計算)が暗号化ファイルの実sha256と一致する。
#[test]
fn age_sha256_matches_file() {
    let Some(()) = (|| {
        let _age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let plain = b"age-file-sha-check-plaintext".to_vec();
        let tmp = TestDir::new("rt-agesha");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "key1");
        let src = tmp.path().join("a.zip");
        write_file(&src, &plain);
        let dest = tmp.path().join("a.zip.age");
        let result = encrypt_stream(&src, &dest, &[public]).expect("encrypt_stream成功");
        let on_disk = std::fs::read(&dest).expect(".age read");
        assert_eq!(
            result.age_sha256,
            sha256_hex(&on_disk),
            "書込同時計算のage_sha256"
        );
        assert_eq!(result.age_size, on_disk.len() as u64);
        // recipientsは指紋列
        assert_eq!(result.recipients.len(), 1);
        assert!(result.recipients[0].starts_with("SHA256:"));
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.multi-recipient-1of-n]
/// 2鍵recipientでヘッダにssh-ed25519 stanzaが2つ並び、いずれか1鍵で復号できる。
#[test]
fn multi_recipient_1of_n() {
    let Some(()) = (|| {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let plain = b"multi-recipient-plaintext".to_vec();
        let tmp = TestDir::new("rt-multi");
        let (pub1, sec1) = generate_ed25519_keypair(tmp.path(), "k1");
        let (pub2, sec2) = generate_ed25519_keypair(tmp.path(), "k2");
        let src = tmp.path().join("a.zip");
        write_file(&src, &plain);
        let dest = tmp.path().join("a.zip.age");
        let result = encrypt_stream(&src, &dest, &[pub1.clone(), pub2.clone()])
            .expect("2 recipientで暗号化");
        assert_eq!(result.recipients.len(), 2, "recipients指紋は2つ");

        let header = parse_age_header(&dest).expect("ヘッダparse");
        let ssh_stanzas: Vec<_> = header
            .stanzas
            .iter()
            .filter(|s| s.stanza_type == "ssh-ed25519")
            .collect();
        assert_eq!(
            ssh_stanzas.len(),
            2,
            "ssh-ed25519 stanzaが2つ: {:?}",
            header.stanzas
        );

        // いずれか1鍵で復号できる(1-of-N)
        assert_eq!(age_decrypt(&age, &dest, &sec1), plain, "鍵1で復号可");
        assert_eq!(age_decrypt(&age, &dest, &sec2), plain, "鍵2で復号可");
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.header-tags-match-recipients]
/// 暗号化直後のヘッダ照合: ssh-ed25519 stanza数がrecipient数と一致し、各stanzaの
/// 第1引数が対応recipientの指紋prefixと完全一致する。実装の自己検証(設計6.3)は
/// 旧32byte tag形式とv1.2形式の両方を受け付けるため、暗号化成功自体が
/// 両形式対応の動作確認になる。
/// [covers:fingerprint.stanza-tag-matches-age-header]
/// 実age v1.2.1が生成するssh-ed25519 stanzaの識別子(第1引数)が、同一公開鍵から
/// 計算した指紋prefix(b64(SHA-256(blob)先頭4byte)・paddingなし)とbyte一致する。
#[test]
fn header_tags_match_recipients_and_stanza_tag() {
    let Some(()) = (|| {
        let _age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("rt-tag");
        let (pub1, _sec1) = generate_ed25519_keypair(tmp.path(), "k1");
        let (pub2, _sec2) = generate_ed25519_keypair(tmp.path(), "k2");
        let src = tmp.path().join("a.zip");
        write_file(&src, b"tag-check-plaintext");
        let dest = tmp.path().join("a.zip.age");
        encrypt_stream(&src, &dest, &[pub1.clone(), pub2.clone()])
            .expect("暗号化(自己検証は旧/v1.2両形式に対応し通過する)");

        let key1 = parse_ssh_pubkey(&pub1).expect("pub1 parse");
        let key2 = parse_ssh_pubkey(&pub2).expect("pub2 parse");

        // v1.2形式の第1引数は指紋prefix: b64(SHA-256(pubkey_blob)先頭4byte)・paddingなし
        let b64 = base64::engine::general_purpose::STANDARD_NO_PAD;
        let prefix1 = b64.encode(Sha256::digest(&key1.blob)[..4].to_vec());
        let prefix2 = b64.encode(Sha256::digest(&key2.blob)[..4].to_vec());

        let header = parse_age_header(&dest).expect("ヘッダparse");
        let ssh_stanzas: Vec<_> = header
            .stanzas
            .iter()
            .filter(|s| s.stanza_type == "ssh-ed25519")
            .collect();
        assert_eq!(
            ssh_stanzas.len(),
            2,
            "stanza数==recipient数: {:?}",
            header.stanzas
        );

        for stanza in &ssh_stanzas {
            assert_eq!(
                stanza.args.len(),
                2,
                "v1.2形式は<指紋prefix><ephemeral鍵>の2引数"
            );
            assert_eq!(
                b64.decode(stanza.args[0].as_bytes())
                    .expect("第1引数はb64")
                    .len(),
                4,
                "第1引数はSHA-256先頭4byteの指紋prefix"
            );
            assert_eq!(
                b64.decode(stanza.args[1].as_bytes())
                    .expect("第2引数はb64")
                    .len(),
                32,
                "第2引数はephemeral公開鍵32byte"
            );
        }
        let prefixes: Vec<&str> = ssh_stanzas.iter().map(|s| s.args[0].as_str()).collect();
        assert!(
            prefixes.contains(&prefix1.as_str()),
            "鍵1の指紋prefix一致: {prefixes:?}"
        );
        assert!(
            prefixes.contains(&prefix2.as_str()),
            "鍵2の指紋prefix一致: {prefixes:?}"
        );
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.header-mismatch-process-error]
/// ヘッダ照合不一致(誤ったrecipient鍵で作られた.age)は処理エラー4で失敗する。
#[test]
fn header_mismatch_is_process_error() {
    let Some(()) = (|| {
        let _age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("rt-mismatch");
        let (pub1, _sec1) = generate_ed25519_keypair(tmp.path(), "k1");
        let (other_pub, _other_sec) = generate_ed25519_keypair(tmp.path(), "kother");
        let src = tmp.path().join("a.zip");
        write_file(&src, b"mismatch-plaintext");
        let dest = tmp.path().join("a.zip.age");
        encrypt_stream(&src, &dest, &[pub1]).expect("鍵1で暗号化");

        // 期待tag集合は別鍵から計算 → 不一致
        let other_key = parse_ssh_pubkey(&other_pub).expect("other parse");
        let header = parse_age_header(&dest).expect("ヘッダparse");
        let err = verify_recipients(&header, &[stanza_tag(&other_key)]).unwrap_err();
        assert_eq!(err.exit_code(), 4, "処理エラー4");
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.atomic-rename-on-age-failure]
/// age異常終了時 .age は作られず(旧.age保持)・.partは削除される。
#[test]
fn atomic_rename_on_age_failure() {
    let Some(()) = (|| {
        let _age = find_age()?;
        let tmp = TestDir::new("rt-atomic");
        let (public, _secret) = if ssh_keygen_available() {
            generate_ed25519_keypair(tmp.path(), "k1")
        } else {
            // ssh-keygenが無くてもencrypt_stream失敗系は検証できる(不正recipient指定)
            let p = tmp.path().join("dummy.pub");
            write_file(&p, b"not-a-key\n");
            (p, tmp.path().join("dummy"))
        };
        let src = tmp.path().join("a.zip");
        write_file(&src, b"atomic-plaintext");
        let dest = tmp.path().join("out").join("a.zip.age");
        write_file(&dest, b"OLD-VALID-AGE-CONTENT");
        let old_bytes = std::fs::read(&dest).unwrap();
        let part = tmp.path().join("out").join("a.zip.age.part");

        // 存在しないrecipient鍵path → age異常終了
        let missing_key = tmp.path().join("absent-receiver.pub");
        let err = encrypt_stream(&src, &dest, &[missing_key]).unwrap_err();
        assert_eq!(err.exit_code(), 4, "age異常終了は処理エラー4");

        assert_eq!(
            std::fs::read(&dest).unwrap(),
            old_bytes,
            "旧.ageは旧内容のまま保持される"
        );
        assert!(!part.exists(), ".partは削除される");
        let _ = public;
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.large-stream-no-deadlock]
/// チャンク4MiBの16倍超(64MiB超)の平文でデッドロック・停滞せずに暗号化が完結する。
#[test]
fn large_stream_no_deadlock() {
    let Some(()) = (|| {
        let _age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        const SIZE: usize = 64 * 1024 * 1024 + 1024; // 64MiB超
        let tmp = TestDir::new("rt-large");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let src = tmp.path().join("big.zip");
        make_archive(&src, SIZE);
        let expected_plain_sha = sha256_hex(&std::fs::read(&src).unwrap());
        let dest = tmp.path().join("big.zip.age");

        // 停滞検出: 別threadで実行し120s以内の完了を要求する
        let (tx, rx) = std::sync::mpsc::channel();
        let src2 = src.clone();
        let dest2 = dest.clone();
        let pub2 = public.clone();
        let handle = std::thread::spawn(move || {
            let r = encrypt_stream(&src2, &dest2, &[pub2]);
            let _ = tx.send(());
            r
        });
        match rx.recv_timeout(std::time::Duration::from_secs(120)) {
            Ok(()) => {}
            Err(_) => panic!("暗号化が120s以内に完了しない(デッドロック/停滞疑い)"),
        }
        let result = handle
            .join()
            .expect("thread panic")
            .expect("大きな平文も暗号化完結");

        assert_eq!(result.source_size, SIZE as u64);
        assert_eq!(result.source_sha256, expected_plain_sha, "平文sha256");
        let on_disk = std::fs::read(&dest).unwrap();
        assert_eq!(result.age_sha256, sha256_hex(&on_disk), "暗号文sha256");
        // 復号roundtrip(平文byte一致は先頭/末尾スポット検査で時間を抑える)
        let decrypted = age_decrypt(&find_age().unwrap(), &dest, &secret);
        assert_eq!(decrypted.len(), SIZE);
        let original = std::fs::read(&src).unwrap();
        assert_eq!(&decrypted[..1024], &original[..1024], "先頭一致");
        assert_eq!(
            &decrypted[SIZE - 1024..],
            &original[SIZE - 1024..],
            "末尾一致"
        );
        // decrypt_sha256(lib側)も停滞せず完結する
        assert_eq!(decrypt_sha256(&dest, &secret).unwrap(), expected_plain_sha);
        Some(())
    })() else {
        return;
    };
}
