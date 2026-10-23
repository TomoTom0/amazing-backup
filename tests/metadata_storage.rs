//! metadata エリア(TASK-18・tmp/tasks/tm-18/design_key-uuid-versioning.md 2章/5.5)の結合test。
//! src/metadata.rsは実装前(新設module)のため、backup push run末尾のmetadata syncを
//! fake seam(AMAZING_BACKUP_FAKE_S3_STATE)のAPI呼び出し記録とmetadata objectの実体
//! (平文JSON)で観測する(テスト先行のため実行failは許容・cargo test --no-runは通る)。

mod common;

use common::*;

/// registryのfile_write記録からsource_pathでuuidを引く。
fn meta_uuid_of(fx: &StorageFixture, path: &std::path::Path) -> String {
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

/// TASK-18新体系のStorageOpts。
fn meta_opts() -> StorageOpts {
    StorageOpts {
        no_categories: true,
        aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
        ..StorageOpts::default()
    }
}

/// 同一entityへ2版目を重ねる(crash残存re-put経路)。
fn meta_add_version(fx: &StorageFixture, path: &std::path::Path) {
    let uuid = meta_uuid_of(fx, path);
    let key = fx.data_key(&uuid);
    rewrite_registry_lines(&fx.registry, |r| {
        !(r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
            && r.get("s3_key").and_then(|k| k.as_str()) == Some(key.as_str()))
    });
    let (code, stdout, stderr) = fx.push(&[path], &[], &[]);
    assert_eq!(code, 0, "2版目(re-put): {stdout}{stderr}");
}

/// metadata keyへのPutObject呼び出し数。
fn meta_puts(fx: &StorageFixture, meta_key: &str) -> usize {
    fx.calls()
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={meta_key}")))
        .count()
}

/// [covers:metadata.doc-built-from-registry]
/// MetadataDocはregistry reduceから正規形として導出される(TASK-18 2.1・2.3.1):
/// 現行title=uuid直近file_writeのtitle・現行版=bucket一致のuuid直近file_upload
/// (version_id/sha類/date/version_ts)・pins=未削除bucket一致pin_writeから
/// {pin,date,source_sha256}(並びはpin名辞書順=冪等)・recipients=現行版のbatch一致
/// file_write由来・latest_source_path=last_backup最新location。版歴全件・所在履歴全件・
/// 分類は載せない。serializeは同内容ならbyte同一(差分比較の前提)。
#[test]
fn metadata_doc_is_built_from_registry_reduce() {
    let Some(fx) = StorageFixture::new("meta-doc", meta_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"metadata-doc-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "doc-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = meta_uuid_of(&fx, &a);
    // 2版目(re-put)で現行版を切り替え
    meta_add_version(&fx, &a);
    // pinsを辞書順と逆(pm-b→pm-a)で付与
    assert_eq!(
        fx.pin("add", "doc-title", &["--latest", "--as", "pm-b"]).0,
        0
    );
    assert_eq!(
        fx.pin("add", "doc-title", &["--latest", "--as", "pm-a"]).0,
        0
    );

    let doc = fx
        .read_metadata_doc(&uuid, "doc-title")
        .expect("metadata object");
    assert_eq!(doc["schema"], 1, "schema=1: {doc}");
    assert_eq!(doc["record_type"], "metadata");
    assert_eq!(doc["uuid"], serde_json::json!(uuid));
    assert_eq!(doc["title"], "doc-title", "現行title(直近file_write)");
    // 現行版=uuid直近のfile_upload記録値
    let uploads: Vec<serde_json::Value> = fx
        .registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload"))
        .collect();
    let current = uploads.last().expect("直近file_upload");
    assert_eq!(doc["version_id"], current["version_id"], "現行版version_id");
    assert_eq!(doc["source_sha256"], current["source_sha256"], "平文sha");
    assert_eq!(doc["source_size"], current["source_size"], "平文size");
    assert_eq!(doc["age_sha256"], current["age_sha256"], "暗号文sha");
    assert_eq!(doc["age_size"], current["age_size"], "暗号文size");
    assert_eq!(doc["version_ts"], current["version_ts"], "暗号化時刻");
    assert!(
        doc["date"].as_str().is_some_and(|d| d.len() == 10),
        "dateはYYYY-MM-DD: {doc}"
    );
    // recipients(現行版のbatch一致file_write由来)
    let fw = fx
        .registry_values()
        .into_iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid.as_str())
        })
        .last()
        .expect("直近file_write");
    assert_eq!(doc["recipients"], fw["recipients"], "recipients導出");
    // pinsはpin名辞書順(記録順pm-b→pm-aと逆でも冪等)
    let pins = doc["pins"].as_array().expect("pins配列");
    let pin_names: Vec<&str> = pins.iter().filter_map(|p| p["pin"].as_str()).collect();
    assert_eq!(pin_names, vec!["pm-a", "pm-b"], "pin名辞書順: {pins:?}");
    for p in pins {
        for field in ["pin", "date", "source_sha256"] {
            assert!(p.get(field).is_some(), "pin記録に{field}: {p}");
        }
    }
    // latest_source_path=last_backup最新location
    assert_eq!(
        doc["latest_source_path"],
        serde_json::json!(a.canonicalize().unwrap().to_string_lossy().into_owned()),
        "latest_source_path: {doc}"
    );
    // 版歴全件・所在履歴全件・分類は載せない(承認事項1)
    assert!(
        !doc.get("versions").is_some() && !doc.get("locations").is_some(),
        "版歴・所在履歴は載せない: {doc}"
    );
    assert!(
        doc.get("category").is_none() && doc.get("rel_path").is_none(),
        "分類は載せない: {doc}"
    );

    // 同一registryからの2回目の導出はPUTなし(byte同一=差分比較で書かない)
    let meta_key = fx.metadata_key(&uuid, "doc-title");
    let puts = meta_puts(&fx, &meta_key);
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "all-skip再実行: {stdout}{stderr}");
    assert_eq!(
        meta_puts(&fx, &meta_key),
        puts,
        "同一内容の再導出は書き直さない: {}",
        fx.calls()
    );
}

