//! TASK-9 serve基盤+読取API(設計1章D2: 同一binaryのserve subcommand・6章API・
//! 8章server.toml・9章D4 localhost既定・10章dist二階建て・12章Phase P2+P3)。
//!
//! - HTTP層はaxum。mount prefixは`/api/`。P2はliveness(GET /api/health)のみで、
//!   P3が読取API(entities/versions/pins/locations/pending/sync/diff)をrouteする
//! - **serve経由の保護APIはP4(認証)までfail-closed**(設計12章・指摘#11):
//!   認証機構未構成の間は全保護endpointを503(configuration)で応答させ内容を
//!   返さない。実装は`protected_fail_closed` middleware(build_routerの外側)が担い、
//!   handler本体はrouterに接続済み(handler単体test=router oneshotで観測)。
//!   P4でこのgateがsession検証へ置換わる
//! - 応答schemaは`--format json`と共通(設計5章・6章: entry_summary_json等の
//!   storage::queryのJSON語彙を共用)。error体系(指摘#14)は
//!   `{"error":{"code","message"}}`でmessageは一般化文言(内部error文字列の
//!   無加工返却禁止。詳細はserver log=stderrのみ)
//! - 実行境界(設計1章): 既存S3/DB層は同期APIのため、serve起動時sync・各handlerの
//!   S3/DBアクセスは`tokio::task::spawn_blocking`経由で呼ぶ(async context内からの
//!   Runtime混在panic回避。DB接続も同thread内でopen)。DB書込・syncはsingle-flight
//!   (semaphore 1)で直列化
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
    let listen_addr = sec
        .listen_addr
        .unwrap_or_else(|| DEFAULT_LISTEN_ADDR.into());
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
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
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
    let state = AppState::from_ctx_args(serve_ctx_args(args));
    let sync_ctx = state.sync_ctx.clone();
    let sync_handle =
        tokio::task::spawn_blocking(move || crate::storage::manage::catalog_sync_once(&sync_ctx));

    // serve経由の保護APIはfail-closed(P4の認証構成まで503・指摘#11)。
    // gateはbuild_routerの外側に重ね、handler本体はrouterに接続したままにする
    let app = build_router(state).layer(axum::middleware::from_fn(protected_fail_closed));
    // 全応答へCache-Controlが未設定ならno-storeを付与(API応答の途中cache禁止。
    // 静的assetsはhandler側が明示的に設定するため上書きされない)
    let app = app.layer(
        tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        ),
    );

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

/// serveのAPI共有状態(P3)。
///
/// - `sync_permits`: sync・diff実行のsingle-flight(設計1章: DB書込・syncは
///   semaphore 1で直列化)
/// - `sync_ctx`: POST /api/sync・POST /api/diffが呼ぶ同期層(open_session系)への
///   引数。serve起動時の実効profile解決(ServeArgs由来)をそのまま使う
#[derive(Clone)]
struct AppState {
    sync_permits: std::sync::Arc<tokio::sync::Semaphore>,
    sync_ctx: std::sync::Arc<crate::storage::CtxArgs>,
}

impl AppState {
    fn from_ctx_args(ctx: crate::storage::CtxArgs) -> AppState {
        AppState {
            sync_permits: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            sync_ctx: std::sync::Arc::new(ctx),
        }
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

// ---------------------------------------------------------------------------
// error体系(設計6章指摘#14: error code -> HTTP status変換。CLI exit codeの一律変換
// は行わない)。messageは区分ごとの一般化文言に固定し、内部error文字列(内部path・
// 下位層の詳細)は応答へ出さない(詳細はserver log=stderrのみ)。

/// handlerのerror応答(ApiError -> HTTP status+error body)。
struct ApiError {
    status: axum::http::StatusCode,
    code: &'static str,
    message: &'static str,
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, axum::Json(error_body(self.code, self.message))).into_response()
    }
}

fn invalid_request(message: &'static str) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::BAD_REQUEST,
        code: "invalid_request",
        message,
    }
}

fn not_found(message: &'static str) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::NOT_FOUND,
        code: "not_found",
        message,
    }
}

fn validation_failed(message: &'static str) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        code: "validation_failed",
        message,
    }
}

fn upstream(message: &'static str) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::BAD_GATEWAY,
        code: "upstream",
        message,
    }
}

fn configuration(message: &'static str) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
        code: "configuration",
        message,
    }
}

fn internal(message: &'static str) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal",
        message,
    }
}

/// 同期層(P1/S3Backend)のBackupErrorをAPI errorへ変換。詳細(log用)はここで
/// stderrへ出力し、応答messageは区分ごとの一般化文言(指摘#14: 内部error文字列の
/// 無加工返却禁止。秘密情報境界: 設定file path等を応答へ出さない)。
fn api_error_from(e: BackupError) -> ApiError {
    eprintln!("serve: API処理エラー詳細(logのみ): {e}");
    match e {
        BackupError::Internal(_) => internal("内部エラーが発生しました"),
        BackupError::Config(_) => configuration("設定の読み込みに失敗しました"),
        BackupError::Target(_) => not_found("対象が見つかりません"),
        BackupError::Process(_) => validation_failed("処理・検証に失敗しました"),
        BackupError::Transfer(_) => upstream("上流ストレージ(S3)との通信に失敗しました"),
        BackupError::Usage(_) => invalid_request("リクエストの形式が不正です"),
    }
}

/// 同期層(S3/DB)の呼び出しをspawn_blocking経由で実行(設計1章実行境界: async
/// context内からのRuntime混在panic回避。BackupErrorはApiErrorへ変換)。
async fn run_blocking<T, F>(f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> BackupResult<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result.map_err(api_error_from),
        Err(e) => {
            eprintln!("serve: spawn_blockingのjoinに失敗(logのみ): {e}");
            Err(internal("内部エラーが発生しました"))
        }
    }
}

// ---------------------------------------------------------------------------
// 読取API handler(設計6章endpoint一覧・12章P3)。応答schemaは--format jsonと共通
// (storage::queryのJSON語彙を共用)。scopeは<bucket>:<prefix>(bucket名にコロンは
// 使用できないため一意。省略時は実効profileのscope)。

/// GET /api/entities の1pageあたりのentity数(設計上の規定は無いため運用実績値で
/// ある50を固定値とする。設定化は実需要が出た段階で検討)。
const ENTITIES_PER_PAGE: usize = 50;

/// query stringの手動parse(axumのQuery extractorはrejection応答が本APIのerror
/// body形式と異なるため、形式errorを自前でinvalid_requestへmappingする)。
/// valueはpercent-decode(`%XX`・`+`=space)する。
fn parse_query_params(raw: Option<&str>) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Some(raw) = raw else {
        return map;
    };
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

