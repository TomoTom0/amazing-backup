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

    // XDG_STATE_HOME解決側(ABH未指定): <state>/amazing-backup/ が0700で作られる。
    // 初回syncもremote(HEAD)確認から始めるためfake S3 stateを渡す(索引objectは
    // 事前pushでfake state内にある。権限観測が本題のためS3接続はfake seamで足す)
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
        &[
            ("XDG_STATE_HOME", &xdg_state.display().to_string()),
            (
                "AMAZING_BACKUP_FAKE_S3_STATE",
                &fx.state.display().to_string(),
            ),
        ],
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

/// [covers:catalog.db-permissions-remediate-existing]
/// 既存dir(0755)・既存DB file(0644)の権限不備をopen前に是正する(chmod失敗を
/// 無視せず、既存平文DBへの接続前に0600を保証する=レビュー指摘#1)。
#[test]
fn catalog_db_permissions_remediate_existing_dir_and_file() {
    use std::os::unix::fs::PermissionsExt;
    let Some(fx) = StorageFixture::new("cat-perm-fix", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");

    // 既存環境の権限緩みを人為的に作る(0755 dir・0644 DB)
    let db = fx.catalog_db();
    std::fs::set_permissions(&fx.home, std::fs::Permissions::from_mode(0o755))
        .expect("ABH dirを0755へ緩める");
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644))
        .expect("catalog.dbを0644へ緩める");

    // 再syncは接続前に権限を是正して成功する
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "権限緩み状態での再sync: {stdout}{stderr}");
    let dmeta = std::fs::metadata(&fx.home).expect("ABH dir");
    assert_eq!(
        dmeta.permissions().mode() & 0o777,
        0o700,
        "既存dirは0700へ是正される"
    );
    let meta = std::fs::metadata(&db).expect("catalog.db");
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o600,
        "既存DB fileは0600へ是正される"
    );
    // WALも同一境界(fake S3+WAL運用で作られていれば0600)
    for ext in ["-wal", "-shm"] {
        let mut p = db.as_os_str().to_os_string();
        p.push(ext);
        let side = std::path::PathBuf::from(p);
        if side.exists() {
            let m = std::fs::metadata(&side).expect("WAL/SHM metadata");
            assert_eq!(
                m.permissions().mode() & 0o777,
                0o600,
                "WAL/SHM({ext})も0600"
            );
        }
    }
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

/// [covers:catalog.sync-fetches-replica-on-change]
/// local履歴(registry・sync_stateとも)不在でもremote索引の存在をHEADで確認して
/// から取得する(別端末・復旧環境の初回syncがremote確認なしで空をfresh確定しない
/// =レビュー指摘#3。local履歴の不在はremote索引の不在の証拠にならない)。
#[test]
fn catalog_first_sync_on_new_host_fetches_remote_index() {
    let Some(fx) = StorageFixture::new("cat-first", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");

    // 別端末・復旧環境相当: localのregistry・catalog DBを消した状態
    // (remoteのfake S3 stateには索引objectが残る)
    std::fs::remove_file(&fx.registry).expect("registry削除");
    std::fs::remove_file(fx.catalog_db()).expect("catalog.db削除");

    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "初回sync(別端末相当): {stdout}{stderr}");
    let calls = snapshot_calls(&fx);
    let index_key = fx.index_key();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 GetObject") && l.contains(&format!("key={index_key}"))),
        "remote索引をHEAD確認のうえ取得する: {calls}"
    );
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "sync後のlist: {stdout}{stderr}");
    assert!(
        stdout.contains("alpha"),
        "remote索引の内容を同期している(空をfresh確定しない): {stdout}"
    );
    assert_eq!(sync_state_status(&fx), "fresh", "同期結果はfresh確定");
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

/// catalog.dbのsync_state.statusを読む(single scope前提・行不在はpanic)。
fn sync_state_status(fx: &StorageFixture) -> String {
    let conn = rusqlite::Connection::open(fx.catalog_db()).expect("catalog.db open");
    conn.query_row("SELECT status FROM sync_state", [], |r| r.get(0))
        .expect("sync_state行(sync済み前提)")
}

