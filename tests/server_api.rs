//! TASK-9(serve基盤・tmp/tasks/tm-9/design_catalog-frontend-auth.md 1章/6章/8章/9章/10章)
//! Phase P2(serve基盤)条件の結合test(bin subprocess spawn + 素朴HTTP client + SIGTERM観測)。
//! 実装前(src/server.rs・serve subcommand未実装)のため新条件のテストは実行failを許容
//! (cargo test --no-runのcompileは通る)。実装後に全件passへ切り替わる。
//! src/server.rs内のhandler unit testはP3実装時にsrc側へ追加する。

mod common;

use common::{free_local_port, make_archive, wait_http_ready, TestDir};
use std::path::PathBuf;

/// serveを起動し(指定portのserver.tomlを書いて)listen待ち合わせる。
/// 戻り値: (addr, BinChild, stderr log path)。
fn start_serve(fx: &common::StorageFixture, port: u16, tag: &str) -> (String, common::BinChild, PathBuf) {
    let body = format!("[server]\nlisten_addr = \"127.0.0.1\"\nport = {port}\n");
    fx.write_server_toml(&body);
    let stderr_path = fx
        .state
        .parent()
        .expect("tmp dir")
        .join(format!("serve-{tag}-{port}.log"));
    let child = fx.spawn_serve(&["serve"], &[], Some(&stderr_path));
    let addr = format!("127.0.0.1:{port}");
    assert!(
        wait_http_ready(&addr, 100),
        "serveがlistenしない(実装前=serve未知のexit 64を含む): stderr={}",
        std::fs::read_to_string(&stderr_path).unwrap_or_default()
    );
    (addr, child, stderr_path)
}

/// [covers:server.serve-subcommand-health] serve起動とGET /api/healthのliveness応答
#[test]
fn serve_subcommand_responds_health() {
    let Some(fx) = common::StorageFixture::new("srv-health", common::StorageOpts::default()) else {
        return;
    };
    let (addr, mut child, _err) = start_serve(&fx, free_local_port(), "health");
    let (code, body) = common::http_get(&addr, "/api/health").expect("health応答");
    assert_eq!(code, 200, "GET /api/healthは200: body={body}");
    // 起動継続の確認(連続応答)
    let (code2, _) = common::http_get(&addr, "/api/health").expect("health再応答");
    assert_eq!(code2, 200);
    let _ = child.send_sigterm();
    let _ = child.wait();
}

/// [covers:server.server-toml-listen-config] server.tomlのlisten_addr/port反映と既定40510
#[test]
fn serve_listens_per_server_toml_and_default_port() {
    let Some(fx) = common::StorageFixture::new("srv-toml", common::StorageOpts::default()) else {
        return;
    };
    // 明示port
    let port = free_local_port();
    let (addr, mut child, _err) = start_serve(&fx, port, "explicit");
    let (code, _) = common::http_get(&addr, "/api/health").expect("明示portで応答");
    assert_eq!(code, 200, "server.tomlのport明示時に当該portでlisten");
    let _ = child.send_sigterm();
    let _ = child.wait();

    // port key省略時は既定40510(D5本番serve)でlistenする
    fx.write_server_toml("[server]\nlisten_addr = \"127.0.0.1\"\n");
    let stderr_path = fx
        .state
        .parent()
        .expect("tmp dir")
        .join("serve-default-port.log");
    let mut child2 = fx.spawn_serve(&["serve"], &[], Some(&stderr_path));
    let addr = "127.0.0.1:40510";
    assert!(
        wait_http_ready(addr, 100),
        "port省略時は既定40510でlisten: stderr={}",
        std::fs::read_to_string(&stderr_path).unwrap_or_default()
    );
    let (code, _) = common::http_get(addr, "/api/health").expect("40510で応答");
    assert_eq!(code, 200, "既定port 40510でhealth応答");
    let _ = child2.send_sigterm();
    let _ = child2.wait();
}

