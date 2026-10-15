//! pin add/rm(ピン留め版の付与/解除・設計3.2・1.3)。

use super::get::{RestoreCheck, ensure_restored, find_entry, restore_guidance, select_version};
use super::{CtxArgs, now_jst, open_session, read_index, require_versioning, resolve_key};
use crate::errors::{BackupError, BackupResult};
use crate::index::IndexPin;
use crate::registry;
use std::path::Path;

#[derive(clap::Args, Debug)]
pub struct PinArgs {
    #[command(subcommand)]
    pub sub: PinSub,
}

#[derive(clap::Subcommand, Debug)]
pub enum PinSub {
    /// ピン留め付与(write-once写し)
    Add {
        #[command(flatten)]
        ctx: CtxArgs,
        name: String,
        #[arg(long = "latest", group = "pin_version")]
        latest: bool,
        #[arg(long, group = "pin_version", value_parser = super::parse_date)]
        version: Option<String>,
        #[arg(long = "as")]
        as_pin: Option<String>,
    },
    /// ピン留め解除
    Rm {
        #[command(flatten)]
        ctx: CtxArgs,
        name: String,
        #[arg(long = "pin")]
        pin: String,
    },
}

pub fn cmd_pin(args: &PinArgs) -> i32 {
    match &args.sub {
        PinSub::Add {
            ctx,
            name,
            version,
            as_pin,
            ..
        } => cmd_pin_add(ctx, name, version.as_deref(), as_pin.as_deref()),
        PinSub::Rm { ctx, name, pin } => cmd_pin_rm(ctx, name, pin),
    }
}

