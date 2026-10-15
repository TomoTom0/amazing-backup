//! registry/manifestを含むbatch単位の結合test(設計1章・8章)。
//! bin backup-remoteをsubprocess実行し、実ageでの暗号化とfake aws(PATH差替えの
//! 状態を持つS3 simulator)でのuploadを検証する。

mod common;

use common::*;
use std::path::{Path, PathBuf};

fn bin_env_age_only(tmp: &Path, tag: &str) -> Vec<(String, String)> {
    // ageのみを含むPATH(aws CLI不在を担保)
    let bin_dir = tmp.join(format!("bin-age-only-{tag}"));
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let age = find_age().expect("このtestはage必須");
    let link = bin_dir.join("age");
    if !link.exists() {
        std::os::unix::fs::symlink(&age, &link).expect("age symlink");
    }
    vec![("PATH".to_string(), bin_dir.display().to_string())]
}

fn envs_to_ref(envs: &[(String, String)]) -> Vec<(&str, &str)> {
    envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

/// [covers:encrypt.registry-append-fields]
/// registryへencrypt記録が1行appendされる(4.1 schemaどおりのフィールド型・JST時刻)。
#[test]
fn registry_append_fields() {
    let Some(()) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-reg");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, Some(&secret));
        let src = tmp.path().join("photo.zip");
        make_archive(&src, 4096);

        let (code, stdout, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &src.display().to_string(),
        ]);
        assert_eq!(code, 0, "encrypt成功: {stdout}{stderr}");

        let lines = read_registry_lines(&reg);
        assert_eq!(lines.len(), 1, "1行append: {lines:?}");
        let rec = &lines[0];
        assert_eq!(rec["schema"], 1);
        assert_eq!(rec["record_type"], "encrypt");
        assert_eq!(rec["batch_date"], serde_json::json!(jst_today()), "JST暦日");
        assert!(
            rec["recorded_at"].as_str().unwrap().ends_with("+09:00"),
            "JST ISO8601"
        );
        assert_eq!(rec["source_name"], "photo.zip");
        assert_eq!(rec["source_size"], serde_json::json!(4096));
        assert!(rec["source_mtime"].as_str().unwrap().ends_with("+09:00"));
        assert_eq!(
            rec["source_sha256"],
            serde_json::json!(sha256_hex(
                &std::fs::read(tmp.path().join("photo.zip")).unwrap()
            ))
        );
        let age_file = out.join(jst_today()).join("photo.zip.age");
        assert_eq!(
            rec["age_path"],
            serde_json::json!(age_file.display().to_string())
        );
        assert_eq!(
            rec["age_size"],
            serde_json::json!(std::fs::metadata(&age_file).unwrap().len())
        );
        assert_eq!(
            rec["age_sha256"],
            serde_json::json!(sha256_hex(&std::fs::read(&age_file).unwrap()))
        );
        let fps = rec["recipients"].as_array().unwrap();
        assert_eq!(fps.len(), 1);
        assert!(fps[0].as_str().unwrap().starts_with("SHA256:"));
        assert_eq!(
            rec["manifest_path"],
            serde_json::json!(
                out.join(jst_today())
                    .join("manifest.jsonl.age")
                    .display()
                    .to_string()
            )
        );
        assert!(
            rec["batch_id"]
                .as_str()
                .unwrap()
                .starts_with(&jst_today().replace('-', ""))
        );
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.reencrypt-without-registry-record]
/// .age存在でもregistry記録がない場合は上書き再暗号化される。
#[test]
fn reencrypt_without_registry_record() {
    let Some(()) = (|| {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-reenc");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, Some(&secret));
        let src = tmp.path().join("a.zip");
        let plain = b"reencrypt-plaintext".to_vec();
        write_file(&src, &plain);
        // 書込途中crash相当: 不完全な.ageのみ存在しregistryなし
        let age_file = out.join(jst_today()).join("a.zip.age");
        write_file(&age_file, b"TORN-PARTIAL-CONTENT");
        assert!(!reg.exists(), "registryはまだ無い");

        let (code, stdout, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &src.display().to_string(),
        ]);
        assert_eq!(code, 0, "上書き再暗号化でexit 0: {stdout}{stderr}");

        let on_disk = std::fs::read(&age_file).unwrap();
        assert_ne!(on_disk, b"TORN-PARTIAL-CONTENT".to_vec(), "上書きされる");
        assert_eq!(
            age_decrypt(&age, &age_file, &secret),
            plain,
            "新.ageは復号可能"
        );
        assert_eq!(read_registry_lines(&reg).len(), 1, "registryへ新記録append");
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.reencrypt-on-source-change]
/// sourceのsize/mtime変化時は再暗号化され、新記録がreduce後勝ちになる。
#[test]
fn reencrypt_on_source_change() {
    let Some(()) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-srcchg");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, None);
        let src = tmp.path().join("a.zip");
        write_file(&src, b"v1-content");

        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &src.display().to_string(),
        ]);
        assert_eq!(code, 0, "1回目: {stderr}");
        let first_age = std::fs::read(out.join(jst_today()).join("a.zip.age")).unwrap();

        // source変更(size・mtime)
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_file(&src, b"v2-content-changed-longer");

        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &src.display().to_string(),
        ]);
        assert_eq!(code, 0, "2回目: {stderr}");
        let second_age = std::fs::read(out.join(jst_today()).join("a.zip.age")).unwrap();
        assert_ne!(first_age, second_age, "再暗号化される");

        // registryは2行・reduce後は新記録が有効
        let lines = read_registry_lines(&reg);
        assert_eq!(lines.len(), 2, "再暗号化で二重記録(append-only)");
        assert_eq!(lines[0]["source_size"], serde_json::json!(10));
        assert_eq!(lines[1]["source_size"], serde_json::json!(25));
        assert_eq!(
            lines[1]["age_sha256"],
            serde_json::json!(sha256_hex(&second_age)),
            "新記録が現状を指す"
        );
        let records: Vec<backup_remote::registry::Record> = lines
            .iter()
            .map(|v| {
                let e: backup_remote::registry::EncryptRecord =
                    serde_json::from_value(v.clone()).expect("EncryptRecord");
                backup_remote::registry::Record::Encrypt(e)
            })
            .collect();
        let reduced = backup_remote::registry::reduce_records(&records);
        assert_eq!(reduced.encrypts.len(), 1, "同一age_pathは後勝ち");
        assert_eq!(reduced.encrypts[0].age_sha256, sha256_hex(&second_age));
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.manifest-content-roundtrip]
/// バッチmanifestが暗号化生成され、復号するとバッチヘッダ+ファイルレコードが正しい。
#[test]
fn manifest_content_roundtrip() {
    let Some(()) = (|| {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-manifest");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, Some(&secret));
        let a = tmp.path().join("a.zip");
        let b = tmp.path().join("b.7z");
        write_file(&a, b"manifest-content-A");
        write_file(&b, b"manifest-content-BB");

        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
            &b.display().to_string(),
        ]);
        assert_eq!(code, 0, "2件暗号化: {stderr}");

        let manifest = out.join(jst_today()).join("manifest.jsonl.age");
        assert!(manifest.exists(), "manifest生成");
        let plain = age_decrypt(&age, &manifest, &secret);
        let text = String::from_utf8(plain).expect("manifest平文はUTF-8 JSONL");
        let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.len(), 3, "ヘッダ1 + file 2");

        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["record_type"], "batch");
        assert_eq!(header["file_count"], 2);
        assert_eq!(header["total_source_size"], serde_json::json!(19 + 18));
        assert!(
            header["recipients"].as_array().unwrap()[0]
                .as_str()
                .unwrap()
                .starts_with("SHA256:")
        );

        let recs = read_registry_lines(&reg);
        for (i, line) in lines[1..].iter().enumerate() {
            let fr: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(fr["record_type"], "file");
            assert_eq!(
                fr["source_sha256"], recs[i]["source_sha256"],
                "registry記録と一致"
            );
            assert_eq!(
                fr["age_sha256"], recs[i]["age_sha256"],
                "registry記録と一致"
            );
        }
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.keep-going-partial-success]
/// --keep-goingで一部失敗しても成功分のregistry/manifestが作られ、exitは最初のエラー区分。
/// [covers:encrypt.keep-going-manifest-success-only]
/// keep-going時のmanifestには成功分のみ含まれる。
#[test]
fn keep_going_partial_success() {
    let Some(()) = (|| {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-keep");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, Some(&secret));
        let ok_src = tmp.path().join("ok.zip");
        let ng_src = tmp.path().join("ng.zip");
        write_file(&ok_src, b"keep-going-ok-plain");
        write_file(&ng_src, b"keep-going-ng-plain");
        // 読込不能化(解決は成功しreadで失敗する)。root環境では読めるためskipする
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&ng_src, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        if std::fs::read(&ng_src).is_ok() {
            eprintln!("skipping: 実行userがrootのため読込不能化が効かない");
            return None;
        }

        let (code, stdout, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            "--keep-going",
            &ok_src.display().to_string(),
            &ng_src.display().to_string(),
        ]);
        assert_eq!(code, 4, "最初のエラー区分(処理エラー): {stdout}{stderr}");

        let lines = read_registry_lines(&reg);
        assert_eq!(lines.len(), 1, "成功分のみregistry記録");
        assert_eq!(lines[0]["source_name"], "ok.zip");

        let manifest = out.join(jst_today()).join("manifest.jsonl.age");
        assert!(manifest.exists(), "成功0件でないためmanifest生成");
        let plain = age_decrypt(&age, &manifest, &secret);
        let text = String::from_utf8(plain).unwrap();
        let mlines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
        assert_eq!(mlines.len(), 2, "ヘッダ1 + 成功分1のみ");
        let header: serde_json::Value = serde_json::from_str(mlines[0]).unwrap();
        assert_eq!(header["file_count"], 1, "失敗対象を含まない");
        let fr: serde_json::Value = serde_json::from_str(mlines[1]).unwrap();
        assert_eq!(fr["source_name"], "ok.zip");
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.runs-without-aws]
/// encryptはaws CLI不在・aws.toml不在でも完結する(PATHをageのみに絞って担保)。
#[test]
fn encrypt_runs_without_aws() {
    let Some(()) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-noaws");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, None);
        let src = tmp.path().join("a.zip");
        write_file(&src, b"no-aws-encrypt-plain");
        assert!(
            !tmp.path().join("aws.toml").exists(),
            "aws.tomlは配置しない"
        );

        let envs = bin_env_age_only(tmp.path(), "enc");
        let (code, stdout, stderr) = run_bin_env(
            &[
                "encrypt",
                "--config",
                &cfg.display().to_string(),
                &src.display().to_string(),
            ],
            &envs_to_ref(&envs),
        );
        assert_eq!(code, 0, "aws不在でも暗号化完結: {stdout}{stderr}");
        assert!(
            out.join(jst_today()).join("a.zip.age").exists(),
            "成果物あり"
        );
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.name-override-paths]
/// name override: <out>/<date>/<name>.age出力・registry.source_nameがoverride名・
/// S3キー(<batch_date>/<name>.age)もoverride名。PATHは1個以上必須(CLI-005)のため
/// 位置引数にも別対象を渡し、--from-listとの和集合で実行する。
#[test]
fn name_override_paths() {
    let Some(()) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-name");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, None);
        let src = tmp.path().join("2025").join("docs.7z");
        write_file(&src, b"name-override-plain");
        // 位置引数側はname override無しの別対象(from-listと和集合)
        let other = tmp.path().join("other.zip");
        write_file(&other, b"name-override-other-plain");
        let list = tmp.path().join("targets.toml");
        write_file(
            &list,
            format!(
                "[[targets]]\npath = \"{}\"\nname = \"docs-2025.7z\"\n",
                src.display()
            )
            .as_bytes(),
        );

        let (code, stdout, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            "--from-list",
            &list.display().to_string(),
            &other.display().to_string(),
        ]);
        assert_eq!(code, 0, "name overrideで暗号化: {stdout}{stderr}");

        let age_file = out.join(jst_today()).join("docs-2025.7z.age");
        assert!(age_file.exists(), "<out>/<date>/<name>.ageに出力");
        assert!(
            out.join(jst_today()).join("other.zip.age").exists(),
            "和集合の位置引数側も出力"
        );
        let lines = read_registry_lines(&reg);
        let override_rec = lines
            .iter()
            .find(|l| l["source_name"] == "docs-2025.7z")
            .unwrap_or_else(|| panic!("source_nameがoverride名の記録がある: {lines:?}"));

        // upload列挙のS3キーもoverride名
        let records: Vec<backup_remote::registry::Record> = lines
            .iter()
            .map(|v| {
                let e: backup_remote::registry::EncryptRecord =
                    serde_json::from_value(v.clone()).expect("EncryptRecord");
                backup_remote::registry::Record::Encrypt(e)
            })
            .collect();
        let reduced = backup_remote::registry::reduce_records(&records);
        let pending = backup_remote::registry::pending_uploads_for_bucket(
            &reduced,
            None,
            "it-bucket",
            "it-bucket",
        );
        assert!(
            pending
                .iter()
                .any(|p| p.s3_key == format!("{}/docs-2025.7z.age", jst_today()))
        );
        let _ = override_rec;
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.skip-on-registry-match]
/// 再実行skip: .age存在+registry記録一致ならskip表示・registry追記なしでexit 0。
#[test]
fn rerun_shows_skip() {
    let Some(()) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-skip");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, None);
        let src = tmp.path().join("a.zip");
        write_file(&src, b"skip-check-plain");

        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &src.display().to_string(),
        ]);
        assert_eq!(code, 0, "1回目: {stderr}");
        let age_file = out.join(jst_today()).join("a.zip.age");
        let before = std::fs::read(&age_file).unwrap();

        let (code, stdout, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &src.display().to_string(),
        ]);
        assert_eq!(code, 0, "再実行もexit 0: {stderr}");
        assert!(stdout.to_lowercase().contains("skip"), "skip表示: {stdout}");
        assert_eq!(read_registry_lines(&reg).len(), 1, "registry追記なし");
        assert_eq!(std::fs::read(&age_file).unwrap(), before, ".age不変");
        Some(())
    })() else {
        return;
    };
}

