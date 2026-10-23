//! manage系(TASK-18 5.12・6.2/6.3/6.4・8章): prune(data/一括)・lifecycle(3rule)・
//! index(rebuild/reconcile)・jobs(reissue)・location rm(旧所在の明示除去)・
//! reset(dev bucket初期化の全消去・8.1)。

use super::get::{RestoreCheck, ensure_restored, restore_guidance};
use super::{CtxArgs, now_jst, open_session, open_session_with_pipeline, resolve_key, sha256_hex};
use crate::errors::{BackupError, BackupResult};
use crate::index::{INDEX_SCHEMA, IndexFileEntry, IndexLocation, IndexPin, IndexVersion};
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
    /// 世代管理の即時実行(data/一括・dry-run既定)
    Prune {
        #[command(flatten)]
        ctx: CtxArgs,
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
    /// 索引の再構築(manifestから)・整合(lifecycle消滅版)
    Index {
        #[command(subcommand)]
        sub: IndexSub,
    },
    /// 旧所在の明示除去(索引・registryの記録値のみ。S3・実体には触れない)
    Location {
        #[command(subcommand)]
        sub: LocationSub,
    },
    /// 復元job管理(再発行)
    Jobs {
        #[command(subcommand)]
        sub: JobsSub,
    },
    /// dev bucketの初期化(全消去・冪等。安全弁付き。dry-run既定・--applyで実行)
    Reset {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        apply: bool,
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
    /// manifestから索引を再構築
    Rebuild {
        #[command(flatten)]
        ctx: CtxArgs,
    },
    /// lifecycle消滅版の索引整合(既定dry-run・--applyで実行)
    Reconcile {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        apply: bool,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum LocationSub {
    /// 旧所在の除去(location_remove記録→索引再構築→metadata sync)
    Rm {
        #[command(flatten)]
        ctx: CtxArgs,
        /// 除去する所在のrealpath(source_path完全一致)
        source_path: String,
        /// entity絞り込み(uuid)
        #[arg(long)]
        uuid: Option<String>,
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
            dry_run,
            apply,
        } => cmd_manage_prune(ctx, *dry_run, *apply),
        ManageSub::Lifecycle { sub } => match sub {
            LifecycleSub::Show { ctx } => cmd_manage_lifecycle(ctx, &LifecycleOp::Show),
            LifecycleSub::Apply { ctx } => cmd_manage_lifecycle(ctx, &LifecycleOp::Apply),
        },
        ManageSub::Index { sub } => match sub {
            IndexSub::Rebuild { ctx } => cmd_manage_index_rebuild(ctx),
            IndexSub::Reconcile { ctx, apply } => cmd_manage_index_reconcile(ctx, *apply),
        },
        ManageSub::Location { sub } => match sub {
            LocationSub::Rm {
                ctx,
                source_path,
                uuid,
            } => cmd_manage_location_rm(ctx, source_path, uuid.as_deref()),
        },
        ManageSub::Jobs { sub } => match sub {
            JobsSub::Reissue {
                ctx,
                object_key,
                tier,
            } => cmd_manage_jobs_reissue(ctx, object_key, tier.as_deref()),
        },
        ManageSub::Reset { ctx, apply } => cmd_manage_reset(ctx, *apply),
    }
}

/// manage jobs reissue <key>: 対象objectの復元jobを再発行する。
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

/// manage prune(TASK-18 5.12・6.5): {prefix}data/一括。`/default.age`終端のみ・
/// noncurrent版のみ・keep=[aws.lifecycle].keep_versions(--keep廃止・実行時上書き不可)。
fn cmd_manage_prune(ctx: &CtxArgs, dry_run: bool, apply: bool) -> i32 {
    let run = (|| -> BackupResult<i32> {
        if dry_run && apply {
            return Err(BackupError::Usage(
                "--dry-runと--applyは併用できません(既定はdry-run・実行は--apply)".to_string(),
            ));
        }
        let session = open_session_with_pipeline(ctx)?;
        let cfg = session
            .cfg
            .as_ref()
            .expect("pipeline.toml必須経路(open_session_with_pipeline)");
        let keep = session.aws.lifecycle.keep_versions as usize;
        // 対象計算: ListObjectVersions(data/一括)。同一keyのnoncurrent版を新しい順に並べ、
        // newer noncurrent数がkeep以上となった版を削除対象とする(NCVEと同一定義)。
        // 現行version(is_latest)・pin key(default.age以外)・metadata keyは対象外(構造的保護)。
        let prefix = layout::data_prefix(&session.aws.prefix);
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
                continue; // pin key・metadata keyは構造的保護
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
                if i >= keep {
                    deletions.push((key.clone(), v.version_id.clone()));
                }
            }
        }
        if dry_run || !apply {
            for (key, vid) in &deletions {
                println!("[dry-run] delete {key} version_id={vid}");
            }
            println!(
                "[dry-run] manage prune: prefix={prefix} keep={keep} 削除対象={}版 (実行は --apply)",
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
                    schema: 2,
                    record_type: "prune".into(),
                    recorded_at: now_jst(),
                    deleted: vec![registry::PruneDeleted {
                        s3_key: key.clone(),
                        version_id: vid.clone(),
                    }],
                },
            )?;
            deleted_ok.push((key.clone(), vid.clone()));
        }
        // 索引の再構築・配置(削除済み版の除外はbuild_entriesがprune記録で行う)
        rebuild_and_upload(&session, &registry_path, "prune")?;
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

/// lifecycle操作(show|apply)。値はすべてaws.toml [aws.lifecycle]から(ハードコード禁止)。
enum LifecycleOp {
    Show,
    Apply,
}

fn cmd_manage_lifecycle(ctx: &CtxArgs, op: &LifecycleOp) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let session = open_session(ctx)?;
        let lc = session.aws.lifecycle;
        // 3rule(data/一括・TASK-18 7章):
        // 1) data-ncve: NCVE{NoncurrentDays, NewerNoncurrentVersions}
        // 2) mpu-abort: AbortIncompleteMultipartUpload
        // 3) delete-marker-sweep: Expiration ExpiredObjectDeleteMarker
        let rules: Vec<crate::s3::LifecycleSpec> = vec![
            crate::s3::LifecycleSpec::Noncurrent {
                prefix: layout::data_prefix(&session.aws.prefix),
                noncurrent_days: lc.noncurrent_days as i32,
                newer_noncurrent_versions: lc.keep_versions as i32,
            },
            crate::s3::LifecycleSpec::MpuAbort {
                prefix: session.aws.prefix.clone(),
                days_after_initiation: lc.mpu_abort_days as i32,
            },
            crate::s3::LifecycleSpec::DeleteMarkerSweep {
                prefix: layout::data_prefix(&session.aws.prefix),
                expired_object_delete_marker: true,
            },
        ];
        match op {
            LifecycleOp::Show => {
                let current = session.backend.get_lifecycle(&session.aws.bucket)?;
                match current {
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
        crate::s3::LifecycleSpec::DeleteMarkerSweep { prefix, .. } => {
            format!("Expiration {prefix} ExpiredObjectDeleteMarker=true")
        }
    }
}

/// registryから索引を再構築してS3へ配置する(prune/pin等の索引影響操作後)。
fn rebuild_and_upload(
    session: &super::Session,
    registry_path: &std::path::Path,
    run_tag: &str,
) -> BackupResult<()> {
    let cfg = session
        .cfg
        .as_ref()
        .ok_or_else(|| BackupError::Config("pipeline.tomlが必要です".to_string()))?;
    let records = registry::load_registry(registry_path)?;
    let reduced = registry::reduce_records(&records);
    let entries =
        crate::index::build_entries(&reduced, &session.aws.bucket, &session.canonical_bucket);
    crate::index::upload_index(
        session.backend.as_ref(),
        &session.aws.bucket,
        &session.aws.prefix,
        &cfg.public_keys,
        &entries,
        run_tag,
        &now_jst(),
        registry_path,
    )?;
    Ok(())
}

/// manage index reconcile(TASK-18 6.3): read_index → ListObjectVersions(bucket,
/// {prefix}data/)のpresent集合と突合し、索引versions[]・pins[]のうちS3に存在しない
/// もの(lifecycle消滅版)を特定 → sweep記録→索引再構築+upload。
/// 既定dry-run(件数表示)・--applyで実行。
fn cmd_manage_index_reconcile(ctx: &CtxArgs, apply: bool) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let session = open_session_with_pipeline(ctx)?;
        let cfg = session
            .cfg
            .as_ref()
            .expect("pipeline.toml必須経路(open_session_with_pipeline)");
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        let (_h, entries) = super::read_index(session.backend.as_ref(), &session.aws, &key_path)?;
        // present集合(key, version_id・delete marker除外)。version_id空の索引記録は
        // key存在のみで判定
        let versions = session.backend.list_versions(
            &session.aws.bucket,
            &layout::data_prefix(&session.aws.prefix),
        )?;
        let mut present_keys: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut present_pairs: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();
        for v in &versions {
            if v.is_delete_marker {
                continue;
            }
            present_keys.insert(v.key.clone());
            present_pairs.insert((v.key.clone(), v.version_id.clone()));
        }
        // 索引記載のうちS3に存在しない版・pinを特定
        let mut vanished: Vec<registry::PruneDeleted> = Vec::new();
        for e in &entries {
            for v in &e.versions {
                let gone = if v.version_id.is_empty() {
                    !present_keys.contains(&v.s3_key)
                } else {
                    !present_pairs.contains(&(v.s3_key.clone(), v.version_id.clone()))
                };
                if gone {
                    vanished.push(registry::PruneDeleted {
                        s3_key: v.s3_key.clone(),
                        version_id: v.version_id.clone(),
                    });
                }
            }
            for p in &e.pins {
                if !present_keys.contains(&p.s3_key) {
                    vanished.push(registry::PruneDeleted {
                        s3_key: p.s3_key.clone(),
                        version_id: String::new(),
                    });
                }
            }
        }
        if !apply {
            for d in &vanished {
                println!("[dry-run] sweep {} version_id={}", d.s3_key, d.version_id);
            }
            println!(
                "[dry-run] manage index reconcile: 消滅版候補={}件 (実行は --apply)",
                vanished.len()
            );
            return Ok(0);
        }
        // sweep記録(以降のbuild_entries・manage index rebuildでも当該版は復活しない)
        let registry_path = cfg.registry_path.clone();
        registry::append_record(
            &registry_path,
            &registry::SweepRecord {
                schema: 1,
                record_type: "sweep".into(),
                recorded_at: now_jst(),
                reason: "lifecycle".into(),
                deleted: vanished.clone(),
            },
        )?;
        rebuild_and_upload(&session, &registry_path, "reconcile")?;
        println!(
            "sweep {} version(s): 索引をS3実在集合へ整合しました",
            vanished.len()
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

/// manage location rm <source_path> [--uuid](TASK-18 6.2): 索引からsource_path完全一致の
/// 所在を探し(--uuid指定でentity絞込)、不在は対象エラー3。実行はlocation_remove記録→
/// 索引再構築+upload→当該entityのmetadata sync。S3・実体には触れない。
/// 再backupすると所在が復活する(recorded_at比較規則)。
fn cmd_manage_location_rm(ctx: &CtxArgs, source_path: &str, uuid_filter: Option<&str>) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let session = open_session_with_pipeline(ctx)?;
        let cfg = session
            .cfg
            .as_ref()
            .expect("pipeline.toml必須経路(open_session_with_pipeline)");
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        let (_h, entries) = super::read_index(session.backend.as_ref(), &session.aws, &key_path)?;
        // 索引からsource_path完全一致の所在を探す(uuid指定でentity絞込)
        let mut hits: Vec<(&IndexFileEntry, &IndexLocation)> = Vec::new();
        for e in &entries {
            if uuid_filter.is_some_and(|u| e.uuid != u) {
                continue;
            }
            for l in &e.locations {
                if l.source_path == source_path {
                    hits.push((e, l));
                }
            }
        }
        if hits.is_empty() {
            return Err(BackupError::Target(format!(
                "所在(source_path={source_path})は索引にありません(list files/queryで確認してください)"
            )));
        }
        if hits.len() > 1 {
            return Err(BackupError::Target(format!(
                "所在(source_path={source_path})が複数entityで見つかりました。--uuidで特定してください: {}",
                hits.iter()
                    .map(|(e, _)| e.uuid.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let (entry, loc) = hits[0];
        let registry_path = cfg.registry_path.clone();
        // location_remove記録 → 索引再構築+upload → metadata sync
        registry::append_record(
            &registry_path,
            &registry::LocationRemoveRecord {
                schema: 1,
                record_type: "location_remove".into(),
                recorded_at: now_jst(),
                uuid: entry.uuid.clone(),
                identity_hash: loc.identity_hash.clone(),
                source_path: loc.source_path.clone(),
            },
        )?;
        rebuild_and_upload(&session, &registry_path, "location-rm")?;
        let records = registry::load_registry(&registry_path)?;
        let reduced = registry::reduce_records(&records);
        crate::metadata::sync_entity_metadata(
            session.backend.as_ref(),
            &session.aws,
            &session.canonical_bucket,
            cfg,
            &reduced,
            &entry.uuid,
        )?;
        println!(
            "ok: 所在を除去しました (uuid={} source_path={})。S3・実体には触れていません",
            entry.uuid, loc.source_path
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

/// manage index rebuild(TASK-18 5.12・3.3): manifest(V2・schema=2: uuid/title入り)から
/// entity単位へentry再構築。locationsはfile recordsのsource_path単位(最新)から復元。
/// prunes/sweepsの除外集合を適用(prune除去版の索引への復活防止)。pin復元はuuid照合。
/// version_id欠損manifestはListObjectVersionsで補完。完了時は続けての
/// manage index reconcile実行を案内表示する。
fn cmd_manage_index_rebuild(ctx: &CtxArgs) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let session = open_session_with_pipeline(ctx)?;
        let cfg = session
            .cfg
            .as_ref()
            .expect("pipeline.toml必須経路(open_session_with_pipeline)");
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

        // manifest取得・復号・parse(TASK-18 3.3 schema=2: uuid/title入り・
        // identity_hash/folder無し・name/category/rel_path/source_pathは所在記録として維持)
        struct Rec {
            uuid: String,
            title: String,
            category: String,
            rel_path: String,
            source_path: String,
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
                "amazing-backup-manifest-{}-{}.age",
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
                    uuid: g("uuid"),
                    title: g("title"),
                    category: g("category"),
                    rel_path: g("rel_path"),
                    source_path: g("source_path"),
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

        // uuid単位のentry組立(versionsは新しい順・locationsはfile recordsの
        // source_path単位(最新)から復元。identity_hashはsource_pathから再計算
        // =file_write記録と同一規則)
        let mut order: Vec<String> = Vec::new();
        let mut entries: BTreeMap<String, IndexFileEntry> = BTreeMap::new();
        // 所在はsource_path単位で最新(version_ts最大)の記録値を採用
        struct LocRec {
            source_path: String,
            category: String,
            rel_path: String,
            last_backup: String,
        }
        let mut latest_loc: BTreeMap<(String, String), LocRec> = BTreeMap::new();
        for r in &all {
            let key = (r.uuid.clone(), r.source_path.clone());
            // 同日の再記録も最新側(>=)を採用する(低8: 同日境界で先行記録が勝つと
            // 当日の再backup所在が落ちる)
            let replace = match latest_loc.get(&key) {
                Some(prev) => prev.last_backup.as_str() <= r.version_ts.get(..10).unwrap_or(""),
                None => true,
            };
            if replace {
                latest_loc.insert(
                    key,
                    LocRec {
                        source_path: r.source_path.clone(),
                        category: r.category.clone(),
                        rel_path: r.rel_path.clone(),
                        last_backup: r.date.clone(),
                    },
                );
            }
        }
        // title導出(中1): uuid毎のversion_ts最大file recordのtitle(entity_titleと
        // 同一規則・同値はVec後方=後勝ち)。manifest走査の最初出現titleだとtitle変更後に
        // rebuildした際に旧titleへ退行するため、最終記録のtitleを採る
        let mut latest_title: BTreeMap<String, (String, String)> = BTreeMap::new();
        for r in &all {
            let replace = match latest_title.get(&r.uuid) {
                // 記録順走査のため同version_tsの後方レコードも置換する(>=)
                Some((ts, _)) => ts.as_str() <= r.version_ts.as_str(),
                None => true,
            };
            if replace {
                latest_title.insert(r.uuid.clone(), (r.version_ts.clone(), r.title.clone()));
            }
        }
        for r in &all {
            if !entries.contains_key(&r.uuid) {
                order.push(r.uuid.clone());
                let mut locations: Vec<IndexLocation> = latest_loc
                    .iter()
                    .filter(|((u, _), _)| u == &r.uuid)
                    .map(|(_, l)| IndexLocation {
                        source_path: l.source_path.clone(),
                        category: l.category.clone(),
                        rel_path: l.rel_path.clone(),
                        identity_hash: layout::identity_hash(std::path::Path::new(&l.source_path)),
                        last_backup: l.last_backup.clone(),
                    })
                    .collect();
                locations.sort_by(|a, b| a.source_path.cmp(&b.source_path));
                entries.insert(
                    r.uuid.clone(),
                    IndexFileEntry {
                        schema: INDEX_SCHEMA,
                        record_type: "file".into(),
                        uuid: r.uuid.clone(),
                        title: latest_title
                            .get(&r.uuid)
                            .map(|(_, t)| t.clone())
                            .unwrap_or_else(|| r.title.clone()),
                        versions: Vec::new(),
                        pins: Vec::new(),
                        locations,
                    },
                );
            }
        }
        let mut sorted = all;
        sorted.sort_by(|a, b| a.version_ts.cmp(&b.version_ts));
        for r in sorted {
            if let Some(e) = entries.get_mut(&r.uuid) {
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
            e.locations
                .sort_by(|a, b| a.source_path.cmp(&b.source_path)); // 冪等
        }
        let mut entry_list: Vec<IndexFileEntry> = order
            .into_iter()
            .filter_map(|id| entries.remove(&id))
            .collect();
        // pins復元(uuid照合)・prunes/sweepsの除外集合適用(TASK-18 5.12:
        // prune/sweep済み版がrebuildで索引へ復活しない。registry喪失後等の除外集合が
        // 無い環境では除外が効かないため、完了時にreconcile実行を案内する)
        let registry_path = cfg.registry_path.clone();
        let mut excluded: std::collections::BTreeSet<(String, String)> =
            std::collections::BTreeSet::new();
        if registry_path.is_file() {
            let records = registry::load_registry(&registry_path)?;
            let reduced_now = registry::reduce_records(&records);
            for d in reduced_now
                .prunes
                .iter()
                .flat_map(|p| p.deleted.iter())
                .chain(reduced_now.sweeps.iter().flat_map(|p| p.deleted.iter()))
            {
                excluded.insert((d.s3_key.clone(), d.version_id.clone()));
            }
            for e in entry_list.iter_mut() {
                e.versions.retain(|v| {
                    v.version_id.is_empty()
                        || !excluded.contains(&(v.s3_key.clone(), v.version_id.clone()))
                });
                e.versions.sort_by(|a, b| b.version_ts.cmp(&a.version_ts)); // date降順維持
            }
            for pin in reduced_now.pin_writes.values() {
                if !registry::record_bucket_matched(
                    &pin.s3_bucket,
                    &session.aws.bucket,
                    &session.canonical_bucket,
                ) {
                    continue;
                }
                if reduced_now
                    .pin_removes
                    .contains_key(&(pin.pin_s3_key.clone(), pin.s3_bucket.clone()))
                {
                    continue;
                }
                if let Some(entry) = entry_list.iter_mut().find(|e| e.uuid == pin.uuid) {
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
        println!(
            "案内: 続けて `amazing-backup manage index reconcile` を実行してください(manifest由来の再構築ではregistryのprune/sweep除外が及ばない場合があるため、S3実在集合へ整合します)"
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

/// manage reset(TASK-18 8章・8.1): dev bucket初期化の恒久コマンド(全消去・冪等)。
/// 既定dry-run・--applyで実行(prune/reconcileと同一流儀)。
/// 安全弁(違反は設定エラー2+案内・S3 API呼び出しなし): 1) 実効profileのpurposeが
/// devであること(AwsConfigのpurpose保持。prod等ではbucket指定によらず拒否)
/// 2) ローカルregistryに当該bucketと異なるbucket宛のfile_upload記録(prod由来。
/// registry.jsonlはprofile共用の1ファイル)があれば実行拒否(案内: prod記録の退避・協議)。
/// apply操作: registry退避→list_versions(root prefix)でVersions・DeleteMarkers全件を
/// delete_object_version→ローカルregistry削除(cache dirは対象外・手動掃除)→
/// 再list_versionsで0件確認。S3Backend操作は既存のlist_versions・
/// delete_object_versionのみ(新API追加なし)。
fn cmd_manage_reset(ctx: &CtxArgs, apply: bool) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let session = open_session_with_pipeline(ctx)?;
        let cfg = session
            .cfg
            .as_ref()
            .expect("pipeline.toml必須経路(open_session_with_pipeline)");
        // 安全弁1: purpose=dev限定
        if session.aws.purpose != "dev" {
            return Err(BackupError::Config(format!(
                "manage resetはdev用bucketの初期化コマンドです(実効profile {}のpurpose={})。prod等のbucketではbucket指定によらず実行できません",
                session.aws.profile, session.aws.purpose
            )));
        }
        // 安全弁2: 当該bucketと異なるbucket宛のfile_upload記録(prod由来)の混在拒否
        let registry_path = cfg.registry_path.clone();
        if registry_path.is_file() {
            let records = registry::load_registry(&registry_path)?;
            let reduced = registry::reduce_records(&records);
            if let Some(u) = reduced.file_uploads.iter().find(|u| {
                !registry::record_bucket_matched(
                    &u.s3_bucket,
                    &session.aws.bucket,
                    &session.canonical_bucket,
                )
            }) {
                return Err(BackupError::Config(format!(
                    "registry({})に当該bucket({})と異なるbucket({})宛のfile_upload記録があります。resetで消去すると当該bucket側の記録を失うため実行を拒否します(prod記録の退避・対応を協議してください)",
                    registry_path.display(),
                    session.aws.bucket,
                    u.s3_bucket
                )));
            }
        }
        let backup_path = registry_backup_path();
        let prefix = session.aws.prefix.clone();
        let versions = session
            .backend
            .list_versions(&session.aws.bucket, &prefix)?;
        let n_versions = versions.iter().filter(|v| !v.is_delete_marker).count();
        let n_markers = versions.iter().filter(|v| v.is_delete_marker).count();
        if !apply {
            println!(
                "[dry-run] manage reset: prefix={prefix} Versions={n_versions} DeleteMarkers={n_markers}"
            );
            println!("[dry-run] registry退避先: {}", backup_path.display());
            println!(
                "[dry-run] 実行は --apply (data/配下に留まらずsys/索引・manifestも消去します)"
            );
            return Ok(0);
        }
        // 1. registry退避(証跡。復元しない前提。不在ならskip)
        if registry_path.is_file() {
            if let Some(parent) = backup_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    BackupError::Process(format!(
                        "registry退避先dirの作成に失敗({}): {e}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::copy(&registry_path, &backup_path).map_err(|e| {
                BackupError::Process(format!(
                    "registry退避に失敗({} -> {}): {e}",
                    registry_path.display(),
                    backup_path.display()
                ))
            })?;
            println!(
                "registry退避: {} -> {}",
                registry_path.display(),
                backup_path.display()
            );
        } else {
            println!(
                "registry退避: skip ({}は存在しません)",
                registry_path.display()
            );
        }
        // 2. S3全消去: Versions・DeleteMarkers全件をDeleteObjectVersion
        //    (root prefix配下一括。途中失敗時は再実行で残りを削除)
        for v in &versions {
            session
                .backend
                .delete_object_version(&session.aws.bucket, &v.key, &v.version_id)?;
            println!("delete {} version_id={}", v.key, v.version_id);
        }
        // 3. ローカルregistry削除(cache dirはreset対象外・手動掃除)
        if registry_path.is_file() {
            std::fs::remove_file(&registry_path).map_err(|e| {
                BackupError::Process(format!(
                    "registryの削除に失敗({}): {e}",
                    registry_path.display()
                ))
            })?;
        }
        // 4. 検証: 再list_versionsで0件であることを確認表示
        let rest = session
            .backend
            .list_versions(&session.aws.bucket, &prefix)?;
        if rest.is_empty() {
            println!("検証: 再list_versionsで残存version 0件を確認しました");
        } else {
            return Err(BackupError::Process(format!(
                "reset後も{}件のversionが残っています。再実行(--apply)で残りを削除してください",
                rest.len()
            )));
        }
        println!("reset完了: Versions={n_versions} DeleteMarkers={n_markers}を削除しました");
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

/// registry退避先path(8.1): cwd基準 ./tmp/<yyyymmdd(JST)>_registry_before_reset.jsonl。
fn registry_backup_path() -> std::path::PathBuf {
    let jst = chrono::FixedOffset::east_opt(9 * 3600).expect("JST offset");
    let ymd = chrono::Utc::now()
        .with_timezone(&jst)
        .format("%Y%m%d")
        .to_string();
    std::path::PathBuf::from(format!("./tmp/{ymd}_registry_before_reset.jsonl"))
}
