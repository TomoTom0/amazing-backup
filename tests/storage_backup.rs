//! TASK-10操作系CLI(backup push/pull・versioning確認・索引不在・災害時動作・
//! tmp/tasks/tm-10/design_s3_storage.md 2.4・3.2・5.1・7.3・8.3)と
//! TASK-10追加のconfig/encrypt条件(config.keep-artifacts-run-cleanup・
//! encrypt.skip-by-registry-record)の結合test。fake seam(BACKUP_REMOTE_FAKE_S3_STATE)の
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

/// [covers:cli.backup-push-integrated-run]
/// backup push統合正常系(設計3.2・4.1・7.3): 対象解決→分類解決→暗号化→upload
/// (default.age・manifest・索引更新)を1実行で行う。data/outは <out>/<batch_id>/<name>.age の
/// run単位構造。registryの新経路eventは新record_type(file_write・file_upload・manifest・
/// index_upload)で記録され、旧uploadのpending列挙(record_type=="encrypt"のみ対象)に
/// 拾われない=旧uploadによる新.ageの二重書き込みなし。
#[test]
fn backup_push_runs_integrated_pipeline() {
    let Some(fx) = StorageFixture::new(
        "sb-integrated",
        StorageOpts {
            keep_artifacts: "always",
            categories: "[categories.photos]\nkeep = 3\n".into(),
            files: "[files.\"album.zip\"]\ncategory = \"photos\"\npath = \"album\"\n".into(),
            ..StorageOpts::default()
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
    let (code, stdout, stderr) = fx.push(&[&travel, &album], &[], &[]);
    assert_eq!(code, 0, "backup pushはexit 0: {stdout}{stderr}");

    // 新key(default.age)・manifest・索引が更新される
    let travel_key = fx.file_key("unfiled", Some("2020-2023/travel"), "travel.zip");
    let album_key = fx.file_key("photos", Some("album"), "album.zip");
    for key in [&travel_key, &album_key] {
        assert!(fx.object_path(key).is_file(), "新key({key})へobject配置");
    }
    assert!(fx.object_path(&fx.index_key()).is_file(), "索引object配置");

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

    // registryに新record_typeの記録がappendされる
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
    assert_eq!(registry_count(&registry, "manifest"), 1, "manifest記録");
    assert_eq!(
        registry_count(&registry, "index_upload"),
        1,
        "index_upload記録"
    );

    // 旧uploadサブコマンドのdry-run(pending列挙)に新経路分が現れない
    let cfg_s = fx.cfg.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let state_s = fx.state.display().to_string();
    let (code, stdout, stderr) = run_bin_env(
        &[
            "upload",
            "--config",
            &cfg_s,
            "--aws-config",
            &aws_s,
            "--dry-run",
        ],
        &[("BACKUP_REMOTE_FAKE_S3_STATE", state_s.as_str())],
    );
    assert_eq!(
        code, 0,
        "旧upload dry-runはexit 0(対象なし): {stdout}{stderr}"
    );
    assert!(
        !stdout.contains("travel.zip.age") && !stdout.contains("album.zip.age"),
        "旧uploadのpending列挙に新経路分は現れない: {stdout}"
    );
}

/// [covers:cli.backup-push-dry-run-plan]
/// backup push --dry-runは旧plan相当(設計3.2): 対象・分類(category/rel_path)・衝突・
/// size集計・予定keyを表示し、暗号化(age実行)とS3 API呼び出しを行わない。
#[test]
fn backup_push_dry_run_plans_without_side_effects() {
    let Some(fx) = StorageFixture::new("sb-dryrun", StorageOpts::default()) else {
        return;
    };
    let travel = fx.data_root.join("travel.zip");
    write_file(&travel, b"dryrun-travel");
    let dup_a = fx.elsewhere.join("a").join("dup.zip");
    let dup_b = fx.elsewhere.join("b").join("dup.zip");
    write_file(&dup_a, b"dryrun-dup-a");
    write_file(&dup_b, b"dryrun-dup-b");

    let (code, stdout, stderr) = fx.push(&[&travel, &dup_a, &dup_b], &["--dry-run"], &[]);
    assert_eq!(code, 0, "backup push --dry-runはexit 0: {stdout}{stderr}");
    // 対象・分類・予定keyの表示
    assert!(stdout.contains("travel.zip"), "対象表示: {stdout}");
    assert!(stdout.contains("unfiled"), "分類(category)表示: {stdout}");
    let plain_key = fx.file_key("unfiled", None, "dup.zip");
    let hash_b = hash8_of_path(&dup_b);
    let hashed_key = fx.file_key("unfiled", None, &format!("dup--{hash_b}"));
    assert!(
        stdout.contains(&plain_key),
        "予定key表示(無hash側): {stdout}"
    );
    assert!(
        stdout.contains(&hashed_key),
        "衝突検出のhash付き予定key表示: {stdout}"
    );
    // 暗号化・S3 API呼び出し無し
    assert_eq!(fx.calls(), "", "S3 API呼び出し無し: {}", fx.calls());
    assert!(!fx.registry.is_file(), "registryへ記録しない");
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);

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
        &[("BACKUP_REMOTE_FAKE_S3_STATE", state_s.as_str())],
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
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
            &[("BACKUP_REMOTE_FAKE_S3_STATE", state_s.as_str())],
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
/// backup pull --run ID(設計3.2): 当該run(batch)対象へ一括で復元job発行+取得+検証+復号。
/// 対象外runのfileは取得されない(旧restore downloadのrun単位相当)。
#[test]
fn backup_pull_run_restores_only_run_scope() {
    let Some(fx) = StorageFixture::new("sb-pullrun", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.data_root.join("b.zip");
    let c = fx.data_root.join("c.zip");
    write_file(&a, b"pull-run-a");
    write_file(&b, b"pull-run-b");
    write_file(&c, b"pull-run-c");
    assert_eq!(fx.push(&[&a, &b], &[], &[]).0, 0);
    assert_eq!(fx.push(&[&c], &[], &[]).0, 0);
    let batch_ids = run_batch_ids(&fx);
    assert_eq!(batch_ids.len(), 2, "2 run: {batch_ids:?}");
    let run1 = &batch_ids[0];

    let out = fx.elsewhere.join("pullrun-out");
    let (code, stdout, stderr) =
        fx.backup_pull(&["--run", run1, "--yes", "--out", &out.display().to_string()]);
    assert_eq!(code, 0, "backup pull --runはexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("run1のa.zip"),
        b"pull-run-a".to_vec(),
        "run1対象の取得・復号"
    );
    assert_eq!(
        std::fs::read(out.join("b.zip")).expect("run1のb.zip"),
        b"pull-run-b".to_vec(),
        "run1対象の取得・復号(b)"
    );
    assert!(!out.join("c.zip").exists(), "対象外runのfileは取得されない");
    let c_key = fx.file_key("unfiled", None, "c.zip");
    assert!(
        !fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 GetObject") && l.contains(&format!("key={c_key}"))),
        "対象外runのkeyへのGetObjectは行われない: {}",
        fx.calls()
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);

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
    assert_eq!(fx.push(&[&travel], &[], &[]).0, 0);

    // 作成環境資産(pipeline.toml・registry)が不在の作業dirで実行
    let bare = fx.elsewhere.join("bare-workspace");
    std::fs::create_dir_all(&bare).expect("bare workspace");
    let aws_s = fx.aws_toml.display().to_string();
    let key_s = fx.secret.display().to_string();
    let state_s = fx.state.display().to_string();
    let env = [("BACKUP_REMOTE_FAKE_S3_STATE", state_s.as_str())];

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
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "push成功: {stdout}{stderr}");
    assert_no_age_files(
        &fx.out_dir,
        "成功後はdata/out配下の当該run分.ageが削除される",
    );
    // objectはupload済み(掃除はローカル成果物のみ)
    assert!(
        fx.object_path(&fx.file_key("unfiled", None, "a.zip"))
            .is_file(),
        "S3側のobjectは残る"
    );

    // 失敗時は残る(失敗注入でexit 5)
    let Some(fx2) = StorageFixture::new("sb-keepfail", StorageOpts::default()) else {
        return;
    };
    let b = fx2.data_root.join("b.zip");
    write_file(&b, b"keep-fail-b");
    let fail_key = fx2.file_key("unfiled", None, "b.zip");
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
    assert!(
        fx2.object_path(&fx2.file_key("unfiled", None, "b.zip"))
            .is_file(),
        "S3側のobjectは残る"
    );
}

/// [covers:encrypt.skip-by-registry-record]
/// encrypt skip判定のregistry基準化(設計8.1 registry.rs行・8.3): file_write成功記録+
/// source指紋(size/mtime)一致で .age 不在でもskipされる(data/out掃除後の再実行で
/// 重複暗号化が起きない)。skip時は平文read・age実行・registry追記なし。
#[test]
fn encrypt_skip_is_based_on_registry_record() {
    let Some(fx) = StorageFixture::new("sb-skipreg", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"skip-registry-plain");
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "初回push: {stdout}{stderr}");
    let key = fx.file_key("unfiled", None, "a.zip");
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
    let Some(fx) = StorageFixture::new("sb-idxskip", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"idxskip-v1");
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
    let index_key = fx.index_key();

    // v2 push: 索引PUTのみ失敗注入(file upload・registry記録は成功)
    write_file(&a, b"idxskip-v2");
    let (code, _, _) = fx.push(&[&a], &[], &[("FAKE_S3_FAIL_CP_FOR", index_key.as_str())]);
    assert_eq!(code, 5, "索引PUT失敗は転送エラー5");
    assert!(
        registry_count(&fx.registry_values(), "file_upload") >= 2,
        "file_upload記録は済んでいる(部分失敗)"
    );

    // 同内容の再実行: 全skipでも索引が更新される
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "全skip再実行はexit 0: {stdout}{stderr}");
    assert!(stdout.contains("skip"), "skip表示: {stdout}");
    assert!(
        fx.object_path(&index_key).is_file(),
        "索引objectは存在(初回pushで作成済み)"
    );
    let lines = fx.read_index();
    let entry = index_file_entry(&lines, "a.zip");
    let vids = index_version_ids(entry);
    let reg_ids = registry_upload_version_ids(
        &fx.registry_values(),
        &fx.file_key("unfiled", None, "a.zip"),
    );
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
    assert_eq!(fx.push(&[&a], &[], &[]).0, 0);
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
        &[("BACKUP_REMOTE_FAKE_S3_STATE", state_s.as_str())],
    );
    assert_eq!(code, 2, "--configなしpin addは設定エラー2: {stderr}");
    assert_eq!(
        read_calls(&fx.state),
        "",
        "S3 API呼び出しは行われない: {}",
        read_calls(&fx.state)
    );
}
