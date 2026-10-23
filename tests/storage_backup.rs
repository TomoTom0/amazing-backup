//! TASK-10操作系CLI(backup push/pull・versioning確認・索引不在・災害時動作・
//! tmp/tasks/tm-10/design_s3_storage.md 2.4・3.2・5.1・7.3・8.3)と
//! TASK-10追加のconfig/encrypt条件(config.keep-artifacts-run-cleanup・
//! encrypt.skip-by-registry-record)の結合test。fake seam(AMAZING_BACKUP_FAKE_S3_STATE)の
//! bin subprocessで検証する(テスト先行のため実装前の実行failは許容・--no-runは通る)。

mod common;

use common::*;

/// dir配下のfileを再帰的に列挙する(data/out掃除の検証用)。
fn walk_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
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
    out
}

/// .age fileかどうか(拡張子判定)。
fn is_age_file(p: &std::path::Path) -> bool {
    p.extension()
        .map(|e| e.to_string_lossy() == "age")
        .unwrap_or(false)
}

/// dir配下に.age fileが無いことのassert。
fn assert_no_age_files(dir: &std::path::Path, tag: &str) {
    let files = walk_files(dir);
    assert!(
        files.iter().all(|p| !is_age_file(p)),
        "{tag}: .ageが残っていないこと: {files:?}"
    );
}

/// calls.logを空にする(以降の呼び出しのみを観測する)。
fn clear_calls(fx: &StorageFixture) {
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
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

/// TASK-18結合test用: title付きの新規発行push(pty prompt経由)。
fn sb_push_titled(
    fx: &StorageFixture,
    paths: &[&std::path::Path],
    titles: &str,
    extra: &[&str],
) -> Option<(i32, String)> {
    push_titled_pty(fx, paths, titles, extra)
}

/// registryのfile_write記録からsource_pathでuuidを引く(同一pathには内容変更で
/// 別entityの記録が並び得るため最新=最後の記録を採る。設計4.3)。
fn sb_uuid_of(fx: &StorageFixture, path: &std::path::Path) -> String {
    uuid_of_path(fx, path)
}

/// TASK-18新体系のStorageOpts([categories]廃止・[aws.lifecycle]必須)。
fn sb_opts() -> StorageOpts {
    StorageOpts {
        no_categories: true,
        aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
        ..StorageOpts::default()
    }
}

/// [covers:cli.backup-push-integrated-run]
/// backup push統合正常系(TASK-18 1.2・5.7): 対象解決(+title)→分類解決→暗号化→
/// upload(default.age(data/{uuid}/)・manifest(V2・schema=2)・索引再構築(schema=2)・
/// touched_entitiesのmetadata sync)を1実行で行う(順序処理)。data/outは
/// <out>/<batch_id>/<name>.ageのrun単位構造。registryのeventはschema2 record
/// (file_write・file_upload(uuid/title入り)・manifest・index_upload)で記録される。
#[test]
fn backup_push_runs_integrated_pipeline() {
    let Some(fx) = StorageFixture::new(
        "sb-integrated",
        StorageOpts {
            keep_artifacts: "always",
            ..sb_opts()
        },
    ) else {
        return;
    };
    let travel = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("travel.zip");
    write_file(&travel, b"integrated-travel");
    let album = fx.elsewhere.join("album.zip");
    write_file(&album, b"integrated-album");
    let Some((code, stdout)) =
        sb_push_titled(&fx, &[&travel, &album], "旅のしおり\nalbum-2026", &[])
    else {
        return;
    };
    assert_eq!(code, 0, "backup pushはexit 0: {stdout}");

    // data/{uuid}/default.age・manifest・索引・metadataが配置される
    let travel_uuid = sb_uuid_of(&fx, &travel);
    let album_uuid = sb_uuid_of(&fx, &album);
    let travel_key = fx.data_key(&travel_uuid);
    let album_key = fx.data_key(&album_uuid);
    for key in [&travel_key, &album_key] {
        assert!(fx.object_path(key).is_file(), "data key({key})へobject配置");
    }
    assert!(fx.object_path(&fx.index_key()).is_file(), "索引object配置");
    assert!(
        fx.object_path(&fx.metadata_key(&travel_uuid, "旅のしおり"))
            .is_file(),
        "metadata_<title>.jsonが配置される"
    );
    assert!(
        fx.object_path(&fx.metadata_key(&album_uuid, "album-2026"))
            .is_file(),
        "metadata_<title>.jsonが配置される(album)"
    );

    // data/outに <batch_id>/<name>.age が生成される(run単位構造)
    let batch_ids = run_batch_ids(&fx);
    assert_eq!(batch_ids.len(), 1, "1 run: {batch_ids:?}");
    let run_dir = fx.out_dir.join(&batch_ids[0]);
    assert!(
        run_dir.join("travel.zip.age").is_file(),
        "run単位の成果物: {}",
        run_dir.display()
    );
    assert!(
        run_dir.join("album.zip.age").is_file(),
        "run単位の成果物(album): {}",
        run_dir.display()
    );

    // registryにschema2 recordの記録がappendされる
    let registry = fx.registry_values();
    assert_eq!(
        registry_count(&registry, "file_write"),
        2,
        "file_write記録は2対象分"
    );
    assert_eq!(
        registry_count(&registry, "file_upload"),
        2,
        "file_upload記録は2対象分"
    );
    for r in registry.iter().filter(|r| {
        matches!(
            r.get("record_type").and_then(|t| t.as_str()),
            Some("file_write") | Some("file_upload")
        )
    }) {
        assert_eq!(r["schema"], 2, "schema=2: {r}");
        assert!(r.get("uuid").is_some(), "uuid入り: {r}");
        assert!(r.get("title").is_some(), "title入り: {r}");
    }
    assert_eq!(registry_count(&registry, "manifest"), 1, "manifest記録");
    assert_eq!(
        registry_count(&registry, "index_upload"),
        1,
        "index_upload記録"
    );

    // 旧uploadサブコマンドはTASK-11(Phase B)で削除され、旧uploadによる新.ageの
    // 二重書き込み(7.3干渉防止)は構造的に不可能(未知サブコマンドでusage error 64)
    let cfg_s = fx.cfg.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let state_s = fx.state.display().to_string();
    let (code, _stdout, _stderr) = run_bin_env(
        &[
            "upload",
            "--config",
            &cfg_s,
            "--aws-config",
            &aws_s,
            "--dry-run",
        ],
        &[("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str())],
    );
    assert_eq!(
        code, 64,
        "旧uploadサブコマンドは未知サブコマンド扱い(併用可能性の構造的消滅)"
    );
}

/// [covers:cli.backup-push-dry-run-plan]
/// backup push --dry-run(TASK-18 4.1・6.1): 暗号化せず平文sha256のみ計算して
/// (1)所在skip (2)crash残存のre-put (3)sha合流のmerge(合流先uuid表示) (4)新規発行のnew
/// (uuid未確定はkeyをdata/(新規uuid)/default.ageと表示)を出し分ける。
/// dry-run中もrun_map相当のmapを更新するため同一run内同一shaの2件目以降はmerge表示に
/// なる。新規発行予測でtitle未指定の場合は警告表示。age実行とS3 API呼び出しを行わない。
#[test]
fn backup_push_dry_run_plans_without_side_effects() {
    let Some(fx) = StorageFixture::new("sb-dryrun", sb_opts()) else {
        return;
    };
    // (A)skip対象: push済み・変更なし
    let skip_t = fx.data_root.join("skip.zip");
    write_file(&skip_t, b"dryrun-skip");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&skip_t], "skip-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "skip対象の初回push: {stdout}");
    let skip_uuid = sb_uuid_of(&fx, &skip_t);

    // (B)crash残存(file_writeのみ・file_upload無し)対象
    let crash_t = fx.data_root.join("crash.zip");
    write_file(&crash_t, b"dryrun-crash");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&crash_t], "crash-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "crash対象の初回push: {stdout}");
    let crash_uuid = sb_uuid_of(&fx, &crash_t);
    let crash_key = fx.data_key(&crash_uuid);
    rewrite_registry_lines(&fx.registry, |r| {
        !(r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
            && r.get("s3_key").and_then(|k| k.as_str()) == Some(crash_key.as_str()))
    });

    // (C)合流対象(mtime touch・内容不変)・(D)新規(titleあり)・(E)新規(titleなし)
    let merge_t = fx.data_root.join("merge.zip");
    write_file(&merge_t, b"dryrun-same");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&merge_t], "merge-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "合流対象の初回push: {stdout}");
    let merge_uuid = sb_uuid_of(&fx, &merge_t);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&merge_t, b"dryrun-same");
    let new_t = fx.data_root.join("new.zip");
    write_file(&new_t, b"dryrun-new");
    // (F)同一run内同一sha: new_tと同一内容(titleあり)
    let dup_t = fx.data_root.join("dup.zip");
    write_file(&dup_t, b"dryrun-new");

    // dry-run: new_tはfrom-listでtitle指定、dup_tはfrom-list・titleなし(同一run内合流で
    // title不要)、notitleはpositionalでtitleなし(警告対象)
    let notitle_t = fx.data_root.join("notitle.zip");
    write_file(&notitle_t, b"dryrun-notitle");
    let registry_before = fx.registry_values().len();
    clear_calls(&fx);
    let (code, stdout, stderr) = fx.push_from_list(
        &[&skip_t, &merge_t, &crash_t, &notitle_t],
        &[(&new_t, Some("new-title")), (&dup_t, None)],
        &["--dry-run"],
    );
    assert_eq!(code, 0, "backup push --dry-runはexit 0: {stdout}{stderr}");
    // 予測表示の出し分け
    assert!(stdout.contains("skip"), "skip予測表示: {stdout}");
    assert!(
        stdout.contains("re-put") && stdout.contains(&crash_key),
        "crash残存のre-put予測表示(合流先key {crash_key}): {stdout}"
    );
    assert!(
        stdout.contains("merge") && stdout.contains(&merge_uuid),
        "sha合流のmerge予測表示(合流先uuid {merge_uuid}): {stdout}"
    );
    assert!(
        stdout.contains("data/(新規uuid)/default.age") || stdout.contains("新規uuid"),
        "新規発行はuuid未確定のkey表示: {stdout}"
    );
    assert!(
        stdout.contains("new-title") && stdout.contains("new.zip"),
        "新規発行(new)予測表示: {stdout}"
    );
    // 同一run内同一shaの2件目(dup_t)もmerge表示(dry-run内run_map相当の更新)
    let merge_count = stdout.matches("merge").count();
    assert!(
        merge_count >= 2,
        "合流対象(C)と同一run内同一sha(F)の両方がmerge表示: {stdout}"
    );
    // 新規発行予測でtitle未指定(notitle.zip)は警告表示
    assert!(
        (stdout.contains("WARNING") || stdout.contains("警告"))
            && (stdout.contains("notitle.zip") || stdout.contains("title")),
        "新規+titleなしの警告表示: {stdout}{stderr}"
    );
    let _ = skip_uuid;
    // 暗号化・S3 API呼び出し無し・registry追記なし
    assert_eq!(fx.calls(), "", "S3 API呼び出し無し: {}", fx.calls());
    assert_eq!(
        fx.registry_values().len(),
        registry_before,
        "dry-runはregistryへ記録しない(現状のまま)"
    );
    assert!(walk_files(&fx.out_dir).is_empty(), "data/outへ成果物無し");
}

