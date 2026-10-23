//! pin エリアの結合test(TASK-10 S3ストレージ設計・tmp/tasks/tm-10/design_s3_storage.md 1.3・3.2)。
//! pin add/rm・backup push --pinをfake seam(AMAZING_BACKUP_FAKE_S3_STATE)のbin subprocessで
//! 検証する(テスト先行のため実装前の実行failは許容・cargo test --no-runは通る)。

mod common;

use common::*;

/// pin系test共通: 未復元(DEEP_ARCHIVE)版へのpin addが復元job→CopyObjectで写るまでの
/// danceを実行する。fakeのRestoreObjectは発行成功でrestored状態へ遷移するため、
/// 1実行でCopyObjectまで行う実装と進行中案内で再実行を要する実装の両方に対応する。
fn pin_add_until_copied(fx: &StorageFixture, name: &str, pin: &str) {
    let pin_arg = format!("--as={pin}");
    let uuid = uuid_of(fx, &fx.data_root.join(name));
    let pin_key = fx.data_pin_key(&uuid, pin);
    let mut last = String::new();
    for _ in 0..2 {
        let (code, stdout, stderr) = fx.pin("add", name, &["--latest", &pin_arg]);
        last = format!("{stdout}{stderr}");
        assert_eq!(code, 0, "pin add({name} -> {pin})はexit 0: {last}");
        if fx
            .calls()
            .lines()
            .any(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={pin_key}")))
        {
            return;
        }
    }
    panic!("pin add({name} -> {pin})のCopyObjectが行われない: {last}");
}

/// name=data_root直下のfile想定でpin keyを組み立てる。
fn pin_key_of(fx: &StorageFixture, name: &str, pin: &str) -> String {
    fx.pin_key("unfiled", None, name, pin)
}

/// [covers:pin.add-writes-pin-key-from-version]
/// pin add正常系(TASK-18 5.8・6.6): 探索語彙<title完全一致|uuid完全一致>でentityを解決し、
/// --latest/--versionで対象versionを解決、未復元時は復元jobを発行、復元済み版を
/// CopyObjectでpin key(data/{uuid}/{pin名}.age)へ写す。storage classはfile objectと同じ
/// (profileのstorage_class)。索引更新+registry記録(PinWriteRecord schema2・uuid入り)+
/// metadata sync(pins反映)。
#[test]
fn pin_add_copies_version_to_pin_key() {
    let Some(fx) = StorageFixture::new(
        "pin-add",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-add-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "pin-add-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push(titleはprompt): {stdout}");
    let uuid = uuid_of(&fx, &a);
    let file_key = fx.data_key(&uuid);
    clear_calls(&fx);

    // 未復元(DEEP_ARCHIVE)版へのpin add(探索はtitle): 復元job発行を経てpin keyへ写る
    pin_add_until_copied_data(&fx, "pin-add-title", "pm1", &uuid);
    let calls = fx.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={file_key}"))),
        "未復元版へのpin addは復元jobを発行する: {calls}"
    );
    let pm1_key = fx.data_pin_key(&uuid, "pm1");
    let copy_line = calls
        .lines()
        .find(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={pm1_key}")))
        .unwrap_or_else(|| panic!("pin key({pm1_key})へのCopyObject: {calls}"));
    assert!(
        copy_line.contains("storage_class=DEEP_ARCHIVE"),
        "pin objectのstorage_classはprofile値: {copy_line}"
    );
    // pin objectの実体はfile objectと同じbyte(版の写し)
    assert_eq!(
        std::fs::read(fx.object_path(&pm1_key)).expect("pin object実体"),
        std::fs::read(fx.object_path(&file_key)).expect("file object実体"),
        "pin objectは対象versionの写し"
    );

    // 復元済み版へのpin addは即時にCopyObject(新規の復元jobなし)
    let restores_before = fx
        .calls()
        .lines()
        .filter(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={file_key}")))
        .count();
    let (code, stdout, stderr) = fx.pin("add", "pin-add-title", &["--latest", "--as=pm2"]);
    assert_eq!(code, 0, "復元済み版へのpin addはexit 0: {stdout}{stderr}");
    let calls = fx.calls();
    let pm2_key = fx.data_pin_key(&uuid, "pm2");
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={pm2_key}"))),
        "復元済み版は即時にCopyObject: {calls}"
    );
    let restores_after = calls
        .lines()
        .filter(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={file_key}")))
        .count();
    assert_eq!(
        restores_after, restores_before,
        "復元済み版へのpin addは復元jobを再発行しない: {calls}"
    );

    // 索引更新(uuid照合で接続)+registry記録(schema2: uuid入り・name/category/rel_path/folder無し)
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "uuid", &uuid);
    let pins = entity_pins(entry);
    assert_eq!(pins.len(), 2, "索引pins[]へ2件反映: {pins:?}");
    let registry = fx.registry_values();
    assert_eq!(
        registry_count(&registry, "pin_write"),
        2,
        "registryのpin_write記録は2件"
    );
    for pw in registry
        .iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("pin_write"))
    {
        assert_eq!(
            pw.get("uuid").and_then(|u| u.as_str()),
            Some(uuid.as_str()),
            "pin_write記録はuuid入り(schema2): {pw}"
        );
        for absent in ["name", "category", "rel_path", "folder"] {
            assert!(
                pw.get(absent).is_none(),
                "pin_write記録に{absent}は廃止(schema2): {pw}"
            );
        }
    }
    // metadata sync(pins反映)
    let doc = fx
        .read_metadata_doc(&uuid, "pin-add-title")
        .expect("metadata doc");
    let mpins = doc
        .get("pins")
        .and_then(|p| p.as_array())
        .expect("metadata pins");
    assert_eq!(mpins.len(), 2, "metadataのpins反映: {doc}");
}

