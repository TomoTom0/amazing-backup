//! TASK-9(serve基盤・tmp/tasks/tm-9/design_catalog-frontend-auth.md 1章/6章/8章/9章/10章)
//! Phase P2(serve基盤)条件の結合test(bin subprocess spawn + 素朴HTTP client + SIGTERM観測)。
//! 実装前(src/server.rs・serve subcommand未実装)のため新条件のテストは実行failを許容
//! (cargo test --no-runのcompileは通る)。実装後に全件passへ切り替わる。
//! src/server.rs内のhandler unit testはP3実装時にsrc側へ追加する。

mod common;

use common::{TestDir, free_local_port, make_archive, wait_http_ready};
use std::path::PathBuf;

/// serveを起動し(指定portのserver.tomlを書いて)listen待ち合わせる。
/// 戻り値: (addr, BinChild, stderr log path)。
fn start_serve(
    fx: &common::StorageFixture,
    port: u16,
    tag: &str,
) -> (String, common::BinChild, PathBuf) {
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

/// [covers:server.protected-endpoints-fail-closed]
/// P3で読取API endpointが実装された後も、serve経由はP4(認証)までfail-closedの
/// まま(指摘#11: P3の機密APIはP4までfail-closed。handler単体testとは別にserve経由の
/// 閉鎖維持を回帰検出する)。P3読取APIのendpoint群(+POST)が未認証で内容を返さない。
#[test]
fn serve_p3_read_api_still_fail_closed_until_auth() {
    let Some(fx) = common::StorageFixture::new("srv-closed-p3", common::StorageOpts::default())
    else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");

    let (addr, mut child, _err) = start_serve(&fx, free_local_port(), "closed-p3");
    for path in [
        "/api/entities",
        "/api/pins",
        "/api/locations",
        "/api/pending",
        "/api/sync",
        "/api/diff",
    ] {
        let (code, body) = common::http_get(&addr, path).expect("応答");
        assert!(
            code == 401 || code == 503,
            "P3実装後も未認証の{path}は401/503(fail-closed維持): status={code} body={body}"
        );
        assert!(
            !body.contains("alpha"),
            "{path}の応答に保護対象データは含まれない: {body}"
        );
    }
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
    let Some(fx) = common::StorageFixture::new("srv-startup", common::StorageOpts::default())
    else {
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

// ---------------------------------------------------------------------------
// TASK-9 Phase P4(passkey認証)の結合test(テスト先行。実装前のため実行failを許容、
// cargo test --no-runのcompileは通る。認証契約の詳細はsrc/server.rs内unit testと
// tests/design/webauthn.tomlが担う)。

/// serve --bootstrapのstdoutから最初の空でない行(token行)を期限付き(5秒)で読む。
/// serveは起動を続けるためstdoutは開いたまま=行単位readがblockする。readを別thread
/// に逃がしてchannel+recv_timeoutで待つ(実装前は--bootstrap未知のexit 64で即EOF)。
fn read_bootstrap_token_line(stdout: std::process::ChildStdout) -> Option<String> {
    use std::io::BufRead as _;
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF(実装前=exit 64で即終了)
                Ok(_) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        let _ = tx.send(trimmed.to_string());
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).ok()
}

/// [covers:webauthn.bootstrap-token-generation]
/// serve --bootstrapがone-time token(128bit乱数のbase64)を生成しstdoutへ表示し、
/// 起動を継続する(毎回新規生成=実行のたび異なる値は本観測の対比で担保)。
#[test]
fn serve_bootstrap_flag_prints_one_time_token_on_stdout() {
    let Some(fx) = common::StorageFixture::new("srv-bootstrap", common::StorageOpts::default())
    else {
        return;
    };
    let port = free_local_port();
    fx.write_server_toml(&format!(
        "[server]\nlisten_addr = \"127.0.0.1\"\nport = {port}\n"
    ));
    let state_s = fx.state.display().to_string();
    let home_s = fx.home.display().to_string();
    // stdoutからtokenを読むためpipedでspawn(spawn_serveはstdout継承のため直組)
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_amazing-backup"))
        .args(["serve", "--bootstrap"])
        .env("AMAZING_BACKUP_FAKE_S3_STATE", &state_s)
        .env("AMAZING_BACKUP_HOME", &home_s)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("serve --bootstrap spawn");
    let token = read_bootstrap_token_line(child.stdout.take().expect("stdout piped"));
    let Some(token) = token else {
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "serve --bootstrapがstdoutへtoken行を出す(実装前=--bootstrap未知のexit 64で不在は赤許容)"
        );
    };
    // tokenは128bit乱数のbase64: base64 decodeすると16byte(標準/URL-safe両方を受容)
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&token)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&token))
        .unwrap_or_else(|_| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("tokenはbase64文字列である: {token}")
        });
    assert_eq!(decoded.len(), 16, "128bit(16byte)のbase64である: {token}");
    // token表示後もserveは起動を継続する(health応答)
    let addr = format!("127.0.0.1:{port}");
    assert!(
        common::wait_http_ready(&addr, 100),
        "token表示後もserveは起動を継続する"
    );
    let (code, _) = common::http_get(&addr, "/api/health").expect("health応答");
    assert_eq!(code, 200);
    let _ = child.kill();
    let _ = child.wait();
}

