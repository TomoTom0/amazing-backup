//! get <name>(設計3.1・TASK-10/11)。取得(単一file中心。未復元なら復元job発行を内包)。
//! 事前検査(--key・ageバイナリ)は索引GETより前(restore.download-preflight-errors)。
//! 平文衝突保護(restore.download-plain-conflict): 出力先に既存平文がありsha256が
//! 取得対象のsource_sha256と一致→skip(冪等)・不一致→上書きせず対象エラー3
//! (--yes指定時のみ上書き)。sha一致skipはローカルregistryからも判定でき、
//! その場合は索引GETを含むS3 API呼び出しを行わない。

use super::{
    CtxArgs, Format, open_session, print_json, read_index_for_query, resolve_key, sha256_hex,
    sha256_of_plain,
};
use crate::config::{AwsConfig, PipelineConfig};
use crate::errors::{BackupError, BackupResult};
use crate::index::{IndexFileEntry, IndexPin, IndexVersion};
use crate::registry;
use crate::s3::{HeadInfo, S3Backend};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Debug)]
pub struct GetArgs {
    #[command(flatten)]
    pub ctx: CtxArgs,
    /// 対象ファイル名(basename)
    pub name: String,
    /// 同名候補のpath特定
    #[arg(long)]
    pub path: Option<String>,
    /// 版の日付(YYYY-MM-DD)
    #[arg(long, value_parser = super::parse_date)]
    pub version: Option<String>,
    /// 版のversion_id指定
    #[arg(long)]
    pub version_id: Option<String>,
    /// ピン留め版の取得
    #[arg(long = "pin")]
    pub pin: Option<String>,
    /// 取得先dir(既定 data/restore)
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[arg(long)]
    pub tier: Option<String>,
    #[arg(long)]
    pub days: Option<u32>,
    /// sha不一致の既存平文への上書きを許可(平文衝突保護の免除)
    #[arg(long)]
    pub yes: bool,
}

pub fn cmd_get(args: &GetArgs) -> i32 {
    match run_get(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            e.exit_code()
        }
    }
}

/// tier/daysの検証(get/pull共通・旧restore仕様と同一)。
pub(crate) fn resolve_tier_days(
    tier: Option<&str>,
    days: Option<u32>,
) -> Result<(String, u32), BackupError> {
    let tier = tier.unwrap_or("Standard").to_string();
    if !["Standard", "Bulk"].contains(&tier.as_str()) {
        return Err(BackupError::Usage(format!(
            "--tierはStandard/Bulkのいずれかです: {tier}"
        )));
    }
    let days = days.unwrap_or(7);
    if !(1..=365).contains(&days) {
        return Err(BackupError::Config(format!(
            "--daysは1〜365で指定してください: {days}"
        )));
    }
    Ok((tier, days))
}

/// 復元環境の事前検査(索引GETより前・restore.download-preflight-errors):
/// 秘密鍵file不在・ageバイナリ不在は設定・環境エラー2(対象処理に紛れない)。
pub(crate) fn preflight_decrypt_env(key_path: &Path) -> BackupResult<()> {
    if !key_path.is_file() {
        return Err(BackupError::Config(format!(
            "復号用秘密鍵ファイルが存在しない ({})",
            key_path.display()
        )));
    }
    crate::crypto::find_age()?;
    Ok(())
}