/// [covers:cli.backup-pull-all-confirmation]
/// backup pull --all(設計3.2): yes/no確認を伴い(--yesで省略可)・非TTYで--yesなしは
/// usage error 64。確認でnoの場合は実行せずexit 0。
#[test]
fn backup_pull_all_requires_confirmation() {
    let Some(fx) = StorageFixture::new("sb-pullall", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pull-all-a");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");

    // 非TTY(subprocess pipe)で--yesなしはusage error 64
    let (code, _, _) = fx.backup_pull(&["--all"]);
    assert_eq!(code, 64, "非TTYで--yesなしのpull --allはusage error 64");

    // 確認プロンプトでno: 実行せずexit 0(pty経由でstdinへno)
    let cfg_s = fx.cfg.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let key_s = fx.secret.display().to_string();
    let state_s = fx.state.display().to_string();
    clear_calls(&fx);
    let result = run_bin_pty_stdin(
        &[
            "backup",
            "pull",
            "--all",
            "--config",
            &cfg_s,
            "--aws-config",
            &aws_s,
            "--key",
            &key_s,
        ],
        &[("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str())],
        "no",
    );
    if let Some((code, stdout)) = result {
        assert_eq!(code, 0, "確認noは実行せずexit 0: {stdout}");
        assert!(
            !fx.calls()
                .lines()
                .any(|l| l.starts_with("s3 GetObject") || l.starts_with("s3 RestoreObject")),
            "noの場合は復元job発行・取得を行わない: {}",
            fx.calls()
        );
    }
}

/// [covers:cli.backup-pull-category-run-confirmation]
/// --category C / --run ID 指定でも確認ガードは省略されない(設計3.2。
/// --date必須は撤廃し--category/--run/--allのいずれかで範囲指定)。
#[test]
fn backup_pull_category_and_run_keep_confirmation_guard() {
    let Some(fx) = StorageFixture::new("sb-pullguard", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pull-guard-a");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let run_id = run_batch_ids(&fx)[0].clone();

    // 非TTYで--yesなしはusage error 64(--category/--runでも同一ガード)
    let (code, _, _) = fx.backup_pull(&["--category", "unfiled"]);
    assert_eq!(code, 64, "--category指定でも非TTY--yesなしはusage error 64");
    let (code, _, _) = fx.backup_pull(&["--run", &run_id]);
    assert_eq!(code, 64, "--run指定でも非TTY--yesなしはusage error 64");

    // 確認でnoなら復元job発行等を行わずexit 0
    let cfg_s = fx.cfg.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let key_s = fx.secret.display().to_string();
    let state_s = fx.state.display().to_string();
    for scope_args in [vec!["--category", "unfiled"], vec!["--run", &run_id]] {
        let mut args: Vec<&str> = vec!["backup", "pull"];
        args.extend(scope_args.iter().copied());
        args.extend(["--config", &cfg_s, "--aws-config", &aws_s, "--key", &key_s]);
        clear_calls(&fx);
        if let Some((code, stdout)) = run_bin_pty_stdin(
            &args,
            &[("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str())],
            "no",
        ) {
            assert_eq!(code, 0, "{scope_args:?}の確認noはexit 0: {stdout}");
            assert!(
                !fx.calls()
                    .lines()
                    .any(|l| l.starts_with("s3 GetObject") || l.starts_with("s3 RestoreObject")),
                "noの場合は何も行わない: {}",
                fx.calls()
            );
        }
    }
}

/// [covers:cli.backup-pull-run-scope]
/// backup pull --run ID(TASK-18 5.10): 当該run(batch)対象へ一括で復元job発行+取得+
/// 検証+復号。--runはversionsのbatch_id一致の現行語彙維持。復元先は<out>/<title>=getと同一。
/// 対象外runのfileは取得されない。
#[test]
fn backup_pull_run_restores_only_run_scope() {
    let Some(fx) = StorageFixture::new("sb-pullrun", sb_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.data_root.join("b.zip");
    let c = fx.data_root.join("c.zip");
    write_file(&a, b"pull-run-a");
    write_file(&b, b"pull-run-b");
    write_file(&c, b"pull-run-c");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a, &b], "pull-a\npull-b", &[]) else {
        return;
    };
    assert_eq!(code, 0, "run1のpush: {stdout}");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&c], "pull-c", &[]) else {
        return;
    };
    assert_eq!(code, 0, "run2のpush: {stdout}");
    let batch_ids = run_batch_ids(&fx);
    assert_eq!(batch_ids.len(), 2, "2 run: {batch_ids:?}");
    let run1 = &batch_ids[0];

    let out = fx.elsewhere.join("pullrun-out");
    let (code, stdout, stderr) =
        fx.backup_pull(&["--run", run1, "--yes", "--out", &out.display().to_string()]);
    assert_eq!(code, 0, "backup pull --runはexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out.join("pull-a")).expect("run1のa"),
        b"pull-run-a".to_vec(),
        "run1対象の取得・復号(<out>/<title>)"
    );
    assert_eq!(
        std::fs::read(out.join("pull-b")).expect("run1のb"),
        b"pull-run-b".to_vec(),
        "run1対象の取得・復号(b)"
    );
    assert!(
        !out.join("pull-c").exists(),
        "対象外runのfileは取得されない"
    );
    let c_uuid = sb_uuid_of(&fx, &c);
    let c_key = fx.data_key(&c_uuid);
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 GetObject") && l.contains(&format!("key={c_key}"))),
        "対象外runのkeyへのGetObjectは行われない: {}",
        fx.calls()
    );
}

/// [covers:cli.backup-pull-scope-options-exclusive]
/// backup pullの--all/--category/--runの範囲指定は排他(L-7): 2つ以上の併用は
/// usage error 64(確認ガード・索引GETの前に判定される)。
#[test]
fn backup_pull_scope_options_are_exclusive() {
    let Some(fx) = StorageFixture::new("sb-pullexcl", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pull-exclusive-a");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let run_id = run_batch_ids(&fx)[0].clone();
    let _ = std::fs::remove_file(fx.state.join("calls.log"));

    for scope in [
        vec!["--all", "--category", "unfiled"],
        vec!["--all", "--run", run_id.as_str()],
        vec!["--category", "unfiled", "--run", run_id.as_str()],
    ] {
        let mut extra: Vec<&str> = scope.clone();
        extra.push("--yes");
        let (code, stdout, stderr) = fx.backup_pull(&extra);
        assert_eq!(
            code, 64,
            "範囲指定の併用({scope:?})はusage error 64: {stdout}{stderr}"
        );
    }
    assert_eq!(
        read_calls(&fx.state),
        "",
        "S3 API呼び出し(索引GET)は行われない: {}",
        read_calls(&fx.state)
    );
}

/// [covers:cli.versioning-disabled-error]
/// backup push・pin addは起動時にGetBucketVersioningで確認し(設計5.1)、無効なら
/// 設定エラー2+有効化command表示。読み取り系(list files)はversioning確認を行わない。
#[test]
fn versioning_disabled_rejects_writes_with_guidance() {
    let Some(fx) = StorageFixture::new("sb-vercheck", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"vercheck-a");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");

    // 読み取り系はGetBucketVersioningを呼ばない
    clear_calls(&fx);
    let (code, stdout, stderr) = fx.list_env("files", &[], &[("FAKE_S3_VERSIONING_DISABLED", "1")]);
    assert_eq!(
        code, 0,
        "versioning無効でも読み取り系は動作: {stdout}{stderr}"
    );
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 GetBucketVersioning")),
        "読み取り系はGetBucketVersioningを呼ばない: {}",
        fx.calls()
    );

    // backup push: 無効ならS3書き込み前にexit 2+有効化案内
    let b = fx.data_root.join("b.zip");
    write_file(&b, b"vercheck-b");
    clear_calls(&fx);
    let (code, _, stderr) = fx.push(&[&b], &[], &[("FAKE_S3_VERSIONING_DISABLED", "1")]);
    assert_eq!(
        code, 2,
        "versioning無効bucketへのpushは設定エラー2: {stderr}"
    );
    assert!(
        stderr.contains("put-bucket-versioning"),
        "有効化commandの専用案内: {stderr}"
    );
    assert!(
        !fx.calls().lines().any(|l| l.starts_with("s3 PutObject")),
        "S3書き込みは行われない: {}",
        fx.calls()
    );
    assert!(
        fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 GetBucketVersioning")),
        "起動時のGetBucketVersioning確認: {}",
        fx.calls()
    );

    // pin addも同一の起動時確認
    let (code, _, stderr) = fx.pin_env(
        "add",
        "a.zip",
        &["--latest", "--as", "pm"],
        &[("FAKE_S3_VERSIONING_DISABLED", "1")],
    );
    assert_eq!(
        code, 2,
        "versioning無効bucketへのpin addは設定エラー2: {stderr}"
    );
}

/// [covers:cli.index-missing-error]
/// 照会系共通規則(設計3章・11章F4): 最初の索引GET(sys/index.jsonl.age)が404(不在)または
/// 復号失敗の場合は設定・環境エラー2+専用案内(manage index rebuild・またはaws.tomlの
/// bucket/prefix確認)。索引なしでのS3 listing直接照会への自動fall backは行わない。
#[test]
fn missing_index_is_config_error_for_all_read_commands() {
    let Some(fx) = StorageFixture::new("sb-noidx", StorageOpts::default()) else {
        return;
    };
    for (tag, run) in [
        ("list files", fx.list("files", &[])),
        ("query", fx.query("travel", &[])),
        ("get", fx.get("a.zip", &[])),
        ("diff", fx.diff(&[])),
    ] {
        let (code, stdout, stderr) = run;
        assert_eq!(
            code, 2,
            "{tag}: 索引不在は設定・環境エラー2: {stdout}{stderr}"
        );
        let combined = format!("{stdout}{stderr}");
        assert!(
            combined.contains("manage index rebuild") || combined.contains("index rebuild"),
            "{tag}: rebuild案内: {combined}"
        );
        assert!(
            combined.contains("bucket") || combined.contains("prefix"),
            "{tag}: bucket/prefix確認の案内: {combined}"
        );
    }
    // S3 listingへの自動fall backは行われない
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 ListObjectsV2") || l.starts_with("s3 ListObjectVersions")),
        "索引不在でlisting照会へfall backしない: {}",
        fx.calls()
    );
}

