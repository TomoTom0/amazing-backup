//! registry/manifestを含むbatch単位の結合test(設計1章・8章・TASK-10設計2.2・7.3・8.1)。
//! TASK-11(Phase B)で旧encrypt/uploadサブコマンドが削除され、暗号化・registry記録・
//! manifest構築・S3転送はbackup push(暗号化→upload→manifest→索引更新の統合run)経路で
//! 検証する。実ageでの暗号化とfake seam(AMAZING_BACKUP_FAKE_S3_STATE)のbin subprocess実行。

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
/// 暗号化成功時にregistryへfile_write記録(TASK-20 3.2のschema=3: uuid/title/label入り・
/// folder廃止)が1行appendされる(backup pushでの暗号化成功・title指定)。
#[test]
fn registry_append_fields() {
    let Some(fx) = StorageFixture::new(
        "eb-reg",
        StorageOpts {
            keep_artifacts: "always",
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let src = fx.data_root.join("photo.zip");
    make_archive(&src, 4096);
    let Some((code, stdout)) = push_titled_pty(&fx, &[&src], "photo-2026", &[]) else {
        return;
    };
    assert_eq!(code, 0, "push(titleはprompt): {stdout}");

    let writes = file_writes(&fx);
    assert_eq!(writes.len(), 1, "file_write記録は1行: {writes:?}");
    let rec = &writes[0];
    assert_eq!(rec["schema"], 3, "schema=3(TASK-20): {rec}");
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
    let uuid = rec["uuid"].as_str().expect("uuid(TASK-18追加)");
    assert_eq!(uuid.len(), 36, "uuidはUUIDv7形式(36字): {uuid}");
    assert_eq!(rec["title"], "photo-2026", "title(TASK-18追加)");
    assert!(rec.get("folder").is_none(), "folderは廃止: {rec}");
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
    // layout分類解決の記録(category/label/identity_hash)とUUID体系のs3_key
    assert_eq!(rec["category"], "unfiled");
    assert_eq!(rec["label"], "");
    assert_eq!(
        rec["identity_hash"],
        serde_json::json!(hash8_of_path(&src)),
        "identity_hash=sha256(realpath)先頭8hex"
    );
    assert_eq!(
        rec["s3_key"],
        serde_json::json!(fx.data_key(uuid)),
        "s3_keyはdata/{uuid}/default.age記録値"
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
    let Some((code, stdout)) = push_titled_pty(&fx, &[&src], "reencrypt-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push(titleはprompt): {stdout}");
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

    // 記録が無ければ新規発行のためtitle必須(設計6.1: pty promptで与える)
    let Some((code, stdout)) = push_titled_pty(&fx, &[&src], "reencrypt-title-2", &[]) else {
        return;
    };
    assert_eq!(code, 0, "記録なしは再暗号化(新規発行)でexit 0: {stdout}");
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
/// mtimeのみ変化(内容不変)した対象は再暗号化(平文sha256入手)のうえsha照合へ進み、
/// 内容同一なら既存uuidへ合流する(TASK-18 4.3「mtimeのみ変化」):
/// default.ageへのuploadなし・版増加なしでfile_write新記録が追記されreduce後勝ちになる
/// (次回から所在skipが回復)。
#[test]
fn reencrypt_on_source_change() {
    let Some(fx) = StorageFixture::new(
        "eb-srcchg",
        StorageOpts {
            keep_artifacts: "always",
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let src = fx.data_root.join("a.zip");
    write_file(&src, b"v1-content");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&src], "src-change", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = writes_uuid_of(&fx, &src);
    let key = fx.data_key(&uuid);
    let puts_before = fx
        .calls()
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}")))
        .count();

    // mtimeのみ変化(内容不変: sleep後に同一内容を書き直す)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&src, b"v1-content");
    let (code, stdout, stderr) = fx.push(&[&src], &[], &[]);
    assert_eq!(code, 0, "mtime変化後のpush: {stdout}{stderr}");
    assert!(
        stdout.contains("merge"),
        "sha照合で合流(merge表示): {stdout}"
    );

    let writes = file_writes(&fx);
    assert_eq!(writes.len(), 2, "合流でもfile_writeは追記(append-only)");
    assert_eq!(
        writes
            .iter()
            .filter(|w| w["uuid"] == serde_json::json!(uuid))
            .count(),
        2,
        "両記録とも同一uuid: {writes:?}"
    );
    assert_eq!(
        fx.calls()
            .lines()
            .filter(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}")))
            .count(),
        puts_before,
        "default.ageへの再PUTなし(版増加なし): {}",
        fx.calls()
    );

    // reduce後は同一identity_hashの新記録(file_writes)が有効(次回から所在skip回復)
    let records: Vec<amazing_backup::registry::Record> = writes
        .iter()
        .map(|v| {
            let e: amazing_backup::registry::FileWriteRecord =
                serde_json::from_value(v.clone()).expect("FileWriteRecord");
            amazing_backup::registry::Record::FileWrite(e)
        })
        .collect();
    let reduced = amazing_backup::registry::reduce_records(&records);
    assert_eq!(reduced.file_writes.len(), 1, "同一identityは後勝ち");
}

/// registryのfile_write記録からsource_pathでuuidを引く(TASK-18)。
fn writes_uuid_of(fx: &StorageFixture, path: &std::path::Path) -> String {
    let canonical = std::fs::canonicalize(path).expect("realpath");
    fx.registry_values()
        .into_iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("source_path").and_then(|p| p.as_str())
                    == Some(canonical.to_string_lossy().as_ref())
        })
        .and_then(|r| r.get("uuid").and_then(|u| u.as_str()).map(String::from))
        .unwrap_or_else(|| panic!("file_write.uuid({})", canonical.display()))
}

/// [covers:encrypt.manifest-content-roundtrip]
/// runのmanifest(V3・TASK-20 3.3のfile record schema=3・batch header行はschema=2のまま)が
/// 暗号化生成され、復号するとバッチヘッダ+ファイルレコードが正しい:
/// file recordはuuid/title追加・identity_hash/folder削除で、
/// name/category/label/source_pathは所在記録として維持(index rebuildのlocations源)・
/// s3_key(data/{uuid}/default.age)・version_id・sha類・size類・date・version_ts・batch_idは維持。
#[test]
fn manifest_content_roundtrip() {
    let Some(fx) = StorageFixture::new(
        "eb-manifest",
        StorageOpts {
            keep_artifacts: "always",
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.elsewhere.join("b.7z");
    write_file(&a, b"manifest-content-A");
    write_file(&b, b"manifest-content-BB");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a, &b], "manifest-a\nmanifest-b", &[])
    else {
        return;
    };
    assert_eq!(code, 0, "push(titleはprompt): {stdout}");

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
    assert_eq!(
        header["schema"], 2,
        "batch header行のschema=2は据え置き(TASK-20): {header}"
    );
    assert_eq!(header["batch_id"], serde_json::json!(batch_ids[0]));
    assert_eq!(header["file_count"], 2);
    assert_eq!(header["total_source_size"], serde_json::json!(18 + 19));
    assert!(
        header["recipients"].as_array().unwrap()[0]
            .as_str()
            .unwrap()
            .starts_with("SHA256:")
    );

    // file record: registryのfile_upload記録と一致し、uuid/title・所在記録・実keyを含む
    let uploads = file_uploads(&fx);
    assert_eq!(uploads.len(), 2);
    for line in &lines[1..] {
        let fr: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(fr["record_type"], "file");
        assert_eq!(fr["schema"], 3, "file record schema=3(TASK-20): {fr}");
        assert!(
            fr.get("label").is_some(),
            "labelは所在記録として維持(TASK-20): {fr}"
        );
        let uuid = fr["uuid"].as_str().unwrap().to_string();
        let up = uploads
            .iter()
            .find(|u| u["uuid"] == serde_json::json!(uuid))
            .unwrap_or_else(|| panic!("registry file_upload記録(uuid={uuid}): {uploads:?}"));
        assert_eq!(fr["title"], up["title"], "titleはregistry記録と一致");
        // nameは所在記録としてmanifestに維持(file_uploadはschema2でname廃止のため
        // 比較正本はsource_pathのbasename・設計3.3)
        let basename = std::path::Path::new(fr["source_path"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            fr["name"],
            serde_json::json!(basename),
            "nameは所在記録として維持"
        );
        assert!(
            fr.get("source_path").is_some(),
            "source_pathは所在記録として維持: {fr}"
        );
        assert!(
            fr.get("identity_hash").is_none() && fr.get("folder").is_none(),
            "identity_hash/folderは削除(TASK-18): {fr}"
        );
        assert_eq!(
            fr["source_sha256"], up["source_sha256"],
            "registry記録と一致"
        );
        assert_eq!(fr["age_sha256"], up["age_sha256"], "registry記録と一致");
        assert_eq!(fr["s3_key"], up["s3_key"], "実s3_keyを記録");
        assert_eq!(
            fr["s3_key"],
            serde_json::json!(fx.data_key(&uuid)),
            "s3_keyはdata/{uuid}/default.age"
        );
        assert_eq!(fr["version_id"], up["version_id"], "version_idを記録");
        assert!(
            !fr["version_id"].as_str().unwrap_or("").is_empty(),
            "version_idは空でない(upload後に構築)"
        );
        for field in ["date", "version_ts", "batch_id", "source_size", "age_size"] {
            assert!(fr.get(field).is_some(), "file recordに{field}維持: {fr}");
        }
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

    // ngは位置引数(暗号化読込失敗=処理エラー4が最初のエラー)、okはfrom-listのtitle欄
    // 付き(新規発行のtitle必須・設計6.1)で継続処理される
    let (code, stdout, stderr) = fx.push_from_list(
        &[&ng_src],
        &[(&ok_src, Some("ok-title"))],
        &["--keep-going"],
    );
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
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "cp-opts-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "backup pushはexit 0: {stdout}");

    let key = fx.data_key(&uuid_of_path(&fx, &a));
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
    // ngをcrash残存(file_upload記録のみ除去)にしてPUT失敗注入の対象keyを確定
    // (再PUT経路はtitle不要・設計4.4)。okはfrom-listのtitle欄で新規発行
    let Some((code, stdout)) = push_titled_pty(&fx, &[&ng], "ng-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "ngの初回push: {stdout}");
    let ng_key = fx.data_key(&uuid_of_path(&fx, &ng));
    rewrite_registry_lines(&fx.registry, |r| {
        !(r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
            && r.get("s3_key").and_then(|k| k.as_str()) == Some(ng_key.as_str()))
    });
    // 注入付き実行: ngの再PUTが失敗し転送エラー5・okは継続(成功分の記録が残る)
    let (code, stdout, stderr) = fx.push_from_list_env(
        &[&ng],
        &[(&ok, Some("ok-title"))],
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
                && r.get("title").and_then(|n| n.as_str()) == Some("ok-title")
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
    // crash残存化(file_upload記録のみ除去)して注入対象keyを確定(再PUTはtitle不要)
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "cp-fail-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let key = fx.data_key(&uuid_of_path(&fx, &a));
    rewrite_registry_lines(&fx.registry, |r| {
        !(r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
            && r.get("s3_key").and_then(|k| k.as_str()) == Some(key.as_str()))
    });
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
