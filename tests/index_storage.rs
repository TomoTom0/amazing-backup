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

/// [covers:index.schema-and-serialization]
/// 索引JSONL schema(設計2.1): index_header(schema=1・updated_at・run_id・tool_version・
/// entries=N)+file entry(name・category・rel_path・source_path・identity_hash・versions[]・
/// pins[])。versionsはdate降順(先頭=最新)で各versionはdate・version_ts・batch_id・s3_key・
/// version_id・source_sha256・age_sha256・source_size・age_sizeを持ち、pins[]はpin・s3_key・
/// created_at・date・source_sha256・age_sha256・source_sizeを持つ。
/// identity_hash=sha256(realpath)先頭8hex。次コマンドでの索引parseが全fieldを保つ。
#[test]
fn index_jsonl_schema_and_field_roundtrip() {
    let Some(fx) = StorageFixture::new("idx-schema", StorageOpts::default()) else {
        return;
    };
    let a = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("travel.zip");
    write_file(&a, b"index-schema-v1");
    let b = fx.elsewhere.join("x.zip");
    write_file(&b, b"index-schema-b");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    write_file(&a, b"index-schema-v2");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    assert_eq!(fx.push(&[&b], &[], &[]).0, 0);
    assert_eq!(
        fx.pin("add", "travel.zip", &["--latest", "--as", "pre-migration"])
            .0,
        0
    );

    let lines = fx.read_index();
    let header = index_header(&lines);
    assert_eq!(
        header.get("schema").and_then(|v| v.as_i64()),
        Some(1),
        "schema=1: {header}"
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
        "entries=Nはfile entry数と一致"
    );

    let entry = index_file_entry(&lines, "travel.zip");
    for field in [
        "name",
        "category",
        "rel_path",
        "source_path",
        "identity_hash",
        "versions",
        "pins",
    ] {
        assert!(
            entry.get(field).is_some(),
            "file entryに{field}がある: {entry}"
        );
    }
    assert_eq!(
        entry.get("rel_path").and_then(|v| v.as_str()),
        Some("2020-2023/travel"),
        "rel_pathはauto相対化値"
    );
    assert_eq!(
        entry.get("identity_hash").and_then(|v| v.as_str()),
        Some(hash8_of_path(&a).as_str()),
        "identity_hash=sha256(realpath)先頭8hex"
    );

    let versions = entry_versions(entry);
    assert_eq!(versions.len(), 2, "travel.zipは2版: {versions:?}");
    let reg_ids = registry_upload_version_ids(
        &fx.registry_values(),
        &fx.file_key("unfiled", Some("2020-2023/travel"), "travel.zip"),
    );
    assert_eq!(reg_ids.len(), 2, "registryのfile_upload記録は2件");
    // versionsは新しい順(先頭=最新=最後のpush)
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
    }

    let pins = entry
        .get("pins")
        .and_then(|p| p.as_array())
        .expect("pins配列");
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
    let (code, stdout, stderr) = fx.list("versions", &["--name", "travel.zip"]);
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
#[test]
fn index_rebuilt_from_registry_each_run() {
    let Some(fx) = StorageFixture::new("idx-rebuild", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.elsewhere.join("b.zip");
    let a_key = fx.file_key("unfiled", None, "a.zip");
    write_file(&a, b"rebuild-a-v1");
    write_file(&b, b"rebuild-b");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    write_file(&a, b"rebuild-a-v2");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    assert_eq!(fx.push(&[&b], &[], &[]).0, 0);

    let lines = fx.read_index();
    let entry = index_file_entry(&lines, "a.zip");
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
        index_file_entry(&lines, "b.zip").get("versions").is_some(),
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
    let entry2 = index_file_entry(&lines2, "a.zip");
    assert_eq!(
        entry_versions(entry2),
        &versions_before,
        "再構築後のversionsは1回目と同一内容(冪等)"
    );
    let pins2 = entry2
        .get("pins")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
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
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "DEEP_ARCHIVE profileでのpush: {stdout}{stderr}");
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
    // file objectはprofile値(DEEP_ARCHIVE)のまま
    let file_key = fx.file_key("unfiled", None, "a.zip");
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
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "2鍵構成でのpush: {stdout}{stderr}");

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
    let (code, stdout, stderr) = fx.push(&[&small, &big], &[], &[]);
    assert_eq!(code, 0, "単発+multipartのpush: {stdout}{stderr}");

    let calls = fx.calls();
    let big_key = fx.file_key("unfiled", None, "big.zip");
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
        ("small.zip", fx.file_key("unfiled", None, "small.zip")),
        ("big.zip", big_key),
    ] {
        let entry = index_file_entry(&lines, name);
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
/// manage index rebuild(設計2.3): (1)ListObjectsV2でmanifest一覧 (2)未復元のmanifestへ
/// 復元job発行(Bulk既定・進行中は再実行で継続) (3)取得・復号・parse→file recordから
/// entry再構築 (4)version_id欠損manifestはListObjectVersionsでkey+LastModifiedから補完
/// (5)sys/index.jsonl.ageへPUT。開始時にtier別単価で区別した費用・所要の概算を表示。
#[test]
fn manage_index_rebuild_rebuilds_entries_from_manifests() {
    let Some(fx) = StorageFixture::new(
        "idx-mreb",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"rebuild-manifests-v1");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    write_file(&a, b"rebuild-manifests-v2");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let reg_ids = registry_upload_version_ids(
        &fx.registry_values(),
        &fx.file_key("unfiled", None, "a.zip"),
    );
    assert_eq!(reg_ids.len(), 2, "2 run分のfile_upload記録");

    // 1件のmanifestを旧式(version_id欠損)へ書き換える
    let manifests = manifest_objects(&fx);
    assert_eq!(manifests.len(), 2, "manifestは2件: {manifests:?}");
    let old_manifest = manifests[0].clone();
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
    // 開始時のtier別費用・所要の概算表示
    assert!(
        all_stdout.contains("Bulk") && all_stdout.contains("Standard"),
        "tier別(Standard/Bulk)の費用・所要概算表示: {all_stdout}"
    );

    // 再構築された索引: 2版ともversion_idが入る(欠損分はListObjectVersionsで補完)
    let lines = fx.read_index();
    let entry = index_file_entry(&lines, "a.zip");
    let mut vids = index_version_ids(entry);
    vids.sort();
    let mut expected = reg_ids.clone();
    expected.sort();
    assert_eq!(
        vids, expected,
        "再構築されたversion_idはmanifest記録値+補完値: {entry}"
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    assert_eq!(
        fx.pin("add", "a.zip", &["--latest", "--as", "pre-migration"])
            .0,
        0
    );

    let lines = fx.read_index();
    let entry = index_file_entry(&lines, "a.zip");
    let pins = entry
        .get("pins")
        .and_then(|p| p.as_array())
        .expect("pins配列");
    assert_eq!(pins.len(), 1, "pin add後のpinsは1件: {pins:?}");
    let p = &pins[0];
    assert_eq!(p.get("pin").and_then(|v| v.as_str()), Some("pre-migration"));
    let pin_key = fx.pin_key("unfiled", None, "a.zip", "pre-migration");
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
    let entry2 = index_file_entry(&lines2, "a.zip");
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

/// [covers:index.rebuild-preserves-folder-and-pins]
/// manage index rebuildはfolderを落とさず(H-3: manifest V2のfolder記録を使用)、
/// registryのpin記録からpins[]を復元する。rebuild後のpin addが正しいpin keyへ写る。
#[test]
fn rebuild_preserves_folder_and_restores_pins() {
    let Some(fx) = StorageFixture::new("idx-rebfolder", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"rebuild-folder");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    assert_eq!(fx.pin("add", "a.zip", &["--latest", "--as", "pm1"]).0, 0);

    // 索引objectを喪失させて再構築
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
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

    // pins[]が復元されている(registryのpin_write由来)
    let lines = fx.read_index();
    let entry = index_file_entry(&lines, "a.zip");
    let pins = entry
        .get("pins")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(pins.len(), 1, "pm1が復元される: {pins:?}");
    assert_eq!(pins[0].get("pin").and_then(|v| v.as_str()), Some("pm1"));

    // rebuild後のpin addがfolder付きの正しいpin keyへ写る(二重/なし)
    assert_eq!(fx.pin("add", "a.zip", &["--latest", "--as", "pm2"]).0, 0);
    let pm2_key = fx.pin_key("unfiled", None, "a.zip", "pm2");
    assert!(
        fx.object_path(&pm2_key).is_file(),
        "rebuild後のpin addが正しいkeyへ写る: {pm2_key}"
    );
    let lines2 = fx.read_index();
    let entry2 = index_file_entry(&lines2, "a.zip");
    let pins2 = entry2
        .get("pins")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(pins2.len(), 2, "pm1・pm2の2件: {pins2:?}");
}

/// [covers:index.version-ts-from-file-write]
/// 索引versions[].version_tsの時刻源は暗号化時刻(TASK-11 Phase B): registryの当該版
/// file_write記録recorded_atと一致する(file_upload記録の時刻ではなく・file_writeと同一時刻)。
/// 同一日内複数版の一意化(getの--version-id特定・候補表示)はこの時刻で行う。
#[test]
fn version_ts_comes_from_file_write_recorded_at() {
    let Some(fx) = StorageFixture::new("idx-vts", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"version-ts-v1");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    // 同一日内2版目(mtimeを変えるためsleep)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&a, b"version-ts-v2-longer");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);

    // registryのfile_write記録(recorded_at)と索引versions[].version_tsの対応
    let writes: Vec<serde_json::Value> = fx
        .registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .collect();
    assert_eq!(writes.len(), 2, "2版分のfile_write記録: {writes:?}");
    let lines = fx.read_index();
    let entry = index_file_entry(&lines, "a.zip");
    let versions = entry_versions(entry);
    assert_eq!(versions.len(), 2, "索引は2版: {versions:?}");
    for v in versions.iter() {
        let vts = v
            .get("version_ts")
            .and_then(|t| t.as_str())
            .expect("version_ts");
        assert!(
            writes
                .iter()
                .any(|w| w["recorded_at"].as_str() == Some(vts)),
            "version_ts({vts})は当該版file_write記録のrecorded_atと一致: {writes:?}"
        );
    }
    // file_writeとfile_uploadの記録時刻は独立に記録されるが、version_tsは
    // file_write側(暗号化時刻)に等しい(全version_tsがいずれかのfile_write記録に対応することで検証)
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);

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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    // pin add(pm1)→pin rm(pm1)→同一pm1再付与(write-onceの正規フロー)
    assert_eq!(fx.pin("add", "a.zip", &["--latest", "--as", "pm1"]).0, 0);
    assert_eq!(fx.pin("rm", "a.zip", &["--pin", "pm1"]).0, 0);
    let (code, stdout, stderr) = fx.pin("add", "a.zip", &["--latest", "--as", "pm1"]);
    assert_eq!(
        code, 0,
        "rm後の同一pin名再付与はwrite-once違反にならずexit 0: {stdout}{stderr}"
    );
    let pin_key = fx.pin_key("unfiled", None, "a.zip", "pm1");
    assert!(
        fx.object_path(&pin_key).is_file(),
        "再付与でpin objectが再作成される: {pin_key}"
    );

    // 索引当該entry pins[]へpm1が載る(created_atは再付与のpin_write記録時刻)
    let lines = fx.read_index();
    let entry = index_file_entry(&lines, "a.zip");
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
    let entry2 = index_file_entry(&lines2, "a.zip");
    let pins2 = entry2
        .get("pins")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
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
        "[aws]\nactive_profile = \"dev\"\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"bucket-dev\"\nregion = \"ap-northeast-1\"\nprefix = \"backup-remote/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n\n[aws.profiles.prod]\npurpose = \"prod\"\nbucket = \"bucket-prod\"\nregion = \"ap-northeast-1\"\nprefix = \"backup-remote/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{creds}\"\n",
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0, "dev(既定profile) push");
    assert_eq!(
        fx.pin("add", "a.zip", &["--latest", "--as", "pm1"]).0,
        0,
        "dev側(先付け)pin add pm1"
    );
    assert_eq!(
        fx.push(&[&a], &["--profile", "prod"], &[]).0,
        0,
        "prod側push(prod bucketへfile_upload記録)"
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
    let prod_created_at =
        chrono::DateTime::parse_from_rfc3339(&dev_created_at)
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
    let pin_key = fx.pin_key("unfiled", None, "a.zip", "pm1");
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
    let pins = entry_pins(index_file_entry(&fx.read_index(), "a.zip"));
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
    assert!(stdout.contains("pm1"), "list pins(dev)にpm1が現れる: {stdout}");
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
    let pins = entry_pins(index_file_entry(&fx.read_index(), "a.zip"));
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
    let pins = entry_pins(index_file_entry(&fx.read_index(), "a.zip"));
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
        calls.lines().any(|l| l.starts_with("s3 DeleteObjectVersion")
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
    let pins = entry_pins(index_file_entry(&fx.read_index(), "a.zip"));
    assert!(pins.is_empty(), "dev側索引pins[]からpm1が除かれる: {pins:?}");

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
    let pins = entry_pins(index_file_entry(&fx.read_index(), "a.zip"));
    assert_eq!(
        pins.len(),
        1,
        "prod側索引pins[]はpm1を保持する: {pins:?}"
    );
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
    let (code, stdout, stderr) = fx.pin_env("rm", "a.zip", &["--pin", "pm1", "--profile", "prod"], &[]);
    assert_eq!(code, 0, "prod側pin rmもexit 0: {stdout}{stderr}");
    let calls = fx.calls();
    assert!(
        calls.lines().any(|l| l.starts_with("s3 DeleteObjectVersion")
            && l.contains("bucket=bucket-prod")
            && l.contains(&format!("key={}", st.pin_key))),
        "prod側rmのpin object削除はprod bucketへ向かう: {calls}"
    );
    assert!(
        !calls.lines().any(|l| l.contains("bucket=bucket-dev")),
        "prod側rmのS3 API呼び出しにdev bucketは無い: {calls}"
    );
    let pins = entry_pins(index_file_entry(&fx.read_index(), "a.zip"));
    assert!(
        pins.is_empty(),
        "prod側索引pins[]からpm1が除かれる: {pins:?}"
    );
}

/// [covers:index.parse-failure-guides-rebuild]
/// 索引parse失敗(schema違反JSON)のerror文には`manage index rebuild`による索引再構築の
/// 案内を含める(L-6。終了コードは検証エラー4のまま)。
#[test]
fn parse_failure_error_guides_index_rebuild() {
    let Some(fx) = StorageFixture::new("idx-parseguide", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"parse-guide");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);

    // 索引objectをschema違反へ書き換える(file entryから必須field(versions)を除去)
    let mut broken: Vec<serde_json::Value> = fx.read_index();
    let entry = broken
        .iter_mut()
        .find(|v| v.get("record_type").and_then(|r| r.as_str()) == Some("file"))
        .expect("file entry");
    entry
        .as_object_mut()
        .expect("file entryはobject")
        .remove("versions");
    fx.rewrite_index(&broken);

    let (code, _, stderr) = fx.list("files", &[]);
    assert_eq!(code, 4, "schema違反索引は検証エラー4: {stderr}");
    assert!(
        stderr.contains("manage index rebuild"),
        "manage index rebuildの案内: {stderr}"
    );
}
