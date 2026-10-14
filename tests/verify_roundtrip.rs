//! verify --roundtrip系の結合test(設計1章・8章)。実ageバイナリを使用する。

mod common;

use backup_remote::crypto::decrypt_sha256;
use common::*;
use std::path::PathBuf;

/// 1件暗号化済みの検証環境を用意し、(cfg, registry, out, secret, tmp)を返す。
/// TestDirはdrop時に一時dirを再帰削除するため、戻り値に含めて呼び出し側の生存期間に
/// 合わせる(呼び出し側は _tmp 等のbindで保持すること。`_` 単独だと即座にdropされる)。
fn encrypted_env(tag: &str, plain: &[u8]) -> Option<(PathBuf, PathBuf, PathBuf, PathBuf, TestDir)> {
    find_age()?;
    if !ssh_keygen_available() {
        eprintln!("skipping: ssh-keygenが利用できない");
        return None;
    }
    let tmp = TestDir::new(tag);
    let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
    let out = tmp.path().join("out");
    let reg = tmp.path().join("registry.jsonl");
    let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, Some(&secret));
    let src = tmp.path().join("a.zip");
    write_file(&src, plain);
    let (code, _, stderr) = run_bin(&[
        "encrypt", "--config", &cfg.display().to_string(),
        &src.display().to_string(),
    ]);
    assert_eq!(code, 0, "下準備の暗号化: {stderr}");
    Some((cfg, reg, out, secret, tmp))
}

/// [covers:verify.roundtrip-sha256-match]
/// --roundtripで復号平文sha256がregistry記録(source_sha256)と一致することを検証する。
#[test]
fn roundtrip_sha256_match() {
    let Some(()) = (|| {
        let (cfg, _reg, _out, secret, _tmp) =
            encrypted_env("vr-match", b"verify-roundtrip-plaintext")?;

        let (code, stdout, stderr) = run_bin(&[
            "verify", "--config", &cfg.display().to_string(),
            "--roundtrip", "--key", &secret.display().to_string(),
        ]);
        assert_eq!(code, 0, "roundtrip検証成功: {stdout}{stderr}");
        Some(())
    })() else {
        return;
    };
}

/// [covers:verify.roundtrip-wrong-key-error]
/// --roundtripで鍵不一致(誤鍵)は処理エラー4。
#[test]
fn roundtrip_wrong_key_is_process_error() {
    let Some(()) = (|| {
        let _age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("vr-wrongkey");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "right");
        let (wrong_pub, wrong_secret) = generate_ed25519_keypair(tmp.path(), "wrong");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, Some(&wrong_secret));
        let src = tmp.path().join("a.zip");
        write_file(&src, b"wrong-key-plaintext");
        let (code, _, stderr) = run_bin(&[
            "encrypt", "--config", &cfg.display().to_string(),
            &src.display().to_string(),
        ]);
        assert_eq!(code, 0, "下準備: {stderr}");
        let _ = wrong_pub;

        let (code, stdout, stderr) = run_bin(&[
            "verify", "--config", &cfg.display().to_string(),
            "--roundtrip", "--key", &wrong_secret.display().to_string(),
        ]);
        assert_eq!(code, 4, "誤鍵は処理エラー4: {stdout}{stderr}");
        Some(())
    })() else {
        return;
    };
}

/// [covers:verify.roundtrip-manifest-crosscheck]
/// --roundtripでmanifest復号→registry突合を行い、不一致を検出する。
#[test]
fn roundtrip_manifest_crosscheck_detects_mismatch() {
    let Some(()) = (|| {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("vr-manifest");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public.clone()], &out, &reg, Some(&secret));
        let src = tmp.path().join("a.zip");
        write_file(&src, b"manifest-crosscheck-plain");
        let (code, _, stderr) = run_bin(&[
            "encrypt", "--config", &cfg.display().to_string(),
            &src.display().to_string(),
        ]);
        assert_eq!(code, 0, "下準備: {stderr}");

        // registry記録と突合しないmanifestを作る: 本物を復号→sha256を改変→再暗号化
        let date = jst_today();
        let manifest = out.join(&date).join("manifest.jsonl.age");
        let plain = age_decrypt(&age, &manifest, &secret);
        let mut text = String::from_utf8(plain).expect("manifest UTF-8");
        let tampered = text.replace(
            &format!("\"source_sha256\": \"{}\"", {
                let recs = read_registry_lines(&reg);
                recs[0]["source_sha256"].as_str().unwrap().to_string()
            }),
            "\"source_sha256\": \"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\"",
        );
        assert_ne!(tampered, text, "改変が反映されている");
        text = tampered;
        age_encrypt_bytes(&age, &[public], text.as_bytes(), &manifest);

        let (code, stdout, stderr) = run_bin(&[
            "verify", "--config", &cfg.display().to_string(),
            "--roundtrip", "--key", &secret.display().to_string(),
        ]);
        assert_eq!(code, 4, "manifest突合不一致を検出: {stdout}{stderr}");
        Some(())
    })() else {
        return;
    };
}

