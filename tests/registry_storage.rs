//! registry エリア(TASK-18・tmp/tasks/tm-18/design_key-uuid-versioning.md 3.2/5.3)の結合test。
//! src/registry.rsは実装前のため、bin subprocess(backup push/list files)+fake seam+
//! registry.jsonlへの直接編集(crash残存・sweep/location_removeの模擬)で観測する
//! (テスト先行のため実装前の実行failは許容・cargo test --no-runは通る)。

mod common;

use common::*;

/// registryのfile_write記録からsource_pathでuuidを引く。
fn reg_uuid_of(fx: &StorageFixture, path: &std::path::Path) -> String {
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
fn reg_opts() -> StorageOpts {
    StorageOpts {
        no_categories: true,
        aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
        ..StorageOpts::default()
    }
}

/// [covers:registry.schema2-records-roundtrip]
/// recordのappend→load往復(TASK-20 3.2: file_writeのみschema=3・label入りへ改訂。
/// 他recordのschema=2は維持): file_write(schema=3・uuid/title/label入り・identity_hash維持)・
/// file_upload(schema=2・uuid/title入り)・pin_write(schema=2・uuid入り)・
/// pin_remove(schema=2・uuid入り)・prune(schema=2・deleted[]維持)の全fieldが
/// serialize→parseで失われることなく復元される(s3_keyはdata/{uuid}/default.age記録値・
/// Record enumへ正しく分類)。
#[test]
fn schema2_records_survive_append_load_roundtrip() {
    let Some(fx) = StorageFixture::new(
        "reg-round",
        StorageOpts {
            keep_artifacts: "always",
            aws_lifecycle: aws_lifecycle_toml(180, 1, 7),
            ..reg_opts()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"schema2-roundtrip-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "round-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = reg_uuid_of(&fx, &a);
    let key = fx.data_key(&uuid);
    // 3版(各版のfile_upload記録を保持するcrash残存re-put経路・設計4.4)。
    // keep_versions=1のpruneで削除対象が生じる構成(境界=keep番目に新しいnoncurrent版は
    // 残るため2版では削除0件となる・設計5.12)→ prune --applyでprune記録を作る
    let ids = add_version_via_reput(&fx, &a, &[]);
    assert_eq!(ids.len(), 2, "2版目追加: {ids:?}");
    let ids = add_version_via_reput(&fx, &a, &[]);
    assert_eq!(ids.len(), 3, "3版目追加: {ids:?}");
    assert_eq!(
        fx.pin("add", "round-title", &["--latest", "--as", "pm1"]).0,
        0,
        "pin add"
    );
    assert_eq!(
        fx.pin("rm", "round-title", &["--pin", "pm1"]).0,
        0,
        "pin rm"
    );
    assert_eq!(fx.manage("prune", &["--apply"]).0, 0, "prune --apply");

    // 各record_typeのschema field検査(file_writeはschema=3・他はschema=2)
    let registry = fx.registry_values();
    let fw = registry
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .expect("file_write記録");
    assert_eq!(fw["schema"], 3, "file_write schema=3(TASK-20): {fw}");
    assert_eq!(fw["uuid"], serde_json::json!(uuid), "uuid追加: {fw}");
    assert_eq!(fw["title"], "round-title", "title追加: {fw}");
    assert!(
        fw.get("label").is_some(),
        "file_writeはlabel入り(TASK-20): {fw}"
    );
    assert!(fw.get("folder").is_none(), "folder廃止: {fw}");
    assert!(
        fw.get("identity_hash").is_some() && fw.get("s3_key").is_some(),
        "identity_hash/s3_keyは維持: {fw}"
    );

    let fu = registry
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload"))
        .expect("file_upload記録");
    assert_eq!(fu["schema"], 2, "file_upload schema=2: {fu}");
    assert_eq!(fu["uuid"], serde_json::json!(uuid), "uuid追加: {fu}");
    assert!(fu.get("title").is_some(), "title追加: {fu}");
    for absent in ["name", "category", "label", "identity_hash"] {
        assert!(
            fu.get(absent).is_none(),
            "file_uploadの{absent}は削除(uuid接続へ一本化): {fu}"
        );
    }
    assert!(
        fu.get("version_id").is_some() && fu.get("s3_key").is_some(),
        "version_id/s3_key等は不変: {fu}"
    );

    let pw = registry
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("pin_write"))
        .expect("pin_write記録");
    assert_eq!(pw["schema"], 2, "pin_write schema=2: {pw}");
    assert_eq!(pw["uuid"], serde_json::json!(uuid), "uuid追加: {pw}");
    for absent in ["name", "category", "label", "folder"] {
        assert!(
            pw.get(absent).is_none(),
            "pin_writeの{absent}は削除(uuid照合のみへ単純化): {pw}"
        );
    }

    let pr = registry
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("pin_remove"))
        .expect("pin_remove記録");
    assert_eq!(pr["schema"], 2, "pin_remove schema=2: {pr}");
    assert_eq!(pr["uuid"], serde_json::json!(uuid), "uuid追加: {pr}");
    assert!(pr.get("name").is_none(), "pin_removeのnameは削除: {pr}");

    let pn = registry
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("prune"))
        .expect("prune記録");
    assert_eq!(pn["schema"], 2, "prune schema=2: {pn}");
    assert!(pn.get("category").is_none(), "pruneのcategoryは削除: {pn}");
    assert!(
        pn.get("keep").is_none(),
        "pruneのkeepは削除(aws.toml値): {pn}"
    );
    assert!(
        pn.get("deleted")
            .and_then(|d| d.as_array())
            .is_some_and(|d| !d.is_empty()),
        "deleted[]は維持: {pn}"
    );

    // 往復: 次コマンド(list files)が全recordを読み込んで索引を表示できる
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "全record群のload往復: {stdout}{stderr}");
    assert!(stdout.contains("round-title"), "entity表示: {stdout}");
}

