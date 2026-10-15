//! manage系(prune/lifecycle/index/jobs・設計3.2)。

use super::get::{RestoreCheck, ensure_restored, restore_guidance};
use super::{CtxArgs, now_jst, open_session, resolve_key, sha256_hex};
use crate::errors::{BackupError, BackupResult};
use crate::index::{INDEX_SCHEMA, IndexFileEntry, IndexPin, IndexVersion};
use crate::layout;
use crate::registry;
use std::collections::BTreeMap;

#[derive(clap::Args, Debug)]
pub struct ManageArgs {
    #[command(subcommand)]
    pub sub: ManageSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum ManageSub {
    /// 世代管理の即時実行(dry-run既定)
    Prune {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long, required = true)]
        category: String,
        /// 当該実行のみのkeep上書き
        #[arg(long)]
        keep: Option<u32>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
    },
    /// bucket側保持設定の確認・適用
    Lifecycle {
        #[command(subcommand)]
        sub: LifecycleSub,
    },
    /// 索引の再構築(manifestから)
    Index {
        #[command(subcommand)]
        sub: IndexSub,
    },
    /// 復元job管理(再発行)
    Jobs {
        #[command(subcommand)]
        sub: JobsSub,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum LifecycleSub {
    /// 現在の設定表示
    Show {
        #[command(flatten)]
        ctx: CtxArgs,
    },
    /// rule一式の適用(置換)
    Apply {
        #[command(flatten)]
        ctx: CtxArgs,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum IndexSub {
    /// manifest(DA)から索引を再構築
    Rebuild {
        #[command(flatten)]
        ctx: CtxArgs,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum JobsSub {
    /// 復元jobの再発行(tier変更可。復元済み・進行中にも発行できる)
    Reissue {
        #[command(flatten)]
        ctx: CtxArgs,
        /// 対象objectのS3 key
        object_key: String,
        /// 復元Tier(Standard/Bulk・既定Standard)
        #[arg(long)]
        tier: Option<String>,
    },
}

pub fn cmd_manage(args: &ManageArgs) -> i32 {
    match &args.sub {
        ManageSub::Prune {
            ctx,
            category,
            keep,
            dry_run,
            apply,
        } => cmd_manage_prune(ctx, category, *keep, *dry_run, *apply),
        ManageSub::Lifecycle { sub } => match sub {
            LifecycleSub::Show { ctx } => cmd_manage_lifecycle(ctx, &LifecycleOp::Show),
            LifecycleSub::Apply { ctx } => cmd_manage_lifecycle(ctx, &LifecycleOp::Apply),
        },
        ManageSub::Index { sub } => match sub {
            IndexSub::Rebuild { ctx } => cmd_manage_index_rebuild(ctx),
        },
        ManageSub::Jobs { sub } => match sub {
            JobsSub::Reissue {
                ctx,
                object_key,
                tier,
            } => cmd_manage_jobs_reissue(ctx, object_key, tier.as_deref()),
        },
    }
}

/// manage jobs reissue <key>(TASK-11 Phase B): 対象objectの復元jobを再発行する。
/// tierはStandard/Bulkのallowlist検査(allowlist外はusage 64・S3 API呼び出しなし)。
fn cmd_manage_jobs_reissue(ctx: &CtxArgs, key: &str, tier: Option<&str>) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let (tier, days) = super::get::resolve_tier_days(tier, Some(7))?;
        let session = open_session(ctx)?;
        session
            .backend
            .restore(&session.aws.bucket, key, days, &tier)?;
        println!("ok: 復元jobを再発行しました (key={key}, tier={tier}, days={days})");
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

fn cmd_manage_prune(
    ctx: &CtxArgs,
    category: &str,
    keep_override: Option<u32>,
    dry_run: bool,
    apply: bool,
) -> i32 {
    let run = (|| -> BackupResult<i32> {
        if dry_run && apply {
            return Err(BackupError::Usage(
                "--dry-runと--applyは併用できません(既定はdry-run・実行は--apply)".to_string(),
            ));
        }
        let session = open_session(ctx)?;
        let cfg = session.cfg.as_ref().ok_or_else(|| {
            BackupError::Config("manage pruneには--config(pipeline.toml)が必要です".to_string())
        })?;
        // categoryの検証(M-2: 予約語(sys)・旧表記(_sys)・形式違反・未定義categoryを
        // ListObjectVersionsより前に弾く)
        layout::validate_category_name(category)
            .map_err(|e| BackupError::Config(format!("--categoryが不正です: {e}")))?;
        if !cfg.categories.contains_key(category) {
            return Err(BackupError::Config(format!(
                "category({category})は[categories]に定義されていません"
            )));
        }
        let keep = match keep_override {
            Some(k) => {
                if !(1..=100).contains(&k) {
                    return Err(BackupError::Config(format!(
                        "--keepは1以上100以下で指定してください: {k}"
                    )));
                }
                k
            }
            None => cfg
                .categories
                .get(category)
                .map(|c| c.keep)
                .ok_or_else(|| {
                    BackupError::Config(format!(
                        "category({category})は[categories]に定義されていません"
                    ))
                })?,
        };
        // 対象計算: ListObjectVersions。同一keyのnoncurrent版を新しい順に並べ、
        // newer noncurrent数がkeep以上となった版を削除対象とする(NCVEと同一定義・5.2)。
        // 現行version(is_latest)・pin key(default.age以外)は対象外(構造的保護)。
        let prefix = format!("{}{}/", session.aws.prefix, category);
        let versions = session
            .backend
            .list_versions(&session.aws.bucket, &prefix)?;
        let mut by_key: BTreeMap<String, Vec<&crate::s3::VersionInfo>> = BTreeMap::new();
        for v in &versions {
            by_key.entry(v.key.clone()).or_default().push(v);
        }
        let mut deletions: Vec<(String, String)> = Vec::new();
        for (key, mut vs) in by_key {
            if !key.ends_with("/default.age") {
                continue; // pin key等は構造的保護
            }
            // listは新しい順に揃える(実装差を吸収するため明示sort・安定)。
            // 現行(is_latest)を除外した残り=noncurrent(新しい順)。
            // 順序はlast_modified_epochの数値比較(L-4: 文字列比較はRFC3339のtimezone
            // 表記混在等で辞書順と時刻順が食い違うため)
            vs.sort_by(|a, b| {
                b.last_modified_epoch
                    .cmp(&a.last_modified_epoch)
                    .then_with(|| b.version_id.cmp(&a.version_id))
            });
            vs.retain(|v| !v.is_latest && !v.is_delete_marker);
            for (i, v) in vs.iter().enumerate() {
                if i >= keep as usize {
                    deletions.push((key.clone(), v.version_id.clone()));
                }
            }
        }
        if dry_run || !apply {
            for (key, vid) in &deletions {
                println!("[dry-run] delete {key} version_id={vid}");
            }
            println!(
                "[dry-run] manage prune: category={category} keep={keep} 削除対象={}版 (実行は --apply)",
                deletions.len()
            );
            return Ok(0);
        }
        let registry_path = cfg.registry_path.clone();
        let mut deleted_ok: Vec<(String, String)> = Vec::new();
        for (key, vid) in &deletions {
            session
                .backend
                .delete_object_version(&session.aws.bucket, key, vid)?;
            println!("delete {key} version_id={vid}");
            // 削除成功分をその都度PruneRecordへ記録する(L-2: loop途中の失敗で中断して
            // も削除済み版が記録に載る=以降の索引再構築で幽霊版が復活しない。
            // 冪等な再実行で残対象の削除・記録が継続される)
            registry::append_record(
                &registry_path,
                &registry::PruneRecord {
                    schema: 1,
                    record_type: "prune".into(),
                    recorded_at: now_jst(),
                    category: category.to_string(),
                    keep,
                    deleted: vec![registry::PruneDeleted {
                        s3_key: key.clone(),
                        version_id: vid.clone(),
                    }],
                },
            )?;
            deleted_ok.push((key.clone(), vid.clone()));
        }
        // 索引の再構築・配置(削除済み版の除外はbuild_entriesがprune記録で行う)
        let records = registry::load_registry(&registry_path)?;
        let reduced = registry::reduce_records(&records);
        let entries =
            crate::index::build_entries(&reduced, &session.aws.bucket, &session.canonical_bucket);
        crate::index::upload_index(
            session.backend.as_ref(),
            &session.aws.bucket,
            &session.aws.prefix,
            &cfg.public_keys,
            &entries,
            "prune",
            &now_jst(),
            &registry_path,
        )?;
        println!("deleted {} version(s)", deleted_ok.len());
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

/// lifecycle操作(show|apply)。引数構造の都合でleafで受けてopへ写像する。
enum LifecycleOp {
    Show,
    Apply,
}

fn cmd_manage_lifecycle(ctx: &CtxArgs, op: &LifecycleOp) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let session = open_session(ctx)?;
        let cfg = session.cfg.as_ref().ok_or_else(|| {
            BackupError::Config("manage lifecycleには--config(pipeline.toml)が必要です".to_string())
        })?;
        match op {
            LifecycleOp::Show => {
                let rules = session.backend.get_lifecycle(&session.aws.bucket)?;
                match rules {
                    None => println!("未設定 (manage lifecycle apply で適用できます)"),
                    Some(rules) => {
                        for r in &rules {
                            println!("{}", format_spec(r));
                        }
                    }
                }
                Ok(0)
            }
            LifecycleOp::Apply => {
                // category毎NCVE rule(prefix filterで予約領域は構造的に対象外)+MPU abort
                let mut rules: Vec<crate::s3::LifecycleSpec> = Vec::new();
                for (name, cat) in &cfg.categories {
                    rules.push(crate::s3::LifecycleSpec::Noncurrent {
                        prefix: format!("{}{}/", session.aws.prefix, name),
                        noncurrent_days: 180,
                        newer_noncurrent_versions: cat.keep as i32,
                    });
                }
                rules.push(crate::s3::LifecycleSpec::MpuAbort {
                    prefix: session.aws.prefix.clone(),
                    days_after_initiation: 7,
                });
                for r in &rules {
                    println!("{}", format_spec(r));
                }
                session.backend.put_lifecycle(&session.aws.bucket, &rules)?;
                println!("lifecycle設定を適用しました(rule一式の置換・冪等)");
                Ok(0)
            }
        }
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

fn format_spec(r: &crate::s3::LifecycleSpec) -> String {
    match r {
        crate::s3::LifecycleSpec::Noncurrent {
            prefix,
            noncurrent_days,
            newer_noncurrent_versions,
        } => format!(
            "NCVE {prefix} NoncurrentDays={noncurrent_days} NewerNoncurrentVersions={newer_noncurrent_versions}"
        ),
        crate::s3::LifecycleSpec::MpuAbort {
            prefix,
            days_after_initiation,
        } => format!("AbortIncompleteMultipartUpload {prefix} {days_after_initiation}日"),
    }
}

fn cmd_manage_index_rebuild(ctx: &CtxArgs) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let session = open_session(ctx)?;
        let cfg = session.cfg.as_ref().ok_or_else(|| {
            BackupError::Config(
                "manage index rebuildには--config(pipeline.toml)が必要です".to_string(),
            )
        })?;
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        println!("索引をmanifest(sys/manifests/)から再構築します");
        println!("復元費用の目安: Standard ≈$0.10/GB(最大12時間) / Bulk ≈$0.02/GB(最大48時間)");

        let manifests_prefix = format!(
            "{}{}/manifests/",
            session.aws.prefix,
            layout::RESERVE_SEGMENT
        );
        let manifest_keys = session
            .backend
            .list_keys(&session.aws.bucket, &manifests_prefix)?;
        if manifest_keys.is_empty() {
            println!("0件 (manifestがありません。push実行後に再実行してください)");
            return Ok(0);
        }

        // 未復元manifestへの復元job発行(Bulk既定)。発行したら案内してexit 0(再実行で継続)
        let mut issued = 0usize;
        for key in &manifest_keys {
            match ensure_restored(session.backend.as_ref(), &session.aws, key, None, 7, "Bulk")? {
                RestoreCheck::Issued => issued += 1,
                RestoreCheck::Ongoing => issued += 1,
                RestoreCheck::Ready => {}
            }
        }
        if issued > 0 {
            println!("{}", restore_guidance("manage index rebuild"));
            return Ok(0);
        }

        // manifest取得・復号・parse
        struct Rec {
            name: String,
            category: String,
            rel_path: String,
            source_path: String,
            identity_hash: String,
            folder: String,
            s3_key: String,
            version_id: Option<String>,
            date: String,
            version_ts: String,
            batch_id: String,
            source_sha256: String,
            age_sha256: String,
            source_size: u64,
            age_size: u64,
        }
        let mut all: Vec<Rec> = Vec::new();
        for key in &manifest_keys {
            let tmp = std::env::temp_dir().join(format!(
                "backup-remote-manifest-{}-{}.age",
                std::process::id(),
                sha256_hex(key.as_bytes())
                    .chars()
                    .take(12)
                    .collect::<String>()
            ));
            session
                .backend
                .get_to_file(&session.aws.bucket, key, &tmp)?;
            let plain = crate::crypto::decrypt_bytes(&tmp, &key_path);
            let _ = std::fs::remove_file(&tmp);
            let plain = plain?;
            let text = String::from_utf8(plain).map_err(|e| {
                BackupError::Process(format!("manifest平文がUTF-8でない({key}): {e}"))
            })?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                let v: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                    BackupError::Process(format!("manifest行のparse失敗({key}): {e}"))
                })?;
                if v.get("record_type").and_then(|t| t.as_str()) != Some("file") {
                    continue;
                }
                let g = |f: &str| {
                    v.get(f)
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                all.push(Rec {
                    name: g("name"),
                    category: g("category"),
                    rel_path: g("rel_path"),
                    source_path: g("source_path"),
                    identity_hash: g("identity_hash"),
                    folder: {
                        // manifestのfolder記録を使う(欠損時はnameから導出・H-3:
                        // folderが落ちるとrebuild後のpin key生成が壊れる)
                        let f = g("folder");
                        if f.is_empty() { g("name") } else { f }
                    },
                    s3_key: g("s3_key"),
                    version_id: v
                        .get("version_id")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    date: g("date"),
                    version_ts: g("version_ts"),
                    batch_id: g("batch_id"),
                    source_sha256: g("source_sha256"),
                    age_sha256: g("age_sha256"),
                    source_size: v.get("source_size").and_then(|x| x.as_u64()).unwrap_or(0),
                    age_size: v.get("age_size").and_then(|x| x.as_u64()).unwrap_or(0),
                });
            }
        }
        // version_id欠損の補完: ListObjectVersionsで当該keyの版(古い順)から
        // 未claimのものを記録順(古い順)へ割当てる
        let keys_missing: std::collections::BTreeSet<String> = all
            .iter()
            .filter(|r| r.version_id.is_none())
            .map(|r| r.s3_key.clone())
            .collect();
        for key in keys_missing {
            let missing_idx: Vec<usize> = all
                .iter()
                .enumerate()
                .filter(|(_, r)| r.version_id.is_none() && r.s3_key == key)
                .map(|(i, _)| i)
                .collect();
            let claimed: Vec<String> = all
                .iter()
                .filter(|r| r.s3_key == key)
                .filter_map(|r| r.version_id.clone())
                .collect();
            let mut versions = session.backend.list_versions(&session.aws.bucket, &key)?;
            versions.retain(|v| !v.is_delete_marker);
            // listは新しい順→古い順へ反転
            versions.reverse();
            let mut unclaimed = versions
                .into_iter()
                .filter(|v| !claimed.contains(&v.version_id) && !v.version_id.is_empty())
                .map(|v| v.version_id)
                .collect::<Vec<_>>();
            for i in missing_idx {
                if let Some(vid) = unclaimed.first().cloned() {
                    all[i].version_id = Some(vid);
                    unclaimed.remove(0);
                }
            }
        }

        // identity単位のentry組立(versionsは新しい順)
        let mut order: Vec<String> = Vec::new();
        let mut entries: BTreeMap<String, IndexFileEntry> = BTreeMap::new();
        for r in &all {
            if !entries.contains_key(&r.identity_hash) {
                order.push(r.identity_hash.clone());
                entries.insert(
                    r.identity_hash.clone(),
                    IndexFileEntry {
                        schema: INDEX_SCHEMA,
                        record_type: "file".into(),
                        name: r.name.clone(),
                        category: r.category.clone(),
                        rel_path: r.rel_path.clone(),
                        source_path: r.source_path.clone(),
                        identity_hash: r.identity_hash.clone(),
                        folder: r.folder.clone(),
                        versions: Vec::new(),
                        pins: Vec::new(),
                    },
                );
            }
        }
        let mut sorted = all;
        sorted.sort_by(|a, b| a.version_ts.cmp(&b.version_ts));
        for r in sorted {
            if let Some(e) = entries.get_mut(&r.identity_hash) {
                e.versions.push(IndexVersion {
                    date: r.date,
                    version_ts: r.version_ts,
                    batch_id: r.batch_id,
                    s3_key: r.s3_key,
                    version_id: r.version_id.unwrap_or_default(),
                    source_sha256: r.source_sha256,
                    age_sha256: r.age_sha256,
                    source_size: r.source_size,
                    age_size: r.age_size,
                });
            }
        }
        for e in entries.values_mut() {
            e.versions.reverse(); // date降順(先頭=最新)
        }
        let mut entry_list: Vec<IndexFileEntry> = order
            .into_iter()
            .filter_map(|id| entries.remove(&id))
            .collect();
        // pins復元(H-3・設計2.3): ローカルregistryのpin_write記録(未削除)を該当entryへ
        // 接続する。採用・削除済み判定ともbucket一致(registry::record_bucket_matched。
        // M-R1: 他方bucketのpin_writeは採用せず・pin_removeもs3_bucket一致するもののみ
        // 削除済み扱い。索引構築build_entriesと同一規則)。registry喪失時(完全被災の新環境)
        // はpin情報の補完は行われない(manifestにpin実体の記録が無いため。list_versions
        // 走査での補完はPhase B課題)
        let registry_path = cfg.registry_path.clone();
        if registry_path.is_file() {
            let records = registry::load_registry(&registry_path)?;
            let reduced_now = registry::reduce_records(&records);
            for pin in reduced_now.pin_writes.values() {
                if !registry::record_bucket_matched(
                    &pin.s3_bucket,
                    &session.aws.bucket,
                    &session.canonical_bucket,
                ) {
                    continue;
                }
                if let Some(remove) = reduced_now.pin_removes.get(&pin.pin_s3_key) {
                    if registry::record_bucket_matched(
                        &remove.s3_bucket,
                        &session.aws.bucket,
                        &session.canonical_bucket,
                    ) {
                        continue;
                    }
                }
                if let Some(entry) = entry_list.iter_mut().find(|e| {
                    e.name == pin.name
                        && e.category == pin.category
                        && e.rel_path == pin.rel_path
                        && e.folder == pin.folder
                }) {
                    entry.pins.push(IndexPin {
                        pin: pin.pin.clone(),
                        s3_key: pin.pin_s3_key.clone(),
                        created_at: pin.recorded_at.clone(),
                        date: pin.date.clone(),
                        source_sha256: pin.source_sha256.clone(),
                        age_sha256: pin.age_sha256.clone(),
                        source_size: pin.source_size,
                    });
                }
            }
            // pinsの並びはs3_keyで確定する(採用源HashMapのiteration orderはprocess毎に
            // 不定。build_entriesと同一の冪等担保)
            for e in entry_list.iter_mut() {
                e.pins.sort_by(|a, b| a.s3_key.cmp(&b.s3_key));
            }
        }
        let run_id = entry_list
            .first()
            .and_then(|e| e.versions.first().map(|v| v.batch_id.clone()))
            .unwrap_or_else(|| "rebuild".to_string());
        crate::index::upload_index(
            session.backend.as_ref(),
            &session.aws.bucket,
            &session.aws.prefix,
            &cfg.public_keys,
            &entry_list,
            &run_id,
            &now_jst(),
            &registry_path,
        )?;
        println!(
            "索引を再構築しました: entries={} (manifests={})",
            entry_list.len(),
            manifest_keys.len()
        );
        Ok(0)
    })();
    match run {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}
