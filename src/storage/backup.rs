//! backup push(書き込み)・backup pull(一括復元)・backup verify(ローカル整合検査)。
//! TASK-18 4.1/5.7: 所在skip/uuid再利用判定→暗号化→sha照合(合流/新規発行)の
//! 対象1件処理とrun内状態(run_map・run_entity_titles・touched_entities)。

use super::{
    CtxArgs, fmt_size, load_aws, mtime_jst_precise, now_jst, require_versioning, sha256_hex,
    sha256_hex_streaming,
};
use crate::config::{self, PipelineConfig};
use crate::errors::{BackupError, BackupResult};
use crate::registry::{self, FileUploadRecord, FileWriteRecord, Reduced};
use std::collections::{BTreeMap, HashMap};
use std::io::IsTerminal as _;
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct BackupArgs {
    #[command(subcommand)]
    pub sub: BackupSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum BackupSub {
    /// 書き込み(暗号化→upload→manifest→索引更新→metadata syncを1実行で)
    Push {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        from_list: Option<PathBuf>,
        /// 対象アーカイブ(zip/7z)
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// 予測表示のみ(暗号化せず平文sha256のみ計算)
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
        /// 出力dir(必須: 省略はusage 64。<out>/<title>へ配置)
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        dry_run: bool,
        /// 確認を省略(非TTYでは必須)
        #[arg(long)]
        yes: bool,
    },
    /// ローカル整合検査(registry・cache dirのrun成果物)
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
            out,
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

/// pushの対象1件(解決・分類解決済み。TASK-18: キーは発行時のuuidから生成するため
/// plan段階では分類とidentityのみ確定する)。
struct Planned {
    target: crate::targets::Target,
    category: String,
    label: String,
    identity: String,
}

fn plan_push(
    targets: &[crate::targets::Target],
    cfg: &PipelineConfig,
) -> Result<Vec<Planned>, BackupError> {
    let files: BTreeMap<String, crate::layout::FileMapping> = cfg
        .files
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                crate::layout::FileMapping {
                    category: v.category.clone(),
                    label: v.label.clone(),
                },
            )
        })
        .collect();
    let unfiled = cfg.layout.unfiled_category.clone();
    let roots = cfg.layout.auto_path_roots.clone();
    let mut planned = Vec::with_capacity(targets.len());
    for t in targets {
        let c = crate::layout::classify(&t.source_path, &t.source_name, &files, &unfiled, &roots)?;
        planned.push(Planned {
            identity: crate::layout::identity_hash(&t.source_path),
            category: c.category,
            label: c.label,
            target: clone_target(t),
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
        title: t.title.clone(),
    }
}

/// 新規発行時のtitle入力prompt(tty時)。空・違反は再要求する(6.1)。
fn prompt_title() -> BackupResult<String> {
    use std::io::Write as _;
    loop {
        print!("title: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| BackupError::Process(format!("title入力の読込に失敗: {e}")))?;
        let title = line.trim().to_string();
        match crate::layout::validate_title(&title) {
            Ok(()) => return Ok(title),
            Err(e) => eprintln!("{e}(再入力してください)"),
        }
    }
}

/// 合流・再PUT時のtitle解決(4.1 resolve_title): 指定titleがあれば現行titleと比較
/// (異なればtitle変更扱い)・未指定なら現行titleを継続(prompt不要)。
fn resolve_title(current: Option<&str>, specified: Option<&str>) -> BackupResult<String> {
    match specified {
        Some(t) => {
            crate::layout::validate_title(t)?;
            Ok(t.to_string())
        }
        None => Ok(current.unwrap_or_default().to_string()),
    }
}

/// run内のbackup push実行状態(4.1前提)。
struct RunState {
    /// 同一run内同一sha合流(即時追記+このmap)
    run_map: HashMap<String, String>,
    /// run内のentity title最新値(初期値=reducedから一括導出・file_write追記のたび更新。
    /// 同一run内のtitle変更を2件目以降の合流にも反映させる)
    run_entity_titles: HashMap<String, String>,
    /// run末尾のmetadata sync対象uuid(記録順・重複除去)
    touched_entities: Vec<String>,
    /// 当該runで追記したfile_upload記録(pin組み立て等の最新版参照用)
    run_uploads: Vec<FileUploadRecord>,
}

impl RunState {
    /// run開始時の初期化(4.2): run_entity_titlesはreducedから一括導出
    /// (uuid毎のrecorded_at最大・同値ならVec後方=後勝ちのtitle)。
    fn new(reduced: &Reduced) -> Self {
        let mut by_uuid: HashMap<&str, Vec<&registry::FileWriteRecord>> = HashMap::new();
        for w in &reduced.file_writes {
            by_uuid.entry(w.uuid.as_str()).or_default().push(w);
        }
        let run_entity_titles = by_uuid
            .into_iter()
            .map(|(uuid, writes)| {
                let title = writes
                    .iter()
                    .max_by(|a, b| a.recorded_at.cmp(&b.recorded_at))
                    .map(|w| w.title.clone())
                    .unwrap_or_default();
                (uuid.to_string(), title)
            })
            .collect();
        RunState {
            run_map: HashMap::new(),
            run_entity_titles,
            touched_entities: Vec::new(),
            run_uploads: Vec::new(),
        }
    }

    fn touch(&mut self, uuid: &str) {
        if !self.touched_entities.iter().any(|u| u == uuid) {
            self.touched_entities.push(uuid.to_string());
        }
    }

    fn set_title(&mut self, uuid: &str, title: &str) {
        self.run_entity_titles
            .insert(uuid.to_string(), title.to_string());
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
        // pipeline.tomlは必須経路。--config省略時はXDG規定path(TASK-19)を読む
        // (不在はcopy案内付き設定エラー2)
        let cfg = config::load_pipeline(&super::pipeline_config_path(&ctx.config))?;
        let aws = load_aws(&ctx.aws_config, ctx.profile.as_deref())?;
        // 正本profile(active_profile)のbucket(bucket一致規則の正本側)
        let canonical_bucket = config::canonical_bucket(&super::aws_config_path(&ctx.aws_config))?;

        // 接続先表示: 実効profile・bucket・prefixを実行の最初(対象解決・S3 API呼び出しに
        // 先行)に表示し、誤profile・誤bucketへの書き込みに気づかせる。dry-runでも表示
        println!(
            "接続先: profile={} bucket={} prefix={}",
            aws.profile, aws.bucket, aws.prefix
        );

        let targets = crate::targets::resolve_targets_storage(paths, from_list.as_deref())?;
        // from-listのtitle欄は引数形式検証として先に(usage 64・S3 API呼び出しに先行)
        for t in &targets {
            if let Some(title) = t.title.as_deref()
                && let Err(e) = crate::layout::validate_title(title)
            {
                return Err(e);
            }
        }
        let registry_path = cfg.registry_path.clone();
        let records = registry::load_registry(&registry_path)?;
        let reduced = registry::reduce_records(&records);
        let planned = plan_push(&targets, &cfg)?;
        let tty = std::io::stdin().is_terminal();
        let mut state = RunState::new(&reduced);

        if dry_run {
            return dry_run_push(&planned, &reduced, &aws, &canonical_bucket, &mut state);
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
        let mut merged: Vec<MergedItem> = Vec::new();
        let mut skipped: usize = 0;
        let mut skipped_uuids: Vec<String> = Vec::new();
        let mut failed = 0usize;
        let mut first_err: Option<i32> = None;

        for p in &planned {
            let mtime_str = mtime_jst_precise(&p.target.source_path)?;
            // (1) 所在skip / uuid再利用の判定(4.1): file_write成功記録一致
            // (identity_hash+size+mtime)かつfile_upload記録がuuid照合+bucket一致で存在。
            // file_write一致でもfile_upload無し(crash残存)はskipせず(2.5)再PUT分岐へ。
            let w = reduced.file_writes.iter().find(|w| {
                w.identity_hash == p.identity
                    && w.source_size == p.target.size
                    && w.source_mtime == mtime_str
            });
            let upload_recorded = w.is_some_and(|w| {
                reduced.file_uploads.iter().any(|u| {
                    u.uuid == w.uuid
                        && registry::record_bucket_matched(
                            &u.s3_bucket,
                            &aws.bucket,
                            &canonical_bucket,
                        )
                })
            });
            if let (Some(w), true) = (w, upload_recorded) {
                println!("skip  {} (registry match)", p.target.source_name);
                skipped += 1;
                skipped_uuids.push(w.uuid.clone());
                // skip対象もmetadata sync対象に含める(2.3の差分比較により変更無ければ
                // PUTは行われず、metadata object不在(404)時の復旧もrunを通じて機能する)
                state.touch(&w.uuid);
                continue;
            }
            let reuse_uuid = w.filter(|_| !upload_recorded).map(|w| w.uuid.clone());

            // (2) 暗号化(平文sha256入手)
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

            if let Some(uuid) = reuse_uuid {
                // (2.5) crash救済のuuid再利用再PUT分岐: sha照合・新規発行を経ず
                // 同uuidのdefault.ageへ再PUTしfile_uploadを記録(孤児uuid不発生)。
                let current_title = state.run_entity_titles.get(&uuid).cloned();
                let title = resolve_title(current_title.as_deref(), p.target.title.as_deref())?;
                let title_changed = current_title.as_deref() != Some(title.as_str());
                if title_changed {
                    // title変更時のみfile_writeを再追記(新title・新batch_id)
                    append_file_write(
                        &registry_path,
                        &aws.prefix,
                        &batch_id,
                        &batch_date,
                        &uuid,
                        &title,
                        p,
                        &mtime_str,
                        &res,
                        &recorded_at,
                        &age_path,
                    )?;
                    state.set_title(&uuid, &title);
                }
                let key = crate::layout::data_object_key(&aws.prefix, &uuid)?;
                let vid = match backend.put(&aws.bucket, &key, &age_path, &aws.storage_class) {
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
                let upload = file_upload_record(
                    &batch_id,
                    &uuid,
                    &title,
                    &key,
                    &aws,
                    &vid,
                    p,
                    &res,
                    &recorded_at,
                );
                registry::append_record(&registry_path, &upload)?;
                state.run_uploads.push(upload.clone());
                state
                    .run_map
                    .insert(res.source_sha256.clone(), uuid.clone());
                uploaded.push(UploadItem {
                    uuid: uuid.clone(),
                    title: title.clone(),
                    name: p.target.source_name.clone(),
                    category: p.category.clone(),
                        label: p.label.clone(),
                    source_path: p.target.source_path.display().to_string(),
                    s3_key: key.clone(),
                    version_id: vid.clone(),
                    date: batch_date.clone(),
                    version_ts: recorded_at.clone(),
                    source_sha256: res.source_sha256.clone(),
                    age_sha256: res.age_sha256.clone(),
                    source_size: p.target.size,
                    age_size: res.age_size,
                    age_path: age_path.display().to_string(),
                });
                state.touch(&uuid);
                println!(
                    "re-put {}  ->  {} (crash救済の再PUT・version {vid})",
                    p.target.source_name, key
                );
                continue;
            }

            // (3) sha照合(run内map優先 → registry lookup)
            let sha = res.source_sha256.clone();
            let found = state.run_map.get(&sha).cloned().or_else(|| {
                registry::sha_uuid_lookup(&reduced, &sha, &aws.bucket, &canonical_bucket)
            });
            match found {
                Some(uuid) => {
                    // (3a) 合流: uploadしない・file_write追記(新所在)→索引locations追加
                    let current_title = state.run_entity_titles.get(&uuid).cloned();
                    let title = resolve_title(current_title.as_deref(), p.target.title.as_deref())?;
                    append_file_write(
                        &registry_path,
                        &aws.prefix,
                        &batch_id,
                        &batch_date,
                        &uuid,
                        &title,
                        p,
                        &mtime_str,
                        &res,
                        &recorded_at,
                        &age_path,
                    )?;
                    state.run_map.insert(sha.clone(), uuid.clone());
                    state.set_title(&uuid, &title);
                    merged.push(MergedItem { uuid: uuid.clone() });
                    state.touch(&uuid);
                    println!(
                        "merge {}  ->  data/{uuid}/default.age (所在追加・upload省略)",
                        p.target.source_name
                    );
                }
                None => {
                    // (3b) 新規発行。記録順序は「file_write追記 → PUT → file_upload追記」
                    // (4.4: registry記録がS3変更に先行=crash救済)
                    let title = match p.target.title.as_deref() {
                        Some(t) => t.to_string(),
                        None if tty => prompt_title()?,
                        None => {
                            // 非tty+title無しの新規発行は当該対象の失敗(対象エラー3。
                            // keep_goingに従い残対象の処理を継続)
                            eprintln!(
                                "error: 対象エラー: 新規発行にはtitleが必要です(ttyの対話入力または--from-listのtitle欄): {}",
                                p.target.source_path.display()
                            );
                            failed += 1;
                            first_err.get_or_insert(3);
                            if !keep_going {
                                return Ok(3);
                            }
                            continue;
                        }
                    };
                    warn_if_title_duplicate(&reduced, &title);
                    let uuid = crate::layout::generate_uuid();
                    append_file_write(
                        &registry_path,
                        &aws.prefix,
                        &batch_id,
                        &batch_date,
                        &uuid,
                        &title,
                        p,
                        &mtime_str,
                        &res,
                        &recorded_at,
                        &age_path,
                    )?;
                    state.set_title(&uuid, &title);
                    let key = crate::layout::data_object_key(&aws.prefix, &uuid)?;
                    let vid = match backend.put(&aws.bucket, &key, &age_path, &aws.storage_class) {
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
                    let upload = file_upload_record(
                        &batch_id,
                        &uuid,
                        &title,
                        &key,
                        &aws,
                        &vid,
                        p,
                        &res,
                        &recorded_at,
                    );
                    registry::append_record(&registry_path, &upload)?;
                    state.run_uploads.push(upload);
                    state.run_map.insert(sha.clone(), uuid.clone());
                    uploaded.push(UploadItem {
                        uuid: uuid.clone(),
                        title: title.clone(),
                        name: p.target.source_name.clone(),
                        category: p.category.clone(),
                        label: p.label.clone(),
                        source_path: p.target.source_path.display().to_string(),
                        s3_key: key.clone(),
                        version_id: vid.clone(),
                        date: batch_date.clone(),
                        version_ts: recorded_at.clone(),
                        source_sha256: res.source_sha256.clone(),
                        age_sha256: res.age_sha256.clone(),
                        source_size: p.target.size,
                        age_size: res.age_size,
                        age_path: age_path.display().to_string(),
                    });
                    state.touch(&uuid);
                    println!(
                        "ok    {}  ->  {} (version {vid})  title={title}",
                        p.target.source_name, key
                    );
                }
            }
        }

        // backup時ピン: uploaded(新規+再PUT)=手元.ageをpin keyへ直接upload
        // (復元job不要・CopyObject 5GB上限の制約なし)。merged/skipped=最新版から
        // ensure_restored+copy_object(現行skip分pin付与と同一経路・M-6の統合)
        if let Some(pin_name) = pin {
            for item in &uploaded {
                let pin_key = crate::layout::pin_object_key(&aws.prefix, &item.uuid, pin_name)?;
                // write-once違反(既存pin key)は警告+当該fileのみskip(M-2: run全体を中断
                // しない。run末尾の索引再構築・manifest uploadは必ず実行する)
                if backend.head(&aws.bucket, &pin_key)?.is_some() {
                    eprintln!(
                        "WARNING: pin名({pin_name})は既に存在します(write-once)。当該fileのpin付与を飛ばします(pin rm後に再付与してください): {pin_key}"
                    );
                    continue;
                }
                let pin_vid = backend.put(
                    &aws.bucket,
                    &pin_key,
                    std::path::Path::new(&item.age_path),
                    &aws.storage_class,
                )?;
                registry::append_record(
                    &registry_path,
                    &registry::PinWriteRecord {
                        schema: 2,
                        record_type: "pin_write".into(),
                        recorded_at: now_jst(),
                        uuid: item.uuid.clone(),
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
                    },
                )?;
                state.touch(&item.uuid);
                println!("ok    pin {pin_name} -> {pin_key} (version {pin_vid})");
            }

            let mut copy_targets: Vec<String> = merged.iter().map(|m| m.uuid.clone()).collect();
            copy_targets.extend(skipped_uuids.iter().cloned());
            if !copy_targets.is_empty() {
                println!(
                    "WARNING: --pin指定の対象に変更なし(skip)・合流(merge)のfileが{}件あります。最新版へpinを付与します",
                    copy_targets.len()
                );
                for uuid in &copy_targets {
                    // 当該entityの最新版(run内追記分を含むbucket一致file_uploadの最終記録)
                    let Some(version) =
                        latest_upload(&reduced, &state, uuid, &aws, &canonical_bucket)
                    else {
                        continue;
                    };
                    let pin_key = crate::layout::pin_object_key(&aws.prefix, uuid, pin_name)?;
                    if backend.head(&aws.bucket, &pin_key)?.is_some() {
                        eprintln!(
                            "WARNING: pin名({pin_name})は既に存在します(write-once)。skip/合流対象のpin付与を飛ばします: {pin_key}"
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
                                    "pin add --latest --as {pin_name}"
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
                            schema: 2,
                            record_type: "pin_write".into(),
                            recorded_at: now_jst(),
                            uuid: uuid.clone(),
                            pin: pin_name.to_string(),
                            pin_s3_key: pin_key.clone(),
                            source_s3_key: version.s3_key.clone(),
                            source_version_id: version.version_id.clone(),
                            date: version.version_ts.get(..10).unwrap_or("").to_string(),
                            source_sha256: version.source_sha256.clone(),
                            age_sha256: version.age_sha256.clone(),
                            source_size: version.source_size,
                            s3_bucket: aws.bucket.clone(),
                            s3_size: version.age_size as i64,
                            s3_checksum_sha256: version.age_sha256.clone(),
                            uploaded_at: now_jst(),
                        },
                    )?;
                    state.touch(uuid);
                    println!("ok    pin {pin_name} -> {pin_key} (skip/合流対象の最新版)");
                }
            }
        }

        // run manifest(file record schema=3: file upload後に構築。file recordにuuid/title入・
        // identity_hash/folder無しの所在記録と実s3_key+version_idを記録)
        if !uploaded.is_empty() {
            // ヘッダのrecipientsは現在設定の指紋列
            let mut current_fingerprints = Vec::with_capacity(cfg.public_keys.len());
            for k in &cfg.public_keys {
                let pk = crate::fingerprint::parse_ssh_pubkey(k)?;
                current_fingerprints.push(crate::fingerprint::fingerprint(&pk));
            }
            let manifest_body = crate::manifest::build_manifest_v3(
                &batch_id,
                &now_jst(),
                &uploaded
                    .iter()
                    .map(|u| crate::manifest::ManifestV3File {
                        uuid: u.uuid.clone(),
                        title: u.title.clone(),
                        name: u.name.clone(),
                        category: u.category.clone(),
                        label: u.label.clone(),
                        source_path: u.source_path.clone(),
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
            let entries = crate::index::build_entries(&reduced_now, &aws.bucket, &canonical_bucket);
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
            // touched_entities各uuidのmetadata sync(2.3: 変更時のみPUT+旧titleキー掃除)
            for uuid in &state.touched_entities {
                crate::metadata::sync_entity_metadata(
                    backend.as_ref(),
                    &aws,
                    &canonical_bucket,
                    &cfg,
                    &reduced_now,
                    uuid,
                )?;
            }
        }

        // cache dir掃除(keep_artifacts="run"・run成功時のみ。TASK-19 XDG配置の暗号化
        // 一時物dir)。掃除対象は当該run分(<batch_id> dir)のみ(旧経路産物・失敗runの
        // 残りを消さない)
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

/// file_write記録(schema2)の追記組み立て(4.1の各分岐共通)。
#[allow(clippy::too_many_arguments)]
fn append_file_write(
    registry_path: &std::path::Path,
    prefix: &str,
    batch_id: &str,
    batch_date: &str,
    uuid: &str,
    title: &str,
    p: &Planned,
    mtime_str: &str,
    res: &crate::crypto::EncryptResult,
    recorded_at: &str,
    age_path: &std::path::Path,
) -> BackupResult<()> {
    let s3_key = crate::layout::data_object_key(prefix, uuid)?;
    let write = FileWriteRecord {
        schema: 3,
        record_type: "file_write".into(),
        recorded_at: recorded_at.to_string(),
        batch_id: batch_id.to_string(),
        batch_date: batch_date.to_string(),
        uuid: uuid.to_string(),
        title: title.to_string(),
        source_path: p.target.source_path.display().to_string(),
        source_name: p.target.source_name.clone(),
        source_size: p.target.size,
        source_mtime: mtime_str.to_string(),
        source_sha256: res.source_sha256.clone(),
        age_path: age_path.display().to_string(),
        age_size: res.age_size,
        age_sha256: res.age_sha256.clone(),
        recipients: res.recipients.clone(),
        category: p.category.clone(),
        label: p.label.clone(),
        identity_hash: p.identity.clone(),
        s3_key,
    };
    registry::append_record(registry_path, &write)
}

/// file_upload記録(schema2)の組み立て。
#[allow(clippy::too_many_arguments)]
fn file_upload_record(
    batch_id: &str,
    uuid: &str,
    title: &str,
    s3_key: &str,
    aws: &config::AwsConfig,
    version_id: &str,
    p: &Planned,
    res: &crate::crypto::EncryptResult,
    recorded_at: &str,
) -> FileUploadRecord {
    FileUploadRecord {
        schema: 2,
        record_type: "file_upload".into(),
        recorded_at: recorded_at.to_string(),
        batch_id: batch_id.to_string(),
        version_ts: recorded_at.to_string(),
        uuid: uuid.to_string(),
        title: title.to_string(),
        s3_key: s3_key.to_string(),
        s3_bucket: aws.bucket.clone(),
        s3_region: aws.region.clone(),
        version_id: version_id.to_string(),
        s3_size: res.age_size as i64,
        s3_checksum_sha256: res.age_sha256.clone(),
        s3_checksum_type: "FULL_OBJECT".into(),
        source_sha256: res.source_sha256.clone(),
        age_sha256: res.age_sha256.clone(),
        source_size: p.target.size,
        age_size: res.age_size,
        uploaded_at: recorded_at.to_string(),
    }
}

/// 新規発行時のtitle重複警告(4.1・4.3): 既存entityとのtitle重複は許容+警告
/// (titleは識別子でなくラベル・一意性はuuidが担う)。
fn warn_if_title_duplicate(reduced: &Reduced, title: &str) {
    let dup = reduced.file_writes.iter().any(|w| w.title == title);
    if dup {
        println!(
            "WARNING: title({title})は既存entityと重複しています(許容されます。一意化はuuid/所在指定で行ってください)"
        );
    }
}

/// 当該uuidの最新版(run内追記分を含むbucket一致file_uploadの最終記録)。
fn latest_upload(
    reduced: &Reduced,
    state: &RunState,
    uuid: &str,
    aws: &config::AwsConfig,
    canonical_bucket: &str,
) -> Option<FileUploadRecord> {
    reduced
        .file_uploads
        .iter()
        .chain(state.run_uploads.iter())
        .rfind(|u| {
            u.uuid == uuid
                && registry::record_bucket_matched(&u.s3_bucket, &aws.bucket, canonical_bucket)
        })
        .cloned()
}

/// backup push --dry-run(4.1・6.1): 暗号化せず平文sha256のみ計算(ファイル読込)して
/// (1)skip (2)crash残存のre-put (3)sha合流のmerge(合流先uuid表示) (4)新規発行のnew
/// (uuid未確定はkeyをdata/(新規uuid)/default.ageと表示)を出し分ける。
/// dry-run中もrun_map相当のmapを更新する(同一run内同一shaの2件目以降はmerge表示)。
/// age実行・S3 API呼び出し・registry追記を行わない。
fn dry_run_push(
    planned: &[Planned],
    reduced: &Reduced,
    aws: &config::AwsConfig,
    canonical_bucket: &str,
    state: &mut RunState,
) -> BackupResult<i32> {
    let mut counts = (0usize, 0usize, 0usize, 0usize); // skip, re-put, merge, new
    let mut total: u64 = 0;
    for p in planned {
        total += p.target.size;
        let mtime_str = mtime_jst_precise(&p.target.source_path)?;
        let w = reduced.file_writes.iter().find(|w| {
            w.identity_hash == p.identity
                && w.source_size == p.target.size
                && w.source_mtime == mtime_str
        });
        let upload_recorded = w.is_some_and(|w| {
            reduced.file_uploads.iter().any(|u| {
                u.uuid == w.uuid
                    && registry::record_bucket_matched(&u.s3_bucket, &aws.bucket, canonical_bucket)
            })
        });
        if let (Some(_w), true) = (w, upload_recorded) {
            println!("skip  {} (registry match)", p.target.source_name);
            counts.0 += 1;
            continue;
        }
        // 平文sha256のみ計算(暗号化しない。全文readを避けるためstreaming計算)
        let sha = sha256_hex_streaming(&p.target.source_path)?;
        if let Some(uuid) = w.map(|w| w.uuid.clone()) {
            // (2.5) crash残存のre-put予測
            let key = crate::layout::data_object_key(&aws.prefix, &uuid)?;
            println!(
                "re-put {}  ->  {key} (crash救済の再PUT)",
                p.target.source_name
            );
            state.run_map.insert(sha, uuid);
            counts.1 += 1;
            continue;
        }
        match state
            .run_map
            .get(&sha)
            .cloned()
            .or_else(|| registry::sha_uuid_lookup(reduced, &sha, &aws.bucket, canonical_bucket))
        {
            Some(uuid) => {
                // (3a) 合流予測(同一run内同一shaの2件目以降もこの表示)
                println!(
                    "merge {}  ->  data/{uuid}/default.age (所在追加・upload省略)",
                    p.target.source_name
                );
                counts.2 += 1;
            }
            None => {
                // (3b) 新規発行予測(uuid未確定のためkeyは仮表示)
                match p.target.title.as_deref() {
                    Some(t) => println!(
                        "new   {}  ->  data/(新規uuid)/default.age  title={t}",
                        p.target.source_name
                    ),
                    None => {
                        println!(
                            "new   {}  ->  data/(新規uuid)/default.age",
                            p.target.source_name
                        );
                        println!(
                            "WARNING: {} は新規発行予測ですがtitleが未指定です(実行時は対象エラーになります。--from-listのtitle欄または対話入力で指定してください)",
                            p.target.source_name
                        );
                    }
                }
                // dry-run内でもrun_map相当を更新( uuidは未発行のためshaのみ登録できない
                // =新規uuidの代わりに仮idを発行して2件目以降をmerge表示にする)
                let pseudo = format!("(dry-run:{})", &sha[..16.min(sha.len())]);
                state.run_map.insert(sha, pseudo);
                counts.3 += 1;
            }
        }
    }
    println!(
        "plan: {}件  計 {}  skip={} re-put={} merge={} new={} (dry-run: 暗号化・S3 API呼び出しは行いません)",
        planned.len(),
        fmt_size(total),
        counts.0,
        counts.1,
        counts.2,
        counts.3
    );
    Ok(0)
}

/// pushでuploadした対象1件(manifest・pin組み立て用)。
pub(crate) struct UploadItem {
    uuid: String,
    title: String,
    name: String,
    category: String,
    label: String,
    source_path: String,
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

/// 合流分(pin組み立て用。手元.ageが無いため最新版からCopyObject)。
struct MergedItem {
    uuid: String,
}