/// [covers:metadata.put-only-if-changed]
/// metadata書込は変更時のみ(TASK-18 2.3・低3): 導出docをGETした現行metadata keyの内容
/// とbyte比較し、内容不一致またはGETが404(不在)のときのみPUT(STANDARD固定)。
/// 同一内容なら書かない。GETの404以外の失敗はエラーとして伝播する。
#[test]
fn metadata_put_only_if_changed() {
    // (1) 内容変化後(新版)はPUTされる・(2) 同一内容(再skip run)はPUTなし
    let Some(fx) = StorageFixture::new("meta-put", meta_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"metadata-put-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "put-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = meta_uuid_of(&fx, &a);
    let meta_key = fx.metadata_key(&uuid, "put-title");
    assert!(
        meta_puts(&fx, &meta_key) >= 1,
        "(1)新規作成でPUTされる: {}",
        fx.calls()
    );
    let calls = fx.calls();
    let put_line = calls
        .lines()
        .find(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={meta_key}")))
        .expect("metadata PUT行");
    assert!(
        put_line.contains("storage_class=STANDARD"),
        "metadataはSTANDARD固定: {put_line}"
    );
    let puts_after_first = meta_puts(&fx, &meta_key);
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "(2)同一内容の再実行: {stdout}{stderr}");
    assert!(
        meta_puts(&fx, &meta_key) == puts_after_first,
        "同一内容ならPUTなし: {}",
        fx.calls()
    );

    // (3) metadata object不在(404)時はPUTされる
    let _ = std::fs::remove_file(fx.object_path(&meta_key));
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "(3)object不在時の再実行: {stdout}{stderr}");
    assert!(
        meta_puts(&fx, &meta_key) > puts_after_first,
        "404(不在)時はPUTされる: {}",
        fx.calls()
    );

    // (4) GETの404以外の失敗(失敗注入)はエラーとして伝播(比較skipの冪等性維持)
    let (code, _stdout, stderr) =
        fx.push(&[&a], &[], &[("FAKE_S3_FAIL_CP_FOR", meta_key.as_str())]);
    assert_ne!(
        code, 0,
        "(4)GET失敗は比較skipによる黙示成功とならずエラー伝播: {stderr}"
    );
}

