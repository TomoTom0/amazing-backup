//! backup verify --roundtrip系の結合test(設計1章・8章・TASK-10設計3.2・8.5)。
//! TASK-11(Phase B)で旧verifyサブコマンドが削除され、ローカル整合検査は
//! backup verify(src/storage.rs)経路で検証する。実ageバイナリを使用する。
//! Phase B実装前のためbackup verifyサブコマンドは未実装=各testはfailを許容。

mod common;

use backup_remote::crypto::decrypt_sha256;
use common::*;

/// 1件backup push済みの検証環境(keep_artifacts=alwaysでdata/outの.ageを保持)を用意する。
fn pushed_env(tag: &str, plain: &[u8]) -> Option<StorageFixture> {
    let fx = StorageFixture::new(
        tag,
        StorageOpts {
            keep_artifacts: "always",
            ..StorageOpts::default()
        },
    )?;
    let src = fx.data_root.join("a.zip");
    write_file(&src, plain);
    assert_eq!(fx.push(&[&src], &[], &[]).0, 0);
    Some(fx)
}

/// registryのfile_write記録のage_path(run dirの.age)。
fn pushed_age_path(fx: &StorageFixture) -> std::path::PathBuf {
    let write = fx
        .registry_values()
        .into_iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .expect("file_write記録");
    std::path::PathBuf::from(write["age_path"].as_str().unwrap())
}

/// [covers:verify.roundtrip-sha256-match]
/// --roundtripで復号平文sha256(ストリーム計算)がregistry記録(source_sha256)と
/// 一致することを検証する(backup verify --roundtrip)。
#[test]
fn roundtrip_sha256_match() {
    let Some(fx) = pushed_env("vr-match", b"verify-roundtrip-plaintext") else {
        return;
    };
    let (code, stdout, stderr) = fx.bin(
        &[
            "backup",
            "verify",
            "--config",
            fx.cfg.display().to_string().as_str(),
            "--key",
            fx.secret.display().to_string().as_str(),
            "--roundtrip",
        ],
        &[],
    );
    assert_eq!(code, 0, "roundtrip検証成功: {stdout}{stderr}");
}

/// [covers:verify.roundtrip-wrong-key-error]
/// --roundtripで鍵不一致(誤鍵)は処理エラー4。
#[test]
fn roundtrip_wrong_key_is_process_error() {
    let Some(fx) = pushed_env("vr-wrongkey", b"wrong-key-plaintext") else {
        return;
    };
    // 暗号化recipientと対にならない秘密鍵を別途生成
    let (wrong_pub, wrong_secret) = generate_ed25519_keypair(fx.elsewhere.as_path(), "wrong");
    let _ = wrong_pub;
    let (code, stdout, stderr) = fx.bin(
        &[
            "backup",
            "verify",
            "--config",
            fx.cfg.display().to_string().as_str(),
            "--key",
            wrong_secret.display().to_string().as_str(),
            "--roundtrip",
        ],
        &[],
    );
    assert_eq!(code, 4, "誤鍵は処理エラー4: {stdout}{stderr}");
}

/// [covers:verify.roundtrip-manifest-crosscheck]
/// --roundtripでmanifest復号→registry突合を行い、不一致を検出する
/// (backup verify --roundtrip。manifestはrun単位の実配置)。
#[test]
fn roundtrip_manifest_crosscheck_detects_mismatch() {
    let Some(fx) = pushed_env("vr-manifest", b"manifest-crosscheck-plain") else {
        return;
    };
    // registry記録と突合しないmanifestを作る: 本物を復号→source_sha256を改変→再暗号化
    let writes: Vec<_> = fx
        .registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .collect();
    assert_eq!(writes.len(), 1);
    let manifest = fx
        .out_dir
        .join(writes[0]["batch_id"].as_str().unwrap())
        .join("manifest.jsonl.age");
    let plain = age_decrypt(&fx.age, &manifest, &fx.secret);
    let text = String::from_utf8(plain).expect("manifest UTF-8");
    let tampered = text.replace(
        &format!(
            "\"source_sha256\": \"{}\"",
            writes[0]["source_sha256"].as_str().unwrap()
        ),
        "\"source_sha256\": \"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\"",
    );
    assert_ne!(tampered, text, "改変が反映されている");
    age_encrypt_bytes(
        &fx.age,
        &[fx.public.clone()],
        tampered.as_bytes(),
        &manifest,
    );

    let (code, stdout, stderr) = fx.bin(
        &[
            "backup",
            "verify",
            "--config",
            fx.cfg.display().to_string().as_str(),
            "--key",
            fx.secret.display().to_string().as_str(),
            "--roundtrip",
        ],
        &[],
    );
    assert_eq!(code, 4, "manifest突合不一致を検出: {stdout}{stderr}");
}

