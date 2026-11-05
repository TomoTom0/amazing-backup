//! TASK-9 local catalog DB層(tmp/tasks/tm-9/design_catalog-frontend-auth.md
//! 2章(DB設計・D1/D11)・4章(freshness整合規則)・12章Phase P1)。
//! SQLite(rusqlite bundled+rusqlite_migration・WAL mode)へS3索引replica(復号後の
//! 平文view・D11承認)を保持し、照会系の毎回S3取得を検証済みfresh時のみ省略する。
//! 真正はJSONL registryのまま(DBは導出index+S3索引replica=cache。常にJSONL+S3から
//! 再構築可能)。
//!
//! table契約(2章・指摘#2):
//! - cache class(削除再構築可): entities/versions/pins/locations/sync_state/pending_items
//! - 永続class(rebuildでもdropしない): credentials/passkey・push_intents
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
    ])
}

/// file権限0600・dir権限0700の適用(D11: 平文replicaを含むための権限境界)。
fn apply_private_permissions(db_path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600));
    for ext in ["-wal", "-shm"] {
        let mut p = db_path.as_os_str().to_os_string();
        p.push(ext);
        let _ = std::fs::set_permissions(Path::new(&p), std::fs::Permissions::from_mode(0o600));
    }
}

/// DB親dirの作成(不在時のみ・0700。設計2章: 専用directoryは0700で作成)。
fn ensure_private_parent(db_path: &Path) -> BackupResult<()> {
    let Some(parent) = db_path.parent() else {
        return Ok(());
    };
    if parent.is_dir() {
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

impl CatalogDb {
    /// 規定path(catalog_db_path)で開く。新規作成時はdir 0700・file 0600で作成する。
    pub fn open() -> BackupResult<CatalogDb> {
        CatalogDb::open_at(&catalog_db_path())
    }

    /// 指定pathで開く(unit test・seam用)。
    pub fn open_at(db_path: &Path) -> BackupResult<CatalogDb> {
        ensure_private_parent(db_path)?;
        // 新規fileは0600で作成してからopen(既存fileの権限はopen後に0600へ合わせる)
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if !db_path.exists() {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(db_path)
                    .map_err(|e| {
                        BackupError::Config(format!(
                            "catalog DB fileの作成に失敗({}): {e}",
                            db_path.display()
                        ))
                    })?;
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
            // WAL mode(設計2章)。journal_modeは値を返すpragmaのためqueryで実行する
            conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            Ok(conn)
        };
        let mut conn = open_db().map_err(|e| {
            BackupError::Config(format!("catalog DBを開けない({}): {e}", db_path.display()))
        })?;
        migrations()
            .to_latest(&mut conn)
            .map_err(|e| BackupError::Config(format!("catalog DBのmigrationに失敗: {e}")))?;
        apply_private_permissions(db_path);
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

    /// sync_state読み出し(行が無いNone)。
    pub fn sync_state(&self, bucket: &str, prefix: &str) -> BackupResult<Option<SyncState>> {
        let scope = scope_key(bucket, prefix);
        let mut stmt = self
            .conn
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
    /// 永続class(credentials/push_intents)には触れない。
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

    /// cacheから索引entriesを復元(ordinal順=反映時の索引並び。headerはsync_stateの
    /// 保存値から再構築)。照会系のcache hit経路が使い、entry構造はS3経路と同一。
    pub fn load_entries(
        &self,
        bucket: &str,
        prefix: &str,
    ) -> BackupResult<(IndexHeader, Vec<IndexFileEntry>)> {
        let scope = scope_key(bucket, prefix);
        let Some(st) = self.sync_state(bucket, prefix)? else {
            return Ok((empty_header(), Vec::new()));
        };
        // entities(ordinal順)
        let mut entities: Vec<(String, String)> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT uuid, title FROM entities WHERE scope = ?1 ORDER BY ordinal")
                .map_err(|e| BackupError::Internal(format!("entities準備に失敗: {e}")))?;
            let rows = stmt
                .query_map([&scope], |r| {
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
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT uuid, date, version_ts, batch_id, s3_key, version_id, \
                     source_sha256, age_sha256, source_size, age_size FROM versions \
                     WHERE scope = ?1 ORDER BY uuid, ordinal",
                )
                .map_err(|e| BackupError::Internal(format!("versions準備に失敗: {e}")))?;
            let rows = stmt
                .query_map([&scope], |r| {
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
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT uuid, pin, s3_key, created_at, date, source_sha256, age_sha256,\
                     source_size FROM pins WHERE scope = ?1 ORDER BY uuid, ordinal",
                )
                .map_err(|e| BackupError::Internal(format!("pins準備に失敗: {e}")))?;
            let rows = stmt
                .query_map([&scope], |r| {
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
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT uuid, source_path, category, label, identity_hash, last_backup \
                     FROM locations WHERE scope = ?1 ORDER BY uuid, ordinal",
                )
                .map_err(|e| BackupError::Internal(format!("locations準備に失敗: {e}")))?;
            let rows = stmt
                .query_map([&scope], |r| {
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
        let entries = entities
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
            .collect();
        let header = IndexHeader {
            schema: INDEX_SCHEMA,
            record_type: "index_header".into(),
            updated_at: st.index_updated_at,
            run_id: st.index_run_id,
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            entries: st.entry_count,
        };
        Ok((header, entries))
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

/// scope分離filter: prefix外のs3_keyを持つ版・pinを除外し、版もpinも無くなった
/// entityは落とす(指摘#3。L-R1と同一の意味論をscope単位へ適用)。
fn scope_filter_entries(entries: &[IndexFileEntry], prefix: &str) -> Vec<IndexFileEntry> {
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
/// 警告表示して処理継続する(JSONLが正本のため)。
pub fn reflect_written_index(
    backend: &dyn S3Backend,
    bucket: &str,
    prefix: &str,
    header: &IndexHeader,
    entries: &[IndexFileEntry],
    mtimes: &SourceMtimes,
) -> Result<(), String> {
    let reflect = || -> BackupResult<()> {
        let mut db = CatalogDb::open()?;
        let index_key = crate::layout::index_object_key(prefix);
        // 直前にuploadした索引objectのvalidatorをHEAD 1回で確定する(この時点のsnapshot
        // 鮮度=自端末書込直後の無条件fresh。HEAD失敗時はstale化して旧freshを残さない)
        let validator = match backend.head(bucket, &index_key)? {
            Some(h) => Validator::from_head(&h),
            None => Validator::default(),
        };
        let status = if validator.etag.is_empty() && validator.version_id.is_empty() {
            STATUS_STALE
        } else {
            STATUS_FRESH
        };
        db.replace_scope(
            bucket,
            prefix,
            header,
            entries,
            mtimes,
            &validator,
            status,
            ReflectOrigin::Written,
        )?;
        Ok(())
    };
    reflect().map_err(|e| format!("{e}"))
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
    /// pending_items)+永続class 2 table(credentials/push_intents)を構築する。
    /// 各tableの主要column(設計2章schema要約)を直接観測する。
    #[test]
    fn migration_builds_eight_tables_with_expected_columns() {
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
}