/// [covers:metadata.latest-source-path-modes]
/// latest_source_pathは[metadata].latest_source_path設定に従い3形態(TASK-18 2.2・承認事項11):
/// "none"=null・"plain"(既定)=path文字列そのまま・"encrypted"=object{format:"age",
/// recipients, sha256, data(base64)}。encryptedは実ageで暗号化され秘密鍵で復号できる。
#[test]
fn metadata_latest_source_path_modes() {
    let a_plain = b"metadata-modes-plain";
    for (tag, setting, section) in [
        (
            "none",
            "none",
            "[metadata]\nlatest_source_path = \"none\"\n",
        ),
        (
            "plain",
            "plain",
            "[metadata]\nlatest_source_path = \"plain\"\n",
        ),
        (
            "encrypted",
            "encrypted",
            "[metadata]\nlatest_source_path = \"encrypted\"\n",
        ),
    ] {
        let Some(fx) = StorageFixture::new(
            &format!("meta-mode-{tag}"),
            StorageOpts {
                metadata_section: section.to_string(),
                ..meta_opts()
            },
        ) else {
            return;
        };
        let a = fx.data_root.join("a.zip");
        write_file(&a, a_plain);
        let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "mode-title", &[]) else {
            return;
        };
        assert_eq!(code, 0, "push({tag}): {stdout}");
        let uuid = meta_uuid_of(&fx, &a);
        let doc = fx
            .read_metadata_doc(&uuid, "mode-title")
            .unwrap_or_else(|| panic!("metadata object({tag})"));
        let lsp = &doc["latest_source_path"];
        match setting {
            "none" => {
                assert!(lsp.is_null(), "noneはnull: {doc}");
            }
            "plain" => {
                assert_eq!(
                    lsp.as_str(),
                    Some(a.canonicalize().unwrap().to_string_lossy().as_ref()),
                    "plainはpath文字列: {doc}"
                );
            }
            _ => {
                // encrypted: object{format:"age", recipients, sha256, data}
                assert!(lsp.is_object(), "encryptedはobject: {doc}");
                assert_eq!(lsp["format"], "age", "format=age: {lsp}");
                let recipients = lsp["recipients"].as_array().expect("recipients");
                assert!(
                    recipients.len() == 1 && recipients[0].as_str().unwrap().starts_with("SHA256:"),
                    "鍵特定用の指紋列: {lsp}"
                );
                let data_b64 = lsp["data"].as_str().expect("data(base64)");
                let blob = base64_decode(data_b64);
                assert_eq!(
                    lsp["sha256"].as_str(),
                    Some(sha256_hex(&blob).as_str()),
                    "sha256は暗号文blobのsha256: {lsp}"
                );
                // dataを秘密鍵で復号すると元path文字列に一致
                let blob_path = fx.elsewhere.join(format!("lsp-{tag}.age"));
                write_file(&blob_path, &blob);
                let plain = age_decrypt(&fx.age, &blob_path, &fx.secret);
                assert_eq!(
                    String::from_utf8_lossy(&plain),
                    a.canonicalize().unwrap().to_string_lossy(),
                    "復号すると元path文字列: {lsp}"
                );
            }
        }
    }

    // 既定(セクション省略)はplain
    let Some(fx) = StorageFixture::new("meta-mode-default", meta_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, a_plain);
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "mode-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "push(既定): {stdout}");
    let uuid = meta_uuid_of(&fx, &a);
    let doc = fx
        .read_metadata_doc(&uuid, "mode-title")
        .expect("metadata doc");
    assert_eq!(
        doc["latest_source_path"].as_str(),
        Some(a.canonicalize().unwrap().to_string_lossy().as_ref()),
        "セクション省略時はplain(既定): {doc}"
    );
}

