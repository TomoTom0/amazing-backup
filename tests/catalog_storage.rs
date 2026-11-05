//! TASK-9(local catalog DB・tmp/tasks/tm-9/design_catalog-frontend-auth.md 2章/4章/5章)
//! Phase P1(DB層)条件の結合test(bin subprocess + fake seam観測)。
//! 実装前(src/catalog.rs・manage catalog系未実装)のため新条件のテストは実行failを許容
//! (cargo test --no-runのcompileは通る)。実装後に全件passへ切り替わる。
//! src/catalog.rs直接参照のunit test(schema詳細・WAL mode等)は実装時にsrc側へ追加する。

mod common;

use common::{
    StorageFixture, StorageOpts, aws_lifecycle_toml, make_archive, read_calls, run_bin_env,
    write_file,
};

/// calls.log追記分(実行前後の差分行)。
fn added_calls<'a>(before: &'a str, after: &'a str) -> Vec<&'a str> {
    let n = before.lines().count();
    after.lines().skip(n).collect()
}

/// calls.log中の当該keyへの操作行数。
fn calls_count(added: &[&str], op: &str, key: &str) -> usize {
    added
        .iter()
        .filter(|l| l.starts_with(op) && l.contains(&format!("key={key}")))
        .count()
}

/// fake S3 stateの呼び出し記録を区切り付きで読む(実行前後の差分採取用)。
fn snapshot_calls(fx: &StorageFixture) -> String {
    read_calls(&fx.state)
}

/// [covers:catalog.db-path-and-file-permissions] DB格納先と権限(0700 dir・0600 file)
#[test]
fn catalog_db_path_and_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let Some(fx) = StorageFixture::new("cat-perm", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(
        code, 0,
        "manage catalog sync(実装前は64でfailを許容): {stdout}{stderr}"
    );

    let db = fx.catalog_db();
    let meta = std::fs::metadata(&db).expect("$ABH/catalog.dbが作成されている");
    assert!(meta.is_file(), "catalog.dbはfile");
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o600,
        "DB fileは0600(group/other読取不可)"
    );

    // XDG_STATE_HOME解決側(ABH未指定): <state>/amazing-backup/ が0700で作られる
    let xdg_state = fx.state.parent().expect("tmp dir").join("xdg-state");
    let (code, stdout, stderr) = run_bin_env(
        &[
            "manage",
            "catalog",
            "sync",
            "--config",
            &fx.cfg.display().to_string(),
            "--aws-config",
            &fx.aws_toml.display().to_string(),
            "--key",
            &fx.secret.display().to_string(),
        ],
        &[("XDG_STATE_HOME", &xdg_state.display().to_string())],
    );
    assert_eq!(code, 0, "manage catalog sync(XDG): {stdout}{stderr}");
    let dir = xdg_state.join("amazing-backup");
    let dmeta = std::fs::metadata(&dir).expect("XDG state側の専用dirが作成されている");
    assert!(dmeta.is_dir());
    assert_eq!(dmeta.permissions().mode() & 0o777, 0o700, "専用dirは0700");
    let xdb = dir.join("catalog.db");
    let xmeta = std::fs::metadata(&xdb).expect("XDG側catalog.dbが作成されている");
    assert_eq!(xmeta.permissions().mode() & 0o777, 0o600);
}

/// [covers:catalog.schema-migration] 新規作成でSQLite DB fileが生成される(migration冪等)
#[test]
fn catalog_db_created_as_sqlite_and_reopen_safe() {
    let Some(fx) = StorageFixture::new("cat-schema", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");

    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "初回sync: {stdout}{stderr}");
    let bytes = std::fs::read(fx.catalog_db()).expect("catalog.db読込");
    assert!(
        bytes.len() >= 16 && &bytes[..15] == b"SQLite format 3",
        "SQLite database magic headerを持つ(file長={})",
        bytes.len()
    );

    // 同一DBへの再実行(migration再適用・冪等)
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "再sync(migration冪等): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "DB再オープン後の照会: {stdout}{stderr}");
    assert!(stdout.contains("alpha"), "既存内容が壊れていない: {stdout}");
}

