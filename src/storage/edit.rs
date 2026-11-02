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
        // TASK-7 D-3: aws.tomlを先に読み、その実効profile名でpipeline.tomlを読む。
        // --config省略時はXDG規定path(TASK-19)
        let aws = config::load_aws_profile(
            &super::aws_config_path(&args.ctx.aws_config),
            args.ctx.profile.as_deref(),
            None,
        )?;
        let cfg_path = super::pipeline_config_path(&args.ctx.config);
        let cfg = config::load_pipeline(&cfg_path, &aws.profile)?;
        // 各対象の出力1行目に実効profile名を表示(TASK-7: 表示中のsectionが
        // [profiles.<実効profile名>]配下のものであることを明示する)
        match args.target.as_str() {
            "metadata" => {
                println!("実効profile: {}", cfg.profile);
                println!(
                    "[profiles.{}.metadata] latest_source_path = {:?}({})",
                    cfg.profile,
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
                println!("実効profile: {}", cfg.profile);
                println!("現在の[files]手動写像({}):", cfg_path.display());
                for (name, m) in &cfg.files {
                    println!(
                        "  [files.{name:?}] category = {:?} label = {:?}",
                        m.category, m.label
                    );
                }
                if cfg.files.is_empty() {
                    println!("  (なし:auto_path_roots相対化またはfallbackへ分類)");
                }
            }
            "layout" => {
                println!("実効profile: {}", cfg.profile);
                println!(
                    "[profiles.{}.layout] unfiled_category={} auto_path_roots={}",
                    cfg.profile,
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
                println!("実効profile: {}", cfg.profile);
                println!(
                    "[profiles.{}.behavior] keep_artifacts = {:?}(cache dir({})配下のrun成果物)",
                    cfg.profile,
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