/// TASK-18版のpin add dance: 探索語彙<title|uuid>で解決しpin key(data/{uuid}/{pin}.age)
/// へのCopyObjectが行われるまで再実行(fakeのRestoreObjectは発行でrestoredへ遷移する
/// ため、1実行でCopyObjectまで行う実装と進行中案内で再実行を要する実装の両方に対応)。
fn pin_add_until_copied_data(fx: &StorageFixture, term: &str, pin: &str, uuid: &str) {
    let pin_arg = format!("--as={pin}");
    let pin_key = fx.data_pin_key(uuid, pin);
    let mut last = String::new();
    for _ in 0..2 {
        let (code, stdout, stderr) = fx.pin("add", term, &["--latest", &pin_arg]);
        last = format!("{stdout}{stderr}");
        assert_eq!(code, 0, "pin add({term} -> {pin})はexit 0: {last}");
        if fx
            .calls()
            .lines()
            .any(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={pin_key}")))
        {
            return;
        }
    }
    panic!("pin add({term} -> {pin})のCopyObjectが行われない: {last}");
}

/// registryのfile_write記録からsource_pathでuuidを引く(TASK-18)。
fn uuid_of(fx: &StorageFixture, path: &std::path::Path) -> String {
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

/// calls.logを空にする(以降の呼び出しのみを観測する)。
fn clear_calls(fx: &StorageFixture) {
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
}

/// [covers:pin.copy-oversize-uses-uploadpartcopy]
/// 後付けpinのCopyObjectは1回5GB上限(設計1.3・11章S3-3・8.1 s3.rs行): 5GB超の版は
/// CreateMultipartUpload→UploadPartCopy×N→CompleteMultipartUploadで分割写しとなり
/// 単発CopyObjectは使われない(backup時ピンは同制約を受けない)。
/// 5GB超objectの実体は用意せず、fake seamのsize上書き注入(FAKE_S3_OBJECT_SIZE_FOR)と
/// 索引のsize記録書換えでsize分岐のみを駆動する(実体は数byteのまま)。
#[test]
fn pin_add_oversize_version_uses_upload_part_copy() {
    let Some(fx) = StorageFixture::new("pin-oversize", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-oversize-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push(titleはprompt): {stdout}");
    let uuid = uuid_of(&fx, &a);

    // 5GB以内の版: 単発CopyObjectでpin keyへ写る
    assert_eq!(fx.pin("add", "a.zip", &["--latest", "--as", "small"]).0, 0);
    let small_key = fx.data_pin_key(&uuid, "small");
    let calls = fx.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={small_key}"))),
        "5GB以内の版は単発CopyObject: {calls}"
    );

    // 5GB超の版: 索引のsize記録とHeadObject応答sizeを上書きしてsize分岐を駆動
    let mut lines = fx.read_index();
    for l in lines.iter_mut() {
        if l.get("record_type").and_then(|r| r.as_str()) == Some("file")
            && l.get("title").and_then(|n| n.as_str()) == Some("a.zip")
        {
            if let Some(vs) = l.get_mut("versions").and_then(|v| v.as_array_mut()) {
                for v in vs.iter_mut() {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert(
                            "source_size".into(),
                            serde_json::Value::from(6_000_000_000u64),
                        );
                        obj.insert("age_size".into(), serde_json::Value::from(6_000_000_000u64));
                    }
                }
            }
        }
    }
    fx.rewrite_index(&lines);
    let file_key = fx.data_key(&uuid);
    let size_val = format!("{file_key}:6000000000");
    let size_env = [("FAKE_S3_OBJECT_SIZE_FOR", size_val.as_str())];
    let (code, stdout, stderr) =
        fx.pin_env("add", "a.zip", &["--latest", "--as", "oversize"], &size_env);
    assert_eq!(code, 0, "5GB超版へのpin addはexit 0: {stdout}{stderr}");

    let big_key = fx.data_pin_key(&uuid, "oversize");
    let calls = fx.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 CreateMultipartUpload")
                && l.contains(&format!("key={big_key}"))),
        "5GB超はCreateMultipartUploadで分割写し開始: {calls}"
    );
    let parts = calls
        .lines()
        .filter(|l| l.starts_with("s3 UploadPartCopy") && l.contains(&format!("key={big_key}")))
        .count();
    assert!(parts >= 1, "UploadPartCopyで分割写し: {calls}");
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 CompleteMultipartUpload")
                && l.contains(&format!("key={big_key}"))),
        "CompleteMultipartUploadで分割写し完了: {calls}"
    );
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={big_key}"))),
        "5GB超の版へ単発CopyObjectは使わない: {calls}"
    );
}