/// percent-encodingのdecode(不正なescapeはそのまま文字として残す)。
fn percent_decode(s: &str) -> String {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        if b == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// page query paramのparse(1始点。0・非数値はinvalid_request 400)。
fn parse_page_param(raw: Option<&str>) -> Result<u32, ApiError> {
    let Some(raw) = raw else {
        return Ok(1);
    };
    match raw.parse::<u32>() {
        Ok(page) if page >= 1 => Ok(page),
        _ => Err(invalid_request("pageは1以上の数値で指定してください")),
    }
}

/// scope query param(<bucket>:<prefix>)のparse。形式不正はNone。
fn parse_scope_param(raw: &str) -> Option<(String, String)> {
    let (bucket, prefix) = raw.split_once(':')?;
    if bucket.is_empty() || prefix.is_empty() {
        return None;
    }
    Some((bucket.to_string(), prefix.to_string()))
}

/// 実行scopeの解決(scope明示は<bucket>:<prefix>・省略時は実効profile)。
/// 設定file読み込みを伴うためspawn_blocking内で使う。
fn resolve_scope(explicit: Option<&str>) -> BackupResult<(String, String)> {
    if let Some(scope) = explicit {
        return parse_scope_param(scope).ok_or_else(|| {
            BackupError::Usage("scopeは <bucket>:<prefix> 形式で指定してください".to_string())
        });
    }
    let aws = crate::storage::load_aws(&None, None)?;
    Ok((aws.bucket, aws.prefix))
}

/// query絞り込み(CLI queryと同一規則: title部分一致/uuid前方一致/所在path部分一致)。
fn query_match(e: &crate::index::IndexFileEntry, want: Option<&str>) -> bool {
    want.is_none_or(|t| {
        e.title.contains(t)
            || e.uuid.starts_with(t)
            || e.locations.iter().any(|l| l.source_path.contains(t))
    })
}

/// label絞り込み(いずれかのlocation.label一致)。
fn label_match(e: &crate::index::IndexFileEntry, want: Option<&str>) -> bool {
    want.is_none_or(|l| e.locations.iter().any(|loc| loc.label == l))
}

/// pin行JSON(list pins語彙: pin/date/source_size)。
fn pins_json(e: &crate::index::IndexFileEntry) -> Vec<serde_json::Value> {
    e.pins
        .iter()
        .map(|p| serde_json::json!({"pin": p.pin, "date": p.date, "source_size": p.source_size}))
        .collect()
}

/// sync_state 1行分のJSON(未syncはstatus=unknown+空値。frontendのstaleness表示が
/// 常に描画できるよう200で応答する。scopeは<bucket>:<prefix>形式)。
fn sync_state_json(
    bucket: &str,
    prefix: &str,
    st: Option<&crate::catalog::SyncState>,
) -> serde_json::Value {
    let scope = format!("{bucket}:{prefix}");
    match st {
        None => serde_json::json!({
            "scope": scope,
            "status": "unknown",
            "index_etag": "",
            "index_version_id": "",
            "fetched_at": "",
            "last_write_at": "",
            "last_verified_at": "",
            "entry_count": 0,
        }),
        Some(st) => serde_json::json!({
            "scope": scope,
            "status": st.status,
            "index_etag": st.index_etag,
            "index_version_id": st.index_version_id,
            "fetched_at": st.fetched_at,
            "last_write_at": st.last_write_at,
            "last_verified_at": st.last_verified_at,
            "entry_count": st.entry_count,
        }),
    }
}

/// router組立(設計6章のendpoint一覧のP2+P3分)。serve経由の保護APIはこのrouterの
/// 外側にfail-closed gate(protected_fail_closed)を重ねる(P4の認証構成まで503。
/// handler単体testはgate無しのこのrouterをoneshotする)。
fn build_router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/api/health", axum::routing::get(health))
        .route("/api/entities", axum::routing::get(get_entities))
        .route(
            "/api/entities/{uuid}",
            axum::routing::get(get_entity_detail),
        )
        .route(
            "/api/entities/{uuid}/versions",
            axum::routing::get(get_entity_versions),
        )
        .route("/api/pins", axum::routing::get(get_pins))
        .route("/api/locations", axum::routing::get(get_locations))
        .route("/api/pending", axum::routing::get(get_pending))
        .route(
            "/api/sync",
            axum::routing::get(get_sync_state).post(post_sync_refresh),
        )
        .route(
            "/api/diff",
            axum::routing::get(get_diff_last).post(post_diff_run),
        )
        .fallback(fallback)
        .with_state(state)
}

/// GET /api/health(認証不要のliveness・設計6章)。
async fn health() -> impl axum::response::IntoResponse {
    axum::Json(serde_json::json!({"status": "ok"}))
}

/// 保護APIのfail-closed gate(設計12章P2〜P3・指摘#11): 認証機構が構成されるまで
/// (P4)、/api/health以外の/api/*はhandlerへ渡さず503(configuration)で応答する
/// (未認証401との使い分けはP4以降)。P4でsession検証のgateへ置換える。
async fn protected_fail_closed(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path();
    if path == "/api/health" || !(path == "/api" || path.starts_with("/api/")) {
        return next.run(req).await;
    }
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(error_body(
            "configuration",
            "保護APIは認証機構の構成が完了するまで閉鎖されています",
        )),
    )
        .into_response()
}

/// GET /api/entities(設計6章: entity一覧。entry_summary_json互換schemaで
/// {"entities":[...],"page":n,"total_pages":n}形式・page=1始点)。
async fn get_entities(uri: axum::http::Uri) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let params = parse_query_params(uri.query());
    let page = parse_page_param(params.get("page").map(|s| s.as_str()))?;
    let scope = params.get("scope").cloned();
    let query = params.get("query").cloned();
    let category = params.get("category").cloned();
    let label = params.get("label").cloned();
    let entries = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(scope.as_deref())?;
        let db = crate::catalog::CatalogDb::open()?;
        let (_header, entries) = db.load_entries(&bucket, &prefix)?;
        Ok(entries)
    })
    .await?;
    // 絞り込み(query=CLI queryと同一規則・category/label=location属性一致)
    let matched: Vec<&crate::index::IndexFileEntry> = entries
        .iter()
        .filter(|e| {
            query_match(e, query.as_deref())
                && crate::storage::query::category_matched(e, category.as_deref())
                && label_match(e, label.as_deref())
        })
        .collect();
    // paging(page=1始点・total_pagesは0件でも1)
    let total = matched.len();
    let total_pages = total.div_ceil(ENTITIES_PER_PAGE).max(1);
    let skip = ((page - 1) as usize).saturating_mul(ENTITIES_PER_PAGE);
    let page_items: Vec<serde_json::Value> = matched
        .into_iter()
        .skip(skip)
        .take(ENTITIES_PER_PAGE)
        .map(|e| crate::storage::query::entry_summary_json(e, false))
        .collect();
    Ok(axum::Json(serde_json::json!({
        "entities": page_items,
        "page": page,
        "total_pages": total_pages,
    })))
}

/// 実効scopeのcacheから対象entityを1件引き当てる(spawn_blocking内で使う)。
fn find_entry_by_uuid(
    db: &crate::catalog::CatalogDb,
    bucket: &str,
    prefix: &str,
    uuid: &str,
) -> BackupResult<crate::index::IndexFileEntry> {
    let (_header, entries) = db.load_entries(bucket, prefix)?;
    entries
        .into_iter()
        .find(|e| e.uuid == uuid)
        .ok_or_else(|| BackupError::Target("対象のentityが見つかりません".to_string()))
}

