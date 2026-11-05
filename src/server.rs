//! TASK-9 serve基盤(設計1章D2: 同一binaryのserve subcommand・6章API・8章server.toml・
//! 9章D4 localhost既定・10章dist二階建て・12章Phase P2)。
//!
//! - HTTP層はaxum。mount prefixは`/api/`。P2の実endpointはliveness(GET /api/health)のみ
//! - 保護API(entities等の読取系)はP3で実装予定だが、認証はP4まで構成されないため
//!   `fail-closed`: 未構成の間は全保護endpointを503(configuration)で応答させ
//!   応答内容を返さない(設計12章P2のverify可能状態・指摘#11)
//! - 実行境界(設計1章): 既存S3/DB層は同期APIのため、serve起動時のHEAD validator検証+
//!   DB反映(manage::catalog_sync_once=P1同期層)は`tokio::task::spawn_blocking`経由で
//!   呼ぶ(async context内からのRuntime混在panic回避。DB接続も同thread内でopen)
//! - 静的配信(設計10章): frontend/distをrust-embedで埋め込み。`#[allow_missing = true]`
//!   によりdist欠落でもbuild・起動可能で、欠落時は静的配信なし(APIのみ)で起動し
//!   warning+frontend build手順をstderrへ表示する
//! - shutdownはgraceful(設計1章): SIGTERM/SIGINT→進行中spawn_blockingの完了待ち→close・
//!   exit 0

use crate::errors::{BackupError, BackupResult};
use axum::response::IntoResponse as _;
use std::path::PathBuf;

/// serve subcommandの引数(設計1章・8章: serveは既存profile解決chainを使うため
/// backup系と同じ選択optionを持つ。server.toml自体のpath指定は設計対象外)。
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// pipeline設定(省略時はXDG規定path。起動時syncの復号鍵解決に使用)
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// aws設定(省略時はXDG規定path。実効profileの解決に必須)
    #[arg(long)]
    pub aws_config: Option<PathBuf>,
    /// aws.toml/pipeline.toml両方の実効profile選択(当該実行限りの上書き)
    #[arg(long)]
    pub profile: Option<String>,
}

/// server.tomlの`[server]`section(設計8章)。server共通設定のみでbackup profileとは分離。
/// rp_id/rp_originはP4(passkey認証)で使用。P2では読込と既定値解決のみ行う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// listen address(既定127.0.0.1。D4: v1はlocalhost既定)
    pub listen_addr: String,
    /// listen port(D5: 既定40510=本番serve)
    pub port: u16,
    /// WebAuthn RP ID(既定"localhost"。P4で使用)
    pub rp_id: String,
    /// WebAuthn RP origin(既定`http://localhost:<port>`。P4で使用)
    pub rp_origin: String,
}

/// listen_addrの既定値(D4・template正本configs/server.example.tomlと同一値)。
pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1";
/// portの既定値(D5本番serve・template正本と同一値)。
pub const DEFAULT_PORT: u16 = 40510;
/// rp_idの既定値(template正本と同一値)。
pub const DEFAULT_RP_ID: &str = "localhost";

/// frontend/distの埋め込み(設計10章dist二階建て)。`#[allow_missing = true]`で
/// dist欠落時もbuildでき、欠落時は空assetsとなる(実行時に`iter()`空で検出)。
#[derive(rust_embed::RustEmbed)]
#[folder = "frontend/dist/"]
#[allow_missing = true]
struct FrontendDist;

/// 静的配信assetsが利用可能か。
/// - release build: build時の埋め込み有無(rust-embed。dist欠落build=空assets)
/// - dev profile(debug): 常に無効。devでのfrontend確認はVite dev server(設計10章
///   「devはVite dev serverを直接見る」)で行う。debugではrust-embedが実行時の
///   frontend/distを読むため埋め込み有無が環境依存となり、cargo testの
///   dist欠落観測(条件書server.dist-missing-api-only-with-warningのgiven=
///   「distが存在しない環境=CI/test環境の既定」)が不安定になる。debugを
///   静的配信なし(dist欠落時と同一挙動)に固定することで観測を決定論にする
fn static_assets_available() -> bool {
    #[cfg(debug_assertions)]
    {
        false
    }
    #[cfg(not(debug_assertions))]
    {
        FrontendDist::iter().next().is_some()
    }
}