/// [covers:cli.disaster-recovery-reads]
/// query/get/list/diffはaws.toml(bucket/region/prefix)と秘密鍵のみで動作(設計2.4):
/// pipeline.toml・ローカルregistryが不在の作業dirでも、最初に索引をGET→復号→
/// 以降の操作は索引+s3 APIで完結する。
#[test]
fn read_commands_work_without_pipeline_and_registry() {
    let Some(fx) = StorageFixture::new("sb-disaster", StorageOpts::default()) else {
        return;
    };
    let travel = fx.data_root.join("travel.zip");
    write_file(&travel, b"disaster-travel");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&travel], "travel.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");

    // 作成環境資産(pipeline.toml・registry)が不在の作業dirで実行
    let bare = fx.elsewhere.join("bare-workspace");
    std::fs::create_dir_all(&bare).expect("bare workspace");
    let aws_s = fx.aws_toml.display().to_string();
    let key_s = fx.secret.display().to_string();
    let state_s = fx.state.display().to_string();
    let env = [("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str())];

    let (code, stdout, stderr) = run_bin_cwd(
        &["query", "travel", "--aws-config", &aws_s, "--key", &key_s],
        &env,
        &bare,
    );
    assert_eq!(
        code, 0,
        "不在pipelineの作業dirでqueryはexit 0: {stdout}{stderr}"
    );
    assert!(stdout.contains("travel.zip"), "検索結果の表示: {stdout}");

    let (code, stdout, stderr) = run_bin_cwd(
        &["list", "files", "--aws-config", &aws_s, "--key", &key_s],
        &env,
        &bare,
    );
    assert_eq!(
        code, 0,
        "不在pipelineの作業dirでlist filesはexit 0: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("travel.zip") || stdout.contains("unfiled"),
        "目録表示: {stdout}"
    );
}

/// [covers:config.keep-artifacts-run-cleanup]
/// keep_artifacts="run"(設計4.1・8.3): backup push成功(run完結)後にdata/out配下の
/// 当該run分(<batch_id>/*.age)を削除。失敗残りは残り、再実行で再利用される。
#[test]
fn keep_artifacts_run_cleans_out_dir_after_success() {
    // 成功後はdata/out配下が掃除される(既定run)
    let Some(fx) = StorageFixture::new("sb-keepclean", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"keep-clean-a");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "push成功: {stdout}");
    assert_no_age_files(
        &fx.out_dir,
        "成功後はdata/out配下の当該run分.ageが削除される",
    );
    // objectはupload済み(掃除はローカル成果物のみ)
    assert!(
        fx.object_path(&fx.data_key(&uuid_of_path(&fx, &a)))
            .is_file(),
        "S3側のobjectは残る"
    );

    // 失敗時は残る(失敗注入でexit 5。bはcrash残存化して注入対象keyを確定)
    let Some(fx2) = StorageFixture::new("sb-keepfail", StorageOpts::default()) else {
        return;
    };
    let b = fx2.data_root.join("b.zip");
    write_file(&b, b"keep-fail-b");
    let Some((code, stdout)) = sb_push_titled(&fx2, &[&b], "b.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "bの初回push: {stdout}");
    let fail_key = fx2.data_key(&uuid_of_path(&fx2, &b));
    rewrite_registry_lines(&fx2.registry, |r| {
        !(r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
            && r.get("s3_key").and_then(|k| k.as_str()) == Some(fail_key.as_str()))
    });
    let (code, _, _) = fx2.push(&[&b], &[], &[("FAKE_S3_FAIL_CP_FOR", fail_key.as_str())]);
    assert_eq!(code, 5, "失敗注入は転送エラー5");
    let leftovers: Vec<_> = walk_files(&fx2.out_dir)
        .into_iter()
        .filter(|p| is_age_file(p))
        .collect();
    assert!(
        !leftovers.is_empty(),
        "失敗時はdata/outに.ageが残る: {leftovers:?}"
    );

    // 再実行(注入なし)で成功: 掃除対象は当該run分(<batch_id> dir)のみ(Phase A併存で
    // 旧経路産物・失敗runの残りを消さない)。失敗runの残りは残る
    let before = walk_files(&fx2.out_dir)
        .into_iter()
        .filter(|p| is_age_file(p))
        .count();
    assert!(before > 0, "失敗runの.ageが残っている前提: {before}");
    let (code, stdout, stderr) = fx2.push(&[&b], &[], &[]);
    assert_eq!(code, 0, "再実行はexit 0: {stdout}{stderr}");
    let after = walk_files(&fx2.out_dir)
        .into_iter()
        .filter(|p| is_age_file(p))
        .count();
    assert_eq!(
        after, before,
        "成功runの掃除は当該run分のみ(失敗runの残りは消さない)"
    );
    assert!(fx2.object_path(&fail_key).is_file(), "S3側のobjectは残る");
}

/// [covers:encrypt.skip-by-registry-record]
/// encrypt skip判定のregistry基準化(TASK-18 4.1(1)): skip条件はfile_write成功記録一致
/// (identity_hash+size+mtime)かつfile_upload記録が存在(u.uuid == w.uuid照合+bucket一致。
/// 旧s3_key比較の置換)。.age不在でもskipされる(data/out掃除後の再実行で重複暗号化が
/// 起きない)。file_write一致でもfile_upload無し(crash残存)はskipせず(2.5)再PUT分岐へ
/// 進む(振る舞いの詳細はcli.backup-push-crash-reput-same-uuidが担保)。
#[test]
fn encrypt_skip_is_based_on_registry_record() {
    let Some(fx) = StorageFixture::new("sb-skipreg", sb_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"skip-registry-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "skip-reg-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = sb_uuid_of(&fx, &a);
    let key = fx.data_key(&uuid);
    let puts_after_first = fx
        .calls()
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}")))
        .count();
    assert_eq!(puts_after_first, 1, "初回は1回PUT: {}", fx.calls());
    // keep_artifacts="run"既定によりdata/outの.ageは掃除済み(.age不在の状態)
    assert_no_age_files(&fx.out_dir, "初回成功後は.age不在");

    // 再実行: 変更なし→registry基準でskip(.age不在でも)
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "再実行はexit 0: {stdout}{stderr}");
    assert!(stdout.contains("skip"), "skip表示: {stdout}");
    assert_eq!(
        registry_count(&fx.registry_values(), "file_write"),
        1,
        "skip時はfile_write記録の追記なし"
    );
    assert_eq!(
        fx.calls()
            .lines()
            .filter(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}")))
            .count(),
        puts_after_first,
        "skip時は当該keyへの再PUTなし: {}",
        fx.calls()
    );
}

