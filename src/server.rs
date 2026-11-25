//! TASK-9 serve基盤+読取API+passkey認証(設計1章D2: 同一binaryのserve subcommand・
//! 6章API・8章server.toml・passkey認証(D3/D9)・9章D4 localhost既定・10章dist二階建て・
//! 12章Phase P2〜P4)。
//!
//! - HTTP層はaxum。mount prefixは`/api/`。P3が読取API(entities/versions/pins/
//!   locations/pending/sync/diff)をrouteし、P4が認証API(register/login start/finish・
//!   logout・status)と保護API開放(session検証gate)を担う。TASK-17 W1がruns/jobs
//!   読取API(run履歴集約)を追加し、W4(TASK-27)がjob機構(jobs table正本・
//!   JobManager直列worker・POST /api/jobs/get・GET /api/jobs(/:id)・cancel・
//!   waiting_restore定期確認・起動時interrupted一括化)を追加
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
use std::path::{Path, PathBuf};

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
    let code = runtime.block_on(run_server(cfg, args));
    // runtimeのdropは実行中spawn_blocking(jobの実処理)の完了を無界に待つため、
    // worker joinの有界待ち(10秒)だけでは停止全体が有界にならない。明示的に
    // 有界shutdownし、超過分はprocess終了で中断する(実行中jobは次回起動時の
    // interrupted一括化で回復する=設計5.4)
    runtime.shutdown_timeout(std::time::Duration::from_secs(10));
    code
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

    // TASK-17 W4(TASK-27・設計5.4): 起動処理でstatusがqueued/running/
    // waiting_restoreのjob行をinterruptedへ一括更新する(自動再開しない=対象・費用
    // (DEEP_ARCHIVE復元)の意図しない再発行を避ける。get/pushとも冪等設計のため
    // 再発行で安全に回復する)。worker起動の前に行う(旧queuedをworkerが拾う前に
    // interruptedへ確定させる)。run_blocking経由=async contextからの同期層呼出境界
    {
        let scope = state.scope.clone();
        match run_blocking(move || JobManager::interrupt_jobs_at_boot(&scope)).await {
            Ok(updated) if updated > 0 => {
                eprintln!(
                    "serve: 前回起動時の未完了job {updated}件をinterruptedにしました(再実行は転送タブから)"
                );
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("WARNING: 起動時のjob状態interrupted化に失敗: {}", e.message);
            }
        }
    }

    // TASK-17 W4(設計5.1): job直列実行worker(同時実行1・書込APIと同一semaphoreで
    // 相互直列化・waiting_restore遷移時に解放)。graceful shutdownはwatch channel
    // でworkerへ伝達し、workerの終了待ちは有界(進行中jobのspawn_blockingが長時間
    // の場合に停止を塞がない=残った実行中行は次回起動時のinterrupted一括化で回復)
    let (job_shutdown_tx, job_shutdown_rx) = tokio::sync::watch::channel(false);
    let job_handle = {
        let mgr = state.jobs.clone();
        tokio::task::spawn(async move { mgr.worker_loop(job_shutdown_rx).await })
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
    // (結果のlog出力はtask側で完了時点に済んでいるためここでは待ち合わせのみ)。
    // job workerも停止通知→有界待ち(超過時はprocess終了で中断し・実行中job行は
    // 次回起動時のinterrupted一括化(設計5.4)で回復する)
    let _ = job_shutdown_tx.send(true);
    if tokio::time::timeout(std::time::Duration::from_secs(10), job_handle)
        .await
        .is_err()
    {
        eprintln!(
            "serve: job workerの停止待ちがタイムアウトしました(進行中jobは次回起動時にinterrupted化)"
        );
    }
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
/// - `jobs`: TASK-17 W4のjob機構(JobManager)。workerの起床notify・時刻注入seam
///   の到達先(直列worker自体はrun_serverがspawnする)
#[derive(Clone)]
struct AppState {
    sync_permits: std::sync::Arc<tokio::sync::Semaphore>,
    sync_ctx: std::sync::Arc<crate::storage::CtxArgs>,
    scope: (String, String),
    auth: Option<std::sync::Arc<AuthState>>,
    jobs: std::sync::Arc<JobManager>,
}

impl AppState {
    /// 実行context(起動引数)から構築。実効scopeはCtxArgsの解決結果
    /// (--aws-config/--profile込み)を起動時に一度だけ解決して保持する
    /// (aws.toml読込を伴うため呼出側の同期文脈で使う)。
    fn from_ctx_args(ctx: crate::storage::CtxArgs) -> BackupResult<AppState> {
        let aws = crate::storage::load_aws(&ctx.aws_config, ctx.profile.as_deref())?;
        let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let scope = (aws.bucket, aws.prefix);
        let jobs = JobManager::new(std::sync::Arc::new(ctx.clone()), scope.clone(), permits.clone());
        Ok(AppState {
            sync_permits: permits,
            sync_ctx: std::sync::Arc::new(ctx),
            scope,
            auth: None,
            jobs,
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

/// 重複(409・TASK-17 W2: collectionsのUNIQUE(scope,name)由来=同名作成・改名先同名)。
fn duplicate(message: &'static str) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::CONFLICT,
        code: "duplicate",
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
        .route(
            "/api/entities/{uuid}/tags",
            axum::routing::put(put_entity_tags),
        )
        .route(
            "/api/entities/{uuid}/rename",
            axum::routing::post(post_entity_rename),
        )
        .route(
            "/api/entities/{uuid}/location",
            axum::routing::post(post_entity_location),
        )
        .route("/api/tags", axum::routing::get(get_tags))
        .route("/api/folders", axum::routing::get(get_folders))
        .route(
            "/api/collections",
            axum::routing::get(get_collections).post(post_collection),
        )
        .route(
            "/api/collections/{id}",
            axum::routing::get(get_collection_detail)
                .put(put_collection)
                .delete(delete_collection),
        )
        .route(
            "/api/collections/{id}/items",
            axum::routing::post(post_collection_items),
        )
        .route(
            "/api/collections/{id}/items/{uuid}",
            axum::routing::delete(delete_collection_item),
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
        .route("/api/runs", axum::routing::get(get_runs))
        .route("/api/jobs", axum::routing::get(get_jobs))
        .route("/api/jobs/get", axum::routing::post(post_jobs_get))
        .route("/api/jobs/{id}", axum::routing::get(get_job_detail))
        .route("/api/jobs/{id}/cancel", axum::routing::post(post_job_cancel))
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
    // W2絞り込み: ?tag=<tag名>=当該tagを持つentityへ・?collection=<collections.id>=
    // 当該collection構成のentityへ(folder絞り込み用の新paramは設けず既存label=で成立)
    let tag = params.get("tag").cloned();
    // ?untagged=1=tagを1つも持たないentityのみ(設計2.2「tag無しも選択可」=コード
    // レビュー中9。tag=との同時指定・値1以外は400 invalid_request)
    let untagged = match params.get("untagged") {
        None => false,
        Some(v) if v == "1" => true,
        Some(_) => return Err(invalid_request("untaggedは1でのみ指定してください")),
    };
    if untagged && tag.is_some() {
        return Err(invalid_request("untaggedとtagは同時に指定できません"));
    }
    let collection_id = match params.get("collection") {
        None => None,
        Some(raw) => Some(parse_collection_id(raw)?),
    };
    let effective = state.scope.clone();
    let (entries, tag_rows, collection_members) = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(scope.as_deref(), &effective)?;
        let db = crate::catalog::CatalogDb::open()?;
        let (_header, entries) = db.load_entries(&bucket, &prefix)?;
        let tag_rows = db.scope_tag_rows(&bucket, &prefix)?;
        let collection_members = match collection_id {
            // 不在collection idは空集合=空配列200(絞り込みの一種でありerrorにしない)
            Some(id) => db
                .collection_detail(&bucket, &prefix, id)?
                .map(|c| c.uuids)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Ok((entries, tag_rows, collection_members))
    })
    .await?;
    // uuid毎のtag一覧(scope_tag_rowsはuuid・tag昇順のため挿入順がそのまま昇順)
    let mut tags_by_uuid: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (uuid, tag) in &tag_rows {
        tags_by_uuid.entry(uuid.clone()).or_default().push(tag.clone());
    }
    let member_set: std::collections::HashSet<String> = collection_members.into_iter().collect();
    // 絞り込み(query=CLI queryと同一規則・category/label=location属性一致(label=空文字は
    // ラベルなし完全一致)・tag/collection=W2 local語彙=実効scope内の行のみ・untagged=
    // tag行を持たないentityのみ)
    let matched: Vec<&crate::index::IndexFileEntry> = entries
        .iter()
        .filter(|e| {
            query_match(e, query.as_deref())
                && crate::storage::query::category_matched(e, category.as_deref())
                && label_match(e, label.as_deref())
                && tag.as_deref().is_none_or(|want| {
                    tags_by_uuid
                        .get(&e.uuid)
                        .is_some_and(|list| list.iter().any(|t| t == want))
                })
                && (!untagged || !tags_by_uuid.contains_key(&e.uuid))
                && (collection_id.is_none() || member_set.contains(&e.uuid))
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
        .map(|e| {
            // W2: 各summary行へtags(昇順)を含める(行のtag列chip表示の基盤)
            let mut summary = crate::storage::query::entry_summary_json(e, false);
            summary["tags"] = serde_json::json!(
                tags_by_uuid.get(&e.uuid).cloned().unwrap_or_default()
            );
            summary
        })
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
/// version_list付きentry_summary_json互換+TASK-17 W2のtags・collections(所属
/// collection名一覧)。不在は404 not_found)。
async fn get_entity_detail(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let db = crate::catalog::CatalogDb::open()?;
        let e = find_entry_by_uuid(&db, &bucket, &prefix, &uuid)?;
        entity_detail_with_vocabulary(&db, &bucket, &prefix, &e)
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

/// GET /api/runs(TASK-17設計3.2・2.1: run(batch_id)毎の履歴)。catalogのversionsを
/// batch_id単位で集約し`{"runs":[{batch_id,date,entity_count}]}`形式で応答する
/// (ホームの前回backup・管理のbackup実行履歴(W7)の基盤)。集約規則は
/// server.runs-list条件の解釈に従う: entity_count=当該batch_idのversion数(=runの
/// 対象実体数)・date=当該runのversion日付の最新(dateは`YYYY-MM-DD`のため辞書順
/// 比較で最新が決まる)・並びはdate降順(直近runが先頭=前回backup表示は先頭要素で
/// 成立。同date内はrun内の最新version_ts降順=実行順。batch_idの接尾辞は時刻の
/// hashのため辞書順は実行順を保証せず、sort keyには使わない)。version_tsはJST
/// 固定のISO8601のため辞書順比較で時刻順になる。応答schemaにversion_tsは含め
/// ない(内部sort keyのみ)。scopeは読取API共通(server.read-api-
/// uses-serve-profileと同一扱い)。
async fn get_runs(
    axum::extract::State(state): axum::extract::State<AppState>,
    uri: axum::http::Uri,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let params = parse_query_params(uri.query());
    let scope = params.get("scope").cloned();
    let effective = state.scope.clone();
    let runs = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(scope.as_deref(), &effective)?;
        let db = crate::catalog::CatalogDb::open()?;
        let (_header, entries) = db.load_entries(&bucket, &prefix)?;
        // batch_id集約: (date=run内version日付の最新, 最新version_ts, version数)
        let mut agg: std::collections::HashMap<String, (String, String, i64)> =
            std::collections::HashMap::new();
        for entry in &entries {
            for v in &entry.versions {
                let slot = agg
                    .entry(v.batch_id.clone())
                    .or_insert((v.date.clone(), v.version_ts.clone(), 0));
                slot.2 += 1;
                if v.date > slot.0 {
                    slot.0 = v.date.clone();
                }
                if v.version_ts > slot.1 {
                    slot.1 = v.version_ts.clone();
                }
            }
        }
        let mut rows: Vec<(String, String, String, i64)> = agg
            .into_iter()
            .map(|(batch_id, (date, latest_ts, entity_count))| {
                (batch_id, date, latest_ts, entity_count)
            })
            .collect();
        // date降順・同date内はrun内の最新version_ts降順(実行順。batch_id接尾辞は
        // 時刻hashのためkeyにしない)
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.2.cmp(&a.2)));
        Ok(rows
            .into_iter()
            .map(|(batch_id, date, _latest_ts, entity_count)| {
                serde_json::json!({
                    "batch_id": batch_id,
                    "date": date,
                    "entity_count": entity_count,
                })
            })
            .collect::<Vec<_>>())
    })
    .await?;
    Ok(axum::Json(serde_json::json!({ "runs": runs })))
}

/// GET /api/jobs(TASK-17設計3.2・9章W4: job一覧`{"jobs":[...]}`の実質化)。
/// jobs tableから実効scopeの行を応答する(設計5.4: job状態の正本はjobs table)。
/// 並びは進行中(queued/running/waiting_restore)優先・各group内はid降順
/// (=新しい順。進行中jobが一覧上位に来る=ホーム・転送タブの追従表示基盤)。
/// ?status=・?kind=絞り込み(不正値は400 invalid_request=既存error体系と共通)。
/// scope絞り込みparamは設計3.2どおり持たない(実効1 scope)。保護は/api/*共通の
/// auth_gateが構造的に担う(job系APIも同一session gate=設計6章D-W11)。
async fn get_jobs(
    axum::extract::State(state): axum::extract::State<AppState>,
    uri: axum::http::Uri,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let params = parse_query_params(uri.query());
    let status = parse_job_status_param(&params)?;
    let kind = parse_job_kind_param(&params)?;
    let scope = state.scope.clone();
    let jobs = run_blocking(move || {
        let db = crate::catalog::CatalogDb::open()?;
        db.list_jobs(&scope.0, &scope.1, status.as_deref(), kind.as_deref())
    })
    .await?;
    let items = jobs.iter().map(job_json).collect::<Vec<_>>();
    Ok(axum::Json(serde_json::json!({ "jobs": items })))
}

/// GET /api/jobs/:id(job詳細。params・progress・result・error付きのjob objectを
/// `{"job": {...}}`で応答。不在id・他scopeのidは404 not_found)。
async fn get_job_detail(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let id = parse_job_id(&id)?;
    let scope = state.scope.clone();
    let job = run_blocking(move || {
        let db = crate::catalog::CatalogDb::open()?;
        db.job_by_id(&scope.0, &scope.1, id)?
            .ok_or_else(|| BackupError::Target("対象のjobが見つかりません".to_string()))
    })
    .await?;
    Ok(axum::Json(serde_json::json!({ "job": job_json(&job) })))
}

/// POST /api/jobs/:id/cancel(TASK-17設計3.2: 待機中(queued)jobの取消)。
/// queuedのみ受理(status=cancelledへ更新・応答は操作後のjob object)。実行中
/// (running/waiting_restore)は409 duplicate(完了まで実行=冪等再実行で回復)。
/// 完了系(succeeded/failed/interrupted/cancelled)も404でなく409(取消対象が
/// 無い=取り消し済み・確定済み)。不在idは404 not_found。status="cancelled"は
/// 設計3.2に取消後の状態値の規定が無いための解釈(取消の記録=再実行対象として
/// 一覧に残る。jobs tableは履歴兼用のため行は消さない)。
async fn post_job_cancel(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let id = parse_job_id(&id)?;
    let scope = state.scope.clone();
    let outcome = run_blocking(move || {
        let db = crate::catalog::CatalogDb::open()?;
        let job = db
            .job_by_id(&scope.0, &scope.1, id)?
            .ok_or_else(|| BackupError::Target("対象のjobが見つかりません".to_string()))?;
        if job.status != "queued" {
            // 実行中・完了系は取消不可(呼出側で409へ変換)
            return Ok(CancelOutcome::NotQueued);
        }
        let updated = db.cancel_queued_job(id)?;
        if updated == 0 {
            // status確認後にworkerが実行へ遷移した競合も409(実行中扱い)
            return Ok(CancelOutcome::NotQueued);
        }
        let job = db
            .job_by_id(&scope.0, &scope.1, id)?
            .ok_or_else(|| BackupError::Internal("取消直後のjob行が読めない".to_string()))?;
        Ok(CancelOutcome::Cancelled(job))
    })
    .await?;
    match outcome {
        CancelOutcome::Cancelled(job) => {
            Ok(axum::Json(serde_json::json!({ "job": job_json(&job) })))
        }
        CancelOutcome::NotQueued => Err(duplicate(
            "実行中・完了済みのjobは取り消せません(完了まで実行されます)",
        )),
    }
}

/// cancel処理の結果(NotQueued=409変換用)。
enum CancelOutcome {
    Cancelled(crate::catalog::JobRow),
    NotQueued,
}

/// POST /api/jobs/get(TASK-17設計3.2・9章W4: 取得job発行)。body
/// {uuid,version_id?,pin?,out,tier?,days?,overwrite?}を検証してjob行をqueuedで
/// insertし即時応答する(設計5.1: POST /api/jobs/*はjob行insert(queued)で即時応答)。
/// 対象の索引解決は発行時に行わずjob実行時の失敗(status=failed+error)として記録
/// する(長時間jobのため発行を解決で止めない)。省略時既定はCLI getと同一
/// (tier=Standard・days=7・overwrite=false=getの--yes相当)。
/// 応答は{"job": <job object>}(status=queued。操作後の対象状態=読取系と同一schema)。
async fn post_jobs_get(
    axum::extract::State(state): axum::extract::State<AppState>,
    body: axum::body::Bytes,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let params = parse_get_job_body(&body)?;
    let scope = state.scope.clone();
    let job = run_blocking(move || {
        let db = crate::catalog::CatalogDb::open()?;
        let params_json =
            serde_json::to_string(&params.to_json()).expect("job params serialize");
        let progress_json = job_progress_json("queued", false, None, None);
        db.insert_job(&scope.0, &scope.1, "get", &params_json, &progress_json)
    })
    .await?;
    // workerへの起床通知(queued到着の即時実行。通知を逃してもworkerのtickが拾う
    // =DBが正本のため通知はbest-effort)
    state.jobs.notify_worker();
    Ok(axum::Json(serde_json::json!({ "job": job_json(&job) })))
}

// ---------------------------------------------------------------------------
// TASK-17 W4(TASK-27)job機構(設計4.1 jobs table・5章)。jobs tableを正本とする
// 長時間job(get。pushはW5)の直列実行worker(JobManager)。

/// waiting_restore定期確認の既定間隔(分・設計5.2の既定15分)。W7でsettings table
/// (restore_check_interval_min)へ移行するまでJobManager側の値として持つ。
const RESTORE_CHECK_INTERVAL_MIN: i64 = 15;

/// workerの待機tick(ms)。queued到着はnotifyで即時起床する一方・waiting_restoreの
/// 確認時刻(next_check_at)到来はこのtick(1秒粒度)で検出する(確認自体は15分単位
/// のため1秒粒度で十分)。
const JOB_WORKER_TICK_MS: u64 = 1000;

/// job status語彙(?status=絞り込みの検証・並び規則と共通の正本)。
const JOB_STATUSES: [&str; 7] = [
    "queued",
    "running",
    "waiting_restore",
    "succeeded",
    "failed",
    "interrupted",
    "cancelled",
];

/// job kind語彙(?kind=絞り込みの検証。get=W4・push=W5で発行される)。
const JOB_KINDS: [&str; 2] = ["get", "push"];

/// get jobの発行params(POST /api/jobs/get body=jobs table params_jsonの内容)。
#[derive(Debug, Clone)]
struct GetJobParams {
    uuid: String,
    version_id: Option<String>,
    pin: Option<String>,
    out: std::path::PathBuf,
    tier: String,
    days: u32,
    overwrite: bool,
}

impl GetJobParams {
    /// paramsの応答・保存表現(version_id/pinの未指定はnullで表現する)。
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "uuid": self.uuid,
            "version_id": self.version_id,
            "pin": self.pin,
            "out": self.out.display().to_string(),
            "tier": self.tier,
            "days": self.days,
            "overwrite": self.overwrite,
        })
    }

    /// params_jsonからの復元(job実行側)。params_jsonは本構造がto_jsonで書いた値
    /// ため不正時はNone(呼出側はjobをfailedで確定させる)。
    fn from_json(v: &serde_json::Value) -> Option<GetJobParams> {
        let uuid = v.get("uuid")?.as_str()?.to_string();
        let version_id = opt_str_of(v, "version_id")?;
        let pin = opt_str_of(v, "pin")?;
        let out = std::path::PathBuf::from(v.get("out")?.as_str()?);
        let tier = v.get("tier")?.as_str()?.to_string();
        let days = v.get("days")?.as_u64()? as u32;
        let overwrite = v.get("overwrite")?.as_bool()?;
        Some(GetJobParams {
            uuid,
            version_id,
            pin,
            out,
            tier,
            days,
            overwrite,
        })
    }
}

/// JSON objectからnull許容のstring fieldを取り出す(null・不在はNone・非stringは
/// None=復元失敗扱い)。
fn opt_str_of(v: &serde_json::Value, key: &str) -> Option<Option<String>> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(s)) => Some(Some(s.clone())),
        _ => None,
    }
}

/// POST /api/jobs/get bodyの検証(必須field欠落・型違反は400 invalid_request・
/// 規則違反(tier/daysの値域・空uuid/out)は422 validation_failed=既存書込APIと
/// 同一の区分)。
fn parse_get_job_body(body: &axum::body::Bytes) -> Result<GetJobParams, ApiError> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    let str_field = |name: &str| v.get(name).and_then(|x| x.as_str()).map(String::from);
    let uuid = str_field("uuid")
        .ok_or_else(|| invalid_request("uuidは文字列で指定してください"))?;
    let out = str_field("out")
        .ok_or_else(|| invalid_request("outは文字列で指定してください"))?;
    let opt_str_field = |name: &'static str, label: &'static str| match v.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid_request(label)),
    };
    let version_id = opt_str_field("version_id", "version_idは文字列で指定してください")?
        .filter(|s| !s.is_empty());
    let pin = opt_str_field("pin", "pinは文字列で指定してください")?.filter(|s| !s.is_empty());
    let tier = match v.get("tier") {
        None => "Standard".to_string(),
        Some(serde_json::Value::String(s)) => s.to_string(),
        Some(_) => return Err(invalid_request("tierは文字列で指定してください")),
    };
    if !["Standard", "Bulk"].contains(&tier.as_str()) {
        return Err(validation_failed(
            "tierはStandard/Bulkのいずれかです",
        ));
    }
    let days = match v.get("days") {
        None => 7,
        Some(serde_json::Value::Number(n)) => {
            let d = n
                .as_u64()
                .ok_or_else(|| invalid_request("daysは数値で指定してください"))?;
            u32::try_from(d).map_err(|_| invalid_request("daysは数値で指定してください"))?
        }
        Some(_) => return Err(invalid_request("daysは数値で指定してください")),
    };
    if !(1..=365).contains(&days) {
        return Err(validation_failed("daysは1〜365で指定してください"));
    }
    let overwrite = match v.get("overwrite") {
        None => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(_) => return Err(invalid_request("overwriteは真偽値で指定してください")),
    };
    if uuid.is_empty() {
        return Err(validation_failed("uuidを指定してください"));
    }
    if out.is_empty() {
        return Err(validation_failed("出力dirを指定してください"));
    }
    Ok(GetJobParams {
        uuid,
        version_id,
        pin,
        out: std::path::PathBuf::from(out),
        tier,
        days,
        overwrite,
    })
}

