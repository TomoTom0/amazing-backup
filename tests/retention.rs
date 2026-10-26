//! retention エリアの結合test(TASK-10 S3ストレージ設計・tmp/tasks/tm-10/design_s3_storage.md 3.2・5.2)。
//! manage lifecycle apply・manage pruneをfake seam(AMAZING_BACKUP_FAKE_S3_STATE)の
//! bin subprocessで検証する(テスト先行のため実装前の実行failは許容・--no-runは通る)。

mod common;

use common::*;

/// 同一entity(a.zip)へn版重ね(n版のversioning模擬)、registry記録順
/// (古い順)のversion_id列を返す。TASK-18では同一entityの複数版はcrash残存
/// re-put経路(設計4.4・各版のfile_upload記録を保持)で構築する。
fn push_versions(fx: &StorageFixture, n: usize) -> Vec<String> {
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"retention-version-plain");
    let Some((code, stdout)) = push_titled_pty(fx, &[&a], "retention-a", &[]) else {
        return Vec::new();
    };
    assert_eq!(code, 0, "初回発行(pty promptでtitle付与): {stdout}");
    let mut ids = Vec::new();
    for i in 1..n {
        ids = add_version_via_reput(fx, &a, &[]);
        assert_eq!(ids.len(), i + 1, "re-put({i})後の版数: {ids:?}");
    }
    if ids.is_empty() {
        let key = fx.data_key(&uuid_of_path(fx, &a));
        ids = registry_upload_version_ids(&fx.registry_values(), &key);
    }
    assert_eq!(ids.len(), n, "file_upload記録は{n}件: {ids:?}");
    ids
}

/// calls.logを空にする(以降の呼び出しのみを観測する)。
fn clear_calls(fx: &StorageFixture) {
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
}

/// TASK-18向けprune系testの共通setup: 1実体(default.age 1 key)へn版重ねた状態を作る。
/// TASK-18では通常runは1実体1版(mtime変化は合流・内容変化は別uuid)のため、
/// crash残存(file_upload記録の除去)→再pushのre-put経路(4.1 2.5)で同一keyへ版を
/// 重ねる(現行実装でもskip不成立=同key再PUTで同様に版が増える)。
/// 戻り値のidsは時系列順(古い方から)のversion_id。titleの初回発行のみprompt。
fn reput_setup(tag: &str, n: usize, keep_versions: u32) -> Option<(StorageFixture, Vec<String>)> {
    let fx = StorageFixture::new(
        tag,
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, keep_versions, 7),
            ..StorageOpts::default()
        },
    )?;
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"retention-reput-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "retention-a", &[]) else {
        return None;
    };
    assert_eq!(code, 0, "初回発行(pty promptでtitle付与): {stdout}");
    let mut ids = vec![last_upload_version_id(&fx)];
    for i in 1..n {
        rewrite_registry_lines(&fx.registry, |r| {
            r.get("record_type").and_then(|t| t.as_str()) != Some("file_upload")
        });
        let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
        assert_eq!(code, 0, "re-put({i}): {stdout}{stderr}");
        ids.push(last_upload_version_id(&fx));
    }
    Some((fx, ids))
}

