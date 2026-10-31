//! index エリアの結合test(TASK-10 S3ストレージ設計・tmp/tasks/tm-10/design_s3_storage.md 2章)。
//! src/index.rsは実装前(Phase Aで新設)のため、索引の構築・直列化はfake seam経由の
//! backup push/pin add/rm・manage index rebuildの観測(stateの索引objectを復号して検証)で
//! 行う(テスト先行のため実装前の実行failは許容・cargo test --no-runは通る)。

mod common;

use common::*;
use std::path::PathBuf;

/// multipart閾値(64 MiB・index.version-id-recordedのmultipart側で使用)。
const THRESHOLD: usize = 64 * 1024 * 1024;

/// 索引のheader行を取り出す。
fn index_header(lines: &[serde_json::Value]) -> &serde_json::Value {
    lines
        .iter()
        .find(|v| v.get("record_type").and_then(|r| r.as_str()) == Some("index_header"))
        .expect("索引にindex_header行がある")
}

/// entryのversions配列を取り出す。
fn entry_versions(entry: &serde_json::Value) -> &Vec<serde_json::Value> {
    entry
        .get("versions")
        .and_then(|v| v.as_array())
        .expect("versions配列")
}

/// dir配下のfileを再帰的に列挙する(manifest一覧の発見用)。
fn walk_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_files(&p));
        } else {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// state内のmanifest object一覧(sys/manifests/配下)。
fn manifest_objects(fx: &StorageFixture) -> Vec<PathBuf> {
    let prefix_dir = fx.prefix.trim_end_matches('/');
    walk_files(
        &fx.state
            .join("objects")
            .join(prefix_dir)
            .join("sys")
            .join("manifests"),
    )
}

/// calls.logを空にする(以降の呼び出しのみを観測する)。
fn clear_calls(fx: &StorageFixture) {
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
}

/// TASK-18結合test用: title付きの新規発行push(pty prompt)。
fn idx_push_titled(
    fx: &StorageFixture,
    paths: &[&std::path::Path],
    titles: &str,
) -> Option<(i32, String)> {
    push_titled_pty(fx, paths, titles, &[])
}

/// registryのfile_write記録からsource_pathでuuidを引く。
fn idx_uuid_of(fx: &StorageFixture, path: &std::path::Path) -> String {
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

/// 同一entityへ2版目を重ねる(crash残存re-put経路・設計4.4(ii)(iii)。共通helperは
/// 除去したfile_upload記録を復元するため、registryには各版の記録が残る)。
fn idx_add_second_version(fx: &StorageFixture, path: &std::path::Path) {
    let _ = add_version_via_reput(fx, path, &[]);
}

/// TASK-18新体系のStorageOpts([categories]廃止・[aws.lifecycle]必須)。
fn idx_opts() -> StorageOpts {
    StorageOpts {
        no_categories: true,
        aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
        ..StorageOpts::default()
    }
}

/// [covers:index.schema-and-serialization]
/// 索引JSONL schema(TASK-20 3.1・5.4のschema=3・実体単位entity構造):
/// index_header(schema=3・updated_at・run_id・tool_version・entries=N)+file entity
/// (record_type=file維持・uuid・title・versions[]・pins[]・locations[])。
/// versionsはdate降順で各versionのfield構成は不変(s3_key=data/{uuid}/default.age記録値)。
/// IndexLocation: source_path・category・label・identity_hash・last_backup
/// (TASK-20: rel_path→label改名。空=labelなし)。
/// locationsの並びはsource_path辞書順。旧entryのname/category等はlocationsへ移行・
/// folder廃止。直列化→parseの往復で全fieldが保たれる。
#[test]
fn index_jsonl_schema_and_field_roundtrip() {
    let Some(fx) = StorageFixture::new("idx-schema", idx_opts()) else {
        return;
    };
    let a = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("travel.zip");
    write_file(&a, b"index-schema-plain");
    let b = fx.elsewhere.join("x.zip");
    write_file(&b, b"index-schema-b");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a, &b], "旅のしおり\nx-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    idx_add_second_version(&fx, &a);
    assert_eq!(
        fx.pin("add", "旅のしおり", &["--latest", "--as", "pre-migration"])
            .0,
        0
    );

    let lines = fx.read_index();
    let header = index_header(&lines);
    assert_eq!(
        header.get("schema").and_then(|v| v.as_i64()),
        Some(3),
        "schema=3: {header}"
    );
    for field in ["updated_at", "run_id", "tool_version"] {
        assert!(
            header.get(field).is_some(),
            "headerに{field}がある: {header}"
        );
    }
    let file_entries = lines
        .iter()
        .filter(|v| v.get("record_type").and_then(|r| r.as_str()) == Some("file"))
        .count();
    assert_eq!(
        header.get("entries").and_then(|v| v.as_i64()),
        Some(file_entries as i64),
        "entries=Nはentity数と一致"
    );

    let uuid = idx_uuid_of(&fx, &a);
    let entry = index_entity_by(&lines, "uuid", &uuid);
    assert_eq!(
        entry.get("title").and_then(|t| t.as_str()),
        Some("旅のしおり"),
        "entityのtitle: {entry}"
    );
    // 旧entryのfieldはlocationsへ移行(entry直下には無い)
    for field in [
        "name",
        "category",
        "label",
        "source_path",
        "identity_hash",
        "folder",
    ] {
        assert!(
            entry.get(field).is_none(),
            "entity直下に{field}は無い(locationsへ移行・folder廃止): {entry}"
        );
    }
    // IndexLocation: source_path・category・label・identity_hash・last_backup
    let locations = entity_locations(entry);
    assert_eq!(locations.len(), 1, "所在1件: {locations:?}");
    let loc = &locations[0];
    assert_eq!(
        loc.get("source_path").and_then(|v| v.as_str()),
        Some(a.canonicalize().unwrap().to_string_lossy().as_ref()),
        "source_pathはrealpath絶対path: {loc}"
    );
    assert_eq!(
        loc.get("category").and_then(|v| v.as_str()),
        Some("unfiled"),
        "categoryは分類解決結果の記録値: {loc}"
    );
    assert_eq!(
        loc.get("label").and_then(|v| v.as_str()),
        Some("2020-2023/travel"),
        "labelは正規化済み記録値: {loc}"
    );
    assert_eq!(
        loc.get("identity_hash").and_then(|v| v.as_str()),
        Some(hash8_of_path(&a).as_str()),
        "identity_hash=sha256(realpath)先頭8hex: {loc}"
    );
    assert!(
        loc.get("last_backup").is_some(),
        "last_backup(YYYY-MM-DD)がある: {loc}"
    );

    let versions = entry_versions(entry);
    assert_eq!(versions.len(), 2, "当該entityは2版: {versions:?}");
    let reg_ids = registry_upload_version_ids(&fx.registry_values(), &fx.data_key(&uuid));
    assert_eq!(reg_ids.len(), 2, "registryのfile_upload記録は2件");
    // versionsは新しい順(先頭=最新)
    assert_eq!(
        index_version_ids(entry),
        reg_ids.iter().rev().cloned().collect::<Vec<_>>(),
        "versionsはdate降順(先頭=最新)"
    );
    for v in versions {
        for field in [
            "date",
            "version_ts",
            "batch_id",
            "s3_key",
            "version_id",
            "source_sha256",
            "age_sha256",
            "source_size",
            "age_size",
        ] {
            assert!(v.get(field).is_some(), "versionに{field}がある: {v}");
        }
        assert_eq!(
            v.get("s3_key").and_then(|k| k.as_str()),
            Some(fx.data_key(&uuid).as_str()),
            "s3_keyはdata/{uuid}/default.age記録値: {v}"
        );
    }

    let pins = entity_pins(entry);
    assert_eq!(pins.len(), 1, "pin add後のpinsは1件: {pins:?}");
    for field in [
        "pin",
        "s3_key",
        "created_at",
        "date",
        "source_sha256",
        "age_sha256",
        "source_size",
    ] {
        assert!(
            pins[0].get(field).is_some(),
            "pin entryに{field}がある: {pins:?}"
        );
    }

    // 直列化→parseの往復: 次のコマンド(list versions)が当該索引をparseして
    // 2版とも表示できる=fieldが失われていない
    let (code, stdout, stderr) = fx.list("versions", &["--title", "旅のしおり"]);
    assert_eq!(code, 0, "list versionsはexit 0: {stdout}{stderr}");
    for vid in index_version_ids(entry) {
        assert!(
            stdout.contains(&vid),
            "parse後もversion_id({vid})が参照できる: {stdout}"
        );
    }
}

/// [covers:index.rebuild-from-registry-per-run]
/// 索引の更新単位はrun毎+索引に影響する操作後(設計2.2): ローカルregistry(reduce後)から
/// 全entryを再構築し、versions[].version_id・s3_keyはregistryのfile_upload記録値。
/// 再構築は冪等(影響操作を挟んでもregistryから再構築された内容は同一)。
/// TASK-18: 同一entityの2版はcrash残存re-put経路(設計4.4)で構築する。
#[test]
fn index_rebuilt_from_registry_each_run() {
    let Some(fx) = StorageFixture::new("idx-rebuild", idx_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.elsewhere.join("b.zip");
    write_file(&a, b"rebuild-a");
    write_file(&b, b"rebuild-b");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a, &b], "a.zip\nb.zip") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    // 同一entityへの2版目(crash残存re-put経路・記録は復元して両版を保持)
    let ids = add_version_via_reput(&fx, &a, &[]);
    assert_eq!(ids.len(), 2, "a.zipのfile_upload記録は2件");

    let lines = fx.read_index();
    let uuid = idx_uuid_of(&fx, &a);
    let a_key = fx.data_key(&uuid);
    let entry = index_entity_by(&lines, "uuid", &uuid);
    let reg_ids = registry_upload_version_ids(&fx.registry_values(), &a_key);
    assert_eq!(reg_ids.len(), 2, "a.zipのfile_upload記録は2件");
    let versions = entry_versions(entry);
    assert_eq!(versions.len(), 2, "run毎の再構築で2版が載る: {versions:?}");
    // versions[].version_id・s3_keyはregistryのfile_upload記録値
    for (v, reg_id) in versions.iter().zip(reg_ids.iter().rev()) {
        assert_eq!(
            v.get("version_id").and_then(|i| i.as_str()),
            Some(reg_id.as_str()),
            "version_idはregistry記録値: {v}"
        );
        assert_eq!(
            v.get("s3_key").and_then(|k| k.as_str()),
            Some(a_key.as_str()),
            "s3_keyはregistry記録値: {v}"
        );
    }
    assert!(
        index_entity_by(&lines, "uuid", &idx_uuid_of(&fx, &b))
            .get("versions")
            .is_some(),
        "b.zip entryも再構築される"
    );

    // 冪等: pin add→pin rm(索引に影響する操作)で再構築・再PUTされても
    // registry由来のversions内容は同一(pinsは空へ戻る)
    let versions_before = entry_versions(entry).clone();
    assert_eq!(
        fx.pin("add", "a.zip", &["--latest", "--as=keep-check"]).0,
        0
    );
    assert_eq!(fx.pin("rm", "a.zip", &["--pin", "keep-check"]).0, 0);
    let lines2 = fx.read_index();
    let entry2 = index_entity_by(&lines2, "uuid", &uuid);
    assert_eq!(
        entry_versions(entry2),
        &versions_before,
        "再構築後のversionsは1回目と同一内容(冪等)"
    );
    let pins2 = entity_pins(entry2);
    assert!(pins2.is_empty(), "pin rm後はpinsが空: {pins2:?}");
}