/// [covers:cli.backup-push-index-updated-on-all-skip-rerun]
/// 部分失敗run(索引PUTのみ失敗・registry記録済み)の全skip再実行でも索引が更新される
/// (H-2: 成功run末尾で常時再構築。索引がstaleのままexit 0しない)。
#[test]
fn all_skip_rerun_after_index_failure_updates_index() {
    let Some(fx) = StorageFixture::new(
        "sb-idxskip",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"idxskip-v1");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let index_key = fx.index_key();
    let key = fx.data_key(&uuid_of_path(&fx, &a));

    // 2版目(同一内容のcrash残存re-put・設計4.4)のpushで索引PUTのみ失敗注入
    // (file upload・registry記録は成功)
    rewrite_registry_lines(&fx.registry, |r| {
        !(r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
            && r.get("s3_key").and_then(|k| k.as_str()) == Some(key.as_str()))
    });
    let (code, _, _) = fx.push(&[&a], &[], &[("FAKE_S3_FAIL_CP_FOR", index_key.as_str())]);
    assert_eq!(code, 5, "索引PUT失敗は転送エラー5");
    assert!(
        registry_count(&fx.registry_values(), "file_upload") >= 1,
        "file_upload記録は済んでいる(部分失敗)"
    );

    // 同内容の再実行: 全skipでも索引が更新される(成功run末尾で常時再構築)
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "全skip再実行はexit 0: {stdout}{stderr}");
    assert!(stdout.contains("skip"), "skip表示: {stdout}");
    assert!(
        fx.object_path(&index_key).is_file(),
        "索引objectは存在(初回pushで作成済み)"
    );
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "title", "a.zip");
    let vids = index_version_ids(entry);
    let reg_ids = registry_upload_version_ids(&fx.registry_values(), &key);
    assert_eq!(
        vids,
        reg_ids.iter().rev().cloned().collect::<Vec<_>>(),
        "全skip再実行後に索引が最新run分まで反映されている(={vids:?})"
    );
}

