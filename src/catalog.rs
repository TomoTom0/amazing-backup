//! TASK-9 local catalog DB層(tmp/tasks/tm-9/design_catalog-frontend-auth.md
//! 2章(DB設計・D1/D11)・4章(freshness整合規則)・12章Phase P1)。
//! SQLite(rusqlite bundled+rusqlite_migration・WAL mode)へS3索引replica(復号後の
//! 平文view・D11承認)を保持し、照会系の毎回S3取得を検証済みfresh時のみ省略する。
//! 真正はJSONL registryのまま(DBは導出index+S3索引replica=cache。常にJSONL+S3から
//! 再構築可能)。
//!
//! table契約(2章・指摘#2):
//! - cache class(削除再構築可): entities/versions/pins/locations/sync_state/pending_items
//! - 永続class(rebuildでもdropしない): credentials/passkey・push_intents・api_keys・
//!   tags/collections/collection_items/audit_log(TASK-17 W2=local語彙とUI書込操作の
//!   履歴。cache側に置くとrebuildでtag・履歴が失れるため)・
//!   jobs(TASK-17 W4=長時間jobの実行記録の正本。設計4.1「再構築不能の実行記録」)
//! 全cache tableはscope(bucket+prefix)をPK/検索条件に持ち(指摘#3)、scope単位の
//! 置換は単一transactionでdelete+insertする(原子的置換・途中状態を作らない)。

use crate::errors::{BackupError, BackupResult};
use crate::index::{
    INDEX_SCHEMA, IndexFileEntry, IndexHeader, IndexLocation, IndexPin, IndexVersion,
};
use crate::s3::S3Backend;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};

/// catalog DB file名。
pub const DB_FILE_NAME: &str = "catalog.db";

/// scope検索キーの区切り文字(bucket名に使用できない制御文字のため分割は一意)。
const SCOPE_SEP: char = '\u{1f}';

/// scope(bucket+prefix)のcache table検索キー。
pub fn scope_key(bucket: &str, prefix: &str) -> String {
    format!("{bucket}{SCOPE_SEP}{prefix}")
}

/// catalog DBのpath(resolve_app_pathsのregistryと同一規則・設計2章。
/// AMAZING_BACKUP_HOME指定時$ABH/catalog.db・未指定時
/// $XDG_STATE_HOME/amazing-backup/catalog.db)。
pub fn catalog_db_path() -> PathBuf {
    let paths = crate::config::resolve_app_paths();
    paths
        .registry
        .parent()
        .map(|p| p.join(DB_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(DB_FILE_NAME))
}

/// 索引objectのvalidator(設計4章: ETag+VersionId。HEAD応答由来)。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Validator {
    pub etag: String,
    pub version_id: String,
}

impl Validator {
    pub fn from_head(head: &crate::s3::HeadInfo) -> Validator {
        Validator {
            etag: head.etag.clone().unwrap_or_default(),
            version_id: head.version_id.clone().unwrap_or_default(),
        }
    }
}

/// sync_stateのfreshness値(設計4章)。
pub const STATUS_FRESH: &str = "fresh";
pub const STATUS_STALE: &str = "stale";
pub const STATUS_ERROR: &str = "error";

/// sync_state 1行分の読み出し結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState {
    pub index_etag: String,
    pub index_version_id: String,
    pub fetched_at: String,
    pub last_write_at: String,
    pub last_verified_at: String,
    pub status: String,
    pub entry_count: i64,
    /// 索引headerの保存値(cache hit時にheaderを忠実に再構築するため)
    pub index_updated_at: String,
    pub index_run_id: String,
}

impl SyncState {
    /// HEAD応答のvalidatorと一致するか。
    fn validator_matches(&self, v: &Validator) -> bool {
        self.index_etag == v.etag && self.index_version_id == v.version_id
    }
}

/// 索引snapshotのDB反映由来(sync_stateのfetched_at/last_write_atどちらを刻むか)。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReflectOrigin {
    /// S3索引replicaの取得(manage catalog sync・cache miss時の照会系反映)
    Fetched,
    /// 自端末書込直後の同一process内反映(push/pin/prune等・設計4章の
    /// 「同一process内で自端末書込直後のみ無条件fresh」)
    Written,
}

/// file_write記録由来のsource_mtime(設計2章※: versions/locationsの比較基準供給源)。
/// S3索引のentry自体はmtimeを持たないため、registry手元にある場合のみ供給する。
#[derive(Default)]
pub struct SourceMtimes {
    by_identity: std::collections::HashMap<String, String>,
    by_uuid_sha: std::collections::HashMap<(String, String), String>,
}

impl SourceMtimes {
    /// 空のlookup(S3索引replica取得経由=mtime供給源なし)。
    pub fn empty() -> SourceMtimes {
        SourceMtimes::default()
    }

    /// registry reduce結果からlookupを構築(自端末書込反映経由)。
    pub fn from_reduced(reduced: &crate::registry::Reduced) -> SourceMtimes {
        let mut mt = SourceMtimes::default();
        for w in &reduced.file_writes {
            mt.by_identity
                .insert(w.identity_hash.clone(), w.source_mtime.clone());
            mt.by_uuid_sha.insert(
                (w.uuid.clone(), w.source_sha256.clone()),
                w.source_mtime.clone(),
            );
        }
        mt
    }

    fn of_location(&self, l: &IndexLocation) -> String {
        self.by_identity
            .get(&l.identity_hash)
            .cloned()
            .unwrap_or_default()
    }

    fn of_version(&self, uuid: &str, v: &IndexVersion) -> String {
        self.by_uuid_sha
            .get(&(uuid.to_string(), v.source_sha256.clone()))
            .cloned()
            .unwrap_or_default()
    }
}

/// JST現在時刻(ISO8601 +09:00付き・storage::now_jstと同一形式)。
fn now_jst() -> String {
    crate::targets::format_jst(&crate::targets::now_jst())
}

// ---------------------------------------------------------------------------
// migration(設計2章schema要約。cache class 6 table+永続class 2 table)

/// 初回open時のmigration適用のprocess内直列化(並行openのcheck-then-act競合
/// 防止。W4。ロックは適用済みDBのopenでは即時に通る=常用経路の直列化ではない)
static OPEN_MIGRATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn migrations() -> rusqlite_migration::Migrations<'static> {
    use rusqlite_migration::M;
    rusqlite_migration::Migrations::new(vec![
        M::up(
            r#"
            CREATE TABLE entities (
                scope TEXT NOT NULL,
                uuid TEXT NOT NULL,
                title TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                PRIMARY KEY (scope, uuid));
            CREATE TABLE versions (
                scope TEXT NOT NULL,
                s3_key TEXT NOT NULL,
                version_id TEXT NOT NULL,
                uuid TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                date TEXT NOT NULL,
                version_ts TEXT NOT NULL,
                batch_id TEXT NOT NULL,
                source_sha256 TEXT NOT NULL,
                age_sha256 TEXT NOT NULL,
                source_size INTEGER NOT NULL,
                age_size INTEGER NOT NULL,
                source_mtime TEXT NOT NULL,
                PRIMARY KEY (scope, s3_key, version_id),
                FOREIGN KEY (scope, uuid) REFERENCES entities (scope, uuid) ON DELETE CASCADE);
            CREATE TABLE pins (
                scope TEXT NOT NULL,
                pin TEXT NOT NULL,
                s3_key TEXT NOT NULL,
                uuid TEXT NOT NULL,
                created_at TEXT NOT NULL,
                date TEXT NOT NULL,
                source_sha256 TEXT NOT NULL,
                age_sha256 TEXT NOT NULL,
                source_size INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                PRIMARY KEY (scope, s3_key),
                FOREIGN KEY (scope, uuid) REFERENCES entities (scope, uuid) ON DELETE CASCADE);
            CREATE TABLE locations (
                scope TEXT NOT NULL,
                uuid TEXT NOT NULL,
                source_path TEXT NOT NULL,
                category TEXT NOT NULL,
                label TEXT NOT NULL,
                identity_hash TEXT NOT NULL,
                last_backup TEXT NOT NULL,
                source_mtime TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                PRIMARY KEY (scope, uuid, source_path),
                FOREIGN KEY (scope, uuid) REFERENCES entities (scope, uuid) ON DELETE CASCADE);
            CREATE TABLE sync_state (
                scope TEXT PRIMARY KEY,
                index_etag TEXT NOT NULL,
                index_version_id TEXT NOT NULL,
                fetched_at TEXT NOT NULL,
                last_write_at TEXT NOT NULL,
                last_verified_at TEXT NOT NULL,
                status TEXT NOT NULL,
                entry_count INTEGER NOT NULL,
                index_updated_at TEXT NOT NULL,
                index_run_id TEXT NOT NULL);
            CREATE TABLE pending_items (
                scope TEXT NOT NULL,
                source_root TEXT NOT NULL,
                path TEXT NOT NULL,
                size INTEGER NOT NULL,
                mtime TEXT NOT NULL,
                status TEXT NOT NULL,
                last_scan_at TEXT NOT NULL,
                PRIMARY KEY (scope, source_root, path));
            CREATE TABLE credentials (
                credential_id TEXT PRIMARY KEY,
                passkey_json TEXT NOT NULL,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL);
            CREATE TABLE push_intents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                recorded_at TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload_json TEXT NOT NULL);
            "#,
        ),
        // TASK-9 12章P3: POST /api/diffの最終結果保存(設計6章「結果をDB保存」)。
        // cache class(再実行で置換される導出物)・scope単位に最新1件のみ保持する。
        // manage catalog rebuildの置換対象(2章cache class列挙)には含めない(索引由来
        // でなく突合実行由来のため。突合結果は常に再実行で再構築可能)
        M::up(
            r#"
            CREATE TABLE diff_results (
                scope TEXT PRIMARY KEY,
                performed_at TEXT NOT NULL,
                result_json TEXT NOT NULL);
            "#,
        ),
        // TASK-13(api-key認証・D12): api-keyのSHA-256 hash一覧。永続classのため
        // manage catalog rebuildのcache class置換でもdropしない。平文keyは保存
        // しない(key_hashから復元不可)。M::up listへの追加で既存DBにも適用
        M::up(
            r#"
            CREATE TABLE api_keys (
                credential_id TEXT PRIMARY KEY,
                key_hash TEXT NOT NULL,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL);
            "#,
        ),
        // TASK-17 W2(TASK-19・設計4.1): tag/collection(local語彙)とUI書込操作の
        // 履歴。4 tableとも永続class(cache側に置くとmanage catalog rebuildで
        // tag・履歴が失われるため=replace_scopeの置換対象外)。scopeはAPI表示と
        // 同一の<bucket>:<prefix>形式(cache tableのscope_key区切りと異なる点に
        // 注意。条件書server.write-api-audit-log-recordedの契約)
        M::up(
            r#"
            CREATE TABLE tags (
                scope TEXT NOT NULL,
                uuid TEXT NOT NULL,
                tag TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (scope, uuid, tag));
            CREATE TABLE collections (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE (scope, name));
            CREATE TABLE collection_items (
                collection_id INTEGER NOT NULL,
                scope TEXT NOT NULL,
                uuid TEXT NOT NULL,
                added_at TEXT NOT NULL,
                PRIMARY KEY (collection_id, scope, uuid),
                FOREIGN KEY (collection_id) REFERENCES collections (id) ON DELETE CASCADE);
            CREATE TABLE audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                recorded_at TEXT NOT NULL,
                action TEXT NOT NULL,
                detail_json TEXT NOT NULL,
                outcome TEXT NOT NULL,
                error_code TEXT);
            "#,
        ),
        // TASK-17 W4(TASK-27・設計4.1・5章): 長時間job(get/push)の正本table。
        // 永続class(実行記録はregistry+S3から再構築不能のため=4.1 class分類。
        // cache側に置くとmanage catalog rebuildでjob履歴が失われる)。scopeは
        // tags/audit_logと同一の<bucket>:<prefix>形式。status値はserver側で
        // queued/running/waiting_restore/succeeded/failed/interrupted/cancelled。
        // progress_jsonは{phase,total,done,current_path}(waiting_restore時は
        // next_check_atを追加)でUI表示とtimer確認時刻の双方に使う
        M::up(
            r#"
            CREATE TABLE jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                params_json TEXT NOT NULL,
                progress_json TEXT NOT NULL,
                result_json TEXT,
                error_code TEXT,
                error_message TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL);
            "#,
        ),
    ])
}

/// file権限0600の適用(D11: 平文replicaを含むための権限境界)。chmod失敗は
/// エラーにする(権限境界の保証を黙って諦めない=他user所有等で直せない配置は
/// 設定エラーとして起動拒否が安全側)。WAL/SHMは存在する場合のみ適用
/// (open直後は未作成の場合があるため)。
fn apply_private_permissions(db_path: &Path) -> BackupResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let apply = |p: &Path| -> BackupResult<()> {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
                BackupError::Config(format!(
                    "catalog DBの権限を0600へ設定できません({}): {e}\
                     (他user所有の配置が疑われる場合は配置を確認してください)",
                    p.display()
                ))
            })
        };
        apply(db_path)?;
        for ext in ["-wal", "-shm"] {
            let mut p = db_path.as_os_str().to_os_string();
            p.push(ext);
            let side = Path::new(&p);
            if side.exists() {
                apply(side)?;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = db_path;
    }
    Ok(())
}

/// 既存DB fileの権限0600保証(open前=平文露出の時間窓を開けない)。unix以外は
/// chmod相当が無いため何もしない。
fn ensure_private_db_file(db_path: &Path) -> BackupResult<()> {
    #[cfg(unix)]
    if db_path.exists() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            BackupError::Config(format!(
                "catalog DB fileの権限を0600へ設定できません({}): {e}",
                db_path.display()
            ))
        })?;
    }
    #[cfg(not(unix))]
    {
        let _ = db_path;
    }
    Ok(())
}