fn run_get(args: &GetArgs) -> BackupResult<i32> {
    let (tier, days) = resolve_tier_days(args.tier.as_deref(), args.days)?;
    let session = open_session(&args.ctx)?;
    let key_path = resolve_key(&args.ctx, session.cfg.as_ref())?;
    preflight_decrypt_env(&key_path)?;
    let out_dir = args
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from("data/restore"));

    // 平文衝突の事前検査(ローカルregistryで判定できる場合はS3 API呼び出しなしで
    // skip・衝突を確定させる。registryが無い災害時等は通常経路へ続行)
    if let Some(code) = plain_conflict_precheck(
        session.cfg.as_ref(),
        &args.name,
        args.path.as_deref(),
        args.version.as_deref(),
        args.version_id.as_deref(),
        args.pin.as_deref(),
        &out_dir,
        args.yes,
        args.ctx.format,
    )? {
        return Ok(code);
    }

    let (_h, entries) = read_index_for_query(&session, &key_path)?;
    let target = resolve_fetch_target(
        &entries,
        &args.name,
        args.path.as_deref(),
        args.version.as_deref(),
        args.version_id.as_deref(),
        args.pin.as_deref(),
    )?;
    // 平文衝突保護(索引正本のsource_sha256と比較・上書きは--yesのみ)
    if let Some(code) = plain_conflict_check(&target, &out_dir, args.yes, args.ctx.format)? {
        return Ok(code);
    }
    match ensure_restored(
        session.backend.as_ref(),
        &session.aws,
        &target.s3_key,
        target.version_id.as_deref(),
        days,
        &tier,
    )? {
        RestoreCheck::Issued | RestoreCheck::Ongoing => {
            // 案内は人間向け通知(--format jsonではstderrへ出しstdoutのJSONを壊さない)
            match args.ctx.format {
                Format::Json => {
                    eprintln!(
                        "{}",
                        restore_guidance(&format!("backup-remote get {}", args.name))
                    );
                    print_json(&serde_json::json!({
                        "name": target.dest_name,
                        "restore": "pending",
                    }));
                }
                Format::Human => println!(
                    "{}",
                    restore_guidance(&format!("backup-remote get {}", args.name))
                ),
            }
            return Ok(0);
        }
        RestoreCheck::Ready => {}
    }
    let dest =
        target.fetch_verify_decrypt(session.backend.as_ref(), &session.aws, &out_dir, &key_path)?;
    // 表示は<out>基準の相対path(出力先dirの違いが表示へ漏れないようにする)
    let shown = dest
        .strip_prefix(&out_dir)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| dest.display().to_string());
    match args.ctx.format {
        Format::Json => print_json(&serde_json::json!({
            "name": target.dest_name,
            "dest": shown,
        })),
        Format::Human => println!("ok: {shown}"),
    }
    Ok(0)
}

/// 復元要否の判定と発行(get/pull/pin add共通)。
pub(crate) enum RestoreCheck {
    /// 取得可能(非archive・または復元済み)
    Ready,
    /// 復元jobをこの実行で発行した(案内してexit 0)
    Issued,
    /// 復元jobが進行中(案内してexit 0)
    Ongoing,
}

