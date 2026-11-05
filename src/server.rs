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

    // -----------------------------------------------------------------------
    // P3(読取API)のhandler単体test(設計12章P3「handler単体test中心」・11章
    // 「API handler(in-memory DB+axum test utility)」)。
    // routerをoneshotで直接呼ぶ=serve経由のfail-closed(保護API 503)とは別経路で
    // handlerの応答schemaとerror体系を観測する。DBはABH配下のcatalog.dbへP1 API
    // (CatalogDb+replace_scope)で注入し、S3側はfake seam(env AMAZING_BACKUP_FAKE_S3_STATE
    // +空state)で用意する。
    // 実装前(本test作成時点)は/api/*の読取endpointが未実装のためfallbackの503で
    // 実行failを許容(compileは通る)。P3実装でbuild_routerがserver状態を受け取る
    // signatureへ変わった場合は api() helper内の呼び出しのみ追随(期待値は不変)。

    /// P3 handler test用の実効profile構成(bucket・prefix。aws.tomlとseedの突合用)。
    const API_TEST_BUCKET: &str = "api-bucket";
    const API_TEST_PREFIX: &str = "amazing-backup/";
    /// scope指定の対照用(実効profile以外のscope)。
    const API_OTHER_BUCKET: &str = "other-bucket";
    /// 索引object key(prefix+sys/index.jsonl.age。HEAD失敗注入の対象指定用)。
    const API_INDEX_KEY: &str = "amazing-backup/sys/index.jsonl.age";

    /// P3 handler testの環境(直列化lock+ABH+最小config+fake S3 state)。
    /// env書換え系のため必ず config_env_lock() を保持したまま使う。
    struct ApiEnv {
        _lock: std::sync::MutexGuard<'static, ()>,
        _tmp: TempDir,
        _abh: crate::testutil::AbhEnvGuard,
        /// ABH home(内部error文字列の無加工返却検出の対象path)
        home: PathBuf,
    }

    impl Drop for ApiEnv {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("AMAZING_BACKUP_FAKE_S3_STATE") };
        }
    }

    /// ABH配下に実効profile devの最小config(pipeline.toml・aws.toml・credential)と
    /// 空のfake S3 state dirを用意する(storage/mod.rs unit testと同一構成)。
    fn api_env(tag: &str) -> ApiEnv {
        use crate::testutil::write_file;
        let lock = config_env_lock();
        let tmp = TempDir::new(tag);
        let home = tmp.path().join("abh");
        std::fs::create_dir_all(&home).expect("ABH home dir");
        let abh = set_amazing_backup_home(&home);
        // pipeline.toml: recipients公開鍵+verify.decrypt_key(resolve_keyはfile参照のみ)
        let pubkey = crate::testutil::write_test_ed25519_pub(tmp.path());
        let secret = tmp.path().join("secret.key");
        write_file(&secret, b"fake-seam-dummy-key\n");
        write_file(
            &home.join("pipeline.toml"),
            format!(
                "[profiles.dev.recipients]\npublic_keys = [\"{}\"]\n\
                 [profiles.dev.verify]\ndecrypt_key = \"{}\"\n",
                pubkey.display(),
                secret.display()
            )
            .as_bytes(),
        );
        // aws.toml: 実効profile dev(bucket=API_TEST_BUCKET・prefix=API_TEST_PREFIX)
        let creds = tmp.path().join("aws-credentials.toml");
        write_file(
            &creds,
            b"[credentials]\naccess_key_id = \"AKIAFAKESEAMONLY00000A\"\nsecret_access_key = \"fake-seam-only-not-a-real-secret\"\n",
        );
        write_file(
            &home.join("aws.toml"),
            format!(
                "[aws]\nactive_profile = \"dev\"\n\
                 [aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\
                 [aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"{API_TEST_BUCKET}\"\n\
                 region = \"ap-northeast-1\"\nprefix = \"{API_TEST_PREFIX}\"\n\
                 storage_class = \"STANDARD\"\ncredentials_file = \"{}\"\n",
                creds.display()
            )
            .as_bytes(),
        );
        // fake S3 state(空=索引object・registryともに無い未書込namespace)
        let state = tmp.path().join("s3state");
        std::fs::create_dir_all(&state).expect("fake state dir");
        unsafe { std::env::set_var("AMAZING_BACKUP_FAKE_S3_STATE", &state) };
        ApiEnv {
            _lock: lock,
            _tmp: tmp,
            _abh: abh,
            home,
        }
    }

    /// catalog.db(ABH配下)へscope単位でentriesを注入(P1 APIの直接利用)。
    fn api_seed(entries: &[crate::index::IndexFileEntry], bucket: &str, prefix: &str) {
        use crate::catalog::{CatalogDb, ReflectOrigin, SourceMtimes, Validator, STATUS_FRESH};
        let header = crate::index::IndexHeader {
            schema: crate::index::INDEX_SCHEMA,
            record_type: "index_header".into(),
            updated_at: "2026-11-05T12:00:00+09:00".into(),
            run_id: "20261105-abcdef01".into(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            entries: entries.len() as i64,
        };
        let mut db = CatalogDb::open().expect("catalog DB open");
        db.replace_scope(
            bucket,
            prefix,
            &header,
            entries,
            &SourceMtimes::empty(),
            &Validator {
                etag: "\"etag-1\"".into(),
                version_id: "vid-1".into(),
            },
            STATUS_FRESH,
            ReflectOrigin::Fetched,
        )
        .expect("seed replace_scope");
    }

    /// 手組みのIndexFileEntry(1版・pin・location付き。category/labelは引数)。
    fn api_entry(uuid: &str, title: &str, category: &str, label: &str) -> crate::index::IndexFileEntry {
        use crate::index::{IndexFileEntry, IndexLocation, IndexPin, IndexVersion, INDEX_SCHEMA};
        IndexFileEntry {
            schema: INDEX_SCHEMA,
            record_type: "file".into(),
            uuid: uuid.into(),
            title: title.into(),
            versions: vec![IndexVersion {
                date: "2026-11-05".into(),
                version_ts: "2026-11-05T10:00:00+09:00".into(),
                batch_id: "20261105-abcdef01".into(),
                s3_key: format!("{API_TEST_PREFIX}data/{uuid}/a.age"),
                version_id: "v1".into(),
                source_sha256: "a".repeat(64),
                age_sha256: "b".repeat(64),
                source_size: 100,
                age_size: 140,
            }],
            pins: vec![IndexPin {
                pin: "pin-1".into(),
                s3_key: format!("{API_TEST_PREFIX}data/{uuid}/pin-1.age"),
                created_at: "2026-11-05T11:00:00+09:00".into(),
                date: "2026-11-05".into(),
                source_sha256: "a".repeat(64),
                age_sha256: "b".repeat(64),
                source_size: 100,
            }],
            locations: vec![IndexLocation {
                source_path: format!("/data/{title}"),
                category: category.into(),
                label: label.into(),
                identity_hash: format!("hash-{uuid}"),
                last_backup: "2026-11-05".into(),
            }],
        }
    }

    /// build_routerをoneshotで呼び(status, JSON body)を返す。bodyがJSONで無い場合は
    /// Null(応答形式の検査でfailする)。
    fn api(method: &str, uri: &str) -> (u16, serde_json::Value) {
        let router = build_router();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("oneshot runtime");
        rt.block_on(async {
            use tower::ServiceExt as _;
            let req = axum::http::Request::builder()
                .method(axum::http::Method::from_bytes(method.as_bytes()).expect("method"))
                .uri(uri)
                .body(axum::body::Body::empty())
                .expect("request build");
            let resp = router.oneshot(req).await.expect("oneshot");
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .expect("body read");
            let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (status, value)
        })
    }

    /// error応答のcode/message検査helper({"error":{"code","message"}}形式)。
    fn expect_error_body(body: &serde_json::Value, want_code: &str) {
        let error = body
            .get("error")
            .unwrap_or_else(|| panic!("error bodyは{{\"error\":{{...}}}}形式: {body}"));
        assert_eq!(
            error.get("code").and_then(|c| c.as_str()),
            Some(want_code),
            "error code一致: {body}"
        );
        assert!(
            error.get("message").is_some_and(|m| m.as_str().is_some_and(|m| !m.is_empty())),
            "messageは空でないstring: {body}"
        );
    }

    /// [covers:server.entities-list-endpoint]
    /// GET /api/entitiesはentry_summary_json互換schemaで実効scopeのentity一覧を返し、
    /// query/category/label/scope絞り込みとpageingが機能する。
    #[test]
    fn api_entities_list_responds_entry_summaries_with_filters() {
        let env = api_env("srv-api-entities");
        let alpha = api_entry("11111111-1111-1111-1111-111111111111", "alpha", "docs", "work");
        let beta = api_entry("22222222-2222-2222-2222-222222222222", "beta", "unfiled", "");
        api_seed(&[alpha, beta], API_TEST_BUCKET, API_TEST_PREFIX);
        // 実効scope外のデータ(scope絞り込みの対照)
        let gamma = api_entry("33333333-3333-3333-3333-333333333333", "gamma", "docs", "work");
        api_seed(&[gamma], API_OTHER_BUCKET, API_TEST_PREFIX);

        // 全体(実効profileのscopeのみ。他scopeは混入しない)
        let (status, body) = api("GET", "/api/entities");
        assert_eq!(status, 200, "GET /api/entitiesは200: {body}");
        let entities = body
            .get("entities")
            .and_then(|v| v.as_array())
            .expect("entities配列");
        assert_eq!(entities.len(), 2, "実効scopeの2entity: {body}");
        assert!(
            entities.iter().all(|e| e.get("uuid").is_some_and(|u| u != gamma_uuid())),
            "他scope(other-bucket)のentityは含まれない: {body}"
        );
        // pageing(1page目・総page数)
        assert_eq!(body.get("page").and_then(|p| p.as_i64()), Some(1), "page=1: {body}");
        assert!(
            body.get("total_pages").and_then(|p| p.as_i64()).is_some_and(|p| p >= 1),
            "total_pages付き: {body}"
        );
        // entry_summary_json互換schema(uuid/title/locations/versions=count/
        // latest_date/latest_size/pins)
        let first = &entities[0];
        for key in ["uuid", "title", "latest_date", "latest_size"] {
            assert!(first.get(key).is_some(), "entry_summary互換key {key}: {first}");
        }
        assert!(first.get("versions").is_some_and(|v| v.as_i64() == Some(1)));
        let locations = first
            .get("locations")
            .and_then(|v| v.as_array())
            .expect("locations配列");
        let l0 = &locations[0];
        for key in ["source_path", "category", "label", "identity_hash", "last_backup"] {
            assert!(
                l0.get(key).is_some_and(|v| !v.is_null()),
                "locationのkey {key}: {l0}"
            );
        }
        assert!(
            first.get("pins").is_some_and(|p| p.as_array().is_some_and(|a| !a.is_empty())),
            "pins配列: {first}"
        );

        // query絞り込み(title部分一致)
        let (_, body) = api("GET", "/api/entities?query=beta");
        let entities = body.get("entities").and_then(|v| v.as_array()).expect("entities");
        assert_eq!(entities.len(), 1, "query=betaはbetaのみ: {body}");
        assert!(entities[0].get("title").is_some_and(|t| t == "beta"));

        // category絞り込み(location.category一致)
        let (_, body) = api("GET", "/api/entities?category=docs");
        let entities = body.get("entities").and_then(|v| v.as_array()).expect("entities");
        assert_eq!(entities.len(), 1, "category=docsはalphaのみ: {body}");
        assert!(entities[0].get("title").is_some_and(|t| t == "alpha"));

        // label絞り込み(location.label一致)
        let (_, body) = api("GET", "/api/entities?label=work");
        let entities = body.get("entities").and_then(|v| v.as_array()).expect("entities");
        assert_eq!(entities.len(), 1, "label=workはalphaのみ: {body}");

        // scope明示絞り込み(他bucketを明示すればgammaが見える)
        let (_, body) = api("GET", "/api/entities?scope=other-bucket:amazing-backup/");
        let entities = body.get("entities").and_then(|v| v.as_array()).expect("entities");
        assert_eq!(entities.len(), 1, "scope=other-bucket:amazing-backup/はgammaのみ: {body}");
        assert!(entities[0].get("title").is_some_and(|t| t == "gamma"));
        drop(env);
    }

    /// gammaのuuid(scope混入検出用の期待値文字列)。
    fn gamma_uuid() -> &'static str {
        "33333333-3333-3333-3333-333333333333"
    }

    /// [covers:server.entity-detail-endpoint]
    /// GET /api/entities/:uuidはversions/pins/locationsを含む詳細(version_list付き
    /// entry_summary_json互換)を返す。不在uuidはnot_found 404のerror body。
    #[test]
    fn api_entity_detail_returns_full_entry_and_404_for_missing() {
        let env = api_env("srv-api-detail");
        let alpha = api_entry("11111111-1111-1111-1111-111111111111", "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api("GET", "/api/entities/11111111-1111-1111-1111-111111111111");
        assert_eq!(status, 200, "entity詳細は200: {body}");
        assert_eq!(body.get("uuid").and_then(|v| v.as_str()), Some("11111111-1111-1111-1111-111111111111"));
        assert_eq!(body.get("title").and_then(|v| v.as_str()), Some("alpha"));
        assert!(
            body.get("locations").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty())),
            "詳細はlocationsを含む: {body}"
        );
        assert!(
            body.get("pins").is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty())),
            "詳細はpinsを含む: {body}"
        );
        let version_list = body
            .get("version_list")
            .and_then(|v| v.as_array())
            .expect("詳細はversion_listを含む");
        let v0 = &version_list[0];
        for key in ["date", "version_ts", "version_id", "batch_id", "source_size"] {
            assert!(
                v0.get(key).is_some_and(|v| !v.is_null()),
                "version_list[0]のkey {key}: {v0}"
            );
        }

        // 不在uuidは404 not_found
        let (status, body) = api("GET", "/api/entities/99999999-9999-9999-9999-999999999999");
        assert_eq!(status, 404, "不在uuidは404: {body}");
        expect_error_body(&body, "not_found");
        drop(env);
    }

    /// [covers:server.entity-versions-endpoint]
    /// GET /api/entities/:uuid/versionsはlist versions --format json相当の
    /// entity object(uuid/title/locations/versions/pins)を返す。不在uuidは404。
    #[test]
    fn api_entity_versions_returns_version_rows_and_404_for_missing() {
        let env = api_env("srv-api-versions");
        let alpha = api_entry("11111111-1111-1111-1111-111111111111", "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api("GET", "/api/entities/11111111-1111-1111-1111-111111111111/versions");
        assert_eq!(status, 200, "versions一覧は200: {body}");
        assert_eq!(body.get("uuid").and_then(|v| v.as_str()), Some("11111111-1111-1111-1111-111111111111"));
        assert_eq!(body.get("title").and_then(|v| v.as_str()), Some("alpha"));
        let versions = body
            .get("versions")
            .and_then(|v| v.as_array())
            .expect("versions配列");
        assert_eq!(versions.len(), 1);
        let v0 = &versions[0];
        for key in ["date", "version_ts", "version_id", "source_size", "state"] {
            assert!(
                v0.get(key).is_some_and(|v| !v.is_null()),
                "versions[0]のkey {key}: {v0}"
            );
        }
        assert!(
            body.get("pins").is_some_and(|v| v.as_array().is_some()),
            "versions一覧もpinsを含む(list versions相当): {body}"
        );

        let (status, body) = api("GET", "/api/entities/99999999-9999-9999-9999-999999999999/versions");
        assert_eq!(status, 404, "不在uuidは404: {body}");
        expect_error_body(&body, "not_found");
        drop(env);
    }

    /// [covers:server.pins-list-endpoint]
    /// GET /api/pinsはlist pins --format json相当(entity単位のuuid/title/pins[])の
    /// pin一覧を返す。
    #[test]
    fn api_pins_list_returns_pins_per_entity() {
        let env = api_env("srv-api-pins");
        let alpha = api_entry("11111111-1111-1111-1111-111111111111", "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api("GET", "/api/pins");
        assert_eq!(status, 200, "GET /api/pinsは200: {body}");
        let pins = body
            .get("pins")
            .and_then(|v| v.as_array())
            .expect("pins配列");
        assert_eq!(pins.len(), 1, "pinを持つentity単位: {body}");
        assert_eq!(pins[0].get("title").and_then(|t| t.as_str()), Some("alpha"));
        let pin_rows = pins[0]
            .get("pins")
            .and_then(|v| v.as_array())
            .expect("entityのpins配列");
        let p0 = &pin_rows[0];
        for key in ["pin", "date", "source_size"] {
            assert!(
                p0.get(key).is_some_and(|v| !v.is_null()),
                "pin行のkey {key}: {p0}"
            );
        }
        drop(env);
    }

    /// [covers:server.locations-list-endpoint]
    /// GET /api/locationsはlocation一覧(uuid/title+location_json相当の各field)を返す。
    #[test]
    fn api_locations_list_returns_location_rows() {
        let env = api_env("srv-api-locations");
        let alpha = api_entry("11111111-1111-1111-1111-111111111111", "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api("GET", "/api/locations");
        assert_eq!(status, 200, "GET /api/locationsは200: {body}");
        let locations = body
            .get("locations")
            .and_then(|v| v.as_array())
            .expect("locations配列");
        assert_eq!(locations.len(), 1, "location行: {body}");
        let row = &locations[0];
        for key in ["uuid", "title", "source_path", "category", "label", "identity_hash", "last_backup"] {
            assert!(
                row.get(key).is_some_and(|v| !v.is_null()),
                "location行のkey {key}: {row}"
            );
        }
        assert_eq!(row.get("source_path").and_then(|v| v.as_str()), Some("/data/alpha"));
        drop(env);
    }

    /// [covers:server.pending-endpoint-schema]
    /// GET /api/pendingはpending_itemsのschema(source_root/path/size/mtime/status/
    /// last_scan_at)で応答する。P3時点ではscan(P5)が未実装のため空配列。
    #[test]
    fn api_pending_responds_empty_list_with_schema_shape() {
        let env = api_env("srv-api-pending");
        let (status, body) = api("GET", "/api/pending");
        assert_eq!(status, 200, "GET /api/pendingは200: {body}");
        let pending = body
            .get("pending")
            .and_then(|v| v.as_array())
            .expect("pending配列(schemaの枠)");
        assert!(
            pending.is_empty(),
            "P3時点はscan未実装のため空: {body}"
        );
        drop(env);
    }

    /// [covers:server.sync-state-endpoint]
    /// GET /api/syncはsync_state 1行分(scope/status/validator/fetched_at/entry_count)を
    /// 返す。未sync時はstatus=unknownで200(staleness表示のため常に応答)。
    #[test]
    fn api_sync_state_returns_sync_state_row() {
        let env = api_env("srv-api-sync");
        // 未sync(DBにsync_state行が無い)でもUI描画のため200+unknown
        let (status, body) = api("GET", "/api/sync");
        assert_eq!(status, 200, "未sync時も200: {body}");
        assert_eq!(body.get("status").and_then(|s| s.as_str()), Some("unknown"), "未syncはunknown: {body}");

        // seed反映後はsync_stateの保存値を返す
        let alpha = api_entry("11111111-1111-1111-1111-111111111111", "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);
        let (status, body) = api("GET", "/api/sync");
        assert_eq!(status, 200, "sync状態は200: {body}");
        assert_eq!(body.get("status").and_then(|s| s.as_str()), Some("fresh"), "seed直後はfresh: {body}");
        assert_eq!(
            body.get("scope").and_then(|s| s.as_str()),
            Some("api-bucket:amazing-backup/"),
            "scopeは<bucket>:<prefix>形式: {body}"
        );
        assert_eq!(body.get("index_etag").and_then(|v| v.as_str()), Some("\"etag-1\""));
        assert_eq!(body.get("index_version_id").and_then(|v| v.as_str()), Some("vid-1"));
        for key in ["fetched_at", "last_write_at", "last_verified_at"] {
            assert!(body.get(key).is_some_and(|v| v.as_str().is_some()), "key {key}: {body}");
        }
        assert_eq!(body.get("entry_count").and_then(|v| v.as_i64()), Some(1));
        drop(env);
    }

    /// [covers:server.sync-refresh-endpoint]
    /// POST /api/syncは手動refresh(起動時syncと同一の同期層)を実行し、最新の
    /// sync_stateで応答する。未書込namespace(索引・registryともに無い)では空cacheの
    /// fresh確定として成功する。
    #[test]
    fn api_sync_refresh_runs_manual_refresh() {
        let env = api_env("srv-api-sync-refresh");
        let (status, body) = api("POST", "/api/sync");
        assert_eq!(status, 200, "POST /api/syncは200: {body}");
        assert_eq!(
            body.get("status").and_then(|s| s.as_str()),
            Some("fresh"),
            "未書込namespaceは空cacheのfresh確定: {body}"
        );
        assert!(
            body.get("fetched_at")
                .is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty())),
            "手動refreshの応答はfetched_at刻み済みの最新状態: {body}"
        );
        // GET /api/syncにも反映される
        let (status, body) = api("GET", "/api/sync");
        assert_eq!(status, 200);
        assert_eq!(body.get("status").and_then(|s| s.as_str()), Some("fresh"), "refresh後の状態: {body}");
        drop(env);
    }

    /// [covers:server.diff-endpoints]
    /// POST /api/diffはdiff実行(未書込namespaceでは空結果)を行い結果を保存・
    /// GET /api/diffは保存済みの最終結果を返す。未実行時のGETはnot_found 404。
    #[test]
    fn api_diff_post_runs_and_get_returns_last_result() {
        let env = api_env("srv-api-diff");
        // 未実行時のGETは404 not_found(保存済み結果がまだ無い)
        let (status, body) = api("GET", "/api/diff");
        assert_eq!(status, 404, "未実行時のGET /api/diffは404: {body}");
        expect_error_body(&body, "not_found");

        // POST /api/diff: 未書込namespaceのdiffは空結果で成功(cmd_diffのJSON語彙)
        let (status, body) = api("POST", "/api/diff");
        assert_eq!(status, 200, "POST /api/diffは200: {body}");
        for key in ["performed_at", "entries", "files", "missing", "unexpected"] {
            assert!(body.get(key).is_some(), "diff結果のkey {key}: {body}");
        }
        assert_eq!(body.get("entries").and_then(|v| v.as_i64()), Some(0), "空結果: {body}");

        // GET /api/diffは保存済みの最終結果(POSTと同一内容)を返す
        let (status, got) = api("GET", "/api/diff");
        assert_eq!(status, 200, "実行後のGET /api/diffは200: {got}");
        assert_eq!(got, body, "GETは保存済みの最終diff結果を返す");
        drop(env);
    }

    /// [covers:server.error-code-status-mapping]
    /// error codeとHTTP statusの変換表(設計6章指摘#14)のうちhandler観測可能な
    /// 代表: invalid_request=400・not_found=404・upstream=502。
    /// (unauthenticated 401はP4の認証構成後・configuration 503はfail-closedの
    /// server.protected-endpoints-fail-closedで観測)
    #[test]
    fn api_error_codes_map_to_http_statuses() {
        let env = api_env("srv-api-errors");
        let alpha = api_entry("11111111-1111-1111-1111-111111111111", "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);

        // invalid_request=400(query paramの形式不正: pageが数値でない)
        let (status, body) = api("GET", "/api/entities?page=abc");
        assert_eq!(status, 400, "page=abcは400: {body}");
        expect_error_body(&body, "invalid_request");

        // not_found=404(対象不在)
        let (status, body) = api("GET", "/api/entities/99999999-9999-9999-9999-999999999999");
        assert_eq!(status, 404);
        expect_error_body(&body, "not_found");

        // upstream=502(S3転送・remote索引の問題): HEAD失敗注入でsyncを失敗させる
        // (registry fileを置くことでHEAD検証経路へ入る)
        crate::testutil::write_file(&env.home.join("registry.jsonl"), b"");
        unsafe { std::env::set_var("FAKE_S3_FAIL_HEAD_FOR", API_INDEX_KEY) };
        let (status, body) = api("POST", "/api/sync");
        unsafe { std::env::remove_var("FAKE_S3_FAIL_HEAD_FOR") };
        assert_eq!(status, 502, "S3側失敗はupstream 502: {body}");
        expect_error_body(&body, "upstream");
        drop(env);
    }

    /// [covers:server.error-body-generalized]
    /// error bodyは常に{"error":{"code","message"}}形式で、messageは一般化された
    /// 文言(内部error文字列・一時dir等の内部pathを無加工で含まない)。
    #[test]
    fn api_error_body_is_generalized_without_internal_leak() {
        let env = api_env("srv-api-error-body");
        let alpha = api_entry("11111111-1111-1111-1111-111111111111", "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api("GET", "/api/entities?page=abc");
        assert_eq!(status, 400);
        expect_error_body(&body, "invalid_request");

        let (status, body) = api("GET", "/api/entities/99999999-9999-9999-9999-999999999999");
        assert_eq!(status, 404);
        expect_error_body(&body, "not_found");

        // upstream系errorのmessageに内部path(ABH・一時dir)が含まれてはならない
        crate::testutil::write_file(&env.home.join("registry.jsonl"), b"");
        unsafe { std::env::set_var("FAKE_S3_FAIL_HEAD_FOR", API_INDEX_KEY) };
        let (status, body) = api("POST", "/api/sync");
        unsafe { std::env::remove_var("FAKE_S3_FAIL_HEAD_FOR") };
        assert_eq!(status, 502);
        expect_error_body(&body, "upstream");
        let message = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .expect("message");
        let home_str = env.home.display().to_string();
        let tmp_str = env
            .home
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        assert!(
            !message.contains(&home_str) && !message.contains(&tmp_str),
            "内部pathの無加工返却は禁止(詳細はserver logのみ): message={message}"
        );
        drop(env);
    }
}
