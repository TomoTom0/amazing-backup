//! registry/manifestを含むbatch単位の結合test(設計1章・8章・TASK-10設計2.2・7.3・8.1)。
//! TASK-11(Phase B)で旧encrypt/uploadサブコマンドが削除され、暗号化・registry記録・
//! manifest構築・S3転送はbackup push(暗号化→upload→manifest→索引更新の統合run)経路で
//! 検証する。実ageでの暗号化とfake seam(BACKUP_REMOTE_FAKE_S3_STATE)のbin subprocess実行。

mod common;

use common::*;

/// registryのfile_write記録を取り出す。
fn file_writes(fx: &StorageFixture) -> Vec<serde_json::Value> {
    fx.registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .collect()
}

/// registryのfile_upload記録を取り出す。
fn file_uploads(fx: &StorageFixture) -> Vec<serde_json::Value> {
    fx.registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload"))
        .collect()
}

/// registryのfile_write記録からbatch_idを登場順に読む。
fn run_batch_ids(fx: &StorageFixture) -> Vec<String> {
    let mut ids = Vec::new();
    for r in fx.registry_values() {
        if r.get("record_type").and_then(|t| t.as_str()) == Some("file_write") {
            if let Some(id) = r.get("batch_id").and_then(|v| v.as_str()) {
                if !ids.contains(&id.to_string()) {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids
}

/// [covers:encrypt.registry-append-fields]
/// 暗号化成功時にregistryへfile_write記録(新経路event・設計7.3/8.1のschema)が
/// 1行appendされる(backup pushでの暗号化成功)。
#[test]
fn registry_append_fields() {
    let Some(fx) = StorageFixture::new(
        "eb-reg",
        StorageOpts {
            keep_artifacts: "always",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let src = fx.data_root.join("photo.zip");
    make_archive(&src, 4096);
    assert_eq!(fx.push(&[&src], &[], &[]).0, 0);

    let writes = file_writes(&fx);
    assert_eq!(writes.len(), 1, "file_write記録は1行: {writes:?}");
    let rec = &writes[0];
    assert_eq!(rec["schema"], 1);
    assert_eq!(rec["record_type"], "file_write");
    assert!(
        rec["recorded_at"].as_str().unwrap().ends_with("+09:00"),
        "JST ISO8601"
    );
    assert!(
        rec["batch_id"]
            .as_str()
            .unwrap()
            .starts_with(&jst_today().replace('-', "")),
        "batch_idは日付起点: {}",
        rec["batch_id"]
    );
    assert_eq!(rec["source_name"], "photo.zip");
    assert_eq!(rec["source_size"], serde_json::json!(4096));
    assert!(rec["source_mtime"].as_str().unwrap().ends_with("+09:00"));
    assert_eq!(
        rec["source_sha256"],
        serde_json::json!(sha256_hex(&std::fs::read(&src).unwrap())),
        "平文sha256(1pass計算)"
    );
    // run単位の成果物配置: <out>/<batch_id>/<name>.age
    let batch_id = rec["batch_id"].as_str().unwrap();
    let age_file = fx.out_dir.join(batch_id).join("photo.zip.age");
    assert!(
        age_file.is_file(),
        "run単位の.age配置: {}",
        age_file.display()
    );
    assert_eq!(
        rec["age_path"],
        serde_json::json!(age_file.display().to_string()),
        "age_pathは実配置"
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
    // layout分類解決の記録(category/rel_path/identity_hash/folder/s3_key)
    assert_eq!(rec["category"], "unfiled");
    assert_eq!(rec["rel_path"], "");
    assert_eq!(
        rec["identity_hash"],
        serde_json::json!(hash8_of_path(&src)),
        "identity_hash=sha256(realpath)先頭8hex"
    );
    assert_eq!(rec["folder"], "photo.zip");
    assert_eq!(
        rec["s3_key"],
        serde_json::json!(fx.file_key("unfiled", None, "photo.zip")),
        "s3_keyは実key"
    );
}

/// [covers:encrypt.reencrypt-without-registry-record]
/// registryに成功記録(file_write)がない対象は再暗号化される(.age等の産物のみ残る
/// 書込途中crash等の状態。skipはregistry基準のため記録がなければskipしない)。
#[test]
fn reencrypt_without_registry_record() {
    let Some(fx) = StorageFixture::new(
        "eb-reenc",
        StorageOpts {
            keep_artifacts: "always",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let src = fx.data_root.join("a.zip");
    let plain = b"reencrypt-plaintext".to_vec();
    write_file(&src, &plain);
    assert_eq!(fx.push(&[&src], &[], &[]).0, 0);
    let writes_before = file_writes(&fx);
    assert_eq!(writes_before.len(), 1);

    // 書込途中crash相当: registryのfile_write記録を消す(.age産物は残る)
    let all = fx.registry_values();
    let kept: Vec<serde_json::Value> = all
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) != Some("file_write"))
        .collect();
    let mut text = String::new();
    for r in kept {
        text.push_str(&serde_json::to_string(&r).unwrap());
        text.push('\n');
    }
    write_file(&fx.registry, text.as_bytes());

    let (code, stdout, stderr) = fx.push(&[&src], &[], &[]);
    assert_eq!(code, 0, "記録なしは再暗号化でexit 0: {stdout}{stderr}");
    let writes_after = file_writes(&fx);
    assert_eq!(writes_after.len(), 1, "registryへ新file_write記録をappend");
    assert_ne!(
        writes_after[0]["age_path"], writes_before[0]["age_path"],
        "新runの.ageが生成される(記録なしはskipしない)"
    );
    // 新.ageは実鍵で復号できる
    let new_age = fx
        .out_dir
        .join(writes_after[0]["batch_id"].as_str().unwrap())
        .join("a.zip.age");
    assert_eq!(
        age_decrypt(&fx.age, &new_age, &fx.secret),
        plain,
        "新.ageは復号可能"
    );
}

/// [covers:encrypt.reencrypt-on-source-change]
/// sourceのsize/mtime変化時は再暗号化され、新記録がreduce後勝ちになる
/// (backup pushのskip指紋不一致・identity_hashキーの後勝ち)。
#[test]
fn reencrypt_on_source_change() {
    let Some(fx) = StorageFixture::new(
        "eb-srcchg",
        StorageOpts {
            keep_artifacts: "always",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let src = fx.data_root.join("a.zip");
    write_file(&src, b"v1-content");
    assert_eq!(fx.push(&[&src], &[], &[]).0, 0);

    // source変更(size・mtime)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&src, b"v2-content-changed-longer");
    let (code, stdout, stderr) = fx.push(&[&src], &[], &[]);
    assert_eq!(code, 0, "変更後のpush: {stdout}{stderr}");

    let writes = file_writes(&fx);
    assert_eq!(writes.len(), 2, "再暗号化で記録が2件(append-only)");
    assert_eq!(writes[0]["source_size"], serde_json::json!(10));
    assert_eq!(writes[1]["source_size"], serde_json::json!(25));
    let second_age = fx
        .out_dir
        .join(writes[1]["batch_id"].as_str().unwrap())
        .join("a.zip.age");
    assert_eq!(
        writes[1]["age_sha256"],
        serde_json::json!(sha256_hex(&std::fs::read(&second_age).unwrap())),
        "新記録が現状を指す"
    );

    // reduce後は同一identity_hashの新記録(file_writes)が有効
    let records: Vec<backup_remote::registry::Record> = writes
        .iter()
        .map(|v| {
            let e: backup_remote::registry::FileWriteRecord =
                serde_json::from_value(v.clone()).expect("FileWriteRecord");
            backup_remote::registry::Record::FileWrite(e)
        })
        .collect();
    let reduced = backup_remote::registry::reduce_records(&records);
    assert_eq!(reduced.file_writes.len(), 1, "同一identityは後勝ち");
    assert_eq!(
        reduced.file_writes[0].age_sha256,
        sha256_hex(&std::fs::read(&second_age).unwrap())
    );
}

/// [covers:encrypt.manifest-content-roundtrip]
/// runのmanifestが暗号化生成され、復号するとバッチヘッダ+ファイルレコードが正しい
/// (backup pushのmanifestはfile upload後・version_id確定後に構築され、file recordは
/// 実s3_key+version_idを記録する拡張schema)。
#[test]
fn manifest_content_roundtrip() {
    let Some(fx) = StorageFixture::new(
        "eb-manifest",
        StorageOpts {
            keep_artifacts: "always",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.elsewhere.join("b.7z");
    write_file(&a, b"manifest-content-A");
    write_file(&b, b"manifest-content-BB");
    assert_eq!(fx.push(&[&a, &b], &[], &[]).0, 0);

    let batch_ids = run_batch_ids(&fx);
    assert_eq!(batch_ids.len(), 1, "1 run: {batch_ids:?}");
    let manifest = fx.out_dir.join(&batch_ids[0]).join("manifest.jsonl.age");
    assert!(manifest.is_file(), "run単位のmanifest生成");
    let plain = age_decrypt(&fx.age, &manifest, &fx.secret);
    let text = String::from_utf8(plain).expect("manifest平文はUTF-8 JSONL");
    let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    assert_eq!(lines.len(), 3, "ヘッダ1 + file 2");

    let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(header["record_type"], "batch");
    assert_eq!(header["batch_id"], serde_json::json!(batch_ids[0]));
    assert_eq!(header["file_count"], 2);
    assert_eq!(header["total_source_size"], serde_json::json!(19 + 18));
    assert!(
        header["recipients"].as_array().unwrap()[0]
            .as_str()
            .unwrap()
            .starts_with("SHA256:")
    );

    // file record: registryのfile_upload記録と一致し、実s3_key+version_idを含む
    let uploads = file_uploads(&fx);
    assert_eq!(uploads.len(), 2);
    let keys: Vec<String> = ["a.zip", "b.7z"]
        .iter()
        .map(|n| fx.file_key("unfiled", None, n))
        .collect();
    for line in &lines[1..] {
        let fr: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(fr["record_type"], "file");
        let name = fr["name"].as_str().unwrap().to_string();
        let up = uploads
            .iter()
            .find(|u| u["name"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("registry file_upload記録({name}): {uploads:?}"));
        assert_eq!(
            fr["source_sha256"], up["source_sha256"],
            "registry記録と一致"
        );
        assert_eq!(fr["age_sha256"], up["age_sha256"], "registry記録と一致");
        assert_eq!(fr["s3_key"], up["s3_key"], "実s3_keyを記録");
        assert!(
            keys.contains(&fr["s3_key"].as_str().unwrap().to_string()),
            "s3_keyは新配置の実key: {fr}"
        );
        assert_eq!(fr["version_id"], up["version_id"], "version_idを記録");
        assert!(
            !fr["version_id"].as_str().unwrap_or("").is_empty(),
            "version_idは空でない(upload後に構築)"
        );
    }
}

/// [covers:encrypt.keep-going-partial-success]
/// --keep-goingで一部失敗しても成功分のregistry/manifestが作られ、exitは最初のエラー区分
/// (backup pushの暗号化段階)。
/// [covers:encrypt.keep-going-manifest-success-only]
/// keep-going時のmanifestには成功分のみ含まれる(backup pushのmanifest)。
#[test]
fn keep_going_partial_success() {
    let Some(fx) = StorageFixture::new(
        "eb-keep",
        StorageOpts {
            keep_artifacts: "always",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let ok_src = fx.data_root.join("ok.zip");
    let ng_src = fx.data_root.join("ng.zip");
    write_file(&ok_src, b"keep-going-ok-plain");
    write_file(&ng_src, b"keep-going-ng-plain");
    // 読込不能化(解決は成功しreadで失敗する)。root環境では読めるためskipする
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&ng_src, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read(&ng_src).is_ok() {
        eprintln!("skipping: 実行userがrootのため読込不能化が効かない");
        return;
    }

    let (code, stdout, stderr) = fx.push(&[&ok_src, &ng_src], &["--keep-going"], &[]);
    assert_eq!(code, 4, "最初のエラー区分(処理エラー): {stdout}{stderr}");

    let writes = file_writes(&fx);
    assert_eq!(writes.len(), 1, "成功分のみfile_write記録");
    assert_eq!(writes[0]["source_name"], "ok.zip");

    let batch_ids = run_batch_ids(&fx);
    assert_eq!(batch_ids.len(), 1);
    let manifest = fx.out_dir.join(&batch_ids[0]).join("manifest.jsonl.age");
    assert!(manifest.is_file(), "成功0件でないためmanifest生成");
    let plain = age_decrypt(&fx.age, &manifest, &fx.secret);
    let text = String::from_utf8(plain).unwrap();
    let mlines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    assert_eq!(mlines.len(), 2, "ヘッダ1 + 成功分1のみ");
    let header: serde_json::Value = serde_json::from_str(mlines[0]).unwrap();
    assert_eq!(header["file_count"], 1, "失敗対象を含まない");
    let fr: serde_json::Value = serde_json::from_str(mlines[1]).unwrap();
    assert_eq!(fr["name"], "ok.zip");
}

/// [covers:upload.cp-command-options]
/// backup pushのobject PUT時のPutObject呼び出しにstorage_class(設定値)と
/// checksum_algorithm SHA256が指定される。
#[test]
fn put_call_includes_required_options_via_push() {
    let Some(fx) = StorageFixture::new(
        "eb-cpopts",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"cp-options-plain");
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "backup pushはexit 0: {stdout}{stderr}");

    let key = fx.file_key("unfiled", None, "a.zip");
    let calls = fx.calls();
    let put_line = calls
        .lines()
        .find(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}")))
        .unwrap_or_else(|| panic!("対象keyへのPutObject: {calls}"));
    assert!(
        put_line.contains("storage_class=DEEP_ARCHIVE"),
        "storage_class(設定値): {put_line}"
    );
    assert!(
        put_line.contains("checksum=SHA256"),
        "checksum_algorithm SHA256: {put_line}"
    );
    assert!(
        put_line.contains("bucket=it-bucket"),
        "bucket参数: {put_line}"
    );
}

/// [covers:upload.keep-going-continues]
/// backup push --keep-goingでper-object失敗(PutObject失敗)でも継続し、exitは最初の
/// エラー区分(5)。成功分の記録(file_upload・manifest等の新経路event)がregistryにappendされる。
#[test]
fn keep_going_continues_after_put_failure() {
    let Some(fx) = StorageFixture::new(
        "eb-keepup",
        StorageOpts {
            keep_artifacts: "always",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let ng = fx.data_root.join("ng.zip");
    let ok = fx.data_root.join("ok.zip");
    write_file(&ng, b"keep-upload-ng");
    write_file(&ok, b"keep-upload-ok");
    let ng_key = fx.file_key("unfiled", None, "ng.zip");
    let (code, stdout, stderr) = fx.push(
        &[&ng, &ok],
        &["--keep-going"],
        &[("FAKE_S3_FAIL_CP_FOR", ng_key.as_str())],
    );
    assert_eq!(
        code, 5,
        "put失敗の最初のエラー区分は転送エラー5: {stdout}{stderr}"
    );

    // 成功分(ok.zip)の新経路event記録がappendされる
    let registry = fx.registry_values();
    let ok_uploads: Vec<&serde_json::Value> = registry
        .iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                && r.get("name").and_then(|n| n.as_str()) == Some("ok.zip")
        })
        .collect();
    assert_eq!(ok_uploads.len(), 1, "成功分のfile_upload記録: {registry:?}");
    assert!(
        registry
            .iter()
            .any(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("manifest")),
        "manifest記録(成功run扱い)"
    );
    assert!(
        registry
            .iter()
            .any(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("index_upload")),
        "index_upload記録"
    );
}

/// [covers:upload.cp-failure-transfer-error]
/// backup pushのPutObject失敗(SDK error相当・失敗注入FAKE_S3_FAIL_CP_FOR)は転送エラー5。
/// messageは操作名・bucket・keyを含みcredential値を含まない(TASK-11 Phase B: 旧upload
/// サブコマンド削除によりbackup push経路へ。旧src/uploader.rs単体testの引継ぎ)。
#[test]
fn put_failure_is_transfer_error_via_push() {
    let Some(fx) = StorageFixture::new("eb-cpfail", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"cp-fail-plain");
    let key = fx.file_key("unfiled", None, "a.zip");
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[("FAKE_S3_FAIL_CP_FOR", key.as_str())]);
    assert_eq!(code, 5, "PutObject失敗は転送エラー5: {stdout}{stderr}");
    assert!(stderr.contains("PutObject"), "操作名: {stderr}");
    assert!(stderr.contains("bucket=it-bucket"), "bucket: {stderr}");
    assert!(stderr.contains(&format!("key={key}")), "key: {stderr}");
    assert!(
        !stderr.contains("fake-seam-only-not-a-real-secret"),
        "credential値を含まない: {stderr}"
    );
}