/// [covers:verify.decrypt-error-includes-age-stderr]
/// 復号のstdin投入threadエラー時(誤鍵でage -dが異常終了しEPIPEを受けるケース)の
/// エラーメッセージに、起因エラーとage stderrの両方が含まれる(encrypt側と対称)。
#[test]
fn decrypt_error_includes_age_stderr() {
    let Some(()) = (|| {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("vr-stderr");
        let (public, _right_secret) = generate_ed25519_keypair(tmp.path(), "right");
        let (_wrong_pub, wrong_secret) = generate_ed25519_keypair(tmp.path(), "wrong");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, None);
        let src = tmp.path().join("a.zip");
        write_file(&src, b"decrypt-stderr-plain");
        let (code, _, stderr) = run_bin(&[
            "encrypt", "--config", &cfg.display().to_string(),
            &src.display().to_string(),
        ]);
        assert_eq!(code, 0, "下準備: {stderr}");
        let age_file = out.join(jst_today()).join("a.zip.age");
        let _ = age;

        // 誤鍵で復号 → age -dが異常終了し、stdin投入threadはEPIPEを受け取る
        let err = decrypt_sha256(&age_file, &wrong_secret).unwrap_err();
        let msg = format!("{err}");
        assert_eq!(err.exit_code(), 4, "処理エラー4: {msg}");
        assert!(msg.contains("age stderr:"), "age stderrの添付がある: {msg}");
        assert!(
            msg.contains("no identity matched") || msg.contains("failed"),
            "age stderrの実内容が含まれる: {msg}"
        );
        Some(())
    })() else {
        return;
    };
}

/// [covers:verify.structure-recipients-bijection]
/// 実ageバイナリで暗号化した実際のv1.2形式ヘッダに対する構造検査(既定・--roundtripなし)
/// recipient照合: registry記録指紋とヘッダの指紋prefixが全単射一致する場合はexit 0、
/// 記録指紋を別鍵の実形式指紋へ書換えた(誤recipient・.age本体は不変)場合は検証エラー4。
/// テスト先行注記: TASK-2実装(構造検査でのヘッダ照合)前の現状では誤recipient caseも
/// verifyが通過してしまい本テストはfailする(実装後にexit 4を検出してpassとなる想定)。
#[test]
fn structure_check_recipient_bijection_with_real_age() {
    let Some(()) = (|| {
        let (cfg, reg, _out, _secret, tmp) =
            encrypted_env("vr-bijection", b"recipient-bijection-plain")?;

        // 正recipient: 実ageが生成したヘッダと記録指紋が対応 → 構造検査はexit 0
        let (code, stdout, stderr) =
            run_bin(&["verify", "--config", &cfg.display().to_string()]);
        assert_eq!(code, 0, "正recipientは構造検査通過: {stdout}{stderr}");

        // 誤recipient: registry記録のrecipients指紋を別鍵の実形式指紋へ書換え(.ageは不変)
        let (other_pub, _other_secret) = generate_ed25519_keypair(tmp.path(), "other");
        let other = backup_remote::fingerprint::parse_ssh_pubkey(&other_pub)
            .expect("別鍵のparse");
        let wrong_fp = backup_remote::fingerprint::fingerprint(&other);
        let recs = read_registry_lines(&reg);
        let right_fp = recs[0]["recipients"][0]
            .as_str()
            .expect("recipients[0]")
            .to_string();
        assert_ne!(wrong_fp, right_fp, "書換え先は別鍵の指紋");
        let text = std::fs::read_to_string(&reg).expect("registry read");
        let tampered = text.replace(&right_fp, &wrong_fp);
        assert_ne!(tampered, text, "指紋の書換えが反映されている");
        write_file(&reg, tampered.as_bytes());

        let (code, stdout, stderr) =
            run_bin(&["verify", "--config", &cfg.display().to_string()]);
        assert_eq!(code, 4, "誤recipient指紋は検証エラー4: {stdout}{stderr}");
        Some(())
    })() else {
        return;
    };
}