pub(crate) fn ensure_restored(
    backend: &dyn S3Backend,
    aws: &AwsConfig,
    key: &str,
    version_id: Option<&str>,
    days: u32,
    tier: &str,
) -> BackupResult<RestoreCheck> {
    let head: Option<HeadInfo> = match version_id.filter(|v| !v.is_empty()) {
        Some(vid) => backend.head_version(&aws.bucket, key, vid)?,
        None => backend.head(&aws.bucket, key)?,
    };
    let Some(h) = head else {
        return Err(BackupError::Transfer(format!(
            "対象keyがS3に存在しない(key={key})"
        )));
    };
    let archived = matches!(
        h.storage_class.as_deref(),
        Some("DEEP_ARCHIVE") | Some("GLACIER") | Some("GLACIER_IR")
    );
    if !archived {
        return Ok(RestoreCheck::Ready);
    }
    match h.restore.as_deref() {
        None => {
            match version_id.filter(|v| !v.is_empty()) {
                Some(vid) => backend.restore_version(&aws.bucket, key, vid, days, tier)?,
                None => backend.restore(&aws.bucket, key, days, tier)?,
            };
            Ok(RestoreCheck::Issued)
        }
        Some(r) if r.contains(r#"ongoing-request="true""#) => Ok(RestoreCheck::Ongoing),
        Some(_) => Ok(RestoreCheck::Ready),
    }
}

pub(crate) fn restore_guidance(what: &str) -> String {
    format!("復元jobを発行しました(進行中・約12〜48h)。完了後に再実行してください: {what}")
}

/// 復元job状態の文言(headのRestore headerから)。
pub(crate) fn restore_state(head: Option<&HeadInfo>) -> &'static str {
    match head.and_then(|h| h.restore.as_deref()) {
        None => "not-requested",
        Some(r) if r.contains(r#"ongoing-request="true""#) => "ongoing",
        Some(_) => "restored",
    }
}

/// 取得対象1件(get/pull共通)。
pub(crate) struct FetchTarget {
    pub(crate) s3_key: String,
    pub(crate) version_id: Option<String>,
    pub(crate) age_sha256: String,
    pub(crate) source_sha256: String,
    pub(crate) dest_name: String,
    pub(crate) rel_path: String,
}

impl FetchTarget {
    pub(crate) fn from_version(entry: &IndexFileEntry, v: &IndexVersion) -> FetchTarget {
        FetchTarget {
            s3_key: v.s3_key.clone(),
            version_id: Some(v.version_id.clone()),
            age_sha256: v.age_sha256.clone(),
            source_sha256: v.source_sha256.clone(),
            dest_name: entry.name.clone(),
            rel_path: entry.rel_path.clone(),
        }
    }

    pub(crate) fn from_pin(entry: &IndexFileEntry, p: &IndexPin) -> FetchTarget {
        FetchTarget {
            s3_key: p.s3_key.clone(),
            version_id: None,
            age_sha256: p.age_sha256.clone(),
            source_sha256: p.source_sha256.clone(),
            dest_name: entry.name.clone(),
            rel_path: entry.rel_path.clone(),
        }
    }

    /// 配置先path(<out>/<rel_path>/<name>。rel_pathなしfileは<out>/<name>)。
    pub(crate) fn dest_path(&self, out_dir: &Path) -> PathBuf {
        let mut dest = out_dir.to_path_buf();
        if !self.rel_path.is_empty() {
            for seg in self.rel_path.split('/') {
                dest.push(seg);
            }
        }
        dest.push(&self.dest_name);
        dest
    }

    /// 取得→sha検証(索引正本)→復号→<out>/<rel_path>/<name>へ配置。
    pub(crate) fn fetch_verify_decrypt(
        &self,
        backend: &dyn S3Backend,
        aws: &AwsConfig,
        out_dir: &Path,
        key_path: &Path,
    ) -> BackupResult<PathBuf> {
        let tmp_age = std::env::temp_dir().join(format!(
            "backup-remote-get-{}-{}.age",
            std::process::id(),
            sha256_hex(self.s3_key.as_bytes())
                .chars()
                .take(16)
                .collect::<String>()
        ));
        let result = (|| -> BackupResult<PathBuf> {
            match self.version_id.as_deref().filter(|v| !v.is_empty()) {
                Some(vid) => {
                    backend.get_version_to_file(&aws.bucket, &self.s3_key, vid, &tmp_age)?
                }
                None => backend.get_to_file(&aws.bucket, &self.s3_key, &tmp_age)?,
            }
            let age_sha = super::sha256_hex_file(&tmp_age)?;
            if age_sha != self.age_sha256 {
                return Err(BackupError::Process(format!(
                    "検証エラー: 取得objectのsha256が索引記録と不一致(key={})",
                    self.s3_key
                )));
            }
            let plain = crate::crypto::decrypt_bytes(&tmp_age, key_path)?;
            let source_sha = sha256_hex(&plain);
            if source_sha != self.source_sha256 {
                return Err(BackupError::Process(format!(
                    "検証エラー: 復号平文のsha256が索引記録と不一致(key={})",
                    self.s3_key
                )));
            }
            let dest = self.dest_path(out_dir);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| BackupError::Process(format!("出力先dirの作成に失敗: {e}")))?;
            }
            std::fs::write(&dest, &plain).map_err(|e| {
                BackupError::Process(format!("復元先への書込に失敗({}): {e}", dest.display()))
            })?;
            Ok(dest)
        })();
        let _ = std::fs::remove_file(&tmp_age);
        result
    }
}

/// 索引からnameでentryを引き、候補が複数なら--path特定を求める(対象エラー3)。
pub(crate) fn find_entry<'a>(
    entries: &'a [IndexFileEntry],
    name: &str,
    path: Option<&str>,
) -> Result<&'a IndexFileEntry, BackupError> {
    let mut matched = entries.iter().filter(|e| e.name == name);
    if let Some(p) = path {
        return matched.find(|e| e.rel_path == p).ok_or_else(|| {
            BackupError::Target(format!(
                "name={name} path={p} の組は索引にありません(--pathの確認またはqueryで候補を表示してください)"
            ))
        });
    }
    let first = matched.next().ok_or_else(|| {
        BackupError::Target(format!(
            "索引にname={name} がありません(queryで確認してください)"
        ))
    })?;
    if matched.next().is_some() {
        return Err(BackupError::Target(format!(
            "name={name} の候補が複数あります。--path で特定してください(query {name} で候補一覧)"
        )));
    }
    Ok(first)
}

