//! edit(設定の編集対象表示・設計3.2)。検証付きの薄いラッパ。

use super::CtxArgs;
use crate::config;
use crate::errors::{BackupError, BackupResult};

#[derive(clap::Args, Debug)]
pub struct EditArgs {
    #[command(flatten)]
    pub ctx: CtxArgs,
    pub target: String,
}

pub fn cmd_edit(args: &EditArgs) -> i32 {
    let run = (|| -> BackupResult<i32> {
        // --config省略時はXDG規定path(TASK-19)
        let cfg_path = super::pipeline_config_path(&args.ctx.config);
        let cfg = config::load_pipeline(&cfg_path)?;
        match args.target.as_str() {
            "metadata" => {
                println!(
                    "[metadata] latest_source_path = {:?}({})",
                    match cfg.metadata.latest_source_path {
                        config::PathMode::Plain => "plain",
                        config::PathMode::Encrypted => "encrypted",
                        config::PathMode::None => "none",
                    },
                    cfg_path.display()
                );
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
                    "[behavior] keep_artifacts = {:?}(cache dir({})配下のrun成果物)",
                    match cfg.keep_artifacts {
                        config::KeepArtifacts::Run => "run",
                        config::KeepArtifacts::Always => "always",
                    },
                    cfg.output_dir.display()
                );
                println!("保持数(lifecycle)はaws.toml(規定path)の[aws.lifecycle]で管理します");
            }
            other => {
                return Err(BackupError::Usage(format!(
                    "editの対象は metadata|files|layout|policy です: {other}"
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
