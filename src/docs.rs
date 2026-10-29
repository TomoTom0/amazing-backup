//! docsサブコマンド(TASK-4。tmp/tasks/tm-4/design_docs_command.md 3章)。
//! 配布ドキュメント(使用手順書・設定example・LLM/agent向け契約・SKILL.md・pointer
//! snippet)をstdoutへ出力する。設定読込・S3 backend構築を行わない純粋出力command
//! (completions・config initのtemplateと同じ契約。AMAZING_BACKUP_HOME・XDG非依存)。
//! 正本はrepo fileをinclude_str!で埋め込む(docs/usage_backup.md・docs/llm/*.md)。
//! configs/*.example.tomlはconfig.rsの埋め込みconstを共用する(同一fileのinclude_str!を
//! 二重に書かず埋め込み箇所を1つに保つ=design doc 3.2)。

use crate::errors::BackupResult;
use std::io::Write;
use std::path::Path;

/// docs usage pageの正本(docs/usage_backup.md。TASK-3再構成後のinstall後前提doc本身。
/// version行のみ出力時に置換する=design doc 3.4)。
const USAGE_BACKUP_DOC: &str = include_str!("../docs/usage_backup.md");

/// docs llm配下3pageの正本(docs/llm/*.md。design doc 3.5/3.6)。
const LLM_USAGE_DOC: &str = include_str!("../docs/llm/usage.md");
const LLM_SKILL_DOC: &str = include_str!("../docs/llm/skill.md");
const LLM_SNIPPET_DOC: &str = include_str!("../docs/llm/snippet.md");

/// docs config pageのaws側marker置換後の例示path(design doc 3.3)。固定literalとし
/// 実行環境のXDG解決(resolve_app_paths)を使わない(docsは参照資料であり出力が環境で
/// 変わるとtest・引用が不安定になるため。`~`は展開されないliteral=usage_backup.md
/// 2.2/2.3の規定path表記と同一の表現)。
const DOCS_CONFIG_CREDENTIALS_EXAMPLE: &str = "~/.config/amazing-backup/aws-credentials.toml";

/// docs config pageの2file(pipeline.toml・aws.toml)の区切り行(design doc 3.3。
/// raw連結ではどこからがaws.tomlか分からないため表示上の必須)。
const DOCS_CONFIG_SEPARATOR_LINE: &str = "# ---- aws.toml ----";

/// docs引数なしのページ一覧(design doc 3.7の設計済み出力。clap既定の
/// MissingSubcommand(64)とせず発見性のある一覧をstdout+exit 0へ出す)。
const DOCS_PAGE_LIST: &str = concat!(
    "amazing-backup docs — 配布ドキュメント一覧\n",
    "  usage        使用手順書全文(人間向け)\n",
    "  config       設定example全文(pipeline.toml・aws.toml・marker置換済み)\n",
    "  llm usage    LLM/agent向けusage(コマンド契約・終了コード・運用規則)\n",
    "  llm skill    SKILL.md正本(~/.claude/skills/amazing-backup/SKILL.mdへ配置)\n",
    "  llm snippet  CLAUDE.md/AGENTS.md追記用pointer\n",
    "各ページはstdoutへ全文出力する(例: amazing-backup docs usage)\n",
);

/// docs llm引数なしのllm配下ページ一覧(design doc 3.7)。
const DOCS_LLM_PAGE_LIST: &str = concat!(
    "amazing-backup docs llm — LLM/agent向け配布ページ一覧\n",
    "  usage    LLM/agent向けusage(コマンド契約・終了コード・運用規則)\n",
    "  skill    SKILL.md正本(~/.claude/skills/amazing-backup/SKILL.mdへ配置)\n",
    "  snippet  CLAUDE.md/AGENTS.md追記用pointer\n",
    "各ページはstdoutへ全文出力する(例: amazing-backup docs llm usage)\n",
);

/// docsサブコマンド群の引数(design doc 3.1。全ページ引数ゼロ。サブcommand省略時は
/// ページ一覧=3.7)。
#[derive(clap::Args, Debug)]
pub struct DocsArgs {
    #[command(subcommand)]
    pub sub: Option<DocsSub>,
}

#[derive(clap::Subcommand, Debug)]
pub enum DocsSub {
    /// 使用手順書全文(人間向け)
    Usage,
    /// 設定example全文(pipeline.toml・aws.toml・marker置換済み)
    Config,
    /// LLM/agent向け配布ページ(usage/skill/snippet)
    Llm(DocsLlmArgs),
}

/// docs llm配下の引数(design doc 3.1。サブcommand省略時はllm配下一覧)。
#[derive(clap::Args, Debug)]
pub struct DocsLlmArgs {
    #[command(subcommand)]
    pub sub: Option<DocsLlmSub>,
}

#[derive(clap::Subcommand, Debug)]
pub enum DocsLlmSub {
    /// LLM/agent向けusage(コマンド契約・終了コード・運用規則)
    Usage,
    /// SKILL.md正本(~/.claude/skills/amazing-backup/SKILL.mdへ配置)
    Skill,
    /// CLAUDE.md/AGENTS.md追記用pointer
    Snippet,
}

