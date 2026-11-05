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

// ---------------------------------------------------------------------------
// TASK-9 Phase P5(pending catalog)条件の結合test(テスト先行。実装前のため
// manage catalog scan未知subcommandのexit 64等で実行failを許容・compileは通る。
// scanはlocal FS走査+catalog DB比較のみのためfake S3は不要だがStorageFixtureの
// 構成(pipeline.toml・catalog.db)を共用する)。

/// pipeline.tomlへ[profiles.dev.catalog.scan]を追記する。現行のpipeline schemaは
/// 未知sectionを拒否(deny_unknown_fields)するため、追記はpush・rebuildなど
/// pipeline.tomlを読む操作の完了後(scan直前)に行うこと(P5実装でschemaに
/// catalog.scanが加わればこの制約は解消される)。
fn add_scan_section(
    fx: &StorageFixture,
    roots: &[&std::path::Path],
    extensions: &[&str],
    exclude: &[&str],
) {
    let roots_toml = roots
        .iter()
        .map(|r| format!("\"{}\"", r.display()))
        .collect::<Vec<_>>()
        .join(", ");
    let mut section = format!("\n[profiles.dev.catalog.scan]\nroots = [{roots_toml}]\n");
    if !extensions.is_empty() {
        let ext = extensions
            .iter()
            .map(|e| format!("\"{e}\""))
            .collect::<Vec<_>>()
            .join(", ");
        section.push_str(&format!("extensions = [{ext}]\n"));
    }
    if !exclude.is_empty() {
        let ex = exclude
            .iter()
            .map(|e| format!("\"{e}\""))
            .collect::<Vec<_>>()
            .join(", ");
        section.push_str(&format!("exclude = [{ex}]\n"));
    }
    fx.rewrite_pipeline_toml(&format!("{}{}", fx.pipeline_body(), section));
}

/// catalog.dbのpush_intents行(kind, payload_json)をID順に読む(永続classの直接観測)。
fn push_intents_rows(fx: &StorageFixture) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(fx.catalog_db()).expect("catalog.db open");
    let mut stmt = conn
        .prepare("SELECT kind, payload_json FROM push_intents ORDER BY id")
        .expect("push_intents準備(migration済みschema)");
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .expect("push_intents読込");
    rows.collect::<Result<Vec<_>, _>>()
        .expect("push_intents行読取")
}

/// catalog.dbのpending_items件数(status絞り込み任意。scan全置換の直接観測)。
fn pending_items_count(fx: &StorageFixture, status: Option<&str>) -> i64 {
    let conn = rusqlite::Connection::open(fx.catalog_db()).expect("catalog.db open");
    match status {
        Some(s) => conn
            .query_row(
                "SELECT COUNT(*) FROM pending_items WHERE status = ?1",
                [s],
                |r| r.get(0),
            )
            .expect("pending_items件数(status絞り込み)"),
        None => conn
            .query_row("SELECT COUNT(*) FROM pending_items", [], |r| r.get(0))
            .expect("pending_items件数"),
    }
}