/// 検証用の最小base64 decoder(standard alphabet・padding考慮)。
fn base64_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for ch in s.chars() {
        if ch == '=' || ch == '\n' {
            continue;
        }
        let v = match ch {
            'A'..='Z' => ch as u32 - 'A' as u32,
            'a'..='z' => ch as u32 - 'a' as u32 + 26,
            '0'..='9' => ch as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => panic!("base64外の文字: {ch}"),
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

/// [covers:metadata.title-change-rotates-key]
/// title変更時のキーローテーション(TASK-18 2.3・5.5低2): 新キーmetadata_{新title}.jsonへ
/// PUT先行のうえ旧キーへDELETE(delete marker付与)。旧titleキーの特定はlist_keys
/// (bucket, "{prefix}data/{uuid}/")でmetadata_接頭辞キーを列挙し現行titleキー以外を
/// すべてDELETEする方式(冪等。複数回のtitle変更の残骸を掃除)。default.ageは不動。
#[test]
fn metadata_title_change_rotates_key() {
    let Some(fx) = StorageFixture::new("meta-rot", meta_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"metadata-rot-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "rot-A", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push(title A): {stdout}");
    let uuid = meta_uuid_of(&fx, &a);
    let key_a = fx.metadata_key(&uuid, "rot-A");
    let data_key = fx.data_key(&uuid);
    assert!(fx.object_path(&key_a).is_file(), "metadata_A.json配置");

    // title A→B(mtime touch合流経路でtitle変更)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&a, b"metadata-rot-plain");
    let seed = fx.data_root.join("seed.zip");
    write_file(&seed, b"seed-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&seed], "rot-seed", &[]) else {
        return;
    };
    assert_eq!(code, 0, "seed push: {stdout}");
    let (code, stdout, stderr) = fx.push_from_list(&[&seed], &[(&a, Some("rot-B"))], &[]);
    assert_eq!(code, 0, "title A→B: {stdout}{stderr}");
    let key_b = fx.metadata_key(&uuid, "rot-B");
    assert!(
        fx.object_path(&key_b).is_file(),
        "metadata_B.json配置(PUT先行)"
    );

    // title B→C
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&a, b"metadata-rot-plain");
    let (code, stdout, stderr) = fx.push_from_list(&[&seed], &[(&a, Some("rot-C"))], &[]);
    assert_eq!(code, 0, "title B→C: {stdout}{stderr}");
    let key_c = fx.metadata_key(&uuid, "rot-C");
    assert!(fx.object_path(&key_c).is_file(), "metadata_C.json配置");

    // 旧キー(metadata_A/B)へDELETE(delete marker)。list_keys呼び出しはsync対象uuidごと
    let calls = fx.calls();
    for old_key in [&key_a, &key_b] {
        assert!(
            calls
                .lines()
                .any(|l| l.starts_with("s3 DeleteObject") && l.contains(&format!("key={old_key}"))),
            "旧キー({old_key})へDELETE: {calls}"
        );
    }
    assert!(
        calls.lines().any(|l| l.starts_with("s3 ListObjectsV2")
            && l.contains(&format!("prefix=amazing-backup/data/{uuid}/"))),
        "旧titleキーの特定はlist_keys列挙: {calls}"
    );
    // default.age・pin objectへの変更は行われない
    assert!(
        !calls.lines().any(|l| {
            (l.starts_with("s3 DeleteObject") || l.starts_with("s3 CopyObject"))
                && l.contains(&format!("key={data_key}"))
        }),
        "default.ageは不動: {calls}"
    );
    assert!(
        !calls.lines().any(|l| l.starts_with("s3 DeleteObject")
            && l.contains(".age")
            && !l.contains("metadata_")),
        "pin objectへのDELETEは無い: {calls}"
    );
}