/// usage_backup.mdの`- 実装バージョン:`行頭の行の値をversionへ置換する(design doc
/// 3.4)。行頭が対象prefixに一致する行のみを置換対象とし(行頭空白・文中出現は対象外)、
/// 対象行以外のbyteは一切変化させず入力末尾の改行を保持する。repo正本には実versionを
/// literalのまま書き、docs usage出力時に恒に実行する(正本literalとbinary versionが
/// 一致する通常時は出力上の変化なし=sync testと出力testの二重防御)。
pub fn substitute_version_lines(doc: &str, version: &str) -> String {
    const PREFIX: &str = "- 実装バージョン:";
    let mut out = String::with_capacity(doc.len() + 16);
    for line in doc.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix(PREFIX) {
            out.push_str(PREFIX);
            out.push(' ');
            out.push_str(version);
            if rest.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(line);
        }
    }
    out
}

/// docs config pageの組立(design doc 3.3): pipeline.example.toml全文+区切り行+
/// aws.example.toml(markerを固定例示pathへ置換したもの)全文。置換はconfig initと
/// 同一経路(render_init_template。置換前後検査込み)を再利用するため「docs configに
/// 見えるもの=initが生成するもの」の一致が単一実装で構造保証される。template正本の
/// 劣化は`BackupError::Internal`(exit 1)。
fn render_docs_config_page() -> BackupResult<String> {
    let aws_rendered = crate::config::render_init_template(
        crate::config::INIT_AWS_TEMPLATE,
        Path::new(DOCS_CONFIG_CREDENTIALS_EXAMPLE),
    )?;
    Ok(format!(
        "{}{DOCS_CONFIG_SEPARATOR_LINE}\n{aws_rendered}",
        crate::config::INIT_PIPELINE_TEMPLATE
    ))
}

/// pageの内容(変換適用後)を組立る。design doc 3.7: 各ページは正本の内容をそのまま
/// (byte一致・行追加・装飾なし。docs configの区切り行のみ例外)。
fn render_docs_page(sub: &Option<DocsSub>) -> BackupResult<Vec<u8>> {
    Ok(match sub {
        None => DOCS_PAGE_LIST.as_bytes().to_vec(),
        Some(DocsSub::Usage) => {
            substitute_version_lines(USAGE_BACKUP_DOC, env!("CARGO_PKG_VERSION")).into_bytes()
        }
        Some(DocsSub::Config) => render_docs_config_page()?.into_bytes(),
        Some(DocsSub::Llm(llm)) => match &llm.sub {
            None => DOCS_LLM_PAGE_LIST,
            Some(DocsLlmSub::Usage) => LLM_USAGE_DOC,
            Some(DocsLlmSub::Skill) => LLM_SKILL_DOC,
            Some(DocsLlmSub::Snippet) => LLM_SNIPPET_DOC,
        }
        .as_bytes()
        .to_vec(),
    })
}