/// fake S3 backend差替え(TASK-9まではPATH差替えのfake aws scriptをtests/commonの
/// write_fake_awsで書き出していた)は、TASK-9でbin内のfake seam
/// (env BACKUP_REMOTE_FAKE_S3_STATE・src/s3.rsのFakeS3)へ移行した。
/// 状態構造(objects/・restore/・calls.log)と失敗注入env 4種はtests/common/mod.rsのdoc参照。

/// [covers:encrypt.manifest-rebuilt-from-reduced-records]
/// 同日再実行で一部skip時、manifestは当該実行の成功分のみでなく
/// manifest_path単位のreduce後全encrypt記録(成功+skip済み)から再構築される。
/// 再実行後にverify --roundtripのmanifest突合が通る。
#[test]
fn manifest_rebuilt_from_reduced_records() {
    let Some(()) = (|| {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-manirebuild");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, Some(&secret));
        let a = tmp.path().join("a.zip");
        let b = tmp.path().join("b.7z");
        write_file(&a, b"manifest-rebuild-plain-A");
        write_file(&b, b"manifest-rebuild-plain-B");

        // 1回目: aのみ暗号化
        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
        ]);
        assert_eq!(code, 0, "1回目: {stderr}");
        assert_eq!(read_registry_lines(&reg).len(), 1);

        // 2回目(同日): a skip + b 新規成功
        let (code, stdout, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
            &b.display().to_string(),
        ]);
        assert_eq!(code, 0, "2回目: {stderr}");
        assert!(stdout.contains("skip"), "aはskip表示: {stdout}");
        assert_eq!(read_registry_lines(&reg).len(), 2, "新規成功分bのみ追記");

        // manifestはskip済みa + 新規b の2件分(reduce後全記録)で再構築されている
        let manifest = out.join(jst_today()).join("manifest.jsonl.age");
        let plain = age_decrypt(&age, &manifest, &secret);
        let text = String::from_utf8(plain).expect("manifest UTF-8");
        let mlines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
        assert_eq!(
            mlines.len(),
            3,
            "ヘッダ1 + ファイルレコード2(skip済み含む): {text}"
        );
        let header: serde_json::Value = serde_json::from_str(mlines[0]).unwrap();
        assert_eq!(header["file_count"], 2, "skip済みも含むfile_count: {text}");
        let names: Vec<String> = mlines[1..]
            .iter()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["source_name"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(
            names.iter().any(|n| n == "a.zip") && names.iter().any(|n| n == "b.7z"),
            "両対象を含む: {names:?}"
        );

        // verify --roundtripのmanifest突合(reduce後記録との一致)が通る
        let (code, stdout, stderr) = run_bin(&[
            "verify",
            "--config",
            &cfg.display().to_string(),
            "--roundtrip",
            "--key",
            &secret.display().to_string(),
        ]);
        assert_eq!(code, 0, "manifest突合を含むroundtrip検証: {stdout}{stderr}");
        Some(())
    })() else {
        return;
    };
}

