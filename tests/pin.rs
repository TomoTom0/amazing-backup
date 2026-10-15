//! pin エリアの結合test(TASK-10 S3ストレージ設計・tmp/tasks/tm-10/design_s3_storage.md 1.3・3.2)。
//! pin add/rm・backup push --pinをfake seam(BACKUP_REMOTE_FAKE_S3_STATE)のbin subprocessで
//! 検証する(テスト先行のため実装前の実行failは許容・cargo test --no-runは通る)。

mod common;

use common::*;

/// pin系test共通: 未復元(DEEP_ARCHIVE)版へのpin addが復元job→CopyObjectで写るまでの
/// danceを実行する。fakeのRestoreObjectは発行成功でrestored状態へ遷移するため、
/// 1実行でCopyObjectまで行う実装と進行中案内で再実行を要する実装の両方に対応する。
fn pin_add_until_copied(fx: &StorageFixture, name: &str, pin: &str) {
    let pin_arg = format!("--as={pin}");
    let mut last = String::new();
    for _ in 0..2 {
        let (code, stdout, stderr) = fx.pin("add", name, &["--latest", &pin_arg]);
        last = format!("{stdout}{stderr}");
        assert_eq!(code, 0, "pin add({name} -> {pin})はexit 0: {last}");
        let pin_key = pin_key_of(fx, name, pin);
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
/// pin add正常系(設計3.2・1.3): --latest/--versionで対象versionを解決し、未復元時は
/// 復元jobを発行、復元済み版をCopyObjectでpin key(<prefix>/.../<name>/<pin名>.age)へ写す。
/// storage classはfile objectと同じ(profileのstorage_class)。索引更新+registry記録(pin_write)。
#[test]
fn pin_add_copies_version_to_pin_key() {
    let Some(fx) = StorageFixture::new(
        "pin-add",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-add-plain");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let file_key = fx.file_key("unfiled", None, "a.zip");
    clear_calls(&fx);

    // 未復元(DEEP_ARCHIVE)版へのpin add: 復元job発行を経てpin keyへ写る
    pin_add_until_copied(&fx, "a.zip", "pm1");
    let calls = fx.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={file_key}"))),
        "未復元版へのpin addは復元jobを発行する: {calls}"
    );
    let pm1_key = pin_key_of(&fx, "a.zip", "pm1");
    let copy_line = calls
        .lines()
        .find(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={pm1_key}")))
        .unwrap_or_else(|| panic!("pin keyへのCopyObject: {calls}"));
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
    let (code, stdout, stderr) = fx.pin("add", "a.zip", &["--latest", "--as=pm2"]);
    assert_eq!(code, 0, "復元済み版へのpin addはexit 0: {stdout}{stderr}");
    let calls = fx.calls();
    let pm2_key = pin_key_of(&fx, "a.zip", "pm2");
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

    // 索引更新+registry記録
    let lines = fx.read_index();
    let entry = index_file_entry(&lines, "a.zip");
    let pins = entry
        .get("pins")
        .and_then(|p| p.as_array())
        .expect("pins配列");
    assert_eq!(pins.len(), 2, "索引pins[]へ2件反映: {pins:?}");
    assert_eq!(
        registry_count(&fx.registry_values(), "pin_write"),
        2,
        "registryのpin_write記録は2件"
    );
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);

    // 5GB以内の版: 単発CopyObjectでpin keyへ写る
    assert_eq!(fx.pin("add", "a.zip", &["--latest", "--as", "small"]).0, 0);
    let small_key = pin_key_of(&fx, "a.zip", "small");
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
            && l.get("name").and_then(|n| n.as_str()) == Some("a.zip")
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
    let file_key = fx.file_key("unfiled", None, "a.zip");
    let size_val = format!("{file_key}:6000000000");
    let size_env = [("FAKE_S3_OBJECT_SIZE_FOR", size_val.as_str())];
    let (code, stdout, stderr) =
        fx.pin_env("add", "a.zip", &["--latest", "--as", "oversize"], &size_env);
    assert_eq!(code, 0, "5GB超版へのpin addはexit 0: {stdout}{stderr}");

    let big_key = pin_key_of(&fx, "a.zip", "oversize");
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    assert_eq!(
        fx.pin("add", "a.zip", &["--latest", "--as", "pre-migration"])
            .0,
        0
    );
    let pm_key = pin_key_of(&fx, "a.zip", "pre-migration");
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
    let (code, stdout, stderr) = fx.push(&[&a], &["--pin", "pre-migration"], &[]);
    assert_eq!(code, 0, "backup push --pinはexit 0: {stdout}{stderr}");

    let calls = fx.calls();
    let file_key = fx.file_key("unfiled", None, "a.zip");
    let pm_key = pin_key_of(&fx, "a.zip", "pre-migration");
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
    let entry = index_file_entry(&lines, "a.zip");
    let pins = entry
        .get("pins")
        .and_then(|p| p.as_array())
        .expect("pins配列");
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    pin_add_until_copied(&fx, "a.zip", "pre-migration");
    let pm_key = pin_key_of(&fx, "a.zip", "pre-migration");
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
    let entry = index_file_entry(&lines, "a.zip");
    let pins = entry
        .get("pins")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(pins.is_empty(), "pin rm後のpinsは空: {pins:?}");
    assert_eq!(
        registry_count(&fx.registry_values(), "pin_remove"),
        1,
        "registryのpin_remove記録"
    );
}

/// [covers:pin.push-pin-on-skip-targets]
/// backup push --pinでskip対象(変更なし)のfileが含まれる場合(M-6・12章レビュー):
/// 警告表示(WARNING: 変更なしfileの件数と最新版へのpin付与を通知)のうえ、当該fileの
/// 最新版(default.age)からCopyObjectでpin keyへ付与する(skip対象がpinなしにならないのを
/// 防ぐ)。新規upload分はpin keyへの直接PUT。
#[test]
fn backup_push_pin_covers_skip_targets_on_latest() {
    let Some(fx) = StorageFixture::new("pin-skip", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-skip-a");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let b = fx.data_root.join("b.zip");
    write_file(&b, b"pin-skip-b");
    let file_a_key = fx.file_key("unfiled", None, "a.zip");

    // a.zipは変更なし(skip)・b.zipは新規: --pin pm で警告+双方へpin付与
    let (code, stdout, stderr) = fx.push(&[&a, &b], &["--pin", "pm"], &[]);
    assert_eq!(
        code, 0,
        "skip対象を含むpush --pinはexit 0: {stdout}{stderr}"
    );
    assert!(stdout.contains("skip"), "skip表示: {stdout}");
    assert!(
        stdout.contains("WARNING") && stdout.contains("pin"),
        "skip対象への--pin警告表示: {stdout}"
    );

    // skip対象(a.zip)は最新版からCopyObjectでpin付与(新規.ageが無いため写し)
    let calls = fx.calls();
    let a_pm = pin_key_of(&fx, "a.zip", "pm");
    let copy_line = calls
        .lines()
        .find(|l| l.starts_with("s3 CopyObject") && l.contains(&format!("key={a_pm}")))
        .unwrap_or_else(|| panic!("skip対象へのCopyObject pin付与: {calls}"));
    assert!(
        copy_line.contains(&format!("key={file_a_key}"))
            || copy_line.contains("copy_source=")
            || copy_line.contains(file_a_key.as_str()),
        "写し元はdefault.age最新版: {copy_line}"
    );
    // 新規分(b.zip)はpin keyへの直接PUT
    let b_pm = pin_key_of(&fx, "b.zip", "pm");
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={b_pm}"))),
        "新規分はpin keyへの直接PUT: {calls}"
    );

    // 索引pins[]に双方反映される
    let lines = fx.read_index();
    for name in ["a.zip", "b.zip"] {
        let entry = index_file_entry(&lines, name);
        let pins = entry
            .get("pins")
            .and_then(|p| p.as_array())
            .unwrap_or_else(|| panic!("{name}のpins配列: {entry}"));
        assert!(
            pins.iter()
                .any(|p| p.get("pin").and_then(|v| v.as_str()) == Some("pm")),
            "{name}の索引pins[]へpm反映: {pins:?}"
        );
    }
}