/// GET /api/entities/:uuid(設計6章: 詳細=versions/pins/locations入り。
/// version_list付きentry_summary_json互換。不在は404 not_found)。
async fn get_entity_detail(
    axum::extract::Path(uuid): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None)?;
        let db = crate::catalog::CatalogDb::open()?;
        let e = find_entry_by_uuid(&db, &bucket, &prefix, &uuid)?;
        Ok(crate::storage::query::entry_summary_json(&e, true))
    })
    .await?;
    Ok(axum::Json(value))
}

/// GET /api/entities/:uuid/versions(list versions --format json相当の
/// entity object: uuid/title/locations/versions[](state付き)/pins[])。
async fn get_entity_versions(
    axum::extract::Path(uuid): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None)?;
        let db = crate::catalog::CatalogDb::open()?;
        let e = find_entry_by_uuid(&db, &bucket, &prefix, &uuid)?;
        let current_vid = e.versions.first().map(|f| f.version_id.as_str());
        Ok(serde_json::json!({
            "uuid": e.uuid,
            "title": e.title,
            "locations": e.locations.iter().map(crate::storage::query::location_json).collect::<Vec<_>>(),
            "versions": e.versions.iter().map(|v| serde_json::json!({
                "date": v.date,
                "version_ts": v.version_ts,
                "version_id": v.version_id,
                "source_size": v.source_size,
                "state": if Some(v.version_id.as_str()) == current_vid { "現行" } else { "noncurrent" },
            })).collect::<Vec<_>>(),
            "pins": pins_json(&e),
        }))
    })
    .await?;
    Ok(axum::Json(value))
}

/// GET /api/pins(list pins --format json相当: entity単位のuuid/title/pins[]を
/// {"pins":[...]}形式で返す)。
async fn get_pins() -> Result<axum::Json<serde_json::Value>, ApiError> {
    let value = run_blocking(|| {
        let (bucket, prefix) = resolve_scope(None)?;
        let db = crate::catalog::CatalogDb::open()?;
        let (_header, entries) = db.load_entries(&bucket, &prefix)?;
        Ok(serde_json::json!({
            "pins": entries.iter().map(|e| serde_json::json!({
                "uuid": e.uuid,
                "title": e.title,
                "pins": pins_json(e),
            })).collect::<Vec<_>>(),
        }))
    })
    .await?;
    Ok(axum::Json(value))
}

/// GET /api/locations(location_json語彙+entityのuuid/title案内を
/// {"locations":[...]}形式で返す)。
async fn get_locations() -> Result<axum::Json<serde_json::Value>, ApiError> {
    let value = run_blocking(|| {
        let (bucket, prefix) = resolve_scope(None)?;
        let db = crate::catalog::CatalogDb::open()?;
        let (_header, entries) = db.load_entries(&bucket, &prefix)?;
        let rows: Vec<serde_json::Value> = entries
            .iter()
            .flat_map(|e| {
                e.locations.iter().map(move |l| {
                    serde_json::json!({
                        "uuid": e.uuid,
                        "title": e.title,
                        "source_path": l.source_path,
                        "category": l.category,
                        "label": l.label,
                        "identity_hash": l.identity_hash,
                        "last_backup": l.last_backup,
                    })
                })
            })
            .collect();
        Ok(serde_json::json!({ "locations": rows }))
    })
    .await?;
    Ok(axum::Json(value))
}

/// GET /api/pending(pending_itemsのschema(source_root/path/size/mtime/status/
/// last_scan_at)で{"pending":[...]}形式。P3時点はscan(P5)が未実装のため空配列)。
async fn get_pending() -> Result<axum::Json<serde_json::Value>, ApiError> {
    let value = run_blocking(|| {
        let (bucket, prefix) = resolve_scope(None)?;
        let db = crate::catalog::CatalogDb::open()?;
        let items = db.pending_items(&bucket, &prefix)?;
        Ok(serde_json::json!({
            "pending": items.iter().map(|i| serde_json::json!({
                "source_root": i.source_root,
                "path": i.path,
                "size": i.size,
                "mtime": i.mtime,
                "status": i.status,
                "last_scan_at": i.last_scan_at,
            })).collect::<Vec<_>>(),
        }))
    })
    .await?;
    Ok(axum::Json(value))
}

/// GET /api/sync(sync_state 1行分。未sync時もstatus=unknownで200=frontendの
/// staleness表示が常に描画できる)。
async fn get_sync_state() -> Result<axum::Json<serde_json::Value>, ApiError> {
    let value = run_blocking(|| {
        let (bucket, prefix) = resolve_scope(None)?;
        let db = crate::catalog::CatalogDb::open()?;
        let st = db.sync_state(&bucket, &prefix)?;
        Ok(sync_state_json(&bucket, &prefix, st.as_ref()))
    })
    .await?;
    Ok(axum::Json(value))
}

/// POST /api/sync(手動refresh: 起動時syncと同一のP1同期層catalog_sync_onceを
/// single-flightで実行し、最新のsync_stateで応答。未書込namespaceは空cacheの
/// fresh確定として成功)。
async fn post_sync_refresh(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    // single-flight(設計1章: syncはsemaphore 1で直列化)
    let _permit = state
        .sync_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| {
            eprintln!("serve: sync semaphoreのacquireに失敗(logのみ): {e}");
            internal("内部エラーが発生しました")
        })?;
    let ctx = state.sync_ctx.clone();
    let value = run_blocking(move || {
        crate::storage::manage::catalog_sync_once(&ctx)?;
        let aws = crate::storage::load_aws(&ctx.aws_config, ctx.profile.as_deref())?;
        let db = crate::catalog::CatalogDb::open()?;
        let st = db.sync_state(&aws.bucket, &aws.prefix)?;
        Ok(sync_state_json(&aws.bucket, &aws.prefix, st.as_ref()))
    })
    .await?;
    Ok(axum::Json(value))
}