/// [covers:encrypt.manifest-recovery-when-missing]
/// reduce後encrypt記録が存在するのにmanifestファイルが不在(前回の生成失敗相当)の
/// 場合、全skipの再実行でもmanifestが再構築されて復旧する。
#[test]
fn manifest_recovered_when_missing_on_all_skip() {
    let Some(()) = (|| {
        let age = find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-manirecov");
        let (public, secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, Some(&secret));
        let a = tmp.path().join("a.zip");
        write_file(&a, b"manifest-recovery-plain");
        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
        ]);
        assert_eq!(code, 0, "1回目: {stderr}");

        // 前回のmanifest生成失敗を模倣してmanifestを削除
        let manifest = out.join(jst_today()).join("manifest.jsonl.age");
        std::fs::remove_file(&manifest).expect("manifest削除");
        let reg_lines_before = read_registry_lines(&reg).len();

        // 全skipの再実行 → manifestが再構築される
        let (code, stdout, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
        ]);
        assert_eq!(code, 0, "全skip再実行: {stdout}{stderr}");
        assert!(stdout.contains("skip"), "skip表示: {stdout}");
        assert!(manifest.exists(), "manifestが再構築されている");
        assert_eq!(
            read_registry_lines(&reg).len(),
            reg_lines_before,
            "registry追記なし"
        );

        let plain = age_decrypt(&age, &manifest, &secret);
        let text = String::from_utf8(plain).expect("manifest UTF-8");
        let mlines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
        assert_eq!(mlines.len(), 2, "ヘッダ1 + レコード1");
        let header: serde_json::Value = serde_json::from_str(mlines[0]).unwrap();
        assert_eq!(header["file_count"], 1);

        // verifyのmanifest存在検査・roundtrip突合が通る
        let (code, stdout, stderr) = run_bin(&[
            "verify",
            "--config",
            &cfg.display().to_string(),
            "--roundtrip",
            "--key",
            &secret.display().to_string(),
        ]);
        assert_eq!(code, 0, "復旧後のverify: {stdout}{stderr}");
        Some(())
    })() else {
        return;
    };
}

