//! TASK-9 serve基盤+読取API+passkey認証(設計1章D2: 同一binaryのserve subcommand・
//! 6章API・8章server.toml・passkey認証(D3/D9)・9章D4 localhost既定・10章dist二階建て・
//! 12章Phase P2〜P4)。
//!
//! - HTTP層はaxum。mount prefixは`/api/`。P3が読取API(entities/versions/pins/
//!   locations/pending/sync/diff)をrouteし、P4が認証API(register/login start/finish・
//!   logout・status)と保護API開放(session検証gate)を担う
//! - 保護APIのgate(設計12章P4・指摘#11の解消): 認証機構構成後(P4)は
//!   `/api/health`・`/api/auth/*`以外の`/api/*`をsession検証し、未認証は
//!   401(unauthenticated)で応答する(P2〜P3の503 fail-closedからの置換)。
//!   handler単体test(P3系)はgate無効(authを持たないserver state)のrouterをoneshot
//! - passkey認証(設計8章・D3 webauthn-rs 0.6系/D9 single user):
//!   - bootstrap: `serve --bootstrap`がone-time token(128bit乱数のbase64)を生成し
//!     stdout表示。in-memory保持・期限10分・原子的消費(consumed flagのatomic swap
//!     1回限り)・再起動で失効。credential数0かつtoken不一致は登録閉鎖(fail-closed)。
//!     既登録状態での追加登録はlogin済み+tokenの両方が必要
//!   - challenge: in-memory・期限120秒・一度限り消費・発行時のanonymous session
//!     cookieと結合(別browserへのchallenge転用防止)
//!   - session: tower-sessions in-memory。cookie HttpOnly+SameSite=Strict(rp_origin
//!     がhttps時Secure)。login成功時にsession IDを付け替え(session固定化対策)・
//!     期限24時間(非活動で失効)
//!   - credential保存: credentials table(永続class)へPasskeyをserdeで保存
//!     (danger-allow-state-serialisation不使用)
//!   - 期限系契約(token 10分・challenge 120秒)の観測のため時刻注入seam
//!     (AuthState::set_time_offset_ms)を提供(testability)
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
    /// passkey登録用のone-time token(128bit乱数のbase64)を生成しstdoutへ表示する
    /// (設計8章bootstrap。tokenは本人がLoginViewへ入力するための表示であり
    /// in-memory保持・期限10分・原子的消費・再起動で失効)
    #[arg(long)]
    pub bootstrap: bool,
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

    // bootstrap one-time token(設計8章: --bootstrap時に128bit乱数base64をstdoutの
    // 1行目として表示。本人がLoginViewへ入力するための表示であり、stdoutの他出力は
    // このtoken行より後に存在しない=最初の非空行がtoken)
    let bootstrap_token = if args.bootstrap {
        match generate_bootstrap_token() {
            Ok(t) => {
                println!("{t}");
                Some(t)
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        None
    };
    // P4認証状態(server.tomlのrp_id/rp_origin+bootstrap token)
    let auth = match AuthState::new(&cfg, bootstrap_token) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };

    // 起動時sync(設計4章・1章実行境界): P1同期層(manage::catalog_sync_once)を
    // spawn_blockingから呼ぶ。失敗してもserveは継続(staleness表示はP3+のAPIが担う)。
    // single-flight(設計1章): 手動sync(POST /api/sync)等と同じsemaphoreを取得して
    // 実行する=起動直後の並走・遅延した起動時syncによる新しいsnapshotの上書きを防ぐ
    let state = match AppState::from_ctx_args(serve_ctx_args(args)) {
        Ok(s) => s.with_auth(auth),
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    let sync_permits = state.sync_permits.clone();
    let sync_ctx = state.sync_ctx.clone();
    let sync_handle = tokio::task::spawn(async move {
        let permit = sync_permits.acquire_owned().await;
        let result = match permit {
            Ok(_permit) => {
                let ctx = sync_ctx;
                let run = tokio::task::spawn_blocking(move || {
                    crate::storage::manage::catalog_sync_once(&ctx)
                })
                .await;
                match run {
                    Ok(Ok(msg)) => {
                        // 完了はその時点で通知(停止時までlog出力を遅延させない)
                        eprintln!("serve: 起動時catalog sync: {msg}");
                        Ok(msg)
                    }
                    Ok(Err(e)) => {
                        eprintln!(
                            "WARNING: 起動時catalog syncに失敗しました(表示はstale扱い): {e}"
                        );
                        Err(e)
                    }
                    Err(e) => {
                        eprintln!("WARNING: 起動時catalog syncが異常終了しました: {e}");
                        Err(BackupError::Internal(format!(
                            "起動時catalog syncが異常終了: {e}"
                        )))
                    }
                }
            }
            Err(e) => {
                eprintln!("WARNING: 起動時sync semaphoreのacquireに失敗: {e}");
                Err(BackupError::Internal(format!(
                    "起動時sync semaphoreのacquire失敗: {e}"
                )))
            }
        };
        result
    });

    // P4(設計12章): 保護APIは認証gate(session検証)構成済み=未認証401。P2〜P3の
    // protected_fail_closed(503)はこのgateへ置換わった
    let app = build_router(state);
    // 全応答へCache-Controlが未設定ならno-storeを付与(API応答の途中cache禁止。
    // 静的assetsはhandler側が明示的に設定するため上書きされない)
    let app = app.layer(
        tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        ),
    );

    // ConnectInfo<SocketAddr>の挿入(TASK-13設計2.3: api-key認証のD14接続元判定に
    // remote addrを渡す。本番はaxumがこの機構でextensionsへ挿入する=testの
    // Request extension挿入と同一経路)
    let serve = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal());
    if let Err(e) = serve.await {
        eprintln!("error: 内部エラー: serverの実行に失敗: {e}");
        return 1;
    }
    // graceful shutdown(設計1章): 進行中spawn_blocking(起動時sync)の完了を待つ
    // (結果のlog出力はtask側で完了時点に済んでいるためここでは待ち合わせのみ)
    if !sync_handle.is_finished() {
        eprintln!("serve: 起動時syncの完了を待っています(graceful shutdown)");
    }
    let _ = sync_handle.await;
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

/// serveのAPI共有状態(P3+P4)。
///
/// - `sync_permits`: sync・diff・scan・起動時sync実行のsingle-flight(設計1章:
///   DB書込・syncはsemaphore 1で直列化)
/// - `sync_ctx`: POST /api/sync・POST /api/diff・POST /api/pending/scanが呼ぶ
///   同期層(open_session系)への引数。serve起動時の実効profile解決(ServeArgs由来)
///   をそのまま使う
/// - `scope`: serve起動時に解決済みの実効scope(bucket, prefix)。読取APIのscope
///   省略時既定(設計8章: 単一の実効profileを解決しAPI対象scope・復号鍵はそれに
///   従う=更新対象(sync/scan/diff)と表示対象(読取API)が同じscopeになる)
/// - `auth`: P4認証状態。`None`は認証機構未構成のrouter(P3系handler単体testが
///   oneshotするgate無効構成。serve起動では常に`Some`)
#[derive(Clone)]
struct AppState {
    sync_permits: std::sync::Arc<tokio::sync::Semaphore>,
    sync_ctx: std::sync::Arc<crate::storage::CtxArgs>,
    scope: (String, String),
    auth: Option<std::sync::Arc<AuthState>>,
}

impl AppState {
    /// 実行context(起動引数)から構築。実効scopeはCtxArgsの解決結果
    /// (--aws-config/--profile込み)を起動時に一度だけ解決して保持する
    /// (aws.toml読込を伴うため呼出側の同期文脈で使う)。
    fn from_ctx_args(ctx: crate::storage::CtxArgs) -> BackupResult<AppState> {
        let aws = crate::storage::load_aws(&ctx.aws_config, ctx.profile.as_deref())?;
        Ok(AppState {
            sync_permits: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            sync_ctx: std::sync::Arc::new(ctx),
            scope: (aws.bucket, aws.prefix),
            auth: None,
        })
    }

    /// P4認証状態を設定(serve起動・P4 handler test用)。
    fn with_auth(mut self, auth: AuthState) -> AppState {
        self.auth = Some(std::sync::Arc::new(auth));
        self
    }
}

// ---------------------------------------------------------------------------
// passkey認証(設計8章・D3 webauthn-rs 0.6系/D9 single user・12章P4)。
// bootstrap token・challengeはin-memory、credentialはcatalog.db credentials table
// (永続class)へ保存。秘密情報境界(設計2章): token・challenge・session値は
// API応答へ出さない(bootstrap tokenのstdout表示は本人入力用の仕様上の出力)。

/// bootstrap tokenの期限(設計8章指摘#8: 10分)。
const BOOTSTRAP_TOKEN_TTL_SECS: u64 = 600;
/// challengeの期限(設計8章: 120秒)。
const CHALLENGE_TTL_SECS: u64 = 120;
/// 保留challengeの全体上限(start時の期限切れ掃除後の保有数上限。超過時は期限が
/// 最も古いものから破棄する=未認証のstart連打による無制限蓄積を防ぐ)。
const MAX_PENDING_CHALLENGES: usize = 64;

/// bootstrap one-time token(設計8章指摘#8契約: in-memory保持・期限10分・
/// 原子的消費=consumed flagのatomic swap 1回限り・再起動で失効)。
struct BootstrapToken {
    value: String,
    consumed: std::sync::atomic::AtomicBool,
    expires_at: std::time::SystemTime,
}

/// 発行済みchallengeとその保留state(設計8章: in-memory・期限120秒・一度限り消費・
/// 発行時のanonymous session cookie結合=転用防止)。
enum PendingChallenge {
    Registration {
        state: webauthn_rs::prelude::PasskeyRegistration,
        session_id: i128,
        expires_at: std::time::SystemTime,
    },
    Authentication {
        state: webauthn_rs::prelude::PasskeyAuthentication,
        session_id: i128,
        expires_at: std::time::SystemTime,
    },
}

impl PendingChallenge {
    /// 結合されたanonymous session id(回収・同一session制限の判定用)。
    fn bound_session_id(&self) -> i128 {
        match self {
            PendingChallenge::Registration { session_id, .. }
            | PendingChallenge::Authentication { session_id, .. } => *session_id,
        }
    }