/// ?status=絞り込みparamの検証(不正値は400 invalid_request。設計3.2 status語彙)。
fn parse_job_status_param(
    params: &std::collections::HashMap<String, String>,
) -> Result<Option<String>, ApiError> {
    let Some(value) = params.get("status") else {
        return Ok(None);
    };
    if JOB_STATUSES.contains(&value.as_str()) {
        return Ok(Some(value.clone()));
    }
    Err(invalid_request("statusの値が不正です"))
}

/// ?kind=絞り込みparamの検証(不正値は400 invalid_request。get=W4・push=W5)。
fn parse_job_kind_param(
    params: &std::collections::HashMap<String, String>,
) -> Result<Option<String>, ApiError> {
    let Some(value) = params.get("kind") else {
        return Ok(None);
    };
    if JOB_KINDS.contains(&value.as_str()) {
        return Ok(Some(value.clone()));
    }
    Err(invalid_request("kindの値が不正です"))
}

/// :id path paramの検証(数値idで無ければ400)。
fn parse_job_id(raw: &str) -> Result<i64, ApiError> {
    raw.parse::<i64>()
        .map_err(|_| invalid_request("jobは数値idで指定してください"))
}

/// jobs行のAPI応答表現(job object。読取・発行応答・取消応答で共通)。params/
/// progress/resultは保存済みJSON文字列のparse結果(書込は本serverのみのため
/// parse失敗はnullで応答する)。errorは{code,message}(無しはnull)。
fn job_json(row: &crate::catalog::JobRow) -> serde_json::Value {
    let params: serde_json::Value =
        serde_json::from_str(&row.params_json).unwrap_or(serde_json::Value::Null);
    let progress: serde_json::Value =
        serde_json::from_str(&row.progress_json).unwrap_or(serde_json::Value::Null);
    let result: serde_json::Value = row
        .result_json
        .as_deref()
        .and_then(|r| serde_json::from_str(r).ok())
        .unwrap_or(serde_json::Value::Null);
    let error = match (&row.error_code, &row.error_message) {
        (Some(code), Some(message)) => serde_json::json!({ "code": code, "message": message }),
        _ => serde_json::Value::Null,
    };
    serde_json::json!({
        "id": row.id,
        "kind": row.kind,
        "status": row.status,
        "params": params,
        "progress": progress,
        "result": result,
        "error": error,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    })
}

/// get jobのprogress_json(設計5.2: {phase,total,done,current_path}。段階単位で更新
/// する=getは対象file 1件のためtotal=1)。phase語彙: queued(発行直後)→resolve
/// (解決・衝突検査)→restore(復元待ち・定期確認中)→fetch(取得・sha照合・復号・配置)。
/// waiting_restore中のみnext_check_at(timerの次回確認時刻・UIは未知fieldを無視)。
fn job_progress_json(
    phase: &str,
    done: bool,
    current_path: Option<&str>,
    next_check_at: Option<&str>,
) -> String {
    let done_n = if done { 1 } else { 0 };
    serde_json::to_string(&serde_json::json!({
        "phase": phase,
        "total": 1,
        "done": done_n,
        "current_path": current_path,
        "next_check_at": next_check_at,
    }))
    .expect("progress serialize")
}

/// job実行step(spawn_blocking内)の結果。workerがDB反映へ変換する。
enum JobStep {
    /// 終状態(status・progress・result・error一式)
    Terminal {
        status: &'static str,
        progress: String,
        result_json: Option<String>,
        error_code: Option<&'static str>,
        error_message: Option<String>,
    },
    /// 復元job発行済み・未復原(waiting_restoreへ遷移。semaphoreはstepのreturnで
    /// 解放される=permitがclosure内でdropされる。current_path=対象s3_key)
    WaitingRestore { current_path: String },
}

/// get job実行の1step(同期層・spawn_blocking内で実行。permitは呼出側からmove
/// して持ち込み・stepのreturnでdrop=waiting_restore遷移時にsemaphore解放)。
/// 解決→平文衝突検査→復元要否判定→(Readyなら)取得・sha照合・復号・配置。
/// 復元が必要な場合はWaitingRestoreを返す(呼出側はstatus=waiting_restoreへ
/// 遷移しtimerの定期確認で再開する)。
fn execute_get_step(ctx: &crate::storage::CtxArgs, params: &GetJobParams) -> BackupResult<JobStep> {
    use crate::storage::get::{
        ensure_restored, plain_conflict_outcome, preflight_decrypt_env, resolve_fetch_target,
        PlainConflictOutcome, RestoreCheck,
    };
    let session = crate::storage::open_session(ctx)?;
    let key_path = crate::storage::resolve_key(ctx, session.cfg.as_ref())?;
    preflight_decrypt_env(&key_path)?;
    let (_header, entries) = crate::storage::read_index_for_query(&session, &key_path)?;
    let target = resolve_fetch_target(
        &entries,
        &params.uuid,
        None,
        None,
        params.version_id.as_deref(),
        params.pin.as_deref(),
    )?;
    let s3_key = target.s3_key.clone();
    match plain_conflict_outcome(&target, &params.out, params.overwrite)? {
        // sha一致skip(冪等): succeeded+skip付きresult(設計2.3の衝突時はoverwrite
        // 再取得で回復。sha一致は既に回復済みの状態)
        PlainConflictOutcome::Skip => {
            let dest = target.dest_path(&params.out);
            let shown = dest
                .strip_prefix(&params.out)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| dest.display().to_string());
            return Ok(JobStep::Terminal {
                status: "succeeded",
                progress: job_progress_json("fetch", true, Some(&s3_key), None),
                result_json: Some(
                    serde_json::to_string(&serde_json::json!({
                        "name": target.dest_name,
                        "dest": shown,
                        "skip": true,
                    }))
                    .expect("result serialize"),
                ),
                error_code: None,
                error_message: None,
            });
        }
        PlainConflictOutcome::Proceed => {}
    }
    match ensure_restored(
        session.backend.as_ref(),
        &session.aws,
        &target.s3_key,
        target.version_id.as_deref(),
        params.days,
        &params.tier,
    )? {
        RestoreCheck::Issued | RestoreCheck::Ongoing => {
            Ok(JobStep::WaitingRestore { current_path: s3_key })
        }
        RestoreCheck::Ready => {
            let dest = target.fetch_verify_decrypt(
                session.backend.as_ref(),
                &session.aws,
                &params.out,
                &key_path,
            )?;
            let shown = dest
                .strip_prefix(&params.out)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| dest.display().to_string());
            Ok(JobStep::Terminal {
                status: "succeeded",
                progress: job_progress_json("fetch", true, Some(&s3_key), None),
                result_json: Some(
                    serde_json::to_string(&serde_json::json!({
                        "name": target.dest_name,
                        "dest": shown,
                    }))
                    .expect("result serialize"),
                ),
                error_code: None,
                error_message: None,
            })
        }
    }
}

/// waiting_restore jobの復元確認(timerから・読取のみ=semaphore外)。設計5.2:
/// head-objectのRestore headerを確認(`list jobs`と同一parse)。索引は都度読み直す
/// (復元待ちの最大48hの間に索引が置換えられていても対象を追従させる)。
fn probe_restore(ctx: &crate::storage::CtxArgs, params: &GetJobParams) -> BackupResult<bool> {
    use crate::storage::get::{
        ensure_restored, preflight_decrypt_env, resolve_fetch_target, RestoreCheck,
    };
    let session = crate::storage::open_session(ctx)?;
    let key_path = crate::storage::resolve_key(ctx, session.cfg.as_ref())?;
    preflight_decrypt_env(&key_path)?;
    let (_header, entries) = crate::storage::read_index_for_query(&session, &key_path)?;
    let target = resolve_fetch_target(
        &entries,
        &params.uuid,
        None,
        None,
        params.version_id.as_deref(),
        params.pin.as_deref(),
    )?;
    let check = ensure_restored(
        session.backend.as_ref(),
        &session.aws,
        &target.s3_key,
        target.version_id.as_deref(),
        params.days,
        &params.tier,
    )?;
    // true=復元済み(取得処理を再開できる)・false=まだ復元待ち(Issuedは確認の
    // 間に復元要求が失効して再発行された場合=引き続き待つ)
    Ok(matches!(check, RestoreCheck::Ready))
}

/// 長時間jobの実行管理(TASK-17設計5章・AppState拡張)。直列worker(tokio task)が
/// queued順に1件ずつspawn_blockingで実行する(同時実行1=書込系同時実行禁止の
/// server側徹底・設計5.1)。job実行は書込APIと同一のsingle-flight semaphoreを
/// acquireしてから行う(3.1: jobはjob queueで並び実行時にsemaphoreをacquire。
/// 書込APIとjobの相互直列化)。waiting_restoreへ遷移する際にsemaphoreを解放する
/// (復元待ちの最大48hを書込API・他jobの停止にしない)。
struct JobManager {
    /// storage層呼出のcontext(serve起動時の実効profile)
    ctx: std::sync::Arc<crate::storage::CtxArgs>,
    /// 実効scope(bucket, prefix)。jobの読書はこのscopeに限る(設計2章scope分離)
    scope: (String, String),
    /// workerの起床notify(新規job発行・時刻注入seamでの即時再確認)
    notify: tokio::sync::Notify,
    /// 書込APIと共有のsingle-flight semaphore(workerもacquireして直列化)
    permits: std::sync::Arc<tokio::sync::Semaphore>,
    /// waiting_restore定期確認の間隔(分・設計5.2既定15。W7でsettingsへ移行)
    restore_check_interval_min: i64,
    /// 時刻注入seam(test用。本番は常に0=実時間。AuthState::set_time_offset_msと
    /// 同一流儀=waiting_restoreのnext_check_at到来をreal-time待ち無しで進める)
    time_offset_ms: std::sync::atomic::AtomicI64,
}

impl JobManager {
    /// AppState構築時に作る(semaphoreはAppState.sync_permitsと同一Arcを共有)。
    fn new(
        ctx: std::sync::Arc<crate::storage::CtxArgs>,
        scope: (String, String),
        permits: std::sync::Arc<tokio::sync::Semaphore>,
    ) -> std::sync::Arc<JobManager> {
        std::sync::Arc::new(JobManager {
            ctx,
            scope,
            notify: tokio::sync::Notify::new(),
            permits,
            restore_check_interval_min: RESTORE_CHECK_INTERVAL_MIN,
            time_offset_ms: std::sync::atomic::AtomicI64::new(0),
        })
    }

    /// workerへの起床通知(新規jobのqueued到着・seamでの時刻進行)。
    fn notify_worker(&self) {
        self.notify.notify_one();
    }

    /// seam考慮の現在時刻(JST ISO8601秒精度=storage::now_jstと同一形式。本番は
    /// 実時間・testはset_time_offset_msで進められる)。
    fn now_jst(&self) -> String {
        let offset = self
            .time_offset_ms
            .load(std::sync::atomic::Ordering::SeqCst);
        let now =
            crate::targets::now_jst() + chrono::Duration::milliseconds(offset.max(0) as i64);
        crate::targets::format_jst(&now)
    }

    /// 時刻注入seam(testability。AuthStateと同一流儀): 現在時刻への加算offsetを
    /// 設定しworkerを起こす(waiting_restoreの確認時刻経過を短縮検証できる)。
    #[cfg(test)]
    fn set_time_offset_ms(&self, offset_ms: i64) {
        self.time_offset_ms
            .store(offset_ms, std::sync::atomic::Ordering::SeqCst);
        self.notify_worker();
    }

    /// 起動処理(設計5.4): server再起動時にstatusがqueued/running/waiting_restoreの
    /// 行をinterruptedへ一括更新する(自動再開しない。get/pushとも冪等設計のため
    /// 再発行で安全に回復する=UIに再実行案内)。run_serverの起動処理(run_blocking
    /// 経由)から呼ぶ・unit testからは直接呼べる同期関数。
    fn interrupt_jobs_at_boot(scope: &(String, String)) -> BackupResult<usize> {
        let db = crate::catalog::CatalogDb::open()?;
        db.interrupt_active_jobs(&scope.0, &scope.1)
    }

    /// worker本体(直列実行・同時1)。run_serverからspawnする。
    async fn worker_loop(
        self: std::sync::Arc<Self>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        loop {
            if *shutdown.borrow() {
                eprintln!("serve: job workerを停止します(graceful shutdown)");
                break;
            }
            // 1. queued jobを1件実行(設計5.1: queued順に1件ずつ)。semaphoreを
            //    acquireしてからrunningへ遷移する(3.1: 実行時にsemaphoreをacquire
            //    =書込APIとの相互直列化。acquire待ちの間はqueuedのまま)。
            if let Some(job) = self.next_queued().await {
                self.run_job(job).await;
                continue;
            }
            // 2. 確認時刻を過ぎたwaiting_restore jobを確認(読取・semaphore外。
            //    復元済みなら再acquireして取得を再開する)
            if self.check_waiting_restore().await {
                continue;
            }
            // 3. 待機(新規job通知・tick・shutdownのいずれかで起床)
            tokio::select! {
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(JOB_WORKER_TICK_MS)) => {}
                _ = shutdown.changed() => {}
            }
        }
    }

    /// 次のqueued jobの読み出し(DB読取失敗はlogのみで待機へ戻る=worker継続)。
    async fn next_queued(&self) -> Option<crate::catalog::JobRow> {
        let (bucket, prefix) = self.scope.clone();
        run_blocking(move || {
            let db = crate::catalog::CatalogDb::open()?;
            db.next_queued_job(&bucket, &prefix)
        })
        .await
        .ok()
        .flatten()
    }

    /// 1 jobの実行(permitをclosureへmove=stepのreturnで解放。terminalは結果を
    /// 確定し・waiting_restoreはstatus遷移のみ行う)。
    async fn run_job(self: &std::sync::Arc<Self>, job: crate::catalog::JobRow) {
        let params_value: serde_json::Value =
            serde_json::from_str(&job.params_json).unwrap_or(serde_json::Value::Null);
        let Some(params) = GetJobParams::from_json(&params_value) else {
            self.fail_job(
                job.id,
                "internal",
                "jobの実行paramsが読み取れないため失敗しました",
            )
            .await;
            return;
        };
        // semaphore acquire(書込APIと相互直列化)。失敗(worker数0でclose無し)は
        // 内部errorとしてjob失敗にしない=再試行の機会を残すためqueuedのまま戻る
        let Ok(permit) = self.permits.clone().acquire_owned().await else {
            eprintln!("serve: job semaphoreのacquireに失敗(logのみ): id={}", job.id);
            return;
        };
        // runningへ遷移(phase=resolve開始)。遷移はstatus='queued'の条件付き
        // claimで行う: acquire待ちの間にcancelされたjob(取消は200で確定済み)
        // をrunningへ戻して実行しない。claim不成立(取消先行・DB失敗)は実行
        // せずに戻る(jobがqueuedのままなら次回loopで再試行される)
        if !self
            .claim_queued_job(job.id, &job_progress_json("resolve", false, None, None))
            .await
        {
            eprintln!(
                "serve: jobのclaimが不成立のため実行しません: id={}",
                job.id
            );
            return;
        }
        let ctx = self.ctx.clone();
        // permitはclosure内へmove=stepのreturn(終状態・waiting_restore遷移のいず
        // れも)でdropされsemaphoreが解放される(設計5.1: waiting_restore遷移時の解放
        // =復元待ちの最大48hを書込API・他jobの停止にしない)
        let outcome = run_blocking(move || {
            let _held = permit;
            execute_get_step(&ctx, &params)
        })
        .await;
        match outcome {
            Ok(JobStep::Terminal {
                status,
                progress,
                result_json,
                error_code,
                error_message,
            }) => {
                let _ = self
                    .update_job_row(job.id, status, &progress, result_json.as_deref(), error_code, error_message.as_deref())
                    .await;
            }
            Ok(JobStep::WaitingRestore { current_path }) => {
                // waiting_restoreへ遷移・next_check_at=現在時刻+確認間隔。
                // permitはspawn_blocking closureのreturnで既に解放済み(復元待ちの
                // 最大48hを書込API・他jobの停止にしない=設計5.1)
                let next_check_at = self.next_check_at();
                let progress = job_progress_json(
                    "restore",
                    false,
                    Some(&current_path),
                    Some(&next_check_at),
                );
                let _ = self
                    .update_job_row(job.id, "waiting_restore", &progress, None, None, None)
                    .await;
            }
            Err(api_error) => {
                // 実行失敗(job単位で捕捉=設計5.5: status=failed+error code。
                // api_error_from相当の一般化messageはrun_blocking内で付与済み)
                let _ = self
                    .update_job_row(
                        job.id,
                        "failed",
                        &job_progress_json("resolve", false, None, None),
                        None,
                        Some(api_error.code),
                        Some(api_error.message),
                    )
                    .await;
            }
        }
    }

    /// waiting_restore jobの定期確認。戻り値=1件でも処理した(loopを直ちに回す)。
    async fn check_waiting_restore(self: &std::sync::Arc<Self>) -> bool {
        let (bucket, prefix) = self.scope.clone();
        let jobs = match run_blocking(move || {
            let db = crate::catalog::CatalogDb::open()?;
            db.waiting_restore_jobs(&bucket, &prefix)
        })
        .await
        {
            Ok(jobs) => jobs,
            Err(e) => {
                eprintln!("serve: waiting_restore job一覧の読込に失敗(logのみ): {}", e.message);
                return false;
            }
        };
        let now = self.now_jst();
        for job in jobs {
            // progress_jsonのnext_check_at(無し・不正な行は即時確認して回復させる)
            let progress: serde_json::Value =
                serde_json::from_str(&job.progress_json).unwrap_or(serde_json::Value::Null);
            let next_check_at = progress
                .get("next_check_at")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !next_check_at.is_empty() && next_check_at > now.as_str() {
                continue; // まだ確認時刻で無い
            }
            let params_value: serde_json::Value =
                serde_json::from_str(&job.params_json).unwrap_or(serde_json::Value::Null);
            let Some(params) = GetJobParams::from_json(&params_value) else {
                self.fail_job(
                    job.id,
                    "internal",
                    "jobの実行paramsが読み取れないため失敗しました",
                )
                .await;
                return true;
            };
            // 復元確認(読取のみ・semaphore外。設計5.1: timerによるhead-object確認は
            // 読取のためsemaphore不要)。probe側へはparamsのcloneを渡す(復元済み時の
            // 取得再開で元のparamsを続けて使う)
            let ctx = self.ctx.clone();
            let probe_params = params.clone();
            let probe = run_blocking(move || probe_restore(&ctx, &probe_params)).await;
            match probe {
                Ok(true) => {
                    // 復元済み(Restore headerが復元済み・または非archive化):
                    // 再acquireして取得処理を再開(設計5.1)
                    let Ok(permit) = self.permits.clone().acquire_owned().await else {
                        eprintln!(
                            "serve: job semaphoreのacquireに失敗(logのみ): id={}",
                            job.id
                        );
                        return true;
                    };
                    let _ = self
                        .update_job_row(
                            job.id,
                            "running",
                            &job_progress_json("fetch", false, None, None),
                            None,
                            None,
                            None,
                        )
                        .await;
                    let ctx = self.ctx.clone();
                    // permitはclosure内へmove(stepのreturnで解放)
                    let outcome = run_blocking(move || {
                        let _held = permit;
                        execute_get_step(&ctx, &params)
                    })
                    .await;
                    match outcome {
                        Ok(JobStep::Terminal {
                            status,
                            progress,
                            result_json,
                            error_code,
                            error_message,
                        }) => {
                            let _ = self
                                .update_job_row(job.id, status, &progress, result_json.as_deref(), error_code, error_message.as_deref())
                                .await;
                        }
                        Ok(JobStep::WaitingRestore { current_path }) => {
                            // 確認と取得の間に復元が失効する等して再び復元待ちへ
                            let next_check_at = self.next_check_at();
                            let progress = job_progress_json(
                                "restore",
                                false,
                                Some(&current_path),
                                Some(&next_check_at),
                            );
                            let _ = self
                                .update_job_row(
                                    job.id,
                                    "waiting_restore",
                                    &progress,
                                    None,
                                    None,
                                    None,
                                )
                                .await;
                        }
                        Err(api_error) => {
                            let _ = self
                                .update_job_row(
                                    job.id,
                                    "failed",
                                    &job_progress_json("restore", false, None, None),
                                    None,
                                    Some(api_error.code),
                                    Some(api_error.message),
                                )
                                .await;
                        }
                    }
                    return true;
                }
                Ok(false) => {
                    // まだ復元中(次回確認へ。進行のcurrent_pathは前回値を引き継ぐ)
                    let next_check_at = self.next_check_at();
                    let current = progress
                        .get("current_path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let p = job_progress_json(
                        "restore",
                        false,
                        if current.is_empty() { None } else { Some(current) },
                        Some(&next_check_at),
                    );
                    let _ = self
                        .update_job_row(job.id, "waiting_restore", &p, None, None, None)
                        .await;
                }
                Err(e) => {
                    // 転送系失敗(S3 head等)は一時的=jobを失敗させず次回確認で再試行
                    // (15分後に同じheadを打つ)。それ以外(解決失敗等)はjob失敗。
                    if e.code == "upstream" {
                        let next_check_at = self.next_check_at();
                        let current = progress
                            .get("current_path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let p = job_progress_json(
                            "restore",
                            false,
                            if current.is_empty() { None } else { Some(current) },
                            Some(&next_check_at),
                        );
                        let _ = self
                            .update_job_row(job.id, "waiting_restore", &p, None, None, None)
                            .await;
                        eprintln!(
                            "serve: 復元確認が一時的に失敗(次回確認で再試行): id={}: {}",
                            job.id, e.message
                        );
                    } else {
                        let _ = self
                            .update_job_row(
                                job.id,
                                "failed",
                                &job_progress_json("restore", false, None, None),
                                None,
                                Some(e.code),
                                Some(e.message),
                            )
                            .await;
                    }
                    return true;
                }
            }
        }
        false
    }

    /// 次回確認時刻(現在時刻+確認間隔。ISO8601秒精度のため辞書順比較で時刻順)。
    fn next_check_at(&self) -> String {
        let interval_ms = self.restore_check_interval_min * 60_000;
        let offset = self
            .time_offset_ms
            .load(std::sync::atomic::Ordering::SeqCst)
            .max(0);
        let now =
            crate::targets::now_jst() + chrono::Duration::milliseconds(offset as i64 + interval_ms);
        crate::targets::format_jst(&now)
    }

    /// job失敗の確定(params読取不能などの実行前失敗用)。
    async fn fail_job(&self, id: i64, code: &str, message: &str) {
        let _ = self
            .update_job_row(
                id,
                "failed",
                &job_progress_json("resolve", false, None, None),
                None,
                Some(code),
                Some(message),
            )
            .await;
    }

    /// queued jobの実行開始claim(catalog側の条件付き遷移の非同期wrapper)。
    /// 戻り値=falseは取消先行(更新0行)またはDB失敗=いずれも実行しない。
    async fn claim_queued_job(&self, id: i64, progress: &str) -> bool {
        let progress = progress.to_string();
        let result = tokio::task::spawn_blocking(move || {
            let db = crate::catalog::CatalogDb::open()?;
            db.claim_queued_job(id, &progress)
        })
        .await;
        match result {
            Ok(Ok(updated)) => updated > 0,
            Ok(Err(e)) => {
                eprintln!("serve: jobのclaimに失敗(logのみ・実行しない): id={id}: {e}");
                false
            }
            Err(e) => {
                eprintln!("serve: jobのclaimが異常終了(logのみ・実行しない): id={id}: {e}");
                false
            }
        }
    }

    /// job行の更新(DB書込失敗はlogのみ=worker継続)。引数はfn内で所有化して
    /// spawn_blockingへ渡す(borrowの寿命をawaitをまたいで保持しない)。
    async fn update_job_row(
        &self,
        id: i64,
        status: &str,
        progress: &str,
        result_json: Option<&str>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) {
        let status = status.to_string();
        let progress = progress.to_string();
        let result_json = result_json.map(String::from);
        let error_code = error_code.map(String::from);
        let error_message = error_message.map(String::from);
        let result = tokio::task::spawn_blocking(move || {
            let db = crate::catalog::CatalogDb::open()?;
            db.update_job(
                id,
                &status,
                &progress,
                result_json.as_deref(),
                error_code.as_deref(),
                error_message.as_deref(),
            )
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!("serve: job状態のDB反映に失敗(logのみ): id={id}: {e}")
            }
            Err(e) => {
                eprintln!("serve: job状態のDB反映が異常終了(logのみ): id={id}: {e}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TASK-17 W2(TASK-19)読取API: tag/folder/collection(local語彙の語彙API)。
// いずれもscopeは読取API共通(省略時=実効profile・?scope=明示優先)。

/// GET /api/tags(tag一覧`{"tags":[{tag,entity_count}]}`。並びはtag名昇順・
/// entity_countは当該tagを持つ実効scope内のentity数)。
async fn get_tags(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let db = crate::catalog::CatalogDb::open()?;
        let rows = db.tag_counts(&bucket, &prefix)?;
        Ok(serde_json::json!({
            "tags": rows.into_iter().map(|(tag, entity_count)| {
                serde_json::json!({"tag": tag, "entity_count": entity_count})
            }).collect::<Vec<_>>(),
        }))
    })
    .await?;
    Ok(axum::Json(value))
}

/// GET /api/folders(folder=labelの階層集約`{"folders":[{label,path,entity_count}]}`。
/// 各行=labelのdirectory区切り(`/`)で導出した階層node(中間nodeも1行)・path=当該
/// nodeのrootからの完全path(既存label=絞り込みの値と一致=folder絞り込み用の新param
/// を設けない=D-W4)・label=最終区切り要素・entity_count=当該node配下のentity数
/// (子folder配下を含む合計)・並びはpath昇順。ラベルなし(location.label="")の
/// locationを持つentityは先頭行{label:"",path:"",entity_count}(コードレビュー中10:
/// 設計2.2「(ラベルなし)」nodeの基盤。行の選択は既存label=の空文字完全一致と対応)。
async fn get_folders(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        use std::collections::{BTreeMap, BTreeSet};
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let db = crate::catalog::CatalogDb::open()?;
        let (_header, entries) = db.load_entries(&bucket, &prefix)?;
        // 階層集約: entity毎の(空を除く)label集合をancestor pathへ展開し、
        // node pathごとにuuid集合(BTreeSet=同一entityの複数locationは1件と数える)
        let mut nodes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for e in &entries {
            let labels: BTreeSet<&str> = e
                .locations
                .iter()
                .map(|l| l.label.as_str())
                .filter(|l| !l.is_empty())
                .collect();
            for label in labels {
                let segments: Vec<&str> = label.split('/').filter(|s| !s.is_empty()).collect();
                if segments.is_empty() {
                    continue;
                }
                for depth in 1..=segments.len() {
                    let path = segments[..depth].join("/");
                    nodes.entry(path).or_default().insert(e.uuid.clone());
                }
            }
        }
        // ラベルなしnode(コードレビュー中10): label=""のlocationを持つentity集合を
        // path=""の行として加える(BTreeMapの並びで空文字は常に先頭=応答の先頭行。
        // ラベルなしentityが0件時は他nodeと同一の「実在から導出」規則で出さない)
        let unlabelled: BTreeSet<String> = entries
            .iter()
            .filter(|e| e.locations.iter().any(|l| l.label.is_empty()))
            .map(|e| e.uuid.clone())
            .collect();
        if !unlabelled.is_empty() {
            nodes.insert(String::new(), unlabelled);
        }
        // BTreeMapのkeys昇順=path昇順そのもの
        let folders: Vec<serde_json::Value> = nodes
            .into_iter()
            .map(|(path, uuids)| {
                serde_json::json!({
                    "label": path.rsplit('/').next().unwrap_or(&path),
                    "path": path,
                    "entity_count": uuids.len(),
                })
            })
            .collect();
        Ok(serde_json::json!({ "folders": folders }))
    })
    .await?;
    Ok(axum::Json(value))
}