/// [covers:pin.write-once-violation-rejected]
/// write-once違反(設計1.3): 既存pin keyへのPUTはhead確認で検出して拒否(対象エラー3)。
/// 上書きしたい場合はpin rm後に再付与という案内を表示。既存pin objectは不変。
#[test]
fn pin_add_rejects_overwrite_of_existing_pin() {
    let Some(fx) = StorageFixture::new("pin-writeonce", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-write-once");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push(titleはprompt): {stdout}");
    let uuid = uuid_of(&fx, &a);
    assert_eq!(
        fx.pin("add", "a.zip", &["--latest", "--as", "pre-migration"])
            .0,
        0
    );
    let pm_key = fx.data_pin_key(&uuid, "pre-migration");
    let original = std::fs::read(fx.object_path(&pm_key)).expect("既存pin object");

    let (code, stdout, stderr) = fx.pin("add", "a.zip", &["--latest", "--as", "pre-migration"]);
    assert_eq!(
        code, 3,
        "既存pin名への再付与は対象エラー3: {stdout}{stderr}"
    );
    let out = format!("{stdout}{stderr}");
    assert!(out.contains("pin rm"), "pin rm後の再付与を案内する: {out}");
    assert_eq!(
        std::fs::read(fx.object_path(&pm_key)).expect("既存pin object(再読)"),
        original,
        "既存pin objectは不変"
    );
}

/// [covers:pin.backup-time-pin-upload]
/// [covers:cli.backup-push-pin-option]
/// backup push --pin <版名>(設計3.2・1.3): uploadと同時にpin keyへ書き込まれる
/// (復元job不要・CopyObject 5GB上限の制約を受けない直接upload。storage_classはprofile値)。
/// --pinの版名検証もこの経路で行われ、default予約・規則違反は拒否される。
#[test]
fn backup_push_pin_uploads_pin_key_simultaneously() {
    let Some(fx) = StorageFixture::new("pin-backup", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-backup-time");
    // 新規発行のtitle必須(設計6.1)のためpty promptで与える
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--pin", "pre-migration"])
    else {
        return;
    };
    assert_eq!(code, 0, "backup push --pinはexit 0: {stdout}");

    let uuid = uuid_of(&fx, &a);
    let calls = fx.calls();
    let file_key = fx.data_key(&uuid);
    let pm_key = fx.data_pin_key(&uuid, "pre-migration");
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={file_key}"))),
        "default.ageのPUT: {calls}"
    );
    let pin_put = calls
        .lines()
        .find(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={pm_key}")))
        .unwrap_or_else(|| panic!("pin keyの同時PUT: {calls}"));
    assert!(
        pin_put.contains("storage_class=STANDARD"),
        "pin objectのstorage_classはprofile値: {pin_put}"
    );
    assert!(
        !calls.lines().any(|l| l.starts_with("s3 CopyObject")),
        "backup時ピンはCopyObjectを使わない直接upload: {calls}"
    );
    assert_eq!(
        std::fs::read(fx.object_path(&pm_key)).expect("pin object実体"),
        std::fs::read(fx.object_path(&file_key)).expect("file object実体"),
        "pin objectの内容はdefault.ageと同一"
    );
    // 索引pins[]へ反映
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "title", "a.zip");
    let pins = entity_pins(entry);
    assert_eq!(pins.len(), 1, "索引pins[]へ反映: {pins:?}");
    assert_eq!(
        pins[0].get("pin").and_then(|v| v.as_str()),
        Some("pre-migration")
    );

    // --pin default(予約語)・規則違反は拒否される
    let b = fx.data_root.join("b.zip");
    write_file(&b, b"pin-backup-invalid");
    for bad in ["default", "Bad_Name"] {
        let (code, _, _) = fx.push(&[&b], &["--pin", bad], &[]);
        assert_eq!(code, 64, "--pin {bad}はusage error 64で拒否");
    }
}