/// server.tomlの読込(設計8章)。各キー省略時は既定値・file自体が無い場合も既定値
/// (stderrへnotice表示。file不在時の挙動は設計上未規定だが、server.tomlは秘密値を
/// 持たないため既定値での起動が安全側)。`[server]`内の未知キーはtypo検出のため
/// 拒否(設定エラー2)。top-levelの未知sectionは将来拡張(例: catalog.scan)のため許容。
pub fn load_server_config() -> BackupResult<ServerConfig> {
    let path = crate::config::resolve_app_paths().server_config;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "notice: server.toml({})が無いため既定値(listen {DEFAULT_LISTEN_ADDR}:{DEFAULT_PORT})で起動します。amazing-backup config init で生成できます",
                path.display()
            );
            return Ok(default_server_config());
        }
        Err(e) => {
            return Err(BackupError::Config(format!(
                "server.tomlを読めない ({}): {e}",
                path.display()
            )));
        }
    };
    parse_server_config(&text)
}

/// 既定値のserver.toml相当(rp_originはport既定値から組立)。
pub fn default_server_config() -> ServerConfig {
    ServerConfig {
        listen_addr: DEFAULT_LISTEN_ADDR.to_string(),
        port: DEFAULT_PORT,
        rp_id: DEFAULT_RP_ID.to_string(),
        rp_origin: format!("http://localhost:{DEFAULT_PORT}"),
    }
}

/// server.toml本文からServerConfigへparse(unit test観測用に分離)。
fn parse_server_config(text: &str) -> BackupResult<ServerConfig> {
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        server: Option<Section>,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Section {
        listen_addr: Option<String>,
        port: Option<u16>,
        rp_id: Option<String>,
        rp_origin: Option<String>,
    }
    let file: File = toml::from_str(text).map_err(|e| {
        // 秘密値を含まないfileのためerror文にparse対象を含めても安全
        BackupError::Config(format!("server.tomlのparseに失敗: {e}"))
    })?;
    let sec = file.server.unwrap_or(Section {
        listen_addr: None,
        port: None,
        rp_id: None,
        rp_origin: None,
    });
    let listen_addr = sec.listen_addr.unwrap_or_else(|| DEFAULT_LISTEN_ADDR.into());
    if listen_addr.trim().is_empty() {
        return Err(BackupError::Config(
            "server.tomlのlisten_addrが空です".to_string(),
        ));
    }
    let port = sec.port.unwrap_or(DEFAULT_PORT);
    Ok(ServerConfig {
        rp_id: sec.rp_id.unwrap_or_else(|| DEFAULT_RP_ID.into()),
        rp_origin: sec
            .rp_origin
            .unwrap_or_else(|| format!("http://localhost:{port}")),
        listen_addr,
        port,
    })
}

/// serveのCLI入口(cli.rsの`Command::Serve`から呼ばれる)。戻り値はexit code。
/// 設定読込失敗は設定エラー2。server起動後のshutdownはgraceful(SIGTERM/SIGINTで
/// 進行中spawn_blocking完了待ちのうえexit 0)。
pub fn cmd_serve(args: &ServeArgs) -> i32 {
    let cfg = match load_server_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    if !static_assets_available() {
        // 設計10章: dist欠落時も起動(APIのみ)+warning+frontend build手順表示
        eprintln!(
            "WARNING: frontend静的配信は無効のためAPIのみで起動します。release buildはfrontend/distを埋め込みます(frontend build手順: cd frontend && pnpm install && pnpm build。devでのfrontend確認はVite dev server: cd frontend && pnpm dev)"
        );
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: 内部エラー: tokio runtime構築に失敗: {e}");
            return 1;
        }
    };
    runtime.block_on(run_server(cfg, args))
}