/// POST /api/diff(diff実行: local catalog cache(索引replica)とS3 list_versionsの
/// 全体突合=cmd_diffのJSON語彙{entries/files/missing/unexpected}+performed_at。
/// 結果をDBへ保存しGET /api/diffで参照できるようにする)。
async fn post_diff_run(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    // diff結果のDB保存を伴うためsyncと同じsingle-flightで直列化(設計1章)
    let _permit = state
        .sync_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| {
            eprintln!("serve: diff semaphoreのacquireに失敗(logのみ): {e}");
            internal("内部エラーが発生しました")
        })?;
    let ctx = state.sync_ctx.clone();
    let value = run_blocking(move || {
        use std::collections::BTreeSet;
        let session = crate::storage::open_session(&ctx)?;
        let bucket = session.aws.bucket.clone();
        let prefix = session.aws.prefix.clone();
        let db = crate::catalog::CatalogDb::open()?;
        let (_header, entries) = db.load_entries(&bucket, &prefix)?;
        let versions = session.backend.list_versions(&bucket, &prefix)?;
        let present: BTreeSet<&str> = versions
            .iter()
            .map(|v| v.key.as_str())
            .filter(|k| !crate::storage::query::diff_out_of_scope(&prefix, k))
            .collect();
        let mut index_keys = BTreeSet::new();
        for e in &entries {
            for v in &e.versions {
                index_keys.insert(v.s3_key.clone());
            }
            for p in &e.pins {
                index_keys.insert(p.s3_key.clone());
            }
        }
        let missing: Vec<String> = index_keys
            .iter()
            .filter(|k| !present.contains(k.as_str()))
            .cloned()
            .collect();
        let mut unexpected: Vec<&str> = Vec::new();
        for k in &present {
            if !index_keys.contains(*k) {
                unexpected.push(k);
            }
        }
        let performed_at = crate::storage::now_jst();
        let result = serde_json::json!({
            "performed_at": performed_at,
            "entries": entries.len(),
            "files": entries
                .iter()
                .map(|e| serde_json::json!({"uuid": e.uuid, "title": e.title}))
                .collect::<Vec<_>>(),
            "missing": missing,
            "unexpected": unexpected,
        });
        db.save_diff_result(&bucket, &prefix, &performed_at, &result)?;
        Ok(result)
    })
    .await?;
    Ok(axum::Json(value))
}