/// [covers:pin.rm-updates-index]
/// pin rm(設計3.2): pin objectを削除し索引から該当pin entryを除去。
/// DEEP_ARCHIVE配置のpinも復元不要で削除できる。registryへpin_remove記録されexit 0。
#[test]
fn pin_rm_deletes_pin_object_without_restore() {
    let Some(fx) = StorageFixture::new(
        "pin-rm",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-rm-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push(titleはprompt): {stdout}");
    pin_add_until_copied(&fx, "a.zip", "pre-migration");
    let pm_key = fx.data_pin_key(&uuid_of(&fx, &a), "pre-migration");
    assert!(fx.object_path(&pm_key).is_file(), "pin object配置済み");
    clear_calls(&fx);

    let (code, stdout, stderr) = fx.pin("rm", "a.zip", &["--pin", "pre-migration"]);
    assert_eq!(code, 0, "pin rmはexit 0: {stdout}{stderr}");
    let calls = fx.calls();
    let deletes: Vec<&str> = calls
        .lines()
        .filter(|l| l.contains("s3 DeleteObject") && l.contains(&format!("key={pm_key}")))
        .collect();
    assert!(
        !deletes.is_empty(),
        "pin objectへの削除API呼び出し: {calls}"
    );
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={pm_key}"))),
        "DEEP_ARCHIVE配置のpin削除に復元jobは不要: {calls}"
    );
    // 索引から該当pin entryが除去される
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "title", "a.zip");
    let pins = entity_pins(entry);
    assert!(pins.is_empty(), "pin rm後のpinsは空: {pins:?}");
    assert_eq!(
        registry_count(&fx.registry_values(), "pin_remove"),
        1,
        "registryのpin_remove記録"
    );
}