/// server本体(async側。同期層呼び出しはspawn_blocking経由のみ)。
async fn run_server(cfg: ServerConfig, args: &ServeArgs) -> i32 {
    let addr = format!("{}:{}", cfg.listen_addr, cfg.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: 設定・環境エラー: {addr} へbindできない: {e}");
            return 2;
        }
    };
    eprintln!("serve: http://{addr} でlisten中(liveness: /api/health)");

    // 起動時sync(設計4章・1章実行境界): P1同期層(manage::catalog_sync_once)を
    // spawn_blockingから呼ぶ。失敗してもserveは継続(staleness表示はP3+のAPIが担う)
    let ctx = serve_ctx_args(args);
    let sync_handle = tokio::task::spawn_blocking(move || {
        crate::storage::manage::catalog_sync_once(&ctx)
    });

    let app = build_router();
    // 全応答へCache-Controlが未設定ならno-storeを付与(API応答の途中cache禁止。
    // 静的assetsはhandler側が明示的に設定するため上書きされない)
    let app = app.layer(tower_http::set_header::SetResponseHeaderLayer::if_not_present(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    ));

    let serve = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    if let Err(e) = serve.await {
        eprintln!("error: 内部エラー: serverの実行に失敗: {e}");
        return 1;
    }
    // graceful shutdown(設計1章): 進行中spawn_blocking(起動時sync)の完了を待つ
    if !sync_handle.is_finished() {
        eprintln!("serve: 起動時syncの完了を待っています(graceful shutdown)");
    }
    match sync_handle.await {
        Ok(Ok(msg)) => eprintln!("serve: 起動時catalog sync: {msg}"),
        Ok(Err(e)) => eprintln!("WARNING: 起動時catalog syncに失敗しました(表示はstale扱い): {e}"),
        Err(e) => eprintln!("WARNING: 起動時catalog syncが異常終了しました: {e}"),
    }
    eprintln!("serve: gracefulに停止しました");
    0
}

/// ServeArgsを起動時syncのCtxArgsへ変換(spawn_blocking境界を越えるため所有権ごと渡す)。
fn serve_ctx_args(args: &ServeArgs) -> crate::storage::CtxArgs {
    crate::storage::CtxArgs {
        config: args.config.clone(),
        aws_config: args.aws_config.clone(),
        key: None,
        profile: args.profile.clone(),
        format: crate::storage::Format::Human,
    }
}

/// SIGTERM/SIGINTの待受(graceful shutdown契機。設計1章)。
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler構築");
    let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler構築");
    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv() => {},
    }
}

/// APIのerror body形式(設計6章指摘#14: `{"error":{"code","message"}}`・一般化message)。
fn error_body(code: &str, message: &str) -> serde_json::Value {
    serde_json::json!({"error": {"code": code, "message": message}})
}

/// router組立(設計6章のendpoint一覧のP2分)。
fn build_router() -> axum::Router {
    axum::Router::new()
        .route("/api/health", axum::routing::get(health))
        .fallback(fallback)
}

/// GET /api/health(認証不要のliveness・設計6章)。
async fn health() -> impl axum::response::IntoResponse {
    axum::Json(serde_json::json!({"status": "ok"}))
}

/// routerのfallback: /api/*の未実装endpointはfail-closed、それ以外は静的配信。
async fn fallback(uri: axum::http::Uri) -> axum::response::Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        // 保護API(設計12章P2: 認証機構未構成の間は503 configurationで閉鎖=応答内容を
        // 返さない。P4の認証実装で開放。未認証401との使い分けはP4以降)
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(error_body(
                "configuration",
                "保護APIは認証機構の構成が完了するまで閉鎖されています",
            )),
        )
            .into_response();
    }
    static_handler(path).into_response()
}

/// 埋め込みassetsからの静的配信(SPA)。assetsが空(dist欠落・debug)なら404で案内。
fn static_handler(path: &str) -> axum::response::Response {
    if !static_assets_available() {
        return (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(error_body(
                "not_found",
                "frontend静的配信は無効です(release buildでfrontend/distを埋め込みます。serve起動時のwarningのfrontend build手順を参照)",
            )),
        )
            .into_response();
    }
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if let Some(file) = FrontendDist::get(rel) {
        return asset_response(rel, &file);
    }
    // SPAのhistory routing: 未知pathはindex.htmlへfallback
    if let Some(file) = FrontendDist::get("index.html") {
        return asset_response("index.html", &file);
    }
    (
        axum::http::StatusCode::NOT_FOUND,
        axum::Json(error_body("not_found", "対象のfrontend成果物がありません")),
    )
        .into_response()
}