/// [covers:catalog.sync-fetches-replica-on-change]
/// [covers:catalog.append-then-reflect-with-stale-on-failure]
/// 正常DBのままHEAD失敗・書込反映(HEAD)失敗を注入した場合も旧freshを残さない
/// (syncはstatus=error・pushの反映失敗は当該scopeのstale化=レビュー指摘#6)。
#[test]
fn catalog_sync_and_reflect_failures_mark_status_on_healthy_db() {
    let Some(fx) = StorageFixture::new("cat-stale", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");
    assert_eq!(sync_state_status(&fx), "fresh", "事前はfresh");
    let index_key = fx.index_key();

    // syncのHEAD失敗: 正常DBのままerror化(旧freshを残さない)
    let (code, _stdout, _stderr) = fx.manage_env(
        "catalog",
        &["sync"],
        &[("FAKE_S3_FAIL_HEAD_FOR", index_key.as_str())],
    );
    assert_eq!(code, 5, "HEAD失敗注入のsyncは転送エラー");
    assert_eq!(
        sync_state_status(&fx),
        "error",
        "HEAD失敗もerror化経路(status=error)"
    );

    // 再syncでfresh復帰することも確認(失敗が恒続化しない)
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "再sync: {stdout}{stderr}");
    assert_eq!(sync_state_status(&fx), "fresh", "再syncでfresh復帰");

    // pushの書込反映(reflect内のHEAD)失敗: push自体は継続し当該scopeをstale化
    let b = fx.data_root.join("b.zip");
    make_archive(&b, 2048);
    let (code, stdout, stderr) = fx.push_from_list_env(
        &[],
        &[(&b, Some("beta"))],
        &[],
        &[("FAKE_S3_FAIL_HEAD_FOR", index_key.as_str())],
    );
    assert_eq!(code, 0, "反映HEAD失敗でもpush自体は成功: {stdout}{stderr}");
    assert!(
        stderr.contains("catalog")
            && (stderr.contains("警告") || stderr.to_lowercase().contains("warn")),
        "反映失敗の警告がstderrへ表示される: {stderr}"
    );
    assert_ne!(
        sync_state_status(&fx),
        "fresh",
        "反映失敗時は当該scopeをstale化(旧freshを残さない)"
    );
}