    /// 期限切れ時刻(回収・上限破棄の順序判定用)。
    fn expires_at(&self) -> std::time::SystemTime {
        match self {
            PendingChallenge::Registration { expires_at, .. }
            | PendingChallenge::Authentication { expires_at, .. } => *expires_at,
        }
    }
}

/// P4認証のserver状態(WebAuthn instance・bootstrap token・challenge保留・
/// 時刻注入seam)。期限系契約(token 10分・challenge 120秒)の観測は
/// `set_time_offset_ms`(時刻注入seam)経由で行う(testability要求)。
struct AuthState {
    webauthn: webauthn_rs::prelude::Webauthn,
    /// rp_origin(clientDataJSONのorigin検証とcookie Secure属性判定で使用)
    rp_origin: String,
    /// single user(D9)の固定user id(register時のWebAuthn user)
    user_id: webauthn_rs::prelude::Uuid,
    bootstrap: std::sync::Mutex<Option<BootstrapToken>>,
    challenges: std::sync::Mutex<std::collections::HashMap<String, PendingChallenge>>,
    /// 時刻注入seam:testが現在時刻への加算offsetを差し替える(ms単位。
    /// 本番経路は常に0)
    time_offset_ms: std::sync::atomic::AtomicI64,
}

impl AuthState {
    /// server.tomlのrp_id/rp_originから構築。tokenは`serve --bootstrap`由来
    /// (未指定ならNone=登録閉鎖)。
    fn new(cfg: &ServerConfig, token: Option<String>) -> BackupResult<AuthState> {
        use webauthn_rs::prelude::WebauthnBuilder;
        let origin = webauthn_rs::prelude::Url::parse(&cfg.rp_origin)
            .map_err(|e| BackupError::Config(format!("server.tomlのrp_originが不正です: {e}")))?;
        let mut builder = WebauthnBuilder::new(&cfg.rp_id, &origin).map_err(|e| {
            BackupError::Config(format!("WebAuthn構成(rp_id/rp_origin)が不正です: {e}"))
        })?;
        builder = builder.rp_name("amazing-backup");
        let webauthn = builder
            .build()
            .map_err(|e| BackupError::Config(format!("WebAuthn構成の構築に失敗しました: {e}")))?;
        let expires_at =
            std::time::SystemTime::now() + std::time::Duration::from_secs(BOOTSTRAP_TOKEN_TTL_SECS);
        Ok(AuthState {
            webauthn,
            rp_origin: cfg.rp_origin.clone(),
            user_id: webauthn_rs::prelude::Uuid::nil(),
            bootstrap: std::sync::Mutex::new(token.map(|value| BootstrapToken {
                value,
                consumed: std::sync::atomic::AtomicBool::new(false),
                expires_at,
            })),
            challenges: std::sync::Mutex::new(std::collections::HashMap::new()),
            time_offset_ms: std::sync::atomic::AtomicI64::new(0),
        })
    }

    /// seam考慮の現在時刻(本番は実時間。testは`set_time_offset_ms`で進められる)。
    fn now(&self) -> std::time::SystemTime {
        let offset = self
            .time_offset_ms
            .load(std::sync::atomic::Ordering::SeqCst);
        std::time::SystemTime::now() + std::time::Duration::from_millis(offset.max(0) as u64)
    }

    /// 時刻注入seam(testability): 現在時刻への加算offsetを設定(ms単位)。
    /// token期限・challenge期限の観測をreal-time待ち無しで行うための経路。
    #[cfg(test)]
    fn set_time_offset_ms(&self, offset_ms: i64) {
        self.time_offset_ms
            .store(offset_ms, std::sync::atomic::Ordering::SeqCst);
    }

    /// bootstrap tokenの検証と原子的消費(指摘#8: consumed flagのatomic swap)。
    /// 一致・期限内・未消費なら消費してSome。不一致・期限切れ・消費済みはNone
    /// (期限切れ・消費済みは不一致と同じ登録閉鎖=fail-closed)。
    fn consume_bootstrap_token(&self, presented: &str) -> Option<()> {
        let guard = self.bootstrap.lock().unwrap_or_else(|e| e.into_inner());
        let token = guard.as_ref()?;
        if token.value != presented {
            return None;
        }
        if self.now() >= token.expires_at {
            return None;
        }
        if token
            .consumed
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return None;
        }
        Some(())
    }

    /// challengeの発行登録(in-memory・期限120秒・anonymous session結合)。
    /// 未完了challengeの蓄積対策としてstart時に次を回収・制限する:
    /// (1)期限切れchallengeの掃除 (2)同一sessionの保留challengeは最新1件のみ
    /// (再startで前のchallengeを無効化=1session 1pending) (3)全体件数上限
    /// (MAX_PENDING_CHALLENGES。超過時は期限が最も古いものから破棄)。
    fn store_challenge(&self, challenge: String, state: PendingChallenge) {
        let mut guard = self.challenges.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.now();
        // (1)期限切れchallengeの回収(finishされないまま残った分を掃除する)
        guard.retain(|_, pc| pc.expires_at() > now);
        // (2)同一sessionの保留challengeを破棄(今回の発行のみ有効にする)
        let session_id = state.bound_session_id();
        guard.retain(|_, pc| pc.bound_session_id() != session_id);
        // (3)件数上限: 追加後に上限を超える分だけ期限が古い順に破棄
        if guard.len() >= MAX_PENDING_CHALLENGES {
            let mut ordered: Vec<(String, std::time::SystemTime)> = guard
                .iter()
                .map(|(k, pc)| (k.clone(), pc.expires_at()))
                .collect();
            ordered.sort_by_key(|(_, expires_at)| *expires_at);
            let evict = guard.len() + 1 - MAX_PENDING_CHALLENGES;
            for (key, _) in ordered.into_iter().take(evict) {
                guard.remove(&key);
            }
        }
        guard.insert(challenge, state);
    }

    /// challengeの取り出し(一度限り消費: 一致時点でmapから除去。期限切れ・
    /// session不一致も消費済み扱い)。種別が要求と異なる場合もNone。
    fn take_challenge(
        &self,
        challenge: &str,
        session_id: i128,
        want_registration: bool,
    ) -> Option<PendingChallenge> {
        let mut guard = self.challenges.lock().unwrap_or_else(|e| e.into_inner());
        let pending = guard.remove(challenge)?;
        let is_registration = matches!(pending, PendingChallenge::Registration { .. });
        // 期限切れ・session結合不一致・種別不一致は全て拒否(消費済み=除去済み)
        if self.now() >= pending.expires_at()
            || pending.bound_session_id() != session_id
            || want_registration != is_registration
        {
            return None;
        }
        Some(pending)
    }
}

/// bootstrap tokenの生成(128bit乱数のbase64・設計8章。/dev/urandom由来=
/// 共通helper storage::random_bytes)。
fn generate_bootstrap_token() -> BackupResult<String> {
    let buf = crate::storage::random_bytes(16)?;
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.encode(buf))
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

/// 未認証(401・設計6章error体系。token不正・challenge消費済み・session無効の
/// 区分はすべてこれ。登録閉鎖=fail-closedも503/403でなくこの区分)。
fn unauthenticated(message: &'static str) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::UNAUTHORIZED,
        code: "unauthenticated",
        message,
    }
}

/// 受理拒否(403・TASK-13 D14: api-key認証の接続元LAN外。key検証の前に受理自体を
/// 拒否する区分。passkey認証はこの条件を掛けないため現状api-key専用)。
fn forbidden_source(message: &'static str) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::FORBIDDEN,
        code: "forbidden_source",
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

/// 実行scopeの解決(scope明示は<bucket>:<prefix>・省略時はserve起動時に解決済みの
/// 実効profile。設計8章: 読取APIの対象scopeはsync/scan/diffの更新対象と同一の
/// 実効profileに従う=serveの--profile/--aws-config指定が読取APIにも反映される)。
fn resolve_scope(
    explicit: Option<&str>,
    effective: &(String, String),
) -> BackupResult<(String, String)> {
    if let Some(scope) = explicit {
        return parse_scope_param(scope).ok_or_else(|| {
            BackupError::Usage("scopeは <bucket>:<prefix> 形式で指定してください".to_string())
        });
    }
    Ok(effective.clone())
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

/// router組立(設計6章のendpoint一覧のP2+P3+P4分)。`state.auth`がSome(serve起動・
/// P4 flow test)の場合はauth endpoint群と保護APIのsession検証gate(auth_gate)を
/// 組む。None(P3系handler単体test)は認証機構未構成のrouter(読取APIを直接oneshot)。
fn build_router(state: AppState) -> axum::Router {
    let has_auth = state.auth.is_some();
    let mut router = axum::Router::new()
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
        .route("/api/pending/scan", axum::routing::post(post_pending_scan))
        .route(
            "/api/sync",
            axum::routing::get(get_sync_state).post(post_sync_refresh),
        )
        .route(
            "/api/diff",
            axum::routing::get(get_diff_last).post(post_diff_run),
        )
        .fallback(fallback);
    if has_auth {
        router = router
            .route("/api/auth/status", axum::routing::get(auth_status))
            .route(
                "/api/auth/register/start",
                axum::routing::post(auth_register_start),
            )
            .route(
                "/api/auth/register/finish",
                axum::routing::post(auth_register_finish),
            )
            .route(
                "/api/auth/login/start",
                axum::routing::post(auth_login_start),
            )
            .route(
                "/api/auth/login/finish",
                axum::routing::post(auth_login_finish),
            )
            .route("/api/auth/api-key", axum::routing::post(auth_api_key))
            .route("/api/auth/logout", axum::routing::post(auth_logout))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                auth_gate,
            ));
    }
    // session管理(設計8章: tower-sessions in-memory。cookie HttpOnly+SameSite=Strict・
    // rp_originがhttpsならSecure・期限24時間=非活動で失効)。layer追加順で最外側に
    // するため最後に積む(gate/handlerより先に通る=Session extractorが利用可能)
    let secure = state
        .auth
        .as_ref()
        .is_some_and(|a| a.rp_origin.starts_with("https://"));
    let session_layer =
        tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default())
            .with_name("id")
            .with_path("/")
            .with_http_only(true)
            .with_same_site(tower_sessions::cookie::SameSite::Strict)
            .with_secure(secure)
            .with_expiry(tower_sessions::Expiry::OnInactivity(
                time::Duration::seconds(24 * 60 * 60),
            ));
    router.layer(session_layer).with_state(state)
}