/// [covers:server.config-init-generates-server-toml] config initがserver.tomlを生成
#[test]
fn config_init_generates_server_toml() {
    let tmp = TestDir::new("srv-init");
    let home = tmp.path().join("abh");
    std::fs::create_dir_all(&home).expect("home dir");
    let (code, stdout, stderr) = common::run_bin_env(
        &["config", "init"],
        &[("AMAZING_BACKUP_HOME", &home.display().to_string())],
    );
    assert_eq!(code, 0, "config init: {stdout}{stderr}");
    let server_toml = std::fs::read_to_string(home.join("server.toml"))
        .expect("server.tomlが生成されている(実装前は不在でfailを許容)");
    assert!(
        server_toml.contains("listen_addr = \"127.0.0.1\""),
        "listen_addr既定値を含む: {server_toml}"
    );
    assert!(
        server_toml.contains("port = 40510"),
        "port既定値40510(D5本番serve)を含む: {server_toml}"
    );
    // 既存3fileの生成は従来どおり
    for name in ["pipeline.toml", "aws.toml", "aws-credentials.toml"] {
        assert!(home.join(name).is_file(), "{name}も生成される");
    }
}

/// [covers:server.dist-missing-api-only-with-warning] dist欠落時もAPIのみ起動+warning表示
#[test]
fn serve_dist_missing_starts_api_only_with_warning() {
    let Some(fx) = common::StorageFixture::new("srv-dist", common::StorageOpts::default()) else {
        return;
    };
    let (addr, mut child, stderr_path) = start_serve(&fx, free_local_port(), "dist");
    // APIは応答する(静的配信なし=APIのみで起動)
    let (code, _) = common::http_get(&addr, "/api/health").expect("health応答");
    assert_eq!(code, 200, "dist欠落時もAPIは応答する");
    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    assert!(
        (stderr.contains("警告") || stderr.to_lowercase().contains("warn"))
            && (stderr.contains("frontend") || stderr.contains("dist") || stderr.contains("build")),
        "dist欠落のwarning+frontend build手順が表示される: {stderr}"
    );
    let _ = child.send_sigterm();
    let _ = child.wait();
}

/// [covers:server.protected-endpoints-fail-closed] 未認証の保護endpointはfail-closed
#[test]
fn serve_protected_endpoints_fail_closed() {
    let Some(fx) = common::StorageFixture::new("srv-closed", common::StorageOpts::default()) else {
        return;
    };
    // データが存在する状態で観測(保護対象のデータが応答に漏れないことまで確認)
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");

    let (addr, mut child, _err) = start_serve(&fx, free_local_port(), "closed");
    let (code, body) = common::http_get(&addr, "/api/entities").expect("応答");
    assert!(
        code == 401 || code == 503,
        "未認証の保護endpointは401/503(fail-closed): status={code} body={body}"
    );
    assert!(
        !body.contains("alpha"),
        "保護対象データは応答に含まれない: {body}"
    );
    let _ = child.send_sigterm();
    let _ = child.wait();
}

/// [covers:server.graceful-shutdown] SIGTERMでgracefulにexit 0
#[test]
fn serve_shuts_down_gracefully_on_sigterm() {
    let Some(fx) = common::StorageFixture::new("srv-term", common::StorageOpts::default()) else {
        return;
    };
    let (addr, mut child, _err) = start_serve(&fx, free_local_port(), "term");
    let (code, _) = common::http_get(&addr, "/api/health").expect("health応答");
    assert_eq!(code, 200);
    child.send_sigterm();
    let exit = child.wait();
    assert_eq!(exit, Some(0), "SIGTERMで正常終了(exit 0)");
}

/// [covers:server.startup-sync-blocking-boundary] 起動時syncはasync境界経由でpanicしない
#[test]
fn serve_startup_sync_runs_without_panic() {
    let Some(fx) = common::StorageFixture::new("srv-startup", common::StorageOpts::default()) else {
        return;
    };
    // 索引objectが実在する状態(起動時HEAD validator検証+DB反映が実際に走る構成)
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");

    let (addr, mut child, stderr_path) = start_serve(&fx, free_local_port(), "startup");
    let (code, _) = common::http_get(&addr, "/api/health").expect("health応答");
    assert_eq!(code, 200, "起動時syncを通じてpanicせず起動する");
    let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
    assert!(
        !stderr.contains("panicked"),
        "Runtime混在panicが発生していない: {stderr}"
    );
    let _ = child.send_sigterm();
    let _ = child.wait();
}