/// GET /api/collections(collection一覧`{"collections":[{id,name,entity_count}]}`。
/// 並びはname昇順・entity_countは構成entity数。対照scopeのcollectionは混入しない)。
async fn get_collections(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let db = crate::catalog::CatalogDb::open()?;
        let rows = db.collection_rows(&bucket, &prefix)?;
        Ok(serde_json::json!({
            "collections": rows.into_iter().map(|c| {
                serde_json::json!({"id": c.id, "name": c.name, "entity_count": c.entity_count})
            }).collect::<Vec<_>>(),
        }))
    })
    .await?;
    Ok(axum::Json(value))
}

/// collection詳細schema(`{"collection":{id,name,uuids}}`・uuidsは追加順)。
fn collection_detail_json(c: &crate::catalog::CollectionDetail) -> serde_json::Value {
    serde_json::json!({"collection": {"id": c.id, "name": c.name, "uuids": c.uuids}})
}

/// path/queryのcollection id parse(数値で無い場合はinvalid_request)。
fn parse_collection_id(raw: &str) -> Result<i64, ApiError> {
    raw.parse::<i64>()
        .map_err(|_| invalid_request("collectionは数値idで指定してください"))
}

/// GET /api/collections/:id(詳細。不在idは404 not_found)。
async fn get_collection_detail(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let id = parse_collection_id(&id)?;
    let effective = state.scope.clone();
    let value = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let db = crate::catalog::CatalogDb::open()?;
        let detail = db
            .collection_detail(&bucket, &prefix, id)?
            .ok_or_else(|| BackupError::Target("対象のcollectionが見つかりません".to_string()))?;
        Ok(collection_detail_json(&detail))
    })
    .await?;
    Ok(axum::Json(value))
}

// ---------------------------------------------------------------------------
// TASK-17 W2(TASK-19)書込API: tag/collection操作。書込はsync/scanと同一の
// single-flight(semaphore 1)で直列化しspawn_blocking経由で実行(設計3.1)。
// 全操作をaudit_logへ記録する(成功・失敗両方・D-W10: UI書込操作のみ)。

/// W2 tag名の規則(設計3.2: `^[a-z0-9][a-z0-9-]{0,63}$`を流用)。
fn valid_tag_name(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first_ok = bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit();
    first_ok
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// entity詳細JSON(W2語彙拡張付き: entry_summary_json(要約+version_list)へtags・
/// collectionsを加えた形式。GET /api/entities/:uuidとPUT tagsの応答に共用=
/// 設計3.1「書込応答は読取系と同一schema」)。
fn entity_detail_with_vocabulary(
    db: &crate::catalog::CatalogDb,
    bucket: &str,
    prefix: &str,
    e: &crate::index::IndexFileEntry,
) -> BackupResult<serde_json::Value> {
    let mut value = crate::storage::query::entry_summary_json(e, true);
    value["tags"] = serde_json::json!(db.entity_tags(bucket, prefix, &e.uuid)?);
    value["collections"] = serde_json::json!(db.collections_of_uuid(bucket, prefix, &e.uuid)?);
    Ok(value)
}

/// audit_logへの失敗audit記録(失敗時のみこの経路を通る。成功auditは
/// audit_ok_on_tx(catalog.rs)が操作tx内で記録するためここには来ない。
/// 記録自体の失敗は操作結果に影響させない=logのみ。秘密情報は含まない
/// 対象特定情報のみ)。
fn audit_outcome(
    db: &crate::catalog::CatalogDb,
    bucket: &str,
    prefix: &str,
    action: &str,
    detail: &serde_json::Value,
    outcome: &str,
    error_code: Option<&str>,
) {
    let detail_json = serde_json::to_string(detail).unwrap_or_default();
    if let Err(e) = db.record_audit(bucket, prefix, action, &detail_json, outcome, error_code) {
        eprintln!("serve: audit_log記録に失敗(logのみ): {e}");
    }
}

/// W2書込flow(spawn_blocking内)の処理結果。成否のHTTP変換はhandler側で行い、
/// audit_log記録は各flowが成否両方で済ませる(内部errorはBackupErrorとして
/// run_blocking経由で500系へ変換されるためこの区分には含めない)。
enum W2Outcome {
    /// 成功(応答body)
    Ok(serde_json::Value),
    /// 同名等の重複(409 duplicate)
    Duplicate,
    /// 対象(entity・collection)不在(404 not_found)
    NotFound,
    /// 入力規則違反(422 validation_failed)
    Validation,
}

impl W2Outcome {
    /// HTTP応答への変換(messageは区分ごとの一般化文言)。
    fn into_json_response(self) -> Result<axum::Json<serde_json::Value>, ApiError> {
        match self {
            W2Outcome::Ok(v) => Ok(axum::Json(v)),
            W2Outcome::Duplicate => Err(duplicate("同名のcollectionが既に存在します")),
            W2Outcome::NotFound => Err(not_found("対象が見つかりません")),
            W2Outcome::Validation => Err(validation_failed("入力値の規則に違反しています")),
        }
    }
}

/// W2書込flow内でのstorage層W2WriteErrorの変換(audit記録付き)。
fn w2_map_write_error(
    db: &crate::catalog::CatalogDb,
    bucket: &str,
    prefix: &str,
    action: &str,
    detail: &serde_json::Value,
    e: crate::catalog::W2WriteError,
) -> Result<W2Outcome, BackupError> {
    match e {
        crate::catalog::W2WriteError::Duplicate => {
            audit_outcome(db, bucket, prefix, action, detail, "failed", Some("duplicate"));
            Ok(W2Outcome::Duplicate)
        }
        crate::catalog::W2WriteError::NotFound => {
            audit_outcome(db, bucket, prefix, action, detail, "failed", Some("not_found"));
            Ok(W2Outcome::NotFound)
        }
        crate::catalog::W2WriteError::Other(err) => {
            audit_outcome(db, bucket, prefix, action, detail, "failed", Some("internal"));
            Err(err)
        }
    }
}

/// 書込APIのsingle-flight(semaphore 1)acquire(sync/scan・他の書込APIと直列化)。
async fn acquire_write_permit(
    state: &AppState,
) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    state
        .sync_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| {
            eprintln!("serve: 書込semaphoreのacquireに失敗(logのみ): {e}");
            internal("内部エラーが発生しました")
        })
}

/// W2書込flow内の読取・内部error(BackupError)の共通経路(コードレビュー中5:
/// storage失敗も監査記録の共通経路へ通す=failed/internal記録後に500系へ変換)。
fn w2_audit_internal(
    db: &crate::catalog::CatalogDb,
    bucket: &str,
    prefix: &str,
    action: &str,
    detail: &serde_json::Value,
    e: BackupError,
) -> BackupError {
    audit_outcome(db, bucket, prefix, action, detail, "failed", Some("internal"));
    e
}

/// PUT /api/entities/:uuid/tags(tag一覧の置換。body {tags:[...]}・冪等・空配列は
/// 全tag削除・body内重複は正規化(set化)して受理=コードレビュー中5。応答は
/// EntityDetail形式(tags・collections付き)。tag名規則違反は422・不在uuidは404。
/// 操作と成功audit記録は同一transactionで確定(コードレビュー中4)。
async fn put_entity_tags(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
    body: axum::body::Bytes,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    let Some(raw_tags) = parsed.get("tags").and_then(|t| t.as_array()) else {
        return Err(invalid_request("tagsは文字列の配列で指定してください"));
    };
    let mut tags: Vec<String> = Vec::with_capacity(raw_tags.len());
    for t in raw_tags {
        let Some(tag) = t.as_str() else {
            return Err(invalid_request("tagsは文字列の配列で指定してください"));
        };
        tags.push(tag.to_string());
    }
    // body内重複は「一覧の置換」semanticsで同一tagのため正規化して受理(中5)
    let mut seen = std::collections::HashSet::new();
    tags.retain(|t| seen.insert(t.clone()));
    let _permit = acquire_write_permit(&state).await?;
    let effective = state.scope.clone();
    let outcome = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let mut db = crate::catalog::CatalogDb::open()?;
        let detail = serde_json::json!({"uuid": uuid, "tags": tags});
        let detail_json = serde_json::to_string(&detail).unwrap_or_default();
        let mut flow = || -> Result<W2Outcome, BackupError> {
            // tag名規則(422)
            if tags.iter().any(|t| !valid_tag_name(t)) {
                audit_outcome(
                    &db, &bucket, &prefix, "tags_replace", &detail, "failed",
                    Some("validation_failed"),
                );
                return Ok(W2Outcome::Validation);
            }
            // 対象entityは実効scopeのcacheに実在すること(404)。読取失敗も監査経路へ
            let (_header, entries) = match db.load_entries(&bucket, &prefix) {
                Ok(v) => v,
                Err(e) => {
                    return Err(w2_audit_internal(
                        &db, &bucket, &prefix, "tags_replace", &detail, e,
                    ));
                }
            };
            let Some(e) = entries.iter().find(|e| e.uuid == uuid).cloned() else {
                audit_outcome(
                    &db, &bucket, &prefix, "tags_replace", &detail, "failed",
                    Some("not_found"),
                );
                return Ok(W2Outcome::NotFound);
            };
            // 置換+成功auditは同一transaction(中4)・storage失敗も監査経路(中5)
            match db.replace_entity_tags(
                &bucket, &prefix, &uuid, &tags, "tags_replace", &detail_json,
            ) {
                Ok(()) => Ok(W2Outcome::Ok(entity_detail_with_vocabulary(
                    &db, &bucket, &prefix, &e,
                )?)),
                Err(err) => w2_map_write_error(
                    &db, &bucket, &prefix, "tags_replace", &detail, err,
                ),
            }
        };
        flow()
    })
    .await?;
    outcome.into_json_response()
}

/// POST /api/collections(collection作成。body {name}・応答は詳細schema。空nameは
/// 422・同(scope,name)は409 duplicate。action=collection_create)。
async fn post_collection(
    axum::extract::State(state): axum::extract::State<AppState>,
    body: axum::body::Bytes,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let name = parse_collection_name_body(&body)?;
    let _permit = acquire_write_permit(&state).await?;
    let effective = state.scope.clone();
    let outcome = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let mut db = crate::catalog::CatalogDb::open()?;
        let detail = serde_json::json!({"name": name});
        let detail_json = serde_json::to_string(&detail).unwrap_or_default();
        let mut flow = || -> Result<W2Outcome, BackupError> {
            if name.is_empty() {
                audit_outcome(
                    &db, &bucket, &prefix, "collection_create", &detail, "failed",
                    Some("validation_failed"),
                );
                return Ok(W2Outcome::Validation);
            }
            // 作成+成功auditは同一transaction(コードレビュー中4)
            match db.create_collection(&bucket, &prefix, &name, "collection_create", &detail_json)
            {
                Ok(created) => Ok(W2Outcome::Ok(collection_detail_json(&created))),
                Err(e) => w2_map_write_error(
                    &db, &bucket, &prefix, "collection_create", &detail, e,
                ),
            }
        };
        flow()
    })
    .await?;
    outcome.into_json_response()
}

/// collection名request bodyのparse({name: string})。
fn parse_collection_name_body(body: &axum::body::Bytes) -> Result<String, ApiError> {
    let parsed: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    parsed
        .get("name")
        .and_then(|n| n.as_str())
        .map(String::from)
        .ok_or_else(|| invalid_request("nameは文字列で指定してください"))
}

/// PUT /api/collections/:id(改名。body {name}・応答は詳細schema・id不変。空nameは
/// 422・改名先の同名は409・不在idは404。action=collection_rename)。
async fn put_collection(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: axum::body::Bytes,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let id = parse_collection_id(&id)?;
    let name = parse_collection_name_body(&body)?;
    let _permit = acquire_write_permit(&state).await?;
    let effective = state.scope.clone();
    let outcome = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let mut db = crate::catalog::CatalogDb::open()?;
        let detail = serde_json::json!({"id": id, "name": name});
        let detail_json = serde_json::to_string(&detail).unwrap_or_default();
        let mut flow = || -> Result<W2Outcome, BackupError> {
            if name.is_empty() {
                audit_outcome(
                    &db, &bucket, &prefix, "collection_rename", &detail, "failed",
                    Some("validation_failed"),
                );
                return Ok(W2Outcome::Validation);
            }
            // 改名+成功auditは同一transaction(中4)・応答uuidsは操作後の実際の構成(高2)
            match db.rename_collection(&bucket, &prefix, id, &name, "collection_rename", &detail_json)
            {
                Ok(renamed) => Ok(W2Outcome::Ok(collection_detail_json(&renamed))),
                Err(e) => w2_map_write_error(
                    &db, &bucket, &prefix, "collection_rename", &detail, e,
                ),
            }
        };
        flow()
    })
    .await?;
    outcome.into_json_response()
}

/// DELETE /api/collections/:id(削除=構成参照の解除のみ。collection_itemsはFK
/// CASCADEで消え・entity側には影響しない。応答は{"status":"ok"}。不在idは404。
/// action=collection_delete)。
async fn delete_collection(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let id = parse_collection_id(&id)?;
    let _permit = acquire_write_permit(&state).await?;
    let effective = state.scope.clone();
    let outcome = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let mut db = crate::catalog::CatalogDb::open()?;
        let mut flow = || -> Result<W2Outcome, BackupError> {
            // audit detail用の現name読取(読取失敗も監査経路=コードレビュー中5)
            let existing = match db.collection_detail(&bucket, &prefix, id) {
                Ok(v) => v,
                Err(e) => {
                    return Err(w2_audit_internal(
                        &db,
                        &bucket,
                        &prefix,
                        "collection_delete",
                        &serde_json::json!({"id": id}),
                        e,
                    ));
                }
            };
            let detail = match &existing {
                Some(c) => serde_json::json!({"id": id, "name": c.name}),
                None => serde_json::json!({"id": id}),
            };
            let detail_json = serde_json::to_string(&detail).unwrap_or_default();
            // 削除+成功auditは同一transaction(コードレビュー中4)
            match db.delete_collection(&bucket, &prefix, id, "collection_delete", &detail_json) {
                Ok(()) => Ok(W2Outcome::Ok(serde_json::json!({"status": "ok"}))),
                Err(e) => w2_map_write_error(
                    &db, &bucket, &prefix, "collection_delete", &detail, e,
                ),
            }
        };
        flow()
    })
    .await?;
    outcome.into_json_response()
}

/// POST /api/collections/:id/items(構成entityの一括追加。body {uuids:[...]}・応答は
/// 詳細schema・uuidsは追加順)。不在collection・catalogに無いuuidは404・追加済みuuid
/// の再追加は冪等に成功。action=collection_items_add)。
async fn post_collection_items(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: axum::body::Bytes,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let id = parse_collection_id(&id)?;
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    let Some(raw_uuids) = parsed.get("uuids").and_then(|u| u.as_array()) else {
        return Err(invalid_request("uuidsは文字列の配列で指定してください"));
    };
    let mut uuids: Vec<String> = Vec::with_capacity(raw_uuids.len());
    for u in raw_uuids {
        let Some(uuid) = u.as_str() else {
            return Err(invalid_request("uuidsは文字列の配列で指定してください"));
        };
        uuids.push(uuid.to_string());
    }
    let _permit = acquire_write_permit(&state).await?;
    let effective = state.scope.clone();
    let outcome = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let mut db = crate::catalog::CatalogDb::open()?;
        let detail_base = serde_json::json!({"id": id, "uuids": uuids});
        let mut flow = || -> Result<W2Outcome, BackupError> {
            // audit detail用の現name読取(不在時は後段のNotFoundに任せる・読取失敗も
            // 監査経路=コードレビュー中5)
            let name = match db.collection_detail(&bucket, &prefix, id) {
                Ok(v) => v.map(|c| c.name),
                Err(e) => {
                    return Err(w2_audit_internal(
                        &db,
                        &bucket,
                        &prefix,
                        "collection_items_add",
                        &detail_base,
                        e,
                    ));
                }
            };
            let detail = match &name {
                Some(n) => serde_json::json!({"id": id, "name": n, "uuids": uuids}),
                None => detail_base.clone(),
            };
            let detail_json = serde_json::to_string(&detail).unwrap_or_default();
            // 追加+成功auditは同一transaction(コードレビュー中4)
            match db.add_collection_items(
                &bucket, &prefix, id, &uuids, "collection_items_add", &detail_json,
            ) {
                Ok(added) => Ok(W2Outcome::Ok(collection_detail_json(&added))),
                Err(e) => w2_map_write_error(
                    &db, &bucket, &prefix, "collection_items_add", &detail, e,
                ),
            }
        };
        flow()
    })
    .await?;
    outcome.into_json_response()
}

/// DELETE /api/collections/:id/items/:uuid(構成からの除去。応答は詳細schema。
/// 非構成uuid・不在collectionは404。action=collection_items_remove)。
async fn delete_collection_item(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path((id, uuid)): axum::extract::Path<(String, String)>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let id = parse_collection_id(&id)?;
    let _permit = acquire_write_permit(&state).await?;
    let effective = state.scope.clone();
    let outcome = run_blocking(move || {
        let (bucket, prefix) = resolve_scope(None, &effective)?;
        let mut db = crate::catalog::CatalogDb::open()?;
        let detail_base = serde_json::json!({"id": id, "uuid": uuid});
        let mut flow = || -> Result<W2Outcome, BackupError> {
            // audit detail用の現name読取(不在時は後段のNotFoundに任せる・読取失敗も
            // 監査経路=コードレビュー中5)
            let name = match db.collection_detail(&bucket, &prefix, id) {
                Ok(v) => v.map(|c| c.name),
                Err(e) => {
                    return Err(w2_audit_internal(
                        &db,
                        &bucket,
                        &prefix,
                        "collection_items_remove",
                        &detail_base,
                        e,
                    ));
                }
            };
            let detail = match &name {
                Some(n) => serde_json::json!({"id": id, "name": n, "uuid": uuid}),
                None => detail_base.clone(),
            };
            let detail_json = serde_json::to_string(&detail).unwrap_or_default();
            // 除去+成功auditは同一transaction(コードレビュー中4)
            match db.remove_collection_item(
                &bucket, &prefix, id, &uuid, "collection_items_remove", &detail_json,
            ) {
                Ok(removed) => Ok(W2Outcome::Ok(collection_detail_json(&removed))),
                Err(e) => w2_map_write_error(
                    &db, &bucket, &prefix, "collection_items_remove", &detail, e,
                ),
            }
        };
        flow()
    })
    .await?;
    outcome.into_json_response()
}

// ---------------------------------------------------------------------------
// TASK-17 W3(TASK-22)書込API: title変更(rename)・所在の分類変更(location)。
// W2書込APIと同一のsingle-flight(semaphore 1)で直列化しspawn_blocking経由で実行。
// 書込の真正はregistry.jsonlへのevent追記+索引全体再構築+PUT+DB反映(設計4.2 D-W2)。
// 成功auditは操作完了後の記録(W2の同一tx方式と異なる=registry append・S3 PUT・DB反映
// をまたぐ単一transactionが存在しないため。記録失敗はlogのみで操作結果に影響しない)。

