//! backup pull(一括復元・設計3.2)。復元job発行+取得+検証+復号。平文衝突保護は
//! getと同一規則(sha一致skip・不一致は--yesなしで対象エラー3)。
//! --out(出力dir)は必須(TASK-19 XDG配置: 旧既定data/restoreは廃止)。

use super::get::{FetchTarget, RestoreCheck, ensure_restored, resolve_tier_days, restore_guidance};
use super::{CtxArgs, open_session, read_index, resolve_key};
use crate::errors::{BackupError, BackupResult};
use std::io::IsTerminal as _;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_backup_pull(
    ctx: &CtxArgs,
    category: Option<&str>,
    run: Option<&str>,
    all: bool,
    tier: Option<&str>,
    days: Option<u32>,
    out: &Path,
    dry_run: bool,
    yes: bool,
) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let scope_count =
            usize::from(all) + usize::from(category.is_some()) + usize::from(run.is_some());
        if scope_count == 0 {
            return Err(BackupError::Usage(
                "backup pullには範囲指定(--all / --category C / --run ID)が必要です".to_string(),
            ));
        }
        if scope_count > 1 {
            return Err(BackupError::Usage(
                "--all / --category / --run は併用できません(いずれか1つを指定してください)"
                    .to_string(),
            ));
        }
        // 確認ガード(設計3.2・事故防止はscope確認)。索引GETの前に判定する
        if !yes {
            use std::io::Write as _;
            if !std::io::stdin().is_terminal() {
                return Err(BackupError::Usage(
                    "backup pullは一括復元です。非TTY実行では --yes が必要です(対話確認はTTYのみ)"
                        .to_string(),
                ));
            }
            print!("backup pullは対象を一括で復元します。続行しますか (yes/no): ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .map_err(|e| BackupError::Config(format!("確認入力の読込に失敗: {e}")))?;
            let answer = line.trim().to_ascii_lowercase();
            if answer != "yes" && answer != "y" {
                println!("中止しました (no)");
                return Ok(0);
            }
        }
        let (tier, days) = resolve_tier_days(tier, days)?;
        let session = open_session(ctx)?;
        let key_path = resolve_key(ctx, session.cfg.as_ref())?;
        super::get::preflight_decrypt_env(&key_path)?;
        let (_h, entries) = read_index(session.backend.as_ref(), &session.aws, &key_path)?;

        let mut targets: Vec<FetchTarget> = Vec::new();
        if let Some(run_id) = run {
            for e in &entries {
                for v in &e.versions {
                    if v.batch_id == run_id {
                        targets.push(FetchTarget::from_version(e, v));
                    }
                }
            }
            if targets.is_empty() {
                println!(
                    "0件 (run={run_id} の対象は索引にありません。list runsで確認してください)"
                );
                return Ok(0);
            }
        } else {
            // TASK-18 5.10: --categoryは「いずれかのlocation.category一致」
            let want_category = category;
            let want_all = all;
            for e in &entries {
                if !want_all
                    && !want_category.is_some_and(|c| e.locations.iter().any(|l| l.category == c))
                {
                    continue;
                }
                if let Some(v) = e.versions.first() {
                    targets.push(FetchTarget::from_version(e, v));
                }
                for p in &e.pins {
                    targets.push(FetchTarget::from_pin(e, p));
                }
            }
            if targets.is_empty() {
                println!("0件 (対象がありません。list files / query で確認してください)");
                return Ok(0);
            }
        }
        if dry_run {
            for t in &targets {
                println!("pull  {}  {}", t.dest_name, t.s3_key);
            }
            println!("plan: {}件 (dry-run)", targets.len());
            return Ok(0);
        }
        let out_dir = out.to_path_buf();
        let mut pending = 0usize;
        let mut ok = 0usize;
        let mut skipped = 0usize;
        let mut first_err: Option<i32> = None;
        for t in &targets {
            // 平文衝突保護(getと同一規則・restore.download-plain-conflict)。読込失敗
            // (sha256検証不能)時も--yesなしでは上書きしない(get側と同一挙動・L-5)
            let dest = t.dest_path(&out_dir);
            if dest.is_file() {
                match super::sha256_of_plain(&dest) {
                    Ok(sha) if sha == t.source_sha256 => {
                        println!("skip {} (既存平文のsha256一致・冪等)", dest.display());
                        skipped += 1;
                        continue;
                    }
                    Ok(_) if !yes => {
                        eprintln!(
                            "error: 対象エラー: 出力先にsha256不一致の既存平文があるため上書きしません(手動リネームまたは移動後に再実行。--yesで上書き): {} ({})",
                            dest.display(),
                            t.dest_name
                        );
                        first_err.get_or_insert(3);
                        continue;
                    }
                    Err(_) if !yes => {
                        eprintln!(
                            "error: 対象エラー: 既存平文の読込に失敗するためsha256を検証できず上書きしません(手動リネームまたは移動後に再実行。--yesで上書き): {} ({})",
                            dest.display(),
                            t.dest_name
                        );
                        first_err.get_or_insert(3);
                        continue;
                    }
                    _ => {} // --yes: 上書きして続行
                }
            }
            match ensure_restored(
                session.backend.as_ref(),
                &session.aws,
                &t.s3_key,
                t.version_id.as_deref(),
                days,
                &tier,
            ) {
                Ok(RestoreCheck::Issued) | Ok(RestoreCheck::Ongoing) => {
                    pending += 1;
                    continue;
                }
                Ok(RestoreCheck::Ready) => {}
                Err(e) => {
                    eprintln!("error: {e}");
                    first_err.get_or_insert(e.exit_code());
                    continue;
                }
            }
            match t.fetch_verify_decrypt(
                session.backend.as_ref(),
                &session.aws,
                &out_dir,
                &key_path,
            ) {
                Ok(dest) => {
                    println!("ok: {}", dest.display());
                    ok += 1;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    first_err.get_or_insert(e.exit_code());
                }
            }
        }
        if pending > 0 {
            println!(
                "{}",
                restore_guidance("backup pull (復元job発行済みの対象は約12〜48h後に再実行)")
            );
        }
        println!(
            "pull: ok={ok} skip={skipped} pending={pending} failed={}",
            targets.len() - ok - skipped - pending
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