/// [covers:pin.add-requires-config-early]
/// pin add/rmはregistry記録を要するため--config必須(M-4)。S3変更前に設定エラー2で
/// 拒否される(S3 API呼び出しなし・write-once滞留しない)。
#[test]
fn pin_add_without_config_fails_before_s3_calls() {
    let Some(fx) = StorageFixture::new("sb-pinnoconf", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"pin-no-config");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
    let aws_s = fx.aws_toml.display().to_string();
    let state_s = fx.state.display().to_string();
    let (code, _, stderr) = run_bin_env(
        &[
            "pin",
            "add",
            "a.zip",
            "--latest",
            "--as=pm",
            "--aws-config",
            &aws_s,
        ],
        &[("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str())],
    );
    assert_eq!(code, 2, "--configなしpin addは設定エラー2: {stderr}");
    assert_eq!(
        read_calls(&fx.state),
        "",
        "S3 API呼び出しは行われない: {}",
        read_calls(&fx.state)
    );
}

/// [covers:verify.swept-run-artifacts-excluded]
/// backup verifyはkeep_artifacts="run"既定で成功run直後に掃除されたrun
/// (当該batchのindex_upload成功記録がありrun成果物dirが不在)のfile_write記録・
/// run manifestを検証失敗扱いにしない(M-1: 掃除済みrunは「掃除済み・検査対象外」として
/// 報告し、構造検査・--checksum・--roundtripの対象から除外する)。
#[test]
fn backup_verify_excludes_swept_run_artifacts() {
    let Some(fx) = StorageFixture::new("sb-vsweep", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"verify-swept-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    // keep_artifacts="run"既定: 成功runの成果物dirは掃除済み(.age不在)
    assert_no_age_files(&fx.out_dir, "成功runの成果物は掃除済み");
    let cfg_s = fx.cfg.display().to_string();

    // 構造検査(既定段階): 掃除済みrunは対象外のためexit 4にならない
    let (code, stdout, stderr) = fx.bin(&["backup", "verify", "--config", &cfg_s], &[]);
    assert_eq!(code, 0, "掃除済みrunは検査対象外でexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("掃除済み"),
        "掃除済みrunの報告表示: {stdout}"
    );

    // --checksum: 掃除済みfile_write記録の.age読込は発生しない
    let (code, stdout, stderr) =
        fx.bin(&["backup", "verify", "--config", &cfg_s, "--checksum"], &[]);
    assert_eq!(code, 0, "--checksumもexit 0: {stdout}{stderr}");

    // --roundtrip: 掃除済みfile_write記録・manifest突合は対象外
    let (code, stdout, stderr) = fx.bin(
        &["backup", "verify", "--config", &cfg_s, "--roundtrip"],
        &[],
    );
    assert_eq!(code, 0, "--roundtripもexit 0: {stdout}{stderr}");
}

/// calls.logを空にする(以降の呼び出しのみを観測する)。
fn sb_clear_calls(fx: &StorageFixture) {
    let _ = std::fs::remove_file(fx.state.join("calls.log"));
}

/// data key(data/{uuid}/default.age)へのPutObject呼び出し数。
fn sb_data_puts(fx: &StorageFixture, key: &str) -> usize {
    fx.calls()
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key}")))
        .count()
}

/// registryのfile_write記録でuuid一致の件数。
fn sb_file_write_count(fx: &StorageFixture, uuid: &str) -> usize {
    fx.registry_values()
        .iter()
        .filter(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid)
        })
        .count()
}

/// [covers:cli.backup-push-sha-merge-no-upload]
/// sha照合合流(TASK-18 4.1(3a)・4.3): 所在skip不成立(移動=identity変化・mtime変化等)→
/// 暗号化→平文sha256がrun内mapまたはsha_uuid_lookup(registry)で既存uuidに命中した場合は
/// uploadなしで合流: file_write追記・索引locationsへ新所在追加・版増加なし・merge表示。
#[test]
fn backup_push_merges_by_sha_without_upload() {
    let Some(fx) = StorageFixture::new("sb-merge", sb_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"merge-no-upload-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "merge-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = sb_uuid_of(&fx, &a);
    let key = fx.data_key(&uuid);
    let puts_before = sb_data_puts(&fx, &key);

    // (1) 移動+内容不変: 別pathへmoveして再push → 合流(所在追加・upload省略)
    let moved = fx.data_root.join("moved").join("a.zip");
    std::fs::create_dir_all(&moved.parent().expect("parent")).expect("moved dir");
    std::fs::rename(&a, &moved).expect("move");
    let (code, stdout, stderr) = fx.push(&[&moved], &[], &[]);
    assert_eq!(code, 0, "移動後のpushはexit 0: {stdout}{stderr}");
    assert!(stdout.contains("merge"), "merge表示: {stdout}");
    assert_eq!(
        sb_data_puts(&fx, &key),
        puts_before,
        "default.ageへのPUT無し(版増加なし): {}",
        fx.calls()
    );
    assert_eq!(
        sb_file_write_count(&fx, &uuid),
        2,
        "合流でもfile_writeは追記(新所在)"
    );
    let locations = entity_locations(index_entity_by(&fx.read_index(), "uuid", &uuid));
    assert_eq!(
        locations.len(),
        2,
        "索引locationsが2件(移動case): {locations:?}"
    );
    let doc = fx
        .read_metadata_doc(&uuid, "merge-title")
        .expect("metadata doc");
    assert_eq!(
        doc.get("latest_source_path"),
        Some(&serde_json::json!(
            moved.canonicalize().unwrap().to_string_lossy().into_owned()
        )),
        "metadataのlatest_source_pathが更新される: {doc}"
    );

    // (2) mtimeのみtouch(同一path): 同じく合流・uploadなし
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&moved, b"merge-no-upload-plain");
    let (code, stdout, stderr) = fx.push(&[&moved], &[], &[]);
    assert_eq!(code, 0, "mtime touch後のpushはexit 0: {stdout}{stderr}");
    assert!(stdout.contains("merge"), "merge表示(mtime case): {stdout}");
    assert_eq!(
        sb_data_puts(&fx, &key),
        puts_before,
        "mtimeのみ変化でもPUT無し: {}",
        fx.calls()
    );
    assert_eq!(
        sb_file_write_count(&fx, &uuid),
        3,
        "file_write追記(次回から所在skip回復)"
    );
}

/// [covers:cli.backup-push-moved-changed-new-entity]
/// 移動+内容変更同時(TASK-18 4.3): sha不一致のため合流せず新規uuid発行(title必須)。
/// 旧実体と新実体は別entity(仕様として明記。同一物と思いたければ同titleを付ける)。
#[test]
fn backup_push_moved_changed_becomes_new_entity() {
    let Some(fx) = StorageFixture::new("sb-mvdchg", sb_opts()) else {
        return;
    };
    let seed = fx.data_root.join("seed.zip");
    write_file(&seed, b"seed-plain");
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"old-content");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&seed, &a], "seed-title\nold-title", &[])
    else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let old_uuid = sb_uuid_of(&fx, &a);
    let old_key = fx.data_key(&old_uuid);
    let old_bytes = std::fs::read(fx.object_path(&old_key)).expect("旧object");

    // 移動+内容変更+title指定 → 新規uuid
    let moved = fx.data_root.join("moved").join("a.zip");
    std::fs::create_dir_all(&moved.parent().expect("parent")).expect("moved dir");
    std::fs::rename(&a, &moved).expect("move");
    write_file(&moved, b"new-content-changed");
    let (code, stdout, stderr) = fx.push_from_list(&[&seed], &[(&moved, Some("old-title"))], &[]);
    assert_eq!(code, 0, "移動+内容変更のpushはexit 0: {stdout}{stderr}");
    let new_uuid = sb_uuid_of(&fx, &moved);
    assert_ne!(new_uuid, old_uuid, "sha不一致は合流せず新規uuid発行");
    // 索引は2entity(旧=旧所在・新=新所在)。旧entityのdefault.ageは不変
    let lines = fx.read_index();
    let old_entry = index_entity_by(&lines, "uuid", &old_uuid);
    let new_entry = index_entity_by(&lines, "uuid", &new_uuid);
    assert_eq!(
        entity_locations(old_entry).len(),
        1,
        "旧entityは旧所在のまま: {old_entry}"
    );
    assert_eq!(
        entity_locations(new_entry).len(),
        1,
        "新entityは新所在: {new_entry}"
    );
    assert_eq!(
        std::fs::read(fx.object_path(&old_key)).expect("旧object(再読)"),
        old_bytes,
        "旧uuidのdefault.ageは不変"
    );
}