/// W3書込flowの対象entity導出: registry由来の現行state(build_entries結果)からuuidで
/// 引き当てる。対象uuidが無ければNone(404)。ローカルregistryが知らないentityの
/// rename/locationは索引再構築で当該entityを消失させるため、cacheでなくregistry由来で
/// 存在判定する(設計4.2: registry.jsonlが一次記録)。
fn w3_current_entries(
    session: &crate::storage::Session,
    registry_path: &Path,
) -> BackupResult<Vec<crate::index::IndexFileEntry>> {
    let records = crate::registry::load_registry(registry_path)?;
    let reduced = crate::registry::reduce_records(&records);
    Ok(crate::index::build_entries(
        &reduced,
        &session.aws.bucket,
        &session.canonical_bucket,
    ))
}

/// 操作後の対象entity詳細JSON(索引再構築・DB反映後のcacheから引き当て。
/// EntityDetail形式+tags/collections。失敗は呼び出し側のinternal error経路)。
fn w3_entity_detail_json(
    db: &crate::catalog::CatalogDb,
    bucket: &str,
    prefix: &str,
    uuid: &str,
) -> BackupResult<serde_json::Value> {
    let (_header, entries) = db.load_entries(bucket, prefix)?;
    let Some(e) = entries.iter().find(|e| e.uuid == uuid) else {
        return Err(BackupError::Internal(format!(
            "rename/location後の索引に対象entity({uuid})が無い"
        )));
    };
    entity_detail_with_vocabulary(db, bucket, prefix, e)
}

/// POST /api/entities/:uuid/rename(title変更。body {title}。応答はEntityDetail形式
/// (+title重複時のみwarning=CLI warn_if_title_duplicateと同一文言)。title規則
/// 違反は422・対象uuidがregistry由来の現行stateに無ければ404・同一titleは冪等成功。
/// RenameRecord追記→rebuild_and_upload_index(索引再構築+S3 PUT+DB反映)→
/// metadata sync(新titleキーPUT+旧titleキー掃除)=設計4.2の順。action=entity_rename)
async fn post_entity_rename(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
    body: axum::body::Bytes,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    let Some(title) = parsed.get("title").and_then(|t| t.as_str()).map(String::from)
    else {
        return Err(invalid_request("titleは文字列で指定してください"));
    };
    let _permit = acquire_write_permit(&state).await?;
    let ctx = state.sync_ctx.clone();
    let outcome = run_blocking(move || {
        let session = crate::storage::open_session(&ctx)?;
        let cfg = session
            .cfg
            .as_ref()
            .ok_or_else(|| BackupError::Config("pipeline.tomlが必要です".to_string()))?;
        let registry_path = cfg.registry_path.clone();
        let (bucket, prefix) = (session.aws.bucket.clone(), session.aws.prefix.clone());
        let db = crate::catalog::CatalogDb::open()?;
        let detail = serde_json::json!({"uuid": uuid, "title": title});
        let flow = || -> Result<W2Outcome, BackupError> {
            // title規則(422)
            if let Err(e) = crate::layout::validate_title(&title) {
                eprintln!("serve: renameのtitle規則違反: {e}");
                audit_outcome(
                    &db, &bucket, &prefix, "entity_rename", &detail, "failed",
                    Some("validation_failed"),
                );
                return Ok(W2Outcome::Validation);
            }
            // 対象はregistry由来の現行stateに存在すること(404)
            let entries = match w3_current_entries(&session, &registry_path) {
                Ok(v) => v,
                Err(e) => {
                    return Err(w2_audit_internal(
                        &db, &bucket, &prefix, "entity_rename", &detail, e,
                    ));
                }
            };
            let Some(entry) = entries.iter().find(|e| e.uuid == uuid).cloned() else {
                audit_outcome(
                    &db, &bucket, &prefix, "entity_rename", &detail, "failed",
                    Some("not_found"),
                );
                return Ok(W2Outcome::NotFound);
            };
            // title重複は許容+警告応答(CLI仕様と同一。他entityの現行titleとの比較)
            let warning = if entries
                .iter()
                .any(|e| e.uuid != uuid && e.title == title)
            {
                Some(format!(
                    "title({title})は既存entityと重複しています(許容されます。一意化はuuid/所在指定で行ってください)"
                ))
            } else {
                None
            };
            // RenameRecord追記(真正記録)→索引全体再構築+PUT+DB反映→metadata sync
            let record = crate::registry::RenameRecord {
                schema: 1,
                record_type: "rename".into(),
                recorded_at: crate::storage::now_jst(),
                scope: format!("{bucket}:{prefix}"),
                uuid: uuid.clone(),
                old_title: entry.title.clone(),
                new_title: title.clone(),
            };
            if let Err(e) = crate::registry::append_record(&registry_path, &record) {
                return Err(w2_audit_internal(
                    &db, &bucket, &prefix, "entity_rename", &detail, e,
                ));
            }
            if let Err(e) = crate::storage::pin::rebuild_and_upload_index(
                &session,
                &registry_path,
                "rename",
            ) {
                return Err(w2_audit_internal(
                    &db, &bucket, &prefix, "entity_rename", &detail, e,
                ));
            }
            // rename時はmetadata syncを別呼出(rebuild_and_upload_indexはmetadata syncを
            // 含まない=設計4.2。旧titleキー掃除を含む)
            let records = crate::registry::load_registry(&registry_path)?;
            let reduced = crate::registry::reduce_records(&records);
            if let Err(e) = crate::metadata::sync_entity_metadata(
                session.backend.as_ref(),
                &session.aws,
                &session.canonical_bucket,
                &cfg,
                &reduced,
                &uuid,
            ) {
                return Err(w2_audit_internal(
                    &db, &bucket, &prefix, "entity_rename", &detail, e,
                ));
            }
            // 応答は操作後の対象状態(EntityDetail形式+warning)
            let mut value = match w3_entity_detail_json(&db, &bucket, &prefix, &uuid) {
                Ok(v) => v,
                Err(e) => {
                    return Err(w2_audit_internal(
                        &db, &bucket, &prefix, "entity_rename", &detail, e,
                    ));
                }
            };
            if let Some(w) = warning {
                value["warning"] = serde_json::json!(w);
            }
            // 成功audit(操作完了後の記録。失敗はlogのみ)
            let detail_json = serde_json::to_string(&detail).unwrap_or_default();
            if let Err(e) = db.record_audit(&bucket, &prefix, "entity_rename", &detail_json, "ok", None)
            {
                eprintln!("serve: audit_log記録に失敗(logのみ): {e}");
            }
            Ok(W2Outcome::Ok(value))
        };
        flow()
    })
    .await?;
    outcome.into_json_response()
}