/// [covers:registry.old-schema-fields-rejected]
/// 旧schema形式のrecord行はdeny_unknown_fieldsによりparse失敗となる(旧記録の互換読込・
/// 黙黙変換は行わない。行番号付きparseエラー)。
/// - 旧schema1形式(folder入り・uuid/title無し・schema=1)
/// - 旧schema2形式のfile_write行(rel_path入り・label無し・schema=2。TASK-20の
///   schema 3化前の記録)→ unknown field rel_pathとして同様に拒否
///   (拒否原因はunknown fieldであり「missing field label」文言はtest契約にしない=
///   文言でなく終了code・行番号付きエラーの挙動で検証)
#[test]
fn old_schema_record_line_is_rejected() {
    for (i, old_schema) in [1, 2].into_iter().enumerate() {
        let Some(fx) = StorageFixture::new(&format!("reg-old-{i}"), reg_opts()) else {
            return;
        };
        let a = fx.data_root.join("a.zip");
        write_file(&a, b"old-schema-plain");
        let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "old-title", &[]) else {
            return;
        };
        assert_eq!(code, 0, "初回push: {stdout}");

        // 現行のfile_write記録を旧schema形式の行へ組み替える
        let mut old = fx
            .registry_values()
            .into_iter()
            .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
            .expect("file_write記録")
            .clone();
        if let Some(obj) = old.as_object_mut() {
            match old_schema {
                1 => {
                    obj.insert("schema".into(), serde_json::Value::from(1));
                    obj.insert("folder".into(), serde_json::Value::from("a.zip"));
                    obj.remove("uuid");
                    obj.remove("title");
                }
                _ => {
                    obj.insert("schema".into(), serde_json::Value::from(2));
                    obj.insert("rel_path".into(), serde_json::Value::from("2020/old-place"));
                    obj.remove("label");
                }
            }
        }
        append_registry_line(&fx.registry, &old);

        // 索引objectを削除してregistryを読む照会経路(索引不在→namespace_never_written)へ
        // 誘導する(索引が存在する場合のlistは索引ベース表示でregistryを読まない)
        std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");

        // 読込は行番号付きparseエラー(ProcessError)となり、黙って読み飛ばされない
        let (code, _stdout, stderr) = fx.list("files", &[]);
        assert_eq!(
            code, 4,
            "旧schema={old_schema}行はparseエラー(処理エラー4): {stderr}"
        );
        // 混入行の行番号(内容一致で特定)までエラー文に現れることを検査する
        // (「registryを含む」だけの緩い検査では混入行の位置を検証できない)
        let old_line = serde_json::to_string(&old).expect("追加行serialize");
        let lineno = std::fs::read_to_string(&fx.registry)
            .expect("registry読込")
            .lines()
            .position(|l| l == old_line)
            .expect("混入行の発見")
            + 1;
        assert!(
            stderr.contains(&format!("registryの{lineno}行目")),
            "混入行の行番号付きエラー: {stderr}"
        );
    }
}