fn cmd_pin_add(ctx: &CtxArgs, name: &str, version: Option<&str>, as_pin: Option<&str>) -> i32 {
    // pin名は引数形式検証として最初に(usage 64)
    let Some(pin_name) = as_pin else {
        eprintln!("error: pin名の指定(--as)が必要です");
        return 64;
    };
    if let Err(e) = crate::layout::validate_pin_name(pin_name) {
        eprintln!("error: {e}");
        return e.exit_code();
    }
    let run = (|| -> BackupResult<i32> {
        // registryへの記録を要するため--config必須。S3変更(copy_object)より前に解決して
        // おく(M-4: copy後にregistry解決失敗するとwrite-once滞留で手詰まりになる)
        if ctx.config.is_none() {
            return Err(BackupError::Config(
                "pin addには--config(pipeline.toml・registry記録先)が必要です".to_string(),
            ));
        }
        let session = open_session(ctx)?;
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        let registry_path = session
            .cfg
            .as_ref()
            .map(|c| c.registry_path.clone())
            .expect("configは必須検査済み");
        require_versioning(session.backend.as_ref(), &session.aws)?;
        let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
        let entry = find_entry(&entries, name, None)?;
        let v = if let Some(date) = version {
            select_version(entry, Some(date), None)?
        } else {
            // 既定=最新
            select_version(entry, None, None)?
        };
        // write-once確認(既存pin keyへの再付与拒否)
        let pin_key = crate::layout::pin_object_key(
            &session.aws.prefix,
            &entry.category,
            &entry.rel_path,
            &entry.folder,
            pin_name,
        )?;
        if session
            .backend
            .head(&session.aws.bucket, &pin_key)?
            .is_some()
        {
            return Err(BackupError::Target(format!(
                "pin名({pin_name})は既に存在します(write-once)。上書きする場合は pin rm {name} --pin {pin_name} 後に再付与してください: {pin_key}"
            )));
        }
        match ensure_restored(
            session.backend.as_ref(),
            &session.aws,
            &v.s3_key,
            Some(&v.version_id),
            7,
            "Standard",
        )? {
            RestoreCheck::Issued | RestoreCheck::Ongoing => {
                println!(
                    "{}",
                    restore_guidance(&format!("pin add {name} --as {pin_name}"))
                );
                return Ok(0);
            }
            RestoreCheck::Ready => {}
        }
        session.backend.copy_object(
            &session.aws.bucket,
            &v.s3_key,
            Some(&v.version_id),
            &pin_key,
            &session.aws.storage_class,
        )?;
        let head = session
            .backend
            .head(&session.aws.bucket, &pin_key)?
            .ok_or_else(|| BackupError::Transfer(format!("pin objectの確認に失敗: {pin_key}")))?;
        let recorded_at = now_jst();
        registry::append_record(
            &registry_path,
            &registry::PinWriteRecord {
                schema: 1,
                record_type: "pin_write".into(),
                recorded_at: recorded_at.clone(),
                name: entry.name.clone(),
                category: entry.category.clone(),
                rel_path: entry.rel_path.clone(),
                folder: entry.folder.clone(),
                pin: pin_name.to_string(),
                pin_s3_key: pin_key.clone(),
                source_s3_key: v.s3_key.clone(),
                source_version_id: v.version_id.clone(),
                date: v.date.clone(),
                source_sha256: v.source_sha256.clone(),
                age_sha256: v.age_sha256.clone(),
                source_size: v.source_size,
                s3_bucket: session.aws.bucket.clone(),
                s3_size: head.size,
                s3_checksum_sha256: head.checksum_sha256.clone().unwrap_or_default(),
                uploaded_at: recorded_at,
            },
        )?;
        rebuild_and_upload_index(&session, &registry_path, "pin-add")?;
        println!("ok: pin {pin_name} -> {pin_key}");
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

fn cmd_pin_rm(ctx: &CtxArgs, name: &str, pin_name: &str) -> i32 {
    let run = (|| -> BackupResult<i32> {
        // registryへの記録を要するため--config必須(S3変更より前に解決・M-4)
        if ctx.config.is_none() {
            return Err(BackupError::Config(
                "pin rmには--config(pipeline.toml・registry記録先)が必要です".to_string(),
            ));
        }
        let session = open_session(ctx)?;
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        let registry_path = session
            .cfg
            .as_ref()
            .map(|c| c.registry_path.clone())
            .expect("configは必須検査済み");
        require_versioning(session.backend.as_ref(), &session.aws)?;
        let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;
        let entry = find_entry(&entries, name, None)?;
        let pin: &IndexPin = entry
            .pins
            .iter()
            .find(|p| p.pin == pin_name)
            .ok_or_else(|| {
                BackupError::Target(format!(
                    "pin名({pin_name})はname={name} の索引にありません(list pinsで確認してください)"
                ))
            })?;
        // DA配置のpinも復元jobなしで削除できる(deleteはarchive objectへ可能)。
        // 削除はversion実体の削除(DeleteObjectVersion相当・delete marker付与のみで
        // 実体がnoncurrentへ残る形でない。TASK-11 Phase B・diffのpins報告と整合)
        let versions = session
            .backend
            .list_versions(&session.aws.bucket, &pin.s3_key)?;
        let mut deleted = 0usize;
        for v in versions.iter().filter(|v| v.key == pin.s3_key) {
            if v.is_delete_marker || v.version_id.is_empty() {
                continue;
            }
            session.backend.delete_object_version(
                &session.aws.bucket,
                &pin.s3_key,
                &v.version_id,
            )?;
            deleted += 1;
        }
        if deleted == 0 {
            return Err(BackupError::Transfer(format!(
                "pin objectの版が見つからない(削除できません): {}",
                pin.s3_key
            )));
        }
        // 記録失敗時は索引pins[]がstaleのまま残る(再実行は索引pins[]由来の解決のため
        // 失敗を繰返す)。manage index rebuildによる回復を案内する(L-1)
        registry::append_record(
            &registry_path,
            &registry::PinRemoveRecord {
                schema: 1,
                record_type: "pin_remove".into(),
                recorded_at: now_jst(),
                name: name.to_string(),
                pin: pin_name.to_string(),
                pin_s3_key: pin.s3_key.clone(),
                s3_bucket: session.aws.bucket.clone(),
            },
        )
        .map_err(|e| {
            BackupError::Process(format!(
                "pin_remove記録の失敗: {e}。S3側のpin object削除は完了しています。索引のpins[]が削除前のまま残るため `backup-remote manage index rebuild` で索引を再構築してください"
            ))
        })?;
        rebuild_and_upload_index(&session, &registry_path, "pin-rm")?;
        println!("ok: pin {pin_name} を削除しました ({})", pin.s3_key);
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

/// registryから索引を再構築してS3へ配置する(pin/prune等の索引影響操作後)。
pub(crate) fn rebuild_and_upload_index(
    session: &super::Session,
    registry_path: &Path,
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