/// [covers:upload.dry-run-missing-file-note]
/// --dry-runでも実ファイル不在の対象はPutObject予定でなく「対象エラー想定」注記を表示し、
/// 在る対象(manifest等)はPutObject予定行(API形式)を表示する。S3 API呼び出しは一切起きない。
#[test]
fn dry_run_notes_missing_file_targets() {
    let Some(()) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-drymiss");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, None);
        let a = tmp.path().join("a.zip");
        write_file(&a, b"dry-run-missing-plain");
        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
        ]);
        assert_eq!(code, 0, "encrypt: {stderr}");

        // 対象.ageを削除(manifestは残す)
        std::fs::remove_file(out.join(jst_today()).join("a.zip.age")).expect("remove .age");

        // fake S3 backend seam(BACKUP_REMOTE_FAKE_S3_STATE)でdry-run
        let state = tmp.path().join("s3state");
        std::fs::create_dir_all(&state).expect("state dir");
        let creds = write_fake_credentials(tmp.path());
        let aws_toml = tmp.path().join("aws.toml");
        write_file(
            &aws_toml,
            aws_toml_body("it-bucket", "ap-northeast-1", &creds, None).as_bytes(),
        );
        let state_dir = state.display().to_string();
        let envs: Vec<(&str, &str)> = vec![("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())];

        let (code, stdout, stderr) = run_bin_env(
            &[
                "upload",
                "--config",
                &cfg.display().to_string(),
                "--aws-config",
                &aws_toml.display().to_string(),
                "--dry-run",
            ],
            &envs,
        );
        assert_eq!(code, 0, "dry-runはexit 0: {stdout}{stderr}");
        assert!(
            stdout.contains("対象エラー想定") && stdout.contains("a.zip.age"),
            "不在対象の注記表示: {stdout}"
        );
        assert!(
            !stdout
                .lines()
                .any(|l| l.contains("[dry-run] s3 PutObject") && l.contains("a.zip.age")),
            "不在対象のPutObject予定行は表示しない: {stdout}"
        );
        assert!(
            stdout
                .lines()
                .any(|l| l.contains("[dry-run]") && l.contains("manifest.jsonl.age")),
            "在る対象のPutObject予定は表示: {stdout}"
        );
        // S3 API呼び出し(HeadObject/PutObject)は一切起きない
        assert!(!state.join("calls.log").exists(), "S3 API呼び出しなし");
        Some(())
    })() else {
        return;
    };
}