/// [covers:cli.backup-push-new-uuid-record-order]
/// 新規発行の記録順序(TASK-18 4.1(3b)・4.4): 「file_write追記 → PUT → file_upload追記」
/// でregistry記録がS3変更(PUT)に先行する(「PUT成功後・file_write追記前」のcrash窓が
/// 構造的に存在しない=回収不能の孤児uuidを生まない)。uuidはv7・PUT応答のversion_idを
/// file_uploadに記録・ok表示にdata/{uuid}/default.ageとversion_id・titleを含む。
#[test]
fn backup_push_new_uuid_records_write_before_put() {
    let Some(fx) = StorageFixture::new("sb-order", sb_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"record-order-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "order-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "pushはexit 0: {stdout}");

    let registry = fx.registry_values();
    let uuid = sb_uuid_of(&fx, &a);
    let key = fx.data_key(&uuid);
    let fw_idx = registry
        .iter()
        .position(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid.as_str())
        })
        .expect("file_write記録");
    let fu_idx = registry
        .iter()
        .position(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(uuid.as_str())
        })
        .expect("file_upload記録");
    assert!(
        fw_idx < fu_idx,
        "file_write追記がPUT(→file_upload追記)に先行: fw={fw_idx} fu={fu_idx}"
    );
    assert_eq!(
        registry[fw_idx]["s3_key"],
        serde_json::json!(key),
        "file_writeのs3_keyはdata/{uuid}/default.age"
    );
    let vid = registry[fu_idx]["version_id"].as_str().expect("version_id");
    assert!(!vid.is_empty(), "file_uploadはPUT応答のversion_idを記録");
    assert_eq!(
        uuid.split('-').nth(2).and_then(|p| p.chars().next()),
        Some('7'),
        "uuidはUUIDv7: {uuid}"
    );
    // ok表示: data/{uuid}/default.age・version・title
    assert!(
        stdout.contains(&key) && stdout.contains(vid) && stdout.contains("order-title"),
        "ok表示にkey・version_id・titleを含む: {stdout}"
    );
}

/// [covers:cli.backup-push-crash-reput-same-uuid]
/// crash救済のuuid再利用(TASK-18 4.1(2.5)・4.4): file_write一致・file_upload無し
/// (crash残存)の対象はskipせず、sha照合・新規発行を経ずに同uuidのdefault.ageへ再PUTし
/// file_uploadを記録(孤児uuid不発生)。title指定なし時のfile_write再追記は行われない。
/// re-put表示。crash前のPUT版は同キーの非現行版として回収対象。
#[test]
fn backup_push_crash_residue_reputs_same_uuid() {
    let Some(fx) = StorageFixture::new("sb-reput", sb_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"crash-reput-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "reput-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let uuid = sb_uuid_of(&fx, &a);
    let key = fx.data_key(&uuid);

    // (2) PUT成功後・file_upload追記前crash相当(S3に版があるがfile_upload無し)
    rewrite_registry_lines(&fx.registry, |r| {
        r.get("record_type").and_then(|t| t.as_str()) != Some("file_upload")
    });
    sb_clear_calls(&fx);
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "crash残存の再実行はexit 0: {stdout}{stderr}");
    assert!(stdout.contains("re-put"), "re-put表示: {stdout}");
    assert_eq!(sb_uuid_of(&fx, &a), uuid, "新規uuid不発行(同uuid再利用)");
    assert!(
        sb_data_puts(&fx, &key) >= 1,
        "同uuidのdefault.ageへ再PUT: {}",
        fx.calls()
    );
    assert_eq!(
        registry_count(&fx.registry_values(), "file_upload"),
        1,
        "file_upload記録が追記される"
    );
    assert_eq!(
        sb_file_write_count(&fx, &uuid),
        1,
        "title指定なし時のfile_write再追記は行われない"
    );

    // (1) file_write追記後・PUT前crash相当(S3 objectも無い)
    rewrite_registry_lines(&fx.registry, |r| {
        r.get("record_type").and_then(|t| t.as_str()) != Some("file_upload")
    });
    let _ = std::fs::remove_file(fx.object_path(&key));
    let _ = std::fs::remove_file(fx.state.join("versions").join(&key));
    sb_clear_calls(&fx);
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "PUT前crash相当の再実行もexit 0: {stdout}{stderr}");
    assert!(stdout.contains("re-put"), "re-put表示(PUT前case): {stdout}");
    assert_eq!(sb_uuid_of(&fx, &a), uuid, "同uuid再利用(PUT前case)");
    assert!(fx.object_path(&key).is_file(), "再PUTでobjectが配置される");
}

/// [covers:cli.backup-push-crash-orphan-on-diff]
/// crash残存+内容変更(TASK-18 4.4の残る限界): PUT成功後・file_upload追記前crashの後に
/// 内容が変更されたまま再実行した場合、所在指紋不一致で(2.5)は発動せず新規uuidへ進み、
/// 旧uuidのdefault.age現行版は索引非載載のまま残る。diffのunexpected(索引に無いdata/
/// 配下key)として検出可能(回収は手動)。
#[test]
fn backup_push_crash_orphan_detected_by_diff() {
    let Some(fx) = StorageFixture::new("sb-orphan", sb_opts()) else {
        return;
    };
    let seed = fx.data_root.join("seed.zip");
    write_file(&seed, b"seed-plain");
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"orphan-old");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&seed, &a], "seed-title\norphan-title", &[])
    else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let old_uuid = sb_uuid_of(&fx, &a);
    let old_key = fx.data_key(&old_uuid);

    // crash残存(file_upload無し・S3版あり)のまま内容を変更して再push → 新規uuid
    rewrite_registry_lines(&fx.registry, |r| {
        !(r.get("record_type").and_then(|t| t.as_str()) == Some("file_upload")
            && r.get("s3_key").and_then(|k| k.as_str()) == Some(old_key.as_str()))
    });
    write_file(&a, b"orphan-new-content");
    let (code, stdout, stderr) = fx.push_from_list(&[&seed], &[(&a, Some("orphan-title"))], &[]);
    assert_eq!(
        code, 0,
        "内容変更の再pushは新規uuid発行でexit 0: {stdout}{stderr}"
    );
    assert_ne!(sb_uuid_of(&fx, &a), old_uuid, "新規uuidへ進む");

    // 旧uuidのdefault.ageは索引非載載のまま残り、diffのunexpectedで検出される
    assert!(
        fx.object_path(&old_key).is_file(),
        "旧uuidのdefault.ageはS3に残存"
    );
    let (code, stdout, stderr) = fx.diff(&[]);
    assert_eq!(code, 0, "diffは報告のみでexit 0: {stdout}{stderr}");
    assert!(stdout.contains("unexpected"), "unexpected報告: {stdout}");
    assert!(
        stdout.contains(&old_key),
        "旧uuidのdata keyがunexpectedとして報告: {stdout}"
    );
}

/// [covers:cli.backup-push-same-sha-in-run]
/// 同一run内の同一sha重複(TASK-18 4.1(3)・4.3): 1件目のfile_write/file_upload追記が
/// 即時に行われるため(順序処理)、2件目はrun_map(run内map優先)→registry lookupで合流
/// (2件目はuploadなし・merge表示)。位置引数とfrom-listの混在でも成立。
#[test]
fn backup_push_same_sha_in_run_merges_second() {
    let Some(fx) = StorageFixture::new("sb-samesha", sb_opts()) else {
        return;
    };
    let p1 = fx.data_root.join("one").join("dup.zip");
    let p2 = fx.data_root.join("two").join("dup.zip");
    std::fs::create_dir_all(&p1.parent().expect("parent")).expect("one dir");
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("two dir");
    write_file(&p1, b"same-sha-plain");
    write_file(&p2, b"same-sha-plain");
    // p1は位置引数(titleはprompt)・p2はfrom-list(title指定)の混在run
    let list = write_from_list(
        fx.cfg.parent().expect("tmp dir"),
        &[(&p2, Some("same-sha-title"))],
    );
    let list_s = list.display().to_string();
    let Some((code, stdout)) =
        sb_push_titled(&fx, &[&p1], "same-sha-title", &["--from-list", &list_s])
    else {
        return;
    };
    assert_eq!(code, 0, "混在runのpushはexit 0: {stdout}");
    let uuid = sb_uuid_of(&fx, &p1);
    assert_eq!(sb_uuid_of(&fx, &p2), uuid, "2件目は同一uuidへ合流(run_map)");
    // 1件目は新規発行(ok)・2件目は合流(merge・uploadなし)
    let stdout_lower = stdout.clone();
    assert!(
        stdout_lower.contains("ok") || stdout_lower.contains("same-sha-title"),
        "1件目の新規発行表示: {stdout}"
    );
    assert!(stdout.contains("merge"), "2件目はmerge表示: {stdout}");
    assert_eq!(
        registry_upload_version_ids(&fx.registry_values(), &fx.data_key(&uuid)).len(),
        1,
        "2件目のuploadなし(file_uploadは1件)"
    );
    // 索引entityは1件(uuid同一)・locations 2件
    let lines = fx.read_index();
    let entities = lines
        .iter()
        .filter(|v| {
            v.get("record_type").and_then(|r| r.as_str()) == Some("file")
                && v.get("title").and_then(|t| t.as_str()) == Some("same-sha-title")
        })
        .count();
    assert_eq!(entities, 1, "同一shaの2対象はentity 1件: {lines:?}");
    let locations = entity_locations(index_entity_by(&lines, "uuid", &uuid));
    assert_eq!(locations.len(), 2, "locations 2件: {locations:?}");
}