/// [covers:catalog.sync-fetches-replica-on-change] syncはvalidator比較で変化時のみ索引GET
#[test]
fn catalog_sync_fetches_replica_on_change_only() {
    let Some(fx) = StorageFixture::new("cat-sync", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let index_key = fx.index_key();

    // 初回sync: validator取得(HEAD)+変化あり(初回)でGET+DB反映
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "初回sync: {stdout}{stderr}");
    let calls1 = snapshot_calls(&fx);
    assert!(
        calls1
            .lines()
            .any(|l| l.starts_with("s3 GetObject") && l.contains(&format!("key={index_key}"))),
        "初回syncは索引objectを取得する: {calls1}"
    );

    // 2回目sync(変化なし): HEADのみでGETを行わない
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "2回目sync: {stdout}{stderr}");
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert_eq!(
        calls_count(&added, "s3 GetObject", &index_key),
        0,
        "変化なしの2回目syncは索引GETを行わない: {added:?}"
    );

    // 索引objectを書換えた後(validator変化): 再度GET
    let lines = fx.read_index();
    fx.rewrite_index(&lines);
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "3回目sync(変化後): {stdout}{stderr}");
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert_eq!(
        calls_count(&added, "s3 GetObject", &index_key),
        1,
        "索引変化後のsyncは再度取得する: {added:?}"
    );
}

/// [covers:catalog.list-cache-hit-after-head-check] 検証済みfresh時の照会はHEAD 1回でS3 GETをskip
#[test]
fn catalog_list_cache_hit_after_head_check() {
    let Some(fx) = StorageFixture::new("cat-hit", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");
    let index_key = fx.index_key();

    // list files(別process起動): HEAD validator確認のみでcache hit
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "list files: {stdout}{stderr}");
    assert!(
        stdout.contains("alpha"),
        "cache経由でも索引内容を表示: {stdout}"
    );
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert_eq!(
        calls_count(&added, "s3 GetObject", &index_key),
        0,
        "検証済みfresh時は索引GETを行わない(cache hit): {added:?}"
    );
    assert!(
        calls_count(&added, "s3 HeadObject", &index_key) >= 1,
        "別process起動時のHEAD validator確認は行う: {added:?}"
    );

    // queryも同一契約
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.query("alpha", &[]);
    assert_eq!(code, 0, "query: {stdout}{stderr}");
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert_eq!(
        calls_count(&added, "s3 GetObject", &index_key),
        0,
        "queryもcache hit: {added:?}"
    );
}

/// [covers:catalog.cache-miss-refreshes-db] validator不一致時はS3再取得+DB反映・失敗時は転送エラー
#[test]
fn catalog_cache_miss_refreshes_db() {
    let Some(fx) = StorageFixture::new("cat-miss", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");
    let index_key = fx.index_key();

    // 索引objectを書換える(validator変化=stale)
    let mut lines = fx.read_index();
    for l in lines.iter_mut() {
        if l.get("record_type").and_then(|t| t.as_str()) == Some("file") {
            l["title"] = serde_json::Value::String("alpha2".into());
        }
    }
    fx.rewrite_index(&lines);

    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "stale時のlist(従来経路): {stdout}{stderr}");
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert_eq!(
        calls_count(&added, "s3 GetObject", &index_key),
        1,
        "validator不一致時は従来経路で索引GET: {added:?}"
    );

    // DB反映済み=続く実行はcache hit
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "再実行: {stdout}{stderr}");
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert_eq!(
        calls_count(&added, "s3 GetObject", &index_key),
        0,
        "反映後の再実行はcache hit: {added:?}"
    );

    // 転送失敗注入: 索引GETが失敗する状態でのlistは転送エラー5
    let mut lines = fx.read_index();
    for l in lines.iter_mut() {
        if l.get("record_type").and_then(|t| t.as_str()) == Some("file") {
            l["title"] = serde_json::Value::String("alpha3".into());
        }
    }
    fx.rewrite_index(&lines);
    let (code, stdout, stderr) =
        fx.list_env("files", &[], &[("FAKE_S3_FAIL_CP_FOR", index_key.as_str())]);
    assert_eq!(code, 5, "取得失敗時は転送エラー: {stdout}{stderr}");
}