/// [covers:catalog.scan-targets-in-pipeline-profile]
/// scan対象はpipeline.toml [profiles.dev.catalog.scan]の登録のみ(auto_path_rootsは
/// 転用しない)。未登録は0件でexit 0・scanはS3 APIを呼ばない・roots外は走査しない。
#[test]
fn catalog_scan_targets_registered_in_pipeline_profile() {
    let Some(fx) = StorageFixture::new("cat-scan-reg", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");

    // 未登録(auto_path_rootsにはdata_rootが含まれるが[catalog.scan]は無い):
    // auto_path_rootsを転用せず0件走査でexit 0
    let (code, stdout, stderr) = fx.manage("catalog", &["scan"]);
    assert_eq!(code, 0, "未登録のscanは0件で正常終了: {stdout}{stderr}");
    assert!(stdout.contains("scan: 0件"), "0件走査: {stdout}");

    // roots登録: data_root配下のみ走査(roots外のelsewhere配下は対象外)
    let out = fx.elsewhere.join("out.zip");
    make_archive(&out, 512);
    add_scan_section(&fx, &[&fx.data_root], &[], &[]);
    let before = snapshot_calls(&fx);
    let (code, stdout, stderr) = fx.manage("catalog", &["scan"]);
    assert_eq!(code, 0, "roots登録後のscan: {stdout}{stderr}");
    assert!(
        stdout.contains("scan: 1件"),
        "roots配下の1fileのみを走査: {stdout}"
    );
    // scanはlocal FS走査+catalog DB比較のみ(S3 APIを呼ばない)
    let after = snapshot_calls(&fx);
    let added = added_calls(&before, &after);
    assert!(
        added.iter().all(|l| !l.starts_with("s3 ")),
        "scanはS3 APIを呼ばない: {added:?}"
    );
}

/// [covers:catalog.scan-file-selection]
/// extensions(拡張子)・exclude(相対path部分一致)・symlink既定不追従の対象選択。
#[test]
fn catalog_scan_selects_files_by_extension_exclude_and_symlinks() {
    let Some(fx) = StorageFixture::new("cat-scan-sel", StorageOpts::default()) else {
        return;
    };
    let x = fx.data_root.join("x.zip");
    make_archive(&x, 256);
    write_file(&fx.data_root.join("y.txt"), b"not a zip target");
    let skip_dir = fx.data_root.join("skip");
    std::fs::create_dir_all(&skip_dir).expect("skip dir");
    make_archive(&skip_dir.join("z.zip"), 256);
    // directory symlink(既定で辿らない): link先に対象拡張子fileを置く
    make_archive(&fx.elsewhere.join("linked.zip"), 256);
    std::os::unix::fs::symlink(&fx.elsewhere, fx.data_root.join("link")).expect("symlink作成");

    add_scan_section(&fx, &[&fx.data_root], &[".zip"], &["skip"]);
    let (code, stdout, stderr) = fx.manage("catalog", &["scan"]);
    assert_eq!(code, 0, "scan: {stdout}{stderr}");
    assert!(
        stdout.contains("scan: 1件"),
        "対象はx.zipのみ(y.txt=拡張子対象外・skip/z.zip=除外・symlink先=不追従): {stdout}"
    );
}

/// [covers:catalog.scan-classification-statuses]
/// 4状態分類: ok(path/basename一致+size/mtime一致)・changed(path一致+不一致)・
/// unbacked(記録なし)・unknown(basename一致のみ+size/mtime不一致=判定不能を偽装しない)。
#[test]
fn catalog_scan_classifies_four_statuses() {
    let Some(fx) = StorageFixture::new("cat-scan-cls", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.data_root.join("b.zip");
    let d = fx.data_root.join("d.zip");
    let e = fx.data_root.join("e.zip");
    for (p, sz) in [(&a, 1024usize), (&b, 512), (&d, 256), (&e, 128)] {
        make_archive(p, sz);
    }
    let (code, stdout, stderr) = fx.push_from_list(
        &[],
        &[
            (&a, Some("alpha")),
            (&b, Some("beta")),
            (&d, Some("delta")),
            (&e, Some("echo")),
        ],
        &[],
    );
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["rebuild"]);
    assert_eq!(code, 0, "rebuild(比較基準のmtime供給): {stdout}{stderr}");

    // FS変更: b.zip=内容変更・c.zip=新規(未backup)・d.zip=moved/配下へ移動(mtime不変)・
    // other/e.zip=同名異内容
    make_archive(&b, 2048);
    let c = fx.data_root.join("c.zip");
    make_archive(&c, 300);
    let moved = fx.data_root.join("moved");
    std::fs::create_dir_all(&moved).expect("moved dir");
    std::fs::rename(&d, moved.join("d.zip")).expect("d.zip移動");
    let other = fx.data_root.join("other");
    std::fs::create_dir_all(&other).expect("other dir");
    make_archive(&other.join("e.zip"), 999);

    add_scan_section(&fx, &[&fx.data_root], &[], &[]);
    let (code, stdout, stderr) = fx.manage("catalog", &["scan"]);
    assert_eq!(code, 0, "scan: {stdout}{stderr}");
    assert!(stdout.contains("unbacked=1"), "c.zipはunbacked: {stdout}");
    assert!(stdout.contains("changed=1"), "b.zipはchanged: {stdout}");
    assert!(
        stdout.contains("ok=3"),
        "a.zip+moved/d.zip+e.zipはok: {stdout}"
    );
    assert!(
        stdout.contains("unknown=1"),
        "other/e.zipはunknown(basename一致+mtime不一致=内容不明): {stdout}"
    );
}

/// [covers:catalog.scan-sha-merge-option]
/// sha256再計算は--sha256指定時のみ: unknown判定をsha_uuid_lookup合流でokへ確定。
#[test]
fn catalog_scan_sha_option_confirms_merge_by_content() {
    let Some(fx) = StorageFixture::new("cat-scan-sha", StorageOpts::default()) else {
        return;
    };
    let f = fx.data_root.join("f.zip");
    make_archive(&f, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&f, Some("fox"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["rebuild"]);
    assert_eq!(code, 0, "rebuild(事前): {stdout}{stderr}");
    // 同一内容のcopy(size一致・mtime相違=未指定scanではbasename一致+mtime不一致)
    let copy_dir = fx.data_root.join("copy");
    std::fs::create_dir_all(&copy_dir).expect("copy dir");
    let copied = copy_dir.join("f.zip");
    std::fs::copy(&f, &copied).expect("copy");
    // mtimeを明示的にずらす(copy時刻とpush時刻の偶然一致を排除してunknownを確定)
    let file = std::fs::File::options()
        .write(true)
        .open(&copied)
        .expect("copy open");
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600))
        .expect("mtime設定");

    add_scan_section(&fx, &[&fx.data_root], &[], &[]);
    let (code, stdout, stderr) = fx.manage("catalog", &["scan"]);
    assert_eq!(code, 0, "scan(既定): {stdout}{stderr}");
    assert!(
        stdout.contains("unknown=1"),
        "sha再計算なしはcopy/f.zip=unknown: {stdout}"
    );
    assert!(stdout.contains("ok=1"), "f.zip本体=ok: {stdout}");

    let (code, stdout, stderr) = fx.manage("catalog", &["scan", "--sha256"]);
    assert_eq!(code, 0, "scan --sha256: {stdout}{stderr}");
    assert!(
        stdout.contains("unknown=0"),
        "sha一致で合流確定しunknown解消: {stdout}"
    );
    assert!(
        stdout.contains("ok=2"),
        "copy/f.zipはokへ確定(sha_uuid_lookup合流): {stdout}"
    );
}