/// [covers:registry.sweep-location-remove-records]
/// 新record_typeのparse/reduce(TASK-18 3.2): sweep={schema:1, record_type:"sweep",
/// recorded_at, reason:"lifecycle", deleted:[{s3_key, version_id}]}・
/// location_remove={schema:1, record_type:"location_remove", recorded_at, uuid,
/// identity_hash, source_path}。両者とも既知種として読み込まれ(未知record_type警告なし)、
/// reduce結果(Reducedのsweeps/location_removes)が索引組立へ反映される。
#[test]
fn sweep_and_location_remove_records_parse_and_reduce() {
    let Some(fx) = StorageFixture::new("reg-swloc", reg_opts()) else {
        return;
    };
    let p1 = fx.data_root.join("one").join("a.zip");
    std::fs::create_dir_all(&p1.parent().expect("parent")).expect("one dir");
    write_file(&p1, b"sweep-loc-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&p1], "swloc-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = reg_uuid_of(&fx, &p1);
    let key = fx.data_key(&uuid);
    // 2版目(crash残存re-put経路・各版のfile_upload記録を保持)→ sweep対象の版idを得る
    let ids = add_version_via_reput(&fx, &p1, &[]);
    assert_eq!(ids.len(), 2, "2版: {ids:?}");
    // 同一内容を別pathへ(合流・2所在)
    let p2 = fx.data_root.join("two").join("a.zip");
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("two dir");
    write_file(&p2, b"sweep-loc-plain");
    assert_eq!(fx.push(&[&p2], &[], &[]).0, 0, "合流push(所在2件)");

    // sweep(deleted 2件)・location_remove(p1側)を追記
    let sweep = serde_json::json!({
        "schema": 1,
        "record_type": "sweep",
        "recorded_at": "2999-01-01T00:00:00+09:00",
        "reason": "lifecycle",
        "deleted": [
            {"s3_key": key, "version_id": ids[0]},
            {"s3_key": fx.data_pin_key(&uuid, "pm1"), "version_id": "pin-vid-x"},
        ],
    });
    let loc_rm = serde_json::json!({
        "schema": 1,
        "record_type": "location_remove",
        "recorded_at": "2999-01-01T00:00:00+09:00",
        "uuid": uuid,
        "identity_hash": hash8_of_path(&p1),
        "source_path": p1.canonicalize().unwrap().to_string_lossy(),
    });
    append_registry_line(&fx.registry, &sweep);
    append_registry_line(&fx.registry, &loc_rm);

    // all-skip push(p2)でregistry読込+reduce+索引再構築:
    // 未知record_type警告なし・sweep版の排除・location_removeの所在除外が反映される
    let (code, stdout, stderr) = fx.push(&[&p2], &[], &[]);
    assert_eq!(code, 0, "all-skip再構築pushはexit 0: {stdout}{stderr}");
    let combined = format!("{stdout}{stderr}");
    assert!(
        !combined.contains("未知のrecord_type"),
        "sweep/location_removeは既知種として読み込まれる: {combined}"
    );
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "uuid", &uuid);
    let vids = index_version_ids(entry);
    assert!(
        !vids.contains(&ids[0]),
        "sweep記録の版はreduce結果から除外(reviveしない): {vids:?}"
    );
    let locations = entity_locations(entry);
    assert_eq!(
        locations.len(),
        1,
        "location_removeの所在は除外: {locations:?}"
    );
    assert!(
        locations
            .iter()
            .all(|l| l.get("source_path").and_then(|p| p.as_str())
                != Some(p1.canonicalize().unwrap().to_string_lossy().as_ref())),
        "除外されるのはp1側: {locations:?}"
    );
}

/// [covers:registry.sha-uuid-lookup-resolves-unique]
/// sha_uuid_lookup(合流正本・承認事項4): file_uploadsからbucket一致する同sha記録の
/// uuid集合を作り、1件ならそのuuidを返す。合流先uuidは同一になる(uploadなし・locations追加)。
#[test]
fn sha_uuid_lookup_resolves_unique_uuid() {
    let Some(fx) = StorageFixture::new("reg-lookup1", reg_opts()) else {
        return;
    };
    let p1 = fx.data_root.join("one").join("a.zip");
    std::fs::create_dir_all(&p1.parent().expect("parent")).expect("one dir");
    write_file(&p1, b"lookup-unique-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&p1], "lookup-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = reg_uuid_of(&fx, &p1);
    let key = fx.data_key(&uuid);
    let puts_before = fx
        .calls()
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}")))
        .count();

    // 移動+内容不変 → lookupが当該uuidを返し合流する
    let p2 = fx.data_root.join("two").join("a.zip");
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("two dir");
    std::fs::rename(&p1, &p2).expect("move");
    let (code, stdout, stderr) = fx.push(&[&p2], &[], &[]);
    assert_eq!(code, 0, "移動後push: {stdout}{stderr}");
    assert_eq!(
        reg_uuid_of(&fx, &p2),
        uuid,
        "lookupの一意解決: 合流先は既存uuid(新規発行しない)"
    );
    assert!(
        fx.calls()
            .lines()
            .filter(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}")))
            .count()
            == puts_before,
        "合流のためuploadなし: {}",
        fx.calls()
    );
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "uuid", &uuid);
    assert_eq!(
        entity_locations(entry).len(),
        2,
        "合流でlocationsが2件(旧・新所在)"
    );
}