/// [covers:catalog.append-then-reflect-with-stale-on-failure] 書込系のDB反映と反映失敗時のstale化警告
#[test]
fn catalog_push_reflects_db_and_stales_on_failure() {
    let Some(fx) = StorageFixture::new("cat-reflect", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");
    let index_key = fx.index_key();

    // 通常のpush: 同一process内でDB反映=次の照会はcache hit
    let b = fx.data_root.join("b.zip");
    make_archive(&b, 2048);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&b, Some("beta"))], &[]);
    assert_eq!(code, 0, "push b.zip: {stdout}{stderr}");
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "push後のlist: {stdout}{stderr}");
    assert!(stdout.contains("beta"), "push反映済み内容を表示: {stdout}");
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert_eq!(
        calls_count(&added, "s3 GetObject", &index_key),
        0,
        "push後の照会はcache hit(同一process反映): {added:?}"
    );

    // catalog.dbを破損状態にした場合のpush: JSONL正本のためpush自体は成功+警告
    write_file(&fx.catalog_db(), b"this is not a sqlite database at all");
    let c = fx.data_root.join("c.zip");
    make_archive(&c, 512);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&c, Some("gamma"))], &[]);
    assert_eq!(code, 0, "破損DB下のpush(正本はJSONL): {stdout}{stderr}");
    assert!(
        stderr.contains("catalog")
            && (stderr.contains("警告") || stderr.to_lowercase().contains("warn")),
        "DB反映失敗の警告がstderrへ表示される: {stderr}"
    );
    // 続く照会はS3経路で回答する(cacheを持ち越さない)
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "破損後のlist: {stdout}{stderr}");
    assert!(
        stdout.contains("gamma"),
        "S3経路でも最新内容を表示: {stdout}"
    );
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert!(
        calls_count(&added, "s3 GetObject", &index_key) >= 1,
        "fresh持ち越しをせずS3経路で取得する: {added:?}"
    );
}

/// [covers:catalog.rebuild-scoped-to-cache-class] rebuildはcache classのみ置換・冪等
#[test]
fn catalog_rebuild_scoped_and_idempotent() {
    let Some(fx) = StorageFixture::new("cat-rebuild", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");

    let (code, stdout, stderr) = fx.manage("catalog", &["rebuild"]);
    assert_eq!(code, 0, "rebuild: {stdout}{stderr}");
    let (code, first, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "rebuild後のlist: {first}{stderr}");
    assert!(first.contains("alpha"), "registry由来の内容: {first}");

    let (code, stdout, stderr) = fx.manage("catalog", &["rebuild"]);
    assert_eq!(code, 0, "rebuild 2回目: {stdout}{stderr}");
    let (code, second, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "rebuild 2回目後のlist: {second}{stderr}");
    assert_eq!(first, second, "再構築は冪等(同一照会結果)");
}

/// [covers:catalog.scope-isolation-multi-profile] 複数scope混在時の衝突・上書き防止
#[test]
fn catalog_scope_isolation_multi_profile() {
    let opts = StorageOpts {
        extra_pipeline_profiles: vec!["prod".to_string()],
        ..StorageOpts::default()
    };
    let Some(fx) = StorageFixture::new("cat-scope", opts) else {
        return;
    };
    // aws.tomlを2 profile(dev=amazing-backup/・prod=prod-ns/。同一bucket・別prefix)へ書換え
    let body = format!(
        "[aws]\nactive_profile = \"dev\"\n\n\
         [aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\n\
         prefix = \"{}\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{}\"\n\n\
         [aws.profiles.prod]\npurpose = \"prod\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\n\
         prefix = \"prod-ns/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{}\"\n{}",
        fx.prefix,
        fx.credentials.display(),
        fx.credentials.display(),
        aws_lifecycle_toml(180, 10, 7),
    );
    fx.rewrite_aws_toml(&body);

    // dev側: push a.zip + sync
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(dev): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(dev): {stdout}{stderr}");

    // prod側: push b.zip + sync(--profile prod)
    let b = fx.data_root.join("b.zip");
    make_archive(&b, 2048);
    let (code, stdout, stderr) =
        fx.push_from_list_env(&[], &[(&b, Some("beta"))], &["--profile", "prod"], &[]);
    assert_eq!(code, 0, "push(prod): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync", "--profile", "prod"]);
    assert_eq!(code, 0, "sync(prod): {stdout}{stderr}");

    // 各profileの照会は当該scopeの内容のみ(後からsyncしたprodがdevのcacheを上書きしない)
    let (code, dev_out, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "list(dev): {dev_out}{stderr}");
    assert!(dev_out.contains("alpha"), "dev側の内容: {dev_out}");
    assert!(
        !dev_out.contains("beta"),
        "他scope(prod)のentityは混入しない: {dev_out}"
    );

    let (code, prod_out, stderr) = fx.list("files", &["--profile", "prod"]);
    assert_eq!(code, 0, "list(prod): {prod_out}{stderr}");
    assert!(prod_out.contains("beta"), "prod側の内容: {prod_out}");
    assert!(
        !prod_out.contains("alpha"),
        "他scope(dev)のentityは混入しない: {prod_out}"
    );
}