/// [covers:catalog.scan-replaces-pending-items]
/// scan結果は当該scopeのpending_itemsを全置換(冪等)・rebuild/syncはpending_itemsに触れない。
#[test]
fn catalog_scan_replaces_pending_items_and_rebuild_keeps_them() {
    let Some(fx) = StorageFixture::new("cat-scan-rep", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["rebuild"]);
    assert_eq!(code, 0, "rebuild(事前): {stdout}{stderr}");
    let c = fx.data_root.join("c.zip");
    make_archive(&c, 300);

    add_scan_section(&fx, &[&fx.data_root], &[], &[]);
    let (code, stdout, stderr) = fx.manage("catalog", &["scan"]);
    assert_eq!(code, 0, "scan: {stdout}{stderr}");
    assert_eq!(
        pending_items_count(&fx, Some("unbacked")),
        1,
        "c.zip=unbacked"
    );
    assert_eq!(
        pending_items_count(&fx, None),
        2,
        "a.zip(ok)+c.zip(unbacked)"
    );

    // 再scanは全置換(前回結果を残さない=二重計上されない・冪等)
    let (code, stdout, stderr) = fx.manage("catalog", &["scan"]);
    assert_eq!(code, 0, "scan再実行: {stdout}{stderr}");
    assert_eq!(pending_items_count(&fx, None), 2, "全置換で行数不変(冪等)");

    // 対象外となったfileの行は再scanで消える
    std::fs::remove_file(&c).expect("c.zip削除");
    let (code, stdout, stderr) = fx.manage("catalog", &["scan"]);
    assert_eq!(code, 0, "削除後scan: {stdout}{stderr}");
    assert_eq!(pending_items_count(&fx, None), 1, "削除fileの行が消える");

    // rebuildはpending_itemsに触れない(置換を行うのは再scanのみ)
    let (code, stdout, stderr) = fx.manage("catalog", &["rebuild"]);
    assert_eq!(code, 0, "rebuild: {stdout}{stderr}");
    assert_eq!(
        pending_items_count(&fx, None),
        1,
        "rebuild後もpending_items保持"
    );
}

/// [covers:catalog.push-intents-recorded]
/// push dry-run/--from-list実行のpush_intents記録(永続class・rebuildで消えない)。
#[test]
fn push_intents_recorded_for_dry_run_and_from_list() {
    let Some(fx) = StorageFixture::new("cat-intents", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(DB作成・事前): {stdout}{stderr}");

    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    // from-list実push: kind=from_listで記録
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(from-list): {stdout}{stderr}");
    // positional再push(skip): 通常pushは記録しない
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "push(positional・skip): {stdout}{stderr}");
    let rows = push_intents_rows(&fx);
    assert_eq!(rows.len(), 1, "通常push(positional)は記録しない: {rows:?}");
    assert_eq!(rows[0].0, "from_list", "kind=from_list: {rows:?}");
    assert!(
        rows[0].1.contains("a.zip"),
        "payloadに投入対象を含む: {rows:?}"
    );

    // dry-run: kind=dry_runで記録
    let (code, stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 0, "push --dry-run(skip予測): {stdout}{stderr}");
    let rows = push_intents_rows(&fx);
    assert_eq!(rows.len(), 2, "dry-runは記録する: {rows:?}");
    assert_eq!(rows[1].0, "dry_run", "kind=dry_run: {rows:?}");
    assert!(
        rows[1].1.contains("a.zip"),
        "payloadに投入対象を含む: {rows:?}"
    );

    // rebuildで消えない(永続class)
    let (code, stdout, stderr) = fx.manage("catalog", &["rebuild"]);
    assert_eq!(code, 0, "rebuild: {stdout}{stderr}");
    assert_eq!(
        push_intents_rows(&fx).len(),
        2,
        "rebuild後も記録は保持(永続class)"
    );
}