/// [covers:index.hot-storage-class-upload]
/// 索引 sys/index.jsonl.age のPUTはstorage class STANDARD固定(設計1.4・2.2。
/// per-object指定)。profileのstorage_classがDEEP_ARCHIVEでもSTANDARDで置かれる。
#[test]
fn index_put_uses_standard_storage_class() {
    let Some(fx) = StorageFixture::new(
        "idx-hot",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"index-hot");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "a.zip") else {
        return;
    };
    assert_eq!(code, 0, "DEEP_ARCHIVE profileでのpush: {stdout}");
    let calls = fx.calls();
    let index_key = fx.index_key();
    let index_put = calls
        .lines()
        .find(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={index_key}")))
        .unwrap_or_else(|| panic!("索引PUT行: {calls}"));
    assert!(
        index_put.contains("storage_class=STANDARD"),
        "索引PUTはSTORAGE CLASS STANDARD固定: {index_put}"
    );
    // 実体objectはprofile値(DEEP_ARCHIVE)のまま(data/{uuid}/default.age)
    let file_key = fx.data_key(&idx_uuid_of(&fx, &a));
    let file_put = calls
        .lines()
        .find(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={file_key}")))
        .unwrap_or_else(|| panic!("file PUT行: {calls}"));
    assert!(
        file_put.contains("storage_class=DEEP_ARCHIVE"),
        "file objectはprofileのstorage_class: {file_put}"
    );
}

/// [covers:index.encrypted-with-configured-recipients]
/// 索引はpipeline.tomlの現在のrecipients構成でage暗号化され(設計2.2)、
/// 各recipientの秘密鍵で復号できる(1-of-N)。平文は索引JSONLとしてparseできる。
#[test]
fn index_encrypted_with_all_configured_recipients() {
    let Some(fx) = StorageFixture::new("idx-recip", StorageOpts::default()) else {
        return;
    };
    // 第2recipientを追加(2鍵構成)
    let (pub2, secret2) = generate_ed25519_keypair(&fx.elsewhere, "k2");
    let pub1 = fx.public.display().to_string();
    fx.rewrite_pipeline_toml(&fx.pipeline_body().replace(
        &format!("public_keys = [\"{pub1}\"]"),
        &format!("public_keys = [\"{pub1}\", \"{}\"]", pub2.display()),
    ));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"index-recipients");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "a.zip") else {
        return;
    };
    assert_eq!(code, 0, "2鍵構成でのpush: {stdout}");

    let index_obj = fx.object_path(&fx.index_key());
    for (tag, secret) in [("第1鍵", fx.secret.clone()), ("第2鍵", secret2)] {
        let plain = age_decrypt(&fx.age, &index_obj, &secret);
        let text =
            String::from_utf8(plain).unwrap_or_else(|e| panic!("{tag}で復号した平文はUTF-8: {e}"));
        let first = text
            .lines()
            .next()
            .unwrap_or_else(|| panic!("{tag}で復号した索引は空でない"));
        let v: serde_json::Value = serde_json::from_str(first)
            .unwrap_or_else(|e| panic!("{tag}の平文は索引JSONLとしてparseできる: {e}"));
        assert_eq!(
            v.get("record_type").and_then(|r| r.as_str()),
            Some("index_header"),
            "{tag}で復号した平文は索引JSONL: {text}"
        );
    }
}

/// [covers:index.version-id-recorded]
/// 索引versions[].version_idの供給源はregistryのfile_upload記録(設計2.1・8.1・11章F3):
/// S3Backend::putはversion_idを返し(単発PUTはPutObject応答・multipartは
/// CompleteMultipartUpload応答)、UploadRecordへversion_idと実s3_keyが記録される。
#[test]
fn index_version_ids_come_from_put_responses_via_registry() {
    let Some(fx) = StorageFixture::new("idx-vid", StorageOpts::default()) else {
        return;
    };
    let small = fx.data_root.join("small.zip");
    write_file(&small, b"index-version-id-small");
    let big = fx.data_root.join("big.zip");
    make_archive(&big, THRESHOLD + 1024 * 1024);
    let Some((code, stdout)) = idx_push_titled(&fx, &[&small, &big], "small.zip\nbig.zip") else {
        return;
    };
    assert_eq!(code, 0, "単発+multipartのpush: {stdout}");

    let calls = fx.calls();
    let big_key = fx.data_key(&idx_uuid_of(&fx, &big));
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 CreateMultipartUpload")
                && l.contains(&format!("key={big_key}"))),
        "big.zipはmultipart: {calls}"
    );
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 CompleteMultipartUpload")
                && l.contains(&format!("key={big_key}"))),
        "big.zipのCompleteMultipartUpload: {calls}"
    );

    let lines = fx.read_index();
    let registry = fx.registry_values();
    for (name, key) in [
        ("small.zip", fx.data_key(&idx_uuid_of(&fx, &small))),
        ("big.zip", big_key),
    ] {
        let entry = index_entity_by(&lines, "title", name);
        let vids = index_version_ids(entry);
        assert_eq!(vids.len(), 1, "{name}は1版: {vids:?}");
        assert!(!vids[0].is_empty(), "{name}のversion_idは空でない");
        let reg_ids = registry_upload_version_ids(&registry, &key);
        assert_eq!(
            reg_ids, vids,
            "{name}の索引version_idはregistryのfile_upload記録値(PUT応答由来)"
        );
        let s3_keys: Vec<&str> = entry_versions(entry)
            .iter()
            .filter_map(|v| v.get("s3_key").and_then(|k| k.as_str()))
            .collect();
        assert_eq!(
            s3_keys,
            vec![key.as_str()],
            "{name}のs3_keyはregistry記録の実key"
        );
    }
}