/// [covers:upload.cp-command-options]
/// upload時のPutObject呼び出しにstorage_class=DEEP_ARCHIVEとchecksum_algorithm SHA256が含まれる。
/// [covers:upload.s3-key-layout-and-manifest-last]
/// S3キーは<batch_date>/<source_name>.age・manifestは最後にuploadされる。
/// [covers:upload.success-record-append]
/// 成功後registryへupload記録(HeadObject実測値)・manifest_upload記録がappendされる。
/// [covers:upload.no-pending-exit-zero]
/// 全対象upload済みの再実行はexit 0で対象なし(PutObjectは再実行されない)。
#[test]
fn upload_via_fake_aws_stateful_simulator() {
    let Some(()) = (|| {
        find_age()?;
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygenが利用できない");
            return None;
        }
        let tmp = TestDir::new("eb-upfake");
        let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
        let out = tmp.path().join("out");
        let reg = tmp.path().join("registry.jsonl");
        let cfg = write_pipeline_toml(tmp.path(), &[public], &out, &reg, None);
        let a = tmp.path().join("a.zip");
        let b = tmp.path().join("b.7z");
        write_file(&a, b"fake-aws-plain-A");
        write_file(&b, b"fake-aws-plain-B");
        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
            &b.display().to_string(),
        ]);
        assert_eq!(code, 0, "encrypt: {stderr}");

        // fake S3 backend seam準備(BACKUP_REMOTE_FAKE_S3_STATE・状態dir)
        let date = jst_today();
        let state = tmp.path().join("s3state");
        std::fs::create_dir_all(&state).expect("state dir");
        let creds = write_fake_credentials(tmp.path());
        let aws_toml = tmp.path().join("aws.toml");
        write_file(
            &aws_toml,
            aws_toml_body("it-bucket", "ap-northeast-1", &creds, Some("DEEP_ARCHIVE")).as_bytes(),
        );
        let state_dir = state.display().to_string();
        let envs: Vec<(&str, &str)> = vec![("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())];

        let (code, stdout, stderr) = run_bin_env(
            &[
                "upload",
                "--config",
                &cfg.display().to_string(),
                "--aws-config",
                &aws_toml.display().to_string(),
            ],
            &envs,
        );
        assert_eq!(code, 0, "upload成功: {stdout}{stderr}");

        // fake backend上のobject配置
        let age_a = std::fs::read(out.join(&date).join("a.zip.age")).unwrap();
        let age_b = std::fs::read(out.join(&date).join("b.7z.age")).unwrap();
        let age_m = std::fs::read(out.join(&date).join("manifest.jsonl.age")).unwrap();
        assert_eq!(
            std::fs::read(state.join("objects").join(&date).join("a.zip.age")).unwrap(),
            age_a
        );
        assert_eq!(
            std::fs::read(state.join("objects").join(&date).join("b.7z.age")).unwrap(),
            age_b
        );
        assert_eq!(
            std::fs::read(state.join("objects").join(&date).join("manifest.jsonl.age")).unwrap(),
            age_m
        );

        // PutObject呼び出し参数とmanifest最後(README配置(2.3-7)のPutObject行はUPL-020系testで検証するため除外)
        let calls = read_calls(&state);
        let put_lines: Vec<&str> = calls
            .lines()
            .filter(|l| l.starts_with("s3 PutObject") && !l.contains("RESTORE_README.txt"))
            .collect();
        assert_eq!(put_lines.len(), 3, "PutObject 3回: {calls}");
        assert!(
            put_lines
                .iter()
                .all(|l| l.contains("storage_class=DEEP_ARCHIVE")),
            "storage_class: {calls}"
        );
        assert!(
            put_lines.iter().all(|l| l.contains("checksum=SHA256")),
            "checksum_algorithm SHA256: {calls}"
        );
        assert!(
            put_lines.iter().all(|l| l.contains("bucket=it-bucket")),
            "bucket参数: {calls}"
        );
        assert!(
            put_lines
                .iter()
                .all(|l| l.contains(&format!("key={date}/"))),
            "key参数: {calls}"
        );
        assert!(
            put_lines[2].contains("manifest.jsonl.age"),
            "manifestが最後: {calls}"
        );

        // registry記録(S3側実測値 = sha256 base64)
        let lines = read_registry_lines(&reg);
        let uploads: Vec<&serde_json::Value> = lines
            .iter()
            .filter(|l| l["record_type"] == "upload")
            .collect();
        let mus: Vec<&serde_json::Value> = lines
            .iter()
            .filter(|l| l["record_type"] == "manifest_upload")
            .collect();
        assert_eq!(uploads.len(), 2);
        assert_eq!(mus.len(), 1);
        let keys: Vec<&str> = uploads
            .iter()
            .map(|r| r["s3_key"].as_str().unwrap())
            .collect();
        assert!(
            keys.contains(&format!("{date}/a.zip.age").as_str()),
            "キー配置: {keys:?}"
        );
        assert!(
            keys.contains(&format!("{date}/b.7z.age").as_str()),
            "キー配置: {keys:?}"
        );
        for rec in &uploads {
            let bytes = if rec["s3_key"].as_str().unwrap().ends_with("a.zip.age") {
                &age_a
            } else {
                &age_b
            };
            assert_eq!(rec["s3_size"].as_i64(), Some(bytes.len() as i64));
            let want = sha256_b64(bytes);
            assert_eq!(rec["s3_checksum_sha256"].as_str(), Some(want.as_str()));
            assert_eq!(rec["s3_checksum_type"], "FULL_OBJECT");
            // 4.1: upload/manifest_upload記録には実効bucket/regionを記録する
            assert_eq!(rec["s3_bucket"], "it-bucket", "実効bucket記録: {rec}");
            assert_eq!(rec["s3_region"], "ap-northeast-1", "実効region記録: {rec}");
        }
        assert_eq!(
            mus[0]["s3_key"],
            serde_json::json!(format!("{date}/manifest.jsonl.age"))
        );
        assert_eq!(
            mus[0]["s3_bucket"], "it-bucket",
            "manifest_upload記録の実効bucket: {}",
            mus[0]
        );
        assert_eq!(
            mus[0]["s3_region"], "ap-northeast-1",
            "manifest_upload記録の実効region: {}",
            mus[0]
        );

        // 再実行: 全対象upload済み → exit 0・PutObjectは増えない(対象なし。READMEのskipはUPL-021系testで検証)
        let put_count_before = put_lines.len();
        let (code, stdout, stderr) = run_bin_env(
            &[
                "upload",
                "--config",
                &cfg.display().to_string(),
                "--aws-config",
                &aws_toml.display().to_string(),
            ],
            &envs,
        );
        assert_eq!(code, 0, "再実行exit 0: {stdout}{stderr}");
        let calls2 = read_calls(&state);
        let put_count_after = calls2
            .lines()
            .filter(|l| l.starts_with("s3 PutObject") && !l.contains("RESTORE_README.txt"))
            .count();
        assert_eq!(
            put_count_after, put_count_before,
            "skip判定でPutObjectは再実行されない"
        );
        Some(())
    })() else {
        return;
    };
}