/// GET /api/diff(保存済みの最終diff結果のみ。未実行は404 not_found)。
async fn get_diff_last() -> Result<axum::Json<serde_json::Value>, ApiError> {
    let value = run_blocking(|| {
        let (bucket, prefix) = resolve_scope(None)?;
        let db = crate::catalog::CatalogDb::open()?;
        db.load_diff_result(&bucket, &prefix)?
            .ok_or_else(|| BackupError::Target("diff結果は未実行です".to_string()))
    })
    .await?;
    Ok(axum::Json(value))
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
    /// fieldは宣言順にdropされるため_lockは最後に宣言する(lockが先に解放されると
    /// 後続testのABH設定を_abhの復元が上書きしてしまう)。
    struct ApiEnv {
        /// ABH home(内部error文字列の無加工返却検出の対象path)
        home: PathBuf,
        _abh: crate::testutil::AbhEnvGuard,
        _tmp: TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
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
        use crate::catalog::{CatalogDb, ReflectOrigin, STATUS_FRESH, SourceMtimes, Validator};
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
    fn api_entry(
        uuid: &str,
        title: &str,
        category: &str,
        label: &str,
    ) -> crate::index::IndexFileEntry {
        use crate::index::{INDEX_SCHEMA, IndexFileEntry, IndexLocation, IndexPin, IndexVersion};
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

    /// handler単体test用のAppState(serve起動時のServeArgs省略と同一=CtxArgs全None
    /// の規定path解決。ABH配下のconfigをapi_envが用意する)。
    fn api_test_state() -> AppState {
        AppState::from_ctx_args(crate::storage::CtxArgs {
            config: None,
            aws_config: None,
            key: None,
            profile: None,
            format: crate::storage::Format::Human,
        })
    }

    /// build_routerをoneshotで呼び(status, JSON body)を返す。bodyがJSONで無い場合は
    /// Null(応答形式の検査でfailする)。P3実装でbuild_routerがAppStateを受け取る
    /// signatureへ変わったため、この呼び出しのみ追随(期待値は不変)。
    fn api(method: &str, uri: &str) -> (u16, serde_json::Value) {
        let router = build_router(api_test_state());
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
            error
                .get("message")
                .is_some_and(|m| m.as_str().is_some_and(|m| !m.is_empty())),
            "messageは空でないstring: {body}"
        );
    }

    /// [covers:server.entities-list-endpoint]
    /// GET /api/entitiesはentry_summary_json互換schemaで実効scopeのentity一覧を返し、
    /// query/category/label/scope絞り込みとpageingが機能する。
    #[test]
    fn api_entities_list_responds_entry_summaries_with_filters() {
        let env = api_env("srv-api-entities");
        let alpha = api_entry(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
        );
        let beta = api_entry(
            "22222222-2222-2222-2222-222222222222",
            "beta",
            "unfiled",
            "",
        );
        api_seed(&[alpha, beta], API_TEST_BUCKET, API_TEST_PREFIX);
        // 実効scope外のデータ(scope絞り込みの対照)
        let gamma = api_entry(
            "33333333-3333-3333-3333-333333333333",
            "gamma",
            "docs",
            "work",
        );
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
            entities
                .iter()
                .all(|e| e.get("uuid").is_some_and(|u| u != gamma_uuid())),
            "他scope(other-bucket)のentityは含まれない: {body}"
        );
        // pageing(1page目・総page数)
        assert_eq!(
            body.get("page").and_then(|p| p.as_i64()),
            Some(1),
            "page=1: {body}"
        );
        assert!(
            body.get("total_pages")
                .and_then(|p| p.as_i64())
                .is_some_and(|p| p >= 1),
            "total_pages付き: {body}"
        );
        // entry_summary_json互換schema(uuid/title/locations/versions=count/
        // latest_date/latest_size/pins)
        let first = &entities[0];
        for key in ["uuid", "title", "latest_date", "latest_size"] {
            assert!(
                first.get(key).is_some(),
                "entry_summary互換key {key}: {first}"
            );
        }
        assert!(first.get("versions").is_some_and(|v| v.as_i64() == Some(1)));
        let locations = first
            .get("locations")
            .and_then(|v| v.as_array())
            .expect("locations配列");
        let l0 = &locations[0];
        for key in [
            "source_path",
            "category",
            "label",
            "identity_hash",
            "last_backup",
        ] {
            assert!(
                l0.get(key).is_some_and(|v| !v.is_null()),
                "locationのkey {key}: {l0}"
            );
        }
        assert!(
            first
                .get("pins")
                .is_some_and(|p| p.as_array().is_some_and(|a| !a.is_empty())),
            "pins配列: {first}"
        );

        // query絞り込み(title部分一致)
        let (_, body) = api("GET", "/api/entities?query=beta");
        let entities = body
            .get("entities")
            .and_then(|v| v.as_array())
            .expect("entities");
        assert_eq!(entities.len(), 1, "query=betaはbetaのみ: {body}");
        assert!(entities[0].get("title").is_some_and(|t| t == "beta"));

        // category絞り込み(location.category一致)
        let (_, body) = api("GET", "/api/entities?category=docs");
        let entities = body
            .get("entities")
            .and_then(|v| v.as_array())
            .expect("entities");
        assert_eq!(entities.len(), 1, "category=docsはalphaのみ: {body}");
        assert!(entities[0].get("title").is_some_and(|t| t == "alpha"));

        // label絞り込み(location.label一致)
        let (_, body) = api("GET", "/api/entities?label=work");
        let entities = body
            .get("entities")
            .and_then(|v| v.as_array())
            .expect("entities");
        assert_eq!(entities.len(), 1, "label=workはalphaのみ: {body}");

        // scope明示絞り込み(他bucketを明示すればgammaが見える)
        let (_, body) = api("GET", "/api/entities?scope=other-bucket:amazing-backup/");
        let entities = body
            .get("entities")
            .and_then(|v| v.as_array())
            .expect("entities");
        assert_eq!(
            entities.len(),
            1,
            "scope=other-bucket:amazing-backup/はgammaのみ: {body}"
        );
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
        let alpha = api_entry(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
        );
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api("GET", "/api/entities/11111111-1111-1111-1111-111111111111");
        assert_eq!(status, 200, "entity詳細は200: {body}");
        assert_eq!(
            body.get("uuid").and_then(|v| v.as_str()),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(body.get("title").and_then(|v| v.as_str()), Some("alpha"));
        assert!(
            body.get("locations")
                .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty())),
            "詳細はlocationsを含む: {body}"
        );
        assert!(
            body.get("pins")
                .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty())),
            "詳細はpinsを含む: {body}"
        );
        let version_list = body
            .get("version_list")
            .and_then(|v| v.as_array())
            .expect("詳細はversion_listを含む");
        let v0 = &version_list[0];
        for key in [
            "date",
            "version_ts",
            "version_id",
            "batch_id",
            "source_size",
        ] {
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
        let alpha = api_entry(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
        );
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api(
            "GET",
            "/api/entities/11111111-1111-1111-1111-111111111111/versions",
        );
        assert_eq!(status, 200, "versions一覧は200: {body}");
        assert_eq!(
            body.get("uuid").and_then(|v| v.as_str()),
            Some("11111111-1111-1111-1111-111111111111")
        );
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

        let (status, body) = api(
            "GET",
            "/api/entities/99999999-9999-9999-9999-999999999999/versions",
        );
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
        let alpha = api_entry(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
        );
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
        let alpha = api_entry(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
        );
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api("GET", "/api/locations");
        assert_eq!(status, 200, "GET /api/locationsは200: {body}");
        let locations = body
            .get("locations")
            .and_then(|v| v.as_array())
            .expect("locations配列");
        assert_eq!(locations.len(), 1, "location行: {body}");
        let row = &locations[0];
        for key in [
            "uuid",
            "title",
            "source_path",
            "category",
            "label",
            "identity_hash",
            "last_backup",
        ] {
            assert!(
                row.get(key).is_some_and(|v| !v.is_null()),
                "location行のkey {key}: {row}"
            );
        }
        assert_eq!(
            row.get("source_path").and_then(|v| v.as_str()),
            Some("/data/alpha")
        );
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
        assert!(pending.is_empty(), "P3時点はscan未実装のため空: {body}");
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
        assert_eq!(
            body.get("status").and_then(|s| s.as_str()),
            Some("unknown"),
            "未syncはunknown: {body}"
        );

        // seed反映後はsync_stateの保存値を返す
        let alpha = api_entry(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
        );
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);
        let (status, body) = api("GET", "/api/sync");
        assert_eq!(status, 200, "sync状態は200: {body}");
        assert_eq!(
            body.get("status").and_then(|s| s.as_str()),
            Some("fresh"),
            "seed直後はfresh: {body}"
        );
        assert_eq!(
            body.get("scope").and_then(|s| s.as_str()),
            Some("api-bucket:amazing-backup/"),
            "scopeは<bucket>:<prefix>形式: {body}"
        );
        assert_eq!(
            body.get("index_etag").and_then(|v| v.as_str()),
            Some("\"etag-1\"")
        );
        assert_eq!(
            body.get("index_version_id").and_then(|v| v.as_str()),
            Some("vid-1")
        );
        for key in ["fetched_at", "last_write_at", "last_verified_at"] {
            assert!(
                body.get(key).is_some_and(|v| v.as_str().is_some()),
                "key {key}: {body}"
            );
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
        assert_eq!(
            body.get("status").and_then(|s| s.as_str()),
            Some("fresh"),
            "refresh後の状態: {body}"
        );
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
        assert_eq!(
            body.get("entries").and_then(|v| v.as_i64()),
            Some(0),
            "空結果: {body}"
        );

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
        let alpha = api_entry(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
        );
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
        let alpha = api_entry(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
        );
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

    // -----------------------------------------------------------------------
    // P4(passkey認証)のhandler単体test(設計12章P4・8章の認証契約・11章の
    // server-to-server自動検証=指摘#12)。HTTP request/responseの連鎖(router
    // oneshot)で認証flowを観測する。bootstrap token・challenge・sessionはserver
    // stateに保持されるため、P3のapi()(呼び出しごとに新規routerを作る)と違い
    // AuthFlowで同一routerを使い回す(restart()でstateを作り直す=再起動相当)。
    // webauthn-rs 0.6系にvirtual authenticator/soft passkeyのtest支援は存在しない
    // (2026-11-05調査・条件書tests/design/webauthn.toml header参照)ため、WebAuthn
    // 仕様準拠の合成認証応答(soft passkey)でregister/loginを完結する。
    // 実装前のため本test群は実行failを許容(cargo test --no-runのcompileは通る)。

    /// test用の固定bootstrap token(秘密ではなく契約観測用の合成値。実装のtoken生成が
    /// 128bit乱数base64であることの観測はwebauthn.bootstrap-token-generation)。
    const AUTH_TEST_TOKEN: &str = "YXV0aC10b2tlbi0xMjgtYml0LXRlc3QtdmFsdWU";
    /// soft passkeyのcredential id(固定値・base64url符号化)。
    const SOFT_PASSKEY_CREDENTIAL_ID: &str = "c29mdC1wYXNza2V5LWNyZWRlbnRpYWwtaWQ";

    /// P4認証flow観測用環境(api_env+同一routerを使い回す呼び出し経路)。
    /// bootstrap token・challenge・sessionはrouterのserver stateに保持される
    /// (callのたびにstateを作り直さない=single-use消費・session継続を観測できる)。
    struct AuthFlow {
        /// api_envの生存期間をflowと同じにする(env書換えの復元をtest末尾へ)
        #[allow(dead_code)]
        env: ApiEnv,
        router: axum::Router,
    }

    impl AuthFlow {
        fn new(tag: &str) -> AuthFlow {
            AuthFlow {
                env: api_env(tag),
                router: build_router(api_test_state()),
            }
        }

        /// server stateを作り直す(serve再起動相当。token・challenge等のin-memory
        /// 状態は消える=webauthn.bootstrap-token-restart-invalidationの観測)。
        /// TODO(P4実装): api_test_state()へbootstrap token(AUTH_TEST_TOKEN)を注入する
        /// seamが加わった場合はこの呼び出しのみ追随(期待値は不変)。
        fn restart(&mut self) {
            self.router = build_router(api_test_state());
        }

        /// oneshot呼び出し(JSON body+Cookie header付き)。応答は
        /// (status, JSON body, Set-Cookie一覧)。bodyがJSONで無い場合はNull。
        fn call(
            &self,
            method: &str,
            uri: &str,
            body: Option<serde_json::Value>,
            cookie: Option<&str>,
        ) -> (u16, serde_json::Value, Vec<String>) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("oneshot runtime");
            rt.block_on(async {
                use tower::ServiceExt as _;
                let mut builder = axum::http::Request::builder()
                    .method(
                        axum::http::Method::from_bytes(method.as_bytes())
                            .expect("method"),
                    )
                    .uri(uri);
                if let Some(c) = cookie {
                    builder = builder.header("cookie", c);
                }
                let req = match body {
                    Some(b) => builder
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(b.to_string()))
                        .expect("request build"),
                    None => builder
                        .body(axum::body::Body::empty())
                        .expect("request build"),
                };
                let resp = self
                    .router
                    .clone()
                    .oneshot(req)
                    .await
                    .expect("oneshot");
                let status = resp.status().as_u16();
                let set_cookies = resp
                    .headers()
                    .get_all(axum::http::header::SET_COOKIE)
                    .iter()
                    .filter_map(|v| v.to_str().ok().map(String::from))
                    .collect();
                let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                    .await
                    .expect("body read");
                let value =
                    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
                (status, value, set_cookies)
            })
        }
    }

    /// Set-Cookie列から最初のcookieの(name, value)を抽出(session ID比較用)。
    fn first_cookie(set_cookies: &[String]) -> Option<(String, String)> {
        let first = set_cookies.first()?;
        let pair = first.split(';').next()?.trim();
        pair.split_once('=')
            .map(|(k, v)| (k.to_string(), v.to_string()))
    }

    /// catalog.db(ABH配下)へcredentialを1件挿入(登録済み状態の作成=P1 test用API)。
    fn auth_seed_credential(credential_id: &str) {
        let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
        db.insert_credential_for_test(credential_id)
            .expect("credential挿入");
    }

    /// register/finishへ送る合成認証応答(RegisterPublicKeyCredential相当のJSON構造)。
    /// TODO(P4実装): optionsのchallengeからclientDataJSON(type=webauthn.create・
    /// challenge・origin=rp_origin)を組み、authenticatorData(rp_idのSHA-256+flags
    /// UP|UV)・attestationObject(fmt="none"+authData+COSE EC2 P-256 ES256公開鍵)を
    /// 合成する(P-256鍵pairはtest側で生成)。実装前は構造のみで署名検証に落ちて
    /// failを許容する(webauthn-rs 0.6系の応答型・base64url符号化への接続はP4実装時)。
    fn soft_register_response(_options: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": SOFT_PASSKEY_CREDENTIAL_ID,
            "rawId": SOFT_PASSKEY_CREDENTIAL_ID,
            "type": "public-key",
            "response": {
                // TODO(P4): fmt=none+COSE鍵の合成attestationObject
                "attestationObject": "o2NmbQ",
                // TODO(P4): challenge束ねのclientDataJSON合成
                "clientDataJSON": "eyJ0eXBlIjoid2ViYXV0aG4uY3JlYXRlIn0",
                "transports": ["internal"],
            },
        })
    }

    /// login/finishへ送る合成認証応答(PublicKeyCredential相当のJSON構造)。
    /// TODO(P4実装): challengeからclientDataJSON(type=webauthn.get・challenge・origin)
    /// を組み、authenticatorData+signature(P-256 ECDSA・SHA-256)を合成する。
    fn soft_login_assertion(_challenge: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": SOFT_PASSKEY_CREDENTIAL_ID,
            "rawId": SOFT_PASSKEY_CREDENTIAL_ID,
            "type": "public-key",
            "response": {
                // TODO(P4): rp_id_hash+flagsのauthenticatorData合成
                "authenticatorData": "SZYN5YgOjGh0NBcPZHZgW4_krrmihjLHmVzzuoMdl2M",
                // TODO(P4): challenge束ねのclientDataJSON合成
                "clientDataJSON": "eyJ0eXBlIjoid2ViYXV0aG4uZ2V0In0",
                // TODO(P4): P-256 ECDSA署名合成
                "signature": "MEUCIQDb3ZhbHVlLXNpZ25hdHVyZS1wbGFjZWhvbGRlcg",
                "userHandle": null,
            },
        })
    }

    /// [covers:server.auth-status-endpoint]
    /// GET /api/auth/statusは認証不要で認証状態と登録credential数を応答する
    /// (未登録・未認証でも200=LoginViewのUI誘導基盤)。
    #[test]
    fn api_auth_status_responds_auth_state_and_credential_count() {
        let flow = AuthFlow::new("srv-auth-status");
        let (status, body, _) = flow.call("GET", "/api/auth/status", None, None);
        assert_eq!(status, 200, "GET /api/auth/statusは200: {body}");
        assert_eq!(
            body.get("authenticated").and_then(|v| v.as_bool()),
            Some(false),
            "未認証はauthenticated=false: {body}"
        );
        assert_eq!(
            body.get("credentials").and_then(|v| v.as_i64()),
            Some(0),
            "未登録はcredentials=0: {body}"
        );

        // credentials tableへ1件挿入後はcredentials=1(passkey登録済み状態)
        auth_seed_credential("soft-credential-1");
        let (status, body, _) = flow.call("GET", "/api/auth/status", None, None);
        assert_eq!(status, 200);
        assert_eq!(
            body.get("credentials").and_then(|v| v.as_i64()),
            Some(1),
            "挿入後はcredentials=1: {body}"
        );
    }

    /// [covers:server.auth-endpoints-routing]
    /// auth endpoint群は404 fallbackでなく定義済みendpointとして応答する
    /// (register/startはtoken必須401・logoutは要認証401)。
    #[test]
    fn api_auth_endpoints_are_routed() {
        let flow = AuthFlow::new("srv-auth-routing");
        // register/start: token無しは401 unauthenticated(bootstrap token必須)
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/register/start",
            Some(serde_json::json!({})),
            None,
        );
        assert_ne!(status, 404, "register/startはroutingされる(404 fallbackでない)");
        assert_eq!(status, 401, "token無しは401: {body}");
        expect_error_body(&body, "unauthenticated");

        // 残るauth endpointも404 fallbackでない定義済み応答
        for (method, path) in [
            ("POST", "/api/auth/register/finish"),
            ("POST", "/api/auth/login/start"),
            ("POST", "/api/auth/login/finish"),
            ("POST", "/api/auth/logout"),
        ] {
            let (status, _, _) =
                flow.call(method, path, Some(serde_json::json!({})), None);
            assert_ne!(status, 404, "{method} {path}はroutingされる(404 fallbackでない)");
        }

        // logoutは要認証(未認証401)
        let (status, body, _) =
            flow.call("POST", "/api/auth/logout", Some(serde_json::json!({})), None);
        assert_eq!(status, 401, "logoutは要認証(未認証401): {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.register-closed-without-valid-token]
    /// credential数0かつtoken不一致(無し・誤値)なら登録は閉鎖(fail-closed)。
    #[test]
    fn api_register_closed_without_valid_token() {
        let flow = AuthFlow::new("srv-auth-closed");
        // token無し
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/register/start",
            Some(serde_json::json!({})),
            None,
        );
        assert_eq!(status, 401, "token無しは401: {body}");
        expect_error_body(&body, "unauthenticated");
        // 不一致token
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/register/start",
            Some(serde_json::json!({"token": "wrong-token-value"})),
            None,
        );
        assert_eq!(status, 401, "不一致tokenは401(登録閉鎖=fail-closed): {body}");
        expect_error_body(&body, "unauthenticated");
        // 登録options(challenge)は応答に含まれない
        assert!(
            body.get("challenge").is_none() && body.get("publicKey").is_none(),
            "登録optionsは返らない: {body}"
        );
    }

    /// [covers:webauthn.bootstrap-token-single-use]
    /// tokenは原子的消費(1回限り)。有効tokenでのregister/startは一度だけ成功し、
    /// 使用済み同一tokenでの再register/startは401で拒否される。
    #[test]
    fn api_bootstrap_token_is_consumed_once() {
        // TODO(P4実装): api_test_state()へのbootstrap token注入seam(AUTH_TEST_TOKEN)を
        // 通じて有効token状態を作る(実装まで本testはfailを許容)。
        let flow = AuthFlow::new("srv-auth-token-once");
        let req = serde_json::json!({"token": AUTH_TEST_TOKEN});
        // 1回目: 有効tokenでのregister/startは登録options+anonymous session cookie
        let (status, body, cookies) =
            flow.call("POST", "/api/auth/register/start", Some(req.clone()), None);
        assert_eq!(status, 200, "有効tokenでのregister/startは200: {body}");
        assert!(
            body.get("challenge").is_some() || body.get("publicKey").is_some(),
            "登録options(challenge含有)を返す: {body}"
        );
        assert!(!cookies.is_empty(), "anonymous session cookie(Set-Cookie)を発行");
        // 2回目: 同一tokenは消費済みのため拒否(原子的消費=1回限り)
        let (status, body, _) =
            flow.call("POST", "/api/auth/register/start", Some(req), None);
        assert_eq!(status, 401, "使用済みtokenの再使用は401: {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.bootstrap-token-expiry]
    /// token期限は10分。期限切れtokenでのregister/startは401(登録閉鎖)。
    #[test]
    fn api_bootstrap_token_expires_after_10min() {
        // TODO(P4実装): 時刻注入seamでtoken発行から10分経過を注入してから
        // register/startを呼ぶ(この位置にseam呼び出しを挿入。実装まで本testは
        // failを許容・実時間の経過待ちには依存しない)。
        let flow = AuthFlow::new("srv-auth-token-expiry");
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/register/start",
            Some(serde_json::json!({"token": AUTH_TEST_TOKEN})),
            None,
        );
        assert_eq!(status, 401, "期限切れtokenは401(不一致と同じ登録閉鎖): {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.bootstrap-token-restart-invalidation]
    /// tokenはin-memory保持のためserve再起動(=新規server state)で失効する。
    #[test]
    fn api_bootstrap_token_invalidated_on_restart() {
        // TODO(P4実装): api_test_state()へのtoken注入seam(AUTH_TEST_TOKEN)。
        let mut flow = AuthFlow::new("srv-auth-token-restart");
        let req = serde_json::json!({"token": AUTH_TEST_TOKEN});
        let (status, _, _) =
            flow.call("POST", "/api/auth/register/start", Some(req.clone()), None);
        assert_eq!(status, 200, "同一state内ではtokenは有効");
        // stateを作り直す(再起動相当)後は同一tokenは無効(in-memory保持のため)
        flow.restart();
        let (status, body, _) = flow.call("POST", "/api/auth/register/start", Some(req), None);
        assert_eq!(status, 401, "再起動でtokenは失効(永続DBに置かない): {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.additional-register-requires-login-and-token]
    /// 登録済み(credential数>0)での追加登録はlogin済みsession+tokenの両方必須
    /// (未loginでのtoken提示のみでは拒否)。
    #[test]
    fn api_additional_register_requires_login_and_token() {
        let flow = AuthFlow::new("srv-auth-additional");
        auth_seed_credential("existing-credential-1");
        // TODO(P4実装): token注入seam(AUTH_TEST_TOKEN)。login済みsessionとの併用で
        // 追加登録が許可される観測はflow test側で強化する。
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/register/start",
            Some(serde_json::json!({"token": AUTH_TEST_TOKEN})),
            None,
        );
        assert_eq!(status, 401, "未loginでの追加登録は401: {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.challenge-single-use-expiry]
    /// challengeは一度限り消費・期限120秒。使用済みchallengeでの再finishは拒否。
    #[test]
    fn api_challenge_is_consumed_once() {
        let flow = AuthFlow::new("srv-auth-challenge-once");
        auth_seed_credential("soft-credential-1");
        // login/start->finish(soft passkey)完結後に同一challengeで再finish
        // (finish成功にはP4実装時の署名合成が必要=実装まで本testはfailを許容)
        let (status, challenge, cookies) =
            flow.call("POST", "/api/auth/login/start", Some(serde_json::json!({})), None);
        assert_eq!(status, 200, "login/startはchallengeを返す: {challenge}");
        let (name, value) = first_cookie(&cookies).expect("anonymous session cookie");
        let cookie = format!("{name}={value}");
        let assertion = soft_login_assertion(&challenge);
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(assertion.clone()),
            Some(&cookie),
        );
        assert_eq!(status, 200, "login/finish成功(soft passkey): {body}");
        // 同一challengeでの再finishは拒否(challengeは一度限り消費)
        let (status, body, _) =
            flow.call("POST", "/api/auth/login/finish", Some(assertion), Some(&cookie));
        assert_eq!(status, 401, "使用済みchallengeの再使用は401: {body}");
        expect_error_body(&body, "unauthenticated");
        // TODO(P4実装): 時刻注入seamで120秒経過を注入した期限切れchallengeも
        // 同様に401となることを観測する(実時間の経過待ちには依存しない)。
    }

    /// [covers:webauthn.challenge-bound-to-session-cookie]
    /// challengeは発行時のanonymous session cookieと結合(別browserへの転用は拒否)。
    #[test]
    fn api_challenge_is_bound_to_anonymous_session_cookie() {
        let flow = AuthFlow::new("srv-auth-challenge-bind");
        auth_seed_credential("soft-credential-1");
        let (status, challenge, cookies) =
            flow.call("POST", "/api/auth/login/start", Some(serde_json::json!({})), None);
        assert_eq!(status, 200, "login/start: {challenge}");
        let (name, _value) = first_cookie(&cookies).expect("anonymous session cookie");
        // 別cookie(転用先=別browserのsession・同名別値)でfinish
        let other_cookie = format!("{name}=another-browser-session-value");
        let assertion = soft_login_assertion(&challenge);
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(assertion.clone()),
            Some(&other_cookie),
        );
        assert_eq!(status, 401, "別cookieでのchallenge使用は401(転用拒否): {body}");
        expect_error_body(&body, "unauthenticated");
        // cookie無しでも拒否
        let (status, body, _) =
            flow.call("POST", "/api/auth/login/finish", Some(assertion), None);
        assert_eq!(status, 401, "cookie無しのfinishも401: {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.register-login-flow-server-to-server]
    /// [covers:server.protected-api-open-with-auth]
    /// server-to-serverでpasskey登録・login flowを完結(soft passkey合成応答)。
    /// 認証済みsessionで保護API(GET /api/entities)が応答する。
    #[test]
    fn api_register_and_login_flow_with_soft_passkey() {
        let flow = AuthFlow::new("srv-auth-flow");
        // register: tokenでstart->合成応答でfinish->credentials table(永続class)へ保存
        // TODO(P4実装): api_test_state()へのtoken注入seam(AUTH_TEST_TOKEN)
        let (status, options, cookies) = flow.call(
            "POST",
            "/api/auth/register/start",
            Some(serde_json::json!({"token": AUTH_TEST_TOKEN})),
            None,
        );
        assert_eq!(status, 200, "register/start: {options}");
        let (rname, rvalue) = first_cookie(&cookies).expect("anonymous session cookie");
        let register_cookie = format!("{rname}={rvalue}");
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/register/finish",
            Some(soft_register_response(&options)),
            Some(&register_cookie),
        );
        assert_eq!(status, 200, "register/finish(soft passkey): {body}");
        let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
        assert_eq!(
            db.credential_count().expect("credentials計数"),
            1,
            "credentials table(永続class)へ保存される"
        );
        drop(db);

        // login: start->合成assertionでfinish->認証済みsession
        let (status, challenge, cookies) =
            flow.call("POST", "/api/auth/login/start", Some(serde_json::json!({})), None);
        assert_eq!(status, 200, "login/start: {challenge}");
        let (lname, lvalue) = first_cookie(&cookies).expect("anonymous session cookie(login)");
        let login_cookie = format!("{lname}={lvalue}");
        let (status, body, auth_cookies) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(soft_login_assertion(&challenge)),
            Some(&login_cookie),
        );
        assert_eq!(status, 200, "login/finish(soft passkey): {body}");
        let (aname, avalue) = first_cookie(&auth_cookies).expect("認証済みsession cookie");

        // 認証済みsessionで保護API(P3読取API)が応答する(保護API開放)
        let auth_cookie = format!("{aname}={avalue}");
        let (status, body, _) = flow.call("GET", "/api/entities", None, Some(&auth_cookie));
        assert_eq!(status, 200, "認証済みsessionで保護APIが応答: {body}");
        assert!(body.get("entities").is_some(), "entity一覧応答: {body}");
    }

    /// [covers:webauthn.login-session-id-rotation]
    /// login成功時にsession IDを付け替える(session固定化対策)。
    #[test]
    fn api_login_rotates_session_id() {
        let flow = AuthFlow::new("srv-auth-rotation");
        auth_seed_credential("soft-credential-1");
        let (status, _, cookies) =
            flow.call("POST", "/api/auth/login/start", Some(serde_json::json!({})), None);
        assert_eq!(status, 200, "login/start");
        let (name, anon_value) = first_cookie(&cookies).expect("anonymous session cookie");
        let login_cookie = format!("{name}={anon_value}");
        let (status, body, cookies) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(soft_login_assertion(&serde_json::json!({}))),
            Some(&login_cookie),
        );
        assert_eq!(status, 200, "login成功: {body}");
        let (_, auth_value) = first_cookie(&cookies).expect("認証済みsession cookie");
        assert_ne!(
            anon_value, auth_value,
            "login成功時にsession IDは付替わる(固定化対策)"
        );
    }

    /// [covers:webauthn.session-cookie-attributes]
    /// session cookieはHttpOnly+SameSite=Strict(localhost既定rp_origin=httpでは
    /// Secure無し)。
    #[test]
    fn api_session_cookie_attributes() {
        let flow = AuthFlow::new("srv-auth-cookie-attr");
        auth_seed_credential("soft-credential-1");
        let (status, _, cookies) =
            flow.call("POST", "/api/auth/login/start", Some(serde_json::json!({})), None);
        assert_eq!(status, 200, "login/start");
        let joined = cookies.join("\n");
        assert!(joined.contains("HttpOnly"), "HttpOnly必須: {joined}");
        assert!(
            joined.contains("SameSite=Strict"),
            "SameSite=Strict必須: {joined}"
        );
        assert!(
            !joined.contains("Secure"),
            "localhost既定rp_origin(http)ではSecure無し: {joined}"
        );
    }

    /// [covers:webauthn.session-expiry-24h]
    /// session期限は24時間・非活動で失効(期限切れsessionでの保護API accessは401)。
    #[test]
    fn api_session_expires_after_24h() {
        // TODO(P4実装): 時刻注入seamでlogin成功から24時間経過を注入してから保護API
        // を呼ぶ(この位置にseam呼び出しを挿入。実装まで本testはfailを許容)。
        let flow = AuthFlow::new("srv-auth-session-expiry");
        let (status, body, _) =
            flow.call("GET", "/api/entities", None, Some("id=expired-session-value"));
        assert_eq!(status, 401, "期限切れsessionは未認証扱い(401): {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.logout-invalidates-session]
    /// POST /api/auth/logoutはsessionを破棄する(logout後の同一cookieでの保護API
    /// accessは401)。
    #[test]
    fn api_logout_invalidates_session() {
        let flow = AuthFlow::new("srv-auth-logout");
        auth_seed_credential("soft-credential-1");
        // loginまで完結(soft passkey)
        let (status, challenge, cookies) =
            flow.call("POST", "/api/auth/login/start", Some(serde_json::json!({})), None);
        assert_eq!(status, 200, "login/start");
        let (name, value) = first_cookie(&cookies).expect("anonymous session cookie");
        let login_cookie = format!("{name}={value}");
        let (status, body, cookies) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(soft_login_assertion(&challenge)),
            Some(&login_cookie),
        );
        assert_eq!(status, 200, "login成功: {body}");
        let (aname, avalue) = first_cookie(&cookies).expect("認証済みsession cookie");
        let auth_cookie = format!("{aname}={avalue}");

        // logoutは200・続く同一cookieでの保護API accessは401(session破棄)
        let (status, body, _) =
            flow.call("POST", "/api/auth/logout", None, Some(&auth_cookie));
        assert_eq!(status, 200, "logoutは200: {body}");
        let (status, body, _) = flow.call("GET", "/api/entities", None, Some(&auth_cookie));
        assert_eq!(status, 401, "logout後の同一cookieは401(session破棄): {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.unauthenticated-protected-api-401]
    /// 未認証での保護endpoint accessは401 unauthenticated(503ではなく。P4の認証gate
    /// がprotected_fail_closedを置換えた後の観測)。
    #[test]
    fn api_unauthenticated_protected_api_is_401() {
        let flow = AuthFlow::new("srv-auth-401");
        let (status, body, _) = flow.call("GET", "/api/entities", None, None);
        assert_eq!(status, 401, "未認証は401(503ではなく): {body}");
        expect_error_body(&body, "unauthenticated");
        assert!(
            body.get("entities").is_none(),
            "保護対象dataは応答に含まれない: {body}"
        );
    }
}