/// [covers:index.rebuild-from-manifests]
/// manage index rebuild(TASK-18 5.12・3.3): manifest(file record schema=3: uuid/title/label入り・
/// TASK-20でschema 3化。batch header行はschema=2のまま)からentity単位へentry再構築。
/// locationsはfile recordsのsource_path単位(最新)から復元(所在記録fieldの読取はlabel)。
/// prunes/sweepsの除外集合を適用(prune除去版の索引への復活防止)。
/// pin復元はuuid照合。version_id欠損manifestはListObjectVersionsで補完。
/// 完了時は続けてのmanage index reconcile実行を案内表示。開始時にtier別単価で
/// 区別した費用・所要の概算を表示。
#[test]
fn manage_index_rebuild_rebuilds_entries_from_manifests() {
    let Some(fx) = StorageFixture::new(
        "idx-mreb",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            ..idx_opts()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"rebuild-manifests-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "rebuild-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    // 同一entityへ3版(各版のfile_upload記録を保持するre-put経路・設計4.4)。
    // keep_versions=1のpruneで削除対象が生じる構成(境界=keep番目は残るため
    // 2版では削除0件となる・設計5.12)
    idx_add_second_version(&fx, &a);
    idx_add_second_version(&fx, &a);
    let uuid = idx_uuid_of(&fx, &a);
    let reg_ids = registry_upload_version_ids(&fx.registry_values(), &fx.data_key(&uuid));
    assert_eq!(reg_ids.len(), 3, "3 run分のfile_upload記録");

    // 最古版をprune --applyで削除(除外集合適用の検証用。keep_versions=1)
    fx.rewrite_aws_toml(
        &fx.aws_toml_body()
            .replace("keep_versions = 10", "keep_versions = 1"),
    );
    let (code, stdout, stderr) = fx.manage("prune", &["--apply"]);
    assert_eq!(code, 0, "prune --apply: {stdout}{stderr}");
    let pruned = &reg_ids[0];

    // 最新版(3版目)のmanifestを旧式(version_id欠損)へ書き換える
    let manifests = manifest_objects(&fx);
    assert_eq!(manifests.len(), 3, "manifestは3件: {manifests:?}");
    let old_manifest = manifests[2].clone();
    let plain = age_decrypt(&fx.age, &old_manifest, &fx.secret);
    let text = String::from_utf8(plain).expect("manifest平文はUTF-8");
    let mut rewritten = String::new();
    for line in text.lines() {
        let mut v: serde_json::Value = serde_json::from_str(line).expect("manifest行はJSON");
        if v.get("version_id").is_some() {
            v.as_object_mut()
                .expect("manifest file recordはobject")
                .remove("version_id");
        }
        rewritten.push_str(&serde_json::to_string(&v).expect("manifest行serialize"));
        rewritten.push('\n');
    }
    age_encrypt_bytes(
        &fx.age,
        &[fx.public.clone()],
        rewritten.as_bytes(),
        &old_manifest,
    );

    // 索引objectを喪失させる
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    clear_calls(&fx);

    // 再構築(未復元manifestへBulk復元job発行。fakeは発行でrestored状態へ遷移するため
    // 1実行で完結する実装と進行中案内で再実行を要する実装の両方に対応する)
    let mut all_stdout = String::new();
    let mut code = 0;
    for _ in 0..2 {
        let (c, stdout, stderr) = fx.manage("index", &["rebuild"]);
        code = c;
        all_stdout.push_str(&stdout);
        all_stdout.push_str(&stderr);
        if fx.object_path(&fx.index_key()).is_file() {
            break;
        }
    }
    assert_eq!(code, 0, "manage index rebuildはexit 0: {all_stdout}");
    assert!(
        fx.object_path(&fx.index_key()).is_file(),
        "索引objectが再PUTされる: {all_stdout}"
    );

    // 未復元manifestへBulkの復元jobが発行される
    let calls = fx.calls();
    let restore_lines: Vec<&str> = calls
        .lines()
        .filter(|l| l.starts_with("s3 RestoreObject") && l.contains("/sys/manifests/"))
        .collect();
    assert!(
        !restore_lines.is_empty(),
        "manifestへのRestoreObject発行: {calls}"
    );
    assert!(
        restore_lines.iter().all(|l| l.contains("tier=Bulk")),
        "復元jobはBulk既定: {calls}"
    );
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 ListObjectsV2") && l.contains("sys/manifests/")),
        "ListObjectsV2でのmanifest一覧: {calls}"
    );
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 ListObjectVersions")),
        "version_id欠損manifestの補完にListObjectVersions: {calls}"
    );
    // 開始時のtier別費用・所要の概算表示とreconcile案内
    assert!(
        all_stdout.contains("Bulk") && all_stdout.contains("Standard"),
        "tier別(Standard/Bulk)の費用・所要概算表示: {all_stdout}"
    );
    assert!(
        all_stdout.contains("reconcile"),
        "完了時のmanage index reconcile実行案内: {all_stdout}"
    );

    // 再構築された索引: entity(uuid/title/locations)・prune済み版は復活しない
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "uuid", &uuid);
    assert_eq!(
        entry.get("title").and_then(|t| t.as_str()),
        Some("rebuild-title"),
        "manifest由来のtitle: {entry}"
    );
    let locations = entity_locations(entry);
    assert_eq!(
        locations.len(),
        1,
        "locationsはfile recordsのsource_path単位で復元: {locations:?}"
    );
    let vids = index_version_ids(entry);
    assert!(
        !vids.contains(pruned),
        "prune済み版({pruned})はversionsへ復活しない: {vids:?}"
    );
    assert!(
        vids.contains(&reg_ids[2]),
        "現行版(version_id欠損manifest分はListObjectVersionsで補完)が載る: {vids:?}"
    );
}

/// [covers:index.rebuild-from-manifests]
/// manage index rebuildの旧manifest拒否(TASK-22・PR#4レビュー指摘P2):
/// 旧schema=2 file record(rel_path入り・label無し)やschema=3のまま必須field(label)が
/// 欠損するfile recordを含むmanifestからのrebuildは、分類labelが黙って消えた状態の
/// schema=3索引をuploadせず索引PUT前に失敗する(schema≠3は設定エラー2+旧manifestを
/// 明示した初期化と再pushの案内・必須field欠損は処理エラー4。registry file_writeの
/// deny_unknown_fields・索引parseのschema≠3拒否と同じ拒否をmanifest file record経路にも適用)。
#[test]
fn manage_index_rebuild_rejects_legacy_manifest_records() {
    let Some(fx) = StorageFixture::new(
        "idx-mreb-legacy",
        StorageOpts {
            files: "[files.\"a.zip\"]\ncategory = \"photos\"\nlabel = \"legacy\"\n".into(),
            ..idx_opts()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"rebuild-legacy-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "legacy-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let manifests = manifest_objects(&fx);
    assert_eq!(manifests.len(), 1, "manifestは1件: {manifests:?}");
    let manifest = manifests[0].clone();
    let original_plain = age_decrypt(&fx.age, &manifest, &fx.secret);

    // manifest平文のfile recordを書き換えて暗号化し直す(batch header行は不変)
    let rewrite_manifest = |transform: &dyn Fn(serde_json::Value) -> serde_json::Value| {
        let text = String::from_utf8(original_plain.clone()).expect("manifest平文はUTF-8");
        let mut rewritten = String::new();
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).expect("manifest行はJSON");
            rewritten.push_str(&serde_json::to_string(&transform(v)).expect("manifest行serialize"));
            rewritten.push('\n');
        }
        age_encrypt_bytes(
            &fx.age,
            &[fx.public.clone()],
            rewritten.as_bytes(),
            &manifest,
        );
    };

    // 索引object不在の状態でrebuildを実行(復元job発行の再実行を吸収)し、
    // 終了コードと出力を返す
    let run_rebuild = || -> (i32, String) {
        let mut out = String::new();
        let mut code = 0;
        for _ in 0..3 {
            let (c, stdout, stderr) = fx.manage("index", &["rebuild"]);
            code = c;
            out.push_str(&stdout);
            out.push_str(&stderr);
            if code != 0 || fx.object_path(&fx.index_key()).is_file() {
                break;
            }
        }
        (code, out)
    };

    // (1) 旧schema=2 file record(rel_path入り・label無し): 設定エラー2+uploadなし
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    rewrite_manifest(&|mut v| {
        if v.get("record_type").and_then(|t| t.as_str()) == Some("file") {
            let obj = v.as_object_mut().expect("manifest file recordはobject");
            obj.insert("schema".into(), serde_json::json!(2));
            let label = obj.remove("label").unwrap_or_default();
            obj.insert("rel_path".into(), label);
        }
        v
    });
    let (code, out) = run_rebuild();
    assert_eq!(code, 2, "旧schema=2 file record混入は設定エラー2: {out}");
    assert!(
        !fx.object_path(&fx.index_key()).is_file(),
        "索引はuploadされない(label黙示消去なし): {out}"
    );
    assert!(
        out.contains("旧形式") && out.contains("再push"),
        "旧manifestと初期化・再pushの案内: {out}"
    );

    // (2) schema=3のまま必須field(label)欠損: 処理エラー4+uploadなし
    // ((1)で索引がuploadされなかったため対象objectは無いまま)
    let _ = std::fs::remove_file(fx.object_path(&fx.index_key()));
    rewrite_manifest(&|mut v| {
        if v.get("record_type").and_then(|t| t.as_str()) == Some("file") {
            v.as_object_mut()
                .expect("manifest file recordはobject")
                .remove("label");
        }
        v
    });
    let (code, out) = run_rebuild();
    assert_eq!(code, 4, "必須field欠損は処理エラー4: {out}");
    assert!(
        !fx.object_path(&fx.index_key()).is_file(),
        "索引はuploadされない: {out}"
    );
    assert!(out.contains("label"), "欠損field名を明示: {out}");

    // (3) 正常なschema=3 recordの同一manifestからはrebuildできる(陽性対照)
    rewrite_manifest(&|v| v);
    let (code, out) = run_rebuild();
    assert_eq!(code, 0, "正常系rebuildは成功: {out}");
    let lines = fx.read_index();
    let uuid = idx_uuid_of(&fx, &a);
    let entry = index_entity_by(&lines, "uuid", &uuid);
    let locations = entity_locations(entry);
    assert_eq!(locations.len(), 1, "locationsは1件: {locations:?}");
    assert_eq!(
        locations[0].get("label").and_then(|l| l.as_str()),
        Some("legacy"),
        "labelは欠損せず復元される: {locations:?}"
    );
}