/// [covers:registry.sha-uuid-lookup-multiple-picks-min-uuid]
/// 同一shaがbucket一致する複数uuidに存在する(歴史的重複)場合は辞書順最小uuidを返す
/// (合流先を決定的にする)。registryへ直接2つ目のuuid記録を構築して観測する。
#[test]
fn sha_uuid_lookup_picks_lexicographically_min_uuid() {
    let Some(fx) = StorageFixture::new("reg-lookup2", reg_opts()) else {
        return;
    };
    let p1 = fx.data_root.join("one").join("a.zip");
    std::fs::create_dir_all(&p1.parent().expect("parent")).expect("one dir");
    write_file(&p1, b"lookup-min-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&p1], "lookup-min-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid1 = reg_uuid_of(&fx, &p1);
    // 辞書順でuuid1より小さい別uuidのfile_write/file_upload(同一sha・別所在)を直接構築
    let mut uuid2 = uuid1.clone();
    uuid2.replace_range(0..8, "00000000");
    assert!(
        uuid2 < uuid1,
        "uuid2は辞書順最小 (uuid1={uuid1} uuid2={uuid2})"
    );
    let registry = fx.registry_values();
    let fw1 = registry
        .iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid1.as_str())
        })
        .expect("uuid1のfile_write")
        .clone();
    let fu1 = registry
        .iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid1.as_str())
        })
        .expect("uuid1のfile_upload")
        .clone();
    let craft_path = "/crafted/lookup-min/dup.zip";
    let mut fw2 = fw1.clone();
    fw2["uuid"] = serde_json::json!(uuid2);
    fw2["source_path"] = serde_json::json!(craft_path);
    fw2["identity_hash"] = serde_json::json!(sha256_hex(craft_path.as_bytes())[..8]);
    fw2["s3_key"] = serde_json::json!(fx.data_key(&uuid2));
    let mut fu2 = fu1.clone();
    fu2["uuid"] = serde_json::json!(uuid2);
    fu2["s3_key"] = serde_json::json!(fx.data_key(&uuid2));
    fu2["version_id"] = serde_json::json!("crafted-vid-2");
    append_registry_line(&fx.registry, &fw2);
    append_registry_line(&fx.registry, &fu2);

    // 移動+内容不変 → 複数uuid候補から辞書順最小(uuid2)へ合流する
    let p2 = fx.data_root.join("two").join("a.zip");
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("two dir");
    std::fs::rename(&p1, &p2).expect("move");
    let (code, stdout, stderr) = fx.push(&[&p2], &[], &[]);
    assert_eq!(code, 0, "移動後push: {stdout}{stderr}");
    let merged = reg_uuid_of(&fx, &p2);
    assert_eq!(
        merged, uuid2,
        "歴史的重複は辞書順最小uuidへ合流(uuid1={uuid1} uuid2={uuid2})"
    );
}

