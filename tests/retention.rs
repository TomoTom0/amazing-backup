//! retention エリアの結合test(TASK-10 S3ストレージ設計・tmp/tasks/tm-10/design_s3_storage.md 3.2・5.2)。
//! manage lifecycle apply・manage pruneをfake seam(BACKUP_REMOTE_FAKE_S3_STATE)の
//! bin subprocessで検証する(テスト先行のため実装前の実行failは許容・--no-runは通る)。

mod common;

use common::*;

/// 同一fileを内容を変えながらn回pushし(n版のversioning模擬)、registry記録順
/// (古い順)のversion_id列を返す。
fn push_versions(fx: &StorageFixture, n: usize) -> Vec<String> {
    let a = fx.data_root.join("a.zip");
    let key = fx.file_key("unfiled", None, "a.zip");
    for i in 0..n {
        write_file(&a, format!("retention-version-{i}").as_bytes());
        let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
        assert_eq!(code, 0, "push({i}): {stdout}{stderr}");
    }
    let ids = registry_upload_version_ids(&fx.registry_values(), &key);
    assert_eq!(ids.len(), n, "file_upload記録は{n}件: {ids:?}");
    ids
}

/// calls.logを空にする(以降の呼び出しのみを観測する)。
fn clear_calls(fx: &StorageFixture) {
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
}

/// DeleteObjectVersion呼び出しのversion_id列(calls.logから)。
fn deleted_versions(fx: &StorageFixture, key: &str) -> Vec<String> {
    version_ids_in_calls(&fx.calls(), "s3 DeleteObjectVersion", key)
}