/// [covers:index.pin-entries]
/// pin add/rmの索引反映(設計2.1・2.2): pin addは当該file entryのpins[]へ
/// (pin・s3_key・created_at・date・source_sha256・age_sha256・source_size)、
/// pin rmは該当pin entryを除去する。
#[test]
fn index_pin_entries_reflect_pin_add_and_rm() {
    let Some(fx) = StorageFixture::new("idx-pins", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let plain = b"index-pin-entries";
    write_file(&a, plain);
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "a.zip") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    assert_eq!(
        fx.pin("add", "a.zip", &["--latest", "--as", "pre-migration"])
            .0,
        0
    );

    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "title", "a.zip");
    let pins = entry
        .get("pins")
        .and_then(|p| p.as_array())
        .expect("pins配列");
    assert_eq!(pins.len(), 1, "pin add後のpinsは1件: {pins:?}");
    let p = &pins[0];
    assert_eq!(p.get("pin").and_then(|v| v.as_str()), Some("pre-migration"));
    let pin_key = fx.data_pin_key(&idx_uuid_of(&fx, &a), "pre-migration");
    assert_eq!(
        p.get("s3_key").and_then(|v| v.as_str()),
        Some(pin_key.as_str())
    );
    assert_eq!(
        p.get("source_sha256").and_then(|v| v.as_str()),
        Some(sha256_hex(plain).as_str()),
        "pin記録のsource_sha256"
    );
    let pin_obj = std::fs::read(fx.object_path(&pin_key)).expect("pin object実体");
    assert_eq!(
        p.get("age_sha256").and_then(|v| v.as_str()),
        Some(sha256_hex(&pin_obj).as_str()),
        "pin記録のage_sha256はpin objectの実測"
    );
    assert_eq!(
        p.get("source_size").and_then(|v| v.as_i64()),
        Some(plain.len() as i64)
    );
    assert_eq!(
        p.get("date").and_then(|v| v.as_str()),
        Some(jst_today().as_str())
    );
    assert!(p.get("created_at").is_some(), "created_atがある: {p}");

    let registry = fx.registry_values();
    assert_eq!(
        registry_count(&registry, "pin_write"),
        1,
        "registryのpin_write記録"
    );

    // pin rmで該当pin entryが除去される
    assert_eq!(fx.pin("rm", "a.zip", &["--pin", "pre-migration"]).0, 0);
    let lines2 = fx.read_index();
    let entry2 = index_entity_by(&lines2, "title", "a.zip");
    let pins2 = entry2
        .get("pins")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(pins2.is_empty(), "pin rm後はpinsが空: {pins2:?}");
    assert_eq!(
        registry_count(&fx.registry_values(), "pin_remove"),
        1,
        "registryのpin_remove記録"
    );
}

/// [covers:index.version-ts-from-file-write]
/// 索引versions[].version_tsの時刻源(TASK-18 5.4・付記): registryのfile_upload記録自身の
/// version_tsとする(旧write_ts_by_group対応付けは廃止・簡素化)。
/// 同一日内複数版の一意化(getの--version-id特定・候補表示)はこの時刻で行う。
#[test]
fn version_ts_comes_from_file_write_recorded_at() {
    let Some(fx) = StorageFixture::new("idx-vts", idx_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"version-ts-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "vts-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    idx_add_second_version(&fx, &a);

    // registryのfile_upload記録(version_ts)と索引versions[].version_tsの対応
    let uuid = idx_uuid_of(&fx, &a);
    let uploads: Vec<serde_json::Value> = fx
        .registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload"))
        .collect();
    assert_eq!(uploads.len(), 2, "2版分のfile_upload記録: {uploads:?}");
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "uuid", &uuid);
    let versions = entry_versions(entry);
    assert_eq!(versions.len(), 2, "索引は2版: {versions:?}");
    for v in versions.iter() {
        let vid = v
            .get("version_id")
            .and_then(|t| t.as_str())
            .expect("version_id");
        let vts = v
            .get("version_ts")
            .and_then(|t| t.as_str())
            .expect("version_ts");
        let up = uploads
            .iter()
            .find(|u| u["version_id"].as_str() == Some(vid))
            .unwrap_or_else(|| panic!("当該版のfile_upload記録({vid}): {uploads:?}"));
        assert_eq!(
            up["version_ts"].as_str(),
            Some(vts),
            "version_tsは当該版file_upload記録自身のversion_tsと一致"
        );
    }
}

/// [covers:index.unknown-record-warning-plain-format]
/// 索引parse時の未知record_type行の警告はrecord_type文字列をそのまま表示する
/// (Some("x")等のRust Debug形式の引用でない・L-5。registry側の未知record_type警告と
/// 表示形式が揃う)。
#[test]
fn unknown_record_warning_uses_plain_format() {
    let Some(fx) = StorageFixture::new("idx-unknown", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"unknown-record-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "a.zip") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");

    // 索引へ未知record_type行を混入
    let mut lines = fx.read_index();
    lines.push(serde_json::json!({
        "schema": 1,
        "record_type": "future_x",
        "note": "前方互換の未知行",
    }));
    fx.rewrite_index(&lines);

    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(
        code, 0,
        "未知record_type行は警告のうえ読込が継続する(exit 0): {stdout}{stderr}"
    );
    assert!(
        stderr.contains("future_x"),
        "警告にrecord_type文字列をそのまま表示: {stderr}"
    );
    assert!(
        !stderr.contains("Some("),
        "Some(...)形式(Debug引用)では表示しない: {stderr}"
    );
}

/// [covers:index.pin-readd-after-remove-wins]
/// write-once正規フロー「pin rm後に再付与」(設計1.3・TASK-12)が索引経由の参照でも
/// 成立する: rm→同一pin名の再addではrecorded_atが新しい再付与のpin_writeが旧pin_removeに
/// 抑止されず勝ち(reduce結果のpin_removesから当該pin_s3_keyが除かれる)。
/// 索引pins[]へ載り(created_atは再付与時刻)list pins・get --pinで参照できる。
/// manage index rebuildのpins復元も同一reduce結果を参照するため共通。
#[test]
fn pin_readd_after_remove_wins_in_index() {
    let Some(fx) = StorageFixture::new("idx-pinreadd", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-readd-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "a.zip") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    // pin add(pm1)→pin rm(pm1)→同一pm1再付与(write-onceの正規フロー)
    assert_eq!(fx.pin("add", "a.zip", &["--latest", "--as", "pm1"]).0, 0);
    assert_eq!(fx.pin("rm", "a.zip", &["--pin", "pm1"]).0, 0);
    let (code, stdout, stderr) = fx.pin("add", "a.zip", &["--latest", "--as", "pm1"]);
    assert_eq!(
        code, 0,
        "rm後の同一pin名再付与はwrite-once違反にならずexit 0: {stdout}{stderr}"
    );
    let pin_key = fx.data_pin_key(&idx_uuid_of(&fx, &a), "pm1");
    assert!(
        fx.object_path(&pin_key).is_file(),
        "再付与でpin objectが再作成される: {pin_key}"
    );

    // 索引当該entry pins[]へpm1が載る(created_atは再付与のpin_write記録時刻)
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "title", "a.zip");
    let pins = entry
        .get("pins")
        .and_then(|p| p.as_array())
        .expect("pins配列");
    assert_eq!(pins.len(), 1, "再付与後のpinsはpm1の1件: {pins:?}");
    assert_eq!(pins[0].get("pin").and_then(|v| v.as_str()), Some("pm1"));
    assert_eq!(
        pins[0].get("s3_key").and_then(|v| v.as_str()),
        Some(pin_key.as_str())
    );
    let registry = fx.registry_values();
    let writes: Vec<&serde_json::Value> = registry
        .iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("pin_write")
                && r.get("pin").and_then(|p| p.as_str()) == Some("pm1")
        })
        .collect();
    assert_eq!(
        writes.len(),
        2,
        "pm1のpin_write記録は初回+再付与の2件: {writes:?}"
    );
    assert_eq!(
        pins[0].get("created_at").and_then(|v| v.as_str()),
        writes[1].get("recorded_at").and_then(|v| v.as_str()),
        "created_atは再付与(後の)pin_write記録時刻: {pins:?}"
    );

    // list pinsにpm1が現れる
    let (code, stdout, stderr) = fx.list("pins", &[]);
    assert_eq!(code, 0, "list pinsはexit 0: {stdout}{stderr}");
    assert!(stdout.contains("pm1"), "list pinsにpm1が現れる: {stdout}");

    // get --pin pm1はexit 0でpin objectを取得できる
    let out = fx.elsewhere.join("pinreadd-out");
    let out_s = out.display().to_string();
    let (code, stdout, stderr) = fx.get("a.zip", &["--pin", "pm1", "--out", &out_s]);
    assert_eq!(code, 0, "get --pin pm1はexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("pin版の復号成果物"),
        b"pin-readd-plain".to_vec(),
        "再付与pin objectの内容が検証・復元されて出力される"
    );

    // 索引object喪失後のmanage index rebuild復元にもpm1が含まれる(同一reduce結果)
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    let mut last = String::new();
    let mut code = 0;
    for _ in 0..2 {
        let (c, stdout, stderr) = fx.manage("index", &["rebuild"]);
        code = c;
        last = format!("{stdout}{stderr}");
        if fx.object_path(&fx.index_key()).is_file() {
            break;
        }
    }
    assert_eq!(code, 0, "manage index rebuildはexit 0: {last}");
    let lines2 = fx.read_index();
    let entry2 = index_entity_by(&lines2, "title", "a.zip");
    let pins2 = entry_pins(entry2);
    assert_eq!(pins2.len(), 1, "rebuild復元後のpinsにもpm1: {pins2:?}");
    assert_eq!(pins2[0].get("pin").and_then(|v| v.as_str()), Some("pm1"));

    // 再付与後のpin rm再実行(pm1を再度rm)はexit 0で成功する
    // (旧bugでは索引pins[]から消えたままのため対象エラー3で不能だった)
    let (code, stdout, stderr) = fx.pin("rm", "a.zip", &["--pin", "pm1"]);
    assert_eq!(
        code, 0,
        "再付与後のpin rm再実行はexit 0で成功: {stdout}{stderr}"
    );
}