/// [covers:registry.sha-uuid-lookup-bucket-scoped]
/// lookupのuuid集合はbucket一致で分離される: 他方bucketの同sha記録は不採用。
/// s3_bucket欠損(空文字)の旧記録相当は正本bucket解釈(record_bucket_matchedと同一規則)で、
/// 実効bucket=正本bucketの実行でのみ採用される。
#[test]
fn sha_uuid_lookup_is_scoped_by_bucket() {
    let Some(fx) = StorageFixture::new(
        "reg-lookup3",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    // dev(active_profile=正本)とprodが同一prefix・別bucketの構成
    fx.rewrite_aws_toml(&format!(
        "[aws]\nactive_profile = \"dev\"\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"bucket-dev\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n\n[aws.profiles.prod]\npurpose = \"prod\"\nbucket = \"bucket-prod\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n",
        creds = fx.credentials.display()
    ));
    let p1 = fx.data_root.join("one").join("a.zip");
    std::fs::create_dir_all(&p1.parent().expect("parent")).expect("one dir");
    write_file(&p1, b"lookup-bucket-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&p1], "bucket-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "dev側初回push: {stdout}");

    // (1) 同shaのfile_uploadがprod bucket側のみ存在 → dev実効bucketでのlookupは不採用
    let prod_uuid = {
        let registry = fx.registry_values();
        let fw = registry
            .iter()
            .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
            .expect("file_write")
            .clone();
        let uuid = reg_uuid_of(&fx, &p1);
        // dev側記録(prod由来のsha一致記録を作るためuuidは別)をprod bucketへ複製
        let mut uuid2 = uuid.clone();
        uuid2.replace_range(0..8, "00000001");
        let mut fw2 = fw.clone();
        fw2["uuid"] = serde_json::json!(uuid2);
        fw2["source_path"] = serde_json::json!("/crafted/bucket/dup.zip");
        fw2["identity_hash"] =
            serde_json::json!(sha256_hex(b"/crafted/bucket/dup.zip")[..8].to_string());
        fw2["s3_key"] = serde_json::json!(fx.data_key(&uuid2));
        append_registry_line(&fx.registry, &fw2);
        let fu = registry
            .iter()
            .find(|r| {
                r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                    && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid.as_str())
            })
            .expect("file_upload")
            .clone();
        let mut fu2 = fu;
        fu2["uuid"] = serde_json::json!(uuid2);
        fu2["s3_bucket"] = serde_json::json!("bucket-prod");
        fu2["s3_key"] = serde_json::json!(fx.data_key(&uuid2));
        fu2["version_id"] = serde_json::json!("prod-vid-1");
        append_registry_line(&fx.registry, &fu2);
        uuid2
    };
    // dev側(正本bucket)のfile_uploadは一旦除去し、他bucket記録のみの状態で移動push
    let dev_uuid = reg_uuid_of(&fx, &p1);
    rewrite_registry_lines(&fx.registry, |r| {
        !(r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
            && r.get("s3_bucket").and_then(|b| b.as_str()) == Some("bucket-dev"))
    });
    let p2 = fx.data_root.join("two").join("a.zip");
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("two dir");
    std::fs::rename(&p1, &p2).expect("move");
    // 新規発行となるpush(設計6.1: title必須のためpty promptで与える)
    let Some((code, stdout)) = push_titled_pty(&fx, &[&p2], "dev-new-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "(1)移動後push(dev): {stdout}");
    let new_uuid = reg_uuid_of(&fx, &p2);
    assert_ne!(
        new_uuid, prod_uuid,
        "他方bucket(prod)の同sha記録は不採用(合流しない=新規発行)"
    );

    // (2) file_upload(s3_bucket空文字=旧記録相当)は正本bucket(dev)側でのみ採用
    rewrite_registry_lines(&fx.registry, |r| {
        if r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload") {
            // prod実績を除去し、dev側1件をs3_bucket空文字へ書き換える
            r.get("s3_bucket").and_then(|b| b.as_str()) != Some("bucket-prod")
        } else {
            true
        }
    });
    let registry = fx.registry_values();
    if let Some(target) = registry
        .iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                && r.get("version_id").and_then(|v| v.as_str()) != Some("prod-vid-1")
        })
        .max_by_key(|r| {
            r.get("recorded_at")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .cloned()
    {
        let mut rewritten = target.clone();
        rewritten["s3_bucket"] = serde_json::json!("");
        // 当該行を差し替え(旧行除去→新行追記)
        rewrite_registry_lines(&fx.registry, |r| r != &target);
        append_registry_line(&fx.registry, &rewritten);
    }
    // prod(実効bucket≠正本)実行: 空文字記録は正本bucket扱いのためprod実効bucketでは
    // 不採用=合流しない(新規発行はtitle必須のためpty promptで与える・設計6.1)
    let p3 = fx.data_root.join("three").join("a.zip");
    std::fs::create_dir_all(&p3.parent().expect("parent")).expect("three dir");
    std::fs::rename(&p2, &p3).expect("move");
    let Some((code, stdout)) =
        push_titled_pty(&fx, &[&p3], "prod-new-title", &["--profile", "prod"])
    else {
        return;
    };
    assert_eq!(code, 0, "(2)prod側実行: {stdout}");
    let prod_new_uuid = reg_uuid_of(&fx, &p3);
    assert_ne!(
        prod_new_uuid, dev_uuid,
        "(2)prod側実行では空文字(正本bucket扱い)記録は不採用"
    );
}