/// 埋め込みfileの応答組立(content-type+Cache-Control)。
fn asset_response(name: &str, file: &rust_embed::EmbeddedFile) -> axum::response::Response {
    // htmlは常に再検証。hash付きassetsはcache可(vite buildの産物はhash付きfile名)
    let cache = if name == "index.html" {
        "no-cache"
    } else {
        "public, max-age=86400"
    };
    let resp = axum::http::Response::builder()
        .status(axum::http::StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, content_type(name))
        .header(axum::http::header::CACHE_CONTROL, cache)
        .body(axum::body::Body::from(file.data.clone()));
    match resp {
        Ok(r) => r.into_response(),
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(error_body("internal", "応答の組立に失敗しました")),
        )
            .into_response(),
    }
}

/// 拡張子からのcontent-type(vite buildが産出する種別のみ。未知はoctet-stream)。
fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain; charset=utf-8",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("webp") => "image/webp",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, config_env_lock, set_amazing_backup_home};

    /// [covers:server.server-toml-listen-config]
    /// server.toml読込: キー省略時は既定値(127.0.0.1・40510・rp_id=localhost・
    /// rp_origin=http://localhost:<port>)。明示値は反映されrp_origin既定はportに追従。
    #[test]
    fn parse_server_config_defaults_and_explicit() {
        let cfg = parse_server_config("[server]\nlisten_addr = \"127.0.0.1\"\n")
            .expect("port省略時は既定40510");
        assert_eq!(cfg.listen_addr, "127.0.0.1");
        assert_eq!(cfg.port, 40510);
        assert_eq!(cfg.rp_id, "localhost");
        assert_eq!(cfg.rp_origin, "http://localhost:40510");

        let cfg = parse_server_config("[server]\nport = 40511\n")
            .expect("listen_addr省略時は既定127.0.0.1");
        assert_eq!(cfg.listen_addr, "127.0.0.1");
        assert_eq!(cfg.port, 40511);
        assert_eq!(cfg.rp_origin, "http://localhost:40511");

        let cfg = parse_server_config(
            "[server]\nlisten_addr = \"127.0.0.1\"\nport = 40513\nrp_id = \"example.com\"\nrp_origin = \"https://example.com\"\n",
        )
        .expect("全キー明示");
        assert_eq!(cfg.rp_id, "example.com");
        assert_eq!(cfg.rp_origin, "https://example.com");

        // 空server.toml・server sectionなしも既定値
        let cfg = parse_server_config("").expect("空fileは既定値");
        assert_eq!(cfg.port, 40510);
    }

    /// [covers:server.server-toml-listen-config]
    /// [server]内の未知キーはtypo検出のため設定エラー。top-levelの未知sectionは許容。
    #[test]
    fn parse_server_config_rejects_unknown_keys_in_section() {
        let err = parse_server_config("[server]\nprot = 1\n").expect_err("未知キーは拒否");
        assert_eq!(err.exit_code(), 2);
        parse_server_config("[server]\nport = 40510\n\n[catalog.scan]\nroots = []\n")
            .expect("top-levelの未知sectionは許容(将来拡張)");
    }

    /// [covers:server.server-toml-listen-config]
    /// load_server_configはfile不在時も既定値で読込成功(notice表示)。
    #[test]
    fn load_server_config_missing_file_uses_defaults() {
        let _lock = config_env_lock();
        let tmp = TempDir::new("srv-cfg-default");
        let _abh = set_amazing_backup_home(tmp.path());
        let cfg = load_server_config().expect("file不在時は既定値");
        assert_eq!(cfg.listen_addr, "127.0.0.1");
        assert_eq!(cfg.port, 40510);
    }
}