/// 索引file entryのpins配列(索引にpinsが無い場合は空)。
fn entry_pins(entry: &serde_json::Value) -> Vec<serde_json::Value> {
    entry
        .get("pins")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default()
}

/// dev(active_profile)とprodが同一prefix・別bucketの2profile aws.toml本文
/// (TASK-15検証用。phase_b_options.rsのsame_prefix_diff_bucket_aws_tomlと同一構成。
/// 結合testの各ファイルは独立crateのため局部helperとして置く)。
fn same_prefix_diff_bucket_aws_toml(fx: &StorageFixture) -> String {
    format!(
        "[aws]\nactive_profile = \"dev\"\n[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"bucket-dev\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n\n[aws.profiles.prod]\npurpose = \"prod\"\nbucket = \"bucket-prod\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n",
        creds = fx.credentials.display()
    )
}

/// 両bucket同名pin付与状態の構築結果(TASK-15検証の共通構築)。
struct BothBucketPin {
    /// pin objectのkey(両bucketで同一・同一prefixのため)
    pin_key: String,
    /// dev側(先付け)pin_write記録のrecorded_at
    dev_created_at: String,
    /// prod側(後付け)pin_write記録のrecorded_at(構築時に差し替えた値)
    prod_created_at: String,
}

/// a.zipをdev・prod両bucketへpushし、同一pin名pm1をdev(先付け)・prod(後付け)の順で
/// 両bucketへ付与した状態を構築する(TASK-15のgiven)。fakeのS3 stateはbucket分離を
/// 持たないため2つ目のbucketへのCLI pin addはwrite-once検査(HeadObject)が共有実体を
/// 検出して拒否される。prod側pin_write記録はdev側実記録を複製しs3_bucket・時刻・
/// source版をprod側へ差し替えてregistry.jsonlへ直接追記する(tests/pin.rsの
/// append_pin_removeと同一のregistry構築手法)。
fn setup_both_bucket_same_name_pin(
    tag: &str,
    plain: &[u8],
) -> Option<(StorageFixture, BothBucketPin)> {
    let Some(fx) = StorageFixture::new(tag, StorageOpts::default()) else {
        return None;
    };
    fx.rewrite_aws_toml(&same_prefix_diff_bucket_aws_toml(&fx));
    let a = fx.data_root.join("a.zip");
    write_file(&a, plain);
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return None;
    };
    assert_eq!(code, 0, "dev(既定profile) push: {stdout}");
    assert_eq!(
        fx.pin("add", "a.zip", &["--latest", "--as", "pm1"]).0,
        0,
        "dev側(先付け)pin add pm1"
    );
    // prod側はbucket分離で別entity発行(設計4.3: skipしない)のためtitle指定でpush
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &["--profile", "prod"]) else {
        return None;
    };
    assert_eq!(
        code, 0,
        "prod側push(prod bucketへfile_upload記録): {stdout}"
    );

    // dev側pin_write実記録を取得し、prod側(後付け)記録を組み立ててregistryへ追記する
    let registry = fx.registry_values();
    let dev_write = registry
        .iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("pin_write")
                && r.get("s3_bucket").and_then(|b| b.as_str()) == Some("bucket-dev")
                && r.get("pin").and_then(|p| p.as_str()) == Some("pm1")
        })
        .cloned()
        .unwrap_or_else(|| panic!("dev側pin_write記録がある: {registry:?}"));
    let dev_created_at = dev_write
        .get("recorded_at")
        .and_then(|v| v.as_str())
        .expect("recorded_at")
        .to_string();
    // prod側付与時刻はdev側より後(後付け)で区別可能な値(+1時間)
    let prod_created_at = chrono::DateTime::parse_from_rfc3339(&dev_created_at)
        .map(|dt| (dt + chrono::Duration::hours(1)).to_rfc3339())
        .unwrap_or_else(|e| panic!("dev側recorded_atのparse: {e}"));
    let prod_vid = registry
        .iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                && r.get("s3_bucket").and_then(|b| b.as_str()) == Some("bucket-prod")
        })
        .and_then(|r| r.get("version_id").and_then(|v| v.as_str()))
        .expect("prod側file_upload記録のversion_id")
        .to_string();
    let mut prod_write = dev_write.clone();
    prod_write["recorded_at"] = serde_json::json!(prod_created_at);
    prod_write["uploaded_at"] = serde_json::json!(prod_created_at);
    prod_write["s3_bucket"] = serde_json::json!("bucket-prod");
    prod_write["source_version_id"] = serde_json::json!(prod_vid);
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&fx.registry)
            .expect("registry.jsonlへ追記");
        writeln!(f, "{prod_write}").expect("prod側pin_write記録の追記");
    }
    let pin_key = fx.data_pin_key(&uuid_of_path(&fx, &a), "pm1");
    Some((
        fx,
        BothBucketPin {
            pin_key,
            dev_created_at,
            prod_created_at,
        },
    ))
}