/// [covers:cli.backup-push-run-title-consistency]
/// run内title一貫(TASK-18 4.2): run_entity_titles(run開始時にreducedから一括初期化・
/// file_write追記ごと更新)がrun内の正本で、同一run内で1件目のtitle変更(A→B)が
/// 2件目の同一sha合流にも反映される(reduced snapshotが旧title Aを返す非一貫を防ぐ)。
#[test]
fn backup_push_run_title_consistency_via_run_map() {
    let Some(fx) = StorageFixture::new("sb-titlemap", sb_opts()) else {
        return;
    };
    let seed = fx.data_root.join("seed.zip");
    write_file(&seed, b"seed-plain");
    let p1 = fx.data_root.join("one").join("dup.zip");
    std::fs::create_dir_all(&p1.parent().expect("parent")).expect("one");
    write_file(&p1, b"run-title-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&seed, &p1], "seed-title\ntitle-A", &[])
    else {
        return;
    };
    assert_eq!(code, 0, "初回push(title A): {stdout}");
    let uuid = sb_uuid_of(&fx, &p1);

    // 同一run内: 1件目=p1(mtime touch合流・title A→B変更)・2件目=p2(同一sha・title省略)
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&p1, b"run-title-plain");
    let p2 = fx.data_root.join("two").join("dup.zip");
    std::fs::create_dir_all(&p2.parent().expect("parent")).expect("two");
    write_file(&p2, b"run-title-plain");
    let (code, stdout, stderr) =
        fx.push_from_list(&[&seed], &[(&p1, Some("title-B")), (&p2, None)], &[]);
    assert_eq!(code, 0, "同一runのpushはexit 0: {stdout}{stderr}");

    // 2件目の合流もtitle Bで処理され(旧title Aにならない)、run後のtitleはB
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "uuid", &uuid);
    assert_eq!(
        entry.get("title").and_then(|t| t.as_str()),
        Some("title-B"),
        "run後のentity titleはB: {entry}"
    );
    assert!(
        fx.object_path(&fx.metadata_key(&uuid, "title-B")).is_file(),
        "metadataキーはmetadata_B.json"
    );
}

/// [covers:cli.backup-push-title-input]
/// title入力(TASK-18 5.6/5.7・6.1): --from-listの[[targets]] title欄(優先)・
/// 新規uuid発行が確定した対象でtitle未指定の場合のみ対話prompt(tty時。空・違反は再要求)。
/// 非ttyでfrom-listにtitleが無い対象が新規発行になった場合は当該対象の失敗(対象エラー3)
/// としてkeep_goingの設定に従って残対象の処理を継続。元ファイル名からの自動導出はしない。
#[test]
fn backup_push_title_input_paths() {
    // (1) from-list title欄指定: 当該titleで発行される
    let Some(fx) = StorageFixture::new("sb-titlein1", sb_opts()) else {
        return;
    };
    let seed = fx.data_root.join("seed.zip");
    write_file(&seed, b"seed-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&seed], "seed-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "seedの初回push: {stdout}");
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"title-input-a");
    let (code, _stdout, stderr) = fx.push_from_list(&[&seed], &[(&a, Some("list-title"))], &[]);
    assert_eq!(code, 0, "from-list title指定のpush: {stderr}");
    let a_uuid = sb_uuid_of(&fx, &a);
    let registry = fx.registry_values();
    let fw = registry
        .iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("uuid").and_then(|u| u.as_str()) == Some(a_uuid.as_str())
        })
        .expect("a.zipのfile_write");
    assert_eq!(fw["title"], "list-title", "from-listのtitleで発行: {fw}");

    // (2) 非tty+title無し新規対象
    let Some(fx) = StorageFixture::new("sb-titlein2", sb_opts()) else {
        return;
    };
    let seed = fx.data_root.join("seed.zip");
    write_file(&seed, b"seed-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&seed], "seed-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "seedの初回push: {stdout}");
    let ng = fx.data_root.join("ng.zip");
    let ok = fx.data_root.join("ok.zip");
    write_file(&ng, b"title-input-ng");
    write_file(&ok, b"title-input-ok");
    // (2a) --keep-going: 当該対象のみ失敗扱いで残対象は継続(全体exitは最初のエラー区分3)
    let (code, stdout, stderr) =
        fx.push_from_list(&[&ng], &[(&ok, Some("ok-title"))], &["--keep-going"]);
    assert_eq!(
        code, 3,
        "非tty+title無し新規は対象エラー3(keep-goingで継続): {stdout}{stderr}"
    );
    assert!(
        fx.registry_values().iter().any(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("source_path").and_then(|p| p.as_str())
                    == Some(ok.canonicalize().unwrap().to_string_lossy().as_ref())
        }),
        "残対象(ok.zip)は継続処理される: {stdout}{stderr}"
    );
    // (2b) --keep-going未指定: 対象エラー3で停止(後続のfrom-list対象は処理されない)
    let ng2 = fx.data_root.join("ng2.zip");
    let ok2 = fx.data_root.join("ok2.zip");
    write_file(&ng2, b"title-input-ng2");
    write_file(&ok2, b"title-input-ok2");
    let (code, _stdout, stderr) = fx.push_from_list(&[&ng2], &[(&ok2, Some("ok2-title"))], &[]);
    assert_eq!(code, 3, "非tty+title無し+停止: {stderr}");
    assert!(
        fx.registry_values().iter().all(|r| {
            r.get("source_path").and_then(|p| p.as_str())
                != Some(ok2.canonicalize().unwrap().to_string_lossy().as_ref())
        }),
        "未指定時は後続対象を処理しない"
    );

    // (3) tty+title無し: promptで空入力→再要求→有効値で発行
    let Some(fx) = StorageFixture::new("sb-titlein3", sb_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"title-input-prompt");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "\npty-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "pty promptのpushはexit 0: {stdout}");
    let registry = fx.registry_values();
    let fw = registry
        .iter()
        .find(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .expect("file_write");
    assert_eq!(
        fw["title"], "pty-title",
        "空入力は再要求され有効値で発行: {fw}"
    );
}