/// registryの末尾のfile_upload記録のversion_id。
fn last_upload_version_id(fx: &StorageFixture) -> String {
    fx.registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload"))
        .filter_map(|r| {
            r.get("version_id")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .next_back()
        .expect("file_upload記録のversion_id")
}

/// registryのfile_upload記録から実key(schema問わず記録値が正本)。
fn reput_key_of(fx: &StorageFixture) -> String {
    fx.registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload"))
        .find_map(|r| r.get("s3_key").and_then(|k| k.as_str()).map(String::from))
        .expect("file_upload記録のs3_key")
}

/// DeleteObjectVersion呼び出しのversion_id列(calls.logから)。
fn deleted_versions(fx: &StorageFixture, key: &str) -> Vec<String> {
    version_ids_in_calls(&fx.calls(), "s3 DeleteObjectVersion", key)
}

/// [covers:retention.mpu-abort-rule]
/// MPU abort rule(TASK-18 7章): prefix <prefix>/へAbortIncompleteMultipartUpload。
/// 日数はaws.toml [aws.lifecycle]のmpu_abort_days値(ハードコード無し。
/// 設定値9日を与えてコード内default(7)と区別して観測する)。
#[test]
fn lifecycle_applies_mpu_abort_rule() {
    let Some(fx) = StorageFixture::new(
        "ret-mpu",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 9),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("lifecycle", &["apply"]);
    assert_eq!(code, 0, "manage lifecycle applyはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("AbortIncompleteMultipartUpload"),
        "MPU abort ruleの表示: {stdout}"
    );
    assert!(
        stdout.contains("9"),
        "MPU abort日数はaws.toml値(9): {stdout}"
    );
    assert!(
        !stdout.contains("mpu-abort 7") && !stdout.contains("7日"),
        "ハードコード値(7)は使われない: {stdout}"
    );
    assert!(
        stdout.contains("amazing-backup/"),
        "MPU abort ruleのprefix(<prefix>/): {stdout}"
    );
}

/// [covers:retention.prune-dry-run-default]
/// pruneは--dry-runが既定(TASK-18 5.12: {prefix}data/配下一括): --applyなき実行
/// (オプション省略・--dry-run明示)は削除を行わず、ListObjectVersionsから計算した
/// 削除対象を報告してexit 0。
#[test]
fn prune_dry_run_is_default_and_reports_without_deleting() {
    let Some((fx, ids)) = reput_setup("ret-dry", 7, 5) else {
        return;
    };
    assert_eq!(ids.len(), 7);
    let key = reput_key_of(&fx);
    clear_calls(&fx);

    for label in ["オプション省略", "--dry-run明示"] {
        let extra: &[&str] = if label == "オプション省略" {
            &[]
        } else {
            &["--dry-run"]
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
/// pruneの対象計算では現行version・pin key(<pin名>.age)は対象外(構造的保護・
/// TASK-18 5.12: data/一括でも`/default.age`終端のみが対象)。
#[test]
fn prune_protects_current_version_and_pin_keys() {
    let Some(fx) = StorageFixture::new(
        "ret-protect",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 5, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let ids = push_versions(&fx, 7);
    let key = fx.data_key(&uuid_of_path(&fx, &a));
    let current = ids.last().expect("現行version").clone();
    // pinを付与(pin keyは保護対象)
    assert_eq!(
        fx.pin("add", "retention-a", &["--latest", "--as", "pre-migration"])
            .0,
        0
    );
    let pm_key = fx.data_pin_key(&uuid_of_path(&fx, &a), "pre-migration");
    clear_calls(&fx);

    // TASK-18 5.12/6.5: pruneはdata/一括(--category/--keepは廃止・keep=aws.toml値)
    let (code, stdout, stderr) = fx.manage("prune", &["--apply"]);
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
/// 削除条件(NCVEとprune共通の単一定義・TASK-18 5.12・7章): 削除対象=同一keyの
/// noncurrent版を新しい順に並べたときnewer noncurrent版の数がkeep以上となった版
/// (境界=ちょうどkeep番目は残す)。keep値はaws.toml [aws.lifecycle] keep_versions
/// (--keepオプションは廃止・当該実行での上書き不可)。--apply時はDeleteObjectVersion。
#[test]
fn prune_apply_deletes_by_newer_noncurrent_count() {
    // (1) keep_versions=5(aws.toml値): 最古の1件のみ削除。境界は残る。再実行で追加削除なし
    let Some((fx, ids)) = reput_setup("ret-apply5", 7, 5) else {
        return;
    };
    let key = reput_key_of(&fx);
    let body_before = fx.aws_toml_body();
    let (code, stdout, stderr) = fx.manage("prune", &["--apply"]);
    assert_eq!(
        code, 0,
        "prune --apply(keep_versions=5)はexit 0: {stdout}{stderr}"
    );
    // noncurrent 6件(id[0]..id[5])を新しい順: id5(新)…id0(最古)。
    // newer noncurrent数が5以上となるのはid0のみ(境界=id1は残る)
    assert_eq!(
        deleted_versions(&fx, &key),
        vec![ids[0].clone()],
        "keep_versions=5は最古の1件のみ削除"
    );
    assert_eq!(
        fx.aws_toml_body(),
        body_before,
        "設定ファイルは変更されない"
    );

    let (code, stdout, stderr) = fx.manage("prune", &["--apply"]);
    assert_eq!(code, 0, "再実行もexit 0: {stdout}{stderr}");
    assert_eq!(
        deleted_versions(&fx, &key),
        vec![ids[0].clone()],
        "keep_versions=5の再実行で追加削除なし(境界=5番目に新しいnoncurrent版は残る)"
    );

    // (2) keep_versions=2(aws.toml値): newer noncurrent数2以上の版(古い4件)が削除される
    let Some((fx, ids)) = reput_setup("ret-apply2", 7, 2) else {
        return;
    };
    let key = reput_key_of(&fx);
    let (code, stdout, stderr) = fx.manage("prune", &["--apply"]);
    assert_eq!(
        code, 0,
        "prune --apply(keep_versions=2)はexit 0: {stdout}{stderr}"
    );
    let mut deleted = deleted_versions(&fx, &key);
    let mut expected: Vec<String> = ids[..4].to_vec();
    deleted.sort();
    expected.sort();
    assert_eq!(
        deleted, expected,
        "keep_versions=2は古い4件(newer noncurrent数>=2)を削除"
    );
    // 削除はDeleteObjectVersionで行われる(deleted_versionsがcalls.logの
    // DeleteObjectVersion行由来であること自体が検証)
}

/// [covers:index.prune-versions-not-revived-on-rebuild]
/// prune --apply済みの版は、その後のpush/pinでの索引再構築で復活しない(H-1:
/// build_entriesがprune記録で版を除外する。get --version-idで幽霊版を選ばない)。
#[test]
fn prune_deleted_versions_stay_excluded_after_rebuild() {
    let Some(fx) = StorageFixture::new(
        "ret-ghost",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 1, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let ids = push_versions(&fx, 3);
    let key = fx.data_key(&uuid_of_path(&fx, &a));
    assert_eq!(ids.len(), 3);
    // keep_versions=1で最古版を削除(v3=現行・noncurrentはv2,v1 → i>=1のv1を削除)
    assert_eq!(fx.manage("prune", &["--apply"]).0, 0);
    let deleted = &ids[0];
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "title", "retention-a"));
    assert!(
        !vids.contains(deleted),
        "prune直後の索引に削除版は無い: {vids:?}"
    );

    // 索引が再構築されても削除版は復活しない(mtime touch合流runでrun末尾再構築を駆動)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&a, b"retention-version-plain");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let vids2 = index_version_ids(index_entity_by(&fx.read_index(), "title", "retention-a"));
    assert!(
        !vids2.contains(deleted),
        "再構築後も削除版(幽霊)は復活しない: {vids2:?}"
    );
    // 幽霊版はget --version-idで選べない(対象エラー3)
    let out = fx.elsewhere.join("ghost-out");
    let (code, _, _) = fx.get(
        "retention-a",
        &["--version-id", deleted, "--out", &out.display().to_string()],
    );
    assert_eq!(code, 3, "削除済みversion_idの選択は対象エラー3");
}

/// [covers:retention.lifecycle-data-ncve-and-sweep-rules]
/// lifecycleはdata/一括の3rule構成(TASK-18 7章・値はすべてaws.toml [aws.lifecycle]から。
/// ハードコード禁止・承認事項7):
/// 1) data-ncve: Prefix={prefix}data/・NoncurrentVersionExpiration{NoncurrentDays=
///    noncurrent_days, NewerNoncurrentVersions=keep_versions}
/// 2) mpu-abort: Prefix={prefix}・AbortIncompleteMultipartUpload{DaysAfterInitiation=
///    mpu_abort_days}
/// 3) delete-marker-sweep: Prefix={prefix}data/・Expiration{ExpiredObjectDeleteMarker=true}
///    (title変更時の旧metadataキーdelete marker・metadata非現行版の掃除)
/// category毎ruleは廃止。sys/は{prefix}data/外のためNCVE対象外。applyはrule一式を
/// 置換(冪等・再実行で同一構成)。
#[test]
fn lifecycle_apply_generates_data_area_three_rules() {
    let Some(fx) = StorageFixture::new(
        "ret-life3",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
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

    // fake実payload(lifecycle.json)のrule一式検証
    let payload =
        std::fs::read_to_string(fx.state.join("lifecycle.json")).expect("fake lifecycle.json");
    let specs: Vec<serde_json::Value> =
        serde_json::from_str(&payload).unwrap_or_else(|e| panic!("lifecycle payload parse: {e}"));
    assert_eq!(specs.len(), 3, "3rule構成(category毎ruleは廃止): {payload}");
    let ncve = serde_json::json!({
        "Noncurrent": {
            "prefix": "amazing-backup/data/",
            "noncurrent_days": 180,
            "newer_noncurrent_versions": 10,
        }
    });
    assert!(
        specs.contains(&ncve),
        "data-ncve rule: 期待={ncve} payload={payload}"
    );
    let mpu = serde_json::json!({
        "MpuAbort": { "prefix": "amazing-backup/", "days_after_initiation": 7 }
    });
    assert!(
        specs.contains(&mpu),
        "mpu-abort rule: 期待={mpu} payload={payload}"
    );
    // delete-marker-sweep rule: {prefix}data/のExpiredObjectDeleteMarker
    let sweep = specs
        .iter()
        .find(|s| {
            serde_json::to_string(s)
                .unwrap_or_default()
                .contains("DeleteMarker")
        })
        .unwrap_or_else(|| panic!("delete-marker-sweep rule: {payload}"));
    let sweep_s = serde_json::to_string(sweep).unwrap();
    assert!(
        sweep_s.contains("amazing-backup/data/"),
        "delete-marker-sweepのprefix(amazing-backup/data/): {sweep_s}"
    );
    assert!(
        sweep_s.contains("true"),
        "ExpiredObjectDeleteMarker=true: {sweep_s}"
    );

    // stdoutにも3ruleの構成(値はaws.toml値)が現れる
    for want in [
        "amazing-backup/data/",
        "NoncurrentDays",
        "180",
        "NewerNoncurrentVersions",
        "10",
        "AbortIncompleteMultipartUpload",
        "7",
    ] {
        assert!(stdout1.contains(want), "apply表示に{want}: {stdout1}");
    }
    assert!(
        !stdout1.contains("amazing-backup/unfiled/"),
        "category毎ruleのprefixは現れない: {stdout1}"
    );
    assert!(
        !stdout1.contains("amazing-backup/sys/"),
        "sys(索引・manifestの予約領域)はNCVE対象外: {stdout1}"
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
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "prune-excl", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    clear_calls(&fx);
    let (code, stdout, stderr) = fx.manage("prune", &["--dry-run", "--apply"]);
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
    let Some(fx) = StorageFixture::new(
        "ret-ptlfail",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 1, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    // 5版(現行1+noncurrent4)。keep_versions=1の削除対象はnoncurrentの古い順に3版
    let ids = push_versions(&fx, 5);
    let a = fx.data_root.join("a.zip");
    let key = fx.data_key(&uuid_of_path(&fx, &a));

    // 1版目の削除は成功させ2版目以降を失敗させる(FAKE_S3_FAIL_DELVER_FOR=<key>:<ok回数>)。
    // 削除順はnoncurrentの新しい方から(keep=1の削除対象は ids[2]→ids[1]→ids[0] の順)
    let (code, _, stderr) = fx.manage_env(
        "prune",
        &["--apply"],
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
    let (code, stdout, stderr) = fx.manage("prune", &["--apply"]);
    assert_eq!(code, 0, "再実行はexit 0: {stdout}{stderr}");
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "title", "retention-a"));
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
    let Some(fx) = StorageFixture::new(
        "ret-epoch",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 1, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    // 3版(現行=ids[2]・noncurrent=ids[0](古い)/ids[1](新しい))
    let ids = push_versions(&fx, 3);
    let key = fx.data_key(&uuid_of_path(&fx, &a));
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
    let (code, stdout, stderr) = fx.manage("prune", &["--apply"]);
    assert_eq!(
        code, 0,
        "prune --apply(keep_versions=1)はexit 0: {stdout}{stderr}"
    );
    assert_eq!(
        deleted_versions(&fx, &key),
        vec![ids[0].clone()],
        "実時刻(epoch)順で最古のids[0]が削除される(文字列順ならids[1]が削除される): ids={ids:?}"
    );
}

// ---------------------------------------------------------------------------
// manage reset(TASK-18 8章・8.1: dev bucket初期化の恒久コマンド)

/// reset系testの共通setup: 1件push(title=tag)済みのfixtureと作業dirを返す
/// (退避先./tmp/がcwd基準のため、reset本体はrepoのtmp/を汚さない作業dirで実行する)。
fn reset_setup(tag: &str) -> Option<(StorageFixture, std::path::PathBuf)> {
    let fx = StorageFixture::new(tag, StorageOpts::default())?;
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"reset-plain");
    let (code, stdout) = push_titled_pty(&fx, &[&a], tag, &[])?;
    assert_eq!(code, 0, "初回push(pty promptでtitle付与): {stdout}");
    let work = fx
        .cfg
        .parent()
        .map(|p| p.join("reset-work"))
        .unwrap_or_default();
    std::fs::create_dir_all(&work).expect("reset作業dir作成");
    Some((fx, work))
}

/// cwd指定で`manage reset`を実行する(registry退避先./tmp/<yyyymmdd>_registry_before_
/// reset.jsonlがcwd基準のため)。
fn reset_run_cwd(
    fx: &StorageFixture,
    extra: &[&str],
    cwd: &std::path::Path,
) -> (i32, String, String) {
    let mut args: Vec<String> = vec!["manage".into(), "reset".into()];
    args.extend(extra.iter().map(|s| s.to_string()));
    args.push("--config".into());
    args.push(fx.cfg.display().to_string());
    args.push("--aws-config".into());
    args.push(fx.aws_toml.display().to_string());
    args.push("--key".into());
    args.push(fx.secret.display().to_string());
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let state_s = fx.state.display().to_string();
    let home_s = fx.home.display().to_string();
    run_bin_cwd(
        &refs,
        &[
            ("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str()),
            ("AMAZING_BACKUP_HOME", home_s.as_str()),
        ],
        cwd,
    )
}

/// versions stateへ指定keyのdelete marker 1件を直接配置する(resetの
/// DeleteMarkers計上・削除の観察用。FakeS3の版state 5列形式: vid/size/mtime/kind/sc)。
/// 版stateを置くkeyは実objectを持たないorphan(pin rm残渣相当)を指定し、
/// push産物の版stateを上書きしない。
fn seed_delete_marker(fx: &StorageFixture, key: &str, vid: &str) {
    let path = fx.state.join("versions").join(key);
    write_file(
        &path,
        format!("{vid}\t0\t2026-10-22T00:00:00Z\tmarker\t\n").as_bytes(),
    );
}

/// reset表示から`<label>=<n>`の値を読む(無ければ-1。値の直後に空白を挟まず
/// 日本語が続く行にも対応)。
fn reset_count(stdout: &str, label: &str) -> i64 {
    stdout
        .split(&format!("{label}="))
        .nth(1)
        .and_then(|rest| {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        })
        .unwrap_or(-1)
}

/// reset系testの実効dev bucket(fixtureのaws.toml既定値・it-bucket)。
const RESET_DEV_BUCKET: &str = "it-bucket";

/// resetのregistry退避先path(TASK-25 2.5: bucket名入り。
/// ./tmp/<yyyymmdd(JST)>_<bucket>_registry_before_reset.jsonl・cwd基準)。
fn reset_backup_path(cwd: &std::path::Path) -> std::path::PathBuf {
    cwd.join("tmp").join(format!(
        "{}_{RESET_DEV_BUCKET}_registry_before_reset.jsonl",
        jst_today().replace('-', "")
    ))
}

/// 表示から`<label>`直後の連続digit列を読む(無ければNone。
/// `除去<N>行・残<M>行`形式のように`=`を挟まない表示用)。
fn count_after_label(stdout: &str, label: &str) -> Option<i64> {
    stdout
        .split(label)
        .nth(1)
        .and_then(|rest| {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<i64>().ok()
        })
}

/// 混在registryの構築(TASK-25 4章追加1): dev push済みregistryへ
/// 他bucket(prod)宛file_upload+そのuuidのfile_write・共有uuidのfile_upload
/// (dev bucket宛・prodのfile_uploadと同一uuid)+同uuidのfile_write・crash残存
/// file_write(対応upload無しuuid)・prune/sweep/location_remove・旧encrypt・
/// 未知record_type行をこの順で追記し、期待温存行(追記順)を返す。
/// 追記行のJSON field名・schema値はsrc/registry.rsの各record structどおり
/// (file_upload=2・file_write=3・prune=2・sweep/location_remove/encrypt=1)。
fn append_mixed_records(fx: &StorageFixture) -> Vec<serde_json::Value> {
    let values = fx.registry_values();
    let mut fu = values
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload"))
        .expect("file_upload記録(dev push)")
        .clone();
    let mut fw = values
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .expect("file_write記録(dev push)")
        .clone();
    let dev_bucket = fu
        .get("s3_bucket")
        .and_then(|b| b.as_str())
        .expect("dev file_uploadのs3_bucket")
        .to_string();
    // prodとdevで共有するuuid(同一entityの再PUT経路で同一uuidが両bucketへuploadされる)
    let shared_uuid = "11111111-0000-7000-8000-000000000011";
    let crash_uuid = "22222222-0000-7000-8000-000000000022";
    let shared_key = fx.data_key(shared_uuid);

    // 1. 他bucket(prod)宛file_upload+そのuuidのfile_write(prod側所在)
    fu["uuid"] = serde_json::json!(shared_uuid);
    fu["title"] = serde_json::json!("prod-entity");
    fu["s3_key"] = serde_json::json!(shared_key);
    fu["s3_bucket"] = serde_json::json!("prod-your-family-bucket");
    fu["version_id"] = serde_json::json!("vid-prod-shared-1");
    let prod_fu = fu.clone();
    fw["uuid"] = serde_json::json!(shared_uuid);
    fw["title"] = serde_json::json!("prod-entity");
    fw["source_path"] = serde_json::json!("/prod/location/a.zip");
    fw["source_name"] = serde_json::json!("a.zip");
    fw["identity_hash"] = serde_json::json!("prodloc1");
    fw["s3_key"] = serde_json::json!(shared_key);
    let prod_fw = fw.clone();
    // 2. 共有uuidのfile_upload(dev bucket宛・prodのfile_uploadと同一uuid)+同uuidの
    //    file_write(dev側所在)。uuidが両bucketのuploadに紐付くためfile_writeは温存
    fu["s3_bucket"] = serde_json::json!(dev_bucket);
    fu["version_id"] = serde_json::json!("vid-dev-shared-1");
    let shared_fu = fu;
    fw["source_path"] = serde_json::json!("/dev/location/a.zip");
    fw["identity_hash"] = serde_json::json!("devloc1");
    let shared_fw = fw.clone();
    // 3. crash残存file_write(対応するfile_uploadが無いuuid)
    fw["uuid"] = serde_json::json!(crash_uuid);
    fw["source_path"] = serde_json::json!("/crash/location/a.zip");
    fw["identity_hash"] = serde_json::json!("crashloc1");
    fw["s3_key"] = serde_json::json!(fx.data_key(crash_uuid));
    let crash_fw = fw;
    // 4. prune/sweep/location_remove・旧encrypt・未知record_typeの各行
    //    (field名・schema値はsrc/registry.rsのrecord structどおり)
    let prune = serde_json::json!({
        "schema": 2,
        "record_type": "prune",
        "recorded_at": "2026-10-26T09:00:00+09:00",
        "deleted": [{ "s3_key": shared_key, "version_id": "vid-prune-1" }],
    });
    let sweep = serde_json::json!({
        "schema": 1,
        "record_type": "sweep",
        "recorded_at": "2026-10-26T09:00:00+09:00",
        "reason": "lifecycle",
        "deleted": [{ "s3_key": shared_key, "version_id": "vid-sweep-1" }],
    });
    let location_remove = serde_json::json!({
        "schema": 1,
        "record_type": "location_remove",
        "recorded_at": "2026-10-26T09:00:00+09:00",
        "uuid": crash_uuid,
        "identity_hash": "crashloc1",
        "source_path": "/crash/location/a.zip",
    });
    let encrypt = serde_json::json!({
        "schema": 1,
        "record_type": "encrypt",
        "recorded_at": "2026-10-26T08:00:00+09:00",
        "batch_id": "20261026-abcdef01",
        "batch_date": "2026-10-26",
        "source_path": "/legacy/path/a.zip",
        "source_name": "a.zip",
        "source_size": 100,
        "source_mtime": "2026-10-01T09:00:00+09:00",
        "source_sha256": "a".repeat(64),
        "age_path": "2026-10-26/a.zip.age",
        "age_size": 140,
        "age_sha256": "b".repeat(64),
        "recipients": ["SHA256:testfingerprint"],
        "manifest_path": "2026-10-26/manifest.jsonl.age",
    });
    let unknown = serde_json::json!({
        "schema": 99,
        "record_type": "future_event",
        "detail": "x",
    });
    let appended = vec![
        prod_fu,
        prod_fw,
        shared_fu,
        shared_fw,
        crash_fw,
        prune,
        sweep,
        location_remove,
        encrypt,
        unknown,
    ];
    for r in &appended {
        append_registry_line(&fx.registry, r);
    }
    appended
}

/// registry末尾へ改行終端しないtorn行を追記する(crash途中writeの模擬)。
fn append_torn_tail(path: &std::path::Path, fragment: &str) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("registry({})へのtorn追記: {e}", path.display()));
    f.write_all(fragment.as_bytes())
        .unwrap_or_else(|e| panic!("torn行の書込({}): {e}", path.display()));
}

/// [covers:retention.reset-dev-purpose-guard]
/// 安全弁1(8.1): 実効profileのpurposeがdevであること。prod等のpurposeでの実行は
/// bucket指定によらず拒否(設定エラー2+案内・S3 API呼び出しなし)。
#[test]
fn reset_rejects_non_dev_purpose_profile() {
    let Some((fx, _)) = reset_setup("ret-reset-prod") else {
        return;
    };
    // aws.tomlへprod/purposeのprofileを追加し実効profileへ指定(dev側と同一bucket・
    // prefixでもpurpose違反で拒否=bucket指定によらないことを観察)。
    // TASK-7: --profile prod1はpipeline.tomlの[profiles.prod1]も選択するため
    // 同名profileをpipeline側へも追加する(purpose安全弁の検証であるため鍵はdevと同一)
    fx.rewrite_aws_toml(&format!(
        "{}\n[aws.profiles.prod1]\npurpose = \"prod\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"{}\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{}\"\n",
        fx.aws_toml_body(),
        fx.prefix,
        fx.credentials.display()
    ));
    fx.rewrite_pipeline_toml(&format!(
        "{}{}",
        fx.pipeline_body(),
        pipeline_profile_section("prod1", &fx.public, &fx.secret)
    ));
    clear_calls(&fx);
    // 既定dry-runの実行でも安全弁は先に判定される
    let (code, stdout, stderr) = fx.manage("reset", &["--profile", "prod1"]);
    assert_eq!(
        code, 2,
        "prod/purposeでの実行は設定エラー2(bucket指定によらず拒否): {stdout}{stderr}"
    );
    assert!(
        stderr.contains("purpose") && stderr.contains("dev"),
        "purpose=dev限定の案内: {stdout}{stderr}"
    );
    assert_eq!(fx.calls(), "", "S3 API呼び出しは行われない: {}", fx.calls());
}

/// [covers:retention.reset-registry-preserves-other-bucket-records]
/// 混在registryでのdev reset --apply(TASK-25 2.2・2.3・安全弁2廃止): 当該bucket
/// 由来行(upload系記録+対象bucketのみにuploadされたuuidのfile_write)のみ除去して
/// 再書込し、他bucket宛行・共有uuidのfile_write(prod側所在情報)・crash残存
/// file_write・prune/sweep/location_remove・旧encrypt・未知record_type行は温存する
/// (行順保持)。退避はfs::copyによる全体copy(bucket名入りの退避先名)。
#[test]
fn reset_rewrites_registry_bucket_scoped() {
    let Some((fx, work)) = reset_setup("ret-reset-mixed") else {
        return;
    };
    let dev_uuid = uuid_of_path(&fx, &fx.data_root.join("a.zip"));
    // dev pushの全行(file_write/file_upload/manifest/index_upload)が除去対象
    let dev_lines = fx.registry_values().len() as i64;
    let kept = append_mixed_records(&fx);
    let before = std::fs::read_to_string(&fx.registry).expect("混在registryの読込");

    let (code, stdout, stderr) = reset_run_cwd(&fx, &["--apply"], &work);
    assert_eq!(
        code, 0,
        "他bucket宛記録の混在でも(温存により)exit 0: {stdout}{stderr}"
    );
    // (b) 退避file=追記込みの全体copy(bucket名入りの名)
    let backup = reset_backup_path(&work);
    assert!(
        backup.is_file(),
        "退避fileが作成される: {}",
        backup.display()
    );
    assert_eq!(
        std::fs::read_to_string(&backup).expect("退避fileの読込"),
        before,
        "退避内容は退避時点のregistryとbyte単位で同一(全体copy)"
    );
    // (c) 再書込後のregistry: dev由来行は除去・追記行は温存(行順保持)
    assert_eq!(
        fx.registry_values(),
        kept,
        "再書込後のregistryは温存行のみ(行順保持)"
    );
    for r in &fx.registry_values() {
        let rt = r.get("record_type").and_then(|t| t.as_str()).unwrap_or("");
        assert!(
            !matches!(rt, "manifest" | "index_upload"),
            "dev bucket宛のmanifest/index_uploadは除去される: {r}"
        );
        if matches!(rt, "file_upload" | "file_write") {
            assert_ne!(
                r.get("uuid").and_then(|u| u.as_str()),
                Some(dev_uuid.as_str()),
                "dev uuidのfile_write/file_uploadは除去される: {r}"
            );
        }
    }
    // 完了表示(2.3): 除去=dev由来行数・残=温存行数
    assert_eq!(
        count_after_label(&stdout, "除去"),
        Some(dev_lines),
        "完了表示の除去行数=dev由来行数: {stdout}"
    );
    assert_eq!(
        count_after_label(&stdout, "行・残"),
        Some(kept.len() as i64),
        "完了表示の残行数=温存行数: {stdout}"
    );
}

/// [covers:retention.reset-registry-rewrite-idempotent]
/// registry再書込の冪等性(TASK-25 2.3): 除去対象0件かつtorn tail無しの再実行は
/// registryを書換えない(temp fileも作成しない。退避copyは実行上書き)。
#[test]
fn reset_registry_rewrite_idempotent_mixed() {
    let Some((fx, work)) = reset_setup("ret-reset-mixed-idem") else {
        return;
    };
    let kept = append_mixed_records(&fx);
    let (code, stdout, stderr) = reset_run_cwd(&fx, &["--apply"], &work);
    assert_eq!(code, 0, "1回目(混在)のreset --apply: {stdout}{stderr}");
    assert_eq!(fx.registry_values(), kept, "1回目で温存行のみ残る");
    let before = std::fs::read_to_string(&fx.registry).expect("registry読込");

    // 2回目: 温存行のみ(除去対象0件・torn tail無し)→registry書換なし・退避copyは実行
    let backup = reset_backup_path(&work);
    std::fs::remove_file(&backup).expect("退避fileを一旦除去(再実行での再作成観察)");
    clear_calls(&fx);
    let (code, stdout, stderr) = reset_run_cwd(&fx, &["--apply"], &work);
    assert_eq!(code, 0, "再実行もexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read_to_string(&fx.registry).expect("registry読込"),
        before,
        "registry内容は再実行前と完全一致(書換なし)"
    );
    assert!(
        backup.is_file(),
        "退避copyは実行される(上書き): {}",
        backup.display()
    );
    assert_eq!(
        std::fs::read_to_string(&backup).expect("退避fileの読込"),
        before,
        "退避内容はregistryと同一"
    );
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 DeleteObjectVersion")),
        "S3削除呼び出しは0件: {}",
        fx.calls()
    );
}

/// [covers:retention.reset-registry-torn-tail]
/// torn write最終行(改行終端しない最終行)は退避file(全体copy)に含まれ、再書込後の
/// registryからは破棄される(load_registryと同解釈。退避copyに残るため証跡喪失なし)。
/// 除去対象0件でもtorn tailがあれば再書込を実行する(tail放置は後続appendの
/// 未完行連結=registry破損要因)。
#[test]
fn reset_registry_drops_torn_tail_on_rewrite() {
    const TORN_MARKER: &str = "torn-tail-marker";
    let torn_line =
        format!("{{\"schema\":2,\"record_type\":\"file_upload\",\"uuid\":\"{TORN_MARKER}");

    // (i) dev由来行あり+末尾torn行: 退避fileにtorn行を含み、再書込後は温存行のみ
    let Some((fx, work)) = reset_setup("ret-reset-torn") else {
        return;
    };
    let kept = append_mixed_records(&fx);
    append_torn_tail(&fx.registry, &torn_line);
    let before = std::fs::read_to_string(&fx.registry).expect("torn付きregistryの読込");

    let (code, stdout, stderr) = reset_run_cwd(&fx, &["--apply"], &work);
    assert_eq!(code, 0, "(i) reset --applyはexit 0: {stdout}{stderr}");
    let backup = reset_backup_path(&work);
    assert!(
        backup.is_file(),
        "(i) 退避fileが作成される: {}",
        backup.display()
    );
    assert_eq!(
        std::fs::read_to_string(&backup).expect("退避fileの読込"),
        before,
        "(i) 退避fileはtorn行を含む全体copy"
    );
    let after = std::fs::read_to_string(&fx.registry).expect("(i) 再書込後registryの読込");
    assert!(
        !after.contains(TORN_MARKER),
        "(i) 再書込後のregistryにtorn行は無い: {after}"
    );
    assert!(
        after.ends_with('\n'),
        "(i) 再書込後のregistryは改行終端(後続append可能): {after:?}"
    );
    assert_eq!(
        read_registry_lines(&fx.registry),
        kept,
        "(i) 再書込後のregistryは温存行のみ"
    );

    // (ii) 除去対象0件(全行非対象bucket宛)+torn tail有り: 再書込が実行されtorn行のみ破棄
    let Some((fx2, work2)) = reset_setup("ret-reset-torn0") else {
        return;
    };
    let values = fx2.registry_values();
    let mut fu = values
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload"))
        .expect("file_upload記録(dev push)")
        .clone();
    let mut fw = values
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .expect("file_write記録(dev push)")
        .clone();
    // registryを非対象bucket(prod)宛の行のみへ組み替え(除去対象0件)
    let prod_uuid = "33333333-0000-7000-8000-000000000033";
    let prod_key = fx2.data_key(prod_uuid);
    fu["uuid"] = serde_json::json!(prod_uuid);
    fu["s3_key"] = serde_json::json!(prod_key);
    fu["s3_bucket"] = serde_json::json!("prod-your-family-bucket");
    fw["uuid"] = serde_json::json!(prod_uuid);
    fw["s3_key"] = serde_json::json!(prod_key);
    fw["source_path"] = serde_json::json!("/prod/location/b.zip");
    fw["identity_hash"] = serde_json::json!("prodonly1");
    let prod_only = vec![fu, fw];
    let mut text = String::new();
    for r in &prod_only {
        text.push_str(&serde_json::to_string(r).expect("registry行serialize"));
        text.push('\n');
    }
    write_file(&fx2.registry, text.as_bytes());
    append_torn_tail(&fx2.registry, &torn_line);
    let before2 = std::fs::read_to_string(&fx2.registry).expect("(ii) torn付きregistryの読込");

    let (code, stdout, stderr) = reset_run_cwd(&fx2, &["--apply"], &work2);
    assert_eq!(code, 0, "(ii) reset --applyはexit 0: {stdout}{stderr}");
    let backup2 = reset_backup_path(&work2);
    assert!(
        backup2.is_file(),
        "(ii) 退避fileが作成される: {}",
        backup2.display()
    );
    assert_eq!(
        std::fs::read_to_string(&backup2).expect("退避fileの読込"),
        before2,
        "(ii) 退避fileはtorn行を含む全体copy"
    );
    let after2 = std::fs::read_to_string(&fx2.registry).expect("(ii) 再書込後registryの読込");
    assert!(
        !after2.contains(TORN_MARKER),
        "(ii) 除去0件でも再書込が実行されtorn行は破棄: {after2}"
    );
    assert_eq!(
        read_registry_lines(&fx2.registry),
        prod_only,
        "(ii) 再書込後のregistryは温存行(prod宛)のみ"
    );
    assert_eq!(
        count_after_label(&stdout, "除去"),
        Some(0),
        "(ii) 除去行数は0(それでも再書込が走る): {stdout}"
    );
}

/// [covers:retention.reset-dry-run-default]
/// manage resetは--applyなき実行が既定dry-run(prune/reconcileと同一流儀): 削除を
/// 行わず、消去対象のobject version数(Versions・DeleteMarkers別)・registry退避先path
/// (./tmp/<yyyymmdd(JST)>_<bucket>_registry_before_reset.jsonl・bucket名入り)・
/// registry再書込の見通し(除去/残行数)を報告してexit 0。
#[test]
fn reset_dry_run_is_default_and_reports_counts() {
    let Some((fx, _)) = reset_setup("ret-reset-dry") else {
        return;
    };
    let uuid = uuid_of_path(&fx, &fx.data_root.join("a.zip"));
    let data_key = fx.data_key(&uuid);
    let marker_key = format!("{}data/{}/pin-gone.age", fx.prefix, uuid);
    seed_delete_marker(&fx, &marker_key, "reset-marker-1");
    clear_calls(&fx);

    let (code, stdout, stderr) = fx.manage("reset", &[]);
    assert_eq!(
        code, 0,
        "reset(オプション省略=dry-run)はexit 0: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run報告であることの表示: {stdout}"
    );
    // 初回pushの最小構成: default.age・manifest・索引・metadataの4版+seed marker 1件
    assert!(
        reset_count(&stdout, "Versions") >= 4,
        "Versions計上(pushの4object以上): {stdout}"
    );
    assert_eq!(
        reset_count(&stdout, "DeleteMarkers"),
        1,
        "DeleteMarkers計上(seed marker 1件): {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "tmp/{}_{}_registry_before_reset.jsonl",
            jst_today().replace('-', ""),
            RESET_DEV_BUCKET
        )),
        "registry退避先pathの表示(bucket名入り): {stdout}"
    );
    // registry再書込の見通し表示(TASK-25 2.4): 除去/残行数。
    // pushのみのfixtureでは全行がdev由来のため除去=registry行数・残=0
    assert!(
        stdout.contains("registry再書込"),
        "registry再書込の見通し表示: {stdout}"
    );
    assert_eq!(
        count_after_label(&stdout, "除去"),
        Some(fx.registry_values().len() as i64),
        "dry-runの除去行数(dev由来の全行): {stdout}"
    );
    assert_eq!(
        count_after_label(&stdout, "行・残"),
        Some(0),
        "dry-runの残行数(温存対象なし): {stdout}"
    );
    // dry-runでは削除・退避とも行わない
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 DeleteObjectVersion")),
        "dry-runではDeleteObjectVersionは呼ばれない: {}",
        fx.calls()
    );
    assert!(fx.registry.is_file(), "registryは削除されない");
    assert!(
        fx.object_path(&data_key).is_file() && fx.object_path(&fx.index_key()).is_file(),
        "data object・索引objectは残る"
    );
}

/// [covers:retention.reset-apply-clears-versions-and-registry]
/// manage reset --apply(2.1の操作順): 1) registry退避(bucket名入りの退避先名) 2)
/// list_versions(root prefix)でVersions・DeleteMarkers全件をdelete_object_version
/// (data/配下に留まらずsys/索引・manifestも消去) 3) 当該bucket由来行のみ除去した
/// registry再書込(本testのfixtureはdev bucketのみの記録=温存対象なしなので残0行→
/// registry file削除。cache dirは対象外) 4) 再list_versionsで0件確認表示。
#[test]
fn reset_apply_clears_versions_and_registry() {
    let Some((fx, work)) = reset_setup("ret-reset-apply") else {
        return;
    };
    let uuid = uuid_of_path(&fx, &fx.data_root.join("a.zip"));
    let data_key = fx.data_key(&uuid);
    let meta_key = fx.metadata_key(&uuid, "ret-reset-apply");
    let marker_key = format!("{}data/{}/pin-gone.age", fx.prefix, uuid);
    seed_delete_marker(&fx, &marker_key, "reset-marker-1");
    let registry_before = fx.registry_values();

    let (code, stdout, stderr) = reset_run_cwd(&fx, &["--apply"], &work);
    assert_eq!(code, 0, "reset --applyはexit 0: {stdout}{stderr}");

    // 1. registry退避: ./tmp/<yyyymmdd>_<bucket>_registry_before_reset.jsonl
    //    (registryと同一内容の全体copy)
    let backup = reset_backup_path(&work);
    assert!(
        backup.is_file(),
        "退避fileが作成される: {}",
        backup.display()
    );
    assert_eq!(
        read_registry_lines(&backup),
        registry_before,
        "退避内容は退避時点のregistryと同一"
    );
    // 2. S3全消去(delete marker含む・sys/索引も消去対象)
    assert!(
        !fx.object_path(&data_key).is_file(),
        "data object(default.age)は削除"
    );
    assert!(
        !fx.object_path(&meta_key).is_file(),
        "metadata objectは削除"
    );
    assert!(
        !fx.object_path(&fx.index_key()).is_file(),
        "索引object(sys/)も削除"
    );
    assert!(
        version_ids_in_calls(&fx.calls(), "s3 DeleteObjectVersion", &marker_key)
            .contains(&"reset-marker-1".to_string()),
        "delete markerもDeleteObjectVersionで削除: {}",
        fx.calls()
    );
    // 3. registry再書込(fixtureはdev bucketのみの記録=温存対象なし→残0行でfile削除)
    assert!(!fx.registry.is_file(), "registry.jsonlは削除される");
    // 4. 再list_versions 0件の確認表示
    assert!(
        stdout.contains("0件"),
        "再list_versions 0件の確認表示: {stdout}"
    );
}

/// [covers:retention.reset-idempotent]
/// 冪等性(8.1): 空状態への再実行(退避skip・0件削除)も成功する。
#[test]
fn reset_is_idempotent_on_empty_state() {
    let Some((fx, work)) = reset_setup("ret-reset-idem") else {
        return;
    };
    let (code, stdout, stderr) = reset_run_cwd(&fx, &["--apply"], &work);
    assert_eq!(code, 0, "1回目のreset --apply: {stdout}{stderr}");
    assert!(!fx.registry.is_file(), "1回目でregistry削除済み");
    let backup = reset_backup_path(&work);
    assert!(backup.is_file(), "1回目で退避file作成済み");

    // 2回目: 空状態(S3 0件・registry不在)への再実行も成功(退避skip・0件削除)
    clear_calls(&fx);
    let (code, stdout, stderr) = reset_run_cwd(&fx, &["--apply"], &work);
    assert_eq!(code, 0, "空状態への再実行もexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("skip"),
        "registry退避はskip表示(不在): {stdout}"
    );
    assert!(
        stdout.contains("registry再書込: skip"),
        "registry再書込もskip表示(不在): {stdout}"
    );
    assert_eq!(reset_count(&stdout, "Versions"), 0, "削除対象0件: {stdout}");
    assert_eq!(
        reset_count(&stdout, "DeleteMarkers"),
        0,
        "削除対象0件(marker): {stdout}"
    );
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 DeleteObjectVersion")),
        "削除呼び出しは無い: {}",
        fx.calls()
    );
    assert!(
        stdout.contains("0件"),
        "再list_versions 0件の確認表示: {stdout}"
    );
}