/// [covers:index.pins-multi-bucket-same-name-kept]
/// 同一prefix・別bucket構成で同一fileへ同名pinを複数bucketへ付与しても、後bucketの
/// pin_writeが先bucketのreduce map entryを上書きせず両bucketの索引pins[]にそれぞれの
/// pinが載る(TASK-15: pin_writes/pin_removesのmap keyが(pin_s3_key, s3_bucket)組である
/// こと。従来はpin_s3_key単位の後勝ちHashMapのため先付けdev側の索引からpinが消えた)。
/// created_atは各bucketの付与時刻で、list pins・get --pinが各profileでexit 0。
/// dev側索引object喪失後のmanage index rebuild復元後も含む。
#[test]
fn pins_multi_bucket_same_name_kept_in_each_index() {
    let plain = b"pins-multi-bucket-plain";
    let Some((fx, st)) = setup_both_bucket_same_name_pin("idx-pmbkt", plain) else {
        return;
    };
    assert_ne!(
        st.dev_created_at, st.prod_created_at,
        "両bucketの付与時刻は区別可能な値"
    );
    let a = fx.data_root.join("a.zip");

    // (1) dev側(先付けbucket)索引の再構築(all-skip push): pm1が載る
    // (後付けprodのpin_writeがdev側のmap entryを上書きしない=片寄りなし)
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0, "dev all-skip push");
    let pins = entry_pins(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert_eq!(
        pins.len(),
        1,
        "dev側索引pins[]にpm1が載る(map上書きで消えない): {pins:?}"
    );
    assert_eq!(pins[0].get("pin").and_then(|v| v.as_str()), Some("pm1"));
    assert_eq!(
        pins[0].get("s3_key").and_then(|v| v.as_str()),
        Some(st.pin_key.as_str())
    );
    assert_eq!(
        pins[0].get("created_at").and_then(|v| v.as_str()),
        Some(st.dev_created_at.as_str()),
        "created_atはdev側(先付け)のpin_write記録時刻: {pins:?}"
    );
    // dev側のlist pins・get --pinが参照できる
    let (code, stdout, stderr) = fx.list("pins", &[]);
    assert_eq!(code, 0, "list pins(dev)はexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("pm1"),
        "list pins(dev)にpm1が現れる: {stdout}"
    );
    let out = fx.elsewhere.join("pmbkt-dev-out");
    let out_s = out.display().to_string();
    let (code, stdout, stderr) = fx.get("a.zip", &["--pin", "pm1", "--out", &out_s]);
    assert_eq!(code, 0, "get --pin pm1(dev)はexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("pin版の復号成果物(dev)"),
        plain.to_vec()
    );

    // (2) prod側(後付けbucket)索引の再構築(all-skip push): pm1が載る
    assert_eq!(
        fx.push(&[&a], &["--profile", "prod"], &[]).0,
        0,
        "prod all-skip push"
    );
    let pins = entry_pins(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert_eq!(
        pins.len(),
        1,
        "prod側索引pins[]にもpm1が載る(片寄りなし): {pins:?}"
    );
    assert_eq!(pins[0].get("pin").and_then(|v| v.as_str()), Some("pm1"));
    assert_eq!(
        pins[0].get("created_at").and_then(|v| v.as_str()),
        Some(st.prod_created_at.as_str()),
        "created_atはprod側(後付け)のpin_write記録時刻: {pins:?}"
    );
    let (code, stdout, stderr) = fx.list_env("pins", &["--profile", "prod"], &[]);
    assert_eq!(code, 0, "list pins(prod)はexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("pm1"),
        "list pins(prod)にpm1が現れる: {stdout}"
    );
    let out = fx.elsewhere.join("pmbkt-prod-out");
    let out_s = out.display().to_string();
    let (code, stdout, stderr) = fx.get_env(
        "a.zip",
        &["--pin", "pm1", "--profile", "prod", "--out", &out_s],
        &[],
    );
    assert_eq!(code, 0, "get --pin pm1(prod)はexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("pin版の復号成果物(prod)"),
        plain.to_vec()
    );

    // (3) dev側索引object喪失後のmanage index rebuild復元にもpm1が含まれる
    // (reduce結果を共有するためmap上書きが直っていれば復元でも載る)
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    let mut last = String::new();
    let mut code = 0;
    for _ in 0..2 {
        let (c, stdout, stderr) = fx.manage("index", &["rebuild"]);
        code = c;
        last = format!("{stdout}{stderr}");
        if fx.object_path(&fx.index_key()).is_file() {
            break;
        }
    }
    assert_eq!(code, 0, "manage index rebuild(dev)はexit 0: {last}");
    let pins = entry_pins(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert_eq!(
        pins.len(),
        1,
        "rebuild復元後のdev側索引pins[]にもpm1が含まれる: {pins:?}"
    );
    assert_eq!(
        pins[0].get("created_at").and_then(|v| v.as_str()),
        Some(st.dev_created_at.as_str()),
        "rebuild復元もdev側のpin_write記録時刻: {pins:?}"
    );
}

/// [covers:index.pin-rm-multi-bucket-independent]
/// 両bucket付与状態でのpin rmはbucket単位で独立している(TASK-15): dev側(先付け=
/// 従来索引からpinが消えていた側)のrmがexit 0(索引pins[]に載っているためpin名不在の
/// 対象エラー3にならない)で当該索引から除去・dev bucketのpin objectを削除する。
/// prod側はpm1を保持し(get --pin exit 0)、続くprod側rmもexit 0。一方のbucketでの
/// 実行のS3 API呼び出しは他方bucketへ向かわない。
#[test]
fn pin_rm_multi_bucket_independent() {
    let plain = b"pin-rm-multi-bucket-plain";
    let Some((fx, st)) = setup_both_bucket_same_name_pin("idx-rmbkt", plain) else {
        return;
    };
    let a = fx.data_root.join("a.zip");

    // dev側索引を再構築(all-skip push)した状態でdev側pin rm(従来は索引pins[]が空のため
    // 対象エラー3になった側)
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0, "dev all-skip push");
    let pin_obj = fx.object_path(&st.pin_key);
    let pin_bytes = std::fs::read(&pin_obj).expect("pin object実体(dev側写し)");
    clear_calls(&fx);
    let (code, stdout, stderr) = fx.pin("rm", "a.zip", &["--pin", "pm1"]);
    assert_eq!(
        code, 0,
        "dev側pin rmは索引pins[]に載っているため対象エラー3にならずexit 0: {stdout}{stderr}"
    );
    let calls = fx.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 DeleteObjectVersion")
                && l.contains("bucket=bucket-dev")
                && l.contains(&format!("key={}", st.pin_key))),
        "dev側rmのpin object削除はdev bucketへ向かう: {calls}"
    );
    assert!(
        !calls.lines().any(|l| l.contains("bucket=bucket-prod")),
        "dev側rmのS3 API呼び出しにprod bucketは無い(他方bucketへ影響しない): {calls}"
    );
    assert!(!pin_obj.is_file(), "dev bucketのpin objectは削除される");
    // dev側索引pins[]からpm1が除かれる(rm実行が索引を再構築する)
    let pins = entry_pins(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert!(
        pins.is_empty(),
        "dev側索引pins[]からpm1が除かれる: {pins:?}"
    );

    // fakeのS3 stateはbucket分離を持たないためdev側rmで共有実体が消えている。
    // 実S3ではprod bucketのpin objectは独立に存在し続けるため、prod側写しの生存を
    // stateへ再現する(実体+版state。prod側pin_write記録のage_sha256と同一bytes)
    fx.seed_object(&st.pin_key, &pin_bytes);
    let version_line = format!(
        "v2\t{}\t{}\tput\tSTANDARD",
        pin_bytes.len(),
        chrono::Local::now().to_rfc3339()
    );
    write_file(
        &fx.state.join("versions").join(&st.pin_key),
        version_line.as_bytes(),
    );

    // prod側はpm1を保持する(dev側rmの影響を受けない): 索引pins[]・get --pin
    assert_eq!(
        fx.push(&[&a], &["--profile", "prod"], &[]).0,
        0,
        "prod all-skip push"
    );
    let pins = entry_pins(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert_eq!(pins.len(), 1, "prod側索引pins[]はpm1を保持する: {pins:?}");
    assert_eq!(
        pins[0].get("created_at").and_then(|v| v.as_str()),
        Some(st.prod_created_at.as_str()),
        "保持しているpm1はprod側のpin_write記録時刻: {pins:?}"
    );
    let out = fx.elsewhere.join("rmbkt-prod-out");
    let out_s = out.display().to_string();
    let (code, stdout, stderr) = fx.get_env(
        "a.zip",
        &["--pin", "pm1", "--profile", "prod", "--out", &out_s],
        &[],
    );
    assert_eq!(code, 0, "prod側get --pin pm1はexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("pin版の復号成果物(prod)"),
        plain.to_vec()
    );

    // 続くprod側rmもexit 0でprod側索引から除去される
    clear_calls(&fx);
    let (code, stdout, stderr) =
        fx.pin_env("rm", "a.zip", &["--pin", "pm1", "--profile", "prod"], &[]);
    assert_eq!(code, 0, "prod側pin rmもexit 0: {stdout}{stderr}");
    let calls = fx.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 DeleteObjectVersion")
                && l.contains("bucket=bucket-prod")
                && l.contains(&format!("key={}", st.pin_key))),
        "prod側rmのpin object削除はprod bucketへ向かう: {calls}"
    );
    assert!(
        !calls.lines().any(|l| l.contains("bucket=bucket-dev")),
        "prod側rmのS3 API呼び出しにdev bucketは無い: {calls}"
    );
    let pins = entry_pins(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert!(
        pins.is_empty(),
        "prod側索引pins[]からpm1が除かれる: {pins:?}"
    );
}

/// [covers:index.parse-failure-guides-rebuild]
/// 索引parse失敗のerror文には`manage index rebuild`による索引再構築の案内を含める
/// (L-6)。schema違反平文は検証エラー4のまま。TASK-20の索引schema=3化に伴い、
/// 旧schema(1・2)索引の読込はschema不一致として設定エラー2+rebuild案内で拒否される
/// (旧索引の誤読防止)。
#[test]
fn parse_failure_error_guides_index_rebuild() {
    let Some(fx) = StorageFixture::new("idx-parseguide", idx_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"parse-guide");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "parse-guide") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let pristine: Vec<serde_json::Value> = fx.read_index();

    // (1) 索引objectをschema違反へ書き換える(entityから必須field(versions)を除去)
    let mut broken = pristine.clone();
    let entry = broken
        .iter_mut()
        .find(|v| v.get("record_type").and_then(|r| r.as_str()) == Some("file"))
        .expect("file entity");
    entry
        .as_object_mut()
        .expect("file entityはobject")
        .remove("versions");
    fx.rewrite_index(&broken);

    let (code, _, stderr) = fx.list("files", &[]);
    assert_eq!(code, 4, "schema違反索引は検証エラー4: {stderr}");
    assert!(
        stderr.contains("manage index rebuild"),
        "manage index rebuildの案内: {stderr}"
    );

    // (2) 旧schema=1・schema=2索引は設定エラー2+rebuild案内で拒否(TASK-20 schema 3化)
    for old_schema in [1, 2] {
        let mut v_old = pristine.clone();
        if let Some(header) = v_old
            .iter_mut()
            .find(|v| v.get("record_type").and_then(|r| r.as_str()) == Some("index_header"))
        {
            if let Some(obj) = header.as_object_mut() {
                obj.insert("schema".into(), serde_json::Value::from(old_schema));
            }
        }
        fx.rewrite_index(&v_old);
        let (code, _, stderr) = fx.list("files", &[]);
        assert_eq!(code, 2, "旧schema={old_schema}索引は設定エラー2: {stderr}");
        assert!(
            stderr.contains("manage index rebuild"),
            "manage index rebuildの案内(schema={old_schema}拒否時): {stderr}"
        );
    }
}