/// [covers:catalog.cli-refresh-flag] --refreshはvalidatorに関係なくS3再取得+DB更新
#[test]
fn catalog_cli_refresh_flag_forces_reload() {
    let Some(fx) = StorageFixture::new("cat-refresh", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");
    let index_key = fx.index_key();

    // 変化なし(fresh)状態でも--refreshは索引GETを行う
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.list("files", &["--refresh"]);
    assert_eq!(code, 0, "list --refresh: {stdout}{stderr}");
    assert!(
        stdout.contains("alpha"),
        "再取得した索引内容を表示: {stdout}"
    );
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert_eq!(
        calls_count(&added, "s3 GetObject", &index_key),
        1,
        "--refreshは強制的にS3 GETする: {added:?}"
    );
}

/// [covers:catalog.json-format-compat] --format json出力はcache/S3両経路で完全互換
#[test]
fn catalog_json_format_compat_between_routes() {
    let Some(fx) = StorageFixture::new("cat-json", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");

    let (code, cached, stderr) = fx.list("files", &["--format", "json"]);
    assert_eq!(code, 0, "list --format json(cache hit): {cached}{stderr}");
    let (code, refreshed, stderr) = fx.list("files", &["--refresh", "--format", "json"]);
    assert_eq!(
        code, 0,
        "list --refresh --format json(S3): {refreshed}{stderr}"
    );
    assert_eq!(
        cached, refreshed,
        "cache経路とS3再取得経路の--format json出力は等価"
    );
    // JSONとしてparse可能なことも確認(語彙の変化が無いことの土台)
    assert!(
        serde_json::from_str::<serde_json::Value>(&cached).is_ok()
            || cached.lines().all(|l| {
                !l.trim().is_empty() && serde_json::from_str::<serde_json::Value>(l.trim()).is_ok()
            }),
        "--format json出力はJSONとしてparse可能: {cached}"
    );
}

/// [covers:catalog.s3-direct-commands-unchanged] diff・get・list jobsは従来どおりS3直接
#[test]
fn catalog_s3_direct_commands_unchanged() {
    let Some(fx) = StorageFixture::new("cat-direct", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");

    // diff: ListObjectVersions等のS3呼び出しを行う(cache skipされた空応答でない)
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.diff(&[]);
    assert_eq!(code, 0, "diff: {stdout}{stderr}");
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert!(
        added.iter().any(|l| l.starts_with("s3 ListObjectVersions")),
        "diffはS3のListObjectVersionsを呼ぶ: {added:?}"
    );

    // get: 索引・実体ともS3から取得する
    let out = fx.out_dir.join("get-out");
    let (code, stdout, stderr) = fx.get("alpha", &["--out", &out.display().to_string()]);
    assert_eq!(code, 0, "get: {stdout}{stderr}");
    let uuid = common::uuid_of_path(&fx, &a);
    let data_key = fx.data_key(&uuid);
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert!(
        added
            .iter()
            .any(|l| l.starts_with("s3 GetObject") && l.contains(&format!("key={data_key}"))),
        "getは実体objectをS3から取得する: {added:?}"
    );

    // list jobs: S3呼び出しを行う
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.list("jobs", &[]);
    assert_eq!(code, 0, "list jobs: {stdout}{stderr}");
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert!(
        !added.is_empty(),
        "list jobsはS3 API呼び出しを行う(cache skipされない): {added:?}"
    );
}