/// GET /api/health(認証不要のliveness・設計6章)。
async fn health() -> impl axum::response::IntoResponse {
    axum::Json(serde_json::json!({"status": "ok"}))
}

/// 保護APIの認証gate(設計12章P4: 未認証401 unauthenticated。P2〜P3の
/// protected_fail_closed(503)を置換)。`/api/health`・`/api/auth/*`と非API path
/// (静的配信)は認証不要。state.authがNone(認証機構未構成)の場合は素通り
/// (P3系handler単体testの観測経路)。
async fn auth_gate(
    axum::extract::State(state): axum::extract::State<AppState>,
    session: tower_sessions::Session,
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path();
    let needs_auth = (path == "/api" || path.starts_with("/api/"))
        && path != "/api/health"
        && !path.starts_with("/api/auth/");
    if !needs_auth || state.auth.is_none() {
        return next.run(req).await;
    }
    if session_is_authenticated(&session).await {
        return next.run(req).await;
    }
    (
        axum::http::StatusCode::UNAUTHORIZED,
        axum::Json(error_body("unauthenticated", "認証が必要です")),
    )
        .into_response()
}

/// GET /api/entities(設計6章: entity一覧。entry_summary_json互換schemaで
/// {"entities":[...],"page":n,"total_pages":n}形式・page=1始点)。
async fn get_entities(
    axum::extract::State(state): axum::extract::State<AppState>,
    uri: axum::http::Uri,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let params = parse_query_params(uri.query());
    let page = parse_page_param(params.get("page").map(|s| s.as_str()))?;
    let scope = params.get("scope").cloned();
    let query = params.get("query").cloned();
    let category = params.get("category").cloned();
    let label = params.get("label").cloned();
    let effective = state.scope.clone();
    let entries = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(scope.as_deref(), &effective)?;
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
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
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
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
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
async fn get_pins(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
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
async fn get_locations(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
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
/// last_scan_at)で{"pending":[...]}形式。?status=<unbacked|changed|ok|unknown>で
/// 単一statusへ絞り込み(省略時全件)。不正値は400 invalid_request)。
async fn get_pending(
    axum::extract::State(state): axum::extract::State<AppState>,
    uri: axum::http::Uri,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let params = parse_query_params(uri.query());
    let status = match params.get("status") {
        None => None,
        Some(v) => match v.as_str() {
            "unbacked" | "changed" | "ok" | "unknown" => Some(v.clone()),
            _ => {
                return Err(invalid_request(
                    "statusは unbacked/changed/ok/unknown のいずれかで指定してください",
                ));
            }
        },
    };
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let db = crate::catalog::CatalogDb::open()?;
        let items = db.pending_items(&bucket, &prefix)?;
        let items: Vec<_> = match &status {
            Some(want) => items.into_iter().filter(|i| &i.status == want).collect(),
            None => items,
        };
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

/// POST /api/pending/scan(scan実行=manage catalog scanと同一のP5同期層。
/// request body無し・sha256再計算optionはCLIのみのためsize+mtime比較固定。
/// 応答はscan実行後の最新一覧をGET /api/pendingと同一schema({"pending":[...]})で
/// 返す。scan対象未登録でも200で0件。DB書込を伴うためsyncと同一のsingle-flightで
/// 直列化しspawn_blocking経由で実行=設計1章実行境界)。
async fn post_pending_scan(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let _permit = state
        .sync_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| {
            eprintln!("serve: scan semaphoreのacquireに失敗(logのみ): {e}");
            internal("内部エラーが発生しました")
        })?;
    let ctx = state.sync_ctx.clone();
    let value = run_blocking(move || {
        let outcome = crate::storage::manage::catalog_scan_once(&ctx, false)?;
        Ok(serde_json::json!({
            "pending": outcome.items.iter().map(|i| serde_json::json!({
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
async fn get_sync_state(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
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
async fn get_diff_last(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let db = crate::catalog::CatalogDb::open()?;
        db.load_diff_result(&bucket, &prefix)?
            .ok_or_else(|| BackupError::Target("diff結果は未実行です".to_string()))
    })
    .await?;
    Ok(axum::Json(value))
}

// ---------------------------------------------------------------------------
// passkey認証API(設計6章endpoint一覧のauth系・8章認証契約・12章P4)。

/// sessionの認証済み判定(login成功時に"authenticated"=trueを格納)。
async fn session_is_authenticated(session: &tower_sessions::Session) -> bool {
    matches!(session.get::<bool>("authenticated").await, Ok(Some(true)))
}

/// challenge発行時のanonymous session確立(設計8章: challengeは発行時のanonymous
/// session cookieと結合)。tower-sessionsはcookie無しrequestではsession idが未採番
/// (save時に採番)のため、start系handlerで明示的にsaveしてidを確定させる
/// (空sessionは応答cookieが発行されないためflow種別を格納=empty回避を兼ねる)。
async fn establish_session_id(
    session: &tower_sessions::Session,
    flow: &str,
) -> Result<i128, ApiError> {
    session.insert("flow", flow).await.map_err(|e| {
        eprintln!("serve: session状態の保存に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    session.save().await.map_err(|e| {
        eprintln!("serve: sessionの保存に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    session
        .id()
        .map(|id| id.0)
        .ok_or_else(|| unauthenticated("認証が必要です"))
}

/// clientDataJSON bytesからchallenge文字列を抽出(finish系のchallenge lookup用)。
fn client_data_challenge(client_data_json: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(client_data_json).ok()?;
    value
        .get("challenge")
        .and_then(|c| c.as_str())
        .map(String::from)
}

/// 応答JSONからchallenge文字列を抽出(start系応答の保存key用)。
fn challenge_from_options(options: &serde_json::Value) -> Option<String> {
    options
        .pointer("/publicKey/challenge")
        .and_then(|c| c.as_str())
        .map(String::from)
}

/// GET /api/auth/status(認証不要: 認証状態と登録credential数・api-key数。未認証・
/// 未登録でも200=LoginViewのUI誘導基盤。api_keysはTASK-13追加・credentialsは
/// passkey件数のまま)。
async fn auth_status(
    session: tower_sessions::Session,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let authenticated = session_is_authenticated(&session).await;
    let (credentials, api_keys) = run_blocking(|| {
        let db = crate::catalog::CatalogDb::open()?;
        Ok((db.credential_count()?, db.api_key_count()?))
    })
    .await?;
    Ok(axum::Json(serde_json::json!({
        "authenticated": authenticated,
        "credentials": credentials,
        "api_keys": api_keys,
    })))
}

/// POST /api/auth/register/start(bootstrap token必須。credential数0かつtoken不一致は
/// 登録閉鎖=fail-closed。既登録状態での追加登録はlogin済み+tokenの両方が必要)。
/// 応答はWebAuthn登録options(PublicKeyCredentialCreationOptions相当)+anonymous
/// session cookie(challengeは発行時のsessionと結合)。
async fn auth_register_start(
    axum::extract::State(state): axum::extract::State<AppState>,
    session: tower_sessions::Session,
    body: axum::body::Bytes,
) -> Result<axum::response::Response, ApiError> {
    let Some(auth) = state.auth.clone() else {
        return Err(configuration("認証機構が構成されていません"));
    };
    // bodyは{"token": "<bootstrap token>"}(tokenの検証はstart時点)
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    let token = parsed
        .get("token")
        .and_then(|t| t.as_str())
        .ok_or_else(|| unauthenticated("認証が必要です"))?;
    // 追加登録条件(設計8章指摘#8): 既登録状態ではlogin済みsessionが必須
    // (single user=D9での乗っ取り登録防止。未loginならtoken有効でも閉鎖)
    let count = run_blocking(|| {
        let db = crate::catalog::CatalogDb::open()?;
        db.credential_count()
    })
    .await?;
    if count > 0 && !session_is_authenticated(&session).await {
        return Err(unauthenticated("認証が必要です"));
    }
    // token検証+原子的消費(不一致・期限切れ・消費済みは登録閉鎖=fail-closed)
    auth.consume_bootstrap_token(token)
        .ok_or_else(|| unauthenticated("認証が必要です"))?;
    let (ccr, reg_state) = auth
        .webauthn
        .start_passkey_registration(auth.user_id, "amazing-backup", "amazing-backup", None)
        .map_err(|e| {
            eprintln!("serve: 登録challengeの発行に失敗(logのみ): {e}");
            internal("内部エラーが発生しました")
        })?;
    let options = serde_json::to_value(&ccr).map_err(|e| {
        eprintln!("serve: 登録optionsの組立に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    let challenge =
        challenge_from_options(&options).ok_or_else(|| internal("内部エラーが発生しました"))?;
    let session_id = establish_session_id(&session, "register").await?;
    auth.store_challenge(
        challenge,
        PendingChallenge::Registration {
            state: reg_state,
            session_id,
            expires_at: auth.now() + std::time::Duration::from_secs(CHALLENGE_TTL_SECS),
        },
    );
    Ok(axum::Json(options).into_response())
}

/// POST /api/auth/register/finish(登録応答の検証+credentialの永続保存)。
/// challengeは一度限り消費・発行時のanonymous session cookieと結合。
async fn auth_register_finish(
    axum::extract::State(state): axum::extract::State<AppState>,
    session: tower_sessions::Session,
    body: axum::body::Bytes,
) -> Result<axum::response::Response, ApiError> {
    use webauthn_rs::prelude::RegisterPublicKeyCredential;
    let Some(auth) = state.auth.clone() else {
        return Err(configuration("認証機構が構成されていません"));
    };
    let reg: RegisterPublicKeyCredential =
        serde_json::from_slice(&body).map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    let challenge = client_data_challenge(&reg.response.client_data_json)
        .ok_or_else(|| unauthenticated("認証が必要です"))?;
    let session_id = session
        .id()
        .map(|id| id.0)
        .ok_or_else(|| unauthenticated("認証が必要です"))?;
    let pending = auth
        .take_challenge(&challenge, session_id, true)
        .ok_or_else(|| unauthenticated("認証が必要です"))?;
    let PendingChallenge::Registration {
        state: reg_state, ..
    } = pending
    else {
        return Err(unauthenticated("認証が必要です"));
    };
    let passkey = auth
        .webauthn
        .finish_passkey_registration(&reg, &reg_state)
        .map_err(|e| {
            eprintln!("serve: passkey登録の検証に失敗(logのみ): {e}");
            unauthenticated("認証に失敗しました")
        })?;
    // 永続保存(設計8章: credentials table=永続classへserde保存)
    let passkey_json = serde_json::to_string(&passkey).map_err(|e| {
        eprintln!("serve: passkey保存値の組立に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    use base64::Engine as _;
    let credential_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(passkey.cred_id());
    run_blocking(move || {
        let db = crate::catalog::CatalogDb::open()?;
        db.upsert_credential(&credential_id, &passkey_json, "passkey")
    })
    .await?;
    Ok(axum::Json(serde_json::json!({"status": "ok"})).into_response())
}

/// POST /api/auth/login/start(challenge発行+anonymous session cookie。
/// 登録credentialからallowCredentialsを組む)。
async fn auth_login_start(
    axum::extract::State(state): axum::extract::State<AppState>,
    session: tower_sessions::Session,
) -> Result<axum::response::Response, ApiError> {
    use webauthn_rs::prelude::Passkey;
    let Some(auth) = state.auth.clone() else {
        return Err(configuration("認証機構が構成されていません"));
    };
    let rows = run_blocking(|| {
        let db = crate::catalog::CatalogDb::open()?;
        db.load_credentials()
    })
    .await?;
    let passkeys: Vec<Passkey> = rows
        .iter()
        .filter_map(|row| match serde_json::from_str(&row.passkey_json) {
            Ok(pk) => Some(pk),
            Err(e) => {
                eprintln!(
                    "serve: credentials行の読み飛ばし(logのみ): credential_id={} {e}",
                    row.credential_id
                );
                None
            }
        })
        .collect();
    if passkeys.is_empty() {
        // 登録済みcredentialが無い(未登録状態ではloginは成立しない)
        return Err(unauthenticated("認証が必要です"));
    }
    let (rcr, ast_state) = auth
        .webauthn
        .start_passkey_authentication(&passkeys)
        .map_err(|e| {
            eprintln!("serve: login challengeの発行に失敗(logのみ): {e}");
            internal("内部エラーが発生しました")
        })?;
    let options = serde_json::to_value(&rcr).map_err(|e| {
        eprintln!("serve: login optionsの組立に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    let challenge =
        challenge_from_options(&options).ok_or_else(|| internal("内部エラーが発生しました"))?;
    let session_id = establish_session_id(&session, "login").await?;
    auth.store_challenge(
        challenge,
        PendingChallenge::Authentication {
            state: ast_state,
            session_id,
            expires_at: auth.now() + std::time::Duration::from_secs(CHALLENGE_TTL_SECS),
        },
    );
    Ok(axum::Json(options).into_response())
}

/// POST /api/auth/login/finish(認証応答の検証+session ID付替+認証済みsession確立)。
async fn auth_login_finish(
    axum::extract::State(state): axum::extract::State<AppState>,
    session: tower_sessions::Session,
    body: axum::body::Bytes,
) -> Result<axum::response::Response, ApiError> {
    use webauthn_rs::prelude::PublicKeyCredential;
    let Some(auth) = state.auth.clone() else {
        return Err(configuration("認証機構が構成されていません"));
    };
    let cred: PublicKeyCredential =
        serde_json::from_slice(&body).map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    let challenge = client_data_challenge(&cred.response.client_data_json)
        .ok_or_else(|| unauthenticated("認証が必要です"))?;
    let session_id = session
        .id()
        .map(|id| id.0)
        .ok_or_else(|| unauthenticated("認証が必要です"))?;
    let pending = auth
        .take_challenge(&challenge, session_id, false)
        .ok_or_else(|| unauthenticated("認証が必要です"))?;
    let PendingChallenge::Authentication {
        state: ast_state, ..
    } = pending
    else {
        return Err(unauthenticated("認証が必要です"));
    };
    auth.webauthn
        .finish_passkey_authentication(&cred, &ast_state)
        .map_err(|e| {
            eprintln!("serve: passkey認証の検証に失敗(logのみ): {e}");
            unauthenticated("認証に失敗しました")
        })?;
    // session ID付替(設計8章: session固定化対策=login成功時に付け替える)
    session.cycle_id().await.map_err(|e| {
        eprintln!("serve: session IDの付替に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    session.insert("authenticated", true).await.map_err(|e| {
        eprintln!("serve: session状態の保存に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    Ok(axum::Json(serde_json::json!({"status": "ok"})).into_response())
}

/// remote addrのIPがLAN内か(TASK-13設計D14のLAN内定義)。
/// IPv4: loopback(127/8)・private(RFC1918: 10/8・172.16/12・192.168/16)・
/// link-local(169.254/16)。IPv6: loopback(::1)・ULA(fc00::/7)・link-local(fe80::/10)。
/// v4-mapped(`::ffff:a.b.c.d`)はto_ipv4_mappedでIPv4へ正規化してから判定
/// (dual-stack listen `[::]`構成でv4接続がこの形で現れるため)。
fn is_lan_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_lan_ip(std::net::IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback()
                || (first & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// POST /api/auth/api-key(TASK-13 D13/D14: api-key認証。接続元LAN内限定)。
/// 1. ConnectInfoのremote addrでLAN内判定(v4-mapped正規化込み)。LAN外はkey検証の
///    前に403 forbidden_sourceで受理拒否
/// 2. SHA-256(api_key)のhexでapi_keysを検索。不在は401 unauthenticated(passkey
///    login finish検証失敗と同一・key存在特定防止の一般化message)
/// 3. hit時はpasskey login finishと同一手順でsession確立(cycle_id→authenticated
///    insert→establish_session_id("api_key")。以降はpasskeyと同一のsession経路)
async fn auth_api_key(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    session: tower_sessions::Session,
    body: axum::body::Bytes,
) -> Result<axum::response::Response, ApiError> {
    if !is_lan_ip(addr.ip()) {
        return Err(forbidden_source("api-key認証はLAN内接続のみ有効です"));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    let api_key = parsed
        .get("api_key")
        .and_then(|k| k.as_str())
        .ok_or_else(|| unauthenticated("認証に失敗しました"))?;
    let key_hash = crate::storage::sha256_hex(api_key.as_bytes());
    let hit = run_blocking(move || {
        let db = crate::catalog::CatalogDb::open()?;
        db.find_api_key_by_hash(&key_hash)
    })
    .await?;
    if hit.is_none() {
        return Err(unauthenticated("認証に失敗しました"));
    }
    // session ID付替(設計2.3: passkey login finishと同一手順=session固定化対策)
    session.cycle_id().await.map_err(|e| {
        eprintln!("serve: session IDの付替に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    session.insert("authenticated", true).await.map_err(|e| {
        eprintln!("serve: session状態の保存に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    establish_session_id(&session, "api_key").await?;
    Ok(axum::Json(serde_json::json!({"status": "ok"})).into_response())
}

/// POST /api/auth/logout(要認証: session破棄。未認証は401)。
async fn auth_logout(
    session: tower_sessions::Session,
) -> Result<axum::response::Response, ApiError> {
    if !session_is_authenticated(&session).await {
        return Err(unauthenticated("認証が必要です"));
    }
    session.flush().await.map_err(|e| {
        eprintln!("serve: session破棄に失敗(logのみ): {e}");
        internal("内部エラーが発生しました")
    })?;
    Ok(axum::Json(serde_json::json!({"status": "ok"})).into_response())
}

/// routerのfallback: /api/*の未定義endpointは404 not_found(P4後は未認証の保護APIは
/// auth_gateが401で応答するため、ここへ届く/api/*は認証済みでの未定義path)。
/// それ以外は静的配信。
async fn fallback(uri: axum::http::Uri) -> axum::response::Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(error_body("not_found", "対象のAPI endpointがありません")),
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
        .expect("test AppState構築(aws.tomlはapi_envが用意)")
    }

    /// build_routerをoneshotで呼び(status, JSON body)を返す。bodyがJSONで無い場合は
    /// Null(応答形式の検査でfailする)。P3実装でbuild_routerがAppStateを受け取る
    /// signatureへ変わったため、この呼び出しのみ追随(期待値は不変)。
    fn api(method: &str, uri: &str) -> (u16, serde_json::Value) {
        api_with_state(method, uri, api_test_state())
    }

    /// api()のstate差し替え版(serveの--profile指定相当のAppStateを渡す検証用)。
    fn api_with_state(method: &str, uri: &str, state: AppState) -> (u16, serde_json::Value) {
        let router = build_router(state);
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

    // -----------------------------------------------------------------------
    // P5(pending catalog)のhandler単体test(テスト先行。実装前のためPOST
    // /api/pending/scan未routingのfallback 404・?status=無視の200で実行failを
    // 許容=赤。compileは通る)。scan対象のpipeline.toml [profiles.dev.catalog.scan]
    // の追記は現行schemaが未知sectionを拒否するため、このtest専用のenvに対してのみ
    // 行う(他testのpipeline.tomlには影響させない)。

    /// P5 scan系handler test用の環境(api_env+scan対象dir+pipeline.tomlへ
    /// [profiles.dev.catalog.scan]追記)。比較基準となるbackup記録は置かない
    /// (scan対象fileは全件unbackedに分類される構成)。
    fn api_env_with_scan(tag: &str, files: &[&str]) -> ApiEnv {
        use crate::testutil::write_file;
        let env = api_env(tag);
        let scan_root = env.home.parent().expect("tmp dir").join("scanroot");
        std::fs::create_dir_all(&scan_root).expect("scan root dir");
        for name in files {
            write_file(&scan_root.join(name), b"scan-target-bytes");
        }
        let body = std::fs::read_to_string(env.home.join("pipeline.toml")).expect("pipeline.toml");
        let body = format!(
            "{body}\n[profiles.dev.catalog.scan]\nroots = [\"{}\"]\n",
            scan_root.display()
        );
        write_file(&env.home.join("pipeline.toml"), body.as_bytes());
        env
    }

    /// [covers:server.pending-scan-endpoint]
    /// POST /api/pending/scanはscanを実行し最新一覧をGET /api/pendingと同一schemaで
    /// 応答する(itemはsource_root/path/size/mtime/status/last_scan_at入り)。
    #[test]
    fn api_pending_scan_endpoint_runs_scan_and_lists_items() {
        let env = api_env_with_scan("srv-api-pending-scan", &["x.zip", "y.zip"]);
        let (status, body) = api("POST", "/api/pending/scan");
        assert_eq!(status, 200, "POST /api/pending/scanは200: {body}");
        let pending = body
            .get("pending")
            .and_then(|v| v.as_array())
            .expect("pending配列(GET /api/pendingと同一schema)");
        assert_eq!(pending.len(), 2, "scan対象2file: {body}");
        let item = &pending[0];
        for key in [
            "source_root",
            "path",
            "size",
            "mtime",
            "status",
            "last_scan_at",
        ] {
            assert!(
                item.get(key).is_some(),
                "itemに{key}を含む(P3条件と同一schema): {body}"
            );
        }
        assert_eq!(
            item.get("status").and_then(|s| s.as_str()),
            Some("unbacked"),
            "比較基準となるbackup記録が無いため全件unbacked: {body}"
        );
        assert!(
            item.get("last_scan_at")
                .and_then(|v| v.as_str())
                .is_some_and(|v| !v.is_empty()),
            "last_scan_atは直近scan時刻(非空): {body}"
        );
        // 実行結果はDBへ反映され、続くGETにも現れる
        let (status, body) = api("GET", "/api/pending");
        assert_eq!(status, 200, "scan後のGET /api/pendingは200: {body}");
        let pending = body
            .get("pending")
            .and_then(|v| v.as_array())
            .expect("pending配列");
        assert_eq!(pending.len(), 2, "scan実行後の一覧: {body}");
        drop(env);
    }

    /// [covers:server.pending-list-status-filter]
    /// GET /api/pendingの?status=絞り込み(単一値)と不正値400。
    #[test]
    fn api_pending_list_filters_by_status_param() {
        let env = api_env_with_scan("srv-api-pending-filter", &["x.zip"]);
        let (status, body) = api("POST", "/api/pending/scan");
        assert_eq!(status, 200, "scan(事前): {body}");

        let (status, body) = api("GET", "/api/pending?status=unbacked");
        assert_eq!(status, 200, "?status=unbackedは200: {body}");
        let pending = body
            .get("pending")
            .and_then(|v| v.as_array())
            .expect("pending配列");
        assert_eq!(pending.len(), 1, "unbackedのみ: {body}");

        let (status, body) = api("GET", "/api/pending?status=ok");
        assert_eq!(status, 200, "?status=okは200: {body}");
        let pending = body
            .get("pending")
            .and_then(|v| v.as_array())
            .expect("pending配列");
        assert!(pending.is_empty(), "ok項目は0件: {body}");

        let (status, body) = api("GET", "/api/pending?status=bogus");
        assert_eq!(status, 400, "不正なstatus値は400 invalid_request: {body}");
        expect_error_body(&body, "invalid_request");
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

    /// [covers:server.read-api-uses-serve-profile]
    /// 読取APIのscope省略時既定はserve起動時の実効profile解決(--profile/--aws-config)
    /// に従う(更新対象のsync/diffと表示対象の読取APIが同じscopeになる=レビュー指摘#2。
    /// 設計8章「単一の実効profileを解決しAPI対象scope・復号鍵はそれに従う」)。
    #[test]
    fn api_read_api_uses_serve_profile_scope() {
        use crate::testutil::write_file;
        let env = api_env("srv-api-profile");
        // aws.tomlへstg profile(bucket=stg-bucket・prefixはdevと同一)を追加
        // (active_profileはdevのまま=--profile stg指定での上書き解決を観測する)
        write_file(
            &env.home.join("aws.toml"),
            format!(
                "[aws]\nactive_profile = \"dev\"\n\
                 [aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\
                 [aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"{API_TEST_BUCKET}\"\n\
                 region = \"ap-northeast-1\"\nprefix = \"{API_TEST_PREFIX}\"\n\
                 storage_class = \"STANDARD\"\ncredentials_file = \"{}\"\n\
                 [aws.profiles.stg]\npurpose = \"stg\"\nbucket = \"stg-bucket\"\n\
                 region = \"ap-northeast-1\"\nprefix = \"{API_TEST_PREFIX}\"\n\
                 storage_class = \"STANDARD\"\ncredentials_file = \"{}\"\n",
                env.home.join("aws-credentials.toml").display(),
                env.home.join("aws-credentials.toml").display(),
            )
            .as_bytes(),
        );
        // pipeline.tomlへstg profileを追加(recipients+verifyのみの最小構成)
        let pubkey = crate::testutil::write_test_ed25519_pub(env.home.parent().expect("tmp dir"));
        let secret = env.home.parent().expect("tmp dir").join("secret.key");
        write_file(
            &env.home.join("pipeline.toml"),
            format!(
                "[profiles.dev.recipients]\npublic_keys = [\"{}\"]\n\
                 [profiles.dev.verify]\ndecrypt_key = \"{}\"\n\
                 [profiles.stg.recipients]\npublic_keys = [\"{}\"]\n\
                 [profiles.stg.verify]\ndecrypt_key = \"{}\"\n",
                pubkey.display(),
                secret.display(),
                pubkey.display(),
                secret.display(),
            )
            .as_bytes(),
        );
        // 両scopeへseed(dev=alpha・stg=stg-alpha)
        let alpha = api_entry(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
        );
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);
        let stg_alpha = api_entry(
            "44444444-4444-4444-4444-444444444444",
            "stg-alpha",
            "docs",
            "work",
        );
        api_seed(&[stg_alpha], "stg-bucket", API_TEST_PREFIX);

        // profile指定なし(既定=dev)の読取はdev scopeのみ
        let (status, body) = api("GET", "/api/entities");
        assert_eq!(status, 200, "dev既定のGET /api/entities: {body}");
        let titles: Vec<&str> = body
            .get("entities")
            .and_then(|e| e.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.get("title").and_then(|t| t.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(titles, vec!["alpha"], "dev scopeのみ: {body}");

        // serve --profile stg相当のstate: 読取APIもstg scopeに従う
        let state = AppState::from_ctx_args(crate::storage::CtxArgs {
            config: None,
            aws_config: None,
            key: None,
            profile: Some("stg".to_string()),
            format: crate::storage::Format::Human,
        })
        .expect("stg profileでAppState構築");
        assert_eq!(state.scope.0, "stg-bucket", "実効scopeはstg解決");
        let (status, body) = api_with_state("GET", "/api/entities", state);
        assert_eq!(status, 200, "stg profileのGET /api/entities: {body}");
        let titles: Vec<&str> = body
            .get("entities")
            .and_then(|e| e.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|e| e.get("title").and_then(|t| t.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            titles,
            vec!["stg-alpha"],
            "読取APIは実効profileのscope: {body}"
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

    /// P4 flow test用のAppState(P4実装のtoken注入seam: bootstrap tokenを注入して
    /// 認証機構構成済みrouterを組む。rp_id/rp_originはserver.toml無しの既定値
    /// =localhost/http://localhost:40510。soft passkey helperもこの既定値に接続)。
    fn api_auth_test_state(token: Option<&str>) -> AppState {
        let auth = AuthState::new(&default_server_config(), token.map(str::to_string))
            .expect("test用AuthState構築");
        AppState::from_ctx_args(crate::storage::CtxArgs {
            config: None,
            aws_config: None,
            key: None,
            profile: None,
            format: crate::storage::Format::Human,
        })
        .expect("test AppState構築(aws.tomlはapi_envが用意)")
        .with_auth(auth)
    }

    /// P4認証flow観測用環境(api_env+同一routerを使い回す呼び出し経路)。
    /// bootstrap token・challenge・sessionはrouterのserver stateに保持される
    /// (callのたびにstateを作り直さない=single-use消費・session継続を観測できる)。
    struct AuthFlow {
        /// api_envの生存期間をflowと同じにする(env書換えの復元をtest末尾へ)
        #[allow(dead_code)]
        env: ApiEnv,
        router: axum::Router,
        /// 時刻注入seam(advance_time)の到達先。restartで差し替わる
        auth: std::sync::Arc<AuthState>,
    }

    impl AuthFlow {
        fn new(tag: &str) -> AuthFlow {
            // api_env(ABH設定)を先に実行する: AppState構築は実効scope解決のため
            // aws.tomlを読む(ABH配下のconfigがapi_envの適用後に見える)
            // bootstrap token注入seam(AUTH_TEST_TOKEN)=--bootstrap起動相当
            let env = api_env(tag);
            let state = api_auth_test_state(Some(AUTH_TEST_TOKEN));
            let auth = state.auth.clone().expect("auth構成済み");
            AuthFlow {
                env,
                router: build_router(state),
                auth,
            }
        }

        /// server stateを作り直す(serve再起動相当。token・challenge等のin-memory
        /// 状態は消える=webauthn.bootstrap-token-restart-invalidationの観測)。
        /// 再起動直後は--bootstrap未実行のためtokenは無い(旧tokenは無効)。
        fn restart(&mut self) {
            let state = api_auth_test_state(None);
            self.auth = state.auth.clone().expect("auth構成済み");
            self.router = build_router(state);
        }

        /// 時刻注入seam(testability): server stateの現在時刻へ加算offsetを設定
        /// (token期限10分・challenge期限120秒の観測を実時間待ち無しで行う)。
        fn advance_time(&self, offset_ms: i64) {
            self.auth.set_time_offset_ms(offset_ms);
        }

        /// oneshot呼び出し(JSON body+Cookie header付き)。応答は
        /// (status, JSON body, Set-Cookie一覧)。bodyがJSONで無い場合はNull。
        /// remote addrは既定=127.0.0.1(LAN内=D14のLAN内判定を通るaddr)。
        fn call(
            &self,
            method: &str,
            uri: &str,
            body: Option<serde_json::Value>,
            cookie: Option<&str>,
        ) -> (u16, serde_json::Value, Vec<String>) {
            self.call_from(
                method,
                uri,
                body,
                cookie,
                "127.0.0.1:41000".parse().expect("LAN内既定remote addr"),
            )
        }

        /// oneshot呼び出し(remote addr明示=D14 LAN内判定の観測seam)。
        /// Request extensionへConnectInfo(addr)を挿入する=本番の
        /// into_make_service_with_connect_infoが挿入するものと同一のextensions機構
        /// (TASK-13設計4章レビューH-1)。
        fn call_from(
            &self,
            method: &str,
            uri: &str,
            body: Option<serde_json::Value>,
            cookie: Option<&str>,
            remote_addr: std::net::SocketAddr,
        ) -> (u16, serde_json::Value, Vec<String>) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("oneshot runtime");
            rt.block_on(async {
                use tower::ServiceExt as _;
                let mut builder = axum::http::Request::builder()
                    .method(axum::http::Method::from_bytes(method.as_bytes()).expect("method"))
                    .uri(uri)
                    .extension(axum::extract::ConnectInfo(remote_addr));
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
                let resp = self.router.clone().oneshot(req).await.expect("oneshot");
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
                let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
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

    /// soft passkeyの秘密鍵(固定32byteの合成値。0 < key < P-256のorder。
    /// registerとloginで同一鍵を使うため固定=決定的)。
    const SOFT_PASSKEY_SECRET: [u8; 32] = [
        0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
        0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e,
        0x3f, 0x40,
    ];
    /// soft passkeyのRP(rp_id/rp_origin=server.toml無しの既定値と同一)。
    const SOFT_RP_ID: &str = DEFAULT_RP_ID;
    const SOFT_RP_ORIGIN: &str = "http://localhost:40510";

    /// soft passkeyの署名鍵(P-256 ECDSA)。
    fn soft_signing_key() -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_slice(&SOFT_PASSKEY_SECRET).expect("soft秘密鍵")
    }

    /// soft passkeyの公開鍵座標(x, y)。
    fn soft_public_xy() -> (Vec<u8>, Vec<u8>) {
        let encoded = soft_signing_key().verifying_key().to_encoded_point(false);
        let bytes = encoded.as_bytes();
        (bytes[1..33].to_vec(), bytes[33..65].to_vec())
    }

    /// base64url符号化(URL_SAFE_NO_PAD=webauthn-rsのBase64UrlSafeData serde形式)。
    fn soft_b64url(data: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    }

    /// base64url復号(soft鍵・credential idのbyte列化)。
    fn soft_b64url_decode(s: &str) -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .expect("base64url復号")
    }

    /// rp_id_hash(SHA-256(rp_id))。
    fn soft_rp_id_hash() -> [u8; 32] {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(SOFT_RP_ID.as_bytes());
        hasher.finalize().into()
    }

    /// COSE EC2 P-256 ES256公開鍵のCBOR(1:kty=EC2・3:alg=ES256・-1:crv=P-256・
    /// -2:x・-3:y。webauthn-rs-coreのCOSEKey parse形式と同一)。
    fn soft_cose_key_cbor() -> Vec<u8> {
        let (x, y) = soft_public_xy();
        let value = coset::cbor::Value::Map(vec![
            (
                coset::cbor::Value::Integer(1.into()),
                coset::cbor::Value::Integer(2.into()),
            ),
            (
                coset::cbor::Value::Integer(3.into()),
                coset::cbor::Value::Integer((-7).into()),
            ),
            (
                coset::cbor::Value::Integer((-1).into()),
                coset::cbor::Value::Integer(1.into()),
            ),
            (
                coset::cbor::Value::Integer((-2).into()),
                coset::cbor::Value::Bytes(x),
            ),
            (
                coset::cbor::Value::Integer((-3).into()),
                coset::cbor::Value::Bytes(y),
            ),
        ]);
        let mut buf = Vec::new();
        coset::cbor::ser::into_writer(&value, &mut buf).expect("COSE鍵CBOR組立");
        buf
    }

    /// 合成attestationObject(fmt="none"+authData+COSE鍵)。
    fn soft_attestation_object() -> Vec<u8> {
        let mut auth_data = Vec::new();
        auth_data.extend_from_slice(&soft_rp_id_hash());
        auth_data.push(0x45); // flags: UP|UV|AT(attested credential data含有)
        auth_data.extend_from_slice(&0u32.to_be_bytes()); // signCount
        auth_data.extend_from_slice(&[0u8; 16]); // aaguid
        let cred_id = soft_b64url_decode(SOFT_PASSKEY_CREDENTIAL_ID);
        auth_data.extend_from_slice(&(cred_id.len() as u16).to_be_bytes());
        auth_data.extend_from_slice(&cred_id);
        auth_data.extend_from_slice(&soft_cose_key_cbor());
        let value = coset::cbor::Value::Map(vec![
            (
                coset::cbor::Value::Text("fmt".into()),
                coset::cbor::Value::Text("none".into()),
            ),
            (
                coset::cbor::Value::Text("attStmt".into()),
                coset::cbor::Value::Map(vec![]),
            ),
            (
                coset::cbor::Value::Text("authData".into()),
                coset::cbor::Value::Bytes(auth_data),
            ),
        ]);
        let mut buf = Vec::new();
        coset::cbor::ser::into_writer(&value, &mut buf).expect("attestationObject CBOR組立");
        buf
    }

    /// catalog.db(ABH配下)へcredentialを1件挿入(登録済み状態の作成)。soft passkey
    /// の鍵でfinish検証を通した本物のPasskeyをserde保存する(login/start->finishの
    /// server-to-server検証が通る既登録状態=P4 flow観測の前提)。
    fn auth_seed_credential(credential_id: &str) {
        use webauthn_rs::prelude::RegisterPublicKeyCredential;
        let auth = AuthState::new(&default_server_config(), None).expect("test用AuthState");
        let (ccr, state) = auth
            .webauthn
            .start_passkey_registration(auth.user_id, "amazing-backup", "amazing-backup", None)
            .expect("seed用challenge発行");
        let options = serde_json::to_value(&ccr).expect("seed用options");
        let reg =
            serde_json::from_value::<RegisterPublicKeyCredential>(soft_register_response(&options))
                .expect("seed用soft応答");
        let passkey = auth
            .webauthn
            .finish_passkey_registration(&reg, &state)
            .expect("seed用passkey検証");
        let passkey_json = serde_json::to_string(&passkey).expect("passkey serde");
        let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
        db.upsert_credential(credential_id, &passkey_json, "test")
            .expect("credential挿入");
    }

    /// register/finishへ送る合成認証応答(RegisterPublicKeyCredential相当のJSON)。
    /// options(登録options応答)のchallengeからclientDataJSON(type=webauthn.create・
    /// challenge・origin=rp_origin)を組み、attestationObject(fmt="none"+authData+
    /// COSE EC2 P-256 ES256公開鍵)を合成する(WebAuthn仕様準拠・P-256鍵は固定)。
    fn soft_register_response(options: &serde_json::Value) -> serde_json::Value {
        let challenge = options
            .pointer("/publicKey/challenge")
            .and_then(|c| c.as_str())
            .expect("登録optionsにchallenge含有");
        let client_data_json = serde_json::json!({
            "type": "webauthn.create",
            "challenge": challenge,
            "origin": SOFT_RP_ORIGIN,
        })
        .to_string();
        serde_json::json!({
            "id": SOFT_PASSKEY_CREDENTIAL_ID,
            "rawId": SOFT_PASSKEY_CREDENTIAL_ID,
            "type": "public-key",
            "response": {
                "attestationObject": soft_b64url(&soft_attestation_object()),
                "clientDataJSON": soft_b64url(client_data_json.as_bytes()),
                "transports": ["internal"],
            },
        })
    }

    /// login/finishへ送る合成認証応答(PublicKeyCredential相当のJSON)。challenge
    /// (login/start応答)からclientDataJSON(type=webauthn.get・challenge・origin)を
    /// 組み、authenticatorData(rp_id_hash+flags UP|UV)+signature(P-256 ECDSA・
    /// SHA-256・authenticatorData||SHA-256(clientDataJSON)へのDER署名)を合成する。
    fn soft_login_assertion(challenge: &serde_json::Value) -> serde_json::Value {
        use p256::ecdsa::signature::Signer as _;
        let challenge = challenge
            .pointer("/publicKey/challenge")
            .and_then(|c| c.as_str())
            .expect("login/start応答にchallenge含有");
        let client_data_json = serde_json::json!({
            "type": "webauthn.get",
            "challenge": challenge,
            "origin": SOFT_RP_ORIGIN,
        })
        .to_string();
        let mut auth_data = Vec::new();
        auth_data.extend_from_slice(&soft_rp_id_hash());
        auth_data.push(0x05); // flags: UP|UV
        auth_data.extend_from_slice(&0u32.to_be_bytes()); // signCount
        let client_hash: [u8; 32] = {
            use sha2::Digest as _;
            let mut hasher = sha2::Sha256::new();
            hasher.update(client_data_json.as_bytes());
            hasher.finalize().into()
        };
        let mut message = auth_data.clone();
        message.extend_from_slice(&client_hash);
        let signature: p256::ecdsa::Signature = soft_signing_key().sign(&message);
        let signature = signature.to_der();
        serde_json::json!({
            "id": SOFT_PASSKEY_CREDENTIAL_ID,
            "rawId": SOFT_PASSKEY_CREDENTIAL_ID,
            "type": "public-key",
            "response": {
                "authenticatorData": soft_b64url(&auth_data),
                "clientDataJSON": soft_b64url(client_data_json.as_bytes()),
                "signature": soft_b64url(signature.as_bytes()),
                "userHandle": null,
            },
        })
    }

    /// [covers:webauthn.challenge-reclaim-and-cap]
    /// 未完了challengeの蓄積対策(レビュー指摘#5): 同一sessionの保留challengeは
    /// 最新1件のみ(再startで旧challengeを無効化=1session 1pending)。全体件数は
    /// 上限(64)で最古から破棄される=未認証のstart連打で期限後も無制限に残らない。
    /// 期限切れ掃除はstart時に実行される(期限切れchallengeはtake時拒否と同じ結果
    /// になるため、ここではsession制限・上限破棄の観測可能面を担保する)。
    #[test]
    fn api_challenges_evicted_per_session_and_capped() {
        let flow = AuthFlow::new("srv-auth-evict");
        auth_seed_credential(SOFT_PASSKEY_CREDENTIAL_ID);

        // (1) 同一session(cookie)でlogin/startを2回: 1回目のchallengeは破棄される
        let (status, first, cookies1) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            None,
        );
        assert_eq!(status, 200, "1回目のlogin/start: {first}");
        let c1 = first_cookie(&cookies1).expect("1回目のsession cookie");
        let cookie_header = format!("{}={}", c1.0, c1.1);
        let (status, second, _) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            Some(&cookie_header),
        );
        assert_eq!(status, 200, "同一sessionでの2回目login/start: {second}");
        // 1回目のchallengeでfinishしても401(同一session再startで旧challengeは無効)
        let assertion = soft_login_assertion(&first);
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(assertion),
            Some(&cookie_header),
        );
        assert_eq!(
            status, 401,
            "同一sessionの旧challengeは再startで破棄される: {body}"
        );
        // 対照: 最新(2回目)のchallengeなら同一sessionでも検証が通る
        let assertion = soft_login_assertion(&second);
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(assertion),
            Some(&cookie_header),
        );
        assert_eq!(status, 200, "最新challengeは有効: {body}");

        // (2) 全体上限: 別sessionで上限(64)までstartした後に1件追加すると
        // 最古(先頭)のchallengeが破棄される(未認証start連打による無制限蓄積の防止)
        let mut oldest: Option<(serde_json::Value, String)> = None;
        for i in 0..MAX_PENDING_CHALLENGES {
            let (status, options, cookies) = flow.call(
                "POST",
                "/api/auth/login/start",
                Some(serde_json::json!({})),
                None,
            );
            assert_eq!(status, 200, "{}個目のlogin/start", i + 1);
            if i == 0 {
                let c = first_cookie(&cookies).expect("先頭分のsession cookie");
                oldest = Some((options, format!("{}={}", c.0, c.1)));
            }
        }
        let (oldest_options, oldest_cookie) = oldest.expect("先頭分のchallengeとcookie");
        // 上限を1つ超える追加(この時点で最古が破棄される)
        let (status, _, _) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            None,
        );
        assert_eq!(status, 200, "上限超過のlogin/startも成功(最古を破棄)");
        let assertion = soft_login_assertion(&oldest_options);
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(assertion),
            Some(&oldest_cookie),
        );
        assert_eq!(
            status, 401,
            "上限超過で最古challengeは破棄される(蓄積しない): {body}"
        );
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
    /// (register/startはtoken必須401・logoutは要認証401・api-keyはLAN内ならkey検証
    /// へ進む=不正keyは401。TASK-13改訂分のapi-key caseは実装前はrouting無しの
    /// 404 fallbackでfailを許容)。
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
        assert_ne!(
            status, 404,
            "register/startはroutingされる(404 fallbackでない)"
        );
        assert_eq!(status, 401, "token無しは401: {body}");
        expect_error_body(&body, "unauthenticated");

        // 残るauth endpointも404 fallbackでない定義済み応答
        for (method, path) in [
            ("POST", "/api/auth/register/finish"),
            ("POST", "/api/auth/login/start"),
            ("POST", "/api/auth/login/finish"),
            ("POST", "/api/auth/logout"),
            ("POST", "/api/auth/api-key"),
        ] {
            let (status, _, _) = flow.call(method, path, Some(serde_json::json!({})), None);
            assert_ne!(
                status, 404,
                "{method} {path}はroutingされる(404 fallbackでない)"
            );
        }

        // logoutは要認証(未認証401)
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/logout",
            Some(serde_json::json!({})),
            None,
        );
        assert_eq!(status, 401, "logoutは要認証(未認証401): {body}");
        expect_error_body(&body, "unauthenticated");

        // api-keyはLAN内既定remote addrならkey検証へ進む(bodyにapi_key不在のため
        // 不正扱い=401。TASK-13実装前はroutingが無く404 fallbackでfailを許容)
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/api-key",
            Some(serde_json::json!({"api_key": "abk-not-registered-key"})),
            None,
        );
        assert_ne!(status, 404, "api-keyはroutingされる(404 fallbackでない)");
        assert_eq!(status, 401, "LAN内の不正keyは401(key検証へ進む): {body}");
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
        assert_eq!(
            status, 401,
            "不一致tokenは401(登録閉鎖=fail-closed): {body}"
        );
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
        assert!(
            !cookies.is_empty(),
            "anonymous session cookie(Set-Cookie)を発行"
        );
        // 2回目: 同一tokenは消費済みのため拒否(原子的消費=1回限り)
        let (status, body, _) = flow.call("POST", "/api/auth/register/start", Some(req), None);
        assert_eq!(status, 401, "使用済みtokenの再使用は401: {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.bootstrap-token-expiry]
    /// token期限は10分。期限切れtokenでのregister/startは401(登録閉鎖)。
    #[test]
    fn api_bootstrap_token_expires_after_10min() {
        let flow = AuthFlow::new("srv-auth-token-expiry");
        // 時刻注入seamでtoken発行から10分+1ms経過を注入(実時間の経過待ち無し)
        flow.advance_time(10 * 60 * 1000 + 1);
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/register/start",
            Some(serde_json::json!({"token": AUTH_TEST_TOKEN})),
            None,
        );
        assert_eq!(
            status, 401,
            "期限切れtokenは401(不一致と同じ登録閉鎖): {body}"
        );
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.bootstrap-token-restart-invalidation]
    /// tokenはin-memory保持のためserve再起動(=新規server state)で失効する。
    #[test]
    fn api_bootstrap_token_invalidated_on_restart() {
        // TODO(P4実装): api_test_state()へのtoken注入seam(AUTH_TEST_TOKEN)。
        let mut flow = AuthFlow::new("srv-auth-token-restart");
        let req = serde_json::json!({"token": AUTH_TEST_TOKEN});
        let (status, _, _) = flow.call("POST", "/api/auth/register/start", Some(req.clone()), None);
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
        let (status, challenge, cookies) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            None,
        );
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
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(assertion),
            Some(&cookie),
        );
        assert_eq!(status, 401, "使用済みchallengeの再使用は401: {body}");
        expect_error_body(&body, "unauthenticated");
        // 期限120秒経過のchallengeも同様に拒否(時刻注入seamで120秒+1ms経過を注入。
        // offsetは絶対値設定のため発行後の注入で期限を切らせる)
        let (status, challenge, cookies) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            None,
        );
        assert_eq!(status, 200, "login/start(期限切れ観測用): {challenge}");
        let (name, value) = first_cookie(&cookies).expect("anonymous session cookie");
        let cookie = format!("{name}={value}");
        flow.advance_time(120 * 1000 + 1);
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(soft_login_assertion(&challenge)),
            Some(&cookie),
        );
        assert_eq!(status, 401, "期限切れchallengeは401: {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:webauthn.challenge-bound-to-session-cookie]
    /// challengeは発行時のanonymous session cookieと結合(別browserへの転用は拒否)。
    #[test]
    fn api_challenge_is_bound_to_anonymous_session_cookie() {
        let flow = AuthFlow::new("srv-auth-challenge-bind");
        auth_seed_credential("soft-credential-1");
        let (status, challenge, cookies) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            None,
        );
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
        assert_eq!(
            status, 401,
            "別cookieでのchallenge使用は401(転用拒否): {body}"
        );
        expect_error_body(&body, "unauthenticated");
        // cookie無しでも拒否
        let (status, body, _) = flow.call("POST", "/api/auth/login/finish", Some(assertion), None);
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
        let (status, challenge, cookies) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            None,
        );
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
    /// 修正(P4実装): 元testはsoft_login_assertionへlogin/startのchallengeを渡さず
    /// 空JSONを渡していた=challenge束ねの署名合成が不可能(WebAuthn検証と設計8章
    /// が矛盾するためchallenge渡しを修正。期待値・観測は不変)。
    #[test]
    fn api_login_rotates_session_id() {
        let flow = AuthFlow::new("srv-auth-rotation");
        auth_seed_credential("soft-credential-1");
        let (status, challenge, cookies) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            None,
        );
        assert_eq!(status, 200, "login/start");
        let (name, anon_value) = first_cookie(&cookies).expect("anonymous session cookie");
        let login_cookie = format!("{name}={anon_value}");
        let (status, body, cookies) = flow.call(
            "POST",
            "/api/auth/login/finish",
            Some(soft_login_assertion(&challenge)),
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
        let (status, _, cookies) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            None,
        );
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
    /// 期限24h自体はExpiry::OnInactivity(24h)としてserverが設定(tower-sessionsの
    /// 内部時刻のためseam注入不可)。ここではstore上に存在しない期限切れ相当の
    /// session ID cookieでの観測とする(期限切れsessionは未認証=401と同じ扱い)。
    #[test]
    fn api_session_expires_after_24h() {
        let flow = AuthFlow::new("srv-auth-session-expiry");
        let (status, body, _) = flow.call(
            "GET",
            "/api/entities",
            None,
            Some("id=expired-session-value"),
        );
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
        let (status, challenge, cookies) = flow.call(
            "POST",
            "/api/auth/login/start",
            Some(serde_json::json!({})),
            None,
        );
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
        let (status, body, _) = flow.call("POST", "/api/auth/logout", None, Some(&auth_cookie));
        assert_eq!(status, 200, "logoutは200: {body}");
        let (status, body, _) = flow.call("GET", "/api/entities", None, Some(&auth_cookie));
        assert_eq!(
            status, 401,
            "logout後の同一cookieは401(session破棄): {body}"
        );
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

    // -----------------------------------------------------------------------
    // TASK-13(api-key認証・2026-11-05テスト先行)のhandler単体test
    // (tmp/tasks/tm-13/design_api-key-auth.md D12〜D14・2.3/2.4章・4章)。
    // remote addr観測はAuthFlow::call_fromのConnectInfo挿入seam=本番の
    // into_make_service_with_connect_infoと同一のextensions機構(レビューH-1)。
    // api_keys tableへの行挿入・削除はrusqlite直接(実装後のinsert_api_key/
    // remove_api_key APIと同一schema)。実装前はendpoint未routingの404 fallback・
    // api_keys table不在の挿入失敗により実行failを許容(赤)。

    /// api-key test用の固定key(検索はSHA-256 hash一致のみのため任意の文字列)。
    const TEST_API_KEY: &str = "abk_test-key_0123456789abcdefghijklmnopqrstuv";
    /// TEST_API_KEYと異なるhashになる対照key(不一致401観測用)。
    const TEST_API_KEY_OTHER: &str = "abk_other-key_9876543210zyxwvutsrqponmlk";

    /// SHA-256 hex符号化(api_keysのkey_hash算出=設計2.1と同一形式)。
    fn sha256_hex_of(s: &str) -> String {
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(s.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// api_keys table(ABH配下catalog.db)へ1行挿入(rusqlite直接=実装後の
    /// insert_api_key APIと同一schema)。
    fn auth_seed_api_key(credential_id: &str, key: &str, name: &str) {
        // 先にP1 APIでopen=migrationを実行(実装後はapi_keys tableが作られる)
        crate::catalog::CatalogDb::open().expect("catalog DB open(migration実行)");
        let abh = std::env::var("AMAZING_BACKUP_HOME").expect("ABH設定済み(api_env)");
        let conn = rusqlite::Connection::open(std::path::Path::new(&abh).join("catalog.db"))
            .expect("catalog.db open");
        conn.execute(
            "INSERT INTO api_keys (credential_id, key_hash, name, created_at) \
             VALUES (?1, ?2, ?3, '2026-11-05T00:00:00+09:00')",
            rusqlite::params![credential_id, sha256_hex_of(key), name],
        )
        .expect("api_keys挿入(実装前はtable不在でfail=赤)");
    }

    /// api_keys tableから1行削除(manage auth remove相当=api-key失効の模擬)。
    fn auth_remove_api_key(credential_id: &str) {
        let abh = std::env::var("AMAZING_BACKUP_HOME").expect("ABH設定済み(api_env)");
        let conn = rusqlite::Connection::open(std::path::Path::new(&abh).join("catalog.db"))
            .expect("catalog.db open");
        conn.execute(
            "DELETE FROM api_keys WHERE credential_id = ?1",
            rusqlite::params![credential_id],
        )
        .expect("api_keys削除");
    }

    /// [covers:server.api-key-auth-establishes-session]
    /// LAN内remote addr+登録済みkey一致のPOST /api/auth/api-keyはlogin finishと
    /// 同一形式で200応答しsessionを確立する(以降の保護APIをsession cookieで通過)。
    #[test]
    fn api_key_auth_succeeds_from_lan_and_passes_protected_api() {
        let flow = AuthFlow::new("srv-apikey-ok");
        auth_seed_api_key("ak-test-1", TEST_API_KEY, "api-key");
        let (status, body, cookies) = flow.call(
            "POST",
            "/api/auth/api-key",
            Some(serde_json::json!({"api_key": TEST_API_KEY})),
            None,
        );
        assert_eq!(status, 200, "api-key認証は200: {body}");
        assert_eq!(
            body.get("status").and_then(|v| v.as_str()),
            Some("ok"),
            "応答shapeはlogin finishと同一: {body}"
        );
        let (name, value) = first_cookie(&cookies).expect("session cookie(Set-Cookie)");
        let cookie = format!("{name}={value}");
        // 確立したsessionで保護APIを通過
        let (status, body, _) = flow.call("GET", "/api/entities", None, Some(&cookie));
        assert_eq!(status, 200, "session cookieで保護API通過: {body}");
    }

    /// [covers:server.api-key-invalid-key-401]
    /// 登録されていないapi_key(LAN内)は401 unauthenticated(passkey login finish
    /// 検証失敗と同一・key存在特定防止の一般化message)。
    #[test]
    fn api_key_auth_wrong_key_is_401() {
        let flow = AuthFlow::new("srv-apikey-401");
        auth_seed_api_key("ak-w-1", TEST_API_KEY, "api-key");
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/api-key",
            Some(serde_json::json!({"api_key": TEST_API_KEY_OTHER})),
            None,
        );
        assert_eq!(
            status, 401,
            "不一致keyは401(403でない=LAN内はkey検証へ進む): {body}"
        );
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:server.api-key-outside-lan-403]
    /// LAN外remote addrはkey検証前に403 forbidden_sourceで受理拒否(正keyでも)。
    /// IPv4-mapped IPv6も正規化後にLAN外判定(dual-stack listen [::]構成)。
    #[test]
    fn api_key_auth_from_outside_lan_is_403() {
        let flow = AuthFlow::new("srv-apikey-lan");
        auth_seed_api_key("ak-lan-1", TEST_API_KEY, "api-key");
        // LAN外IPv4(正keyでも受理拒否=D14)
        let outside: std::net::SocketAddr = "8.8.8.8:51000".parse().expect("LAN外addr");
        let (status, body, _) = flow.call_from(
            "POST",
            "/api/auth/api-key",
            Some(serde_json::json!({"api_key": TEST_API_KEY})),
            None,
            outside,
        );
        assert_eq!(status, 403, "LAN外は正keyでも403(受理自体の拒否): {body}");
        expect_error_body(&body, "forbidden_source");
        // IPv4-mapped IPv6はto_ipv4_mapped正規化後に判定
        let mapped: std::net::SocketAddr = "[::ffff:8.8.8.8]:51001".parse().expect("v4-mapped");
        let (status, body, _) = flow.call_from(
            "POST",
            "/api/auth/api-key",
            Some(serde_json::json!({"api_key": TEST_API_KEY})),
            None,
            mapped,
        );
        assert_eq!(status, 403, "v4-mappedは正規化後にLAN外判定(403): {body}");
        expect_error_body(&body, "forbidden_source");
        // 対照: LAN内は403でない(不正keyはkey検証へ進み401)
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/api-key",
            Some(serde_json::json!({"api_key": TEST_API_KEY_OTHER})),
            None,
        );
        assert_eq!(status, 401, "LAN内は403でなくkey検証へ進む: {body}");
        expect_error_body(&body, "unauthenticated");
    }

    /// [covers:server.api-key-lan-boundary-table]
    /// is_lan_ipのLAN境界IP表(RFC1918 172.16/12・link-local 169.254/16・ULA fc00::/7・
    /// link-local fe80::/10の内外境界。v4-mappedはIPv4正規化後に判定)。
    #[test]
    fn is_lan_ip_boundary_ips() {
        let cases: &[(&str, bool)] = &[
            // RFC1918 172.16.0.0/12(172.15=外・172.16=内・172.31=内・172.32=外)
            ("172.15.255.255", false),
            ("172.16.0.0", true),
            ("172.31.255.255", true),
            ("172.32.0.0", false),
            // link-local 169.254.0.0/16(169.253=外・169.254=内・169.255=外)
            ("169.253.255.255", false),
            ("169.254.0.0", true),
            ("169.254.255.255", true),
            ("169.255.0.0", false),
            // ULA fc00::/7(fbff=外・fc00=内・fdff=内)
            ("fbff::", false),
            ("fc00::", true),
            ("fdff::", true),
            // link-local fe80::/10(fe7f=外・fe80=内・febf=内・fec0=外)
            ("fe7f::", false),
            ("fe80::", true),
            ("febf::", true),
            ("fec0::", false),
            // v4-mappedはto_ipv4_mappedでIPv4へ正規化後に判定(dual-stack listen [::])
            ("::ffff:10.0.0.1", true),
        ];
        for (addr, expect_lan) in cases {
            let ip: std::net::IpAddr = addr.parse().expect("境界IPのparse");
            assert_eq!(is_lan_ip(ip), *expect_lan, "境界IP {addr}のLAN内外判定");
        }
    }

    /// [covers:server.api-key-removal-invalidates-login]
    /// api-key失効(api_keys行削除=manage auth remove相当)後は同一keyで401。失効は
    /// 既存sessionを破棄しない(in-memory 24h=passkey removeと同一挙動)。
    #[test]
    fn api_key_auth_removed_key_is_401_and_session_survives() {
        let flow = AuthFlow::new("srv-apikey-rm");
        auth_seed_api_key("ak-rm-1", TEST_API_KEY, "api-key");
        let (status, _, cookies) = flow.call(
            "POST",
            "/api/auth/api-key",
            Some(serde_json::json!({"api_key": TEST_API_KEY})),
            None,
        );
        assert_eq!(status, 200, "事前のapi-key認証は成功");
        let (name, value) = first_cookie(&cookies).expect("session cookie");
        let cookie = format!("{name}={value}");

        // 失効(api_keysから当該行を削除)
        auth_remove_api_key("ak-rm-1");

        // 同一keyでの新規loginは401
        let (status, body, _) = flow.call(
            "POST",
            "/api/auth/api-key",
            Some(serde_json::json!({"api_key": TEST_API_KEY})),
            None,
        );
        assert_eq!(status, 401, "失効keyでのloginは401: {body}");
        expect_error_body(&body, "unauthenticated");
        // 既存sessionは破棄されない(継続)
        let (status, body, _) = flow.call("GET", "/api/entities", None, Some(&cookie));
        assert_eq!(status, 200, "既存sessionは破棄しない(in-memory 24h): {body}");
    }

    /// [covers:server.auth-status-api-keys-count]
    /// GET /api/auth/statusの応答へapi_keys(件数)が含まれる(credentialsはpasskey
    /// 件数のまま)。
    #[test]
    fn api_auth_status_includes_api_keys_count() {
        let flow = AuthFlow::new("srv-apikey-status");
        let (status, body, _) = flow.call("GET", "/api/auth/status", None, None);
        assert_eq!(status, 200, "statusは200: {body}");
        assert_eq!(
            body.get("api_keys").and_then(|v| v.as_i64()),
            Some(0),
            "api_keys 0件: {body}"
        );
        auth_seed_api_key("ak-c-1", TEST_API_KEY, "api-key");
        let (status, body, _) = flow.call("GET", "/api/auth/status", None, None);
        assert_eq!(status, 200);
        assert_eq!(
            body.get("api_keys").and_then(|v| v.as_i64()),
            Some(1),
            "挿入後はapi_keys=1: {body}"
        );
    }
}