/// fake S3 backend seam環境(BACKUP_REMOTE_FAKE_S3_STATE・追加env)でuploadをbin実行し
/// (exit code, stdout, stderr)を返す(UploadOverrideFixture・README系testの共用)。
fn upload_with_fake_aws(
    cfg: &Path,
    aws_toml: &Path,
    state: &Path,
    cli_args: &[&str],
    extra_env: &[(&str, &str)],
) -> (i32, String, String) {
    let cfg_s = cfg.display().to_string();
    let aws_s = aws_toml.display().to_string();
    let state_dir = state.display().to_string();
    let mut envs: Vec<(&str, &str)> = vec![("BACKUP_REMOTE_FAKE_S3_STATE", state_dir.as_str())];
    envs.extend_from_slice(extra_env);
    let mut args: Vec<&str> = vec!["upload", "--config", &cfg_s, "--aws-config", &aws_s];
    args.extend_from_slice(cli_args);
    run_bin_env(&args, &envs)
}

/// CLI一時上書き系テスト(TASK-6・設計2.3 TASK-6節)とRESTORE_README系テスト
/// (TASK-8・UPL-020〜022)の共通fixture:
/// 1ファイルencrypt済みのregistryとfake S3 backend seam(BACKUP_REMOTE_FAKE_S3_STATE)・
/// aws.toml(bucket=it-bucket・region=ap-northeast-1・credentials_file必須)を用意する。
/// storage_classはOption(省略時は既定DEEP_ARCHIVE。--profileはTASK-9で廃止済み)。
struct UploadOverrideFixture {
    /// tmp dirの生存保持(Dropで削除されるため読み出しはしない)
    #[allow(dead_code)]
    tmp: TestDir,
    cfg: PathBuf,
    aws_toml: PathBuf,
    registry: PathBuf,
    state: PathBuf,
}

impl UploadOverrideFixture {
    /// age/ssh-keygenが使えない環境ではNone(呼び出し側testはskip扱い)。
    fn new(tag: &str, storage_class: Option<&str>) -> Option<Self> {
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
        write_file(&a, b"cli-override-plain");
        let (code, _, stderr) = run_bin(&[
            "encrypt",
            "--config",
            &cfg.display().to_string(),
            &a.display().to_string(),
        ]);
        assert_eq!(code, 0, "encrypt: {stderr}");

        let state = tmp.path().join("s3state");
        std::fs::create_dir_all(&state).expect("state dir");
        let creds = write_fake_credentials(tmp.path());
        let aws_toml = tmp.path().join("aws.toml");
        write_file(
            &aws_toml,
            aws_toml_body("it-bucket", "ap-northeast-1", &creds, storage_class).as_bytes(),
        );
        Some(Self {
            tmp,
            cfg,
            aws_toml,
            registry,
            state,
        })
    }

    /// fake S3 backend seam環境でuploadを実行し(exit code, stdout, stderr)を返す。
    fn upload(&self, cli_args: &[&str]) -> (i32, String, String) {
        upload_with_fake_aws(&self.cfg, &self.aws_toml, &self.state, cli_args, &[])
    }

    /// 追加の環境変数(FAKE_S3_FAIL_CP_FOR等)も渡してuploadを実行する(UPL-022の失敗注入)。
    fn upload_env(&self, cli_args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String, String) {
        upload_with_fake_aws(&self.cfg, &self.aws_toml, &self.state, cli_args, extra_env)
    }

    /// fake backendのAPI呼び出し記録(calls.log。未実行なら空文字列)。
    fn calls(&self) -> String {
        read_calls(&self.state)
    }

    /// registry.jsonlの全行(JSON Value)。
    fn registry_lines(&self) -> Vec<serde_json::Value> {
        read_registry_lines(&self.registry)
    }
}

