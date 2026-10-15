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

/// [covers:retention.prune-dry-run-apply-exclusive]
/// manage pruneの--dry-run/--apply併置はusage error 64(L-8・12章レビュー)。
/// 対象計算(ListObjectVersions)より前に判定される。
#[test]
fn prune_rejects_dry_run_and_apply_together() {
    let Some(fx) = StorageFixture::new("ret-excl", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"prune-exclusive");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    clear_calls(&fx);
    let (code, stdout, stderr) =
        fx.manage("prune", &["--category", "unfiled", "--dry-run", "--apply"]);
    assert_eq!(
        code, 64,
        "--dry-run/--apply併置はusage error 64: {stdout}{stderr}"
    );
    assert_eq!(
        fx.calls(),
        "",
        "S3 API呼び出し(ListObjectVersions)は行われない: {}",
        fx.calls()
    );
}

/// [covers:retention.prune-category-guard-early]
/// prune冒頭のcategory guard(M-2・12章レビュー): --categoryの値がcategory名規則違反・
/// 予約領域(sys・旧表記_sys)・pipeline.tomlの[categories]未定義のいずれかの場合は
/// ListObjectVersions等のS3 API呼び出し前に設定エラー2で拒否する。
#[test]
fn prune_category_guard_rejects_before_s3_calls() {
    let Some(fx) = StorageFixture::new("ret-guard", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"prune-guard");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    clear_calls(&fx);
    for category in ["sys", "_sys", "BadName", "ghost"] {
        let (code, stdout, stderr) = fx.manage("prune", &["--category", category, "--apply"]);
        assert_eq!(
            code, 2,
            "--category {category}は設定エラー2: {stdout}{stderr}"
        );
    }
    assert_eq!(
        fx.calls(),
        "",
        "S3 API呼び出し(ListObjectVersions前): {}",
        fx.calls()
    );
}

/// versions state file(versions/<key>)の指定版のlast_modifiedを書き換える
/// (L-4: 文字列辞書順と実時刻が逆転する組のfixture)。
fn rewrite_version_mtimes(fx: &StorageFixture, key: &str, map: &[(String, String)]) {
    let path = fx.state.join("versions").join(key);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("versions state file({})の読込: {e}", path.display()));
    let mut out = String::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut parts: Vec<String> = line.split('\t').map(String::from).collect();
        for (vid, mtime) in map {
            if parts[0] == *vid {
                parts[2] = mtime.clone();
            }
        }
        out.push_str(&parts.join("\t"));
        out.push('\n');
    }
    write_file(&path, out.as_bytes());
}

/// [covers:retention.prune-partial-failure-records-deleted]
/// prune --applyの削除loopは削除成功分をその都度PruneRecordへ記録する(L-2):
/// 削除loop途中の失敗(転送エラー5)で中断しても、それまでの削除成功分の記録が残り
/// (以降の索引再構築で幽霊版が復活しない)、注入なしの再実行で残対象の削除が継続される。
#[test]
fn prune_partial_failure_records_deleted_versions() {
    let Some(fx) = StorageFixture::new("ret-ptlfail", StorageOpts::default()) else {
        return;
    };
    let key = fx.file_key("unfiled", None, "a.zip");
    // 5版(現行1+noncurrent4)。keep=1の削除対象はnoncurrentの古い順に3版
    let ids = push_versions(&fx, 5);

    // 1版目の削除は成功させ2版目以降を失敗させる(FAKE_S3_FAIL_DELVER_FOR=<key>:<ok回数>)。
    // 削除順はnoncurrentの新しい方から(keep=1の削除対象は ids[2]→ids[1]→ids[0] の順)
    let (code, _, stderr) = fx.manage_env(
        "prune",
        &["--category", "unfiled", "--apply", "--keep", "1"],
        &[("FAKE_S3_FAIL_DELVER_FOR", format!("{key}:1").as_str())],
    );
    assert_eq!(code, 5, "削除loop途中の失敗は転送エラー5: {stderr}");

    // 削除成功分(先頭の1版=ids[2])のみがPruneRecordへ記録済み
    let deleted_all: Vec<String> = fx
        .registry_values()
        .iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("prune"))
        .flat_map(|p| {
            p.get("deleted")
                .and_then(|d| d.as_array())
                .cloned()
                .unwrap_or_default()
        })
        .filter_map(|d| {
            d.get("version_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();
    assert_eq!(
        deleted_all,
        vec![ids[2].clone()],
        "部分失敗時は削除成功分のみ記録: {deleted_all:?} (ids={ids:?})"
    );

    // 注入なし再実行: 残りを削除してexit 0(冪等)
    let (code, stdout, stderr) = fx.manage(
        "prune",
        &["--category", "unfiled", "--apply", "--keep", "1"],
    );
    assert_eq!(code, 0, "再実行はexit 0: {stdout}{stderr}");
    let vids = index_version_ids(index_file_entry(&fx.read_index(), "a.zip"));
    for deleted in [&ids[0], &ids[1], &ids[2]] {
        assert!(
            !vids.contains(deleted),
            "削除済み版は索引へ復活しない: {vids:?} (deleted={deleted})"
        );
    }
}

/// [covers:retention.prune-order-uses-epoch]
/// pruneの版順sort(新しい順)はVersionInfo.last_modified_epochの数値比較で行う(L-4):
/// last_modified文字列の辞書順と実時刻が逆転する組(timezone表記違い)でも、
/// newer noncurrent数の数え上げは実時刻順に行われる(実時刻で古い版が削除対象)。
#[test]
fn prune_orders_versions_by_epoch() {
    let Some(fx) = StorageFixture::new("ret-epoch", StorageOpts::default()) else {
        return;
    };
    let key = fx.file_key("unfiled", None, "a.zip");
    // 3版(現行=ids[2]・noncurrent=ids[0](古い)/ids[1](新しい))
    let ids = push_versions(&fx, 3);
    // noncurrent 2版のlast_modifiedを書き換え:
    //   ids[0](実時刻=2026-10-14T16:00:00Z相当): "2026-10-15T01:00:00+09:00"(辞書順は新しい)
    //   ids[1](実時刻=2026-10-14T20:00:00Z相当): "2026-10-14T20:00:00Z"(辞書順は古い)
    // 実時刻ではids[1]が4時間新しい。keep=1なら実時刻で古いids[0]が削除対象。
    rewrite_version_mtimes(
        &fx,
        &key,
        &[
            (ids[0].clone(), "2026-10-15T01:00:00+09:00".to_string()),
            (ids[1].clone(), "2026-10-14T20:00:00Z".to_string()),
        ],
    );
    let (code, stdout, stderr) = fx.manage(
        "prune",
        &["--category", "unfiled", "--apply", "--keep", "1"],
    );
    assert_eq!(code, 0, "prune --apply --keep 1はexit 0: {stdout}{stderr}");
    assert_eq!(
        deleted_versions(&fx, &key),
        vec![ids[0].clone()],
        "実時刻(epoch)順で最古のids[0]が削除される(文字列順ならids[1]が削除される): ids={ids:?}"
    );
}