/// [covers:pin.push-pin-on-skip-targets]
/// backup push --pinでskip・合流(merge)対象が含まれる場合(TASK-18 4.1の--pin指定時・
/// M-6の経路統合): 警告表示(WARNING: 変更なしfileの件数と最新版へのpin付与を通知)のうえ、
/// merged/skippedにも当該entityの最新版(bucket一致file_uploadの最終記録)から
/// ensure_restored+CopyObjectでpinを付与する。新規upload・再PUT分(手元.ageがある)は
/// pin keyへの直接PUT。
#[test]
fn backup_push_pin_covers_skip_targets_on_latest() {
    let Some(fx) = StorageFixture::new(
        "pin-skip",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-skip-a");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "pin-skip-a", &[]) else {
        return;
    };
    assert_eq!(code, 0, "a.zipの初回push: {stdout}");
    // mtime touchした同一内容(copy)で合流(merge)対象を作る
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&a, b"pin-skip-a");
    let b = fx.data_root.join("b.zip");
    write_file(&b, b"pin-skip-b");
    let c = fx.data_root.join("c.zip");
    write_file(&c, b"pin-skip-c");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&c], "pin-skip-c", &[]) else {
        return;
    };
    assert_eq!(code, 0, "c.zipの初回push(後でskip対象): {stdout}");
    let a_uuid = uuid_of(&fx, &a);
    let file_a_key = fx.data_key(&a_uuid);

    // a.zipはmtime touch(合流)・c.zipは変更なし(skip)・b.zipは新規:
    // --pin pm で警告+全対象へpin付与
    let list = write_from_list(
        fx.cfg.parent().expect("tmp dir"),
        &[(&a, Some("pin-skip-a")), (&b, Some("pin-skip-b"))],
    );
    let Some((code, stdout)) = push_titled_pty(
        &fx,
        &[&c],
        "",
        &["--pin", "pm", "--from-list", &list.display().to_string()],
    ) else {
        return;
    };
    assert_eq!(code, 0, "merge/skip対象を含むpush --pinはexit 0: {stdout}");
    assert!(
        stdout.contains("merge") && stdout.contains("skip"),
        "merge・skip表示: {stdout}"
    );
    assert!(
        stdout.contains("WARNING") && stdout.contains("pin"),
        "merge/skip対象への--pin警告表示: {stdout}"
    );

    let calls = fx.calls();
    // 合流対象(a)は最新版からCopyObjectでpin付与(新規.ageが無いため写し)
    let a_pm = fx.data_pin_key(&a_uuid, "pm");
    let copy_line = calls
        .lines()
        .find(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={a_pm}")))
        .unwrap_or_else(|| panic!("合流対象へのCopyObject pin付与: {calls}"));
    assert!(
        copy_line.contains("copy_source=") || copy_line.contains(file_a_key.as_str()),
        "写し元はdefault.age最新版: {copy_line}"
    );
    // 新規分(b)はpin keyへの直接PUT
    let b_uuid = uuid_of(&fx, &b);
    let b_pm = fx.data_pin_key(&b_uuid, "pm");
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={b_pm}"))),
        "新規分はpin keyへの直接PUT: {calls}"
    );
    // skip対象(c)も最新版からCopyObjectでpin付与される
    let c_uuid = uuid_of(&fx, &c);
    let c_pm = fx.data_pin_key(&c_uuid, "pm");
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={c_pm}"))),
        "skip対象も最新版からCopyObjectでpin付与: {calls}"
    );

    // 索引pins[]に反映される
    let lines = fx.read_index();
    for (tag, uuid) in [("a", &a_uuid), ("b", &b_uuid), ("c", &c_uuid)] {
        let entry = index_entity_by(&lines, "uuid", uuid);
        let pins = entity_pins(entry);
        assert!(
            pins.iter()
                .any(|p| p.get("pin").and_then(|v| v.as_str()) == Some("pm")),
            "{tag}の索引pins[]へpm反映: {pins:?}"
        );
    }
}

/// [covers:pin.push-pin-write-once-violation-skips-file]
/// backup push --pinのwrite-once違反(M-2): upload済みitemへのpin keyが既に存在する
/// 場合はrun全体を中断せず警告+当該fileのみpin付与をskipする(skip対象へのpin付与の
/// 既存挙動と統一)。run末尾のmanifest upload・索引再構築は必ず実行される
/// (filesがupload+registry記録済みなのに索引staleのまま中断しない)。
#[test]
fn push_pin_write_once_violation_skips_file_and_completes_run() {
    let Some(fx) = StorageFixture::new("pin-wowrite", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"write-once-v1");
    // 新規発行のtitle必須(設計6.1)のためpty promptで与える
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--pin", "pm1"]) else {
        return;
    };
    assert_eq!(code, 0, "初回push --pin: {stdout}");
    let key = fx.data_key(&uuid_of(&fx, &a));

    // 同一pin名での同一entity再push(crash残存re-put経路で2版化・設計4.4):
    // pin key既存=write-once違反でもrunは中断しない
    let ids = add_version_via_reput(&fx, &a, &["--pin", "pm1"]);
    let out = String::new();
    assert_eq!(ids.len(), 2, "2版のfile_upload記録(記録復元付き): {ids:?}");
    let calls = fx.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}"))),
        "file keyへのPUT: {calls}"
    );
    assert!(
        calls.lines().any(|l| l.contains("/sys/manifests/")),
        "manifest upload: {calls}"
    );
    assert!(
        calls.lines().any(|l| l.contains("sys/index.jsonl.age")),
        "索引の再構築+配置: {calls}"
    );
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert_eq!(vids.len(), 2, "索引versionsに新version反映: {vids:?}");
}