/// DB親dirの作成(不在時のみ・0700。設計2章: 専用directoryは0700で作成)。
/// 既存dirは0700で無ければchmod 0700で是正する(既存0755 dirを残さない)。
fn ensure_private_parent(db_path: &Path) -> BackupResult<()> {
    let Some(parent) = db_path.parent() else {
        return Ok(());
    };
    if parent.is_dir() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(parent).map_err(|e| {
                BackupError::Config(format!(
                    "catalog DB dirの読取に失敗({}): {e}",
                    parent.display()
                ))
            })?;
            if meta.permissions().mode() & 0o777 != 0o700 {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(
                    |e| {
                        BackupError::Config(format!(
                            "catalog DB dirの権限を0700へ設定できません({}): {e}",
                            parent.display()
                        ))
                    },
                )?;
            }
        }
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent).map_err(|e| {
            BackupError::Config(format!(
                "catalog DB dirの作成に失敗({}): {e}",
                parent.display()
            ))
        })?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            BackupError::Config(format!(
                "catalog DB dirの作成に失敗({}): {e}",
                parent.display()
            ))
        })?;
    }
    Ok(())
}

/// 開いたcatalog DB接続(migration済み・WAL mode)。
pub struct CatalogDb {
    conn: Connection,
}

/// credentials tableの1行(永続class・P4 passkey認証の保存先。passkey_jsonは
/// webauthn-rs Passkeyのserde JSON)。
pub struct CredentialRow {
    pub credential_id: String,
    pub passkey_json: String,
    pub name: String,
    pub created_at: String,
}

/// api_keys tableの1行(TASK-13 D12: api-key認証の保存先=永続class。key_hashは
/// SHA-256(api-key全文)のhexで平文keyは保存しない=hashから復元不可)。
pub struct ApiKeyRow {
    pub credential_id: String,
    pub key_hash: String,
    pub name: String,
    pub created_at: String,
}

/// collections一覧の1行(TASK-17 W2・GET /api/collectionsの要素)。
pub struct CollectionRow {
    pub id: i64,
    pub name: String,
    /// 構成entity数(collection_itemsの行数)
    pub entity_count: i64,
}

/// collection詳細(TASK-17 W2・GET /api/collections/:id系の応答要素。
/// uuidsは追加順)。
pub struct CollectionDetail {
    pub id: i64,
    pub name: String,
    pub uuids: Vec<String>,
}

/// jobs tableの1行(TASK-17 W4・設計4.1/5章。job一覧・詳細の要素で
/// params_json/progress_json/result_jsonはJSON文字列のまま保持し、
/// 応答組立(server側)でparseする)。
#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: i64,
    pub scope: String,
    pub kind: String,
    pub status: String,
    pub params_json: String,
    pub progress_json: String,
    pub result_json: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// W2 tag/collection書込の失敗区分(TASK-17 W2。storage層は状態遷移の可否だけを
/// 返し、HTTPのerror codeへの変換はserver側が行う)。
#[derive(Debug)]
pub enum W2WriteError {
    /// 同一(scope,name)等の重複(UNIQUE制約由来=409 duplicate)
    Duplicate,
    /// 対象(collection・構成uuid)が当該scopeに不在(404 not_found)
    NotFound,
    /// それ以外の失敗(内部error)
    Other(BackupError),
}

impl From<BackupError> for W2WriteError {
    fn from(e: BackupError) -> W2WriteError {
        W2WriteError::Other(e)
    }
}

/// W2 table群(tags/collections/collection_items/audit_log)のscope値。
/// API表示と同一の<bucket>:<prefix>形式(bucket名にコロンは使用できないため
/// 一意。cache tableのscope_key区切り文字とも衝突しない)。
fn w2_scope(bucket: &str, prefix: &str) -> String {
    format!("{bucket}:{prefix}")
}

impl CatalogDb {
    /// 規定path(catalog_db_path)で開く。新規作成時はdir 0700・file 0600で作成する。
    pub fn open() -> BackupResult<CatalogDb> {
        CatalogDb::open_at(&catalog_db_path())
    }