/// 版選択(--version-id > --version日付filter > 既定=最新)。同日複数hitは対象エラー3。
pub(crate) fn select_version<'a>(
    entry: &'a IndexFileEntry,
    version: Option<&str>,
    version_id: Option<&str>,
) -> Result<&'a IndexVersion, BackupError> {
    if let Some(vid) = version_id {
        return entry
            .versions
            .iter()
            .find(|v| v.version_id == vid)
            .ok_or_else(|| {
                BackupError::Target(format!(
                    "version_id={vid} の版は索引にありません(name={})",
                    entry.name
                ))
            });
    }
    if let Some(date) = version {
        let hits: Vec<&IndexVersion> = entry.versions.iter().filter(|v| v.date == date).collect();
        return match hits.as_slice() {
            [one] => Ok(one),
            [] => Err(BackupError::Target(format!(
                "date={date} の版はありません(name={}。候補: {})",
                entry.name,
                entry
                    .versions
                    .iter()
                    .map(|v| v.date.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            many => Err(BackupError::Target(format!(
                "date={date} に複数版があります。--version-id で特定してください(候補: {})",
                many.iter()
                    .map(|v| format!("{} {}", v.version_ts, v.version_id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        };
    }
    entry
        .versions
        .first()
        .ok_or_else(|| BackupError::Target(format!("索引に版がありません(name={})", entry.name)))
}

/// get引数から取得対象を解決する(pin指定 > 版選択規則)。
pub(crate) fn resolve_fetch_target(
    entries: &[IndexFileEntry],
    name: &str,
    path: Option<&str>,
    version: Option<&str>,
    version_id: Option<&str>,
    pin: Option<&str>,
) -> Result<FetchTarget, BackupError> {
    let entry = find_entry(entries, name, path)?;
    if let Some(pin_name) = pin {
        let p = entry
            .pins
            .iter()
            .find(|p| p.pin == pin_name)
            .ok_or_else(|| {
                BackupError::Target(format!(
                    "pin名({pin_name})はname={} の索引にありません(list pinsで確認してください)",
                    entry.name
                ))
            })?;
        return Ok(FetchTarget::from_pin(entry, p));
    }
    let v = select_version(entry, version, version_id)?;
    Ok(FetchTarget::from_version(entry, v))
}

/// 平文衝突保護(索引・ローカルregistry問わず対象のsource_sha256と比較)。
/// 戻り値Some(code)は処理確定(skip=0)・Noneは取得続行(上書き許可または既存なし)。
pub(crate) fn plain_conflict_check(
    target: &FetchTarget,
    out_dir: &Path,
    yes: bool,
    format: Format,
) -> BackupResult<Option<i32>> {
    let dest = target.dest_path(out_dir);
    if !dest.is_file() {
        return Ok(None);
    }
    let sha = sha256_of_plain(&dest)?;
    if sha == target.source_sha256 {
        match format {
            Format::Json => print_json(&serde_json::json!({
                "name": target.dest_name,
                "dest": dest.display().to_string(),
                "skip": true,
            })),
            Format::Human => println!("skip {} (既存平文のsha256一致・冪等)", dest.display()),
        }
        return Ok(Some(0));
    }
    if !yes {
        return Err(BackupError::Target(format!(
            "出力先にsha256不一致の既存平文があるため上書きしません(手動リネームまたは移動後に再実行。--yesで上書き): {} ({})",
            dest.display(),
            target.dest_name
        )));
    }
    Ok(None)
}

/// 平文衝突の事前検査(索引GET前)。ローカルregistryから対象を解決でき
/// 出力先に既存平文がある場合のみS3 API呼び出しなしで確定させる。
/// 解決不能(登録なし・registry不在・版特定失敗)・既存平文なしはNone(通常経路)。
fn plain_conflict_precheck(
    cfg: Option<&PipelineConfig>,
    name: &str,
    path: Option<&str>,
    version: Option<&str>,
    version_id: Option<&str>,
    pin: Option<&str>,
    out_dir: &Path,
    yes: bool,
    format: Format,
) -> BackupResult<Option<i32>> {
    let Some(cfg) = cfg else {
        return Ok(None);
    };
    let registry_path = cfg.registry_path.clone();
    if !registry_path.is_file() {
        return Ok(None);
    }
    let records = registry::load_registry(&registry_path)?;
    let reduced = registry::reduce_records(&records);
    let entries = crate::index::build_entries(&reduced);
    let target = match resolve_fetch_target(&entries, name, path, version, version_id, pin) {
        Ok(t) => t,
        Err(_) => return Ok(None), // 索引が正本のため通常経路へ(エラーはそちらで正式に報告)
    };
    plain_conflict_check(&target, out_dir, yes, format)
}