/// [covers:catalog.sync-invalidation-on-replace-failure-and-missing-index]
/// [covers:catalog.sync-registry-judgment-failure-error]
/// syncのcache置換(replace_scope)失敗・書込履歴があるnamespaceでの索引object不在も
/// 失効させてからエラー終了する(置換失敗=error・索引不在=stale。いずれも正常DBの
/// まま旧freshを残さない=レビュー再指摘#6)。索引不在時の未書込判定(registry読取)
/// 失敗もerror化してエラー終了する。
#[test]
fn catalog_sync_replace_failure_and_missing_index_invalidate_status() {
    let Some(fx) = StorageFixture::new("cat-sync-inv", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(事前): {stdout}{stderr}");
    assert_eq!(sync_state_status(&fx), "fresh", "事前はfresh");

    // (1) 置換失敗: 正常DBのままversions挿入のみを失敗させる(trigger注入。
    // sync_stateへのUPDATE(mark_status)には影響しない)
    {
        let conn = rusqlite::Connection::open(fx.catalog_db()).expect("catalog.db open");
        conn.execute_batch(
            "CREATE TRIGGER fail_versions_insert BEFORE INSERT ON versions\
             \nBEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .expect("置換失敗注入triggerの作成");
    }
    // 索引objectを書換えてvalidatorを変化させる(HEAD不一致で置換が走る状態へ)
    let mut lines = fx.read_index();
    assert!(!lines.is_empty(), "索引にfile行がある(push済み前提)");
    for l in lines.iter_mut() {
        if l["record_type"] == "file" {
            l["title"] = serde_json::Value::String("alpha2".into());
        }
    }
    fx.rewrite_index(&lines);
    let (code, _stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 1, "置換失敗注入のsyncは内部エラー: {stderr}");
    assert_eq!(
        sync_state_status(&fx),
        "error",
        "置換失敗はerror化(rollback後も旧freshを残さない)"
    );

    // 失敗は恒続化しない: 注入を解除すれば再syncでfresh復帰
    {
        let conn = rusqlite::Connection::open(fx.catalog_db()).expect("catalog.db open");
        conn.execute_batch("DROP TRIGGER fail_versions_insert")
            .expect("置換失敗注入triggerの解除");
    }
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "解除後の再sync: {stdout}{stderr}");
    assert_eq!(sync_state_status(&fx), "fresh", "再syncでfresh復帰");

    // (2) 書込履歴があるnamespaceでremote索引object不在(HEADで不在確認):
    // stale化してから案内エラー(旧freshを残さない)
    std::fs::remove_file(fx.object_path(&fx.index_key())).expect("索引objectの削除");
    let (code, _stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 2, "索引不在は案内エラー(Config): {stderr}");
    assert_eq!(
        sync_state_status(&fx),
        "stale",
        "索引不在はstale化(旧freshを残さない)"
    );
    assert!(
        stderr.contains("manage index rebuild"),
        "復旧案内を表示: {stderr}"
    );

    // (3) 索引不在のままregistryが破損(未書込判定の読取・parse失敗): 判定失敗も
    // error化してエラー終了(早期returnで旧状態(stale)を持ち越さない)
    let registry_text = std::fs::read_to_string(&fx.registry).expect("registry読取");
    std::fs::write(
        &fx.registry,
        format!("{registry_text}not-json-line\n"),
    )
    .expect("registry破損書込");
    let (code, _stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 4, "registry破損の未書込判定失敗は処理エラー: {stderr}");
    assert_eq!(
        sync_state_status(&fx),
        "error",
        "判定失敗はerror化(旧状態を残さない)"
    );

    // registryを復旧すれば再び案内エラー経路へ戻る(失敗が恒続化しない)
    std::fs::write(&fx.registry, registry_text).expect("registry復旧書込");
    let (code, _stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 2, "復旧後は索引不在の案内エラーへ戻る: {stderr}");
    assert_eq!(sync_state_status(&fx), "stale", "復旧後はstale化へ戻る");
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

/// [covers:catalog.json-format-compat]
/// 同一bucketに複数prefixの記録が混在する索引でも --refresh(S3直接経路)と次回の
/// cache hit(DB経路)の出力が一致する(両経路へDB保存側と同一のscope分離filterを
/// 適用=レビュー指摘#9)。
#[test]
fn catalog_json_format_compat_multi_prefix_scope() {
    let opts = StorageOpts {
        extra_pipeline_profiles: vec!["prod".to_string()],
        ..StorageOpts::default()
    };
    let Some(fx) = StorageFixture::new("cat-json-mp", opts) else {
        return;
    };
    // aws.tomlを2 profile(dev=amazing-backup/・prod=prod-ns/。同一bucket・別prefix)へ
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

    // dev・prod両方へpush(索引objectはbucket単位で構築され両prefixの記録を含む)
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(dev): {stdout}{stderr}");
    let b = fx.data_root.join("b.zip");
    make_archive(&b, 2048);
    let (code, stdout, stderr) =
        fx.push_from_list_env(&[], &[(&b, Some("beta"))], &["--profile", "prod"], &[]);
    assert_eq!(code, 0, "push(prod): {stdout}{stderr}");

    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(dev): {stdout}{stderr}");

    // dev側scopeの照会: cache hit(DB経路)と--refresh(S3直接経路)の出力が一致
    // (混在索引から当該prefix外の記録を両経路とも同一規則で除外する)
    let (code, cached, stderr) = fx.list("files", &["--format", "json"]);
    assert_eq!(code, 0, "list(cache hit): {cached}{stderr}");
    assert!(cached.contains("alpha"), "dev側の内容: {cached}");
    let (code, refreshed, stderr) = fx.list("files", &["--refresh", "--format", "json"]);
    assert_eq!(code, 0, "list --refresh(S3直接): {refreshed}{stderr}");
    assert_eq!(
        cached, refreshed,
        "複数prefix混在索引でも経路間で出力が一致(他prefixの記録は両経路とも混入しない)"
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

// --- manage auth api-key(TASK-13設計D12・2.2/2.5章: api-key生成・list統合表示・
//     remove両table・0件案内合計判定) ---
//     実装前のためapi_keys table不在のrusqlite直接操作・未知subcommandのexit 64に
//     より本系testは実行failを許容(赤)。

/// SHA-256 hex符号化(api_keysのkey_hash算出=設計2.1と同一形式)。
fn sha256_hex(s: &str) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(s.as_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// api_keys tableへ1行挿入(rusqlite直接=実装後のinsert_api_key APIと同一schema)。
fn insert_api_key(fx: &StorageFixture, credential_id: &str, key: &str, name: &str) {
    let conn = rusqlite::Connection::open(fx.catalog_db()).expect("catalog.db open");
    conn.execute(
        "INSERT INTO api_keys (credential_id, key_hash, name, created_at) \
         VALUES (?1, ?2, ?3, '2026-11-05T00:00:00+09:00')",
        rusqlite::params![credential_id, sha256_hex(key), name],
    )
    .expect("api_keys挿入(実装前はtable不在でfail=赤)");
}

/// catalog.dbのapi_keys行(credential_id, key_hash, name, created_at)を読む。
fn api_key_rows(fx: &StorageFixture) -> Vec<(String, String, String, String)> {
    let conn = rusqlite::Connection::open(fx.catalog_db()).expect("catalog.db open");
    let mut stmt = conn
        .prepare(
            "SELECT credential_id, key_hash, name, created_at FROM api_keys \
             ORDER BY credential_id",
        )
        .expect("api_keys準備(migration済みschema)");
    let rows = stmt
        .query_map(
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .expect("api_keys読込");
    rows.collect::<Result<Vec<_>, _>>()
        .expect("api_keys行読取")
}

/// [covers:cli.manage-auth-api-key-generates]
/// manage auth api-keyは`abk_`前置のkeyを1回だけstdout表示し・SHA-256(key) hexを
/// api_keys tableへ保存する(再表示不可・hashから復元不可)。直後のlistに表示。
#[test]
fn manage_auth_api_key_generates_and_shows_once() {
    let Some(fx) = StorageFixture::new("cli-auth-apikey-gen", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(DB作成・事前): {stdout}{stderr}");

    let (code, stdout, stderr) = fx.manage("auth", &["api-key", "--name", "laptop"]);
    assert_eq!(code, 0, "manage auth api-key: {stdout}{stderr}");
    // abk_前置のkeyがstdoutに1回だけ表示される(再表示しない)
    let key = stdout
        .split_whitespace()
        .find(|w| w.starts_with("abk_"))
        .expect("stdoutにabk_前置のkey表示");
    assert_eq!(
        stdout.matches(key).count(),
        1,
        "key表示は1回のみ(再表示不可): {stdout}"
    );
    // api_keys tableへ1行(key_hash=SHA-256(key全文)hex・credential_idはak-前置)
    let rows = api_key_rows(&fx);
    assert_eq!(rows.len(), 1, "api_keys 1行: {rows:?}");
    let (id, hash, name, created) = &rows[0];
    assert!(id.starts_with("ak-"), "credential_idはak-前置: {id}");
    assert_eq!(hash, &sha256_hex(key), "key_hash=SHA-256(key全文)hex");
    assert_eq!(name, "laptop", "--name指定: {rows:?}");
    assert!(!created.is_empty(), "created_at非空: {rows:?}");
    // 直後のlistに当該credential_idが表示(平文keyは再表示しない)
    let (code, stdout, stderr) = fx.manage("auth", &["list"]);
    assert_eq!(code, 0, "list: {stdout}{stderr}");
    assert!(stdout.contains(id.as_str()), "listにapi-key行: {stdout}");
    assert!(
        !stdout.contains(key),
        "listに平文keyは再表示しない: {stdout}"
    );
}

/// [covers:cli.manage-auth-api-key-rejects-empty-name]
/// manage auth api-key --name ""(空文字)はusage error(exit 64)でapi_keysへ保存しない
/// (name未指定時の既定"api-key"は現状維持=cli.manage-auth-api-key-generatesが担保)。
#[test]
fn manage_auth_api_key_empty_name_is_usage_error() {
    let Some(fx) = StorageFixture::new("cli-auth-apikey-empty-name", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(DB作成・事前): {stdout}{stderr}");

    let (code, stdout, stderr) = fx.manage("auth", &["api-key", "--name", ""]);
    assert_eq!(code, 64, "空文字nameはusage error: {stdout}{stderr}");
    assert!(
        stderr.contains("--nameは空文字を指定できません"),
        "usage error文言: {stderr}"
    );
    assert_eq!(api_key_rows(&fx).len(), 0, "api_keysへ保存しない");
}

/// [covers:cli.manage-auth-list-integrates-api-keys]
/// manage auth listはcredentials(passkey)とapi_keys(api-key)を統合表示する
/// (kind区別付き・合計件数フッタ)。
#[test]
fn manage_auth_list_integrates_passkey_and_api_key() {
    let Some(fx) = StorageFixture::new("cli-auth-list-integ", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(DB作成・事前): {stdout}{stderr}");
    insert_credential(&fx, "cred-a", "laptop-passkey");
    insert_api_key(&fx, "ak-list-1", "abk_list_testkey_0001", "scan-key");

    let (code, stdout, stderr) = fx.manage("auth", &["list"]);
    assert_eq!(code, 0, "list: {stdout}{stderr}");
    assert!(
        stdout.contains("cred-a") && stdout.contains("laptop-passkey"),
        "passkey行表示: {stdout}"
    );
    assert!(
        stdout.contains("ak-list-1") && stdout.contains("scan-key"),
        "api-key行表示: {stdout}"
    );
    assert!(stdout.contains("passkey"), "kind=passkey: {stdout}");
    assert!(stdout.contains("api_key"), "kind=api_key: {stdout}");
    assert!(stdout.contains("2件"), "合計2件のフッタ: {stdout}");
}

/// [covers:cli.manage-auth-remove-both-tables]
/// [covers:cli.manage-auth-list-and-remove]
/// manage auth removeはcredentials→api_keysの順に完全一致削除する(api-keyの
/// credential_idでも削除可)。合計削除0行は対象エラー3。
#[test]
fn manage_auth_remove_deletes_from_both_tables() {
    let Some(fx) = StorageFixture::new("cli-auth-rm-both", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(DB作成・事前): {stdout}{stderr}");
    insert_credential(&fx, "cred-a", "laptop");
    insert_api_key(&fx, "ak-rm-1", "abk_rm_testkey_0001", "api-key");

    // api-key id指定: api_keysのみ削除(credentialsは不変)
    let (code, stdout, stderr) = fx.manage("auth", &["remove", "ak-rm-1"]);
    assert_eq!(code, 0, "api-key idのremove: {stdout}{stderr}");
    assert_eq!(api_key_rows(&fx).len(), 0, "api_keys 0行");
    assert_eq!(credential_rows(&fx).len(), 1, "credentials不変");

    // passkey id指定: credentialsのみ削除(api_keysは不変)
    insert_api_key(&fx, "ak-rm-2", "abk_rm_testkey_0002", "api-key");
    let (code, stdout, stderr) = fx.manage("auth", &["remove", "cred-a"]);
    assert_eq!(code, 0, "passkey idのremove: {stdout}{stderr}");
    assert_eq!(credential_rows(&fx).len(), 0, "credentials 0行");
    assert_eq!(api_key_rows(&fx).len(), 1, "api_keys不変");

    // 存在しないid(両table合計削除0行)は対象エラー3・削除しない
    insert_credential(&fx, "cred-b", "phone");
    let (code, stdout, stderr) = fx.manage("auth", &["remove", "cred-missing"]);
    assert_eq!(
        code, 3,
        "合計削除0行は対象エラー3: {stdout}{stderr}"
    );
    assert_eq!(credential_rows(&fx).len(), 1, "失敗時はcredentials不変");
    assert_eq!(api_key_rows(&fx).len(), 1, "失敗時はapi_keys不変");
}

/// [covers:cli.manage-auth-zero-guides-total-count]
/// [covers:cli.manage-auth-remove-all-guides-rebootstrap]
/// 0件案内(bootstrap案内)・削除後の再bootstrap案内は両table合計0件時のみ
/// (api_keys残存時はapi-key login可能のため案内しない)。
#[test]
fn manage_auth_zero_guides_by_total_count() {
    let Some(fx) = StorageFixture::new("cli-auth-zero-total", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(DB作成・事前): {stdout}{stderr}");

    // api_keys 1行のみ(credentials 0): 合計1件のため案内しない
    insert_api_key(&fx, "ak-z-1", "abk_zero_testkey_01", "api-key");
    let (code, stdout, stderr) = fx.manage("auth", &["list"]);
    assert_eq!(code, 0, "list: {stdout}{stderr}");
    assert!(
        !stdout.contains("--bootstrap"),
        "api_keys残存時のlistはbootstrap案内なし(合計1件): {stdout}"
    );

    // 削除(合計0件): 再bootstrap案内を表示
    let (code, stdout, stderr) = fx.manage("auth", &["remove", "ak-z-1"]);
    assert_eq!(code, 0, "remove: {stdout}{stderr}");
    assert!(
        stdout.contains("--bootstrap"),
        "合計0件のremoveは再bootstrap案内: {stdout}"
    );

    // 両table 0件でのlistも案内
    let (code, stdout, stderr) = fx.manage("auth", &["list"]);
    assert_eq!(code, 0, "0件list: {stdout}{stderr}");
    assert!(
        stdout.contains("--bootstrap"),
        "0件listはbootstrap案内: {stdout}"
    );
}

/// [covers:catalog.api-keys-table-persistent-class]
/// api_keys tableは永続class: manage catalog rebuildのcache class置換でも
/// dropされない(credentials/push_intentsと同一契約)。
#[test]
fn api_keys_table_survives_rebuild() {
    let Some(fx) = StorageFixture::new("cat-apikey-persist", StorageOpts::default()) else {
        return;
    };
    let (code, stdout, stderr) = fx.manage("catalog", &["sync"]);
    assert_eq!(code, 0, "sync(DB作成・事前): {stdout}{stderr}");
    insert_api_key(&fx, "ak-p-1", "abk_persist_testkey_1", "api-key");

    let (code, stdout, stderr) = fx.manage("catalog", &["rebuild"]);
    assert_eq!(code, 0, "rebuild: {stdout}{stderr}");
    assert_eq!(
        api_key_rows(&fx).len(),
        1,
        "rebuild後もapi_keys行は保持(永続class)"
    );
}