    /// 指定pathで開く(unit test・seam用)。
    pub fn open_at(db_path: &Path) -> BackupResult<CatalogDb> {
        ensure_private_parent(db_path)?;
        // 既存fileはopen前に0600へ是正(0644等の平文露出状態で接続しない)。
        // 新規fileは0600で作成してからopen
        ensure_private_db_file(db_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if !db_path.exists() {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(db_path)
                {
                    Ok(_) => {}
                    // 並行openとのcreate_new競合(他の接続が同一瞬間に作成)は
                    // 作成済みfileへそのまま接続する(W4: job workerとhandlerが
                    // 同一fresh DBへ並行openし得るため)
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => {
                        return Err(BackupError::Config(format!(
                            "catalog DB fileの作成に失敗({}): {e}",
                            db_path.display()
                        )));
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            if !db_path.exists() {
                std::fs::File::create(db_path).map_err(|e| {
                    BackupError::Config(format!(
                        "catalog DB fileの作成に失敗({}): {e}",
                        db_path.display()
                    ))
                })?;
            }
        }
        let open_db = || -> Result<Connection, rusqlite::Error> {
            let conn = Connection::open(db_path)?;
            // busy_timeoutを最初に設定する(以降のWAL設定・migrationの書込衝突も
            // 即時SQLITE_BUSYでなく待機に変える。W4: job workerの更新とhandlerの
            // 読書が並行し得るため。書込系APIはsemaphoreで直列化されるが読取と
            // 書込・初回open同士の並行は排他されない)
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            // WAL mode(設計2章)。journal_modeは値を返すpragmaのためqueryで実行する
            conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            Ok(conn)
        };
        let mut conn = open_db().map_err(|e| {
            BackupError::Config(format!("catalog DBを開けない({}): {e}", db_path.display()))
        })?;
        // 初回openのmigration適用はprocess内で直列化する(migrationは
        // user_version確認→CREATEのcheck-then-actのため・2接続が同一fresh DBへ
        // 並行適用すると「table already exists」で失敗し得る。W4: job workerと
        // handlerが同一fresh DBへ並行openし得る。既に適用済みのopenは
        // user_version一致で直ちに通るためlockの実costは初回のみ)
        let _migration_lock = OPEN_MIGRATION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        migrations()
            .to_latest(&mut conn)
            .map_err(|e| BackupError::Config(format!("catalog DBのmigrationに失敗: {e}")))?;
        // open後に作られるWAL/SHMも同一の権限境界へ(失敗はエラー)
        apply_private_permissions(db_path)?;
        Ok(CatalogDb { conn })
    }

    /// 現在のjournal_mode(unit test用)。
    pub fn journal_mode(&self) -> BackupResult<String> {
        self.conn
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .map_err(|e| BackupError::Internal(format!("journal_modeの読取に失敗: {e}")))
    }

    /// 永続class tableへの行挿入(unit test・P4/P5までの契約保持検証用)。
    pub fn insert_credential_for_test(&self, credential_id: &str) -> BackupResult<()> {
        self.conn
            .execute(
                "INSERT INTO credentials (credential_id, passkey_json, name, created_at)\
                 VALUES (?1, '{}', 'test', '2026-11-05T00:00:00+09:00')",
                [credential_id],
            )
            .map_err(|e| BackupError::Internal(format!("credentials挿入に失敗: {e}")))?;
        Ok(())
    }

    /// credentials行数(unit test用)。
    pub fn credential_count(&self) -> BackupResult<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM credentials", [], |r| r.get(0))
            .map_err(|e| BackupError::Internal(format!("credentials計数に失敗: {e}")))
    }

    /// passkey credentialの保存(設計8章: credentials tableへPasskeyを通常のserdeで
    /// 保存=永続class。danger-allow-state-serialisation不使用)。同一credential_idは
    /// 上書き(re-register時の置換)。
    pub fn upsert_credential(
        &self,
        credential_id: &str,
        passkey_json: &str,
        name: &str,
    ) -> BackupResult<()> {
        self.conn
            .execute(
                "INSERT INTO credentials (credential_id, passkey_json, name, created_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(credential_id) DO UPDATE SET \
                 passkey_json = excluded.passkey_json, \
                 name = excluded.name, \
                 created_at = excluded.created_at",
                rusqlite::params![credential_id, passkey_json, name, crate::storage::now_jst()],
            )
            .map_err(|e| BackupError::Internal(format!("credentials保存に失敗: {e}")))?;
        Ok(())
    }

    /// credentials行の一覧(P4 loginのallowCredentials組立)。
    pub fn load_credentials(&self) -> BackupResult<Vec<CredentialRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT credential_id, passkey_json, name, created_at FROM credentials \
                 ORDER BY created_at, credential_id",
            )
            .map_err(|e| BackupError::Internal(format!("credentials読取の準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CredentialRow {
                    credential_id: r.get(0)?,
                    passkey_json: r.get(1)?,
                    name: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })
            .map_err(|e| BackupError::Internal(format!("credentials読取に失敗: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| BackupError::Internal(format!("credentials行の取り出しに失敗: {e}")))
    }

    /// credentialの失効(TASK-9設計8章・12章P6: `manage auth remove`用。
    /// credential_id完全一致の1行を削除するlocal操作。戻り値は削除行数
    /// (0=対象不在=呼び出し側が対象エラー扱いにする))
    pub fn remove_credential(&self, credential_id: &str) -> BackupResult<usize> {
        self.conn
            .execute(
                "DELETE FROM credentials WHERE credential_id = ?1",
                [credential_id],
            )
            .map_err(|e| BackupError::Internal(format!("credentials削除に失敗: {e}")))
    }

    /// api-keyの保存(TASK-13設計2.1: `manage auth api-key`用)。credential_idは
    /// "ak-"+16hex・key_hashはSHA-256(api-key全文)のhex(平文keyは受け取らない
    /// =保存しない)。
    pub fn insert_api_key(
        &self,
        credential_id: &str,
        key_hash: &str,
        name: &str,
    ) -> BackupResult<()> {
        self.conn
            .execute(
                "INSERT INTO api_keys (credential_id, key_hash, name, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![credential_id, key_hash, name, crate::storage::now_jst()],
            )
            .map_err(|e| BackupError::Internal(format!("api_keys挿入に失敗: {e}")))?;
        Ok(())
    }

    /// api_keys行数(manage auth list/removeの両table合計0件判定・auth statusの
    /// api_keys件数)。
    pub fn api_key_count(&self) -> BackupResult<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM api_keys", [], |r| r.get(0))
            .map_err(|e| BackupError::Internal(format!("api_keys計数に失敗: {e}")))
    }

    /// api_keys行の一覧(manage auth listの統合表示用)。
    pub fn load_api_keys(&self) -> BackupResult<Vec<ApiKeyRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT credential_id, key_hash, name, created_at FROM api_keys \
                 ORDER BY created_at, credential_id",
            )
            .map_err(|e| BackupError::Internal(format!("api_keys読取の準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ApiKeyRow {
                    credential_id: r.get(0)?,
                    key_hash: r.get(1)?,
                    name: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })
            .map_err(|e| BackupError::Internal(format!("api_keys読取に失敗: {e}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| BackupError::Internal(format!("api_keys行の取り出しに失敗: {e}")))
    }

    /// api-keyの失効(TASK-13設計2.5: `manage auth remove`用。credentials→api_keys
    /// の順の削除を受け持つ側)。戻り値は削除行数(0=対象不在)。
    pub fn remove_api_key(&self, credential_id: &str) -> BackupResult<usize> {
        self.conn
            .execute(
                "DELETE FROM api_keys WHERE credential_id = ?1",
                [credential_id],
            )
            .map_err(|e| BackupError::Internal(format!("api_keys削除に失敗: {e}")))
    }

    /// key_hash一致のapi-key検索(TASK-13設計2.3: serveのapi-key認証。
    /// SHA-256 hash一致=key一致のため検索はhashのみ)。
    pub fn find_api_key_by_hash(&self, key_hash: &str) -> BackupResult<Option<ApiKeyRow>> {
        self.conn
            .query_row(
                "SELECT credential_id, key_hash, name, created_at FROM api_keys \
                 WHERE key_hash = ?1",
                [key_hash],
                |r| {
                    Ok(ApiKeyRow {
                        credential_id: r.get(0)?,
                        key_hash: r.get(1)?,
                        name: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                },
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(BackupError::Internal(format!("api_keys検索に失敗: {other}"))),
            })
    }

    /// sync_state読み出し(行が無いNone)。
    pub fn sync_state(&self, bucket: &str, prefix: &str) -> BackupResult<Option<SyncState>> {
        read_sync_state(&self.conn, &scope_key(bucket, prefix))
    }

    /// freshness判定: sync_state行が存在しstatus=freshかつvalidator一致(設計4章:
    /// 別process起動時のHEAD 1回 validator確認と組合わせて使う)。自端末書込直後の
    /// 反映(Written由来)を含む=照会系cache hitの判定。
    pub fn is_fresh(
        &self,
        bucket: &str,
        prefix: &str,
        validator: &Validator,
    ) -> BackupResult<bool> {
        Ok(
            matches!(self.sync_state(bucket, prefix)?, Some(st) if st.status == STATUS_FRESH && st.validator_matches(validator)),
        )
    }

    /// replica取得の省略判定(manage catalog sync専用): 照会系のis_freshに加えて
    /// 「sync自身の取得実績(fetched_at)がある」ことを要求する。書込反映のみ
    /// (fetched_at未設定)の状態は初回syncとしてGETを行う(設計4章: syncはremote
    /// replicaの取得commandであるため、自ら取得したsnapshotのみを検証済みreplicaと
    /// みなす。変化(初回含む)あれば取得の「初回」=この状態)。
    pub fn replica_synced_fresh(
        &self,
        bucket: &str,
        prefix: &str,
        validator: &Validator,
    ) -> BackupResult<bool> {
        Ok(
            matches!(self.sync_state(bucket, prefix)?, Some(st) if st.status == STATUS_FRESH
            && !st.fetched_at.is_empty()
            && st.validator_matches(validator)),
        )
    }

    /// HEAD検証一致の記録(last_verified_at更新・statusはfreshのまま)。
    pub fn mark_verified_fresh(&self, bucket: &str, prefix: &str) -> BackupResult<()> {
        let scope = scope_key(bucket, prefix);
        self.conn
            .execute(
                "UPDATE sync_state SET last_verified_at = ?2 WHERE scope = ?1",
                [scope, now_jst()],
            )
            .map_err(|e| BackupError::Internal(format!("sync_state検証時刻の更新に失敗: {e}")))?;
        Ok(())
    }

    /// status失効(stale/error)。当該scopeの行が無い場合は何もしない
    /// (fresh状態が元々無いため「旧freshを残さない」は自明に成立)。
    pub fn mark_status(&self, bucket: &str, prefix: &str, status: &str) -> BackupResult<()> {
        let scope = scope_key(bucket, prefix);
        self.conn
            .execute(
                "UPDATE sync_state SET status = ?2 WHERE scope = ?1",
                [scope, status.to_string()],
            )
            .map_err(|e| BackupError::Internal(format!("sync_state status失効に失敗: {e}")))?;
        Ok(())
    }

    /// scope単位の原子的置換(設計2章: 単一transactionで当該scopeのcache行を
    /// delete+insert。混在状態を作らない)。対象はentities/versions/pins/locations/
    /// sync_state(cache class。pending_itemsはscan領域のため対象外)。
    /// 永続class(credentials/push_intents/api_keys)には触れない。
    ///
    /// entriesは当該scopeのprefix外のs3_keyを持つ版・pinを除外して格納する
    /// (指摘#3: 複数scope混在時の衝突・混入防止。索引はbucket単位で構築されるため
    /// scope分離はDB層の責務)。
    #[allow(clippy::too_many_arguments)]
    pub fn replace_scope(
        &mut self,
        bucket: &str,
        prefix: &str,
        header: &IndexHeader,
        entries: &[IndexFileEntry],
        mtimes: &SourceMtimes,
        validator: &Validator,
        status: &str,
        origin: ReflectOrigin,
    ) -> BackupResult<()> {
        let scope = scope_key(bucket, prefix);
        let now = now_jst();
        let scoped = scope_filter_entries(entries, prefix);
        let tx = self
            .conn
            .transaction()
            .map_err(|e| BackupError::Internal(format!("transaction開始に失敗: {e}")))?;
        // delete(entitiesを消すとFK CASCADEでversions/pins/locationsも消える)
        tx.execute("DELETE FROM entities WHERE scope = ?1", [&scope])
            .map_err(|e| BackupError::Internal(format!("cache置換のdeleteに失敗: {e}")))?;
        // insert
        for (ord, e) in scoped.iter().enumerate() {
            tx.execute(
                "INSERT INTO entities (scope, uuid, title, ordinal) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![scope, e.uuid, e.title, ord as i64],
            )
            .map_err(|err| BackupError::Internal(format!("entities挿入に失敗: {err}")))?;
            for (vord, v) in e.versions.iter().enumerate() {
                tx.execute(
                    "INSERT INTO versions (scope, s3_key, version_id, uuid, ordinal, date,\
                     version_ts, batch_id, source_sha256, age_sha256, source_size, age_size,\
                     source_mtime) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    rusqlite::params![
                        scope,
                        v.s3_key,
                        v.version_id,
                        e.uuid,
                        vord as i64,
                        v.date,
                        v.version_ts,
                        v.batch_id,
                        v.source_sha256,
                        v.age_sha256,
                        v.source_size as i64,
                        v.age_size as i64,
                        mtimes.of_version(&e.uuid, v),
                    ],
                )
                .map_err(|err| BackupError::Internal(format!("versions挿入に失敗: {err}")))?;
            }
            for (pord, p) in e.pins.iter().enumerate() {
                tx.execute(
                    "INSERT INTO pins (scope, pin, s3_key, uuid, created_at, date,\
                     source_sha256, age_sha256, source_size, ordinal)\
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        scope,
                        p.pin,
                        p.s3_key,
                        e.uuid,
                        p.created_at,
                        p.date,
                        p.source_sha256,
                        p.age_sha256,
                        p.source_size as i64,
                        pord as i64,
                    ],
                )
                .map_err(|err| BackupError::Internal(format!("pins挿入に失敗: {err}")))?;
            }
            for (lord, l) in e.locations.iter().enumerate() {
                tx.execute(
                    "INSERT INTO locations (scope, uuid, source_path, category, label,\
                     identity_hash, last_backup, source_mtime, ordinal)\
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    rusqlite::params![
                        scope,
                        e.uuid,
                        l.source_path,
                        l.category,
                        l.label,
                        l.identity_hash,
                        l.last_backup,
                        mtimes.of_location(l),
                        lord as i64,
                    ],
                )
                .map_err(|err| BackupError::Internal(format!("locations挿入に失敗: {err}")))?;
            }
        }
        // sync_stateの確定(設計4章: GETの応答validatorを取得内容と同一transactionで確定)
        let (fetched_at, last_write_at) = match origin {
            ReflectOrigin::Fetched => (now.clone(), String::new()),
            ReflectOrigin::Written => (String::new(), now.clone()),
        };
        tx.execute(
            "INSERT INTO sync_state (scope, index_etag, index_version_id, fetched_at,\
             last_write_at, last_verified_at, status, entry_count, index_updated_at, index_run_id)\
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)\
             ON CONFLICT(scope) DO UPDATE SET index_etag = ?2, index_version_id = ?3,\
             fetched_at = ?4, last_write_at = ?5, last_verified_at = ?6, status = ?7,\
             entry_count = ?8, index_updated_at = ?9, index_run_id = ?10",
            rusqlite::params![
                scope,
                validator.etag,
                validator.version_id,
                fetched_at,
                last_write_at,
                now,
                status,
                scoped.len() as i64,
                header.updated_at,
                header.run_id,
            ],
        )
        .map_err(|e| BackupError::Internal(format!("sync_state確定に失敗: {e}")))?;
        tx.commit()
            .map_err(|e| BackupError::Internal(format!("cache置換のcommitに失敗: {e}")))?;
        Ok(())
    }

    /// 既存scope行(versions/locations)から非空のsource_mtime基線を構築する。
    /// S3索引replica取得経路(sync fetch・cache miss再取得)はmtime供給源を持たない
    /// ため、replace_scopeの前に本methodで既存行の基線を引き継ぐ(空基線で置換する
    /// とlocal backupの記録基線がreplica取得で消え、classify_scan_targetが未変更
    /// fileをfalse changedに分類する。レビュー指摘#2=PR#9対応)。
    /// 同一keyの複数行はordinal順の後勝ち(最後に反映された基線値を採る)。
    pub fn existing_source_mtimes(&self, bucket: &str, prefix: &str) -> BackupResult<SourceMtimes> {
        let scope = scope_key(bucket, prefix);
        let mut mt = SourceMtimes::empty();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT identity_hash, source_mtime FROM locations \
                     WHERE scope = ?1 AND source_mtime != '' ORDER BY ordinal",
                )
                .map_err(|e| BackupError::Internal(format!("locations基線読取に失敗: {e}")))?;
            let rows = stmt
                .query_map([&scope], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| BackupError::Internal(format!("locations基線読取に失敗: {e}")))?;
            for row in rows {
                let (identity_hash, mtime) =
                    row.map_err(|e| BackupError::Internal(format!("locations基線読取に失敗: {e}")))?;
                mt.by_identity.insert(identity_hash, mtime);
            }
        }
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT uuid, source_sha256, source_mtime FROM versions \
                     WHERE scope = ?1 AND source_mtime != '' ORDER BY ordinal",
                )
                .map_err(|e| BackupError::Internal(format!("versions基線読取に失敗: {e}")))?;
            let rows = stmt
                .query_map([&scope], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| BackupError::Internal(format!("versions基線読取に失敗: {e}")))?;
            for row in rows {
                let (uuid, source_sha256, mtime) =
                    row.map_err(|e| BackupError::Internal(format!("versions基線読取に失敗: {e}")))?;
                mt.by_uuid_sha.insert((uuid, source_sha256), mtime);
            }
        }
        Ok(mt)
    }

    /// cacheから索引entriesを復元(ordinal順=反映時の索引並び。headerはsync_stateの
    /// 保存値から再構築)。照会系のcache hit経路が使い、entry構造はS3経路と同一。
    /// 一連の読取(sync_state・entities・versions・pins・locations)は単一の読取
    /// transactionで行う=同一snapshot(読取間にsyncがcommitしても旧entityと新版を
    /// 組み合わせた混在snapshotを返さない)。
    pub fn load_entries(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> BackupResult<(IndexHeader, Vec<IndexFileEntry>)> {
        let scope = scope_key(bucket, prefix);
        let tx = self.read_snapshot_tx()?;
        let st = read_sync_state(&tx, &scope)?;
        let Some(st) = st else {
            commit_read_tx(tx)?;
            return Ok((empty_header(), Vec::new()));
        };
        let entries = read_scoped_entries(&tx, &scope)?;
        let header = header_from_state(&st);
        commit_read_tx(tx)?;
        Ok((header, entries))
    }

    /// freshness判定とentries読込を同一の読取transaction(snapshot)で行う(cache hit
    /// 経路用)。検証済みfresh(HEAD応答のvalidator一致)ならSomeでentriesを返し、
    /// そうでなければNone(呼出側はS3取得経路へ)。判定と読込の間にsyncがcommit
    /// してもfresh判定と内容がずれない。
    pub fn load_entries_if_fresh(
        &self,
        bucket: &str,
        prefix: &str,
        validator: &Validator,
    ) -> BackupResult<Option<(IndexHeader, Vec<IndexFileEntry>)>> {
        let scope = scope_key(bucket, prefix);
        let tx = self.read_snapshot_tx()?;
        let st = read_sync_state(&tx, &scope)?;
        let fresh = matches!(
            &st,
            Some(st) if st.status == STATUS_FRESH && st.validator_matches(validator)
        );
        if !fresh {
            commit_read_tx(tx)?;
            return Ok(None);
        }
        let st = st.expect("fresh判定通過時はsync_state行が存在する");
        let entries = read_scoped_entries(&tx, &scope)?;
        let header = header_from_state(&st);
        commit_read_tx(tx)?;
        Ok(Some((header, entries)))
    }

    /// 読取snapshot用のtransaction開始(WAL modeのdeferred transactionは最初の
    /// SELECT時点でsnapshotを取る)。
    fn read_snapshot_tx(&self) -> BackupResult<rusqlite::Transaction<'_>> {
        self.conn
            .unchecked_transaction()
            .map_err(|e| BackupError::Internal(format!("読取transactionの開始に失敗: {e}")))
    }

    /// 当該scopeのpending_items読み出し(P3時点はscan未実装のため常に空。
    /// GET /api/pendingのschema枠がこの読み出し経路で接続される)。
    pub fn pending_items(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<PendingItem>> {
        let scope = scope_key(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source_root, path, size, mtime, status, last_scan_at \
                 FROM pending_items WHERE scope = ?1 ORDER BY source_root, path",
            )
            .map_err(|e| BackupError::Internal(format!("pending_items準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([&scope], |r| {
                Ok(PendingItem {
                    source_root: r.get(0)?,
                    path: r.get(1)?,
                    size: r.get(2)?,
                    mtime: r.get(3)?,
                    status: r.get(4)?,
                    last_scan_at: r.get(5)?,
                })
            })
            .map_err(|e| BackupError::Internal(format!("pending_items読込に失敗: {e}")))?;
        let mut items = Vec::new();
        for row in rows {
            items.push(
                row.map_err(|e| BackupError::Internal(format!("pending_items行読取に失敗: {e}")))?,
            );
        }
        Ok(items)
    }

    /// scan結果(pending_items)の当該scope全置換(TASK-9設計3章・12章P5)。
    /// 置換を行うのは再scanのみ(replace_scopeのcache置換とは独立=索引由来でない
    /// ため置換対象外)。単一transactionでdelete+insertし冪等(前回scan結果を残さない)。
    pub fn replace_pending_items(
        &mut self,
        bucket: &str,
        prefix: &str,
        items: &[PendingItem],
    ) -> BackupResult<()> {
        let scope = scope_key(bucket, prefix);
        let tx = self
            .conn
            .transaction()
            .map_err(|e| BackupError::Internal(format!("transaction開始に失敗: {e}")))?;
        tx.execute("DELETE FROM pending_items WHERE scope = ?1", [&scope])
            .map_err(|e| BackupError::Internal(format!("pending_items置換のdeleteに失敗: {e}")))?;
        for i in items {
            tx.execute(
                "INSERT INTO pending_items (scope, source_root, path, size, mtime, status,\
                 last_scan_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    scope,
                    i.source_root,
                    i.path,
                    i.size,
                    i.mtime,
                    i.status,
                    i.last_scan_at
                ],
            )
            .map_err(|e| BackupError::Internal(format!("pending_items挿入に失敗: {e}")))?;
        }
        tx.commit()
            .map_err(|e| BackupError::Internal(format!("pending_items置換のcommitに失敗: {e}")))?;
        Ok(())
    }

    /// push dry-run/from-list実行記録の追記(TASK-9設計3章(a)・2章push_intents=
    /// 永続class。rebuildの置換対象外)。通常push(positionalのみ)は呼ばない。
    pub fn record_push_intent(&self, kind: &str, payload_json: &str) -> BackupResult<()> {
        self.conn
            .execute(
                "INSERT INTO push_intents (recorded_at, kind, payload_json) VALUES (?1, ?2, ?3)",
                rusqlite::params![now_jst(), kind, payload_json],
            )
            .map_err(|e| BackupError::Internal(format!("push_intents記録に失敗: {e}")))?;
        Ok(())
    }

    /// scan分類の比較基準(TASK-9設計3章D6承認・12章P5): 当該scopeのlocationsと
    /// そのuuidの最新version(版source_size)を組で返す。mtimeはfile_write由来の
    /// source_mtime保存値(S3索引replica取得のみの環境では供給されない=空文字。
    /// その場合は一致判定不能としてchanged/unknownへ分類される=「確認不能」を
    /// 「済」と偽装しない)。
    pub fn scan_baseline(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<ScanBaseline>> {
        let scope = scope_key(bucket, prefix);
        // uuid毎の最新版(version_ts最大)のsource_size(一致判定のsize基準)
        let mut latest_size: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT uuid, source_size FROM versions WHERE scope = ?1\
                     ORDER BY uuid, version_ts",
                )
                .map_err(|e| BackupError::Internal(format!("versions準備に失敗: {e}")))?;
            let rows = stmt
                .query_map([&scope], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })
                .map_err(|e| BackupError::Internal(format!("versions読込に失敗: {e}")))?;
            for row in rows {
                let (uuid, size) =
                    row.map_err(|e| BackupError::Internal(format!("versions行読取に失敗: {e}")))?;
                latest_size.insert(uuid, size);
            }
        }
        let mut stmt = self
            .conn
            .prepare("SELECT uuid, source_path, source_mtime FROM locations WHERE scope = ?1")
            .map_err(|e| BackupError::Internal(format!("locations準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([&scope], |r| {
                Ok(ScanBaseline {
                    uuid: r.get(0)?,
                    source_path: r.get(1)?,
                    source_mtime: r.get(2)?,
                    latest_size: latest_size
                        .get(&r.get::<_, String>(0)?)
                        .copied()
                        .unwrap_or(-1),
                })
            })
            .map_err(|e| BackupError::Internal(format!("locations読込に失敗: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(
                row.map_err(|e| BackupError::Internal(format!("locations行読取に失敗: {e}")))?,
            );
        }
        Ok(out)
    }

    /// diff実行結果の保存(設計6章: POST /api/diffの結果をDB保存。scope単位に
    /// 最新1件のみ保持=再実行で置換)。
    pub fn save_diff_result(
        &self,
        bucket: &str,
        prefix: &str,
        performed_at: &str,
        result_json: &serde_json::Value,
    ) -> BackupResult<()> {
        let scope = scope_key(bucket, prefix);
        let text = serde_json::to_string(result_json)
            .map_err(|e| BackupError::Internal(format!("diff結果のJSON化に失敗: {e}")))?;
        self.conn
            .execute(
                "INSERT INTO diff_results (scope, performed_at, result_json) VALUES (?1, ?2, ?3)\
                 ON CONFLICT(scope) DO UPDATE SET performed_at = ?2, result_json = ?3",
                rusqlite::params![scope, performed_at, text],
            )
            .map_err(|e| BackupError::Internal(format!("diff結果の保存に失敗: {e}")))?;
        Ok(())
    }

    /// 保存済みの最終diff結果(未実行はNone。result_jsonをparseして返す)。
    pub fn load_diff_result(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> BackupResult<Option<serde_json::Value>> {
        let scope = scope_key(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare("SELECT result_json FROM diff_results WHERE scope = ?1")
            .map_err(|e| BackupError::Internal(format!("diff結果準備に失敗: {e}")))?;
        let row = stmt
            .query_row([&scope], |r| r.get::<_, String>(0))
            .optional()
            .map_err(|e| BackupError::Internal(format!("diff結果読込に失敗: {e}")))?;
        match row {
            None => Ok(None),
            Some(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| BackupError::Internal(format!("保存済みdiff結果のparseに失敗: {e}"))),
        }
    }

    // -----------------------------------------------------------------------
    // TASK-17 W2(TASK-19): tag/collection(local語彙)・audit_logのCRUD。
    // 4 tableとも永続class(replace_scopeの置換対象外)。呼出側はserveの書込API
    // (single-flight直列化の下でspawn_blocking経由)。

    /// 当該entityのtag一覧の置換(TASK-17設計3.2: 冪等・差し替え=削除+追加が
    /// 単一transactionの同一操作。空配列は全tag削除)。tag名の規則検証・body内重複の
    /// 正規化は呼出側。成功audit記録(action/detail_json)を置換と同一transactionで
    /// 確定する(コードレビュー中4: 記録失敗時は置換もrollback)。
    pub fn replace_entity_tags(
        &mut self,
        bucket: &str,
        prefix: &str,
        uuid: &str,
        tags: &[String],
        action: &str,
        detail_json: &str,
    ) -> Result<(), W2WriteError> {
        let scope = w2_scope(bucket, prefix);
        let now = now_jst();
        let tx = self
            .conn
            .transaction()
            .map_err(|e| BackupError::Internal(format!("transaction開始に失敗: {e}")))?;
        tx.execute(
            "DELETE FROM tags WHERE scope = ?1 AND uuid = ?2",
            rusqlite::params![scope, uuid],
        )
        .map_err(|e| BackupError::Internal(format!("tags置換のdeleteに失敗: {e}")))?;
        for tag in tags {
            tx.execute(
                "INSERT INTO tags (scope, uuid, tag, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![scope, uuid, tag, now],
            )
            .map_err(|e| BackupError::Internal(format!("tags挿入に失敗: {e}")))?;
        }
        Self::audit_ok_on_tx(&tx, &scope, action, detail_json)?;
        tx.commit()
            .map_err(|e| BackupError::Internal(format!("tags置換のcommitに失敗: {e}")))?;
        Ok(())
    }

    /// 当該entityのtag一覧(昇順)。
    pub fn entity_tags(
        &self,
        bucket: &str,
        prefix: &str,
        uuid: &str,
    ) -> BackupResult<Vec<String>> {
        let scope = w2_scope(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare("SELECT tag FROM tags WHERE scope = ?1 AND uuid = ?2 ORDER BY tag")
            .map_err(|e| BackupError::Internal(format!("tags準備に失敗: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![scope, uuid], |r| r.get::<_, String>(0))
            .map_err(|e| BackupError::Internal(format!("tags読込に失敗: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| BackupError::Internal(format!("tags行読取に失敗: {e}")))?);
        }
        Ok(out)
    }

    /// 当該scopeの(uuid, tag)全行(uuid・tag昇順)。一覧summary行のtags列組み立て用。
    pub fn scope_tag_rows(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<(String, String)>> {
        let scope = w2_scope(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare("SELECT uuid, tag FROM tags WHERE scope = ?1 ORDER BY uuid, tag")
            .map_err(|e| BackupError::Internal(format!("tags準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([&scope], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| BackupError::Internal(format!("tags読込に失敗: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(
                row.map_err(|e| BackupError::Internal(format!("tags行読取に失敗: {e}")))?,
            );
        }
        Ok(out)
    }

    /// tag一覧とentity数(タグ名昇順。entity_count=当該tagを持つ当該scope内の現存
    /// entity数=tags行を同一scopeのentities cacheとJOINして集計(コードレビュー中11:
    /// sync/rebuildでcacheから消えたentityのtag行は計上しない。tag行自体は永続class
    /// のため保持され・entity復帰時に再び計上される)。全tag保有entityが消えたtagは
    /// 一覧に出ない)。
    pub fn tag_counts(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<(String, i64)>> {
        let scope = w2_scope(bucket, prefix);
        let cache_scope = scope_key(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.tag, COUNT(DISTINCT t.uuid) FROM tags t \
                 JOIN entities e ON e.scope = ?2 AND e.uuid = t.uuid \
                 WHERE t.scope = ?1 GROUP BY t.tag ORDER BY t.tag",
            )
            .map_err(|e| BackupError::Internal(format!("tags集計準備に失敗: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![scope, cache_scope], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(|e| BackupError::Internal(format!("tags集計に失敗: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(
                row.map_err(|e| BackupError::Internal(format!("tags集計行読取に失敗: {e}")))?,
            );
        }
        Ok(out)
    }

    /// 成功audit_log記録(W2書込操作と同一transaction内で呼ぶ=コードレビュー中4:
    /// この挿入に失敗した場合は操作の書込もrollbackされ・成功応答した変更が履歴に
    /// 残らない状況を作らない。失敗audit(422/409/404時)は操作が行われていないため
    /// record_auditの別経路でよい)。
    fn audit_ok_on_tx(
        conn: &Connection,
        scope: &str,
        action: &str,
        detail_json: &str,
    ) -> BackupResult<()> {
        conn.execute(
            "INSERT INTO audit_log (scope, recorded_at, action, detail_json, outcome,\
             error_code) VALUES (?1, ?2, ?3, ?4, 'ok', NULL)",
            rusqlite::params![scope, now_jst(), action, detail_json],
        )
        .map_err(|e| BackupError::Internal(format!("audit_log記録に失敗: {e}")))?;
        Ok(())
    }

    /// 当該tagを持つ当該scope内のuuid一覧(?tag=絞り込み用)。
    pub fn uuids_with_tag(
        &self,
        bucket: &str,
        prefix: &str,
        tag: &str,
    ) -> BackupResult<Vec<String>> {
        let scope = w2_scope(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare("SELECT uuid FROM tags WHERE scope = ?1 AND tag = ?2")
            .map_err(|e| BackupError::Internal(format!("tags準備に失敗: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![scope, tag], |r| r.get::<_, String>(0))
            .map_err(|e| BackupError::Internal(format!("tags読込に失敗: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| BackupError::Internal(format!("tags行読取に失敗: {e}")))?);
        }
        Ok(out)
    }

    /// 当該uuidが所属するcollection名一覧(名前昇順=詳細の所属collection表示用)。
    pub fn collections_of_uuid(
        &self,
        bucket: &str,
        prefix: &str,
        uuid: &str,
    ) -> BackupResult<Vec<String>> {
        let scope = w2_scope(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.name FROM collections c JOIN collection_items i \
                 ON i.collection_id = c.id AND i.scope = c.scope \
                 WHERE c.scope = ?1 AND i.uuid = ?2 ORDER BY c.name",
            )
            .map_err(|e| BackupError::Internal(format!("所属collection準備に失敗: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![scope, uuid], |r| r.get::<_, String>(0))
            .map_err(|e| BackupError::Internal(format!("所属collection読込に失敗: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(
                row.map_err(|e| BackupError::Internal(format!("所属collection行読取に失敗: {e}")))?,
            );
        }
        Ok(out)
    }

    /// collection一覧(名前昇順。entity_count=構成entity数=構成行を同一scopeのentities
    /// cacheとJOINして現存entityのみ計上(コードレビュー中11: sync/rebuildでcacheから
    /// 消えたentityの構成行は計上しない。構成行自体は保持)。
    pub fn collection_rows(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<CollectionRow>> {
        let scope = w2_scope(bucket, prefix);
        let cache_scope = scope_key(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT c.id, c.name, COUNT(e.uuid) FROM collections c \
                 LEFT JOIN collection_items i ON i.collection_id = c.id AND i.scope = c.scope \
                 LEFT JOIN entities e ON e.scope = ?2 AND e.uuid = i.uuid \
                 WHERE c.scope = ?1 GROUP BY c.id ORDER BY c.name",
            )
            .map_err(|e| BackupError::Internal(format!("collections準備に失敗: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![scope, cache_scope], |r| {
                Ok(CollectionRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    entity_count: r.get(2)?,
                })
            })
            .map_err(|e| BackupError::Internal(format!("collections読込に失敗: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(
                row.map_err(|e| BackupError::Internal(format!("collections行読取に失敗: {e}")))?,
            );
        }
        Ok(out)
    }

    /// collection詳細(不在idはNone。uuidsは追加順=rowid順=挿入順。schema上に
    /// ordinal列が無いため挿入順の保存はrowidが担る)。
    pub fn collection_detail(
        &self,
        bucket: &str,
        prefix: &str,
        collection_id: i64,
    ) -> BackupResult<Option<CollectionDetail>> {
        let scope = w2_scope(bucket, prefix);
        let name = self
            .conn
            .query_row(
                "SELECT name FROM collections WHERE scope = ?1 AND id = ?2",
                rusqlite::params![scope, collection_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| BackupError::Internal(format!("collections読込に失敗: {e}")))?;
        let Some(name) = name else {
            return Ok(None);
        };
        let mut stmt = self
            .conn
            .prepare(
                "SELECT uuid FROM collection_items WHERE scope = ?1 AND collection_id = ?2 \
                 ORDER BY rowid",
            )
            .map_err(|e| BackupError::Internal(format!("collection_items準備に失敗: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![scope, collection_id], |r| r.get::<_, String>(0))
            .map_err(|e| BackupError::Internal(format!("collection_items読込に失敗: {e}")))?;
        let mut uuids = Vec::new();
        for row in rows {
            uuids.push(
                row.map_err(|e| BackupError::Internal(format!("collection_items行読取に失敗: {e}")))?,
            );
        }
        Ok(Some(CollectionDetail {
            id: collection_id,
            name,
            uuids,
        }))
    }

    /// collectionの作成(同名はDuplicate=UNIQUE(scope,name)由来。成功audit記録を
    /// 作成と同一transactionで確定する=コードレビュー中4)。
    pub fn create_collection(
        &mut self,
        bucket: &str,
        prefix: &str,
        name: &str,
        action: &str,
        detail_json: &str,
    ) -> Result<CollectionDetail, W2WriteError> {
        let scope = w2_scope(bucket, prefix);
        let tx = self
            .conn
            .transaction()
            .map_err(|e| BackupError::Internal(format!("transaction開始に失敗: {e}")))?;
        let exists = tx
            .query_row(
                "SELECT 1 FROM collections WHERE scope = ?1 AND name = ?2",
                rusqlite::params![scope, name],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| BackupError::Internal(format!("collections読込に失敗: {e}")))?;
        if exists.is_some() {
            return Err(W2WriteError::Duplicate);
        }
        tx.execute(
            "INSERT INTO collections (scope, name, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![scope, name, now_jst()],
        )
        .map_err(|e| BackupError::Internal(format!("collections挿入に失敗: {e}")))?;
        let id = tx.last_insert_rowid();
        Self::audit_ok_on_tx(&tx, &scope, action, detail_json)?;
        tx.commit()
            .map_err(|e| BackupError::Internal(format!("collections挿入のcommitに失敗: {e}")))?;
        Ok(CollectionDetail {
            id,
            name: name.to_string(),
            uuids: Vec::new(),
        })
    }

    /// collectionの改名(id不変。不在idはNotFound・改名先の同名はDuplicate。応答は
    /// 操作後の対象状態として実際の構成uuid一覧を読み戻す=コードレビュー高2。
    /// 成功audit記録を改名と同一transactionで確定する=中4)。
    pub fn rename_collection(
        &mut self,
        bucket: &str,
        prefix: &str,
        collection_id: i64,
        new_name: &str,
        action: &str,
        detail_json: &str,
    ) -> Result<CollectionDetail, W2WriteError> {
        let scope = w2_scope(bucket, prefix);
        let tx = self
            .conn
            .transaction()
            .map_err(|e| BackupError::Internal(format!("transaction開始に失敗: {e}")))?;
        let current = tx
            .query_row(
                "SELECT name FROM collections WHERE scope = ?1 AND id = ?2",
                rusqlite::params![scope, collection_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| BackupError::Internal(format!("collections読込に失敗: {e}")))?;
        if current.is_none() {
            return Err(W2WriteError::NotFound);
        }
        let clash = tx
            .query_row(
                "SELECT 1 FROM collections WHERE scope = ?1 AND name = ?2 AND id != ?3",
                rusqlite::params![scope, new_name, collection_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| BackupError::Internal(format!("collections読込に失敗: {e}")))?;
        if clash.is_some() {
            return Err(W2WriteError::Duplicate);
        }
        tx.execute(
            "UPDATE collections SET name = ?3 WHERE scope = ?1 AND id = ?2",
            rusqlite::params![scope, collection_id, new_name],
        )
        .map_err(|e| BackupError::Internal(format!("collections更新に失敗: {e}")))?;
        Self::audit_ok_on_tx(&tx, &scope, action, detail_json)?;
        tx.commit()
            .map_err(|e| BackupError::Internal(format!("collections更新のcommitに失敗: {e}")))?;
        // 改名後の実際の構成uuid一覧(GET /api/collections/:idと同一内容=高2)
        let detail = self.collection_detail(bucket, prefix, collection_id)?;
        Ok(detail.unwrap_or(CollectionDetail {
            id: collection_id,
            name: new_name.to_string(),
            uuids: Vec::new(),
        }))
    }

    /// collectionの削除(構成参照の解除のみ=collection_itemsはFK CASCADEで消え・
    /// entity側には影響しない)。不在idはNotFound。成功audit記録を削除と同一
    /// transactionで確定する=コードレビュー中4。
    pub fn delete_collection(
        &mut self,
        bucket: &str,
        prefix: &str,
        collection_id: i64,
        action: &str,
        detail_json: &str,
    ) -> Result<(), W2WriteError> {
        let scope = w2_scope(bucket, prefix);
        let tx = self
            .conn
            .transaction()
            .map_err(|e| BackupError::Internal(format!("transaction開始に失敗: {e}")))?;
        let deleted = tx
            .execute(
                "DELETE FROM collections WHERE scope = ?1 AND id = ?2",
                rusqlite::params![scope, collection_id],
            )
            .map_err(|e| BackupError::Internal(format!("collections削除に失敗: {e}")))?;
        if deleted == 0 {
            return Err(W2WriteError::NotFound);
        }
        Self::audit_ok_on_tx(&tx, &scope, action, detail_json)?;
        tx.commit()
            .map_err(|e| BackupError::Internal(format!("collections削除のcommitに失敗: {e}")))?;
        Ok(())
    }

    /// 構成entityの一括追加(不在collection・catalogに無いuuidはNotFound。追加済み
    /// uuidは正規のskip=冪等。検証・挿入・成功audit記録を単一transactionで行い
    /// 部分反映を作らない=コードレビュー中4)。
    pub fn add_collection_items(
        &mut self,
        bucket: &str,
        prefix: &str,
        collection_id: i64,
        uuids: &[String],
        action: &str,
        detail_json: &str,
    ) -> Result<CollectionDetail, W2WriteError> {
        let scope = w2_scope(bucket, prefix);
        let cache_scope = scope_key(bucket, prefix);
        let now = now_jst();
        let tx = self
            .conn
            .transaction()
            .map_err(|e| BackupError::Internal(format!("transaction開始に失敗: {e}")))?;
        let name = tx
            .query_row(
                "SELECT name FROM collections WHERE scope = ?1 AND id = ?2",
                rusqlite::params![scope, collection_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| BackupError::Internal(format!("collections読込に失敗: {e}")))?;
        let Some(name) = name else {
            return Err(W2WriteError::NotFound);
        };
        // 追加uuidは当該scopeのcache(entities)に実在すること(不在はNotFound)
        for uuid in uuids {
            let found = tx
                .query_row(
                    "SELECT 1 FROM entities WHERE scope = ?1 AND uuid = ?2",
                    rusqlite::params![cache_scope, uuid],
                    |_| Ok(()),
                )
                .optional()
                .map_err(|e| BackupError::Internal(format!("entities読込に失敗: {e}")))?;
            if found.is_none() {
                return Err(W2WriteError::NotFound);
            }
        }
        // 追加済みuuidの再追加は冪等に成功(UNIQUE PKとの衝突をerrorにしない)
        for uuid in uuids {
            tx.execute(
                "INSERT INTO collection_items (collection_id, scope, uuid, added_at) \
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
                rusqlite::params![collection_id, scope, uuid, now],
            )
            .map_err(|e| BackupError::Internal(format!("collection_items挿入に失敗: {e}")))?;
        }
        Self::audit_ok_on_tx(&tx, &scope, action, detail_json)?;
        tx.commit().map_err(|e| {
            BackupError::Internal(format!("collection_items追加のcommitに失敗: {e}"))
        })?;
        let detail = self.collection_detail(bucket, prefix, collection_id)?;
        Ok(detail.unwrap_or(CollectionDetail {
            id: collection_id,
            name,
            uuids: Vec::new(),
        }))
    }

    /// 構成からの除去(不在collection・非構成uuidはNotFound。成功audit記録を除去と
    /// 同一transactionで確定する=コードレビュー中4)。
    pub fn remove_collection_item(
        &mut self,
        bucket: &str,
        prefix: &str,
        collection_id: i64,
        uuid: &str,
        action: &str,
        detail_json: &str,
    ) -> Result<CollectionDetail, W2WriteError> {
        let scope = w2_scope(bucket, prefix);
        let tx = self
            .conn
            .transaction()
            .map_err(|e| BackupError::Internal(format!("transaction開始に失敗: {e}")))?;
        let name = tx
            .query_row(
                "SELECT name FROM collections WHERE scope = ?1 AND id = ?2",
                rusqlite::params![scope, collection_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| BackupError::Internal(format!("collections読込に失敗: {e}")))?;
        let Some(name) = name else {
            return Err(W2WriteError::NotFound);
        };
        let deleted = tx
            .execute(
                "DELETE FROM collection_items WHERE scope = ?1 AND collection_id = ?2 AND uuid = ?3",
                rusqlite::params![scope, collection_id, uuid],
            )
            .map_err(|e| BackupError::Internal(format!("collection_items削除に失敗: {e}")))?;
        if deleted == 0 {
            return Err(W2WriteError::NotFound);
        }
        Self::audit_ok_on_tx(&tx, &scope, action, detail_json)?;
        tx.commit().map_err(|e| {
            BackupError::Internal(format!("collection_items削除のcommitに失敗: {e}"))
        })?;
        let detail = self.collection_detail(bucket, prefix, collection_id)?;
        Ok(detail.unwrap_or(CollectionDetail {
            id: collection_id,
            name,
            uuids: Vec::new(),
        }))
    }

    /// UI書込操作のaudit_log記録(TASK-17設計4.1・6.2・D-W10: UI書込操作のみ。
    /// 成功・失敗両方を記録し、失敗時はerror_codeを入れる)。
    pub fn record_audit(
        &self,
        bucket: &str,
        prefix: &str,
        action: &str,
        detail_json: &str,
        outcome: &str,
        error_code: Option<&str>,
    ) -> BackupResult<()> {
        self.conn
            .execute(
                "INSERT INTO audit_log (scope, recorded_at, action, detail_json, outcome,\
                 error_code) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    w2_scope(bucket, prefix),
                    now_jst(),
                    action,
                    detail_json,
                    outcome,
                    error_code
                ],
            )
            .map_err(|e| BackupError::Internal(format!("audit_log記録に失敗: {e}")))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // jobs table(TASK-17 W4・設計4.1/5章)。永続class(replace_scopeの置換対象外)。
    // 呼出側はserve(JobManager・job系API。single-flight直列化の下でspawn_blocking
    // 経由で読み書きする。timerによるwaiting_restore確認のhead-objectは読取のため
    // semaphore外で実行される=本層の呼出も読取で完結する)。

    /// job行の挿入(設計5.1: POST /api/jobs/*はjob行をqueuedでinsertして即時応答)。
    /// progress_jsonは初期値(呼出側がphase=queuedの初期progressを組む)。
    pub fn insert_job(
        &self,
        bucket: &str,
        prefix: &str,
        kind: &str,
        params_json: &str,
        progress_json: &str,
    ) -> BackupResult<JobRow> {
        let now = now_jst();
        self.conn
            .execute(
                "INSERT INTO jobs (scope, kind, status, params_json, progress_json,\
                 result_json, error_code, error_message, created_at, updated_at) \
                 VALUES (?1, ?2, 'queued', ?3, ?4, NULL, NULL, NULL, ?5, ?5)",
                rusqlite::params![
                    w2_scope(bucket, prefix),
                    kind,
                    params_json,
                    progress_json,
                    now
                ],
            )
            .map_err(|e| BackupError::Internal(format!("jobs挿入に失敗: {e}")))?;
        let id = self.conn.last_insert_rowid();
        self.job_by_id(bucket, prefix, id)?
            .ok_or_else(|| BackupError::Internal("挿入直後のjob行が読めない".to_string()))
    }

    /// job状態の更新(status・progressは常に差し替え。result/errorはSome時のみ設定
    /// =Noneは変更しない。遷移で設定済みfieldをクリアする経路は無い)。updated_at刻み。
    pub fn update_job(
        &self,
        id: i64,
        status: &str,
        progress_json: &str,
        result_json: Option<&str>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> BackupResult<()> {
        self.conn
            .execute(
                "UPDATE jobs SET status = ?2, progress_json = ?3, \
                 result_json = COALESCE(?4, result_json), \
                 error_code = COALESCE(?5, error_code), \
                 error_message = COALESCE(?6, error_message), \
                 updated_at = ?7 WHERE id = ?1",
                rusqlite::params![
                    id,
                    status,
                    progress_json,
                    result_json,
                    error_code,
                    error_message,
                    now_jst()
                ],
            )
            .map_err(|e| BackupError::Internal(format!("jobs更新に失敗: {e}")))?;
        Ok(())
    }

    /// job詳細(当該scopeの行のみ。不在・他scopeのidはNone)。
    pub fn job_by_id(
        &self,
        bucket: &str,
        prefix: &str,
        id: i64,
    ) -> BackupResult<Option<JobRow>> {
        let scope = w2_scope(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, scope, kind, status, params_json, progress_json, result_json,\
                 error_code, error_message, created_at, updated_at FROM jobs \
                 WHERE id = ?1 AND scope = ?2",
            )
            .map_err(|e| BackupError::Internal(format!("jobs準備に失敗: {e}")))?;
        stmt.query_row(rusqlite::params![id, scope], job_row_of)
            .optional()
            .map_err(|e| BackupError::Internal(format!("jobs読込に失敗: {e}")))
    }

    /// job一覧(設計3.2: 進行中優先・降順=queued/running/waiting_restoreを先頭group
    /// へ・各group内はid降順=新しい順。status/kind絞り込みは呼出側検証済み値)。
    pub fn list_jobs(
        &self,
        bucket: &str,
        prefix: &str,
        status: Option<&str>,
        kind: Option<&str>,
    ) -> BackupResult<Vec<JobRow>> {
        let scope = w2_scope(bucket, prefix);
        // 絞り込み条件は位置placeholder「?」+動的param数で組む(条件の有無で
        // param過不足エラーにならない構成)
        let mut sql = "SELECT id, scope, kind, status, params_json, progress_json, \
             result_json, error_code, error_message, created_at, updated_at FROM jobs \
             WHERE scope = ?"
            .to_string();
        let mut params: Vec<String> = vec![scope];
        if let Some(s) = status {
            sql.push_str(" AND status = ?");
            params.push(s.to_string());
        }
        if let Some(k) = kind {
            sql.push_str(" AND kind = ?");
            params.push(k.to_string());
        }
        sql.push_str(
            " ORDER BY CASE WHEN status IN ('queued','running','waiting_restore') THEN 0 \
             ELSE 1 END, id DESC",
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| BackupError::Internal(format!("jobs準備に失敗: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), job_row_of)
            .map_err(job_read_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(job_read_err)?);
        }
        Ok(out)
    }

    /// 実行待ちの最古job(queued・id昇順=発行順。JobManager workerの取り出し順)。
    pub fn next_queued_job(&self, bucket: &str, prefix: &str) -> BackupResult<Option<JobRow>> {
        let scope = w2_scope(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, scope, kind, status, params_json, progress_json, result_json,\
                 error_code, error_message, created_at, updated_at FROM jobs \
                 WHERE scope = ?1 AND status = 'queued' ORDER BY id LIMIT 1",
            )
            .map_err(|e| BackupError::Internal(format!("jobs準備に失敗: {e}")))?;
        stmt.query_row([&scope], job_row_of)
            .optional()
            .map_err(|e| BackupError::Internal(format!("jobs読込に失敗: {e}")))
    }

    /// waiting_restore中のjob一覧(id昇順。timerの確認対象)。
    pub fn waiting_restore_jobs(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<JobRow>> {
        let scope = w2_scope(bucket, prefix);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, scope, kind, status, params_json, progress_json, result_json,\
                 error_code, error_message, created_at, updated_at FROM jobs \
                 WHERE scope = ?1 AND status = 'waiting_restore' ORDER BY id",
            )
            .map_err(|e| BackupError::Internal(format!("jobs準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([&scope], job_row_of)
            .map_err(job_read_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(job_read_err)?);
        }
        Ok(out)
    }

    /// queued jobの取消(status=cancelledへ。queuedでない場合は更新0行。呼出側は
    /// 先にjob_by_idで存在とstatusを確認し404/409を区別する)。
    pub fn cancel_queued_job(&self, id: i64) -> BackupResult<usize> {
        let updated = self
            .conn
            .execute(
                "UPDATE jobs SET status = 'cancelled', updated_at = ?2 \
                 WHERE id = ?1 AND status = 'queued'",
                rusqlite::params![id, now_jst()],
            )
            .map_err(|e| BackupError::Internal(format!("jobs取消に失敗: {e}")))?;
        Ok(updated)
    }

    /// queued jobの実行開始claim(status='queued'の条件付きでrunningへ遷移し
    /// progressを差し替える)。workerがsemaphore待ちの間に取消されたjobは
    /// 更新0行となり、呼出側は実行しない(取消の確定を上書きしない)。
    pub fn claim_queued_job(&self, id: i64, progress_json: &str) -> BackupResult<usize> {
        let updated = self
            .conn
            .execute(
                "UPDATE jobs SET status = 'running', progress_json = ?3, \
                 updated_at = ?2 WHERE id = ?1 AND status = 'queued'",
                rusqlite::params![id, now_jst(), progress_json],
            )
            .map_err(|e| BackupError::Internal(format!("jobsのclaimに失敗: {e}")))?;
        Ok(updated)
    }

    /// 起動時の一括interrupted化(設計5.4: queued/running/waiting_restoreを
    /// interruptedへ更新=自動再開しない。server再起動の起動処理から呼ぶ)。
    /// 完了系(succeeded/failed/interrupted/cancelled)の行は変更しない。
    pub fn interrupt_active_jobs(&self, bucket: &str, prefix: &str) -> BackupResult<usize> {
        let updated = self
            .conn
            .execute(
                "UPDATE jobs SET status = 'interrupted', updated_at = ?2 \
                 WHERE scope = ?1 AND status IN ('queued','running','waiting_restore')",
                rusqlite::params![w2_scope(bucket, prefix), now_jst()],
            )
            .map_err(|e| BackupError::Internal(format!("jobsのinterrupted化に失敗: {e}")))?;
        Ok(updated)
    }
}

/// jobs行の読取(rusqlite row → JobRow)。
fn job_row_of(r: &rusqlite::Row) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: r.get(0)?,
        scope: r.get(1)?,
        kind: r.get(2)?,
        status: r.get(3)?,
        params_json: r.get(4)?,
        progress_json: r.get(5)?,
        result_json: r.get(6)?,
        error_code: r.get(7)?,
        error_message: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

/// jobs行読取errorの変換(共通文言)。
fn job_read_err(e: rusqlite::Error) -> BackupError {
    BackupError::Internal(format!("jobs行読取に失敗: {e}"))
}

/// pending_items 1行分(設計2章schema・§3のscan結果)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingItem {
    pub source_root: String,
    pub path: String,
    pub size: i64,
    pub mtime: String,
    pub status: String,
    pub last_scan_at: String,
}

/// scan分類の比較基準1行分(locations + 当該uuidの最新version source_size)。
#[derive(Debug, Clone)]
pub struct ScanBaseline {
    pub uuid: String,
    pub source_path: String,
    /// file_write由来のsource_mtime保存値(replica取得のみの環境では空=判定不能)
    pub source_mtime: String,
    /// 最新versionのsource_size(versionsが無い場合は-1=非一致扱い)
    pub latest_size: i64,
}

/// 空の索引header(sync_state不在時のload_entries用)。
fn empty_header() -> IndexHeader {
    IndexHeader {
        schema: INDEX_SCHEMA,
        record_type: "index_header".into(),
        updated_at: String::new(),
        run_id: String::new(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        entries: 0,
    }
}

/// sync_state読み出し(行が無いNone)。connectionはTransactionも渡せる
/// (DerefでConnectionへ透過するため同一snapshot内の読取に使う)。
fn read_sync_state(conn: &Connection, scope: &str) -> BackupResult<Option<SyncState>> {
    let mut stmt = conn
        .prepare(
            "SELECT index_etag, index_version_id, fetched_at, last_write_at, \
             last_verified_at, status, entry_count, index_updated_at, index_run_id \
             FROM sync_state WHERE scope = ?1",
        )
        .map_err(|e| BackupError::Internal(format!("sync_state準備に失敗: {e}")))?;
    let row = stmt
        .query_row([scope], |r| {
            Ok(SyncState {
                index_etag: r.get(0)?,
                index_version_id: r.get(1)?,
                fetched_at: r.get(2)?,
                last_write_at: r.get(3)?,
                last_verified_at: r.get(4)?,
                status: r.get(5)?,
                entry_count: r.get(6)?,
                index_updated_at: r.get(7)?,
                index_run_id: r.get(8)?,
            })
        })
        .optional()
        .map_err(|e| BackupError::Internal(format!("sync_state読込に失敗: {e}")))?;
    Ok(row)
}

/// 当該scopeのcache行からentriesを復元(entities・versions・pins・locationsを
/// ordinal順=反映時の索引並びで読む。読取transaction内で使う)。
fn read_scoped_entries(conn: &Connection, scope: &str) -> BackupResult<Vec<IndexFileEntry>> {
    // entities(ordinal順)
    let mut entities: Vec<(String, String)> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT uuid, title FROM entities WHERE scope = ?1 ORDER BY ordinal")
            .map_err(|e| BackupError::Internal(format!("entities準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([scope], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(|e| BackupError::Internal(format!("entities読込に失敗: {e}")))?;
        for row in rows {
            entities.push(
                row.map_err(|e| BackupError::Internal(format!("entities行読取に失敗: {e}")))?,
            );
        }
    }
    // versions(uuid・ordinal順)
    let mut versions: std::collections::HashMap<String, Vec<IndexVersion>> =
        std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT uuid, date, version_ts, batch_id, s3_key, version_id, \
                 source_sha256, age_sha256, source_size, age_size FROM versions \
                 WHERE scope = ?1 ORDER BY uuid, ordinal",
            )
            .map_err(|e| BackupError::Internal(format!("versions準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([scope], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    IndexVersion {
                        date: r.get(1)?,
                        version_ts: r.get(2)?,
                        batch_id: r.get(3)?,
                        s3_key: r.get(4)?,
                        version_id: r.get(5)?,
                        source_sha256: r.get(6)?,
                        age_sha256: r.get(7)?,
                        source_size: r.get::<_, i64>(8)?.max(0) as u64,
                        age_size: r.get::<_, i64>(9)?.max(0) as u64,
                    },
                ))
            })
            .map_err(|e| BackupError::Internal(format!("versions読込に失敗: {e}")))?;
        for row in rows {
            let (uuid, v) =
                row.map_err(|e| BackupError::Internal(format!("versions行読取に失敗: {e}")))?;
            versions.entry(uuid).or_default().push(v);
        }
    }
    // pins(uuid・ordinal順)
    let mut pins: std::collections::HashMap<String, Vec<IndexPin>> =
        std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT uuid, pin, s3_key, created_at, date, source_sha256, age_sha256,\
                 source_size FROM pins WHERE scope = ?1 ORDER BY uuid, ordinal",
            )
            .map_err(|e| BackupError::Internal(format!("pins準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([scope], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    IndexPin {
                        pin: r.get(1)?,
                        s3_key: r.get(2)?,
                        created_at: r.get(3)?,
                        date: r.get(4)?,
                        source_sha256: r.get(5)?,
                        age_sha256: r.get(6)?,
                        source_size: r.get::<_, i64>(7)?.max(0) as u64,
                    },
                ))
            })
            .map_err(|e| BackupError::Internal(format!("pins読込に失敗: {e}")))?;
        for row in rows {
            let (uuid, p) =
                row.map_err(|e| BackupError::Internal(format!("pins行読取に失敗: {e}")))?;
            pins.entry(uuid).or_default().push(p);
        }
    }
    // locations(uuid・ordinal順)
    let mut locations: std::collections::HashMap<String, Vec<IndexLocation>> =
        std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT uuid, source_path, category, label, identity_hash, last_backup \
                 FROM locations WHERE scope = ?1 ORDER BY uuid, ordinal",
            )
            .map_err(|e| BackupError::Internal(format!("locations準備に失敗: {e}")))?;
        let rows = stmt
            .query_map([scope], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    IndexLocation {
                        source_path: r.get(1)?,
                        category: r.get(2)?,
                        label: r.get(3)?,
                        identity_hash: r.get(4)?,
                        last_backup: r.get(5)?,
                    },
                ))
            })
            .map_err(|e| BackupError::Internal(format!("locations読込に失敗: {e}")))?;
        for row in rows {
            let (uuid, l) =
                row.map_err(|e| BackupError::Internal(format!("locations行読取に失敗: {e}")))?;
            locations.entry(uuid).or_default().push(l);
        }
    }
    Ok(entities
        .into_iter()
        .map(|(uuid, title)| {
            let versions = versions.remove(&uuid).unwrap_or_default();
            let pins = pins.remove(&uuid).unwrap_or_default();
            let locations = locations.remove(&uuid).unwrap_or_default();
            IndexFileEntry {
                schema: INDEX_SCHEMA,
                record_type: "file".into(),
                uuid,
                title,
                versions,
                pins,
                locations,
            }
        })
        .collect())
}

/// sync_stateの保存値から索引headerを再構築(cache hit時にheaderを忠実に復元)。
fn header_from_state(st: &SyncState) -> IndexHeader {
    IndexHeader {
        schema: INDEX_SCHEMA,
        record_type: "index_header".into(),
        updated_at: st.index_updated_at.clone(),
        run_id: st.index_run_id.clone(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        entries: st.entry_count,
    }
}

/// 読取transactionの終了(読取のみのためrollbackでも等価。commit失敗は読取自体の
/// 異常として伝播させる)。
fn commit_read_tx(tx: rusqlite::Transaction<'_>) -> BackupResult<()> {
    tx.commit()
        .map_err(|e| BackupError::Internal(format!("読取transactionの終了に失敗: {e}")))
}

/// scope分離filter: prefix外のs3_keyを持つ版・pinを除外し、版もpinも無くなった
/// entityは落とす(指摘#3。L-R1と同一の意味論をscope単位へ適用)。
/// DB保存側(replace_scope)とS3取得経路(storage::read_index_cached)の両方が適用する
/// =cache hitとS3直接で出力のscope適用を同一規則に保つ。
pub(crate) fn scope_filter_entries(
    entries: &[IndexFileEntry],
    prefix: &str,
) -> Vec<IndexFileEntry> {
    entries
        .iter()
        .filter_map(|e| {
            let mut scoped = e.clone();
            scoped.versions.retain(|v| v.s3_key.starts_with(prefix));
            scoped.pins.retain(|p| p.s3_key.starts_with(prefix));
            if scoped.versions.is_empty() && scoped.pins.is_empty() {
                None
            } else {
                Some(scoped)
            }
        })
        .collect()
}

/// 書込系(push/pin/prune等)の索引upload成功後の同一process内DB反映(設計2章の書込時序
/// ・4章の自端末書込直後fresh)。反映の成否のみをResultで返し、呼び出し側は失敗時に
/// 警告表示して処理継続する(JSONLが正本のため)。反映失敗(HEAD失敗・置換失敗)時は
/// 当該scopeをstale化して旧freshを残さない(設計2章「反映失敗時は当該scopeをstale化
/// して警告」。mark_status自体の失敗は元のerrorに優先させない=警告logのみ)。
/// uploaded_version_idはupload_indexの応答(putのVersionId)。upload→HEADの間に
/// 他端末が索引objectを置換していた場合、HEADのvalidatorは他端末の版由来となり
/// 自processの旧entriesとの組み合わせでfresh化すると以降の照会がcache hitとして
/// 旧内容を受け入れるため、HEADの現行version_idと一致した場合のみfresh化する
/// (不一致は当該scopeのstale化+警告。versioning無効bucketでput応答が空の場合は
/// version_idでの束縛が不能=従来どおりHEAD応答のvalidatorのみで判断する)。
pub fn reflect_written_index(
    backend: &dyn S3Backend,
    bucket: &str,
    prefix: &str,
    header: &IndexHeader,
    entries: &[IndexFileEntry],
    mtimes: &SourceMtimes,
    uploaded_version_id: &str,
) -> Result<(), String> {
    let reflect = || -> BackupResult<()> {
        let mut db = CatalogDb::open()?;
        reflect_written_index_with_db(
            &mut db,
            backend,
            bucket,
            prefix,
            header,
            entries,
            mtimes,
            uploaded_version_id,
        )
    };
    reflect().map_err(|e| format!("{e}"))
}

/// 観測用のDB引数版反映本体(単体試験がCatalogDbを注入できる。振る舞いは
/// reflect_written_indexと同一)。
#[allow(clippy::too_many_arguments)]
fn reflect_written_index_with_db(
    db: &mut CatalogDb,
    backend: &dyn S3Backend,
    bucket: &str,
    prefix: &str,
    header: &IndexHeader,
    entries: &[IndexFileEntry],
    mtimes: &SourceMtimes,
    uploaded_version_id: &str,
) -> BackupResult<()> {
    let index_key = crate::layout::index_object_key(prefix);
    // 直前にuploadした索引objectのvalidatorをHEAD 1回で確定する(この時点のsnapshot
    // 鮮度=自端末書込直後の無条件fresh)
    let head = match backend.head(bucket, &index_key) {
        Ok(h) => h,
        Err(e) => {
            // HEAD失敗=validator確定不能: 当該scopeをstale化して旧freshを残さない
            if let Err(mark_e) = db.mark_status(bucket, prefix, STATUS_STALE) {
                eprintln!("WARNING: sync_stateのstale化に失敗(logのみ): {mark_e}");
            }
            return Err(e);
        }
    };
    let validator = match head {
        Some(h) => Validator::from_head(&h),
        None => Validator::default(),
    };
    // upload応答のVersionIdでHEAD応答が自processのupload版であることを確認する
    // (versioning有効bucket)。不一致=upload→HEADの間に他端末が索引objectを置換
    // したため、HEADのvalidator(他端末の版)と自processのentriesでfresh化せず
    // 当該scopeをstale化する(旧freshを残さない)
    if !uploaded_version_id.is_empty() && validator.version_id != uploaded_version_id {
        if let Err(mark_e) = db.mark_status(bucket, prefix, STATUS_STALE) {
            eprintln!("WARNING: sync_stateのstale化に失敗(logのみ): {mark_e}");
        }
        return Err(BackupError::Internal(format!(
            "索引object({index_key})の現行version_id({})が本runのupload応答\
             ({uploaded_version_id})と一致しないため反映を中止しました(他端末による書換えの\
             可能性。当該scopeのcacheは失効扱い)",
            validator.version_id
        )));
    }
    let status = if validator.etag.is_empty() && validator.version_id.is_empty() {
        STATUS_STALE
    } else {
        STATUS_FRESH
    };
    if let Err(e) = db.replace_scope(
        bucket,
        prefix,
        header,
        entries,
        mtimes,
        &validator,
        status,
        ReflectOrigin::Written,
    ) {
        // 置換失敗もstale化して旧freshを残さない
        if let Err(mark_e) = db.mark_status(bucket, prefix, STATUS_STALE) {
            eprintln!("WARNING: sync_stateのstale化に失敗(logのみ): {mark_e}");
        }
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{IndexLocation, IndexPin, IndexVersion};
    use crate::testutil::TempDir;

    fn entry(uuid: &str, title: &str, version_key: &str) -> IndexFileEntry {
        IndexFileEntry {
            schema: INDEX_SCHEMA,
            record_type: "file".into(),
            uuid: uuid.into(),
            title: title.into(),
            versions: vec![IndexVersion {
                date: "2026-11-05".into(),
                version_ts: "2026-11-05T10:00:00+09:00".into(),
                batch_id: "20261105-abcdef01".into(),
                s3_key: version_key.into(),
                version_id: "v1".into(),
                source_sha256: "a".repeat(64),
                age_sha256: "b".repeat(64),
                source_size: 100,
                age_size: 140,
            }],
            pins: vec![IndexPin {
                pin: "pin-1".into(),
                s3_key: format!("{version_key}.pin"),
                created_at: "2026-11-05T11:00:00+09:00".into(),
                date: "2026-11-05".into(),
                source_sha256: "a".repeat(64),
                age_sha256: "b".repeat(64),
                source_size: 100,
            }],
            locations: vec![IndexLocation {
                source_path: format!("/data/{title}"),
                category: "unfiled".into(),
                label: String::new(),
                identity_hash: "deadbeef".into(),
                last_backup: "2026-11-05".into(),
            }],
        }
    }

    fn header(entries: usize) -> IndexHeader {
        IndexHeader {
            schema: INDEX_SCHEMA,
            record_type: "index_header".into(),
            updated_at: "2026-11-05T12:00:00+09:00".into(),
            run_id: "20261105-abcdef01".into(),
            tool_version: env!("CARGO_PKG_VERSION").into(),
            entries: entries as i64,
        }
    }

    fn open_test_db(tag: &str) -> (TempDir, CatalogDb) {
        let tmp = TempDir::new(tag);
        let db = CatalogDb::open_at(&tmp.path().join(DB_FILE_NAME)).expect("test DB open");
        (tmp, db)
    }

    fn replace_default(db: &mut CatalogDb, bucket: &str, prefix: &str, entries: &[IndexFileEntry]) {
        db.replace_scope(
            bucket,
            prefix,
            &header(entries.len()),
            entries,
            &SourceMtimes::empty(),
            &Validator {
                etag: "\"etag-1\"".into(),
                version_id: "v1".into(),
            },
            STATUS_FRESH,
            ReflectOrigin::Fetched,
        )
        .expect("replace_scope");
    }

    /// [covers:catalog.schema-migration]
    /// migrationはcache class 6 table(entities/versions/pins/locations/sync_state/
    /// pending_items)+永続class 3 table(credentials/push_intents/api_keys)=9 table
    /// を構築する(TASK-13でapi_keysを追加)。各tableの主要column(設計2章schema要約)
    /// を直接観測する。
    #[test]
    fn migration_builds_nine_tables_with_expected_columns() {
        let (_tmp, db) = open_test_db("cat-schema");
        let columns = |table: &str| -> Vec<String> {
            let mut stmt = db
                .conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let cache_tables = [
            ("entities", vec!["scope", "uuid", "title"]),
            (
                "versions",
                vec![
                    "scope",
                    "s3_key",
                    "version_id",
                    "uuid",
                    "date",
                    "version_ts",
                    "batch_id",
                    "source_sha256",
                    "age_sha256",
                    "source_size",
                    "age_size",
                    "source_mtime",
                ],
            ),
            (
                "pins",
                vec![
                    "scope",
                    "pin",
                    "s3_key",
                    "created_at",
                    "date",
                    "source_sha256",
                    "source_size",
                ],
            ),
            (
                "locations",
                vec![
                    "scope",
                    "uuid",
                    "source_path",
                    "category",
                    "label",
                    "identity_hash",
                    "last_backup",
                ],
            ),
            (
                "sync_state",
                vec![
                    "scope",
                    "index_etag",
                    "index_version_id",
                    "fetched_at",
                    "last_write_at",
                    "last_verified_at",
                    "status",
                    "entry_count",
                ],
            ),
            (
                "pending_items",
                vec![
                    "scope",
                    "source_root",
                    "path",
                    "size",
                    "mtime",
                    "status",
                    "last_scan_at",
                ],
            ),
        ];
        for (table, expect_cols) in cache_tables {
            let cols = columns(table);
            for c in expect_cols {
                assert!(
                    cols.contains(&c.to_string()),
                    "{table}に{c}列がある: {cols:?}"
                );
            }
        }
        let persistent_tables = [
            (
                "credentials",
                vec!["credential_id", "passkey_json", "name", "created_at"],
            ),
            (
                "push_intents",
                vec!["id", "recorded_at", "kind", "payload_json"],
            ),
            // TASK-13(api-key認証): api-keyのhash一覧(永続class・rebuildでもdropしない。
            // key_hashはSHA-256(api-key全文)のhexで平文keyは保存しない)
            (
                "api_keys",
                vec!["credential_id", "key_hash", "name", "created_at"],
            ),
        ];
        for (table, expect_cols) in persistent_tables {
            let cols = columns(table);
            for c in expect_cols {
                assert!(
                    cols.contains(&c.to_string()),
                    "{table}に{c}列がある: {cols:?}"
                );
            }
        }
    }

    /// [covers:catalog.tags-collections-audit-log-tables]
    /// TASK-17 W2(TASK-19)の新4 table(local語彙・UI書込操作の履歴=いずれも永続
    /// class)をmigrationが構築する: tags(scope/uuid/tag/created_at・PK(scope,uuid,tag))・
    /// collections(id/scope/name/created_at・UNIQUE(scope,name))・collection_items
    /// (collection_id/scope/uuid/added_at・PK(collection_id,scope,uuid)・collections.id
    /// へのFOREIGN KEY ON DELETE CASCADE)・audit_log(id/scope/recorded_at/action/
    /// detail_json/outcome/error_code)。tags同PK行・collections同(scope,name)行の
    /// 重複挿入は制約で拒否される(設計4.1のschemaどおり)。
    #[test]
    fn migration_builds_w2_local_vocabulary_tables() {
        let (_tmp, db) = open_test_db("cat-w2-tables");
        let columns = |table: &str| -> Vec<String> {
            let mut stmt = db
                .conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let tables = [
            ("tags", vec!["scope", "uuid", "tag", "created_at"]),
            ("collections", vec!["id", "scope", "name", "created_at"]),
            (
                "collection_items",
                vec!["collection_id", "scope", "uuid", "added_at"],
            ),
            (
                "audit_log",
                vec![
                    "id",
                    "scope",
                    "recorded_at",
                    "action",
                    "detail_json",
                    "outcome",
                    "error_code",
                ],
            ),
        ];
        for (table, expect_cols) in tables {
            let cols = columns(table);
            assert!(
                !cols.is_empty(),
                "{table}が存在する(実装前はtable不在でfailを許容)"
            );
            for c in expect_cols {
                assert!(
                    cols.contains(&c.to_string()),
                    "{table}に{c}列がある: {cols:?}"
                );
            }
        }
        // PK制約: tagsの同(scope,uuid,tag)行は重複挿入不可
        db.conn
            .execute(
                "INSERT INTO tags (scope, uuid, tag, created_at) VALUES ('s', 'u', 't', '2026-11-05T12:00:00+09:00')",
                [],
            )
            .expect("tags行挿入");
        let dup = db.conn.execute(
            "INSERT INTO tags (scope, uuid, tag, created_at) VALUES ('s', 'u', 't', '2026-11-05T12:00:00+09:00')",
            [],
        );
        assert!(dup.is_err(), "tags同PK行の重複挿入は制約違反: {dup:?}");
        // UNIQUE制約: collectionsの同(scope,name)行は重複挿入不可
        db.conn
            .execute(
                "INSERT INTO collections (scope, name, created_at) VALUES ('s', 'n', '2026-11-05T12:00:00+09:00')",
                [],
            )
            .expect("collections行挿入");
        let dup = db.conn.execute(
            "INSERT INTO collections (scope, name, created_at) VALUES ('s', 'n', '2026-11-05T12:00:00+09:00')",
            [],
        );
        assert!(dup.is_err(), "collections同(scope,name)行の重複挿入はUNIQUE制約違反: {dup:?}");
    }

    /// [covers:catalog.jobs-table-schema]
    /// TASK-17 W4(TASK-27・設計4.1)のjobs table(長時間jobの正本=永続class)を
    /// migrationが構築する: id/scope/kind/status/params_json/progress_json/
    /// result_json/error_code/error_message/created_at/updated_at。
    /// insert_job→job_by_id→update_jobの行APIが一貫して動作する(初期値:
    /// status=queued・result/error系はNULL)。
    #[test]
    fn migration_builds_jobs_table() {
        let (_tmp, db) = open_test_db("cat-w4-jobs-table");
        let mut stmt = db
            .conn
            .prepare("PRAGMA table_info(jobs)")
            .expect("jobs table_info(実装前はtable不在でfailを許容)");
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for c in [
            "id",
            "scope",
            "kind",
            "status",
            "params_json",
            "progress_json",
            "result_json",
            "error_code",
            "error_message",
            "created_at",
            "updated_at",
        ] {
            assert!(cols.contains(&c.to_string()), "jobsに{c}列がある: {cols:?}");
        }
        // 行API: insert_jobはqueued・NULL系で作成しjob_by_idが読める
        let row = db
            .insert_job("b", "p/", "get", "{\"uuid\":\"u\"}", "{\"phase\":\"queued\"}")
            .expect("insert_job");
        assert_eq!(row.status, "queued");
        assert_eq!(row.kind, "get");
        assert_eq!(row.scope, "b:p/");
        assert!(row.result_json.is_none());
        assert!(row.error_code.is_none());
        assert!(!row.created_at.is_empty());
        assert_eq!(row.updated_at, row.created_at);
        let got = db.job_by_id("b", "p/", row.id).expect("job_by_id").expect("行がある");
        assert_eq!(got.id, row.id);
        // 他scopeのidでは読めない(scope分離)
        assert!(db.job_by_id("other", "p/", row.id).expect("読み").is_none());
        // update_job: status差し替え・result/errorはSome時のみ設定
        db.update_job(
            row.id,
            "succeeded",
            "{\"phase\":\"fetch\",\"done\":1}",
            Some("{\"name\":\"a\"}"),
            None,
            None,
        )
        .expect("update_job");
        let got = db.job_by_id("b", "p/", row.id).unwrap().expect("行がある");
        assert_eq!(got.status, "succeeded");
        assert_eq!(got.result_json.as_deref(), Some("{\"name\":\"a\"}"));
        assert!(got.error_code.is_none());
        assert!(got.updated_at >= got.created_at, "updated_at刻み");
        // cancel_queued_jobはqueued行のみ更新
        let other = db
            .insert_job("b", "p/", "get", "{}", "{}")
            .expect("insert_job");
        assert_eq!(db.cancel_queued_job(other.id).expect("cancel"), 1);
        assert_eq!(db.cancel_queued_job(other.id).expect("再cancel"), 0);
        // claim_queued_jobはqueued行のみrunningへ遷移する(progress差し替え)
        let third = db
            .insert_job("b", "p/", "get", "{}", "{}")
            .expect("insert_job");
        assert_eq!(
            db.claim_queued_job(third.id, "{\"phase\":\"resolve\"}")
                .expect("claim"),
            1
        );
        let got = db.job_by_id("b", "p/", third.id).unwrap().expect("行がある");
        assert_eq!(got.status, "running");
        assert_eq!(got.progress_json, "{\"phase\":\"resolve\"}");
        // running行・cancelled行はclaim不成立(取消が先行したjobを実行しない)
        assert_eq!(db.claim_queued_job(third.id, "{}").expect("再claim"), 0);
        assert_eq!(db.claim_queued_job(other.id, "{}").expect("取消済みclaim"), 0);
    }

    /// [covers:catalog.schema-migration]
    /// WAL modeで運用する・migrationは冪等(同一DBへの再openで既存内容を壊さない)。
    #[test]
    fn wal_mode_and_migration_idempotent_on_reopen() {
        let tmp = TempDir::new("cat-wal");
        let path = tmp.path().join(DB_FILE_NAME);
        {
            let mut db = CatalogDb::open_at(&path).expect("初回open");
            assert_eq!(db.journal_mode().unwrap(), "wal", "WAL mode");
            replace_default(&mut db, "b1", "p1/", &[]);
        }
        // 同一DBへの再open(migration再適用)・既存内容の保持
        let db2 = CatalogDb::open_at(&path).expect("再open(migration冪等)");
        assert_eq!(db2.journal_mode().unwrap(), "wal");
        let st = db2.sync_state("b1", "p1/").expect("sync_state読込");
        assert!(st.is_some(), "再openでも既存行が残る");
        let db3 = CatalogDb::open_at(&path).expect("3回目open");
        assert_eq!(db3.credential_count().unwrap(), 0);
        // DB fileはSQLite magic headerを持つ
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..15], b"SQLite format 3");
    }

    /// [covers:catalog.rebuild-scoped-to-cache-class]
    /// scope置換はcache classのみを対象とし、永続class(credentials)は保持する。
    /// 併せてscope分離(他scopeのcacheを消さない・prefix外の版・pinは格納しない)と
    /// 置換の冪等性を観測する。
    #[test]
    fn replace_scope_scopes_cache_and_keeps_persistent_class() {
        let (_tmp, mut db) = open_test_db("cat-scope");
        db.insert_credential_for_test("cred-1")
            .expect("credentials挿入");
        let dev_key = "amazing-backup/data/u1/default.age";
        let other_key = "prod-ns/data/u2/default.age";
        replace_default(
            &mut db,
            "b1",
            "amazing-backup/",
            &[entry("u1", "a.zip", dev_key)],
        );
        replace_default(
            &mut db,
            "b1",
            "prod-ns/",
            &[entry("u2", "b.zip", other_key)],
        );

        // dev側scopeの置換を再度実行してもprod側のcache行は消えない(冪等・衝突なし)
        replace_default(
            &mut db,
            "b1",
            "amazing-backup/",
            &[entry("u1", "a.zip", dev_key)],
        );
        let (_, prod_entries) = db.load_entries("b1", "prod-ns/").unwrap();
        assert_eq!(prod_entries.len(), 1, "後からの置換が他scopeを消さない");
        assert_eq!(prod_entries[0].title, "b.zip");

        // 永続classはcache置換でdropされない
        assert_eq!(db.credential_count().unwrap(), 1, "credentialsは保持");
        replace_default(&mut db, "b1", "prod-ns/", &[]);
        assert_eq!(
            db.credential_count().unwrap(),
            1,
            "rebuild相当の置換後も保持"
        );

        // prefix外の版・pinは当該scopeへ格納しない(混入防止)
        let (_, dev_entries) = db.load_entries("b1", "amazing-backup/").unwrap();
        for e in &dev_entries {
            for v in &e.versions {
                assert!(
                    v.s3_key.starts_with("amazing-backup/"),
                    "prefix外の版は混入しない"
                );
            }
            for p in &e.pins {
                assert!(
                    p.s3_key.starts_with("amazing-backup/"),
                    "prefix外のpinは混入しない"
                );
            }
        }
    }

    /// [covers:catalog.list-cache-hit-after-head-check]
    /// 同一process内の自端末書込直後は無条件fresh(設計4章: reflectで確定したvalidator
    /// とsync_stateの組がHEAD検証なしにfreshと判定される=unit領域の観測)。
    /// load_entriesは反映時のentry構造(並び・全field)を復元する。
    #[test]
    fn same_process_reflect_is_fresh_and_load_reconstructs_entries() {
        let (_tmp, mut db) = open_test_db("cat-fresh");
        let entries = vec![
            entry("u1", "a.zip", "p1/data/u1/default.age"),
            entry("u2", "b.zip", "p1/data/u2/default.age"),
        ];
        let v = Validator {
            etag: "\"etag-write\"".into(),
            version_id: "v9".into(),
        };
        db.replace_scope(
            "b1",
            "p1/",
            &header(entries.len()),
            &entries,
            &SourceMtimes::empty(),
            &v,
            STATUS_FRESH,
            ReflectOrigin::Written,
        )
        .expect("reflect");
        // 反映直後は同一process内で無条件fresh(validatorはreflectが確定済み)
        assert!(db.is_fresh("b1", "p1/", &v).unwrap(), "反映直後はfresh");
        // 別validatorではfreshとしない(次process起動時のHEAD検証が不一致を検出する側)
        assert!(!db.is_fresh("b1", "p1/", &Validator::default()).unwrap());
        // entry構造の完全復元(並び・versions/pins/locations)
        let (h, loaded) = db.load_entries("b1", "p1/").unwrap();
        assert_eq!(h.entries, 2);
        assert_eq!(loaded, entries, "反映時のentry構造をordinal順に復元");
        // status失効(stale化)でfreshでなくなる(旧freshを残さない)
        db.mark_status("b1", "p1/", STATUS_STALE).unwrap();
        assert!(
            !db.is_fresh("b1", "p1/", &v).unwrap(),
            "stale化後はfreshでない"
        );
    }

    /// [covers:catalog.list-cache-hit-after-head-check]
    /// load_entries_if_freshはfreshness判定とentries読込を同一の読取
    /// transaction(snapshot)で行う(cache hit経路の結合点=レビュー指摘#7)。
    /// validator一致+freshのみSomeを返し、不一致・stale・未syncはNone。
    #[test]
    fn load_entries_if_fresh_combines_check_and_read() {
        let (_tmp, mut db) = open_test_db("cat-if-fresh");
        let entries = vec![entry("u1", "a.zip", "p1/data/u1/default.age")];
        let v = Validator {
            etag: "\"etag-if\"".into(),
            version_id: "v7".into(),
        };
        // 未sync(sync_state行が無い)はNone
        assert!(db.load_entries_if_fresh("b1", "p1/", &v).unwrap().is_none());
        replace_default(&mut db, "b1", "p1/", &entries);
        // 既定のreplace_defaultはvalidator etag-1/v1・fresh
        let hit = db
            .load_entries_if_fresh(
                "b1",
                "p1/",
                &Validator {
                    etag: "\"etag-1\"".into(),
                    version_id: "v1".into(),
                },
            )
            .unwrap();
        let (h, loaded) = hit.expect("validator一致+freshはSome");
        assert_eq!(loaded.len(), 1, "同一snapshotでentriesを読む");
        assert_eq!(h.entries, 1);
        // HEAD応答のvalidator不一致(stale)はNone(呼出側はS3取得へ)
        assert!(
            db.load_entries_if_fresh("b1", "p1/", &Validator::default())
                .unwrap()
                .is_none()
        );
        // status失効もNone(旧freshを残さない)
        db.mark_status("b1", "p1/", STATUS_STALE).unwrap();
        assert!(
            db.load_entries_if_fresh(
                "b1",
                "p1/",
                &Validator {
                    etag: "\"etag-1\"".into(),
                    version_id: "v1".into(),
                },
            )
            .unwrap()
            .is_none()
        );
    }

    /// mtime基線構築用の最小file_write記録(from_reduced経由の基線供給源)。
    fn file_write_of(uuid: &str, mtime: &str, sha: &str, identity: &str) -> crate::registry::FileWriteRecord {
        crate::registry::FileWriteRecord {
            schema: 1,
            record_type: "file_write".into(),
            recorded_at: "2026-11-05T10:00:00+09:00".into(),
            batch_id: "20261105-abcdef01".into(),
            batch_date: "2026-11-05".into(),
            uuid: uuid.into(),
            title: "t1".into(),
            source_path: "/data/t1".into(),
            source_name: "t1".into(),
            source_size: 100,
            source_mtime: mtime.into(),
            source_sha256: sha.into(),
            age_path: "data/u/20261105-abcdef01.age".into(),
            age_size: 140,
            age_sha256: "b".repeat(64),
            recipients: vec!["age1recipient0000000000000000000000000000000000=".into()],
            category: "unfiled".into(),
            label: String::new(),
            identity_hash: identity.into(),
            s3_key: format!("data/{uuid}/20261105-abcdef01.age"),
        }
    }

    /// [covers:catalog.mtime-baseline-preserved-on-fetch]
    /// existing_source_mtimesは既存scope行(versions/locations)から非空のmtime基線を
    /// 構築し、その基線でFetched置換しても旧値が保持される(S3索引replica取得経路は
    /// mtime供給源を持たないため、基線を引き継がないとlocal backup記録の基線が消えて
    /// classify_scan_targetが未変更fileをfalse changedにする)。
    #[test]
    fn existing_source_mtimes_preserves_baseline_across_replacement() {
        let (_tmp, mut db) = open_test_db("cat-mtime-keep");
        let bucket = "b1";
        let prefix = "p1/";
        let entries = vec![entry("u1", "t1", "p1/data/u1/20261105-abcdef01.age")];
        let mtime = "2026-11-05T09:00:00.123456789+09:00";
        let sha = "a".repeat(64);

        // 自端末書込(Written)反映: registry由来のmtime基線付き
        let reduced = crate::registry::Reduced {
            file_writes: vec![file_write_of("u1", mtime, &sha, "deadbeef")],
            ..Default::default()
        };
        db.replace_scope(
            bucket,
            prefix,
            &header(entries.len()),
            &entries,
            &SourceMtimes::from_reduced(&reduced),
            &Validator {
                etag: "\"etag-m1\"".into(),
                version_id: "vm1".into(),
            },
            STATUS_FRESH,
            ReflectOrigin::Written,
        )
        .expect("Written反映(mtime基線付き)");

        // replica取得(Fetched)反映の前に既存行から基線を構築する=両経路の基線が復元される
        let existing = db.existing_source_mtimes(bucket, prefix).expect("基線構築");
        assert_eq!(existing.of_location(&entries[0].locations[0]), mtime);
        assert_eq!(existing.of_version("u1", &entries[0].versions[0]), mtime);

        // 構築した基線でFetched置換してもversions/locations行のmtimeは消えない
        db.replace_scope(
            bucket,
            prefix,
            &header(entries.len()),
            &entries,
            &existing,
            &Validator {
                etag: "\"etag-m2\"".into(),
                version_id: "vm2".into(),
            },
            STATUS_FRESH,
            ReflectOrigin::Fetched,
        )
        .expect("Fetched再置換(基線引き継ぎ)");
        for (table, key_col) in [("versions", "s3_key"), ("locations", "source_path")] {
            let mut stmt = db
                .conn
                .prepare(&format!(
                    "SELECT source_mtime FROM {table} WHERE scope = ?1 AND {key_col} != ''"
                ))
                .unwrap();
            let got: Vec<String> = stmt
                .query_map([scope_key(bucket, prefix)], |r| r.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(
                got,
                vec![mtime.to_string()],
                "{table}行のsource_mtimeは保持される"
            );
        }
    }

    /// [covers:catalog.written-index-validator-bound-to-uploaded-version]
    /// 書込反映はHEAD現行version_idがupload応答と一致した場合のみfresh化する。
    /// upload→HEADの間に他端末が索引objectを置換した場合(現行version_idがupload応答と
    /// 不一致)、他端末のvalidatorと自processの旧entriesの組でfresh化せず当該scopeを
    /// stale化する(以降のlist/queryがcache hitとして旧内容を受け入れるのを防ぐ)。
    /// upload応答が空(versioning無効bucket=束縛不能)の場合は従来どおりHEAD応答の
    /// validatorのみで判断する。
    #[test]
    fn reflect_written_index_binds_freshness_to_uploaded_version() {
        use crate::s3::FakeS3;
        let tmp_s3 = TempDir::new("cat-writ-vid-s3");
        let backend = FakeS3::new(tmp_s3.path());
        let (_tmp_db, mut db) = open_test_db("cat-writ-vid");
        let bucket = "b1";
        let prefix = "p1/";
        let index_key = crate::layout::index_object_key(prefix);
        let body = tmp_s3.path().join("index-body.age");
        std::fs::write(&body, b"index-bytes").expect("index body作成");
        let entries = vec![entry("u1", "t1", "p1/data/u1/20261105-abcdef01.age")];
        let mtimes = SourceMtimes::empty();
        let h = header(entries.len());
        let head_validator = || {
            let head = backend.head(bucket, &index_key).unwrap().unwrap();
            Validator::from_head(&head)
        };

        // 一致ケース: 自端末のupload版がそのまま現行版ならfresh化される
        let vid1 = backend
            .put(bucket, &index_key, &body, "STANDARD")
            .expect("put1");
        reflect_written_index_with_db(&mut db, &backend, bucket, prefix, &h, &entries, &mtimes, &vid1)
            .expect("一致時の反映は成功");
        let v1 = head_validator();
        assert!(
            db.load_entries_if_fresh(bucket, prefix, &v1)
                .unwrap()
                .is_some(),
            "一致時はfresh化される"
        );

        // 不一致ケース: upload(vid2)→HEADの間に他端末が置換(vid3が現行)
        let vid2 = backend
            .put(bucket, &index_key, &body, "STANDARD")
            .expect("put2(自端末のupload)");
        let vid3 = backend
            .put(bucket, &index_key, &body, "STANDARD")
            .expect("put3(他端末の置換)");
        assert_ne!(vid2, vid3, "fakeはput毎に新規version idを返す");
        let err = reflect_written_index_with_db(
            &mut db,
            &backend,
            bucket,
            prefix,
            &h,
            &entries,
            &mtimes,
            &vid2,
        )
        .expect_err("不一致時は反映中止(Err)");
        assert!(
            err.to_string().contains("一致しないため反映を中止"),
            "他端末書換え検出の警告: {err}"
        );
        // 当該scopeはstale化され、現行版(vid3)のvalidatorでもcache hitしない
        assert_eq!(
            db.sync_state(bucket, prefix).unwrap().map(|s| s.status),
            Some(STATUS_STALE.to_string()),
            "不一致反映後はstale(旧freshを残さない)"
        );
        assert!(
            db.load_entries_if_fresh(bucket, prefix, &head_validator())
                .unwrap()
                .is_none(),
            "他端末のvalidatorで旧entriesがfresh化されることはない"
        );

        // 束縛不能ケース(versioning無効bucketでput応答が空): 従来どおりfresh化する
        let _vid4 = backend
            .put(bucket, &index_key, &body, "STANDARD")
            .expect("put4");
        reflect_written_index_with_db(&mut db, &backend, bucket, prefix, &h, &entries, &mtimes, "")
            .expect("upload応答空は従来どおり反映");
        assert!(
            db.load_entries_if_fresh(bucket, prefix, &head_validator())
                .unwrap()
                .is_some(),
            "束縛不能時はHEAD応答のvalidatorでfresh化(従来挙動)"
        );
    }
}