/// docsサブコマンドの実行(design doc 3.7/3.8)。出力は`Vec<u8>`へ組立後`write_all`
/// 単一呼出(TASK-2 completionsと同一契約: BrokenPipeは`| head`等の先読み終了のため
/// 正常終了0。その他の書込失敗はstderrへerror表示+4。docs configのtemplate正本劣化は
/// render_init_templateの2段検査でInternal=1)。
pub fn cmd_docs(args: &DocsArgs, out: &mut dyn Write) -> i32 {
    let page = match render_docs_page(&args.sub) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return e.exit_code();
        }
    };
    match out.write_all(&page) {
        Ok(()) => 0,
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            eprintln!("error: docsページの出力に失敗: {e}");
            4
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 常にエラーを返す偽Write(書込失敗注入。kindはerror種の指定用)。
    struct FailingWrite(std::io::ErrorKind);

    impl std::io::Write for FailingWrite {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.0))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(self.0))
        }
    }

    fn args(sub: Option<DocsSub>) -> DocsArgs {
        DocsArgs { sub }
    }

    fn llm_args(sub: Option<DocsLlmSub>) -> DocsArgs {
        args(Some(DocsSub::Llm(DocsLlmArgs { sub })))
    }

    /// [covers:docs.pages-stdout-verbatim-from-repository]
    /// unit(Write注入)側: docs usage・docs llm usage/skill/snippetは各正本の内容を
    /// stdout(注入Write)へそのまま(byte一致)出力し exit 0。usageはversion行置換後
    /// (置換関数単独の検証はtests/docs_pages.rsの合成入力testが担い、ここでは
    /// page出力経路の出力内容を観測する)。
    #[test]
    fn docs_pages_write_repository_content_to_injected_write() {
        for (name, a, expected) in [
            (
                "llm usage",
                llm_args(Some(DocsLlmSub::Usage)),
                LLM_USAGE_DOC,
            ),
            (
                "llm skill",
                llm_args(Some(DocsLlmSub::Skill)),
                LLM_SKILL_DOC,
            ),
            (
                "llm snippet",
                llm_args(Some(DocsLlmSub::Snippet)),
                LLM_SNIPPET_DOC,
            ),
        ] {
            let mut out: Vec<u8> = Vec::new();
            assert_eq!(cmd_docs(&a, &mut out), 0, "docs {name} はexit 0");
            assert_eq!(
                String::from_utf8_lossy(&out),
                expected,
                "docs {name} の出力は正本とbyte一致(行追加・装飾なし)"
            );
        }
        // usage: version行がbinary自身のversionへ置換された正本内容
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(cmd_docs(&args(Some(DocsSub::Usage)), &mut out), 0);
        let text = String::from_utf8_lossy(&out).into_owned();
        let targets: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("- 実装バージョン:"))
            .collect();
        assert_eq!(targets.len(), 1, "version行は1行: {targets:?}");
        assert_eq!(
            targets[0],
            format!("- 実装バージョン: {}", env!("CARGO_PKG_VERSION")),
            "version行の値はbinary自身のversion(--versionと同一source)へ置換"
        );
        assert_eq!(
            text,
            substitute_version_lines(USAGE_BACKUP_DOC, env!("CARGO_PKG_VERSION")),
            "usage pageの出力はversion置換後の正本とbyte一致"
        );
    }

    /// [covers:docs.config-marker-replaced]
    /// unit(Write注入)側: docs configは pipeline.example.toml全文+区切り行+
    /// aws.example.toml(markerを固定例示pathへ置換したもの)の全文を出力する
    /// (期待値のaws側はconfig initと同一経路のrender_init_templateで組み立てる)。
    /// 出力中にmarker残存なし。
    #[test]
    fn docs_config_page_replaces_marker_via_init_renderer() {
        let expected = format!(
            "{}{DOCS_CONFIG_SEPARATOR_LINE}\n{}",
            crate::config::INIT_PIPELINE_TEMPLATE,
            crate::config::render_init_template(
                crate::config::INIT_AWS_TEMPLATE,
                Path::new(DOCS_CONFIG_CREDENTIALS_EXAMPLE)
            )
            .expect("期待値のaws側組立(render_init_templateの適用)")
        );
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(
            cmd_docs(&args(Some(DocsSub::Config)), &mut out),
            0,
            "docs configはexit 0"
        );
        let text = String::from_utf8_lossy(&out).into_owned();
        assert_eq!(
            text, expected,
            "pipeline全文+区切り行+aws(marker置換後)全文とbyte一致"
        );
        assert!(
            !text.contains("__CREDENTIALS_FILE__"),
            "出力中にmarker残存なし: {text}"
        );
        assert!(
            text.contains(&format!(
                "credentials_file = \"{DOCS_CONFIG_CREDENTIALS_EXAMPLE}\""
            )),
            "全profileのcredentials_fileが固定例示pathへ置換されている: {text}"
        );
    }

    /// [covers:docs.page-list-no-args]
    /// unit(Write注入)側: docs引数なしはページ一覧(usage/config/llm usage/llm skill/
    /// llm snippetの各説明)を出力し exit 0。docs llm引数なしはllm配下3ページの一覧。
    #[test]
    fn docs_no_args_writes_page_list() {
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(cmd_docs(&args(None), &mut out), 0, "docs(引数なし)はexit 0");
        let list = String::from_utf8_lossy(&out).into_owned();
        for (page, desc) in [
            ("usage", "使用手順書"),
            ("config", "設定"),
            ("llm usage", "LLM"),
            ("llm skill", "SKILL.md"),
            ("llm snippet", "追記"),
        ] {
            assert!(
                list.lines().any(|l| l.contains(page) && l.contains(desc)),
                "ページ({page})とその説明({desc})を含む行がある: {list}"
            );
        }

        let mut out: Vec<u8> = Vec::new();
        assert_eq!(
            cmd_docs(&llm_args(None), &mut out),
            0,
            "docs llm(引数なし)はexit 0"
        );
        let list = String::from_utf8_lossy(&out).into_owned();
        for (page, desc) in [("usage", "LLM"), ("skill", "SKILL.md"), ("snippet", "追記")] {
            assert!(
                list.lines().any(|l| l.contains(page) && l.contains(desc)),
                "llm配下ページ({page})とその説明({desc})を含む行がある: {list}"
            );
        }
    }

    /// [covers:docs.output-write-error-exit-4]
    /// unit(偽Write注入)側: 書込失敗のうちBrokenPipe(`| head`等の先読み終了)は
    /// exit 0。その他の書込失敗は exit 4(TASK-2 completionsと同一契約。
    /// stderrのerror表示・exit 4の組合せ観測はintegration側が担う)。
    #[test]
    fn docs_write_errors_broken_pipe_0_other_4() {
        let mut broken = FailingWrite(std::io::ErrorKind::BrokenPipe);
        assert_eq!(
            cmd_docs(&args(Some(DocsSub::Usage)), &mut broken),
            0,
            "BrokenPipeは正常終了扱い(exit 0)"
        );
        let mut other = FailingWrite(std::io::ErrorKind::Other);
        assert_eq!(
            cmd_docs(&args(Some(DocsSub::Usage)), &mut other),
            4,
            "BrokenPipe以外の書込失敗はexit 4(処理エラー)"
        );
    }
}