/// [covers:verify.decrypt-error-includes-age-stderr]
/// 復号のstdin投入threadエラー時(誤鍵でage -dが異常終了しEPIPEを受けるケース)の
/// エラーメッセージに、起因エラーとage stderrの両方が含まれる(encrypt側と対称)。
#[test]
fn decrypt_error_includes_age_stderr() {
    let Some(fx) = pushed_env("vr-stderr", b"decrypt-stderr-plain") else {
        return;
    };
    let age_file = pushed_age_path(&fx);
    // 誤鍵で復号 → age -dが異常終了し、stdin投入threadはEPIPEを受け取る
    let (wrong_pub, wrong_secret) = generate_ed25519_keypair(fx.elsewhere.as_path(), "wrong2");
    let _ = wrong_pub;
    let err = decrypt_sha256(&age_file, &wrong_secret).unwrap_err();
    let msg = format!("{err}");
    assert_eq!(err.exit_code(), 4, "処理エラー4: {msg}");
    assert!(msg.contains("age stderr:"), "age stderrの添付がある: {msg}");
    assert!(
        msg.contains("no identity matched") || msg.contains("failed"),
        "age stderrの実内容が含まれる: {msg}"
    );
}

/// [covers:verify.structure-recipients-bijection]
/// 実ageバイナリで暗号化した実際のv1.2形式ヘッダに対する構造検査(既定・--roundtripなし)
/// recipient照合: registry記録指紋とヘッダの指紋prefixが全単射一致する場合はexit 0、
/// 記録指紋を別鍵の実形式指紋へ書換えた(誤recipient・.age本体は不変)場合は検証エラー4。
/// テスト先行注記: Phase B実装(backup verify)前は未知サブコマンドでfailする。
#[test]
fn structure_check_recipient_bijection_with_real_age() {
    let Some(fx) = pushed_env("vr-bijection", b"recipient-bijection-plain") else {
        return;
    };

    // 正recipient: 実ageが生成したヘッダと記録指紋が対応 → 構造検査はexit 0
    let (code, stdout, stderr) = fx.bin(
        &[
            "backup",
            "verify",
            "--config",
            fx.cfg.display().to_string().as_str(),
        ],
        &[],
    );
    assert_eq!(code, 0, "正recipientは構造検査通過: {stdout}{stderr}");

    // 誤recipient: registry記録のrecipients指紋を別鍵の実形式指紋へ書換え(.ageは不変)
    let (other_pub, _other_secret) = generate_ed25519_keypair(fx.elsewhere.as_path(), "other");
    let other = backup_remote::fingerprint::parse_ssh_pubkey(&other_pub).expect("別鍵のparse");
    let wrong_fp = backup_remote::fingerprint::fingerprint(&other);
    let recs = fx.registry_values();
    let write = recs
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .expect("file_write記録");
    let right_fp = write["recipients"][0]
        .as_str()
        .expect("recipients[0]")
        .to_string();
    assert_ne!(wrong_fp, right_fp, "書換え先は別鍵の指紋");
    let text = std::fs::read_to_string(&fx.registry).expect("registry read");
    let tampered = text.replace(&right_fp, &wrong_fp);
    assert_ne!(tampered, text, "指紋の書換えが反映されている");
    write_file(&fx.registry, tampered.as_bytes());

    let (code, stdout, stderr) = fx.bin(
        &[
            "backup",
            "verify",
            "--config",
            fx.cfg.display().to_string().as_str(),
        ],
        &[],
    );
    assert_eq!(code, 4, "誤recipient指紋は検証エラー4: {stdout}{stderr}");
}
