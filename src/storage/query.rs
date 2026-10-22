//! list/query/diff(照会系・TASK-18 5.11・6.5/6.6)。entity(uuid/title/locations)単位の
//! 表示。`--format json`はstdout全体が単一JSON(warning等のhuman向け通知はstderr)。
//! 廃止: list files --tree・list categories・--name語彙(→--title)。

use super::get::restore_state;
use super::{
    CtxArgs, Format, fmt_size, open_session, print_json, read_index_for_query, resolve_key,
};
use crate::errors::BackupResult;
use crate::index::{IndexFileEntry, IndexLocation};
use crate::layout;
use std::collections::BTreeMap;

#[derive(clap::Args, Debug)]
pub struct ListArgs {
    #[command(subcommand)]
    pub sub: ListSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum ListSub {
    /// 実体(entity)の目録
    Files {
        #[command(flatten)]
        ctx: CtxArgs,
        /// category絞り込み(いずれかのlocation.category一致)
        #[arg(long)]
        category: Option<String>,
    },
    /// 実体毎の版一覧
    Versions {
        #[command(flatten)]
        ctx: CtxArgs,
        /// entity指定(title完全一致・uuid完全一致も受理)
        #[arg(long)]
        title: Option<String>,
        /// category絞り込み(いずれかのlocation.category一致)
        #[arg(long)]
        category: Option<String>,
    },
    /// ピン留め版一覧
    Pins {
        #[command(flatten)]
        ctx: CtxArgs,
        /// entity指定(title完全一致・uuid完全一致も受理)
        #[arg(long)]
        title: Option<String>,
    },
    /// run(batch)一覧
    Runs {
        #[command(flatten)]
        ctx: CtxArgs,
    },
    /// 復元job状況(全namespace・現行/非現行版ごと)
    Jobs {
        #[command(flatten)]
        ctx: CtxArgs,
    },
}

#[derive(clap::Args, Debug)]
pub struct QueryArgs {
    #[command(flatten)]
    pub ctx: CtxArgs,
    /// 探索の手がかり(title部分一致 / uuid完全・前方一致 / location.source_path部分一致)
    pub term: String,
    #[arg(long)]
    pub category: Option<String>,
    /// 版一覧も表示
    #[arg(long)]
    pub versions: bool,
}

#[derive(clap::Args, Debug)]
pub struct DiffArgs {
    #[command(flatten)]
    pub ctx: CtxArgs,
    /// run(batch)単位の完否監査
    #[arg(long)]
    pub run: Option<String>,
}

pub fn cmd_query(args: &QueryArgs) -> i32 {
    match run_query(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

/// entityのcategory一致判定(--category絞り込み・いずれかのlocation.category一致)。
fn category_matched(e: &IndexFileEntry, want: Option<&str>) -> bool {
    want.is_none_or(|c| e.locations.iter().any(|l| l.category == c))
}

fn location_json(l: &IndexLocation) -> serde_json::Value {
    serde_json::json!({
        "source_path": l.source_path,
        "category": l.category,
        "rel_path": l.rel_path,
        "identity_hash": l.identity_hash,
        "last_backup": l.last_backup,
    })
}

/// entityの要約JSON(--format json・query/list系共通形)。
fn entry_summary_json(e: &IndexFileEntry, with_versions: bool) -> serde_json::Value {
    let latest = e.versions.first();
    let mut obj = serde_json::json!({
        "uuid": e.uuid,
        "title": e.title,
        "locations": e.locations.iter().map(location_json).collect::<Vec<_>>(),
        "versions": e.versions.len(),
        "latest_date": latest.map(|v| v.date.clone()).unwrap_or_default(),
        "latest_size": latest.map(|v| v.source_size).unwrap_or(0),
        "pins": e.pins.iter().map(|p| serde_json::json!({
            "pin": p.pin,
            "date": p.date,
            "source_size": p.source_size,
        })).collect::<Vec<_>>(),
    });
    if with_versions {
        obj["version_list"] = serde_json::Value::Array(
            e.versions
                .iter()
                .map(|v| {
                    serde_json::json!({
                        "date": v.date,
                        "version_ts": v.version_ts,
                        "version_id": v.version_id,
                        "batch_id": v.batch_id,
                        "source_size": v.source_size,
                    })
                })
                .collect(),
        );
    }
    obj
}

fn run_query(args: &QueryArgs) -> BackupResult<i32> {
    let session = open_session(&args.ctx)?;
    let key_path = resolve_key(&args.ctx, session.cfg.as_ref())?;
    let (_header, entries) = read_index_for_query(&session, &key_path)?;
    let term = args.term.as_str();
    let matched: Vec<&IndexFileEntry> = entries
        .iter()
        .filter(|e| {
            // title部分一致 / uuid前方一致 / location.source_path部分一致
            e.title.contains(term)
                || e.uuid.starts_with(term)
                || e.locations.iter().any(|l| l.source_path.contains(term))
        })
        .filter(|e| category_matched(e, args.category.as_deref()))
        .collect();
    if matched.is_empty() {
        match args.ctx.format {
            Format::Json => print_json(&serde_json::json!([])),
            Format::Human => println!(
                "0件 (一致するentityがありません。query <手がかり> でtitle/uuid/所在pathの部分一致を探せます)"
            ),
        }
        return Ok(0);
    }
    if args.ctx.format == Format::Json {
        let value = matched
            .iter()
            .map(|e| entry_summary_json(e, args.versions))
            .collect::<Vec<_>>();
        print_json(&serde_json::json!(value));
        return Ok(0);
    }
    for e in &matched {
        println!(
            "{}  {}  所在{}  {}版  latest {} ({}){}",
            e.title,
            e.uuid,
            e.locations.len(),
            e.versions.len(),
            e.versions.first().map(|v| v.date.as_str()).unwrap_or("-"),
            e.versions
                .first()
                .map(|v| fmt_size(v.source_size))
                .unwrap_or_else(|| "-".into()),
            if e.pins.is_empty() {
                String::new()
            } else {
                format!("  pins={}", e.pins.len())
            },
        );
        for l in &e.locations {
            println!("    所在: {}", l.source_path);
        }
        if args.versions {
            for v in &e.versions {
                println!(
                    "    {} {} {} {}",
                    v.date,
                    v.version_ts,
                    v.version_id,
                    fmt_size(v.source_size)
                );
            }
        }
    }
    // title重複(別uuid同title)候補は--path特定の案内
    let mut titles: Vec<&str> = matched.iter().map(|e| e.title.as_str()).collect();
    titles.sort();
    titles.dedup();
    let duplicated = titles
        .iter()
        .any(|t| matched.iter().filter(|e| e.title == *t).count() > 1);
    if duplicated {
        println!("title重複の候補があります。--path <source_path> で特定してください:");
        for e in &matched {
            let dup = matched.iter().filter(|o| o.title == e.title).count() > 1;
            if dup {
                println!("  {}  (uuid={})", e.title, e.uuid);
                for l in &e.locations {
                    println!("    --path {}", l.source_path);
                }
            }
        }
        println!("WARNING: 同一titleのentityが複数あります(重複は許容・一意化はuuid/所在指定)");
    }
    Ok(0)
}

pub fn cmd_list(args: &ListArgs) -> i32 {
    match run_list(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

fn run_list(args: &ListArgs) -> BackupResult<i32> {
    match &args.sub {
        ListSub::Files { ctx, category } => list_files(ctx, category.as_deref()),
        ListSub::Versions {
            ctx,
            title,
            category,
        } => list_versions(ctx, title.as_deref(), category.as_deref()),
        ListSub::Pins { ctx, title } => list_pins(ctx, title.as_deref()),
        ListSub::Runs { ctx } => list_runs(ctx),
        ListSub::Jobs { ctx } => list_jobs(ctx),
    }
}

/// list files(TASK-18 5.11): entity毎に
/// `title  uuid(先頭8字)  所在n  {v}版  latest {date} ({size})  pins=n`の目録表示。
fn list_files(ctx: &CtxArgs, category_filter: Option<&str>) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let matched: Vec<&IndexFileEntry> = entries
        .iter()
        .filter(|e| category_matched(e, category_filter))
        .collect();
    if matched.is_empty() {
        match ctx.format {
            Format::Json => print_json(&serde_json::json!([])),
            Format::Human => {
                if entries.is_empty() {
                    println!("0件 (索引は空です)");
                } else {
                    println!("0件 (該当categoryのentityがありません)");
                }
            }
        }
        return Ok(0);
    }
    if ctx.format == Format::Json {
        let value: Vec<serde_json::Value> = matched
            .iter()
            .map(|e| entry_summary_json(e, false))
            .collect();
        print_json(&serde_json::json!(value));
        return Ok(0);
    }
    for e in &matched {
        println!(
            "{}  {}  所在{}  {}版  latest {} ({})  pins={}",
            e.title,
            &e.uuid[..8.min(e.uuid.len())],
            e.locations.len(),
            e.versions.len(),
            e.versions.first().map(|v| v.date.as_str()).unwrap_or("-"),
            e.versions
                .first()
                .map(|v| fmt_size(v.source_size))
                .unwrap_or_else(|| "-".into()),
            e.pins.len(),
        );
    }
    // title重複があれば警告行
    let mut titles: Vec<&str> = matched.iter().map(|e| e.title.as_str()).collect();
    titles.sort();
    titles.dedup();
    if titles
        .iter()
        .any(|t| matched.iter().filter(|e| e.title == *t).count() > 1)
    {
        println!("WARNING: 同一titleのentityが複数あります(重複は許容・一意化はuuid/所在指定)");
    }
    Ok(0)
}

fn list_versions(
    ctx: &CtxArgs,
    title: Option<&str>,
    category_filter: Option<&str>,
) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let targets: Vec<&IndexFileEntry> = match title {
        Some(t) => {
            let e = super::get::find_entry(&entries, t, None)?;
            if !category_matched(e, category_filter) {
                match ctx.format {
                    Format::Json => print_json(&serde_json::json!([])),
                    Format::Human => println!("0件 (該当categoryのentityがありません)"),
                }
                return Ok(0);
            }
            vec![e]
        }
        None => entries
            .iter()
            .filter(|e| category_matched(e, category_filter))
            .collect(),
    };
    if targets.is_empty() {
        match ctx.format {
            Format::Json => print_json(&serde_json::json!([])),
            Format::Human => println!("0件 (該当するentityがありません)"),
        }
        return Ok(0);
    }
    if ctx.format == Format::Json {
        let value: Vec<serde_json::Value> = targets
            .iter()
            .map(|e| {
                let current_vid = e.versions.first().map(|f| f.version_id.as_str());
                serde_json::json!({
                    "uuid": e.uuid,
                    "title": e.title,
                    "locations": e.locations.iter().map(location_json).collect::<Vec<_>>(),
                    "versions": e.versions.iter().map(|v| serde_json::json!({
                        "date": v.date,
                        "version_ts": v.version_ts,
                        "version_id": v.version_id,
                        "source_size": v.source_size,
                        "state": if Some(v.version_id.as_str()) == current_vid { "現行" } else { "noncurrent" },
                    })).collect::<Vec<_>>(),
                    "pins": e.pins.iter().map(|p| serde_json::json!({
                        "pin": p.pin,
                        "date": p.date,
                        "source_size": p.source_size,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        print_json(&serde_json::json!(value));
        return Ok(0);
    }
    for e in targets {
        println!("{}  (uuid={})", e.title, e.uuid);
        for l in &e.locations {
            println!(
                "  所在: {}  ({}/{})",
                l.source_path,
                l.category,
                print_rel(&l.rel_path)
            );
        }
        let current_vid = e.versions.first().map(|f| f.version_id.as_str());
        for v in &e.versions {
            println!(
                "  {}  {}  {}  {}  {}",
                v.date,
                v.version_ts,
                v.version_id,
                fmt_size(v.source_size),
                if Some(v.version_id.as_str()) == current_vid {
                    "現行"
                } else {
                    "noncurrent"
                },
            );
        }
        for p in &e.pins {
            println!("  pin {}  {}  {}", p.pin, p.date, fmt_size(p.source_size));
        }
    }
    Ok(0)
}

fn print_rel(rel: &str) -> String {
    if rel.is_empty() {
        "-".to_string()
    } else {
        rel.to_string()
    }
}

fn list_pins(ctx: &CtxArgs, title: Option<&str>) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let targets: Vec<&IndexFileEntry> = match title {
        Some(t) => vec![super::get::find_entry(&entries, t, None)?],
        None => entries.iter().collect(),
    };
    // JSONは対象entity単位の構造(titleとpins[]を並べる。humanの行データと同一対象)
    let file_values: Vec<serde_json::Value> = targets
        .iter()
        .map(|e| {
            serde_json::json!({
                "uuid": e.uuid,
                "title": e.title,
                "pins": e.pins.iter().map(|p| serde_json::json!({
                    "pin": p.pin,
                    "date": p.date,
                    "source_size": p.source_size,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let pin_count: usize = targets.iter().map(|e| e.pins.len()).sum();
    match ctx.format {
        Format::Json => print_json(&serde_json::json!(file_values)),
        Format::Human => {
            if pin_count == 0 {
                println!("0件 (ピン留め版はありません)");
            }
            for e in &targets {
                for p in &e.pins {
                    println!(
                        "{}  {}  {}  {}",
                        e.title,
                        p.pin,
                        p.date,
                        fmt_size(p.source_size),
                    );
                }
            }
        }
    }
    Ok(0)
}

fn list_runs(ctx: &CtxArgs) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    // run(batch_id)ごとの対象entity数とtitle(JSONはhumanの行と同一対象のtitleを含む)
    let mut runs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in &entries {
        for v in &e.versions {
            runs.entry(v.batch_id.as_str())
                .or_default()
                .push(e.title.as_str());
        }
    }
    match ctx.format {
        Format::Json => {
            let value: Vec<serde_json::Value> = runs
                .iter()
                .map(|(run, titles)| {
                    serde_json::json!({
                        "run": run,
                        "files": titles.len(),
                        "titles": titles,
                    })
                })
                .collect();
            print_json(&serde_json::json!(value));
        }
        Format::Human => {
            if runs.is_empty() {
                println!("0件 (索引は空です)");
                return Ok(0);
            }
            for (run, titles) in &runs {
                println!("{run}  files={}", titles.len());
            }
        }
    }
    Ok(0)
}

/// 復元job状況の表示対象1件(現行版・noncurrent版ごと・TASK-11 Phase B)。
struct JobRow {
    key: String,
    version_id: Option<String>,
    /// pin object由来の行(null以外で表示に付与)
    pin: Option<String>,
}

fn list_jobs(ctx: &CtxArgs) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let mut rows: Vec<JobRow> = Vec::new();
    for e in &entries {
        for v in &e.versions {
            if !rows.iter().any(|r| {
                r.key == v.s3_key && r.version_id.as_deref() == Some(v.version_id.as_str())
            }) {
                rows.push(JobRow {
                    key: v.s3_key.clone(),
                    version_id: Some(v.version_id.clone()),
                    pin: None,
                });
            }
        }
        for p in &e.pins {
            if !rows
                .iter()
                .any(|r| r.key == p.s3_key && r.version_id.is_none())
            {
                rows.push(JobRow {
                    key: p.s3_key.clone(),
                    version_id: None,
                    pin: Some(p.pin.clone()),
                });
            }
        }
    }
    rows.sort_by(|a, b| {
        a.key
            .cmp(&b.key)
            .then_with(|| b.version_id.cmp(&a.version_id))
    });
    let mut value_rows: Vec<serde_json::Value> = Vec::new();
    for row in &rows {
        let head = match row.version_id.as_deref().filter(|v| !v.is_empty()) {
            Some(vid) => session
                .backend
                .head_version(&session.aws.bucket, &row.key, vid)?,
            None => session.backend.head(&session.aws.bucket, &row.key)?,
        };
        let state = restore_state(head.as_ref());
        let mut obj = serde_json::json!({
            "key": row.key,
            "version_id": row.version_id,
            "state": state,
        });
        if let Some(pin) = &row.pin {
            obj["pin"] = serde_json::Value::String(pin.clone());
        }
        value_rows.push(obj);
        if ctx.format == Format::Human {
            println!(
                "{}  {}  {}{}",
                row.key,
                row.version_id.as_deref().unwrap_or("-"),
                state,
                row.pin
                    .as_deref()
                    .map(|p| format!("  (pin {p})"))
                    .unwrap_or_default(),
            );
        }
    }
    if ctx.format == Format::Json {
        print_json(&serde_json::json!(value_rows));
        return Ok(0);
    }
    if rows.is_empty() {
        println!("0件 (索引は空です)");
    }
    Ok(0)
}

pub fn cmd_diff(args: &DiffArgs) -> i32 {
    match run_diff(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

/// 突合対象外のkey判定: 予約領域(sys)と{prefix}data/配下のmetadata_接頭辞file名
/// (いずれも索引管理外・TASK-18 5.11)。
fn diff_out_of_scope(prefix: &str, key: &str) -> bool {
    let reserve_prefix = format!("{prefix}{}/", layout::RESERVE_SEGMENT);
    if key.starts_with(reserve_prefix.as_str()) {
        return true;
    }
    let data_prefix = layout::data_prefix(prefix);
    if key.starts_with(data_prefix.as_str()) {
        let basename = key.rsplit('/').next().unwrap_or("");
        return basename.starts_with("metadata_");
    }
    false
}

fn run_diff(args: &DiffArgs) -> BackupResult<i32> {
    let session = open_session(&args.ctx)?;
    let key_path = resolve_key(&args.ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let versions = session
        .backend
        .list_versions(&session.aws.bucket, &session.aws.prefix)?;
    let present: std::collections::BTreeSet<&str> = versions
        .iter()
        .map(|v| v.key.as_str())
        .filter(|k| !diff_out_of_scope(&session.aws.prefix, k))
        .collect();

    if let Some(run) = args.run.as_deref() {
        // 三方突合(索引・manifest・S3実態)でrunの完否を監査(UC-8)
        let mut run_keys: Vec<String> = Vec::new();
        for e in &entries {
            for v in &e.versions {
                if v.batch_id == run && !run_keys.contains(&v.s3_key) {
                    run_keys.push(v.s3_key.clone());
                }
            }
        }
        let mut missing: Vec<String> = Vec::new();
        for k in &run_keys {
            if !present.contains(k.as_str()) {
                missing.push(k.clone());
                if args.ctx.format == Format::Human {
                    println!("missing: {k} (run={run})");
                }
            }
        }
        let manifest_key = layout::manifest_object_key(&session.aws.prefix, run)?;
        let manifest_ok = session
            .backend
            .head(&session.aws.bucket, &manifest_key)?
            .is_some();
        if args.ctx.format == Format::Human {
            if !manifest_ok {
                println!("missing: manifest {manifest_key} (run={run})");
            } else {
                println!("manifest: {manifest_key} OK");
            }
            println!(
                "run {run}: files={} missing={} manifest={}",
                run_keys.len(),
                missing.len(),
                if manifest_ok { "OK" } else { "missing" }
            );
        } else {
            print_json(&serde_json::json!({
                "run": run,
                "files": run_keys.len(),
                "missing": missing,
                "manifest": if manifest_ok { "OK" } else { "missing" },
            }));
        }
        return Ok(0);
    }

    let mut index_keys = std::collections::BTreeSet::new();
    for e in &entries {
        for v in &e.versions {
            index_keys.insert(v.s3_key.clone());
        }
        for p in &e.pins {
            index_keys.insert(p.s3_key.clone());
        }
    }
    let mut missing: Vec<String> = Vec::new();
    for k in &index_keys {
        if !present.contains(k.as_str()) {
            missing.push(k.clone());
            if args.ctx.format == Format::Human {
                println!("missing: {k}");
            }
        }
    }
    let mut unexpected: Vec<&str> = Vec::new();
    for k in &present {
        if !index_keys.contains(*k) {
            unexpected.push(k);
            if args.ctx.format == Format::Human {
                println!("unexpected: {k}");
            }
        }
    }
    match args.ctx.format {
        Format::Json => print_json(&serde_json::json!({
            // 索引側の対象entity一覧(humanの突合対象と同一データ)
            "entries": entries.len(),
            "files": entries
                .iter()
                .map(|e| serde_json::json!({
                    "uuid": e.uuid,
                    "title": e.title,
                }))
                .collect::<Vec<_>>(),
            "missing": missing,
            "unexpected": unexpected,
        })),
        Format::Human => println!(
            "diff: entries={} missing={} unexpected={}",
            entries.len(),
            missing.len(),
            unexpected.len(),
        ),
    }
    Ok(0)
}
