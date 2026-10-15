//! backup push(書き込み)・backup pull(一括復元)・backup verify(ローカル整合検査)。
//! 設計3.2・8.5(暗号化経路・checksum・multipart等の共通基盤は変更なし)。

use super::{
    CtxArgs, fmt_size, load_aws, mtime_jst_precise, now_jst, require_versioning, sha256_hex,
};
use crate::config::{self, PipelineConfig};
use crate::errors::{BackupError, BackupResult};
use crate::registry::{self, FileUploadRecord, FileWriteRecord, Reduced};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct BackupArgs {
    #[command(subcommand)]
    pub sub: BackupSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum BackupSub {
    /// 書き込み(暗号化→upload→manifest→索引更新を1実行で)
    Push {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        from_list: Option<PathBuf>,
        /// 対象アーカイブ(zip/7z)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// 予定表示のみ(旧plan相当)
        #[arg(long)]
        dry_run: bool,
        /// backup時ピン(uploadと同時に版名でも上げる)
        #[arg(long)]
        pin: Option<String>,
        #[arg(long)]
        keep_going: bool,
    },
    /// 一括復元(確認付き)
    Pull {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        tier: Option<String>,
        #[arg(long)]
        days: Option<u32>,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        /// 確認を省略(非TTYでは必須)
        #[arg(long)]
        yes: bool,
    },
    /// ローカル整合検査(registry・data/outのrun成果物)
    Verify {
        #[command(flatten)]
        ctx: CtxArgs,
        /// .ageのsha256を再計算してregistry記録と照合
        #[arg(long)]
        checksum: bool,
        /// 秘密鍵で復号し平文sha256をregistry記録と照合
        #[arg(long)]
        roundtrip: bool,
    },
}

pub fn cmd_backup(args: &BackupArgs) -> i32 {
    match &args.sub {
        BackupSub::Push {
            ctx,
            from_list,
            paths,
            dry_run,
            pin,
            keep_going,
        } => cmd_backup_push(ctx, from_list, paths, *dry_run, pin.as_deref(), *keep_going),
        BackupSub::Pull {
            ctx,
            category,
            run,
            all,
            tier,
            days,
            out,
            dry_run,
            yes,
        } => super::pull::cmd_backup_pull(
            ctx,
            category.as_deref(),
            run.as_deref(),
            *all,
            tier.as_deref(),
            days.to_owned(),
            out.as_deref(),
            *dry_run,
            *yes,
        ),
        BackupSub::Verify {
            ctx,
            checksum,
            roundtrip,
        } => crate::cli::cmd_verify(&crate::cli::VerifyArgs {
            config: ctx.config.clone(),
            date: None,
            checksum: *checksum,
            roundtrip: *roundtrip,
            key: ctx.key.clone(),
        }),
    }
}

/// pushの対象1件(解決・分類・衝突folder確定済み)。
struct Planned {
    target: crate::targets::Target,
    category: String,
    rel_path: String,
    identity: String,
    folder: String,
    s3_key: String,
}

fn plan_push(
    targets: &[crate::targets::Target],
    cfg: &PipelineConfig,
    aws_prefix: &str,
    reduced: &Reduced,
) -> Result<Vec<Planned>, BackupError> {
    let files: BTreeMap<String, crate::layout::FileMapping> = cfg
        .files
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                crate::layout::FileMapping {
                    category: v.category.clone(),
                    path: v.path.clone(),
                },
            )
        })
        .collect();
    let unfiled = cfg.layout.unfiled_category.clone();
    let roots = cfg.layout.auto_path_roots.clone();

    struct Resolved {
        idx: usize,
        category: String,
        rel_path: String,
        identity: String,
    }
    let mut resolved: Vec<Resolved> = Vec::with_capacity(targets.len());
    for (idx, t) in targets.iter().enumerate() {
        let c = crate::layout::classify(&t.source_path, &t.source_name, &files, &unfiled, &roots)?;
        resolved.push(Resolved {
            idx,
            category: c.category,
            rel_path: c.rel_path,
            identity: crate::layout::identity_hash(&t.source_path),
        });
    }

    // 衝突folderの決定論的帰属(設計1.2):
    // (1) registryに同一slot(category+rel_path+name)の無hash保持者があれば維持
    // (2) 自identityに前回folderがあればそれを再利用(安定)
    // (3) 新規slotは同一run内の辞書順最小realpathのみ無hash
    let mut planned = Vec::with_capacity(targets.len());
    for r in &resolved {
        let t = &targets[r.idx];
        let slot_plain_holder = reduced.file_writes.iter().find_map(|w| {
            (w.category == r.category
                && w.rel_path == r.rel_path
                && w.source_name == t.source_name
                && w.folder == t.source_name)
                .then_some(w.identity_hash.as_str())
        });
        let folder = if let Some(holder) = slot_plain_holder {
            // 既存の無hash保持者の維持判定はlayout::existing_holder_primaryへ統一(L-12)
            let primary = crate::layout::existing_holder_primary(holder, &r.identity);
            crate::layout::folder_name(&t.source_name, &r.identity, primary)
        } else if let Some(prev) = reduced
            .file_writes
            .iter()
            .find(|w| w.identity_hash == r.identity)
        {
            prev.folder.clone()
        } else {
            // 同一run内の同slot候補で辞書順最小が無hash
            let candidates: Vec<String> = resolved
                .iter()
                .filter(|o| {
                    o.category == r.category
                        && o.rel_path == r.rel_path
                        && targets[o.idx].source_name == t.source_name
                })
                .map(|o| targets[o.idx].source_path.to_string_lossy().into_owned())
                .collect();
            let own = t.source_path.to_string_lossy().into_owned();
            let primary = crate::layout::lexicographic_primary(
                &candidates.iter().map(String::as_str).collect::<Vec<_>>(),
            )
            .is_some_and(|m| m == own);
            crate::layout::folder_name(&t.source_name, &r.identity, primary)
        };
        let key = crate::layout::file_object_key(aws_prefix, &r.category, &r.rel_path, &folder)?;
        planned.push(Planned {
            target: clone_target(t),
            category: r.category.clone(),
            rel_path: r.rel_path.clone(),
            identity: r.identity.clone(),
            folder,
            s3_key: key,
        });
    }
    Ok(planned)
}