/// [covers:server.protected-api-open-with-auth]
/// P4実装後は保護APIの未認証応答が503(configuration)から401(unauthenticated)へ
/// 置換わる(protected_fail_closedのsession検証gateへの置換。P4実装前は現行503の
/// ままでfailを許容=テスト先行)。
#[test]
fn serve_protected_api_unauthenticated_401_after_p4() {
    let Some(fx) = common::StorageFixture::new("srv-open-p4", common::StorageOpts::default())
    else {
        return;
    };
    // データが存在する状態で観測(保護対象のデータが応答に漏れないことまで確認)
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");

    let (addr, mut child, _err) = start_serve(&fx, free_local_port(), "open-p4");
    let (code, body) = common::http_get(&addr, "/api/entities").expect("応答");
    assert_eq!(
        code, 401,
        "P4実装後は未認証401(503 configurationでない): status={code} body={body}"
    );
    assert!(
        body.contains("\"unauthenticated\""),
        "error bodyは既存error体系形式(unauthenticated): {body}"
    );
    assert!(
        !body.contains("alpha"),
        "保護対象データは応答に含まれない: {body}"
    );
    let _ = child.send_sigterm();
    let _ = child.wait();
}

// ---------------------------------------------------------------------------
// TASK-9 Phase P5(pending catalog)の結合test(テスト先行。POST /api/pending/scan
// は保護APIのため未認証401=auth_gate実装済みの観測は実装前後とも緑。認証済みでの
// scan実行観測はsrc/server.rs内handler単体testが担う)。

/// [covers:server.pending-scan-endpoint]
/// POST /api/pending/scanは保護API: 未認証では401 unauthenticatedでscanを実行しない。
#[test]
fn serve_pending_scan_unauthenticated_401() {
    let Some(fx) = common::StorageFixture::new("srv-pending-scan", common::StorageOpts::default())
    else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    make_archive(&a, 1024);
    let (code, stdout, stderr) = fx.push_from_list(&[], &[(&a, Some("alpha"))], &[]);
    assert_eq!(code, 0, "push(事前): {stdout}{stderr}");

    let (addr, mut child, _err) = start_serve(&fx, free_local_port(), "pending-scan");
    let (code, body) = common::http_post(&addr, "/api/pending/scan").expect("応答");
    assert_eq!(
        code, 401,
        "未認証のPOST /api/pending/scanは401(保護API): status={code} body={body}"
    );
    assert!(
        body.contains("\"unauthenticated\""),
        "error bodyは既存error体系形式(unauthenticated): {body}"
    );
    assert!(
        !body.contains("alpha"),
        "保護対象データは応答に含まれない: {body}"
    );
    let _ = child.send_sigterm();
    let _ = child.wait();
}