/// POST /api/entities/:uuid/location(所在の分類変更。body {source_path, category, label}。
/// 対象locationは(uuid, source_path)完全一致で特定し、不一致・不在uuidは404。
/// category名規則・label正規化規則違反は422。LocationUpdateRecord追記→索引再構築+PUT+
/// DB反映(metadata sync無し=所在変更はmetadataの内容に影響しない)。action=
/// entity_location_update)
async fn post_entity_location(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Path(uuid): axum::extract::Path<String>,
    body: axum::body::Bytes,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let parsed: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| invalid_request("リクエストの形式が不正です"))?;
    let field = |name: &str| -> Result<String, ApiError> {
        parsed
            .get(name)
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| {
                let msg = match name {
                    "source_path" => "source_pathは文字列で指定してください",
                    "category" => "categoryは文字列で指定してください",
                    _ => "labelは文字列で指定してください",
                };
                invalid_request(msg)
            })
    };
    let source_path = field("source_path")?;
    let category = field("category")?;
    let label = field("label")?;
    let _permit = acquire_write_permit(&state).await?;
    let ctx = state.sync_ctx.clone();
    let outcome = run_blocking(move || {
        let session = crate::storage::open_session(&ctx)?;
        let cfg = session
            .cfg
            .as_ref()
            .ok_or_else(|| BackupError::Config("pipeline.tomlが必要です".to_string()))?;
        let registry_path = cfg.registry_path.clone();
        let (bucket, prefix) = (session.aws.bucket.clone(), session.aws.prefix.clone());
        let db = crate::catalog::CatalogDb::open()?;
        let detail = serde_json::json!({
            "uuid": uuid,
            "source_path": source_path,
            "category": category,
            "label": label,
        });
        let flow = || -> Result<W2Outcome, BackupError> {
            // 分類の規則検査(422): category名規則+label正規化(空=ラベルなしは正当)
            if let Err(e) = crate::layout::validate_category_name(&category) {
                eprintln!("serve: location変更のcategory規則違反: {e}");
                audit_outcome(
                    &db, &bucket, &prefix, "entity_location_update", &detail, "failed",
                    Some("validation_failed"),
                );
                return Ok(W2Outcome::Validation);
            }
            let normalized_label = match crate::layout::normalize_label(&label) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("serve: location変更のlabel規則違反: {e}");
                    audit_outcome(
                        &db, &bucket, &prefix, "entity_location_update", &detail, "failed",
                        Some("validation_failed"),
                    );
                    return Ok(W2Outcome::Validation);
                }
            };
            // 対象はregistry由来の現行stateに存在すること+所在は完全一致で特定(404)
            let entries = match w3_current_entries(&session, &registry_path) {
                Ok(v) => v,
                Err(e) => {
                    return Err(w2_audit_internal(
                        &db, &bucket, &prefix, "entity_location_update", &detail, e,
                    ));
                }
            };
            let Some(entry) = entries.iter().find(|e| e.uuid == uuid) else {
                audit_outcome(
                    &db, &bucket, &prefix, "entity_location_update", &detail, "failed",
                    Some("not_found"),
                );
                return Ok(W2Outcome::NotFound);
            };
            let Some(loc) = entry.locations.iter().find(|l| l.source_path == source_path)
            else {
                audit_outcome(
                    &db, &bucket, &prefix, "entity_location_update", &detail, "failed",
                    Some("not_found"),
                );
                return Ok(W2Outcome::NotFound);
            };
            // LocationUpdateRecord追記(真正記録)→索引全体再構築+PUT+DB反映
            let record = crate::registry::LocationUpdateRecord {
                schema: 1,
                record_type: "location_update".into(),
                recorded_at: crate::storage::now_jst(),
                scope: format!("{bucket}:{prefix}"),
                uuid: uuid.clone(),
                source_path: source_path.clone(),
                old_category: loc.category.clone(),
                old_label: loc.label.clone(),
                new_category: category.clone(),
                new_label: normalized_label,
            };
            if let Err(e) = crate::registry::append_record(&registry_path, &record) {
                return Err(w2_audit_internal(
                    &db, &bucket, &prefix, "entity_location_update", &detail, e,
                ));
            }
            if let Err(e) = crate::storage::pin::rebuild_and_upload_index(
                &session,
                &registry_path,
                "location-update",
            ) {
                return Err(w2_audit_internal(
                    &db, &bucket, &prefix, "entity_location_update", &detail, e,
                ));
            }
            // 応答は操作後の対象状態(EntityDetail形式)
            let value = match w3_entity_detail_json(&db, &bucket, &prefix, &uuid) {
                Ok(v) => v,
                Err(e) => {
                    return Err(w2_audit_internal(
                        &db, &bucket, &prefix, "entity_location_update", &detail, e,
                    ));
                }
            };
            let detail_json = serde_json::to_string(&detail).unwrap_or_default();
            if let Err(e) = db.record_audit(
                &bucket,
                &prefix,
                "entity_location_update",
                &detail_json,
                "ok",
                None,
            ) {
                eprintln!("serve: audit_log記録に失敗(logのみ): {e}");
            }
            Ok(W2Outcome::Ok(value))
        };
        flow()
    })
    .await?;
    outcome.into_json_response()
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
        // W2契約統一: summary行はtags: string[](昇順)を含む(未tag付け時は空配列)
        assert!(
            first.get("tags").is_some_and(|t| t.as_array() == Some(&Vec::new())),
            "summary行のtagsは空配列で常に存在: {first}"
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
        // W2契約統一: 詳細はtags(昇順)・collections(所属collection名一覧)を含む
        assert_eq!(
            body.get("tags").and_then(|t| t.as_array()),
            Some(&Vec::new()),
            "詳細はtags(未tag付け時は空配列)を含む: {body}"
        );
        assert_eq!(
            body.get("collections").and_then(|c| c.as_array()),
            Some(&Vec::new()),
            "詳細はcollections(未所属時は空配列)を含む: {body}"
        );

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

    // -----------------------------------------------------------------------
    // TASK-17 W1(TASK-18: Web UI全体設計9章W1行)のruns/jobs API handler単体
    // test(テスト先行・2026-11-14)。実装前のため/api/runs・/api/jobsのroutingが
    // 無くfallback 404で実行failを許容(compileは通る。W1実装で緑化)。

    /// runs集約観測用: api_entryの単一versionへbatch_id・date・version_tsを上書き。
    fn api_entry_with_batch(
        uuid: &str,
        title: &str,
        category: &str,
        label: &str,
        batch_id: &str,
        date: &str,
        version_ts: &str,
    ) -> crate::index::IndexFileEntry {
        let mut entry = api_entry(uuid, title, category, label);
        let v = entry.versions.first_mut().expect("api_entryは1版を含む");
        v.batch_id = batch_id.into();
        v.date = date.into();
        v.version_ts = version_ts.into();
        entry
    }

    /// [covers:server.runs-list]
    /// GET /api/runsはversionsのbatch_id集約run一覧を{"runs":[{batch_id,date,
    /// entity_count}]}・date降順(直近run先頭)で返す。scopeは読取API共通(省略時
    /// 実効scope・明示で対照scope分。他scopeのrunは混入しない)。
    #[test]
    fn api_runs_list_aggregates_versions_by_batch_id() {
        let env = api_env("srv-api-runs");
        // 実効scope: run-1(2026-11-05)=alpha単独・run-2(2026-11-06)=beta+deltaの2entity
        let alpha = api_entry_with_batch(
            "11111111-1111-1111-1111-111111111111",
            "alpha",
            "docs",
            "work",
            "20261105-abcdef01",
            "2026-11-05",
            "2026-11-05T10:00:00+09:00",
        );
        let beta = api_entry_with_batch(
            "22222222-2222-2222-2222-222222222222",
            "beta",
            "unfiled",
            "",
            "20261106-abcdef02",
            "2026-11-06",
            "2026-11-06T10:00:00+09:00",
        );
        let delta = api_entry_with_batch(
            "44444444-4444-4444-4444-444444444444",
            "delta",
            "docs",
            "work",
            "20261106-abcdef02",
            "2026-11-06",
            "2026-11-06T11:00:00+09:00",
        );
        api_seed(&[alpha, beta, delta], API_TEST_BUCKET, API_TEST_PREFIX);
        // 対照scope(実効profile外): run-3(2026-11-07)=gamma
        let gamma = api_entry_with_batch(
            "33333333-3333-3333-3333-333333333333",
            "gamma",
            "docs",
            "work",
            "20261107-abcdef03",
            "2026-11-07",
            "2026-11-07T10:00:00+09:00",
        );
        api_seed(&[gamma], API_OTHER_BUCKET, API_TEST_PREFIX);

        // 実効scopeのみ(run-3は混入しない)・date降順で直近runが先頭
        let (status, body) = api("GET", "/api/runs");
        assert_eq!(status, 200, "GET /api/runsは200: {body}");
        let runs = body
            .get("runs")
            .and_then(|v| v.as_array())
            .expect("runs配列");
        assert_eq!(runs.len(), 2, "実効scopeの2run: {body}");
        let first = &runs[0];
        assert_eq!(
            first.get("batch_id").and_then(|b| b.as_str()),
            Some("20261106-abcdef02"),
            "date降順で直近run(run-2)が先頭: {body}"
        );
        assert_eq!(
            first.get("date").and_then(|d| d.as_str()),
            Some("2026-11-06"),
            "runの日時(=run内version日時の最新): {body}"
        );
        assert_eq!(
            first.get("entity_count").and_then(|c| c.as_i64()),
            Some(2),
            "run-2の対象実体数はbeta+deltaの2: {body}"
        );
        let second = &runs[1];
        assert_eq!(
            second.get("batch_id").and_then(|b| b.as_str()),
            Some("20261105-abcdef01"),
            "2番目は古いrun(run-1): {body}"
        );
        assert_eq!(
            second.get("entity_count").and_then(|c| c.as_i64()),
            Some(1),
            "run-1の対象実体数はalpha単独: {body}"
        );

        // scope明示(対照scopeを明示すればgammaのrunが見える)
        let (_, body) = api("GET", "/api/runs?scope=other-bucket:amazing-backup/");
        let runs = body
            .get("runs")
            .and_then(|v| v.as_array())
            .expect("runs配列");
        assert_eq!(runs.len(), 1, "scope明示はrun-3のみ: {body}");
        assert_eq!(
            runs[0].get("batch_id").and_then(|b| b.as_str()),
            Some("20261107-abcdef03"),
            "対照scopeのrun: {body}"
        );
        drop(env);
    }

    /// [covers:server.runs-list]
    /// 同dateの複数runはrun内の最新version_ts降順(実行順)で並ぶ: batch_idの接尾辞
    /// は時刻のhashのため辞書順は実行順を保証せず、batch_id降順だと同日の古いrunが
    /// 先頭になる(直近run=前回backup表示が誤る)ことを防ぐ。
    #[test]
    fn api_runs_list_orders_same_date_runs_by_execution_order() {
        let env = api_env("srv-api-runs-sameday");
        // 同日(2026-11-06)の2run: run-lateはbatch_id辞書順で小さいが実行は後
        // (version_ts 23:00)・run-earlyは辞書順で大きいが実行は前(10:00)
        let early = api_entry_with_batch(
            "11111111-1111-1111-1111-111111111111",
            "early",
            "docs",
            "work",
            "20261106-zzzzzz99",
            "2026-11-06",
            "2026-11-06T10:00:00+09:00",
        );
        let late = api_entry_with_batch(
            "22222222-2222-2222-2222-222222222222",
            "late",
            "docs",
            "work",
            "20261106-aaaaaa10",
            "2026-11-06",
            "2026-11-06T23:00:00+09:00",
        );
        api_seed(&[early, late], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api("GET", "/api/runs");
        assert_eq!(status, 200, "GET /api/runsは200: {body}");
        let runs = body
            .get("runs")
            .and_then(|v| v.as_array())
            .expect("runs配列");
        assert_eq!(runs.len(), 2, "同日の2run: {body}");
        assert_eq!(
            runs[0].get("batch_id").and_then(|b| b.as_str()),
            Some("20261106-aaaaaa10"),
            "先頭はversion_tsが新しいrun(batch_id降順だと誤ってzzzzzz99が先頭): {body}"
        );
        assert_eq!(
            runs[1].get("batch_id").and_then(|b| b.as_str()),
            Some("20261106-zzzzzz99"),
            "2番目は実行が古いrun: {body}"
        );
        // 応答schemaは不変(version_tsを含まない・date降順+同日内実行順)
        assert_eq!(
            runs[0].as_object().map(|m| m.len()),
            Some(3),
            "応答要素はbatch_id/date/entity_countのみ: {body}"
        );
        drop(env);
    }

    /// [covers:server.jobs-list]
    /// GET /api/jobsはjobs tableから実効scopeの行を応答する(W4実質化。jobが
    /// 1件も無ければ空配列)。並びは進行中(queued/running/waiting_restore)優先・
    /// group内id降順。?status=・?kind=絞り込みが機能し不正値は400 invalid_request。
    /// auth構成routerでの未認証は401(job系APIも同一session gate=設計6章)。
    #[test]
    fn api_jobs_list_responds_from_jobs_table_and_requires_auth() {
        // api_envはAuthFlow::new内で取得する(lock競合防止のため本testでは追加しない)。
        // api()は認証機構未構成のrouterでhandlerを直接観測する(ABHはflowのenvが設定)
        let flow = AuthFlow::new("srv-api-jobs");
        let (status, body) = api("GET", "/api/jobs");
        assert_eq!(status, 200, "GET /api/jobsは200: {body}");
        let jobs = body
            .get("jobs")
            .and_then(|v| v.as_array())
            .expect("jobs配列");
        assert!(jobs.is_empty(), "jobが無ければ空配列: {body}");

        // 保護API: auth構成routerでの未認証は401(job系APIも同一session gate)
        let (status, body, _) = flow.call("GET", "/api/jobs", None, None);
        assert_eq!(status, 401, "未認証のGET /api/jobsは401: {body}");
        expect_error_body(&body, "unauthenticated");

        // job行を注入(発行順: succeeded→running→failed→queued・対照scopeへ1件)して
        // 並び(進行中優先・id降順)・絞り込み・不正値を観測。api_envはflowが既に
        // config_env_lockを保持しているため新たに取らず(flowのABH設定の下で
        // catalog.dbへ直接注入=旧W1 testと同一構成)
        let s1 = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "succeeded", None);
        let r2 = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "running", None);
        let f3 = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "failed", Some("not_found"));
        let q4 = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "queued", None);
        job_seed_row(API_OTHER_BUCKET, API_TEST_PREFIX, "queued", None);

        let (status, body) = api("GET", "/api/jobs");
        assert_eq!(status, 200, "GET /api/jobsは200: {body}");
        let ids: Vec<i64> = body
            .get("jobs")
            .and_then(|v| v.as_array())
            .expect("jobs配列")
            .iter()
            .filter_map(|j| j.get("id").and_then(|i| i.as_i64()))
            .collect();
        assert_eq!(
            ids,
            vec![q4, r2, f3, s1],
            "進行中(queued/running)優先・各group内id降順・対照scopeは混入しない: {body}"
        );

        // status絞り込み(進行中のみへ)
        let (status, body) = api("GET", "/api/jobs?status=running");
        assert_eq!(status, 200, "status=runningは200: {body}");
        let ids: Vec<i64> = body
            .get("jobs")
            .and_then(|v| v.as_array())
            .expect("jobs配列")
            .iter()
            .filter_map(|j| j.get("id").and_then(|i| i.as_i64()))
            .collect();
        assert_eq!(ids, vec![r2], "status絞り込み: {body}");

        // kind絞り込み(全行getのため4件)
        let (status, body) = api("GET", "/api/jobs?kind=get");
        assert_eq!(status, 200, "kind=getは200: {body}");
        assert_eq!(
            body.get("jobs").and_then(|v| v.as_array()).map(|j| j.len()),
            Some(4),
            "kind=getは実効scopeの4件: {body}"
        );

        // 不正値は400 invalid_request
        let (status, body) = api("GET", "/api/jobs?status=bogus");
        assert_eq!(status, 400, "status不正値は400: {body}");
        expect_error_body(&body, "invalid_request");
        let (status, body) = api("GET", "/api/jobs?kind=bogus");
        assert_eq!(status, 400, "kind不正値は400: {body}");
        expect_error_body(&body, "invalid_request");
    }

    // -------------------------------------------------------------------
    // TASK-17 W4(TASK-27)job機構のhandler・JobManager単体test。
    // jobs tableはcatalog.rs migrationで作成される(api_envのcatalog.db openで適用)。

    /// bytesのSHA-256 hex(job実行testのfixture値計算用)。
    fn job_sha256_hex(data: &[u8]) -> String {
        use sha2::Digest;
        let d = sha2::Sha256::digest(data);
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// jobs test用: jobs tableへ1行注入(insert_job+update_jobでstatus指定。
    /// 戻り値=id)。
    fn job_seed_row(bucket: &str, prefix: &str, status: &str, error_code: Option<&str>) -> i64 {
        let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
        let row = db
            .insert_job(
                bucket,
                prefix,
                "get",
                &serde_json::to_string(&serde_json::json!({
                    "uuid": W2_ALPHA_UUID,
                    "version_id": None::<String>,
                    "pin": None::<String>,
                    "out": "/tmp/amazing-backup-job-test-out",
                    "tier": "Standard",
                    "days": 7,
                    "overwrite": false,
                }))
                .expect("params serialize"),
                &job_progress_json("queued", false, None, None),
            )
            .expect("job行挿入");
        if status != "queued" || error_code.is_some() {
            db.update_job(
                row.id,
                status,
                &job_progress_json("resolve", false, Some("amazing-backup/data/x.age"), None),
                None,
                error_code,
                error_code.map(|_| "対象が見つかりません"),
            )
            .expect("job行更新");
        }
        row.id
    }

    /// [covers:server.jobs-get-issue]
    /// POST /api/jobs/get: body検証(必須欠落・型違反400・規則違反422)と
    /// queued job行の即時応答({"job":{...}}。params echo・progress=queued)。
    /// 未認証401(job系APIも同一session gate)。
    #[test]
    fn api_jobs_get_issues_queued_job() {
        let env = api_env("srv-w4-jobs-issue");
        let out = env.home.join("restore-out");
        let body = serde_json::json!({
            "uuid": W2_ALPHA_UUID,
            "out": out.display().to_string(),
        });
        let (status, resp) = api_body("POST", "/api/jobs/get", body);
        assert_eq!(status, 200, "発行は200: {resp}");
        let job = resp.get("job").expect("job object wrap");
        assert_eq!(
            job.get("status").and_then(|s| s.as_str()),
            Some("queued"),
            "即時応答はqueued: {resp}"
        );
        assert_eq!(
            job.get("kind").and_then(|s| s.as_str()),
            Some("get"),
            "kind=get: {resp}"
        );
        assert_eq!(
            job.get("params").and_then(|p| p.get("uuid")).and_then(|u| u.as_str()),
            Some(W2_ALPHA_UUID),
            "paramsは発行bodyのecho: {resp}"
        );
        assert_eq!(
            job.get("params").and_then(|p| p.get("tier")).and_then(|u| u.as_str()),
            Some("Standard"),
            "tier省略既定=Standard: {resp}"
        );
        assert_eq!(
            job.get("params").and_then(|p| p.get("days")).and_then(|u| u.as_i64()),
            Some(7),
            "days省略既定=7: {resp}"
        );
        assert_eq!(
            job.get("params").and_then(|p| p.get("overwrite")).and_then(|u| u.as_bool()),
            Some(false),
            "overwrite省略既定=false: {resp}"
        );
        assert_eq!(
            job.get("progress").and_then(|p| p.get("phase")).and_then(|u| u.as_str()),
            Some("queued"),
            "初期progress.phase=queued: {resp}"
        );
        assert_eq!(
            job.get("progress").and_then(|p| p.get("total")).and_then(|u| u.as_i64()),
            Some(1),
            "progress.total=1(getは対象1件): {resp}"
        );
        assert_eq!(
            job.get("result").and_then(|r| r.as_null()),
            Some(()),
            "発行直後のresultはnull: {resp}"
        );
        // jobs tableへ行が入っている(GET /api/jobsに現れる)
        let (status, list) = api("GET", "/api/jobs");
        assert_eq!(status, 200, "一覧は200: {list}");
        assert_eq!(
            list.get("jobs").and_then(|v| v.as_array()).map(|j| j.len()),
            Some(1),
            "発行したjobが一覧に現れる: {list}"
        );

        // 必須field欠落・型違反は400 invalid_request
        let cases_400: [(serde_json::Value, &str); 5] = [
            (serde_json::json!({"out": "/tmp/x"}), "uuid欠落"),
            (serde_json::json!({"uuid": "u"}), "out欠落"),
            (serde_json::json!({"uuid": 1, "out": "/tmp/x"}), "uuid非string"),
            (serde_json::json!({"uuid": "u", "out": "/tmp/x", "version_id": 5}), "version_id非string"),
            (serde_json::json!({"uuid": "u", "out": "/tmp/x", "overwrite": "yes"}), "overwrite非bool"),
        ];
        for (body, what) in cases_400 {
            let (status, resp) = api_body("POST", "/api/jobs/get", body);
            assert_eq!(status, 400, "{what}は400: {resp}");
            expect_error_body(&resp, "invalid_request");
        }
        // 規則違反は422 validation_failed
        let cases_422: [(serde_json::Value, &str); 4] = [
            (serde_json::json!({"uuid": "", "out": "/tmp/x"}), "空uuid"),
            (serde_json::json!({"uuid": "u", "out": ""}), "空out"),
            (serde_json::json!({"uuid": "u", "out": "/tmp/x", "tier": "Bogus"}), "tier値域外"),
            (serde_json::json!({"uuid": "u", "out": "/tmp/x", "days": 0}), "days値域外"),
        ];
        for (body, what) in cases_422 {
            let (status, resp) = api_body("POST", "/api/jobs/get", body);
            assert_eq!(status, 422, "{what}は422: {resp}");
            expect_error_body(&resp, "validation_failed");
        }
        let (status, resp) = api_body(
            "POST",
            "/api/jobs/get",
            serde_json::json!({"uuid": "u", "out": "/tmp/x", "days": 366}),
        );
        assert_eq!(status, 422, "days=366は422: {resp}");
        expect_error_body(&resp, "validation_failed");

        // 不在JSONは400
        let (status, resp) = api("POST", "/api/jobs/get");
        assert_eq!(status, 400, "body無しは400: {resp}");
        drop(env);

        // 保護API: job系API(発行・詳細・取消)は未認証401
        let flow = AuthFlow::new("srv-w4-jobs-issue-auth");
        let (status, resp, _) = flow.call(
            "POST",
            "/api/jobs/get",
            Some(serde_json::json!({"uuid": "u", "out": "/tmp/x"})),
            None,
        );
        assert_eq!(status, 401, "未認証のPOST /api/jobs/getは401: {resp}");
        expect_error_body(&resp, "unauthenticated");
        let (status, resp, _) = flow.call("GET", "/api/jobs/1", None, None);
        assert_eq!(status, 401, "未認証のGET /api/jobs/:idは401: {resp}");
        expect_error_body(&resp, "unauthenticated");
        let (status, resp, _) = flow.call("POST", "/api/jobs/1/cancel", None, None);
        assert_eq!(status, 401, "未認証のPOST /api/jobs/:id/cancelは401: {resp}");
        expect_error_body(&resp, "unauthenticated");
    }

    /// [covers:server.jobs-detail]
    /// GET /api/jobs/:id: params・progress・result・error付きのjob objectを
    /// {"job":{...}}で応答。不在id・他scopeのidは404 not_found。
    #[test]
    fn api_jobs_detail_response() {
        let env = api_env("srv-w4-jobs-detail");
        // 結果付きのsucceeded行と失敗行を注入
        let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
        let ok_row = db
            .insert_job(
                API_TEST_BUCKET,
                API_TEST_PREFIX,
                "get",
                &serde_json::to_string(&serde_json::json!({
                    "uuid": W2_ALPHA_UUID, "version_id": null, "pin": null,
                    "out": "/tmp/out", "tier": "Standard", "days": 7, "overwrite": false,
                }))
                .expect("params serialize"),
                &job_progress_json("queued", false, None, None),
            )
            .expect("挿入");
        db.update_job(
            ok_row.id,
            "succeeded",
            &job_progress_json("fetch", true, Some("amazing-backup/data/a.age"), None),
            Some(r#"{"name":"alpha","dest":"alpha"}"#),
            None,
            None,
        )
        .expect("更新");
        let fail_row = db
            .insert_job(
                API_TEST_BUCKET,
                API_TEST_PREFIX,
                "get",
                &serde_json::to_string(&serde_json::json!({
                    "uuid": W2_BETA_UUID, "version_id": null, "pin": null,
                    "out": "/tmp/out", "tier": "Bulk", "days": 3, "overwrite": true,
                }))
                .expect("params serialize"),
                &job_progress_json("queued", false, None, None),
            )
            .expect("挿入");
        db.update_job(
            fail_row.id,
            "failed",
            &job_progress_json("resolve", false, None, None),
            None,
            Some("not_found"),
            Some("対象が見つかりません"),
        )
        .expect("更新");

        let (status, resp) = api("GET", &format!("/api/jobs/{}", ok_row.id));
        assert_eq!(status, 200, "詳細は200: {resp}");
        let job = resp.get("job").expect("job object wrap");
        assert_eq!(
            job.get("status").and_then(|s| s.as_str()),
            Some("succeeded"),
            "status: {resp}"
        );
        assert_eq!(
            job.get("result").and_then(|r| r.get("name")).and_then(|n| n.as_str()),
            Some("alpha"),
            "result.name: {resp}"
        );
        assert_eq!(
            job.get("progress").and_then(|p| p.get("done")).and_then(|d| d.as_i64()),
            Some(1),
            "progress.done=1: {resp}"
        );
        assert_eq!(
            job.get("error").and_then(|e| e.as_null()),
            Some(()),
            "成功jobのerrorはnull: {resp}"
        );
        assert!(
            job.get("created_at").is_some_and(|c| c.as_str().is_some())
                && job.get("updated_at").is_some_and(|u| u.as_str().is_some()),
            "created_at/updated_at: {resp}"
        );

        // 失敗jobのerrorは{code,message}
        let (status, resp) = api("GET", &format!("/api/jobs/{}", fail_row.id));
        assert_eq!(status, 200, "失敗job詳細は200: {resp}");
        let error = resp
            .get("job")
            .and_then(|j| j.get("error"))
            .expect("error object");
        assert_eq!(
            error.get("code").and_then(|c| c.as_str()),
            Some("not_found"),
            "error.code: {resp}"
        );
        assert!(
            error.get("message").is_some_and(|m| m.as_str().is_some()),
            "error.message: {resp}"
        );

        // 不在id・他scopeのidは404
        let other = job_seed_row(API_OTHER_BUCKET, API_TEST_PREFIX, "queued", None);
        let (status, resp) = api("GET", "/api/jobs/99999");
        assert_eq!(status, 404, "不在idは404: {resp}");
        expect_error_body(&resp, "not_found");
        let (status, resp) = api("GET", &format!("/api/jobs/{other}"));
        assert_eq!(status, 404, "他scopeのidは404: {resp}");
        expect_error_body(&resp, "not_found");
        // 非数値idは400
        let (status, resp) = api("GET", "/api/jobs/abc");
        assert_eq!(status, 400, "非数値idは400: {resp}");
        expect_error_body(&resp, "invalid_request");
        drop(env);
    }

    /// [covers:server.jobs-cancel]
    /// POST /api/jobs/:id/cancel: queuedはstatus=cancelledで応答(操作後のjob
    /// object)。running・完了系は409 duplicate。不在idは404。
    #[test]
    fn api_jobs_cancel_queued_only() {
        let env = api_env("srv-w4-jobs-cancel");
        let queued = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "queued", None);
        let running = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "running", None);
        let done = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "succeeded", None);

        let (status, resp) = api("POST", &format!("/api/jobs/{queued}/cancel"));
        assert_eq!(status, 200, "queued取消は200: {resp}");
        assert_eq!(
            resp.get("job").and_then(|j| j.get("status")).and_then(|s| s.as_str()),
            Some("cancelled"),
            "取消後のjob object(status=cancelled): {resp}"
        );
        // 取消済みの再取消も409(取消対象が無い)
        let (status, resp) = api("POST", &format!("/api/jobs/{queued}/cancel"));
        assert_eq!(status, 409, "取消済みの再取消は409: {resp}");
        expect_error_body(&resp, "duplicate");

        // 実行中・完了系は409 duplicate
        let (status, resp) = api("POST", &format!("/api/jobs/{running}/cancel"));
        assert_eq!(status, 409, "実行中の取消は409: {resp}");
        expect_error_body(&resp, "duplicate");
        let (status, resp) = api("POST", &format!("/api/jobs/{done}/cancel"));
        assert_eq!(status, 409, "完了jobの取消は409: {resp}");
        expect_error_body(&resp, "duplicate");

        // 不在idは404・非数値idは400
        let (status, resp) = api("POST", "/api/jobs/99999/cancel");
        assert_eq!(status, 404, "不在idは404: {resp}");
        expect_error_body(&resp, "not_found");
        let (status, resp) = api("POST", "/api/jobs/abc/cancel");
        assert_eq!(status, 400, "非数値idは400: {resp}");
        expect_error_body(&resp, "invalid_request");

        // 取消行は一覧に残る(jobs tableは履歴兼用・再実行対象)
        let (status, list) = api("GET", "/api/jobs?status=cancelled");
        assert_eq!(status, 200, "一覧: {list}");
        assert_eq!(
            list.get("jobs").and_then(|v| v.as_array()).map(|j| j.len()),
            Some(1),
            "取消行はcancelled絞り込みに現れる: {list}"
        );
        drop(env);
    }

    /// [covers:server.jobs-interrupted-on-restart]
    /// 起動時の一括interrupted化(設計5.4): queued/running/waiting_restoreは
    /// interruptedへ・完了系は変更しない。自動再開は行わない(本testは状態変更の
    /// み観測。workerが旧queuedを拾わないことの保証はrun_serverの呼出順
    /// (sweep→worker起動)による)。
    #[test]
    fn api_jobs_interrupted_at_boot_sweep() {
        let env = api_env("srv-w4-jobs-interrupt");
        let q = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "queued", None);
        let r = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "running", None);
        let w = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "waiting_restore", None);
        let s = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "succeeded", None);
        let f = job_seed_row(API_TEST_BUCKET, API_TEST_PREFIX, "failed", Some("not_found"));
        job_seed_row(API_OTHER_BUCKET, API_TEST_PREFIX, "queued", None);

        let updated = JobManager::interrupt_jobs_at_boot(&(
            API_TEST_BUCKET.to_string(),
            API_TEST_PREFIX.to_string(),
        ))
        .expect("起動時sweep");
        assert_eq!(updated, 3, "実効scopeの3行(queued/running/waiting_restore)が対象");

        let status_of = |id: i64| {
            let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
            db.job_by_id(API_TEST_BUCKET, API_TEST_PREFIX, id)
                .expect("job読込")
                .expect("行がある")
                .status
        };
        assert_eq!(status_of(q), "interrupted", "queued→interrupted");
        assert_eq!(status_of(r), "interrupted", "running→interrupted");
        assert_eq!(status_of(w), "interrupted", "waiting_restore→interrupted");
        assert_eq!(status_of(s), "succeeded", "succeededは変更しない");
        assert_eq!(status_of(f), "failed", "failedは変更しない");
        // 他scopeのqueuedは実効scopeのsweep対象外
        let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
        let other = db
            .next_queued_job(API_OTHER_BUCKET, API_TEST_PREFIX)
            .expect("読込")
            .expect("他scopeのqueuedは残る");
        assert_eq!(other.status, "queued", "他scopeはsweep対象外");
        drop(env);
    }

    /// W4 job実行test用の鍵・対象object・索引の用意(実ssh鍵pair+実age。
    /// ssh-keygen/ageが使えない環境はNone=呼出側testはskip扱い=restore系結合
    /// testと同一扱い)。
    struct JobExecFixture {
        env: ApiEnv,
        state: AppState,
        router: axum::Router,
        uuid: String,
        title: String,
        s3_key: String,
        plain: Vec<u8>,
        out_dir: std::path::PathBuf,
    }

    /// api_envのconfigを実鍵pairとstorage_classへ書換え・fake S3へ対象objectと
    /// 索引を用意する(索引はregistry由来entriesをupload_indexでPUT=実経路)。
    fn job_exec_setup(tag: &str, storage_class: &str) -> Option<JobExecFixture> {
        let env = api_env(tag);
        // throwaway ssh ed25519鍵pair(recipient=pub・decrypt_key=secret)
        let key_dir = env._tmp.path().join("keys");
        std::fs::create_dir_all(&key_dir).expect("key dir");
        let secret = key_dir.join("jobkey");
        let public = key_dir.join("jobkey.pub");
        let out = std::process::Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-C", "amazing-backup-w4-test", "-f"])
            .arg(&secret)
            .output()
            .expect("ssh-keygen実行");
        if !out.status.success() || !public.is_file() {
            return None; // ssh-keygen不可環境はskip
        }
        // pipeline.toml(recipients=実pub・decrypt_key=実secret)・aws.toml
        // (storage_class)を書換え
        let creds = env._tmp.path().join("aws-credentials.toml");
        crate::testutil::write_file(
            &env.home.join("pipeline.toml"),
            format!(
                "[profiles.dev.recipients]\npublic_keys = [\"{}\"]\n\
                 [profiles.dev.verify]\ndecrypt_key = \"{}\"\n",
                public.display(),
                secret.display()
            )
            .as_bytes(),
        );
        crate::testutil::write_file(
            &env.home.join("aws.toml"),
            format!(
                "[aws]\nactive_profile = \"dev\"\n\
                 [aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\nmpu_abort_days = 7\n\
                 [aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"{API_TEST_BUCKET}\"\n\
                 region = \"ap-northeast-1\"\nprefix = \"{API_TEST_PREFIX}\"\n\
                 storage_class = \"{storage_class}\"\ncredentials_file = \"{}\"\n",
                creds.display()
            )
            .as_bytes(),
        );
        // 対象平文と.age object(実age暗号化)
        let uuid = W2_ALPHA_UUID.to_string();
        let title = "alpha".to_string();
        let plain = b"w4-job-get-plain-content\n".to_vec();
        let plain_sha256 = job_sha256_hex(&plain);
        let source_size = plain.len() as u64;
        let age_path = env._tmp.path().join("alpha.zip.age");
        let age_sha256 =
            crate::crypto::encrypt_bytes(&plain, &age_path, &[public.clone()]).ok()?;
        let age_size = std::fs::metadata(&age_path).map(|m| m.len()).unwrap_or(0);
        let s3_key = format!("{API_TEST_PREFIX}data/{uuid}/default.age");
        // registry(file_write→object PUT→file_upload→索引upload=実経路の順)
        let registry_path = env.home.join("registry.jsonl");
        let ctx = crate::storage::CtxArgs {
            config: None,
            aws_config: None,
            key: None,
            profile: None,
            format: crate::storage::Format::Human,
        };
        let session = crate::storage::open_session(&ctx).expect("session(openはfake選択)");
        let bucket = session.aws.bucket.clone();
        let prefix = session.aws.prefix.clone();
        let reg_out = (|| -> Option<()> {
            crate::registry::append_record(
                &registry_path,
                &crate::registry::FileWriteRecord {
                    schema: 3,
                    record_type: "file_write".into(),
                    recorded_at: "2026-11-24T10:00:00+09:00".into(),
                    batch_id: "20261124-abcdef01".into(),
                    batch_date: "2026-11-24".into(),
                    uuid: uuid.clone(),
                    title: title.clone(),
                    source_path: format!("/data/{title}"),
                    source_name: format!("{title}.zip"),
                    source_size,
                    source_mtime: "2026-11-24T09:00:00+09:00".into(),
                    source_sha256: plain_sha256.clone(),
                    age_path: format!("2026-11-24/{title}.zip.age"),
                    age_size,
                    age_sha256: age_sha256.clone(),
                    recipients: vec!["SHA256:testfingerprint".into()],
                    category: "docs".into(),
                    label: "work".into(),
                    identity_hash: format!("hash-{uuid}"),
                    s3_key: s3_key.clone(),
                },
            )
            .ok()?;
            let vid = session
                .backend
                .put(&bucket, &s3_key, &age_path, storage_class)
                .ok()?;
            crate::registry::append_record(
                &registry_path,
                &crate::registry::FileUploadRecord {
                    schema: 2,
                    record_type: "file_upload".into(),
                    recorded_at: "2026-11-24T10:00:01+09:00".into(),
                    batch_id: "20261124-abcdef01".into(),
                    version_ts: "2026-11-24T10:00:01+09:00".into(),
                    uuid: uuid.clone(),
                    title: title.clone(),
                    s3_key: s3_key.clone(),
                    s3_bucket: bucket.clone(),
                    s3_region: "ap-northeast-1".into(),
                    version_id: vid,
                    s3_size: age_size as i64,
                    s3_checksum_sha256: "c".repeat(43),
                    s3_checksum_type: "FULL_OBJECT".into(),
                    source_sha256: plain_sha256.clone(),
                    age_sha256: age_sha256.clone(),
                    source_size,
                    age_size,
                    uploaded_at: "2026-11-24T10:00:02+09:00".into(),
                },
            )
            .ok()?;
            let entries = w3_current_entries(&session, &registry_path).ok()?;
            crate::index::upload_index(
                session.backend.as_ref(),
                &bucket,
                &prefix,
                &[public.clone()],
                &entries,
                "20261124-abcdef01",
                "2026-11-24T10:00:03+09:00",
                &registry_path,
            )
            .ok()?;
            Some(())
        })();
        reg_out?;
        // catalog DBを先に1回openしておく(worker・handlerの並行初回openで
        // journal_mode=WAL切替のSQLITE_BUSYが起きないように=serve起動時の
        // interrupt sweepが先にopenするのと同一条件)
        crate::catalog::CatalogDb::open().expect("catalog DB事前open");
        // AppState・router(worker testは同一state/semaphoreの下で観測する)
        let state = AppState::from_ctx_args(ctx).expect("AppState構築");
        let router = build_router(state.clone());
        let out_dir = env._tmp.path().join("restore-out");
        Some(JobExecFixture {
            env,
            state,
            router,
            uuid,
            title,
            s3_key,
            plain,
            out_dir,
        })
    }

    /// router oneshot呼出(async・workerと同一runtime上で使う)。
    async fn job_call(
        router: axum::Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (u16, serde_json::Value) {
        use tower::ServiceExt as _;
        let mut builder = axum::http::Request::builder()
            .method(axum::http::Method::from_bytes(method.as_bytes()).expect("method"))
            .uri(uri);
        let req = match body {
            Some(b) => {
                builder = builder.header("content-type", "application/json");
                builder.body(axum::body::Body::from(b.to_string()))
            }
            None => builder.body(axum::body::Body::empty()),
        }
        .expect("request build");
        let resp = router.oneshot(req).await.expect("oneshot");
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body read");
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    /// job行のstatus読み出し(worker testの待ち合わせで使う)。
    fn job_status_of(id: i64) -> String {
        let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
        db.job_by_id(API_TEST_BUCKET, API_TEST_PREFIX, id)
            .expect("job読込")
            .expect("行がある")
            .status
    }

    /// jobの終状態到達待ち(waiting_restoreのような中間statusも指定可)。
    async fn job_wait_status(id: i64, want: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if job_status_of(id) == want {
                return;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "job {id} がstatus={want}へ到達しない(現在={})",
                    job_status_of(id)
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// [covers:server.jobs-get-execution]
    /// get jobの実行(queued→running→succeeded): Standard tier(非archive)対象は
    /// 即時に取得・sha照合・復号・配置まで進みresult={name,dest}を記録する。
    /// 対象不在uuidはfailed+error code(not_found)で終わる。
    #[test]
    fn api_jobs_get_execution_standard_tier() {
        let Some(fx) = job_exec_setup("srv-w4-job-exec", "STANDARD") else {
            eprintln!("SKIP: ssh-keygen/ageが使えない環境");
            return;
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("worker runtime");
        let mgr = fx.state.jobs.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        rt.block_on(async move {
            let _worker = tokio::task::spawn(async move { mgr.worker_loop(shutdown_rx).await });
            // job発行(即時応答はqueued)
            let body = serde_json::json!({
                "uuid": fx.uuid,
                "out": fx.out_dir.display().to_string(),
            });
            let (status, resp) = job_call(fx.router.clone(), "POST", "/api/jobs/get", Some(body)).await;
            assert_eq!(status, 200, "発行は200: {resp}");
            let id = resp
                .get("job")
                .and_then(|j| j.get("id"))
                .and_then(|i| i.as_i64())
                .expect("job id");
            assert_eq!(
                resp.get("job").and_then(|j| j.get("status")).and_then(|s| s.as_str()),
                Some("queued"),
                "即時応答はqueued: {resp}"
            );
            // workerが実行してsucceededへ(Standard=復元不要)
            job_wait_status(id, "succeeded").await;
            let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
            let row = db
                .job_by_id(API_TEST_BUCKET, API_TEST_PREFIX, id)
                .expect("job読込")
                .expect("行がある");
            assert_eq!(row.error_code, None, "成功jobにerrorは無い");
            let result: serde_json::Value =
                serde_json::from_str(&row.result_json.expect("result")).expect("result JSON");
            assert_eq!(
                result.get("name").and_then(|n| n.as_str()),
                Some("alpha"),
                "result.name=配置名: {result}"
            );
            assert_eq!(
                result.get("dest").and_then(|d| d.as_str()),
                Some("alpha"),
                "result.dest=out基準相対path: {result}"
            );
            // 配置fileの実在と平文一致(sha照合・復号を経た配置の観測)
            let dest = fx.out_dir.join("alpha");
            let bytes = std::fs::read(&dest).expect("配置された平文");
            assert_eq!(bytes, fx.plain, "配置平文の内容一致");
            // progressはfetch完了(done=1)
            let progress: serde_json::Value =
                serde_json::from_str(&row.progress_json).expect("progress JSON");
            assert_eq!(
                progress.get("done").and_then(|d| d.as_i64()),
                Some(1),
                "progress.done=1: {progress}"
            );

            // 対象不在uuidのjobはfailed+error_codeで終わる(索引解決失敗)
            let body = serde_json::json!({
                "uuid": W2_ABSENT_UUID,
                "out": fx.out_dir.display().to_string(),
            });
            let (status, resp) = job_call(fx.router.clone(), "POST", "/api/jobs/get", Some(body)).await;
            assert_eq!(status, 200, "不在uuidも発行は200: {resp}");
            let absent_id = resp
                .get("job")
                .and_then(|j| j.get("id"))
                .and_then(|i| i.as_i64())
                .expect("job id");
            job_wait_status(absent_id, "failed").await;
            let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
            let row = db
                .job_by_id(API_TEST_BUCKET, API_TEST_PREFIX, absent_id)
                .expect("job読込")
                .expect("行がある");
            assert_eq!(row.error_code.as_deref(), Some("not_found"), "解決失敗のerror code");
            let _ = shutdown_tx.send(true);
        });
    }

    /// [covers:server.jobs-get-waiting-restore]
    /// DEEP_ARCHIVE対象のget job: running(復元job発行)→waiting_restore(semaphore
    /// 解放=書込APIが待機なく完了)→timerの定期確認(時刻注入seamで短縮)→
    /// 復元済み確認→running→取得→succeeded。復元job進行中はwaiting_restoreの
    /// まま次回確認へ。
    #[test]
    fn api_jobs_get_waiting_restore_timer() {
        let Some(fx) = job_exec_setup("srv-w4-job-wait", "DEEP_ARCHIVE") else {
            eprintln!("SKIP: ssh-keygen/ageが使えない環境");
            return;
        };
        // fake S3の呼出記録確認用にstate dirを控える(seed_restoreで状態を制御)
        let fake_state = std::path::PathBuf::from(
            std::env::var("AMAZING_BACKUP_FAKE_S3_STATE").expect("fake state env"),
        );
        let s3_key = fx.s3_key.clone();
        let restore_state_dir = fake_state.clone();
        let seed_restore = move |state: &str| {
            let p = restore_state_dir.join("restore").join(&s3_key);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("restore parent");
            }
            std::fs::write(p, state.as_bytes()).expect("restore state seed");
        };
        // W2書込API(PUT tags)のsemaphore解放観測に使うentityをcacheへ注入
        api_seed(&[api_entry(W2_ALPHA_UUID, "alpha", "docs", "work")], API_TEST_BUCKET, API_TEST_PREFIX);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("worker runtime");
        let mgr = fx.state.jobs.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        rt.block_on(async move {
            let worker_mgr = mgr.clone();
            let _worker =
                tokio::task::spawn(async move { worker_mgr.worker_loop(shutdown_rx).await });
            // job発行→workerが復元jobを発行してwaiting_restoreへ
            let body = serde_json::json!({
                "uuid": fx.uuid,
                "out": fx.out_dir.display().to_string(),
            });
            let (status, resp) = job_call(fx.router.clone(), "POST", "/api/jobs/get", Some(body)).await;
            assert_eq!(status, 200, "発行は200: {resp}");
            let id = resp
                .get("job")
                .and_then(|j| j.get("id"))
                .and_then(|i| i.as_i64())
                .expect("job id");
            job_wait_status(id, "waiting_restore").await;
            // 復元job発行(RestoreObject)とnext_check_atの記録を観測
            let calls = std::fs::read_to_string(fake_state.join("calls.log")).unwrap_or_default();
            assert!(
                calls.contains(&format!(
                    "s3 RestoreObject bucket={API_TEST_BUCKET} key={}",
                    fx.s3_key
                )),
                "復元job発行(restore)が行われている: {calls}"
            );
            let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
            let row = db
                .job_by_id(API_TEST_BUCKET, API_TEST_PREFIX, id)
                .expect("job読込")
                .expect("行がある");
            let progress: serde_json::Value =
                serde_json::from_str(&row.progress_json).expect("progress JSON");
            assert_eq!(
                progress.get("phase").and_then(|p| p.as_str()),
                Some("restore"),
                "waiting_restore中のphase=restore: {progress}"
            );
            assert!(
                progress.get("next_check_at").is_some_and(|n| n.as_str().is_some()),
                "progressにnext_check_at(timer確認時刻): {progress}"
            );

            // waiting_restore遷移時にsemaphoreは解放されている=書込API(PUT tags)が
            // 待機なく完了する(semaphoreを保持したままだと本呼出はtimeoutする)
            let tags_uri = format!("/api/entities/{}/tags", fx.uuid);
            let put = job_call(
                fx.router.clone(),
                "PUT",
                &tags_uri,
                Some(serde_json::json!({ "tags": ["w4"] })),
            );
            let put = tokio::time::timeout(std::time::Duration::from_secs(5), put).await;
            let (status, resp) = put.expect("PUT tagsがsemaphore待ちで停止していない");
            assert_eq!(status, 200, "waiting_restore中の書込APIは完了する: {resp}");

            // 復元job進行中(ongoing)はtimer確認でもwaiting_restoreのまま
            seed_restore("ongoing");
            mgr.set_time_offset_ms(16 * 60 * 1000);
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            assert_eq!(
                job_status_of(id),
                "waiting_restore",
                "復元中はwaiting_restoreのまま(次回確認へ)"
            );

            // 復元済みへ遷移→seamで確認時刻を進める→running→succeeded
            seed_restore("restored");
            mgr.set_time_offset_ms(32 * 60 * 1000);
            job_wait_status(id, "succeeded").await;
            let db = crate::catalog::CatalogDb::open().expect("catalog DB open");
            let row = db
                .job_by_id(API_TEST_BUCKET, API_TEST_PREFIX, id)
                .expect("job読込")
                .expect("行がある");
            assert_eq!(row.error_code, None, "成功: {row:?}");
            let result: serde_json::Value =
                serde_json::from_str(&row.result_json.expect("result")).expect("result JSON");
            assert_eq!(
                result.get("name").and_then(|n| n.as_str()),
                Some("alpha"),
                "取得結果のname: {result}"
            );
            let dest = fx.out_dir.join("alpha");
            let bytes = std::fs::read(&dest).expect("配置された平文");
            assert_eq!(bytes, fx.plain, "配置平文の内容一致");
            let _ = shutdown_tx.send(true);
        });
    }

    /// [covers:server.jobs-semaphore-serialization]
    /// job実行は書込APIと同一のsingle-flight semaphoreをacquireしてから行う
    /// (設計3.1: jobはjob queueで並び実行時にsemaphoreをacquire=書込APIとの相互
    /// 直列化)。testがsemaphoreを保持している間はjobはqueuedのまま・解放すると
    /// 実行される。
    #[test]
    fn api_jobs_wait_for_write_semaphore() {
        let Some(fx) = job_exec_setup("srv-w4-job-semaphore", "STANDARD") else {
            eprintln!("SKIP: ssh-keygen/ageが使えない環境");
            return;
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("worker runtime");
        let mgr = fx.state.jobs.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        rt.block_on(async move {
            let _worker = tokio::task::spawn(async move { mgr.worker_loop(shutdown_rx).await });
            // test側が書込semaphoreを保持(job実行のacquireを塞ぐ)
            let permit = fx
                .state
                .sync_permits
                .clone()
                .acquire_owned()
                .await
                .expect("test側semaphore acquire");
            let body = serde_json::json!({
                "uuid": fx.uuid,
                "out": fx.out_dir.display().to_string(),
            });
            let (status, resp) = job_call(fx.router.clone(), "POST", "/api/jobs/get", Some(body)).await;
            assert_eq!(status, 200, "発行は200: {resp}");
            let id = resp
                .get("job")
                .and_then(|j| j.get("id"))
                .and_then(|i| i.as_i64())
                .expect("job id");
            // semaphoreが塞がれている間はjobはqueuedのまま(workerは実行を開始しない)
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            assert_eq!(
                job_status_of(id),
                "queued",
                "書込semaphore保持中はjobはqueuedのまま(相互直列化)"
            );
            // 解放するとworkerが実行してsucceededへ
            drop(permit);
            fx.state.jobs.notify_worker();
            job_wait_status(id, "succeeded").await;
            let _ = shutdown_tx.send(true);
        });
    }

    /// [covers:server.jobs-cancel]
    /// semaphore待ちの間に取消されたqueued jobは、permit解放後も実行されない
    /// (workerはacquire後のrunning遷移をstatus='queued'条件付きclaimで行い、
    /// 取消が先行したjobはclaim不成立で実行しない=取消の200応答と矛盾しない)。
    #[test]
    fn api_jobs_cancelled_during_semaphore_wait_is_not_executed() {
        let Some(fx) = job_exec_setup("srv-w4-job-cancel-claim", "STANDARD") else {
            eprintln!("SKIP: ssh-keygen/ageが使えない環境");
            return;
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("worker runtime");
        let mgr = fx.state.jobs.clone();
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        rt.block_on(async move {
            let _worker = tokio::task::spawn(async move { mgr.worker_loop(shutdown_rx).await });
            // test側が書込semaphoreを保持(workerはjobを拾ってacquire待ちに入る)
            let permit = fx
                .state
                .sync_permits
                .clone()
                .acquire_owned()
                .await
                .expect("test側semaphore acquire");
            let body = serde_json::json!({
                "uuid": fx.uuid,
                "out": fx.out_dir.display().to_string(),
            });
            let (status, resp) =
                job_call(fx.router.clone(), "POST", "/api/jobs/get", Some(body)).await;
            assert_eq!(status, 200, "発行は200: {resp}");
            let id = resp
                .get("job")
                .and_then(|j| j.get("id"))
                .and_then(|i| i.as_i64())
                .expect("job id");
            // workerがacquire待ちに入る間もjobはqueuedのまま
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            assert_eq!(job_status_of(id), "queued", "semaphore保持中はqueuedのまま");
            // acquire待ちの間に取消する(queuedのため受理されcancelledで確定する)
            let (status, resp) = job_call(
                fx.router.clone(),
                "POST",
                &format!("/api/jobs/{id}/cancel"),
                None,
            )
            .await;
            assert_eq!(status, 200, "取消は200: {resp}");
            assert_eq!(
                resp.get("job")
                    .and_then(|j| j.get("status"))
                    .and_then(|s| s.as_str()),
                Some("cancelled"),
                "取消後のjob objectはcancelled: {resp}"
            );
            // permitを解放してもworkerはclaim不成立で実行しない
            drop(permit);
            fx.state.jobs.notify_worker();
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            assert_eq!(
                job_status_of(id),
                "cancelled",
                "取消済みjobはpermit解放後も実行されない(claim不成立)"
            );
            assert!(
                !fx.out_dir.join(&fx.title).exists(),
                "取消済みjobの配置ファイルは作られない"
            );
            let _ = shutdown_tx.send(true);
        });
    }

    // -----------------------------------------------------------------------
    // TASK-17 W2(TASK-19: tag/collection・3ビュー・audit_log)のhandler単体test
    // (テスト先行。実装前のためrouting不在のfallback 404・table不在のrusqlite操作で
    // 実行failを許容、cargo test --no-runのcompileは通る)。設計3.2/4.1に詳細が
    // 規定されない点は各条件本文(server.toml)の解釈に従う。

    /// W2 testで使うentity uuid(固定値・api_seedしたentityと対応)。
    const W2_ALPHA_UUID: &str = "11111111-1111-1111-1111-111111111111";
    const W2_BETA_UUID: &str = "22222222-2222-2222-2222-222222222222";
    const W2_GAMMA_UUID: &str = "33333333-3333-3333-3333-333333333333";
    const W2_ABSENT_UUID: &str = "99999999-9999-9999-9999-999999999999";
    /// 対照scopeのtag行混入観測で使うuuid(実効scopeに存在しない)。
    const W2_DELTA_UUID: &str = "44444444-4444-4444-4444-444444444444";

    /// api()のJSON body付き版(書込APIのoneshot=PUT tags・collections系のrequest body
    /// を伴う呼び出し。応答はapi()と同一形式(status, JSON body))。
    fn api_body(method: &str, uri: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
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
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
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

    /// entities応答からuuid一覧を抽出(W2絞り込みtest用)。
    fn uuids_of(body: &serde_json::Value) -> Vec<String> {
        body.get("entities")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|e| {
                        e.get("uuid")
                            .and_then(|u| u.as_str())
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// tags応答から(tag, entity_count)一覧を抽出(W2 tags-list test用)。
    fn tags_of(body: &serde_json::Value) -> Vec<(String, i64)> {
        body.get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|t| {
                        (
                            t.get("tag")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            t.get("entity_count").and_then(|v| v.as_i64()).unwrap_or(-1),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// folders応答から(path, label, entity_count)一覧を抽出(W2 folders test用)。
    fn folders_of(body: &serde_json::Value) -> Vec<(String, String, i64)> {
        body.get("folders")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|f| {
                        (
                            f.get("path")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            f.get("label")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            f.get("entity_count")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(-1),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// collectionを作成してidを返す(W2 testの事前準備。作成API自体は
    /// server.collection-crudが担保するためここでは成功のみ使う)。
    fn w2_create_collection(name: &str) -> i64 {
        let (status, body) =
            api_body("POST", "/api/collections", serde_json::json!({ "name": name }));
        assert_eq!(status, 200, "collection作成(事前): {body}");
        body.get("collection")
            .and_then(|c| c.get("id"))
            .and_then(|i| i.as_i64())
            .unwrap_or_else(|| panic!("作成応答のcollection.id: {body}"))
    }

    /// tags tableへ行を直接注入(rusqlite。対照scopeのtagが実効scopeの絞り込み・
    /// 集計に混入しないことの観測用)。
    fn w2_insert_tag_row(env: &ApiEnv, scope: &str, uuid: &str, tag: &str) {
        let conn = rusqlite::Connection::open(env.home.join("catalog.db"))
            .expect("catalog.db open");
        conn.execute(
            "INSERT INTO tags (scope, uuid, tag, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![scope, uuid, tag, "2026-11-05T12:00:00+09:00"],
        )
        .expect("tags行挿入(実装前はtable不在でfailを許容)");
    }

    /// collections tableへ行を直接注入(rusqlite。対照scopeのcollection混入観測用)。
    /// 戻り値は挿入した行のid。
    fn w2_insert_collection_row(env: &ApiEnv, scope: &str, name: &str) -> i64 {
        let conn = rusqlite::Connection::open(env.home.join("catalog.db"))
            .expect("catalog.db open");
        conn.execute(
            "INSERT INTO collections (scope, name, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![scope, name, "2026-11-05T12:00:00+09:00"],
        )
        .expect("collections行挿入(実装前はtable不在でfailを許容)");
        conn.last_insert_rowid()
    }

    /// audit_logの行(action, detail_json, outcome, error_code, scope, recorded_at)を
    /// 全行(id昇順)で読む(W2 audit_log test用)。
    fn w2_audit_log_rows(env: &ApiEnv) -> Vec<(String, String, String, Option<String>, String, String)> {
        let conn = rusqlite::Connection::open(env.home.join("catalog.db"))
            .expect("catalog.db open");
        let mut stmt = conn
            .prepare(
                "SELECT action, detail_json, outcome, error_code, scope, recorded_at \
                 FROM audit_log ORDER BY id",
            )
            .expect("audit_log読出(実装前はtable不在でfailを許容)");
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .expect("audit_log行読取");
        rows.collect::<Result<Vec<_>, _>>().expect("audit_log行")
    }

    /// [covers:server.entities-tag-collection-filters]
    /// GET /api/entitiesは?tag=<tag名>・?collection=<collection id>で絞り込む。
    /// tag絞り込みは実効scope内のtag付きentityのみ(対照scopeのtag行は混入しない)。
    /// folder絞り込み用の新paramは設けない(既存label=で成立=設計3.2)。
    #[test]
    fn api_entities_filter_by_tag_and_collection() {
        let env = api_env("srv-api-w2-ent-filter");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        let beta = api_entry(W2_BETA_UUID, "beta", "unfiled", "");
        let gamma = api_entry(W2_GAMMA_UUID, "gamma", "docs", "work");
        api_seed(&[alpha, beta, gamma], API_TEST_BUCKET, API_TEST_PREFIX);

        // 事前: alpha/betaへtag付与・collectionへalphaを追加(各書込APIは個別条件が担保)
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_ALPHA_UUID}/tags"),
            serde_json::json!({ "tags": ["docs", "work"] }),
        );
        assert_eq!(status, 200, "alphaへtag付与(事前): {body}");
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_BETA_UUID}/tags"),
            serde_json::json!({ "tags": ["docs"] }),
        );
        assert_eq!(status, 200, "betaへtag付与(事前): {body}");
        let id = w2_create_collection("grp");
        let (status, body) = api_body(
            "POST",
            &format!("/api/collections/{id}/items"),
            serde_json::json!({ "uuids": [W2_ALPHA_UUID] }),
        );
        assert_eq!(status, 200, "collectionへalpha追加(事前): {body}");
        // 対照scopeのtag行(実効scope外=絞り込みに混入してはならない)
        w2_insert_tag_row(&env, "other-bucket:amazing-backup/", W2_DELTA_UUID, "docs");

        // tag絞り込み
        let (status, body) = api("GET", "/api/entities?tag=docs");
        assert_eq!(status, 200, "?tag=docsは200: {body}");
        let mut uuids = uuids_of(&body);
        uuids.sort();
        assert_eq!(
            uuids,
            vec![W2_ALPHA_UUID.to_string(), W2_BETA_UUID.to_string()],
            "tag=docsはalpha+beta(対照scopeのdeltaは混入しない): {body}"
        );
        let (status, body) = api("GET", "/api/entities?tag=work");
        assert_eq!(status, 200, "?tag=workは200: {body}");
        assert_eq!(uuids_of(&body), vec![W2_ALPHA_UUID.to_string()], "tag=workはalphaのみ: {body}");
        let (status, body) = api("GET", "/api/entities?tag=nosuch");
        assert_eq!(status, 200, "?tag=一致なしも200: {body}");
        assert!(uuids_of(&body).is_empty(), "一致なしtagは空配列: {body}");

        // collection絞り込み(collection id指定)
        let (status, body) = api("GET", &format!("/api/entities?collection={id}"));
        assert_eq!(status, 200, "?collection=<id>は200: {body}");
        assert_eq!(
            uuids_of(&body),
            vec![W2_ALPHA_UUID.to_string()],
            "collection絞り込みは構成のalphaのみ: {body}"
        );

        // 契約統一(TASK-19): 一覧の各summary行はtags: string[](昇順)を含む
        // (設計2.2「行の列: 現行7列+tag列」・frontend EntitySummaryのtags必須と整合)
        let (status, body) = api("GET", "/api/entities");
        assert_eq!(status, 200, "GET /api/entitiesは200: {body}");
        let tags_of_summary = |want_uuid: &str| -> Vec<String> {
            body.get("entities")
                .and_then(|v| v.as_array())
                .expect("entities配列")
                .iter()
                .find(|e| e.get("uuid").and_then(|u| u.as_str()) == Some(want_uuid))
                .and_then(|e| e.get("tags"))
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .map(|t| t.as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(
            tags_of_summary(W2_ALPHA_UUID),
            vec!["docs".to_string(), "work".to_string()],
            "alphaのsummary行tagsは昇順: {body}"
        );
        assert_eq!(
            tags_of_summary(W2_BETA_UUID),
            vec!["docs".to_string()],
            "betaのsummary行tags: {body}"
        );
        assert_eq!(
            tags_of_summary(W2_GAMMA_UUID),
            Vec::<String>::new(),
            "tag無しentityのsummary行tagsは空配列: {body}"
        );

        // 契約統一(TASK-19): 詳細はcollections(所属collection名一覧)を含む
        // (設計2.2詳細の「所属collection」表示)
        let (status, body) = api("GET", &format!("/api/entities/{W2_ALPHA_UUID}"));
        assert_eq!(status, 200, "GET /api/entities/:uuidは200: {body}");
        assert_eq!(
            body.get("collections")
                .and_then(|c| c.as_array())
                .map(|a| a.iter().map(|v| v.as_str().unwrap_or_default().to_string()).collect::<Vec<_>>()),
            Some(vec!["grp".to_string()]),
            "詳細の所属collectionはgrp: {body}"
        );
        drop(env);
    }

    /// [covers:server.tags-list]
    /// GET /api/tagsは{tags:[{tag,entity_count}]}をtag名昇順で返す。
    /// entity_countは当該tagを持つ実効scope内のentity数(対照scopeのtag行は計上しない)。
    #[test]
    fn api_tags_list_with_entity_counts() {
        let env = api_env("srv-api-w2-tags-list");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        let beta = api_entry(W2_BETA_UUID, "beta", "unfiled", "");
        api_seed(&[alpha, beta], API_TEST_BUCKET, API_TEST_PREFIX);
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_ALPHA_UUID}/tags"),
            serde_json::json!({ "tags": ["docs", "work"] }),
        );
        assert_eq!(status, 200, "alphaへtag付与(事前): {body}");
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_BETA_UUID}/tags"),
            serde_json::json!({ "tags": ["docs"] }),
        );
        assert_eq!(status, 200, "betaへtag付与(事前): {body}");
        // 対照scopeのtag行(集計に混入しない)
        w2_insert_tag_row(&env, "other-bucket:amazing-backup/", W2_DELTA_UUID, "docs");

        let (status, body) = api("GET", "/api/tags");
        assert_eq!(status, 200, "GET /api/tagsは200: {body}");
        assert_eq!(
            tags_of(&body),
            vec![
                ("docs".to_string(), 2),
                ("work".to_string(), 1),
            ],
            "tag昇順・entity_countはalpha+betaのdocs=2・work=1(対照scopeは計上しない): {body}"
        );
        drop(env);
    }

    /// [covers:server.folders-list]
    /// GET /api/foldersはlabelのdirectory区切りで階層導出したfolder一覧を
    /// {folders:[{label,path,entity_count}]}で返す(設計3.2の応答解釈: 各行=labelの
    /// 階層nodeごと・path=nodeのrootからの完全path(既存label=絞り込みの値と一致)・
    /// label=nodeの最終区切り要素・entity_count=当該node配下のentity数(子folder配下を
    /// 含む合計)・並びはpath昇順。ラベルなし(location.label="")のlocationを持つ
    /// entityは先頭行{label:"",path:"",entity_count}で応答する=設計2.2「(ラベルなし)」
    /// nodeの基盤(コードレビュー中10。行の選択は既存label=の空文字完全一致と対応)。
    /// folder絞り込みparamは新設せずlabel=path完全一致の既存label=で成立する。
    #[test]
    fn api_folders_list_hierarchy_from_labels() {
        let env = api_env("srv-api-w2-folders");
        // beta=work直下・alpha=work/report配下・delta=docs/2026/report配下・gamma=label無し
        let beta = api_entry(W2_BETA_UUID, "beta", "unfiled", "work");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work/report");
        let delta = api_entry(W2_DELTA_UUID, "delta", "docs", "docs/2026/report");
        let gamma = api_entry(W2_GAMMA_UUID, "gamma", "docs", "");
        api_seed(&[alpha, beta, delta, gamma], API_TEST_BUCKET, API_TEST_PREFIX);

        let (status, body) = api("GET", "/api/folders");
        assert_eq!(status, 200, "GET /api/foldersは200: {body}");
        assert_eq!(
            folders_of(&body),
            vec![
                // ラベルなし行(gamma)は常に先頭(コードレビュー中10)
                ("".to_string(), "".to_string(), 1),
                ("docs".to_string(), "docs".to_string(), 1),
                ("docs/2026".to_string(), "2026".to_string(), 1),
                ("docs/2026/report".to_string(), "report".to_string(), 1),
                // workは直下のbeta+配下のalphaの合計2(階層集約)
                ("work".to_string(), "work".to_string(), 2),
                ("work/report".to_string(), "report".to_string(), 1),
            ],
            "先頭はラベルなし行(1=gamma)・以降path昇順・entity_countは配下合計(work=2): {body}"
        );
        // 各要素はlabel/path/entity_countのみ(応答schemaの確定)
        let elems = body.get("folders").and_then(|v| v.as_array()).expect("folders配列");
        for f in elems {
            assert_eq!(
                f.as_object().map(|m| m.len()),
                Some(3),
                "folder要素はlabel/path/entity_countのみ: {f}"
            );
        }
        drop(env);
    }

    /// [covers:server.collections-list-and-detail]
    /// GET /api/collectionsは{collections:[{id,name,entity_count}]}をname昇順で返す
    /// (対照scopeのcollectionは混入しない)。GET /api/collections/:idは詳細
    /// {collection:{id,name,uuids}}(構成uuid一覧・追加順)を返し・不在idは404 not_found。
    #[test]
    fn api_collections_list_and_detail() {
        let env = api_env("srv-api-w2-collections");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        let beta = api_entry(W2_BETA_UUID, "beta", "unfiled", "");
        api_seed(&[alpha, beta], API_TEST_BUCKET, API_TEST_PREFIX);
        // zeta(alpha構成)・alpha-beta(beta構成)の2 collection(name昇順観測の対)
        let zeta_id = w2_create_collection("zeta");
        let (status, body) = api_body(
            "POST",
            &format!("/api/collections/{zeta_id}/items"),
            serde_json::json!({ "uuids": [W2_ALPHA_UUID] }),
        );
        assert_eq!(status, 200, "zetaへalpha追加(事前): {body}");
        let ab_id = w2_create_collection("alpha-beta");
        let (status, body) = api_body(
            "POST",
            &format!("/api/collections/{ab_id}/items"),
            serde_json::json!({ "uuids": [W2_BETA_UUID] }),
        );
        assert_eq!(status, 200, "alpha-betaへbeta追加(事前): {body}");
        // 対照scopeのcollection(一覧に混入しない)
        w2_insert_collection_row(&env, "other-bucket:amazing-backup/", "other");

        let (status, body) = api("GET", "/api/collections");
        assert_eq!(status, 200, "GET /api/collectionsは200: {body}");
        let cols = body
            .get("collections")
            .and_then(|v| v.as_array())
            .expect("collections配列");
        assert_eq!(cols.len(), 2, "実効scopeの2collection(対照scopeは混入しない): {body}");
        assert_eq!(
            cols[0].get("name").and_then(|n| n.as_str()),
            Some("alpha-beta"),
            "name昇順の先頭: {body}"
        );
        assert_eq!(
            cols[0].get("entity_count").and_then(|c| c.as_i64()),
            Some(1),
            "alpha-betaの構成数: {body}"
        );
        assert_eq!(
            cols[1].get("name").and_then(|n| n.as_str()),
            Some("zeta"),
            "name昇順の2番目: {body}"
        );

        // 詳細(構成uuid一覧)
        let (status, body) = api("GET", &format!("/api/collections/{zeta_id}"));
        assert_eq!(status, 200, "GET /api/collections/:idは200: {body}");
        let col = body.get("collection").expect("collection要素");
        assert_eq!(col.get("id").and_then(|i| i.as_i64()), Some(zeta_id), "id: {body}");
        assert_eq!(col.get("name").and_then(|n| n.as_str()), Some("zeta"), "name: {body}");
        assert_eq!(
            col.get("uuids").and_then(|u| u.as_array()).map(|a| a.len()),
            Some(1),
            "構成uuid一覧(追加順・alphaの1件): {body}"
        );
        // 不在id
        let (status, body) = api("GET", "/api/collections/99999");
        assert_eq!(status, 404, "不在collection idは404: {body}");
        expect_error_body(&body, "not_found");
        drop(env);
    }

    /// [covers:server.entity-tags-replace]
    /// PUT /api/entities/:uuid/tagsはbody {tags:[...]}でtag一覧を丸ごと置換する
    /// (冪等・応答はGET /api/entities/:uuidと同一のEntityDetail形式=tags・collections
    /// 付き=契約統一(設計3.1「書込応答は読取系と同一schema・操作後に再GET不要」))。
    /// tag名規則 ^[a-z0-9][a-z0-9-]{0,63}$違反は422 validation_failed・不在uuidは404 not_found。
    #[test]
    fn api_entity_tags_replace_idempotent_and_validated() {
        let env = api_env("srv-api-w2-tags-replace");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);
        let path = format!("/api/entities/{W2_ALPHA_UUID}/tags");

        // 置換: 2tag付与(応答は契約統一によりGET /api/entities/:uuidと同一の
        // EntityDetail形式=uuid/title/locations/versions/latest_date/latest_size/
        // pins/version_list+tags+collections。設計3.1「書込応答は読取系と同一schema・
        // 操作後に再GET不要」)
        let (status, body) =
            api_body("PUT", &path, serde_json::json!({ "tags": ["docs", "work"] }));
        assert_eq!(status, 200, "tag置換は200: {body}");
        assert_eq!(
            body.get("tags").and_then(|t| t.as_array()).map(|a| a.len()),
            Some(2),
            "応答は操作後のtag一覧(2件): {body}"
        );
        assert_eq!(
            body.get("uuid").and_then(|u| u.as_str()),
            Some(W2_ALPHA_UUID),
            "応答はEntityDetail形式(uuid入り): {body}"
        );
        assert_eq!(
            body.get("title").and_then(|t| t.as_str()),
            Some("alpha"),
            "応答はEntityDetail形式(title入り): {body}"
        );
        assert_eq!(
            body.get("collections").and_then(|c| c.as_array()).map(|a| a.len()),
            Some(0),
            "応答はEntityDetail形式(collections入り=未所属は空配列): {body}"
        );
        assert!(
            body.get("version_list").and_then(|v| v.as_array()).is_some(),
            "応答はEntityDetail形式(version_list入り): {body}"
        );
        // 置換(丸ごと差し替え=docsのみ残す)
        let (status, body) =
            api_body("PUT", &path, serde_json::json!({ "tags": ["docs"] }));
        assert_eq!(status, 200, "再置換は200: {body}");
        assert_eq!(
            body.get("tags").and_then(|t| t.as_array()).map(|a| a.len()),
            Some(1),
            "置換後はdocsのみ(workは外れる): {body}"
        );
        // 冪等: 同body再送で同一状態
        let (status, body2) =
            api_body("PUT", &path, serde_json::json!({ "tags": ["docs"] }));
        assert_eq!(status, 200, "冪等再送は200: {body2}");
        assert_eq!(body.get("tags"), body2.get("tags"), "同body再送で同一応答(冪等): {body} {body2}");
        // 空配列=全tag削除
        let (status, body) = api_body("PUT", &path, serde_json::json!({ "tags": [] }));
        assert_eq!(status, 200, "空配列は200: {body}");
        assert_eq!(
            body.get("tags").and_then(|t| t.as_array()).map(|a| a.len()),
            Some(0),
            "空配列で全tag削除: {body}"
        );

        // tag名規則違反は422 validation_failed(先頭大文字・先頭ハイフン・65文字)
        for bad in ["Bad", "-x", &"a".repeat(65)] {
            let (status, body) =
                api_body("PUT", &path, serde_json::json!({ "tags": [bad] }));
            assert_eq!(status, 422, "tag名規則違反({bad})は422: {body}");
            expect_error_body(&body, "validation_failed");
        }
        // 規則内の長さ上限(64文字)は受理
        let ok64 = "a".repeat(64);
        let (status, body) =
            api_body("PUT", &path, serde_json::json!({ "tags": [ok64] }));
        assert_eq!(status, 200, "64文字tagは受理: {body}");

        // 不在uuidは404
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_ABSENT_UUID}/tags"),
            serde_json::json!({ "tags": ["docs"] }),
        );
        assert_eq!(status, 404, "不在uuidのtag置換は404: {body}");
        expect_error_body(&body, "not_found");
        drop(env);
    }

    /// [covers:server.collection-crud]
    /// POST /api/collectionsはcollectionを作成(応答は詳細schema)・同名は409 duplicate
    /// (UNIQUE(scope,name))・空nameは422。PUT /api/collections/:idは改名(応答は詳細
    /// schema)・改名先の同名は409・不在idは404。DELETE /api/collections/:idは削除
    /// (構成参照の解除のみ=entityへ影響しない)・応答{status:"ok"}。
    #[test]
    fn api_collection_create_rename_delete() {
        let env = api_env("srv-api-w2-collections-crud");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        let beta = api_entry(W2_BETA_UUID, "beta", "unfiled", "");
        api_seed(&[alpha, beta], API_TEST_BUCKET, API_TEST_PREFIX);

        // 作成
        let (status, body) =
            api_body("POST", "/api/collections", serde_json::json!({ "name": "trip" }));
        assert_eq!(status, 200, "collection作成は200: {body}");
        let created = body.get("collection").expect("作成応答は詳細schema");
        let id = created.get("id").and_then(|i| i.as_i64()).expect("collection id");
        assert_eq!(
            created.get("name").and_then(|n| n.as_str()),
            Some("trip"),
            "作成応答のname: {body}"
        );
        // 同名は409 duplicate
        let (status, body) =
            api_body("POST", "/api/collections", serde_json::json!({ "name": "trip" }));
        assert_eq!(status, 409, "同名collectionは409: {body}");
        expect_error_body(&body, "duplicate");
        // 空nameは422
        let (status, body) =
            api_body("POST", "/api/collections", serde_json::json!({ "name": "" }));
        assert_eq!(status, 422, "空nameは422: {body}");
        expect_error_body(&body, "validation_failed");

        // 改名
        let (status, body) = api_body(
            "PUT",
            &format!("/api/collections/{id}"),
            serde_json::json!({ "name": "vacation" }),
        );
        assert_eq!(status, 200, "collection改名は200: {body}");
        let renamed = body.get("collection").expect("改名応答は詳細schema");
        assert_eq!(renamed.get("id").and_then(|i| i.as_i64()), Some(id), "id不変: {body}");
        assert_eq!(
            renamed.get("name").and_then(|n| n.as_str()),
            Some("vacation"),
            "改名後のname: {body}"
        );
        // 改名先が既存collectionと同名は409
        w2_insert_collection_row(&env, "api-bucket:amazing-backup/", "occupied");
        let (status, body) = api_body(
            "PUT",
            &format!("/api/collections/{id}"),
            serde_json::json!({ "name": "occupied" }),
        );
        assert_eq!(status, 409, "改名先の同名は409: {body}");
        expect_error_body(&body, "duplicate");
        // 不在idは404
        let (status, body) = api_body(
            "PUT",
            "/api/collections/99999",
            serde_json::json!({ "name": "x" }),
        );
        assert_eq!(status, 404, "不在idの改名は404: {body}");
        expect_error_body(&body, "not_found");

        // 構成を持つcollectionの削除(構成参照の解除のみ)
        let (status, body) = api_body(
            "POST",
            &format!("/api/collections/{id}/items"),
            serde_json::json!({ "uuids": [W2_ALPHA_UUID] }),
        );
        assert_eq!(status, 200, "削除対象へalpha追加(事前): {body}");
        let (status, body) = api_body("DELETE", &format!("/api/collections/{id}"), serde_json::Value::Null);
        assert_eq!(status, 200, "collection削除は200: {body}");
        assert_eq!(
            body.get("status").and_then(|s| s.as_str()),
            Some("ok"),
            "削除応答は{{\"status\":\"ok\"}}: {body}"
        );
        let (status, body) = api("GET", &format!("/api/collections/{id}"));
        assert_eq!(status, 404, "削除後の詳細は404: {body}");
        // entity側は影響しない(構成参照の解除のみ)
        let (status, body) = api("GET", "/api/entities");
        assert_eq!(status, 200, "entity一覧は200: {body}");
        let mut uuids = uuids_of(&body);
        uuids.sort();
        assert_eq!(
            uuids,
            vec![W2_ALPHA_UUID.to_string(), W2_BETA_UUID.to_string()],
            "削除はentityへ影響しない: {body}"
        );
        drop(env);
    }

    /// [covers:server.collection-items]
    /// POST /api/collections/:id/itemsはbody {uuids:[...]}で構成を追加(応答は操作後の
    /// 詳細schema・uuidsは追加順)・不在collectionは404・不在uuidは404。既に構成のuuidの
    /// 再追加は冪等に成功(追加済みはskip)。DELETE /api/collections/:id/items/:uuidは
    /// 構成から除去(応答は操作後の詳細schema)・非構成uuid・不在collectionは404。
    #[test]
    fn api_collection_items_add_and_remove() {
        let env = api_env("srv-api-w2-collections-items");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        let beta = api_entry(W2_BETA_UUID, "beta", "unfiled", "");
        api_seed(&[alpha, beta], API_TEST_BUCKET, API_TEST_PREFIX);
        let id = w2_create_collection("grp");

        // 追加(複数一括・応答のuuidsは追加順)
        let (status, body) = api_body(
            "POST",
            &format!("/api/collections/{id}/items"),
            serde_json::json!({ "uuids": [W2_ALPHA_UUID, W2_BETA_UUID] }),
        );
        assert_eq!(status, 200, "構成追加は200: {body}");
        let uuids: Vec<String> = body
            .get("collection")
            .and_then(|c| c.get("uuids"))
            .and_then(|u| u.as_array())
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            uuids,
            vec![W2_ALPHA_UUID.to_string(), W2_BETA_UUID.to_string()],
            "操作後の構成uuid一覧(追加順): {body}"
        );
        // 既に構成のuuidの再追加は冪等に成功
        let (status, body) = api_body(
            "POST",
            &format!("/api/collections/{id}/items"),
            serde_json::json!({ "uuids": [W2_ALPHA_UUID] }),
        );
        assert_eq!(status, 200, "再追加は200(冪等): {body}");
        assert_eq!(
            body.get("collection")
                .and_then(|c| c.get("uuids"))
                .and_then(|u| u.as_array())
                .map(|a| a.len()),
            Some(2),
            "再追加で構成は増えない(追加済みskip): {body}"
        );
        // 不在uuidは404
        let (status, body) = api_body(
            "POST",
            &format!("/api/collections/{id}/items"),
            serde_json::json!({ "uuids": [W2_ABSENT_UUID] }),
        );
        assert_eq!(status, 404, "不在uuidの追加は404: {body}");
        expect_error_body(&body, "not_found");
        // 不在collectionは404
        let (status, body) = api_body(
            "POST",
            "/api/collections/99999/items",
            serde_json::json!({ "uuids": [W2_ALPHA_UUID] }),
        );
        assert_eq!(status, 404, "不在collectionへの追加は404: {body}");
        expect_error_body(&body, "not_found");

        // 除去(応答は操作後の詳細schema)
        let (status, body) =
            api_body("DELETE", &format!("/api/collections/{id}/items/{W2_ALPHA_UUID}"), serde_json::Value::Null);
        assert_eq!(status, 200, "構成除去は200: {body}");
        let uuids: Vec<String> = body
            .get("collection")
            .and_then(|c| c.get("uuids"))
            .and_then(|u| u.as_array())
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(uuids, vec![W2_BETA_UUID.to_string()], "除去後の構成はbetaのみ: {body}");
        // 非構成uuidの除去は404
        let (status, body) =
            api_body("DELETE", &format!("/api/collections/{id}/items/{W2_ALPHA_UUID}"), serde_json::Value::Null);
        assert_eq!(status, 404, "非構成uuidの除去は404: {body}");
        expect_error_body(&body, "not_found");
        // 不在collectionは404
        let (status, body) =
            api_body("DELETE", &format!("/api/collections/99999/items/{W2_BETA_UUID}"), serde_json::Value::Null);
        assert_eq!(status, 404, "不在collectionからの除去は404: {body}");
        expect_error_body(&body, "not_found");
        drop(env);
    }

    /// [covers:server.write-api-audit-log-recorded]
    /// W2の書込API(tags系・collections系)はUI書込操作としてaudit_logへ記録される
    /// (action=操作種別・detail_json=対象特定情報のJSON文字列・outcome=ok|failed・
    /// 失敗時はerror_code・scope=実効scope・recorded_at)。失敗操作(422)も記録される
    /// (2.5操作履歴の成否表示の基盤)。action語彙はtags_replace/collection_create/
    /// collection_rename/collection_delete/collection_items_add/collection_items_remove。
    #[test]
    fn api_write_operations_recorded_in_audit_log() {
        let env = api_env("srv-api-w2-audit");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);
        let tags_path = format!("/api/entities/{W2_ALPHA_UUID}/tags");

        // 成功書込: tag置換
        let (status, body) =
            api_body("PUT", &tags_path, serde_json::json!({ "tags": ["docs"] }));
        assert_eq!(status, 200, "tag置換(成功例・事前): {body}");
        // 失敗書込: tag名規則違反(422)
        let (status, body) =
            api_body("PUT", &tags_path, serde_json::json!({ "tags": ["BAD"] }));
        assert_eq!(status, 422, "tag名規則違反(失敗例・事前): {body}");
        // 成功書込: collection作成→items追加→改名→削除
        let id = w2_create_collection("grp");
        let (status, body) = api_body(
            "POST",
            &format!("/api/collections/{id}/items"),
            serde_json::json!({ "uuids": [W2_ALPHA_UUID] }),
        );
        assert_eq!(status, 200, "items追加(事前): {body}");
        let (status, body) = api_body(
            "PUT",
            &format!("/api/collections/{id}"),
            serde_json::json!({ "name": "grp2" }),
        );
        assert_eq!(status, 200, "collection改名(事前): {body}");
        let (status, body) =
            api_body("DELETE", &format!("/api/collections/{id}"), serde_json::Value::Null);
        assert_eq!(status, 200, "collection削除(事前): {body}");

        // audit_logの行観測(id昇順=実行順)
        let rows = w2_audit_log_rows(&env);
        assert!(rows.len() >= 6, "書込操作6件が記録される: {rows:?}");
        let (action, detail_json, outcome, _error_code, scope, recorded_at) = &rows[0];
        assert_eq!(action, "tags_replace", "1件目はtag置換: {rows:?}");
        assert_eq!(outcome, "ok", "成功操作はoutcome=ok: {rows:?}");
        assert!(
            detail_json.contains(W2_ALPHA_UUID),
            "detail_jsonは対象(uuid)を含む: {rows:?}"
        );
        assert_eq!(
            scope, "api-bucket:amazing-backup/",
            "scopeは実効scope(<bucket>:<prefix>形式): {rows:?}"
        );
        assert!(!recorded_at.is_empty(), "recorded_atは非空: {rows:?}");
        // 失敗操作(422)はoutcome=failed+error_code
        let (action, _, outcome, error_code, _, _) = &rows[1];
        assert_eq!(action, "tags_replace", "2件目もtag置換(失敗側): {rows:?}");
        assert_eq!(outcome, "failed", "失敗操作はoutcome=failed: {rows:?}");
        assert_eq!(
            error_code.as_deref(),
            Some("validation_failed"),
            "失敗時はerror_code: {rows:?}"
        );
        // collections系の成功記録
        let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        for want in [
            "collection_create",
            "collection_items_add",
            "collection_rename",
            "collection_delete",
        ] {
            assert!(
                actions.contains(&want),
                "action={want}が記録される: {actions:?}"
            );
        }
        // 全行でoutcome/scope/recorded_atは欠落しない
        for row in &rows {
            assert!(!row.2.is_empty(), "outcomeは非空: {row:?}");
            assert!(!row.4.is_empty(), "scopeは非空: {row:?}");
            assert!(!row.5.is_empty(), "recorded_atは非空: {row:?}");
        }
        drop(env);
    }

    /// [covers:server.write-api-auth-gate]
    /// W2の書込API(tags系・collections系)も同一auth_gate(session検証)で保護され、
    /// 未認証は401 unauthenticated(設計6章: 読取/書込の区別なし=同一gate)。
    #[test]
    fn api_write_endpoints_unauthenticated_401() {
        let flow = AuthFlow::new("srv-api-w2-auth");
        let cases: [(&str, String, serde_json::Value); 8] = [
            ("PUT", format!("/api/entities/{W2_ALPHA_UUID}/tags"), serde_json::json!({ "tags": [] })),
            ("POST", "/api/collections".into(), serde_json::json!({ "name": "x" })),
            ("PUT", "/api/collections/1".into(), serde_json::json!({ "name": "x" })),
            ("DELETE", "/api/collections/1".into(), serde_json::Value::Null),
            ("POST", "/api/collections/1/items".into(), serde_json::json!({ "uuids": [] })),
            ("DELETE", format!("/api/collections/1/items/{W2_ALPHA_UUID}"), serde_json::Value::Null),
            // W3(TASK-22): rename・locationも同一gate
            ("POST", format!("/api/entities/{W2_ALPHA_UUID}/rename"), serde_json::json!({ "title": "x" })),
            ("POST", format!("/api/entities/{W2_ALPHA_UUID}/location"), serde_json::json!({ "source_path": "/p", "category": "c", "label": "" })),
        ];
        for (method, path, body) in cases {
            let (status, resp, _) = flow.call(method, &path, Some(body), None);
            assert_eq!(status, 401, "未認証の{method} {path}は401: {resp}");
            expect_error_body(&resp, "unauthenticated");
        }
    }

    // -------------------------------------------------------------------
    // TASK-17 W3(TASK-22)書込API(rename・location)のhandler単体test。
    // テスト先行(実装前はrouting不在のfallback 404で実行failを許容)。

    /// W3 test用: ABH配下のregistry.jsonlへentity実体1件分のfile_write+file_uploadを
    /// 追記する(rename/location flowはregistry由来の現行stateで存在判定するため
    /// cacheでなくregistry側のseedが必要)。
    fn w3_seed_registry(env: &ApiEnv, uuid: &str, title: &str, category: &str, label: &str) {
        use crate::registry::{FileUploadRecord, FileWriteRecord};
        let reg = env.home.join("registry.jsonl");
        let s3_key = format!("{API_TEST_PREFIX}data/{uuid}/default.age");
        crate::registry::append_record(
            &reg,
            &FileWriteRecord {
                schema: 3,
                record_type: "file_write".into(),
                recorded_at: "2026-11-05T10:00:00+09:00".into(),
                batch_id: "20261105-abcdef01".into(),
                batch_date: "2026-11-05".into(),
                uuid: uuid.into(),
                title: title.into(),
                source_path: format!("/data/{title}"),
                source_name: format!("{title}.zip"),
                source_size: 100,
                source_mtime: "2026-11-05T09:00:00+09:00".into(),
                source_sha256: format!("a{uuid}").repeat(8).chars().take(64).collect(),
                age_path: format!("2026-11-05/{title}.zip.age"),
                age_size: 140,
                age_sha256: format!("b{uuid}").repeat(8).chars().take(64).collect(),
                recipients: vec!["SHA256:testfingerprint".into()],
                category: category.into(),
                label: label.into(),
                identity_hash: format!("hash-{uuid}"),
                s3_key: s3_key.clone(),
            },
        )
        .expect("file_write追記");
        crate::registry::append_record(
            &reg,
            &FileUploadRecord {
                schema: 2,
                record_type: "file_upload".into(),
                recorded_at: "2026-11-05T10:00:01+09:00".into(),
                batch_id: "20261105-abcdef01".into(),
                version_ts: "2026-11-05T10:00:01+09:00".into(),
                uuid: uuid.into(),
                title: title.into(),
                s3_key: s3_key.clone(),
                s3_bucket: API_TEST_BUCKET.into(),
                s3_region: "ap-northeast-1".into(),
                version_id: format!("vid-{uuid}"),
                s3_size: 140,
                s3_checksum_sha256: "c".repeat(43),
                s3_checksum_type: "FULL_OBJECT".into(),
                source_sha256: format!("a{uuid}").repeat(8).chars().take(64).collect(),
                age_sha256: format!("b{uuid}").repeat(8).chars().take(64).collect(),
                source_size: 100,
                age_size: 140,
                uploaded_at: "2026-11-05T10:00:02+09:00".into(),
            },
        )
        .expect("file_upload追記");
    }

    /// W3 test用: fake S3の呼出記録(calls.log)読み出し。
    fn w3_fake_calls() -> String {
        let state = std::env::var("AMAZING_BACKUP_FAKE_S3_STATE").expect("fake state env");
        std::fs::read_to_string(std::path::Path::new(&state).join("calls.log"))
            .unwrap_or_default()
    }

    /// [covers:server.entity-rename-endpoint]
    /// POST /api/entities/:uuid/rename: RenameRecord追記+索引再構築+PUT+DB反映+
    /// metadata sync(新titleキーPUT+旧キー掃除)。title規則違反422・不在404・
    /// title重複は許容+warning。audit_logへentity_renameを記録。
    #[test]
    fn api_entity_rename_writes_registry_and_index() {
        let env = api_env("srv-w3-rename");
        w3_seed_registry(&env, W2_ALPHA_UUID, "alpha", "docs", "work");
        w3_seed_registry(&env, W2_BETA_UUID, "beta", "docs", "work");
        let rename_path = format!("/api/entities/{W2_ALPHA_UUID}/rename");

        // 変更成功: 応答はEntityDetail形式(title=renamed・locations/tags/collectionsキー)
        let (status, body) =
            api_body("POST", &rename_path, serde_json::json!({ "title": "renamed" }));
        assert_eq!(status, 200, "renameは200: {body}");
        assert_eq!(
            body.get("title").and_then(|t| t.as_str()),
            Some("renamed"),
            "応答title=新title: {body}"
        );
        assert_eq!(
            body.get("uuid").and_then(|u| u.as_str()),
            Some(W2_ALPHA_UUID),
            "応答は対象entity: {body}"
        );
        assert!(
            body.get("locations").and_then(|l| l.as_array()).is_some_and(|l| !l.is_empty()),
            "応答はEntityDetail形式(locations付き): {body}"
        );
        assert!(
            body.get("tags").and_then(|t| t.as_array()).is_some(),
            "応答はtagsキー付き(W2語彙拡張形式): {body}"
        );
        assert!(
            body.get("warning").is_none(),
            "重複無しrenameにwarningは無い: {body}"
        );

        // registryへRenameRecord追記(old_title=alpha・new_title=renamed)
        let records =
            crate::registry::load_registry(&env.home.join("registry.jsonl")).expect("registry読込");
        let renames: Vec<&crate::registry::RenameRecord> = records
            .iter()
            .filter_map(|r| match r {
                crate::registry::Record::Rename(x) => Some(x),
                _ => None,
            })
            .collect();
        assert_eq!(renames.len(), 1, "rename記録1件: {records:?}");
        assert_eq!(renames[0].old_title, "alpha");
        assert_eq!(renames[0].new_title, "renamed");
        assert_eq!(renames[0].uuid, W2_ALPHA_UUID);
        assert_eq!(renames[0].scope, "api-bucket:amazing-backup/");
        assert!(!renames[0].recorded_at.is_empty());

        // 索引反映: 応答後のGET /api/entities/:uuidでも新title(cacheはfresh反映済み)
        let (status, got) = api("GET", &format!("/api/entities/{W2_ALPHA_UUID}"));
        assert_eq!(status, 200, "詳細取得: {got}");
        assert_eq!(
            got.get("title").and_then(|t| t.as_str()),
            Some("renamed"),
            "索引反映後の詳細も新title: {got}"
        );

        // 索引objectのPUTとmetadata新キーPUTが行われている
        let calls = w3_fake_calls();
        assert!(
            calls.contains(&format!(
                "s3 PutObject bucket={API_TEST_BUCKET} key={API_TEST_PREFIX}sys/index.jsonl.age"
            )),
            "索引再構築のPUT: {calls}"
        );
        assert!(
            calls.contains(&format!(
                "s3 PutObject bucket={API_TEST_BUCKET} key={API_TEST_PREFIX}data/{W2_ALPHA_UUID}/metadata_renamed.json"
            )),
            "metadata新titleキーPUT: {calls}"
        );

        // 2回目のrename: 旧titleキー(metadata_renamed)の掃除が行われる
        let (status, body) =
            api_body("POST", &rename_path, serde_json::json!({ "title": "final" }));
        assert_eq!(status, 200, "2回目rename: {body}");
        let calls = w3_fake_calls();
        assert!(
            calls.contains(&format!(
                "s3 PutObject bucket={API_TEST_BUCKET} key={API_TEST_PREFIX}data/{W2_ALPHA_UUID}/metadata_final.json"
            )),
            "metadata新titleキーPUT(2回目): {calls}"
        );
        assert!(
            calls.contains(&format!(
                "s3 DeleteObject bucket={API_TEST_BUCKET} key={API_TEST_PREFIX}data/{W2_ALPHA_UUID}/metadata_renamed.json"
            )),
            "旧titleキーの掃除: {calls}"
        );

        // title重複は許容+警告応答(betaをfinalへ=alphaと重複)
        let (status, body) = api_body(
            "POST",
            &format!("/api/entities/{W2_BETA_UUID}/rename"),
            serde_json::json!({ "title": "final" }),
        );
        assert_eq!(status, 200, "重複titleは許容: {body}");
        let warning = body.get("warning").and_then(|w| w.as_str()).unwrap_or_default();
        assert!(
            warning.contains("重複"),
            "warningに重複文言(CLIと同一形式): {body}"
        );

        // title規則違反は422
        for bad in ["bad/title", &"x".repeat(66), "bad\ntitle"] {
            let (status, body) =
                api_body("POST", &rename_path, serde_json::json!({ "title": bad }));
            assert_eq!(status, 422, "規則違反({bad:?})は422: {body}");
            expect_error_body(&body, "validation_failed");
        }
        // 不在uuidは404
        let (status, body) = api_body(
            "POST",
            &format!("/api/entities/{W2_ABSENT_UUID}/rename"),
            serde_json::json!({ "title": "x" }),
        );
        assert_eq!(status, 404, "不在uuidは404: {body}");
        expect_error_body(&body, "not_found");

        // audit_log: entity_renameのok/failed行
        let rows = w2_audit_log_rows(&env);
        let rename_rows: Vec<&(String, String, String, Option<String>, String, String)> = rows
            .iter()
            .filter(|r| r.0 == "entity_rename")
            .collect();
        assert!(rename_rows.len() >= 5, "成功3+失敗422x3+404の記録: {rows:?}");
        assert!(
            rename_rows.iter().any(|r| r.2 == "ok"
                && r.1.contains(W2_ALPHA_UUID)
                && r.1.contains("renamed")),
            "成功行(ok・detailにuuidと新title): {rows:?}"
        );
        assert!(
            rename_rows
                .iter()
                .any(|r| r.2 == "failed" && r.3.as_deref() == Some("validation_failed")),
            "422失敗行: {rows:?}"
        );
        assert!(
            rename_rows
                .iter()
                .any(|r| r.2 == "failed" && r.3.as_deref() == Some("not_found")),
            "404失敗行: {rows:?}"
        );
        drop(env);
    }

    /// [covers:server.entity-location-endpoint]
    /// POST /api/entities/:uuid/location: LocationUpdateRecord追記+索引再構築。
    /// (uuid, source_path)完全一致で対象所在を特定(不一致404)・category/label規則違反は
    /// 422・ラベルなし(空文字)は正当。audit_logへentity_location_updateを記録。
    #[test]
    fn api_entity_location_updates_classification() {
        let env = api_env("srv-w3-location");
        w3_seed_registry(&env, W2_ALPHA_UUID, "alpha", "docs", "work");
        let location_path = format!("/api/entities/{W2_ALPHA_UUID}/location");

        // 変更成功: 応答のlocationsは新分類
        let (status, body) = api_body(
            "POST",
            &location_path,
            serde_json::json!({
                "source_path": "/data/alpha",
                "category": "photos",
                "label": "sub",
            }),
        );
        assert_eq!(status, 200, "所在変更は200: {body}");
        let loc = body
            .get("locations")
            .and_then(|l| l.as_array())
            .and_then(|l| l.first())
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            loc.get("source_path").and_then(|p| p.as_str()),
            Some("/data/alpha"),
            "対象所在: {body}"
        );
        assert_eq!(
            loc.get("category").and_then(|c| c.as_str()),
            Some("photos"),
            "category上書き: {body}"
        );
        assert_eq!(
            loc.get("label").and_then(|l| l.as_str()),
            Some("sub"),
            "label上書き: {body}"
        );

        // registryへLocationUpdateRecord追記
        let records =
            crate::registry::load_registry(&env.home.join("registry.jsonl")).expect("registry読込");
        let updates: Vec<&crate::registry::LocationUpdateRecord> = records
            .iter()
            .filter_map(|r| match r {
                crate::registry::Record::LocationUpdate(x) => Some(x),
                _ => None,
            })
            .collect();
        assert_eq!(updates.len(), 1, "location_update記録1件: {records:?}");
        assert_eq!(updates[0].old_category, "docs");
        assert_eq!(updates[0].old_label, "work");
        assert_eq!(updates[0].new_category, "photos");
        assert_eq!(updates[0].new_label, "sub");
        assert_eq!(updates[0].source_path, "/data/alpha");

        // 索引反映: GET詳細でも新分類
        let (status, got) = api("GET", &format!("/api/entities/{W2_ALPHA_UUID}"));
        assert_eq!(status, 200, "詳細取得: {got}");
        let loc = got
            .get("locations")
            .and_then(|l| l.as_array())
            .and_then(|l| l.first())
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            loc.get("category").and_then(|c| c.as_str()),
            Some("photos"),
            "索引反映後の詳細も新分類: {got}"
        );

        // ラベルなし(空文字)は正当
        let (status, body) = api_body(
            "POST",
            &location_path,
            serde_json::json!({
                "source_path": "/data/alpha",
                "category": "photos",
                "label": "",
            }),
        );
        assert_eq!(status, 200, "ラベルなしは200: {body}");

        // source_path完全一致なしは404
        let (status, body) = api_body(
            "POST",
            &location_path,
            serde_json::json!({
                "source_path": "/data/elsewhere",
                "category": "photos",
                "label": "",
            }),
        );
        assert_eq!(status, 404, "不一致source_pathは404: {body}");
        expect_error_body(&body, "not_found");
        // 不在uuidは404
        let (status, body) = api_body(
            "POST",
            &format!("/api/entities/{W2_ABSENT_UUID}/location"),
            serde_json::json!({
                "source_path": "/data/alpha",
                "category": "photos",
                "label": "",
            }),
        );
        assert_eq!(status, 404, "不在uuidは404: {body}");
        expect_error_body(&body, "not_found");
        // category規則違反は422
        let (status, body) = api_body(
            "POST",
            &location_path,
            serde_json::json!({
                "source_path": "/data/alpha",
                "category": "Bad",
                "label": "",
            }),
        );
        assert_eq!(status, 422, "category規則違反は422: {body}");
        expect_error_body(&body, "validation_failed");
        // label規則違反(先頭/)は422
        let (status, body) = api_body(
            "POST",
            &location_path,
            serde_json::json!({
                "source_path": "/data/alpha",
                "category": "photos",
                "label": "/abs",
            }),
        );
        assert_eq!(status, 422, "label規則違反は422: {body}");
        expect_error_body(&body, "validation_failed");

        // audit_log: entity_location_updateのok/failed行
        let rows = w2_audit_log_rows(&env);
        let location_rows: Vec<&(String, String, String, Option<String>, String, String)> = rows
            .iter()
            .filter(|r| r.0 == "entity_location_update")
            .collect();
        assert!(location_rows.len() >= 5, "成功2+404x2+422x2の記録: {rows:?}");
        assert!(
            location_rows.iter().any(|r| r.2 == "ok" && r.1.contains(W2_ALPHA_UUID)),
            "成功行: {rows:?}"
        );
        assert!(
            location_rows
                .iter()
                .any(|r| r.2 == "failed" && r.3.as_deref() == Some("not_found")),
            "404失敗行: {rows:?}"
        );
        assert!(
            location_rows
                .iter()
                .any(|r| r.2 == "failed" && r.3.as_deref() == Some("validation_failed")),
            "422失敗行: {rows:?}"
        );
        drop(env);
    }

    /// [covers:server.entities-untagged-filter]
    /// [covers:server.entities-list-endpoint]
    /// GET /api/entitiesは?untagged=1でtagを1つも持たないentityのみへ絞り込む
    /// (設計2.2「tag無しも選択可」=コードレビュー中9)。?tag=との同時指定・untagged
    /// 値が1以外は400 invalid_request。?label=(空文字)はラベルなし(location.label="")
    /// の完全一致として機能する(コードレビュー中10(b))。
    #[test]
    fn api_entities_untagged_filter_and_empty_label() {
        let env = api_env("srv-api-w2-untagged");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        let beta = api_entry(W2_BETA_UUID, "beta", "unfiled", "");
        let gamma = api_entry(W2_GAMMA_UUID, "gamma", "docs", "");
        api_seed(&[alpha, beta, gamma], API_TEST_BUCKET, API_TEST_PREFIX);
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_ALPHA_UUID}/tags"),
            serde_json::json!({ "tags": ["docs"] }),
        );
        assert_eq!(status, 200, "alphaへtag付与(事前): {body}");
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_BETA_UUID}/tags"),
            serde_json::json!({ "tags": ["docs"] }),
        );
        assert_eq!(status, 200, "betaへtag付与(事前): {body}");

        // untagged=1はtag無しentity(gamma)のみ
        let (status, body) = api("GET", "/api/entities?untagged=1");
        assert_eq!(status, 200, "?untagged=1は200: {body}");
        assert_eq!(
            uuids_of(&body),
            vec![W2_GAMMA_UUID.to_string()],
            "tag無しentityはgammaのみ: {body}"
        );
        // tag=との同時指定は400 invalid_request
        let (status, body) = api("GET", "/api/entities?tag=docs&untagged=1");
        assert_eq!(status, 400, "tag+untagged同時指定は400: {body}");
        expect_error_body(&body, "invalid_request");
        // untaggedの値は1のみ(0等は400)
        let (status, body) = api("GET", "/api/entities?untagged=0");
        assert_eq!(status, 400, "untagged=0は400: {body}");
        expect_error_body(&body, "invalid_request");
        // 他絞り込み(label)との併用はAND(gammaはlabel無しのため空)
        let (status, body) = api("GET", "/api/entities?untagged=1&label=work");
        assert_eq!(status, 200, "untagged+label併用は200: {body}");
        assert!(
            uuids_of(&body).is_empty(),
            "label=work×untaggedはgamma(label無し)が弾かれ空: {body}"
        );

        // ?label=(空文字)はラベルなしの完全一致(beta+gamma)
        let (status, body) = api("GET", "/api/entities?label=");
        assert_eq!(status, 200, "?label=空文字は200: {body}");
        let mut uuids = uuids_of(&body);
        uuids.sort();
        assert_eq!(
            uuids,
            vec![W2_BETA_UUID.to_string(), W2_GAMMA_UUID.to_string()],
            "label=空文字はラベルなしentity(beta+gamma): {body}"
        );
        drop(env);
    }

    /// [covers:server.tags-list]
    /// [covers:server.collections-list-and-detail]
    /// tag_counts・collections一覧のentity_countは同一scopeの現存entities(cache)との
    /// JOINで集計する(コードレビュー中11: sync/rebuildでcacheから消えたentityのtag行・
    /// 構成行は件数に計上しない。行自体は永続classのため保持される)。
    #[test]
    fn api_counts_join_with_existing_entities() {
        let env = api_env("srv-api-w2-stale-join");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_ALPHA_UUID}/tags"),
            serde_json::json!({ "tags": ["docs"] }),
        );
        assert_eq!(status, 200, "alphaへtag付与(事前): {body}");
        // cacheに無いuuidのtag行・構成行(直接注入=sync/rebuildで消えたentity相当)
        w2_insert_tag_row(&env, "api-bucket:amazing-backup/", W2_ABSENT_UUID, "docs");
        w2_insert_tag_row(&env, "api-bucket:amazing-backup/", W2_ABSENT_UUID, "ghost");
        let ghost_id = w2_insert_collection_row(&env, "api-bucket:amazing-backup/", "ghost");
        {
            let conn = rusqlite::Connection::open(env.home.join("catalog.db"))
                .expect("catalog.db open");
            conn.execute(
                "INSERT INTO collection_items (collection_id, scope, uuid, added_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    ghost_id,
                    "api-bucket:amazing-backup/",
                    W2_ABSENT_UUID,
                    "2026-11-05T12:00:00+09:00"
                ],
            )
            .expect("collection_items行挿入");
        }

        // tags一覧: docsは現存alphaのみ計上(1)・ghostは現存entityが0のため一覧に出ない
        let (status, body) = api("GET", "/api/tags");
        assert_eq!(status, 200, "GET /api/tagsは200: {body}");
        assert_eq!(
            tags_of(&body),
            vec![("docs".to_string(), 1)],
            "cache外uuidのtag行は計上しない(ghostは一覧に出ない): {body}"
        );
        // collections一覧: ghostのentity_countは0(現存entityのみ計上)
        let (status, body) = api("GET", "/api/collections");
        assert_eq!(status, 200, "GET /api/collectionsは200: {body}");
        let cols = body
            .get("collections")
            .and_then(|v| v.as_array())
            .expect("collections配列");
        assert_eq!(cols.len(), 1, "collectionはghostのみ: {body}");
        assert_eq!(
            cols[0].get("name").and_then(|n| n.as_str()),
            Some("ghost"),
            "ghost: {body}"
        );
        assert_eq!(
            cols[0].get("entity_count").and_then(|c| c.as_i64()),
            Some(0),
            "cache外uuidの構成はentity_countに計上しない: {body}"
        );
        drop(env);
    }

    /// [covers:server.collection-crud]
    /// [covers:server.entity-tags-replace]
    /// 構成を持つcollectionの改名応答は操作後の実際の構成uuid一覧を含む(コード
    /// レビュー高2=GET詳細・設計3.1「操作後の対象状態」と一致)。PUT tagsのbody内
    /// 重複は正規化(set化)して受理(コードレビュー中5)。
    #[test]
    fn api_rename_response_includes_uuids_and_tags_deduped() {
        let env = api_env("srv-api-w2-review-fix");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);
        let id = w2_create_collection("grp");
        let (status, body) = api_body(
            "POST",
            &format!("/api/collections/{id}/items"),
            serde_json::json!({ "uuids": [W2_ALPHA_UUID] }),
        );
        assert_eq!(status, 200, "alpha追加(事前): {body}");

        // 構成持ちcollectionの改名応答のuuidsは空でなく操作後の構成(高2)
        let (status, body) = api_body(
            "PUT",
            &format!("/api/collections/{id}"),
            serde_json::json!({ "name": "grp2" }),
        );
        assert_eq!(status, 200, "構成持ち改名は200: {body}");
        let renamed = body.get("collection").expect("改名応答は詳細schema");
        assert_eq!(
            renamed.get("uuids").and_then(|u| u.as_array()).map(|a| a.len()),
            Some(1),
            "改名応答のuuidsは操作後の構成(空でない): {body}"
        );
        // GET詳細と同一内容(3.1「書込応答は読取系と同一schema」)
        let (status, detail) = api("GET", &format!("/api/collections/{id}"));
        assert_eq!(status, 200, "詳細は200: {detail}");
        assert_eq!(
            renamed.get("uuids"),
            detail.get("collection").and_then(|c| c.get("uuids")),
            "改名応答のuuidsはGET詳細と同一: {body} {detail}"
        );

        // body内重複tagは正規化して受理(200・tagsは1件)
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_ALPHA_UUID}/tags"),
            serde_json::json!({ "tags": ["docs", "docs"] }),
        );
        assert_eq!(status, 200, "重複tag bodyは正規化して200: {body}");
        assert_eq!(
            body.get("tags").and_then(|t| t.as_array()).map(|a| a.len()),
            Some(1),
            "重複は正規化されdocsの1件: {body}"
        );
        drop(env);
    }

    /// [covers:server.write-api-audit-log-recorded]
    /// 成功audit記録は書込操作と同一transactionで確定し・記録不能な場合は操作も
    /// rollbackされて500 internalで応答する(コードレビュー中4: 成功応答した変更が
    /// 履歴に残らない状況を作らない)。
    #[test]
    fn api_write_rolls_back_when_audit_recording_fails() {
        let env = api_env("srv-api-w2-audit-atomic");
        let alpha = api_entry(W2_ALPHA_UUID, "alpha", "docs", "work");
        api_seed(&[alpha], API_TEST_BUCKET, API_TEST_PREFIX);
        // audit_log表をDROP(記録不能状況の注入。migrationはuser_version済みのため
        // handler内のopenでは再作成されない)
        {
            let conn = rusqlite::Connection::open(env.home.join("catalog.db"))
                .expect("catalog.db open");
            conn.execute("DROP TABLE audit_log", [])
                .expect("audit_log表DROP");
        }
        let (status, body) = api_body(
            "PUT",
            &format!("/api/entities/{W2_ALPHA_UUID}/tags"),
            serde_json::json!({ "tags": ["docs"] }),
        );
        assert_eq!(status, 500, "audit記録不能時の書込は500 internal: {body}");
        expect_error_body(&body, "internal");
        // 操作はrollbackされている(tagsは空のまま)
        let (status, body) = api("GET", &format!("/api/entities/{W2_ALPHA_UUID}"));
        assert_eq!(status, 200, "詳細は200: {body}");
        assert_eq!(
            body.get("tags").and_then(|t| t.as_array()).map(|a| a.len()),
            Some(0),
            "書込はrollbackされtagsは空のまま: {body}"
        );
        drop(env);
    }
}