/// [covers:retention.lifecycle-rules-per-category]
/// category毎NCVE rule(設計5.2): prefix <prefix>/<category>/へ
/// NoncurrentVersionExpiration {NoncurrentDays: 180, NewerNoncurrentVersions: keep}。
/// sys(予約領域)は対象外・Expiration(現行version削除)・STANDARD→DA移行ruleは設定しない。
/// applyはrule一式を置換(冪等・再実行で同一構成)。
#[test]
fn lifecycle_apply_generates_per_category_ncve_rules() {
    let Some(fx) = StorageFixture::new(
        "ret-life",
        StorageOpts {
            categories: "[categories.main]\nkeep = 5\n[categories.volatile]\nkeep = 2\n".into(),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let (code, stdout1, stderr) = fx.manage("lifecycle", &["apply"]);
    assert_eq!(code, 0, "manage lifecycle applyはexit 0: {stdout1}{stderr}");
    assert!(
        fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 PutBucketLifecycleConfiguration")),
        "PutBucketLifecycleConfiguration呼び出し: {}",
        fx.calls()
    );
    for (category, keep) in [("main", "5"), ("volatile", "2"), ("unfiled", "5")] {
        let prefix_line = format!("backup-remote/{category}/");
        assert!(
            stdout1.contains(&prefix_line),
            "category毎ruleのprefix({prefix_line}): {stdout1}"
        );
        assert!(
            stdout1.contains("NoncurrentDays") && stdout1.contains("180"),
            "NoncurrentDays=180: {stdout1}"
        );
        assert!(
            stdout1.contains("NewerNoncurrentVersions") && stdout1.contains(keep),
            "NewerNoncurrentVersions={keep}({category}): {stdout1}"
        );
    }
    assert!(
        !stdout1.contains("backup-remote/sys/"),
        "sys(索引・manifestの予約領域)はprefix filterで構造的に対象外: {stdout1}"
    );
    assert!(
        !stdout1.contains("Transition") && !stdout1.contains("STANDARD_IA"),
        "STANDARD→DA移行ruleは設定しない: {stdout1}"
    );

    // 再実行は同一構成を置換する(冪等)
    clear_calls(&fx);
    let (code, stdout2, stderr) = fx.manage("lifecycle", &["apply"]);
    assert_eq!(code, 0, "再実行もexit 0: {stdout2}{stderr}");
    assert_eq!(stdout2, stdout1, "再実行は同一構成を置換(冪等)");
    assert_eq!(
        fx.calls()
            .lines()
            .filter(|l| l.starts_with("s3 PutBucketLifecycleConfiguration"))
            .count(),
        1,
        "再実行もrule一式を置換(PutBucketLifecycleConfiguration 1回): {}",
        fx.calls()
    );
}

/// [covers:retention.mpu-abort-rule]
/// MPU abort rule(設計5.2): prefix <prefix>/へAbortIncompleteMultipartUpload 7日。
#[test]
fn lifecycle_applies_mpu_abort_rule() {
    let Some(fx) = StorageFixture::new("ret-mpu", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("lifecycle", &["apply"]);
    assert_eq!(code, 0, "manage lifecycle applyはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("AbortIncompleteMultipartUpload"),
        "MPU abort ruleの表示: {stdout}"
    );
    assert!(stdout.contains("7"), "MPU abortは7日: {stdout}");
    assert!(
        stdout.contains("backup-remote/"),
        "MPU abort ruleのprefix(<prefix>/): {stdout}"
    );
}

/// [covers:retention.prune-category-required]
/// manage pruneは--categoryが必須(設計3.2): 未指定はusage error 64。
#[test]
fn prune_requires_category_option() {
    let Some(fx) = StorageFixture::new("ret-catreq", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"prune-category");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let (code, _, _) = fx.manage("prune", &[]);
    assert_eq!(code, 64, "--categoryなしのmanage pruneはusage error 64");
    let (code, stdout, stderr) = fx.manage("prune", &["--category", "unfiled"]);
    assert_ne!(
        code, 64,
        "--category指定はusage errorとならない: {stdout}{stderr}"
    );
}

/// [covers:retention.prune-dry-run-default]
/// pruneは--dry-runが既定(設計3.2): --applyなき実行(オプション省略・--dry-run明示)は
/// 削除を行わず、索引+ListObjectVersionsから計算した削除対象を報告してexit 0。
#[test]
fn prune_dry_run_is_default_and_reports_without_deleting() {
    let Some(fx) = StorageFixture::new("ret-dry", StorageOpts::default()) else {
        return;
    };
    let key = fx.file_key("unfiled", None, "a.zip");
    let ids = push_versions(&fx, 7);
    assert_eq!(ids.len(), 7);
    clear_calls(&fx);

    for label in ["オプション省略", "--dry-run明示"] {
        let extra: &[&str] = if label == "オプション省略" {
            &["--category", "unfiled"]
        } else {
            &["--category", "unfiled", "--dry-run"]
        };
        let (code, stdout, stderr) = fx.manage("prune", extra);
        assert_eq!(code, 0, "prune({label})はexit 0: {stdout}{stderr}");
        assert!(
            stdout.contains("dry-run") || stdout.contains("[dry-run]"),
            "dry-run報告であることの表示({label}): {stdout}"
        );
        assert!(
            stdout.contains(&key),
            "削除対象の報告に当該keyが含まれる({label}): {stdout}"
        );
    }
    assert!(
        deleted_versions(&fx, &key).is_empty(),
        "dry-runではDeleteObjectVersionは呼ばれない: {}",
        fx.calls()
    );
    assert!(
        fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 ListObjectVersions")),
        "対象計算にListObjectVersions: {}",
        fx.calls()
    );
}

/// [covers:retention.prune-protects-current-and-pins]
/// pruneの対象計算では現行version・pin key(<pin名>.age)は対象外(構造的保護・設計3.2)。
#[test]
fn prune_protects_current_version_and_pin_keys() {
    let Some(fx) = StorageFixture::new("ret-protect", StorageOpts::default()) else {
        return;
    };
    let key = fx.file_key("unfiled", None, "a.zip");
    let ids = push_versions(&fx, 7);
    let current = ids.last().expect("現行version").clone();
    // pinを付与(pin keyは保護対象)
    assert_eq!(
        fx.pin("add", "a.zip", &["--latest", "--as", "pre-migration"])
            .0,
        0
    );
    let pm_key = fx.pin_key("unfiled", None, "a.zip", "pre-migration");
    clear_calls(&fx);

    let (code, stdout, stderr) = fx.manage("prune", &["--category", "unfiled", "--apply"]);
    assert_eq!(code, 0, "prune --applyはexit 0: {stdout}{stderr}");
    let deleted = deleted_versions(&fx, &key);
    // keep=5(設定値): noncurrent 6件のうち境界(5番目に新しい)まで残し最古の1件のみ削除
    assert_eq!(
        deleted,
        vec![ids[0].clone()],
        "削除は最古の1件のみ: {deleted:?}"
    );
    assert!(
        !deleted.contains(&current),
        "現行versionは削除対象外: {deleted:?} (current={current})"
    );
    let pm_deletes = fx
        .calls()
        .lines()
        .any(|l| l.contains("s3 DeleteObject") && l.contains(&format!("key={pm_key}")));
    assert!(!pm_deletes, "pin keyへの削除は行われない: {}", fx.calls());
    assert!(fx.object_path(&pm_key).is_file(), "pin objectは残る");
}

/// [covers:retention.prune-apply-deletes-versions]
/// 削除条件(NCVEとprune共通の単一定義・設計3.2・5.2): 削除対象=同一keyのnoncurrent版を
/// 新しい順に並べたときnewer noncurrent版の数がkeep以上となった版(境界=ちょうどkeep番目は残す)。
/// --apply時はDeleteObjectVersion。--keep Nは当該実行のみ[categories].keepを上書きし
/// 未指定時は設定値(実行後も設定値は不変)。
#[test]
fn prune_apply_deletes_by_newer_noncurrent_count() {
    // (1) keep=5(設定値・未指定): 最古の1件のみ削除。境界は残る。再実行で追加削除なし
    let Some(fx) = StorageFixture::new("ret-apply5", StorageOpts::default()) else {
        return;
    };
    let key = fx.file_key("unfiled", None, "a.zip");
    let ids = push_versions(&fx, 7);
    let body_before = fx.pipeline_body();
    let (code, stdout, stderr) = fx.manage("prune", &["--category", "unfiled", "--apply"]);
    assert_eq!(code, 0, "prune --apply(keep=5)はexit 0: {stdout}{stderr}");
    // noncurrent 6件(id[0]..id[5])を新しい順: id5(新)…id0(最古)。
    // newer noncurrent数が5以上となるのはid0のみ(境界=id1は残る)
    assert_eq!(
        deleted_versions(&fx, &key),
        vec![ids[0].clone()],
        "keep=5は最古の1件のみ削除"
    );
    assert_eq!(
        fx.pipeline_body(),
        body_before,
        "設定ファイルは変更されない"
    );

    let (code, stdout, stderr) = fx.manage("prune", &["--category", "unfiled", "--apply"]);
    assert_eq!(code, 0, "再実行もexit 0: {stdout}{stderr}");
    assert_eq!(
        deleted_versions(&fx, &key),
        vec![ids[0].clone()],
        "keep=5の再実行で追加削除なし(境界=5番目に新しいnoncurrent版は残る)"
    );

    // (2) --keep 2: newer noncurrent数2以上の版(古い4件)が削除される。設定値は不変
    let Some(fx) = StorageFixture::new("ret-apply2", StorageOpts::default()) else {
        return;
    };
    let key = fx.file_key("unfiled", None, "a.zip");
    let ids = push_versions(&fx, 7);
    let body_before = fx.pipeline_body();
    let (code, stdout, stderr) = fx.manage(
        "prune",
        &["--category", "unfiled", "--apply", "--keep", "2"],
    );
    assert_eq!(code, 0, "prune --apply --keep 2はexit 0: {stdout}{stderr}");
    let mut deleted = deleted_versions(&fx, &key);
    let mut expected: Vec<String> = ids[..4].to_vec();
    deleted.sort();
    expected.sort();
    assert_eq!(
        deleted, expected,
        "--keep 2は古い4件(newer noncurrent数>=2)を削除"
    );
    assert_eq!(
        fx.pipeline_body(),
        body_before,
        "--keepは当該実行のみの上書き(設定値は不変)"
    );
    // 削除はDeleteObjectVersionで行われる(deleted_versionsがcalls.logの
    // DeleteObjectVersion行由来であること自体が検証)
}

/// [covers:index.prune-versions-not-revived-on-rebuild]
/// prune --apply済みの版は、その後のpush/pinでの索引再構築で復活しない(H-1:
/// build_entriesがprune記録で版を除外する。get --version-idで幽霊版を選ばない)。
#[test]
fn prune_deleted_versions_stay_excluded_after_rebuild() {
    let Some(fx) = StorageFixture::new("ret-ghost", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let key = fx.file_key("unfiled", None, "a.zip");
    for i in 0..3 {
        write_file(&a, format!("ghost-version-{i}").as_bytes());
        assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    }
    let ids = registry_upload_version_ids(&fx.registry_values(), &key);
    assert_eq!(ids.len(), 3);
    // keep=1で最古版を削除(v3=現行・noncurrentはv2,v1 → i>=1のv1を削除)
    assert_eq!(
        fx.manage(
            "prune",
            &["--category", "unfiled", "--apply", "--keep", "1"]
        )
        .0,
        0
    );
    let deleted = &ids[0];
    let vids = index_version_ids(index_file_entry(&fx.read_index(), "a.zip"));
    assert!(
        !vids.contains(deleted),
        "prune直後の索引に削除版は無い: {vids:?}"
    );

    // 内容変更のpushで索引が再構築されても削除版は復活しない
    write_file(&a, b"ghost-version-3");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let vids2 = index_version_ids(index_file_entry(&fx.read_index(), "a.zip"));
    assert!(
        !vids2.contains(deleted),
        "再構築後も削除版(幽霊)は復活しない: {vids2:?}"
    );
    // 幽霊版はget --version-idで選べない(対象エラー3)
    let out = fx.elsewhere.join("ghost-out");
    let (code, _, _) = fx.get(
        "a.zip",
        &["--version-id", deleted, "--out", &out.display().to_string()],
    );
    assert_eq!(code, 3, "削除済みversion_idの選択は対象エラー3");
}

/// [covers:retention.lifecycle-rules-per-category]
/// lifecycle applyのfake実payload(lifecycle.json)検証: category毎NCVE rule+
/// MPU abort ruleのみ(Expiration・移行ruleなし・rule総数=category数+1)。
#[test]
fn lifecycle_apply_payload_matches_expected_rules() {
    let Some(fx) = StorageFixture::new(
        "ret-payload",
        StorageOpts {
            categories: "[categories.main]\nkeep = 5\n[categories.volatile]\nkeep = 2\n".into(),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    assert_eq!(fx.manage("lifecycle", &["apply"]).0, 0);
    let payload =
        std::fs::read_to_string(fx.state.join("lifecycle.json")).expect("fake lifecycle.json");
    let specs: Vec<serde_json::Value> =
        serde_json::from_str(&payload).unwrap_or_else(|e| panic!("lifecycle payload parse: {e}"));
    assert_eq!(
        specs.len(),
        4,
        "rule総数=category数(3)+MPU abort(1)・Expiration/移行ruleなし: {payload}"
    );
    for (category, keep) in [("main", 5), ("volatile", 2), ("unfiled", 5)] {
        let expected = serde_json::json!({
            "Noncurrent": {
                "prefix": format!("backup-remote/{category}/"),
                "noncurrent_days": 180,
                "newer_noncurrent_versions": keep,
            }
        });
        assert!(
            specs.contains(&expected),
            "NCVE rule({category}): 期待={expected} payload={payload}"
        );
    }
    let mpu = serde_json::json!({
        "MpuAbort": { "prefix": "backup-remote/", "days_after_initiation": 7 }
    });
    assert!(specs.contains(&mpu), "MPU abort rule: {payload}");
}