/// [covers:upload.cli-override-reflected-in-aws-argv]
/// CLI一時上書き: --storage-class/--bucket/--region指定時、当該実行のS3 API呼び出し
/// (HeadObject・PutObjectの両方)へ上書き値が反映される(--profileはTASK-9で廃止)。
/// upload/manifest_upload記録には当該実行の実効bucket/regionをs3_bucket/s3_regionとして記録(4.1)。
/// --helpの説明文に「当該実行限りの上書き(既定はaws.toml値)」と優先順位
/// (CLI指定値 > aws.toml値・aws.tomlが正本)が表示される。
#[test]
fn cli_override_reflected_in_aws_argv_and_help_text() {
    let Some(fx) = UploadOverrideFixture::new("eb-ovargv", Some("DEEP_ARCHIVE")) else {
        return;
    };
    let (code, stdout, stderr) = fx.upload(&[
        "--storage-class",
        "GLACIER",
        "--bucket",
        "alt-bucket",
        "--region",
        "us-east-1",
    ]);
    assert_eq!(code, 0, "上書き付きupload成功: {stdout}{stderr}");

    let calls = fx.calls();
    let head_lines: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 HeadObject"))
        .collect();
    // README配置(2.3-7)のPutObject行(storage_class=STANDARD固定)はUPL-020系testで検証するため除外
    let put_lines: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && !l.contains("RESTORE_README.txt"))
        .collect();
    assert!(
        !head_lines.is_empty() && !put_lines.is_empty(),
        "HeadObject・PutObjectとも実行: {calls}"
    );
    // bucketはHeadObject/PutObject呼び出しのbucket参数で検証(SDKはURIでなくbucket/key参数)
    for l in head_lines.iter().chain(put_lines.iter()) {
        assert!(
            l.contains("bucket=alt-bucket"),
            "HeadObject/PutObjectのbucketへ反映: {calls}"
        );
    }
    assert!(
        put_lines
            .iter()
            .all(|l| l.contains("storage_class=GLACIER")),
        "PutObjectのstorage_classへ反映: {calls}"
    );
    assert!(
        put_lines.iter().all(|l| l.contains("checksum=SHA256")),
        "PutObjectのchecksum指定は不変: {calls}"
    );

    // registry記録のs3_bucket/s3_regionは当該実行の実効値(2.3 TASK-6節・4.1。
    // regionはBackend構築(Region::new)へ反映されるため呼び出し記録でなくこちらで検証)
    let lines = fx.registry_lines();
    let recs: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|r| r["record_type"] == "upload" || r["record_type"] == "manifest_upload")
        .collect();
    assert_eq!(recs.len(), 2, "upload 1 + manifest_upload 1");
    for rec in &recs {
        assert_eq!(rec["s3_bucket"], "alt-bucket", "実効bucket記録: {rec}");
        assert_eq!(rec["s3_region"], "us-east-1", "実効region記録: {rec}");
    }

    // --help文言: 上書きである旨と優先順位(2.3 TASK-6節)
    let (code, stdout, stderr) = run_bin(&["upload", "--help"]);
    assert_eq!(code, 0, "upload --helpはexit 0: {stderr}");
    assert!(
        stdout.contains("当該実行限りの上書き"),
        "help説明文(一時上書きの旨): {stdout}"
    );
    assert!(
        stdout.contains("CLI指定値 > aws.toml値"),
        "help説明文(優先順位): {stdout}"
    );
    assert!(
        stdout.contains("aws.tomlが正本"),
        "help説明文(正本): {stdout}"
    );
}

/// [covers:upload.cli-override-unspecified-keeps-toml-values]
/// CLI上書き時、未指定の項目はaws.toml値が使われ、実行後もaws.tomlファイルの内容は
/// 不変(正本の書戻しを行わない)。
#[test]
fn cli_override_unspecified_keeps_toml_values() {
    let Some(fx) = UploadOverrideFixture::new("eb-ovunspec", Some("GLACIER")) else {
        return;
    };
    let before = std::fs::read(&fx.aws_toml).expect("aws.toml読み");
    let (code, stdout, stderr) = fx.upload(&["--storage-class", "STANDARD"]);
    assert_eq!(code, 0, "部分上書きupload成功: {stdout}{stderr}");

    let calls = fx.calls();
    let head_lines: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 HeadObject"))
        .collect();
    // README配置(2.3-7)のPutObject行(storage_class=STANDARD固定)はUPL-020系testで検証するため除外
    let put_lines: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && !l.contains("RESTORE_README.txt"))
        .collect();
    assert!(
        !head_lines.is_empty() && !put_lines.is_empty(),
        "HeadObject・PutObjectとも実行: {calls}"
    );
    // 指定したstorage-classのみ上書きされる
    assert!(
        put_lines
            .iter()
            .all(|l| l.contains("storage_class=STANDARD")),
        "上書き項目はCLI値: {calls}"
    );
    // 未指定項目はaws.toml値(bucket: HeadObject/PutObjectのbucket参数で検証。
    // regionはBackend構築へ反映されるためregistry記録(s3_region)で検証)
    for l in head_lines.iter().chain(put_lines.iter()) {
        assert!(l.contains("bucket=it-bucket"), "bucketはtoml値: {calls}");
    }
    let lines = fx.registry_lines();
    let recs: Vec<&serde_json::Value> = lines
        .iter()
        .filter(|r| r["record_type"] == "upload" || r["record_type"] == "manifest_upload")
        .collect();
    assert_eq!(recs.len(), 2, "upload 1 + manifest_upload 1");
    for rec in &recs {
        assert_eq!(rec["s3_region"], "ap-northeast-1", "regionはtoml値: {rec}");
    }

    // aws.tomlは実行後も不変(正本の書戻しを行わない)
    let after = std::fs::read(&fx.aws_toml).expect("aws.toml再読み");
    assert_eq!(before, after, "aws.tomlの内容は不変");
}