// --- manage auth(TASK-9設計8章・12章P6: 失効はmanage auth remove(CLI)・
//     全credential紛失時はcredentials table削除+再bootstrap=指摘#8) ---

/// catalog.dbのcredentials行(credential_id, name)を読む(manage auth観測用)。
fn credential_rows(fx: &StorageFixture) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(fx.catalog_db()).expect("catalog.db open");
    let mut stmt = conn
        .prepare("SELECT credential_id, name FROM credentials ORDER BY credential_id")
        .expect("credentials準備(migration済みschema)");
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .expect("credentials読込");
    rows.collect::<Result<Vec<_>, _>>()
        .expect("credentials行読取")
}

/// credentials tableへ1行挿入(rusqlite直接=P1 APIと同一schema)。
fn insert_credential(fx: &StorageFixture, credential_id: &str, name: &str) {
    let conn = rusqlite::Connection::open(fx.catalog_db()).expect("catalog.db open");
    conn.execute(
        "INSERT INTO credentials (credential_id, passkey_json, name, created_at) \
         VALUES (?1, '{}', ?2, '2026-11-05T00:00:00+09:00')",
        rusqlite::params![credential_id, name],
    )
    .expect("credentials挿入");
}

/// [covers:cli.manage-auth-list-and-remove]
/// manage auth listはcredential一覧(id・name)を表示し、manage auth remove <id>は
/// credential_id完全一致の1件を削除する(S3接続不要)。存在しないidは対象エラー3。
#[test]
fn manage_auth_lists_and_removes_credential() {
    let Some(fx) = StorageFixture::new("cli-auth-rm", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(DB作成・事前): {stdout}{stderr}");
    insert_credential(&fx, "cred-a", "laptop");
    insert_credential(&fx, "cred-b", "phone");
    assert_eq!(credential_rows(&fx).len(), 2, "事前2行");

    // 一覧: 両credentialの行(credential_id・name)を表示
    let (code, stdout, stderr) = fx.manage("auth", &["list"]);
    assert_eq!(code, 0, "manage auth list: {stdout}{stderr}");
    assert!(stdout.contains("cred-a"), "id表示: {stdout}");
    assert!(stdout.contains("laptop"), "name表示: {stdout}");
    assert!(
        stdout.contains("cred-b") && stdout.contains("phone"),
        "2行表示: {stdout}"
    );

    // 1件削除: 当該1行のみ(他行は不変)
    let (code, stdout, stderr) = fx.manage("auth", &["remove", "cred-a"]);
    assert_eq!(code, 0, "manage auth remove: {stdout}{stderr}");
    let rows = credential_rows(&fx);
    assert_eq!(rows.len(), 1, "残1行: {rows:?}");
    assert_eq!(rows[0].0, "cred-b", "他行は不変: {rows:?}");

    // 存在しないcredential_idは対象エラー3
    let (code, stdout, stderr) = fx.manage("auth", &["remove", "cred-missing"]);
    assert_eq!(code, 3, "存在しないidは対象エラー3: {stdout}{stderr}");
    assert_eq!(credential_rows(&fx).len(), 1, "失敗時は削除しない");
}

/// [covers:cli.manage-auth-remove-all-guides-rebootstrap]
/// 全credential削除(残0件)時は再bootstrap案内を表示して未登録状態へ戻る
/// (credential 0での認証閉鎖のserver側契約はwebauthn.register-closed-without-valid-token
/// 等が担保。本testはCLI表示とcredentials 0遷移を観測)。
#[test]
fn manage_auth_remove_last_guides_rebootstrap() {
    let Some(fx) = StorageFixture::new("cli-auth-rm-all", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(DB作成・事前): {stdout}{stderr}");
    insert_credential(&fx, "cred-a", "laptop");

    let (code, stdout, stderr) = fx.manage("auth", &["remove", "cred-a"]);
    assert_eq!(code, 0, "最後の1件削除: {stdout}{stderr}");
    assert!(
        stdout.contains("--bootstrap"),
        "再bootstrap案内(serve --bootstrap): {stdout}"
    );
    assert_eq!(credential_rows(&fx).len(), 0, "credentials 0行=未登録状態");
}