/// [covers:cli.backup-push-title-change-on-merge]
/// 合流・再PUT分岐でのtitle解決(TASK-18 4.1(resolve_title)・6.1): 指定titleが現行titleと
/// 異なればtitle変更扱い(新file_writeに新title記録=後勝ち+metadata新キーPUT+旧キー
/// delete marker)。未指定なら現行titleを継続(prompt不要)。同一title再指定は変更扱いに
/// ならない(metadataキー不変)。
#[test]
fn backup_push_title_change_on_merge() {
    let Some(fx) = StorageFixture::new("sb-tchg", sb_opts()) else {
        return;
    };
    let seed = fx.data_root.join("seed.zip");
    write_file(&seed, b"seed-plain");
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"title-change-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&seed, &a], "seed-title\ntitle-A", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push(title A): {stdout}");
    let uuid = sb_uuid_of(&fx, &a);

    // (1) 現行titleと異なるtitle指定: title変更扱い
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&a, b"title-change-plain");
    sb_clear_calls(&fx);
    let (code, stdout, stderr) = fx.push_from_list(&[&seed], &[(&a, Some("title-B"))], &[]);
    assert_eq!(code, 0, "title変更の合流push: {stdout}{stderr}");
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "uuid", &uuid);
    assert_eq!(
        entry.get("title").and_then(|t| t.as_str()),
        Some("title-B"),
        "entityのtitleが新titleへ変わる: {entry}"
    );
    assert!(
        fx.object_path(&fx.metadata_key(&uuid, "title-B")).is_file(),
        "metadata_<新title>.jsonがPUTされる"
    );
    let calls = fx.calls();
    assert!(
        calls.lines().any(|l| l.starts_with("s3 DeleteObject")
            && l.contains(&format!("key={}", fx.metadata_key(&uuid, "title-A")))),
        "旧キーmetadata_A.jsonへDELETE(delete marker): {calls}"
    );

    // (2) title省略: 現行title(B)継続・内容不変ならmetadata書換なし
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&a, b"title-change-plain");
    sb_clear_calls(&fx);
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "title省略の合流push: {stdout}{stderr}");
    let lines = fx.read_index();
    let entry = index_entity_by(&lines, "uuid", &uuid);
    assert_eq!(
        entry.get("title").and_then(|t| t.as_str()),
        Some("title-B"),
        "title省略は現行title継続: {entry}"
    );
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains("metadata_")),
        "同一内容ならmetadata書換なし: {}",
        fx.calls()
    );

    // (3) 同一title再指定: 変更扱いにならずmetadataキー不変
    std::thread::sleep(std::time::Duration::from_millis(1100));
    write_file(&a, b"title-change-plain");
    sb_clear_calls(&fx);
    let (code, stdout, stderr) = fx.push_from_list(&[&seed], &[(&a, Some("title-B"))], &[]);
    assert_eq!(code, 0, "同一title再指定の合流push: {stdout}{stderr}");
    assert!(
        fx.object_path(&fx.metadata_key(&uuid, "title-B")).is_file(),
        "metadataキーは不変(title-Bのまま)"
    );
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 DeleteObject") && l.contains("metadata_")),
        "同一title再指定で旧キーDELETEは行われない: {}",
        fx.calls()
    );
}

/// [covers:cli.backup-push-title-duplicate-warned]
/// title重複(別uuid同title)は許容+警告(TASK-18 4.1・4.3・6.1): 新規発行時の
/// warn_if_title_duplicateは既存entityとのtitle重複を警告表示するのみで発行は続行
/// (titleは識別子でなくラベル・一意性はuuidが担う)。
#[test]
fn backup_push_title_duplicate_warned_but_allowed() {
    let Some(fx) = StorageFixture::new("sb-dupwarn", sb_opts()) else {
        return;
    };
    let seed = fx.data_root.join("seed.zip");
    write_file(&seed, b"seed-plain");
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"duplicate-content-a");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&seed, &a], "seed-title\ndup-title", &[])
    else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");

    // 別内容の新規対象へ同一title → 警告表示のみで発行続行
    let b = fx.data_root.join("b.zip");
    write_file(&b, b"duplicate-content-b");
    let (code, stdout, stderr) = fx.push_from_list(&[&seed], &[(&b, Some("dup-title"))], &[]);
    assert_eq!(code, 0, "title重複でもexit 0: {stdout}{stderr}");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("WARNING") || combined.contains("重複"),
        "重複titleの警告表示: {combined}"
    );
    // 索引に同titleの2entity(uuid違い)が載る
    let a_uuid = sb_uuid_of(&fx, &a);
    let b_uuid = sb_uuid_of(&fx, &b);
    let lines = fx.read_index();
    for (tag, uuid) in [("a", &a_uuid), ("b", &b_uuid)] {
        let entry = index_entity_by(&lines, "uuid", uuid);
        assert_eq!(
            entry.get("title").and_then(|t| t.as_str()),
            Some("dup-title"),
            "同titleの2entity({tag}): {entry}"
        );
    }
    assert_ne!(a_uuid, b_uuid);
}

/// [covers:cli.backup-push-registry-loss-new-uuid]
/// registry喪失(索引はS3に残る)時の合流不能の許容(TASK-18 4.3境界条件表):
/// 合流正本がregistryのsha_uuid_lookupのため、同内容のbackupは合流できず重複uuidを
/// 発行する(許容。実害は内容重複と索引上の2entity。データ損失なし)。
#[test]
fn backup_push_registry_loss_issues_new_uuid() {
    let Some(fx) = StorageFixture::new("sb-regloss", sb_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"registry-loss-plain");
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "loss-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    let old_uuid = sb_uuid_of(&fx, &a);
    let old_key = fx.data_key(&old_uuid);
    let old_bytes = std::fs::read(fx.object_path(&old_key)).expect("旧object");

    // ローカルregistryを喪失させる(索引objectはS3に残る)
    std::fs::remove_file(&fx.registry).expect("registry.jsonlの削除");

    // 同内容・同mtimeでの再push: skipされず新規uuidが発行される(合流しない)
    let Some((code, stdout)) = sb_push_titled(&fx, &[&a], "loss-title", &[]) else {
        return;
    };
    assert_eq!(code, 0, "registry喪失後の再pushはexit 0: {stdout}");
    let new_uuid = sb_uuid_of(&fx, &a);
    assert_ne!(new_uuid, old_uuid, "合流できず重複uuidを発行(許容)");
    assert_eq!(
        std::fs::read(fx.object_path(&old_key)).expect("旧object(再読)"),
        old_bytes,
        "旧entityのdefault.ageは不変(データ損失なし)"
    );
    // 索引はregistry reduceから再構築されるため旧entityは載らず新entityのみ
    // (旧entityの実体はS3に残る=データ損失なし。回復はmanage index rebuild・
    // 設計1.2・4.3境界条件表「registry喪失」行)
    let lines = fx.read_index();
    index_entity_by(&lines, "uuid", &new_uuid);
    assert!(
        !lines.iter().any(|v| {
            v.get("record_type").and_then(|r| r.as_str()) == Some("file")
                && v.get("uuid").and_then(|u| u.as_str()) == Some(old_uuid.as_str())
        }),
        "registry喪失後の索引再構築で旧entityは載らない: {lines:?}"
    );
}

/// [covers:cli.removed-options-rejected]
/// TASK-18廃止面の拒否(6.5): manage prune --category/--keep・list files --tree・
/// list categories・探索のname(basename)語彙(--name)は廃止され、指定はusage error 64
/// (未知オプション/サブコマンド扱い)。内部toolのため移行措置なし。
#[test]
fn removed_options_are_rejected_with_usage_error() {
    let Some(fx) = StorageFixture::new("sb-removed", sb_opts()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"removed-options");
    // 対象解決・設定読込より前に弾かれるためpush済み状態は不要だが、
    // 索引不在でもusage errorが先に出ることを確認する(索引GET不可の状態で実行)
    sb_clear_calls(&fx);
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "manage prune --category",
            vec!["manage", "prune", "--category", "unfiled", "--apply"],
        ),
        (
            "manage prune --keep",
            vec!["manage", "prune", "--keep", "2", "--apply"],
        ),
        ("list files --tree", vec!["list", "files", "--tree"]),
        ("list categories", vec!["list", "categories"]),
        (
            "list versions --name",
            vec!["list", "versions", "--name", "x.zip"],
        ),
    ];
    let cfg_s = fx.cfg.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let key_s = fx.secret.display().to_string();
    for (tag, mut args) in cases {
        args.push("--config");
        args.push(&cfg_s);
        args.push("--aws-config");
        args.push(&aws_s);
        args.push("--key");
        args.push(&key_s);
        let state_s = fx.state.display().to_string();
        let (code, stdout, stderr) =
            run_bin_env(&args, &[("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str())]);
        assert_eq!(
            code, 64,
            "{tag}はusage error 64(未知オプション/サブコマンド扱い): {stdout}{stderr}"
        );
    }
    assert_eq!(fx.calls(), "", "S3 API呼び出しは行われない: {}", fx.calls());
}