/// [covers:upload.cli-storage-class-allowlist-rejected]
/// --storage-class上書き値が3.3のallowlist外の場合は設定エラー(exit 2)となり、
/// S3 API呼び出しは一切行われない。空文字・空白のみは未指定扱い(aws.toml値を採用)で
/// エラーとしない。
#[test]
fn cli_storage_class_override_allowlist() {
    let Some(fx) = UploadOverrideFixture::new("eb-ovsc", Some("DEEP_ARCHIVE")) else {
        return;
    };
    // allowlist外(打ち間違い)は設定エラー2・S3 API呼び出しなし
    let (code, stdout, stderr) = fx.upload(&["--storage-class", "DEEP_ARCIVE"]);
    assert_eq!(code, 2, "allowlist外はexit 2: {stdout}{stderr}");
    assert_eq!(fx.calls(), "", "S3 API呼び出しは一切行われない");

    // 空文字は未指定扱い: exit 0でaws.toml値DEEP_ARCHIVEでPutObject
    let (code, stdout, stderr) = fx.upload(&["--storage-class", ""]);
    assert_eq!(code, 0, "空文字は未指定扱い: {stdout}{stderr}");
    let calls = fx.calls();
    let put_lines: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && !l.contains("RESTORE_README.txt"))
        .collect();
    assert!(!put_lines.is_empty(), "PutObject実行: {calls}");
    assert!(
        put_lines
            .iter()
            .all(|l| l.contains("storage_class=DEEP_ARCHIVE")),
        "aws.tomlのstorage_class値を採用: {calls}"
    );

    // 空白のみも未指定扱い(上の実行で対象がupload済みになるため別fixture)
    let Some(fx2) = UploadOverrideFixture::new("eb-ovsc2", Some("DEEP_ARCHIVE")) else {
        return;
    };
    let (code, stdout, stderr) = fx2.upload(&["--storage-class", " "]);
    assert_eq!(code, 0, "空白のみは未指定扱い: {stdout}{stderr}");
    let calls2 = fx2.calls();
    let put2: Vec<&str> = calls2
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && !l.contains("RESTORE_README.txt"))
        .collect();
    assert!(!put2.is_empty(), "PutObject実行: {calls2}");
    assert!(
        put2.iter()
            .all(|l| l.contains("storage_class=DEEP_ARCHIVE")),
        "aws.tomlのstorage_class値を採用: {calls2}"
    );
}

/// [covers:upload.cli-bucket-region-blank-rejected]
/// --bucket/--region上書き値が空文字・空白のみ・前後空白付きの場合は設定エラー
/// (exit 2)となり、S3 API呼び出しは一切行われない(値のtrimは行わない)。
#[test]
fn cli_bucket_region_blank_override_rejected() {
    let Some(fx) = UploadOverrideFixture::new("eb-ovblank", None) else {
        return;
    };
    for arg in ["--bucket", "--region"] {
        for invalid in ["", " ", " padded-value "] {
            let (code, stdout, stderr) = fx.upload(&[arg, invalid]);
            assert_eq!(
                code, 2,
                "{arg}の不正値({invalid:?})はexit 2: {stdout}{stderr}"
            );
        }
    }
    assert_eq!(fx.calls(), "", "S3 API呼び出しは一切行われない");
}

/// [covers:upload.cli-override-with-dry-run]
/// CLI一時上書きと--dry-runの併用: 上書き値を反映した実行予定のAPI形式行が表示され、
/// S3 APIは呼ばれない(regionはBackend構築時固定のためdry-run行に現れず
/// registry記録(s3_region)で検証する)。
#[test]
fn cli_override_with_dry_run_shows_planned_command() {
    let Some(fx) = UploadOverrideFixture::new("eb-ovdry", Some("DEEP_ARCHIVE")) else {
        return;
    };
    let (code, stdout, stderr) = fx.upload(&[
        "--dry-run",
        "--storage-class",
        "GLACIER",
        "--bucket",
        "alt-bucket",
        "--region",
        "us-east-1",
    ]);
    assert_eq!(code, 0, "dry-run併用はexit 0: {stdout}{stderr}");
    // README配置予定行(storage_class=STANDARD固定。2.3-7)はUPL-020系testで検証するため除外
    let planned: Vec<&str> = stdout
        .lines()
        .filter(|l| {
            l.contains("[dry-run]")
                && l.contains("s3 PutObject")
                && !l.contains("RESTORE_README.txt")
        })
        .collect();
    assert!(!planned.is_empty(), "実行予定のPutObject行表示: {stdout}");
    for l in &planned {
        assert!(
            l.contains("storage_class=GLACIER"),
            "予定行へstorage_class反映: {stdout}"
        );
        assert!(
            l.contains("bucket=alt-bucket"),
            "予定行へbucket反映: {stdout}"
        );
        assert!(
            l.contains("checksum=SHA256"),
            "予定行へchecksum指定: {stdout}"
        );
    }
    assert_eq!(fx.calls(), "", "S3 APIは呼ばれない");
}