/// [covers:index.entity-grouping-by-uuid]
/// entity組立(TASK-18 5.4・4.2): 所在単位reduce済みfile_writesをuuidでgroupingし
/// locationsを組立(並びはsource_path辞書順)。entityのtitleはentity_title導出=
/// uuid一致file_write群のうちrecorded_at最大(同形式文字列比較・同値ならVec後方=後勝ち)の
/// title。同一内容2箇所以上所在は実体1つ(uuid)+locations複数。
#[test]
fn entities_grouped_by_uuid_with_sorted_locations() {
    let Some(fx) = StorageFixture::new("idx-egrp", idx_opts()) else {
        return;
    };
    let p1 = fx.data_root.join("b-dir").join("dup.zip");
    let p2 = fx.data_root.join("a-dir").join("dup.zip");
    write_file(&p1, b"entity-group-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&p1], "group-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    // 同一内容を別pathへ(b-dirより辞書順で先になるa-dir)→ 合流で同一entityに所在2件
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("a-dir作成");
    write_file(&p2, b"entity-group-plain");
    let (code, stdout, stderr) = fx.push(&[&p2], &[], &[]);
    assert_eq!(code, 0, "合流push: {stdout}{stderr}");

    let uuid = idx_uuid_of(&fx, &p1);
    let lines = fx.read_index();
    let same_uuid_entities = lines
        .iter()
        .filter(|v| {
            v.get("record_type").and_then(|r| r.as_str()) == Some("file")
                && v.get("uuid").and_then(|u| u.as_str()) == Some(uuid.as_str())
        })
        .count();
    assert_eq!(same_uuid_entities, 1, "同一uuidのentityは1件(grouping)");
    let entry = index_entity_by(&lines, "uuid", &uuid);
    let locations = entity_locations(entry);
    assert_eq!(locations.len(), 2, "所在2件: {locations:?}");
    let paths: Vec<&str> = locations
        .iter()
        .filter_map(|l| l.get("source_path").and_then(|p| p.as_str()))
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(
        paths, sorted,
        "locationsはsource_path辞書順(冪等担保): {locations:?}"
    );

    // 同一recorded_atのfile_write 2件(記録順で後方が新title)→ Vec後方が勝つ
    let mut cloned = fx
        .registry_values()
        .into_iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid.as_str())
        })
        .last()
        .expect("直近のfile_write記録")
        .clone();
    cloned["title"] = serde_json::json!("group-title-2");
    append_registry_line(&fx.registry, &cloned);
    // all-skip push(run末尾の索引再構築で反映)
    let (code, stdout, stderr) = fx.push(&[&p1], &[], &[]);
    assert_eq!(code, 0, "all-skip再構築push: {stdout}{stderr}");
    let lines_after = fx.read_index();
    let entry2 = index_entity_by(&lines_after, "uuid", &uuid);
    assert_eq!(
        entry2.get("title").and_then(|t| t.as_str()),
        Some("group-title-2"),
        "同一recorded_atはVec後方(後勝ち)のtitle: {entry2}"
    );
}

/// [covers:index.location-remove-excludes-and-revives]
/// location_removeの反映規則(TASK-18 3.2・6.2): build_entriesでfile_write(w)を除外する
/// 条件は「同identity_hashのlocation_remove(lr)が存在し lr.recorded_at >= w.recorded_at」。
/// 除去後に同pathを再backup(mtime変化でfile_write新記録・時刻が後)すると所在が復活する。
#[test]
fn location_remove_excludes_and_revives_on_rebackup() {
    let Some(fx) = StorageFixture::new("idx-locrm", idx_opts()) else {
        return;
    };
    let p1 = fx.data_root.join("loc1").join("a.zip");
    let p2 = fx.data_root.join("loc2").join("a.zip");
    std::fs::create_dir_all(&p1.parent().expect("parent")).expect("loc1");
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("loc2");
    write_file(&p1, b"location-remove-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&p1], "loc-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    write_file(&p2, b"location-remove-plain");
    let (code, stdout, stderr) = fx.push(&[&p2], &[], &[]);
    assert_eq!(code, 0, "合流push(所在2件): {stdout}{stderr}");
    let uuid = idx_uuid_of(&fx, &p1);

    // location_remove記録(p1側)を直接追記し索引を再構築(all-skip push)。
    // recorded_atは既存file_writeの記録時刻(除外条件 lr.recorded_at >= w.recorded_at が
    // 既存記録に対して成立し、後から再backupする新file_write(recorded_atが後)には
    // 負ける値・設計3.2)
    let p1_write_at = fx
        .registry_values()
        .into_iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("source_path").and_then(|p| p.as_str())
                    == Some(p1.canonicalize().unwrap().to_string_lossy().as_ref())
        })
        .last()
        .and_then(|r| {
            r.get("recorded_at")
                .and_then(|a| a.as_str())
                .map(String::from)
        })
        .expect("p1のfile_write記録");
    let lr = serde_json::json!({
        "schema": 1,
        "record_type": "location_remove",
        "recorded_at": p1_write_at,
        "uuid": uuid,
        "identity_hash": hash8_of_path(&p1),
        "source_path": p1.canonicalize().unwrap().to_string_lossy(),
    });
    append_registry_line(&fx.registry, &lr);
    let (code, stdout, stderr) = fx.push(&[&p2], &[], &[]);
    assert_eq!(code, 0, "再構築push: {stdout}{stderr}");
    let locations = entity_locations(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert_eq!(
        locations.len(),
        1,
        "location_removeで当該pathの所在が除外: {locations:?}"
    );
    assert!(
        locations
            .iter()
            .all(|l| l.get("source_path").and_then(|p| p.as_str())
                != Some(p1.canonicalize().unwrap().to_string_lossy().as_ref())),
        "除外されるのはp1側: {locations:?}"
    );

    // 除去後に同path(p1)を再backup(mtimeを変える=recorded_atが後の新file_write)で復活
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&p1, b"location-remove-plain");
    let (code, stdout, stderr) = fx.push(&[&p1], &[], &[]);
    assert_eq!(code, 0, "再backup(合流): {stdout}{stderr}");
    let locations = entity_locations(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert_eq!(
        locations.len(),
        2,
        "新file_writeが勝って所在が復活: {locations:?}"
    );
}

/// [covers:index.swept-versions-not-revived]
/// sweep記録(TASK-18 3.2・6.3)に含まれる{s3_key, version_id}は、以降の索引再構築
/// (backup push等のbuild_entries)・manage index rebuildのいずれでもversions/pinsへ
/// 復活しない(prune除外と同じ除外集合。reconcileの効果がrebuildで失われるのを防ぐ)。
#[test]
fn swept_versions_not_revived_on_rebuilds() {
    let Some(fx) = StorageFixture::new("idx-sweep", idx_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"swept-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "sweep-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    idx_add_second_version(&fx, &a);
    idx_add_second_version(&fx, &a);
    let uuid = idx_uuid_of(&fx, &a);
    let key = fx.data_key(&uuid);
    let reg_ids = registry_upload_version_ids(&fx.registry_values(), &key);
    assert_eq!(reg_ids.len(), 3, "3版: {reg_ids:?}");
    let swept = &reg_ids[0];

    // sweep記録(manage index reconcileの実行記録相当)を直接追記
    let sweep = serde_json::json!({
        "schema": 1,
        "record_type": "sweep",
        "recorded_at": "2999-01-01T00:00:00+09:00",
        "reason": "lifecycle",
        "deleted": [{"s3_key": key, "version_id": swept}],
    });
    append_registry_line(&fx.registry, &sweep);

    // (1) backup push(all-skip)の索引再構築で復活しない
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "all-skip push: {stdout}{stderr}");
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert!(
        !vids.contains(swept),
        "sweep済みversion_idはversionsへ復活しない: {vids:?}"
    );
    // 幽霊版はget --version-idで選べない(対象エラー3)
    let out = fx.elsewhere.join("swept-out");
    let (code, _, _) = fx.get(
        "sweep-title",
        &["--version-id", swept, "--out", &out.display().to_string()],
    );
    assert_eq!(code, 3, "sweep済みversion_idの選択は対象エラー3");

    // (2) manage index rebuildでも復活しない
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    let mut last = String::new();
    let mut code = 0;
    for _ in 0..2 {
        let (c, stdout, stderr) = fx.manage("index", &["rebuild"]);
        code = c;
        last = format!("{stdout}{stderr}");
        if fx.object_path(&fx.index_key()).is_file() {
            break;
        }
    }
    assert_eq!(code, 0, "manage index rebuildはexit 0: {last}");
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert!(
        !vids.contains(swept),
        "rebuild後もsweep済み版は復活しない: {vids:?}"
    );
}