/// targets::Targetの簡易clone(derive Cloneが無いため手写し)。
fn clone_target(t: &crate::targets::Target) -> crate::targets::Target {
    crate::targets::Target {
        source_path: t.source_path.clone(),
        source_name: t.source_name.clone(),
        size: t.size,
        mtime: t.mtime,
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_backup_push(
    ctx: &CtxArgs,
    from_list: &Option<PathBuf>,
    paths: &[PathBuf],
    dry_run: bool,
    pin: Option<&str>,
    keep_going: bool,
) -> i32 {
    // pin名は引数形式検証として最初に(usage 64)
    if let Some(pin_name) = pin
        && let Err(e) = crate::layout::validate_pin_name(pin_name)
    {
        eprintln!("error: {e}");
        return e.exit_code();
    }
    let run = (|| -> BackupResult<i32> {
        let cfg = match &ctx.config {
            Some(p) => config::load_pipeline(p)?,
            None => {
                return Err(BackupError::Config(
                    "backup pushには--config(pipeline.toml)が必要です".to_string(),
                ));
            }
        };
        let aws = load_aws(&ctx.aws_config, ctx.profile.as_deref())?;

        let targets = crate::targets::resolve_targets_storage(paths, from_list.as_deref())?;
        let registry_path = cfg.registry_path.clone();
        let records = registry::load_registry(&registry_path)?;
        let reduced = registry::reduce_records(&records);
        let planned = plan_push(&targets, &cfg, &aws.prefix, &reduced)?;

        if dry_run {
            // 旧plan相当: 対象・分類・衝突・size集計・予定keyの表示(S3 API呼び出しなし)
            let total: u64 = planned.iter().map(|p| p.target.size).sum();
            for p in &planned {
                println!(
                    "push  {}  {}/{}  {}  ->  {}",
                    p.target.source_name,
                    p.category,
                    if p.rel_path.is_empty() {
                        "-".to_string()
                    } else {
                        p.rel_path.clone()
                    },
                    fmt_size(p.target.size),
                    p.s3_key
                );
            }
            println!(
                "plan: {}件  計 {}  (dry-run: 暗号化・S3 API呼び出しは行いません)",
                planned.len(),
                fmt_size(total)
            );
            return Ok(0);
        }

        let backend = crate::cli::select_backend(&aws)?;
        require_versioning(backend.as_ref(), &aws)?;

        let batch_date = crate::targets::jst_today();
        let batch_id = format!(
            "{}-{}",
            batch_date.replace('-', ""),
            &sha256_hex(format!("{:?}", std::time::SystemTime::now()).as_bytes())[..8]
        );
        let run_dir = cfg.output_dir.join(&batch_id);
        std::fs::create_dir_all(&run_dir)
            .map_err(|e| BackupError::Process(format!("run dirの作成に失敗: {e}")))?;

        let mut uploaded: Vec<UploadItem> = Vec::new();
        let mut skipped = 0usize;
        let mut skipped_identities: Vec<String> = Vec::new();
        let mut failed = 0usize;
        let mut first_err: Option<i32> = None;

        for p in &planned {
            let mtime_str = mtime_jst_precise(&p.target.source_path)?;
            // skip判定(registry基準・8.3+M-1): file_write成功記録+指紋一致に加え、
            // 同一identityのfile_upload記録(=S3へ実際に載った版)が存在することも確認する
            // (file_write記録後・file_upload記録前にcrashした対象は再uploadする)
            let skip = reduced.file_writes.iter().any(|w| {
                w.identity_hash == p.identity
                    && w.source_size == p.target.size
                    && w.source_mtime == mtime_str
            }) && reduced
                .file_uploads
                .iter()
                .any(|u| u.identity_hash == p.identity && u.s3_key == p.s3_key);
            if skip {
                println!("skip  {} (registry match)", p.target.source_name);
                skipped += 1;
                skipped_identities.push(p.identity.clone());
                continue;
            }
            let age_path = run_dir.join(format!("{}.age", p.target.source_name));
            let res = match crate::crypto::encrypt_stream(
                &p.target.source_path,
                &age_path,
                &cfg.public_keys,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {}: {e}", p.target.source_path.display());
                    failed += 1;
                    if matches!(e, BackupError::Config(_)) {
                        return Err(e);
                    }
                    first_err.get_or_insert(e.exit_code());
                    if !keep_going {
                        return Err(e);
                    }
                    continue;
                }
            };
            let recorded_at = now_jst();
            let version_id =
                match backend.put(&aws.bucket, &p.s3_key, &age_path, &aws.storage_class) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("error: {e}");
                        failed += 1;
                        first_err.get_or_insert(e.exit_code());
                        if !keep_going {
                            return Err(e);
                        }
                        continue;
                    }
                };
            let write = FileWriteRecord {
                schema: 1,
                record_type: "file_write".into(),
                recorded_at: recorded_at.clone(),
                batch_id: batch_id.clone(),
                batch_date: batch_date.clone(),
                source_path: p.target.source_path.display().to_string(),
                source_name: p.target.source_name.clone(),
                source_size: p.target.size,
                source_mtime: mtime_str.clone(),
                source_sha256: res.source_sha256.clone(),
                age_path: age_path.display().to_string(),
                age_size: res.age_size,
                age_sha256: res.age_sha256.clone(),
                recipients: res.recipients.clone(),
                category: p.category.clone(),
                rel_path: p.rel_path.clone(),
                identity_hash: p.identity.clone(),
                folder: p.folder.clone(),
                s3_key: p.s3_key.clone(),
            };
            // version_tsの供給源はfile_write記録のrecorded_at(暗号化時刻。TASK-11 Phase B。
            // upload記録の時刻とは独立)
            let upload = FileUploadRecord {
                schema: 1,
                record_type: "file_upload".into(),
                recorded_at: recorded_at.clone(),
                batch_id: batch_id.clone(),
                version_ts: recorded_at.clone(),
                name: p.target.source_name.clone(),
                category: p.category.clone(),
                rel_path: p.rel_path.clone(),
                identity_hash: p.identity.clone(),
                s3_key: p.s3_key.clone(),
                s3_bucket: aws.bucket.clone(),
                s3_region: aws.region.clone(),
                version_id: version_id.clone(),
                s3_size: res.age_size as i64,
                s3_checksum_sha256: res.age_sha256.clone(),
                s3_checksum_type: "FULL_OBJECT".into(),
                source_sha256: res.source_sha256.clone(),
                age_sha256: res.age_sha256.clone(),
                source_size: p.target.size,
                age_size: res.age_size,
                uploaded_at: recorded_at.clone(),
            };
            if let Err(e) = registry::append_record(&registry_path, &write)
                .and_then(|()| registry::append_record(&registry_path, &upload))
            {
                eprintln!("error: {e}");
                failed += 1;
                first_err.get_or_insert(e.exit_code());
                if !keep_going {
                    return Err(e);
                }
                continue;
            }
            uploaded.push(UploadItem {
                name: p.target.source_name.clone(),
                category: p.category.clone(),
                rel_path: p.rel_path.clone(),
                source_path: p.target.source_path.display().to_string(),
                identity: p.identity.clone(),
                folder: p.folder.clone(),
                s3_key: p.s3_key.clone(),
                version_id: version_id.clone(),
                date: batch_date.clone(),
                version_ts: recorded_at.clone(),
                source_sha256: res.source_sha256.clone(),
                age_sha256: res.age_sha256.clone(),
                source_size: p.target.size,
                age_size: res.age_size,
                age_path: age_path.display().to_string(),
            });
            println!(
                "ok    {}  ->  {} (version {})",
                p.target.source_name, p.s3_key, version_id
            );
        }

        // backup時ピン: upload済み.ageを版名keyへも直接upload(復元job不要・CopyObject制約なし)
        if let Some(pin_name) = pin {
            for item in &uploaded {
                let pin_key = crate::layout::pin_object_key(
                    &aws.prefix,
                    &item.category,
                    &item.rel_path,
                    &item.folder,
                    pin_name,
                )?;
                if backend.head(&aws.bucket, &pin_key)?.is_some() {
                    return Err(BackupError::Target(format!(
                        "pin名({pin_name})は既に存在します(write-once)。pin rm後に再付与してください: {pin_key}"
                    )));
                }
                let pin_vid = backend.put(
                    &aws.bucket,
                    &pin_key,
                    std::path::Path::new(&item.age_path),
                    &aws.storage_class,
                )?;
                let recorded = registry::PinWriteRecord {
                    schema: 1,
                    record_type: "pin_write".into(),
                    recorded_at: now_jst(),
                    name: item.name.clone(),
                    category: item.category.clone(),
                    rel_path: item.rel_path.clone(),
                    folder: item.folder.clone(),
                    pin: pin_name.to_string(),
                    pin_s3_key: pin_key.clone(),
                    source_s3_key: item.s3_key.clone(),
                    source_version_id: item.version_id.clone(),
                    date: item.date.clone(),
                    source_sha256: item.source_sha256.clone(),
                    age_sha256: item.age_sha256.clone(),
                    source_size: item.source_size,
                    s3_bucket: aws.bucket.clone(),
                    s3_size: item.age_size as i64,
                    s3_checksum_sha256: item.age_sha256.clone(),
                    uploaded_at: now_jst(),
                };
                registry::append_record(&registry_path, &recorded)?;
                println!("ok    pin {pin_name} -> {pin_key} (version {pin_vid})");
            }

            // skip対象(変更なし)への--pin指定: 警告表示のうえ最新版へpin付与(M-6。
            // 当該runに新規.ageが無いためCopyObjectで写す)
            if !skipped_identities.is_empty() {
                println!(
                    "WARNING: --pin指定の対象に変更なし(skip)のfileが{}件あります。最新版へpinを付与します",
                    skipped_identities.len()
                );
                for identity in &skipped_identities {
                    let Some(write) = reduced
                        .file_writes
                        .iter()
                        .find(|w| &w.identity_hash == identity)
                    else {
                        continue;
                    };
                    // 当該identityの最新版(registry記録順の最後)
                    let Some(version) = reduced
                        .file_uploads
                        .iter()
                        .rfind(|u| &u.identity_hash == identity)
                    else {
                        continue;
                    };
                    let pin_key = crate::layout::pin_object_key(
                        &aws.prefix,
                        &write.category,
                        &write.rel_path,
                        &write.folder,
                        pin_name,
                    )?;
                    if backend.head(&aws.bucket, &pin_key)?.is_some() {
                        eprintln!(
                            "WARNING: pin名({pin_name})は既に存在します(write-once)。skip対象のpin付与を飛ばします: {pin_key}"
                        );
                        continue;
                    }
                    match super::get::ensure_restored(
                        backend.as_ref(),
                        &aws,
                        &version.s3_key,
                        Some(&version.version_id),
                        7,
                        "Standard",
                    )? {
                        super::get::RestoreCheck::Issued | super::get::RestoreCheck::Ongoing => {
                            println!(
                                "{}",
                                super::get::restore_guidance(&format!(
                                    "pin add {} --as {pin_name}",
                                    write.source_name
                                ))
                            );
                            continue;
                        }
                        super::get::RestoreCheck::Ready => {}
                    }
                    backend.copy_object(
                        &aws.bucket,
                        &version.s3_key,
                        Some(&version.version_id),
                        &pin_key,
                        &aws.storage_class,
                    )?;
                    registry::append_record(
                        &registry_path,
                        &registry::PinWriteRecord {
                            schema: 1,
                            record_type: "pin_write".into(),
                            recorded_at: now_jst(),
                            name: write.source_name.clone(),
                            category: write.category.clone(),
                            rel_path: write.rel_path.clone(),
                            folder: write.folder.clone(),
                            pin: pin_name.to_string(),
                            pin_s3_key: pin_key.clone(),
                            source_s3_key: version.s3_key.clone(),
                            source_version_id: version.version_id.clone(),
                            date: version.recorded_at.get(..10).unwrap_or("").to_string(),
                            source_sha256: version.source_sha256.clone(),
                            age_sha256: version.age_sha256.clone(),
                            source_size: version.source_size,
                            s3_bucket: aws.bucket.clone(),
                            s3_size: version.age_size as i64,
                            s3_checksum_sha256: version.age_sha256.clone(),
                            uploaded_at: now_jst(),
                        },
                    )?;
                    println!("ok    pin {pin_name} -> {pin_key} (skip対象の最新版)");
                }
            }
        }

        // run manifest(F8: file upload後に構築。file recordに実s3_key+version_idを記録)
        if !uploaded.is_empty() {
            // ヘッダのrecipientsは現在設定の指紋列
            let mut current_fingerprints = Vec::with_capacity(cfg.public_keys.len());
            for k in &cfg.public_keys {
                let pk = crate::fingerprint::parse_ssh_pubkey(k)?;
                current_fingerprints.push(crate::fingerprint::fingerprint(&pk));
            }
            let manifest_body = crate::manifest::build_manifest_v2(
                &batch_id,
                &now_jst(),
                &uploaded
                    .iter()
                    .map(|u| crate::manifest::ManifestV2File {
                        name: u.name.clone(),
                        category: u.category.clone(),
                        rel_path: u.rel_path.clone(),
                        source_path: u.source_path.clone(),
                        identity_hash: u.identity.clone(),
                        folder: u.folder.clone(),
                        s3_key: u.s3_key.clone(),
                        version_id: u.version_id.clone(),
                        date: u.date.clone(),
                        version_ts: u.version_ts.clone(),
                        batch_id: batch_id.clone(),
                        source_sha256: u.source_sha256.clone(),
                        age_sha256: u.age_sha256.clone(),
                        source_size: u.source_size,
                        age_size: u.age_size,
                    })
                    .collect::<Vec<_>>(),
                &current_fingerprints,
            )?;
            let manifest_local = run_dir.join("manifest.jsonl.age");
            crate::crypto::encrypt_bytes(
                manifest_body.as_bytes(),
                &manifest_local,
                &cfg.public_keys,
            )?;
            let manifest_key = crate::layout::manifest_object_key(&aws.prefix, &batch_id)?;
            let m_vid = backend.put(
                &aws.bucket,
                &manifest_key,
                &manifest_local,
                &aws.storage_class,
            )?;
            let m_size = std::fs::metadata(&manifest_local)
                .map(|m| m.len() as i64)
                .unwrap_or(-1);
            registry::append_record(
                &registry_path,
                &registry::ManifestV2Record {
                    schema: 1,
                    record_type: "manifest".into(),
                    recorded_at: now_jst(),
                    batch_id: batch_id.clone(),
                    s3_key: manifest_key.clone(),
                    s3_bucket: aws.bucket.clone(),
                    s3_region: aws.region.clone(),
                    version_id: m_vid,
                    s3_size: m_size,
                    s3_checksum_sha256: String::new(),
                    s3_checksum_type: "FULL_OBJECT".into(),
                    uploaded_at: now_jst(),
                },
            )?;
        }

        // 索引の再構築+配置(成功run末尾で常時・冪等)。前回runがregistry記録後に失敗した
        // 場合の再実行が全skipでも、ここで未反映分を拾って索引が更新される(H-2)
        {
            let records_now = registry::load_registry(&registry_path)?;
            let reduced_now = registry::reduce_records(&records_now);
            let entries = crate::index::build_entries(&reduced_now);
            crate::index::upload_index(
                backend.as_ref(),
                &aws.bucket,
                &aws.prefix,
                &cfg.public_keys,
                &entries,
                &batch_id,
                &now_jst(),
                &registry_path,
            )?;
        }

        // data/out掃除(keep_artifacts="run"・run成功時のみ)。掃除対象は当該run分
        // (<batch_id> dir)のみ(旧経路産物・失敗runの残りを消さない)
        if failed == 0 && matches!(cfg.keep_artifacts, config::KeepArtifacts::Run) {
            let _ = std::fs::remove_dir_all(&run_dir);
        }

        println!(
            "run {batch_id}: ok={} skip={} failed={}",
            uploaded.len(),
            skipped,
            failed
        );
        if let Some(code) = first_err {
            return Ok(code);
        }
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

/// pushでuploadした対象1件(manifest・pin組み立て用)。
pub(crate) struct UploadItem {
    name: String,
    category: String,
    rel_path: String,
    source_path: String,
    identity: String,
    folder: String,
    s3_key: String,
    version_id: String,
    date: String,
    version_ts: String,
    source_sha256: String,
    age_sha256: String,
    source_size: u64,
    age_size: u64,
    age_path: String,
}