/// [covers:pin.rm-registry-failure-guides-rebuild]
/// pin rmのpin_remove記録append失敗時(L-1): error文に`manage index rebuild`による
/// 回復案内を含める(索引pins[]由来の解決のため再実行は失敗を繰返すため。
/// 終了コードは従来どおり処理エラー4)。
#[test]
fn pin_rm_registry_failure_guides_index_rebuild() {
    let Some(fx) = StorageFixture::new("pin-rmfail", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-rm-fail");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push(titleはprompt): {stdout}");
    pin_add_until_copied(&fx, "a.zip", "pm1");

    // registry.jsonlを追記不能(directory)へ差し替える
    std::fs::remove_file(&fx.registry).expect("registry.jsonlの削除");
    std::fs::create_dir(&fx.registry).expect("registry.jsonlをdirectory化");

    let (code, _, stderr) = fx.pin("rm", "a.zip", &["--pin", "pm1"]);
    assert_eq!(
        code, 4,
        "registry記録失敗は処理エラー4(終了コードは従来どおり): {stderr}"
    );
    assert!(
        stderr.contains("manage index rebuild"),
        "manage index rebuildによる回復案内: {stderr}"
    );
}

/// registry.jsonlへpin_remove記録を直接追記する(他方bucket・正bucketの
/// s3_bucketを持つ記録の混入模倣。M-R1検証用)。
fn append_pin_remove(fx: &StorageFixture, uuid: &str, pin: &str, pin_key: &str, bucket: &str) {
    use std::io::Write as _;
    // TASK-18 schema=2のpin_remove記録(uuid入り・name廃止・設計3.2)
    let rec = serde_json::json!({
        "schema": 2,
        "record_type": "pin_remove",
        "recorded_at": "2026-10-15T12:00:00+09:00",
        "uuid": uuid,
        "pin": pin,
        "pin_s3_key": pin_key,
        "s3_bucket": bucket,
    });
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&fx.registry)
        .expect("registry.jsonlへ追記");
    writeln!(f, "{rec}").expect("pin_remove記録の追記");
}

/// 索引entity entryのpins[]のpin名一覧(記録順)。
fn pin_names_of(fx: &StorageFixture, name: &str) -> Vec<String> {
    index_entity_by(&fx.read_index(), "title", name)
        .get("pins")
        .and_then(|p| p.as_array())
        .map(|a| {
            a.iter()
                .map(|p| {
                    p.get("pin")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// [covers:index.pins-bucket-isolated]
/// pin_remove記録の削除済み判定はs3_bucket照合を伴う(M-R1): 他方bucketのs3_bucketを
/// 持つpin_removeではpinは索引から消えず、正bucket(実効bucketと一致)のpin_removeでのみ
/// 消える。manage index rebuildのpins復元も同一規則で機能する。
#[test]
fn pin_remove_matched_by_recorded_bucket() {
    let Some(fx) = StorageFixture::new("pin-rmbkt", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-remove-bucket");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push(titleはprompt): {stdout}");
    pin_add_until_copied(&fx, "a.zip", "pm1");
    pin_add_until_copied(&fx, "a.zip", "pm2");
    let uuid = uuid_of(&fx, &a);
    let key1 = fx.data_pin_key(&uuid, "pm1");
    let key2 = fx.data_pin_key(&uuid, "pm2");

    // 他方bucketのs3_bucketを持つpin_removeではpm1は消えない(all-skip pushで索引再構築)
    append_pin_remove(&fx, &uuid, "pm1", &key1, "other-bucket");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    assert_eq!(
        pin_names_of(&fx, "a.zip"),
        vec!["pm1", "pm2"],
        "他方bucketのpin_removeではpinは索引に残る"
    );

    // 正bucket(実効bucket=it-bucketと一致)のpin_removeでpm2が消える
    append_pin_remove(&fx, &uuid, "pm2", &key2, "it-bucket");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    assert_eq!(
        pin_names_of(&fx, "a.zip"),
        vec!["pm1"],
        "正bucketのpin_removeで除去される"
    );

    // manage index rebuildのpins復元も同一規則(索引object喪失からの再構築)
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    let (code, stdout, stderr) = fx.manage("index", &["rebuild"]);
    assert_eq!(code, 0, "manage index rebuildはexit 0: {stdout}{stderr}");
    assert_eq!(
        pin_names_of(&fx, "a.zip"),
        vec!["pm1"],
        "rebuildのpins復元もbucket一致規則(pm2の正bucket削除のみ反映)"
    );
}
