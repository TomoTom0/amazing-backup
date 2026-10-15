//! edit(設定の編集対象表示・設計3.2)。検証付きの薄いラッパ。

use super::CtxArgs;
use crate::config;
use crate::errors::{BackupError, BackupResult};
use std::path::PathBuf;

#[derive(clap::Args, Debug)]
pub struct EditArgs {
    #[command(flatten)]
    pub ctx: CtxArgs,
    pub target: String,
}

pub fn cmd_edit(args: &EditArgs) -> i32 {
    let run = (|| -> BackupResult<i32> {
        let cfg_path = args
            .ctx
            .config
            .clone()
            .unwrap_or_else(|| PathBuf::from("configs/pipeline.toml"));
        let cfg = config::load_pipeline(&cfg_path)?;
        match args.target.as_str() {
            "categories" => {
                println!("現在の[categories]({}):", cfg_path.display());
                for (name, cat) in &cfg.categories {
                    println!("  [categories.{name}] keep = {}", cat.keep);
                }
                println!("編集はpipeline.tomlを直接編集してください(保存後に検証されます)");
            }
            "files" => {
                println!("現在の[files]手動写像({}):", cfg_path.display());
                for (name, m) in &cfg.files {
                    println!(
                        "  [files.{name:?}] category = {:?} path = {:?}",
                        m.category, m.path
                    );
                }
                if cfg.files.is_empty() {
                    println!("  (なし:auto_path_roots相対化またはfallbackへ分類)");
                }
            }
            "layout" => {
                println!(
                    "[layout] unfiled_category={} auto_path_roots={}",
                    cfg.layout.unfiled_category,
                    cfg.layout
                        .auto_path_roots
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            "policy" => {
                println!(
                    "[output] keep_artifacts = {:?}",
                    match cfg.keep_artifacts {
                        config::KeepArtifacts::Run => "run",
                        config::KeepArtifacts::Always => "always",
                    }
                );
                for (name, cat) in &cfg.categories {
                    println!("[categories.{name}] keep = {}", cat.keep);
                }
            }
            other => {
                return Err(BackupError::Usage(format!(
                    "editの対象は categories|files|layout|policy です: {other}"
                )));
            }
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
