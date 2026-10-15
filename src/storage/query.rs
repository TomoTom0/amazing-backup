//! list/query/diff(照会系・設計3.1)。`--format json`はstdout全体が単一JSON
//! (TASK-11 Phase B。humanと同一の対象データ・警告はstderr)。

use super::get::restore_state;
use super::{
    CtxArgs, Format, fmt_size, open_session, print_json, read_index_for_query, resolve_key,
};
use crate::errors::BackupResult;
use crate::index::IndexFileEntry;
use crate::layout;
use std::collections::BTreeMap;

#[derive(clap::Args, Debug)]
pub struct ListArgs {
    #[command(subcommand)]
    pub sub: ListSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum ListSub {
    /// 全ファイルの目録
    Files {
        #[command(flatten)]
        ctx: CtxArgs,
        /// 階層表示
        #[arg(long)]
        tree: bool,
        /// category絞り込み
        #[arg(long)]
        category: Option<String>,
    },
    /// ファイル毎の版一覧
    Versions {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        name: Option<String>,
        /// category絞り込み
        #[arg(long)]
        category: Option<String>,
    },
    /// ピン留め版一覧
    Pins {
        #[command(flatten)]
        ctx: CtxArgs,
        #[arg(long)]
        name: Option<String>,
    },
    /// run(batch)一覧
    Runs {
        #[command(flatten)]
        ctx: CtxArgs,
    },
    /// 管理区分一覧
    Categories {
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
    /// 探索の手がかり(名前/path部分一致)
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

fn run_query(args: &QueryArgs) -> BackupResult<i32> {
    let session = open_session(&args.ctx)?;
    let key_path = resolve_key(&args.ctx, session.cfg.as_ref())?;
    let (_header, entries) = read_index_for_query(&session, &key_path)?;
    let term = args.term.as_str();
    let matched: Vec<&IndexFileEntry> = entries
        .iter()
        .filter(|e| {
            e.name.contains(term) || e.rel_path.contains(term) || e.source_path.contains(term)
        })
        .filter(|e| args.category.as_deref().is_none_or(|c| e.category == c))
        .collect();
    if matched.is_empty() {
        match args.ctx.format {
            Format::Json => print_json(&serde_json::json!([])),
            Format::Human => println!(
                "0件 (一致するfileがありません。query <手がかり> で名前/path部分一致を探せます)"
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
        let latest = e.versions.first();
        println!(
            "{}  {}/{}  {}版  latest {} ({}){}",
            e.name,
            e.category,
            if e.rel_path.is_empty() {
                "-".to_string()
            } else {
                e.rel_path.clone()
            },
            e.versions.len(),
            latest.map(|v| v.date.as_str()).unwrap_or("-"),
            latest
                .map(|v| fmt_size(v.source_size))
                .unwrap_or_else(|| "-".into()),
            if e.pins.is_empty() {
                String::new()
            } else {
                format!("  pins={}", e.pins.len())
            },
        );
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
    // 同名候補が複数ある場合はget --path特定の案内
    let mut names: Vec<&str> = matched.iter().map(|e| e.name.as_str()).collect();
    names.sort();
    names.dedup();
    let duplicated = names
        .iter()
        .any(|n| matched.iter().filter(|e| e.name == *n).count() > 1);
    if duplicated {
        println!("同名の候補が複数あります。候補(rel_path / folder)で特定してください:");
        for e in &matched {
            let dup = matched.iter().filter(|o| o.name == e.name).count() > 1;
            if dup {
                println!(
                    "  {}  --path {}  (folder: {})",
                    e.name,
                    if e.rel_path.is_empty() {
                        "\"\""
                    } else {
                        &e.rel_path
                    },
                    if e.folder.is_empty() { "-" } else { &e.folder }
                );
            }
        }
        println!("get <name> --path <rel_path> で特定できます");
    }
    Ok(0)
}

/// file entryの要約JSON(--format json・query/list系共通形)。
fn entry_summary_json(e: &IndexFileEntry, with_versions: bool) -> serde_json::Value {
    let latest = e.versions.first();
    let mut obj = serde_json::json!({
        "name": e.name,
        "category": e.category,
        "rel_path": e.rel_path,
        "folder": e.folder,
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
        ListSub::Files {
            ctx,
            tree,
            category,
        } => list_files(ctx, *tree, category.as_deref()),
        ListSub::Versions {
            ctx,
            name,
            category,
        } => list_versions(ctx, name.as_deref(), category.as_deref()),
        ListSub::Pins { ctx, name } => list_pins(ctx, name.as_deref()),
        ListSub::Runs { ctx } => list_runs(ctx),
        ListSub::Categories { ctx } => list_categories(ctx),
        ListSub::Jobs { ctx } => list_jobs(ctx),
    }
}

fn list_files(ctx: &CtxArgs, tree: bool, category_filter: Option<&str>) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let mut by_category: BTreeMap<&str, Vec<&IndexFileEntry>> = BTreeMap::new();
    for e in &entries {
        if category_filter.is_some_and(|c| e.category != c) {
            continue;
        }
        by_category.entry(e.category.as_str()).or_default().push(e);
    }
    if by_category.is_empty() {
        match ctx.format {
            Format::Json => print_json(&serde_json::json!([])),
            Format::Human => {
                if entries.is_empty() {
                    println!("0件 (索引は空です)");
                } else {
                    println!("0件 (該当categoryのfileがありません)");
                }
            }
        }
        return Ok(0);
    }
    if ctx.format == Format::Json {
        let value: Vec<serde_json::Value> = by_category
            .iter()
            .map(|(category, list)| {
                let mut obj = category_summary_json(category, list);
                if tree {
                    obj["entries"] = serde_json::Value::Array(
                        list.iter()
                            .map(|e| serde_json::json!({
                                "path": if e.rel_path.is_empty() {
                                    format!("{category}/{}", e.name)
                                } else {
                                    format!("{category}/{}/{}", e.rel_path, e.name)
                                },
                                "versions": e.versions.len(),
                                "latest_date": e.versions.first().map(|v| v.date.clone()).unwrap_or_default(),
                                "pins": e.pins.len(),
                            }))
                            .collect(),
                    );
                }
                obj
            })
            .collect();
        print_json(&serde_json::json!(value));
        return Ok(0);
    }
    for (category, list) in &by_category {
        println!(
            "{category}  files={}  noncurrent={}  最新日={}  last run={}  latest計size={}  pins={}",
            list.len(),
            list.iter()
                .map(|e| e.versions.len().saturating_sub(1))
                .sum::<usize>(),
            list.iter()
                .filter_map(|e| e.versions.first())
                .map(|v| v.date.as_str())
                .max()
                .unwrap_or("-"),
            list.iter()
                .flat_map(|e| e.versions.iter())
                .map(|v| v.batch_id.as_str())
                .max()
                .unwrap_or("-"),
            fmt_size(
                list.iter()
                    .filter_map(|e| e.versions.first())
                    .map(|v| v.source_size)
                    .sum::<u64>()
            ),
            list.iter().map(|e| e.pins.len()).sum::<usize>(),
        );
    }
    if tree {
        for (category, list) in &by_category {
            for e in list {
                println!(
                    "{}/{}  {}版  latest {}{}",
                    category,
                    if e.rel_path.is_empty() {
                        e.name.clone()
                    } else {
                        format!("{}/{}", e.rel_path, e.name)
                    },
                    e.versions.len(),
                    e.versions.first().map(|v| v.date.as_str()).unwrap_or("-"),
                    if e.pins.is_empty() {
                        String::new()
                    } else {
                        format!("  pins={}", e.pins.len())
                    },
                );
            }
        }
    }
    Ok(0)
}

/// category毎の目録要約JSON(humanのcategory行と同一データ)。
fn category_summary_json(category: &str, list: &[&IndexFileEntry]) -> serde_json::Value {
    serde_json::json!({
        "category": category,
        "files": list.len(),
        "noncurrent": list.iter().map(|e| e.versions.len().saturating_sub(1)).sum::<usize>(),
        "latest_date": list.iter()
            .filter_map(|e| e.versions.first())
            .map(|v| v.date.clone())
            .max()
            .unwrap_or_default(),
        "last_run": list.iter()
            .flat_map(|e| e.versions.iter())
            .map(|v| v.batch_id.clone())
            .max()
            .unwrap_or_default(),
        "latest_size": list.iter()
            .filter_map(|e| e.versions.first())
            .map(|v| v.source_size)
            .sum::<u64>(),
        "pins": list.iter().map(|e| e.pins.len()).sum::<usize>(),
    })
}

fn list_versions(
    ctx: &CtxArgs,
    name: Option<&str>,
    category_filter: Option<&str>,
) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let targets: Vec<&IndexFileEntry> = match name {
        Some(n) => {
            let e = super::get::find_entry(&entries, n, None)?;
            if category_filter.is_some_and(|c| e.category != c) {
                match ctx.format {
                    Format::Json => print_json(&serde_json::json!([])),
                    Format::Human => println!("0件 (該当categoryのfileがありません)"),
                }
                return Ok(0);
            }
            vec![e]
        }
        None => entries
            .iter()
            .filter(|e| category_filter.is_none_or(|c| e.category == c))
            .collect(),
    };
    if targets.is_empty() {
        match ctx.format {
            Format::Json => print_json(&serde_json::json!([])),
            Format::Human => println!("0件 (該当するfileがありません)"),
        }
        return Ok(0);
    }
    if ctx.format == Format::Json {
        let value: Vec<serde_json::Value> = targets
            .iter()
            .map(|e| {
                let current_vid = e.versions.first().map(|f| f.version_id.as_str());
                serde_json::json!({
                    "name": e.name,
                    "category": e.category,
                    "rel_path": e.rel_path,
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
        println!("{}  ({}/{})", e.name, e.category, print_rel(&e.rel_path));
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

fn list_pins(ctx: &CtxArgs, name: Option<&str>) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let targets: Vec<&IndexFileEntry> = match name {
        Some(n) => vec![super::get::find_entry(&entries, n, None)?],
        None => entries.iter().collect(),
    };
    // JSONは対象file単位の構造(file名とpins[]を並べる。humanの行データと同一対象)
    let file_values: Vec<serde_json::Value> = targets
        .iter()
        .map(|e| {
            serde_json::json!({
                "name": e.name,
                "category": e.category,
                "rel_path": e.rel_path,
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
                        e.name,
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
    // run(batch_id)ごとの対象file数と名前(JSONはhumanの行と同一対象のfile名を含む)
    let mut runs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in &entries {
        for v in &e.versions {
            runs.entry(v.batch_id.as_str())
                .or_default()
                .push(e.name.as_str());
        }
    }
    match ctx.format {
        Format::Json => {
            let value: Vec<serde_json::Value> = runs
                .iter()
                .map(|(run, names)| {
                    serde_json::json!({
                        "run": run,
                        "files": names.len(),
                        "names": names,
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
            for (run, names) in &runs {
                println!("{run}  files={}", names.len());
            }
        }
    }
    Ok(0)
}

fn list_categories(ctx: &CtxArgs) -> BackupResult<i32> {
    let session = open_session(ctx)?;
    let key_path = resolve_key(ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let mut cats: BTreeMap<String, usize> = BTreeMap::new();
    for e in &entries {
        *cats.entry(e.category.clone()).or_insert(0) += 1;
    }
    // 設定のcategories(索引に実体が無くても表示。keepは設定値・災害時設定なしは"-")
    if let Some(cfg) = session.cfg.as_ref() {
        for name in cfg.categories.keys() {
            cats.entry(name.clone()).or_insert(0);
        }
    }
    match ctx.format {
        Format::Json => {
            let value: Vec<serde_json::Value> = cats
                .iter()
                .map(|(c, n)| {
                    serde_json::json!({
                        "category": c,
                        "files": n,
                        "keep": session
                            .cfg
                            .as_ref()
                            .and_then(|cfg| cfg.categories.get(c))
                            .map(|cat| cat.keep)
                            .map(serde_json::Value::from)
                            .unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect();
            print_json(&serde_json::json!(value));
        }
        Format::Human => {
            if cats.is_empty() {
                println!("0件 (索引は空です)");
                return Ok(0);
            }
            for (c, n) in &cats {
                let keep = session
                    .cfg
                    .as_ref()
                    .and_then(|cfg| cfg.categories.get(c))
                    .map(|cat| cat.keep.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!("{c}  files={n}  keep={keep}");
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

fn run_diff(args: &DiffArgs) -> BackupResult<i32> {
    let session = open_session(&args.ctx)?;
    let key_path = resolve_key(&args.ctx, session.cfg.as_ref())?;
    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let versions = session
        .backend
        .list_versions(&session.aws.bucket, &session.aws.prefix)?;
    // 予約領域(sys)は突合対象外(索引・manifestは索引の管理外)
    let reserve_prefix = format!("{}{}/", session.aws.prefix, layout::RESERVE_SEGMENT);
    let is_reserved = |k: &str| k.starts_with(reserve_prefix.as_str());
    let present: std::collections::BTreeSet<&str> = versions
        .iter()
        .map(|v| v.key.as_str())
        .filter(|k| !is_reserved(k))
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
            // 索引側の対象file一覧(humanの突合対象と同一データ)
            "entries": entries.len(),
            "files": entries
                .iter()
                .map(|e| serde_json::json!({
                    "name": e.name,
                    "category": e.category,
                    "rel_path": e.rel_path,
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