/// [covers:index.pins-connect-by-uuid]
/// 索引pins[]への接続はuuid照合のみ(TASK-18 5.4): pin_writeのuuidがentityのuuidと一致
/// すれば接続される(旧name+category+所在記録値+folder完全一致の照合は廃止。
/// 所在記録値(label)の変化があっても同一uuidなら接続)。
/// bucket一致規則・並びのs3_key sort・pin_remove反映は維持。
#[test]
fn pins_connect_by_uuid_across_location_changes() {
    let Some(fx) = StorageFixture::new("idx-pinuuid", idx_opts()) else {
        return;
    };
    let p1 = fx.data_root.join("sub1").join("a.zip");
    std::fs::create_dir_all(&p1.parent().expect("parent")).expect("sub1");
    write_file(&p1, b"pin-uuid-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&p1], "pin-uuid-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = idx_uuid_of(&fx, &p1);
    assert_eq!(
        fx.pin("add", "pin-uuid-title", &["--latest", "--as", "pm1"])
            .0,
        0
    );

    // 所在を移動(label=sub1→sub2へ変化)して合流: 旧照合(name+category+所在記録値)なら
    // pinが外れる構成だが、uuid照合なら接続が維持される
    let p2 = fx.data_root.join("sub2").join("a.zip");
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("sub2");
    write_file(&p2, b"pin-uuid-plain");
    let (code, stdout, stderr) = fx.push(&[&p2], &[], &[]);
    assert_eq!(code, 0, "合流push: {stdout}{stderr}");

    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "uuid", &uuid);
    let pins = entity_pins(entry);
    assert_eq!(
        pins.len(),
        1,
        "所在記録値が変化してもpins[]はuuid照合で接続: {pins:?}"
    );
    assert_eq!(pins[0].get("pin").and_then(|v| v.as_str()), Some("pm1"));
    assert_eq!(
        pins[0].get("s3_key").and_then(|v| v.as_str()),
        Some(fx.data_pin_key(&uuid, "pm1").as_str()),
        "pinのs3_keyはdata/<uuid>/<pin>.age"
    );
    // get --pinで参照できる
    let out = fx.elsewhere.join("pinuuid-out");
    let (code, stdout, stderr) = fx.get(
        "pin-uuid-title",
        &["--pin", "pm1", "--out", &out.display().to_string()],
    );
    assert_eq!(code, 0, "get --pin pm1はexit 0: {stdout}{stderr}");
}

/// [covers:index.location-rm-command]
/// manage location rm(TASK-18 6.2・新設): 索引からsource_path完全一致の所在を探し
/// (--uuid指定でentity絞込)、不在は対象エラー3。実行はlocation_remove記録(registry)→
/// 索引再構築+upload→当該entityのmetadata sync。S3・実体(default.age・pin object)には
/// 触れない。再backupすると所在が復活する。
#[test]
fn manage_location_rm_updates_index_and_metadata() {
    let Some(fx) = StorageFixture::new("idx-loccmd", idx_opts()) else {
        return;
    };
    let p1 = fx.data_root.join("loc1").join("a.zip");
    let p2 = fx.data_root.join("loc2").join("a.zip");
    std::fs::create_dir_all(&p1.parent().expect("parent")).expect("loc1");
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("loc2");
    write_file(&p1, b"location-rm-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&p1], "loc-rm-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    write_file(&p2, b"location-rm-plain");
    let (code, stdout, stderr) = fx.push(&[&p2], &[], &[]);
    assert_eq!(code, 0, "合流push(所在2件): {stdout}{stderr}");
    let uuid = idx_uuid_of(&fx, &p1);
    clear_calls(&fx);

    // 不在pathは対象エラー3
    let (code, _, stderr) = fx.manage("location", &["rm", "/tmp/ghost-location/a.zip"]);
    assert_eq!(code, 3, "不在pathのlocation rmは対象エラー3: {stderr}");

    // 2所在を持つentityからp1を除去
    let p1_real = p1.canonicalize().unwrap().to_string_lossy().into_owned();
    let (code, stdout, stderr) = fx.manage("location", &["rm", &p1_real]);
    assert_eq!(code, 0, "location rmはexit 0: {stdout}{stderr}");
    let locations = entity_locations(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert_eq!(
        locations.len(),
        1,
        "rm後の索引locationsから当該pathが除外: {locations:?}"
    );
    // location_remove記録がregistryへ追記される
    assert_eq!(
        registry_count(&fx.registry_values(), "location_remove"),
        1,
        "location_remove記録"
    );
    // metadataのlatest_source_pathが残所在(p2)へ更新される
    let doc = fx
        .read_metadata_doc(&uuid, "loc-rm-title")
        .expect("metadata doc");
    assert_eq!(
        doc.get("latest_source_path"),
        Some(&serde_json::json!(
            p2.canonicalize().unwrap().to_string_lossy().into_owned()
        )),
        "latest_source_pathは残所在へ更新: {doc}"
    );
    // S3の実体(default.age)には触れない(呼び出し記録にdata keyへの変更系APIが無い)
    let data_key = fx.data_key(&uuid);
    let calls = fx.calls();
    assert!(
        !calls.lines().any(|l| {
            (l.starts_with("s3 PutObject")
                || l.starts_with("s3 CopyObject")
                || l.starts_with("s3 DeleteObject"))
                && l.contains(&format!("key={data_key}"))
        }),
        "default.ageへの変更系APIは無い: {calls}"
    );
    assert!(
        fx.object_path(&data_key).is_file(),
        "default.age objectは不変(存在維持)"
    );

    // 1所在のみのentityからのrmはlocationsが空になる
    let solo = fx.data_root.join("solo").join("s.zip");
    std::fs::create_dir_all(&solo.parent().expect("parent")).expect("solo");
    write_file(&solo, b"location-rm-solo");
    let list = write_from_list(
        fx.cfg.parent().expect("tmp dir"),
        &[(&solo, Some("solo-title"))],
    );
    let Some((code, stdout)) = push_titled_pty(
        &fx,
        &[&p2],
        "",
        &["--from-list", &list.display().to_string()],
    ) else {
        return;
    };
    assert_eq!(code, 0, "soloの発行push: {stdout}");
    let solo_uuid = idx_uuid_of(&fx, &solo);
    let solo_real = solo.canonicalize().unwrap().to_string_lossy().into_owned();
    let (code, stdout, stderr) = fx.manage("location", &["rm", &solo_real, "--uuid", &solo_uuid]);
    assert_eq!(code, 0, "--uuid付きのlocation rmはexit 0: {stdout}{stderr}");
    let locations = entity_locations(index_entity_by(&fx.read_index(), "uuid", &solo_uuid));
    assert!(
        locations.is_empty(),
        "1所在のみのentityはlocationsが空: {locations:?}"
    );

    // 再backup(mtime変化)で所在が復活する
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&p1, b"location-rm-plain");
    let (code, stdout, stderr) = fx.push(&[&p1], &[], &[]);
    assert_eq!(code, 0, "再backup: {stdout}{stderr}");
    let locations = entity_locations(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert_eq!(locations.len(), 2, "再backupで所在が復活: {locations:?}");
}

/// [covers:index.reconcile-swept-versions]
/// manage index reconcile(TASK-18 6.3・新設): 既定dry-run(件数表示)・--applyで実行。
/// read_index → ListObjectVersions(bucket, {prefix}data/)のpresent集合と突合し、
/// 索引versions[]のうちS3に存在しないもの(lifecycle消滅版)を特定 → sweep記録(registry)→
/// 索引再構築+upload。apply後のmanage index rebuildでも当該版は復活しない。
#[test]
fn manage_index_reconcile_removes_vanished_versions() {
    let Some(fx) = StorageFixture::new("idx-recon", idx_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"reconcile-plain");
    let Some((code, stdout)) = idx_push_titled(&fx, &[&a], "reconcile-title") else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    idx_add_second_version(&fx, &a);
    let uuid = idx_uuid_of(&fx, &a);
    let key = fx.data_key(&uuid);
    let reg_ids = registry_upload_version_ids(&fx.registry_values(), &key);
    assert_eq!(reg_ids.len(), 2, "2版: {reg_ids:?}");
    let vanished = &reg_ids[0];

    // 旧版がlifecycleで消滅した状態を模擬(versions stateから版行を除去)
    drop_version_state(&fx.state, &key, vanished);
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert!(
        vids.contains(vanished),
        "模擬直後の索引には消滅版が残っている前提: {vids:?}"
    );
    clear_calls(&fx);

    // dry-run(既定・--applyなし): 件数表示のみで索引不変・S3削除なし
    let (code, stdout, stderr) = fx.manage("index", &["reconcile"]);
    assert_eq!(code, 0, "reconcile(dry-run)はexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("1") && (stdout.contains("件") || stdout.contains("dry-run")),
        "消滅版の件数表示: {stdout}"
    );
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert!(vids.contains(vanished), "dry-runでは索引は不変: {vids:?}");
    assert!(
        !fx.calls().lines().any(|l| l.starts_with("s3 DeleteObject")),
        "dry-runではS3削除は行わない: {}",
        fx.calls()
    );
    assert_eq!(
        registry_count(&fx.registry_values(), "sweep"),
        0,
        "dry-runではsweep記録なし"
    );

    // --apply: 索引versionsから除去されsweep記録がregistryへ追記される
    let (code, stdout, stderr) = fx.manage("index", &["reconcile", "--apply"]);
    assert_eq!(code, 0, "reconcile --applyはexit 0: {stdout}{stderr}");
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert!(
        !vids.contains(vanished),
        "applyで索引versionsから消滅版が除去: {vids:?}"
    );
    assert_eq!(
        registry_count(&fx.registry_values(), "sweep"),
        1,
        "sweep記録がregistryへ追記される"
    );

    // apply後のmanage index rebuildでも当該版は復活しない
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    let mut last = String::new();
    let mut code = 0;
    for _ in 0..2 {
        let (c, stdout, stderr) = fx.manage("index", &["rebuild"]);
        code = c;
        last = format!("{stdout}{stderr}");
        if fx.object_path(&fx.index_key()).is_file() {
            break;
        }
    }
    assert_eq!(code, 0, "manage index rebuildはexit 0: {last}");
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert!(
        !vids.contains(vanished),
        "apply後のrebuildでも消滅版は復活しない: {vids:?}"
    );
}
