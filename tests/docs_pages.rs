//! TASK-4 docsサブコマンドのテスト(設計tmp/tasks/tm-4/design_docs_command.md 3.9の
//! 観測方針: integration=bin subprocess・AMAZING_BACKUP_HOMEを空dirへ固定して設定
//! 非依存を検証。unit観測のうち変換関数・正本sync・終了コード表一致は本ファイルで、
//! Write注入系のunit観測(各ページ出力・書込失敗の偽Write注入)は実装工程で
//! src/docs.rs内unit testへ追加する=design doc 3.9・5章影響のとおり)。
//! テストが仮定するlib API(design doc 3.4/3.9のtarget_function具現化。実装工程で
//! signatureを調整した場合は本テストを追従させる):
//!   - amazing_backup::docs::substitute_version_lines(doc: &str, version: &str) -> String
//!     (usage_backup.mdの`- 実装バージョン:`行頭の行の値をversionへ置換する変換関数。
//!     合成入力=正本と異なるversion literalでの検証がdesign doc 3.4段1の要件のため、
//!     置換versionは引数とする)
//! テスト先行のため実装前(src/docs.rs・docs/llm/*.md未作成)はcompile error
//! (unresolved import・include_str!対象file不在)を許容する。

mod common;

use amazing_backup::docs::substitute_version_lines;
use common::*;
use std::path::Path;

/// 各配布ページのrepo正本(design doc 3.2。docs/llm/*.mdは次工程で作成)。
const LLM_USAGE_DOC: &str = include_str!("../docs/llm/usage.md");
const LLM_SKILL_DOC: &str = include_str!("../docs/llm/skill.md");
const LLM_SNIPPET_DOC: &str = include_str!("../docs/llm/snippet.md");

/// docs configの正本2file(design doc 3.2/3.3。aws側はmarker置換の元)。
const PIPELINE_EXAMPLE: &str = include_str!("../configs/pipeline.example.toml");
const AWS_EXAMPLE: &str = include_str!("../configs/aws.example.toml");

/// docs系は設定に非依存(design doc 3.8)のためAMAZING_BACKUP_HOMEを空dirへ固定して
/// binを実行する(設定不在環境で動作することの検証も兼ねる)。
fn run_docs(args: &[&str]) -> (i32, String, String) {
    let tmp = TestDir::new("docs-page");
    let home = tmp.path().display().to_string();
    run_bin_env(args, &[("AMAZING_BACKUP_HOME", &home)])
}

/// 正本usage_backup.mdのversion行をversion値へ置換した期待値(test側の独立実装。
/// 実装の変換関数substitute_version_linesを使わない=同じ不具合を見逃さないため。
/// split_inclusive('\n')で末尾改行を保持した行処理とする)。
fn expected_usage_doc_with_version(doc: &str, version: &str) -> String {
    let mut out = String::new();
    for line in doc.split_inclusive('\n') {
        if line.starts_with("- 実装バージョン:") {
            out.push_str("- 実装バージョン: ");
            out.push_str(version);
            out.push('\n');
        } else {
            out.push_str(line);
        }
    }
    out
}

/// doc本文から「終了コード一覧」節(見出し語を含む見出しから次の見出しまで)の
/// markdown表を行単位で抽出する(1行=(code, 区分)。header・区切り行はcode列が
/// 数値parse不能のため自動的に除外される)。
fn extract_exit_code_rows(doc: &str) -> Vec<(u32, String)> {
    let mut rows = Vec::new();
    let mut in_section = false;
    for line in doc.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            if in_section {
                break; // 次の見出し=節の終わり
            }
            in_section = t.contains("終了コード一覧");
            continue;
        }
        if in_section && t.starts_with('|') {
            let cells: Vec<&str> = t.split('|').map(|c| c.trim()).collect();
            if cells.len() >= 3 {
                if let Ok(code) = cells[1].parse::<u32>() {
                    let category = cells[2];
                    if !category.is_empty() {
                        rows.push((code, category.to_string()));
                    }
                }
            }
        }
    }
    rows
}

/// [covers:docs.usage-version-substituted]
/// 変換関数unit(design doc 3.4段1): 正本と異なるversion literal(9.9.9)を持つ合成入力を
/// 変換関数へ渡し、期待値はテスト側で独立に組み立てた文字列(実装の変換関数を使わず、
/// 対象行全体の完全一致置換で作る)とbyte比較する。検証点:
/// (a)`- 実装バージョン:`行頭の行が合成入力中ちょうど1行でその1行のみが置換される
///    (行頭が空白・文中出現の類似行は対象外のまま不変)
/// (b)対象行以外のbyteが一切変化しない
/// (c)入力末尾の改行が保持される
/// (d)正本literalとCARGO_PKG_VERSIONが一致する通常時は無変換出力でも置換検査を
///    通ってしまうため、異なるversion入力による変換処理単独の検証とする
#[test]
fn substitute_version_lines_replaces_only_target_line_on_synthetic_input() {
    let input = concat!(
        "# amazing-backup 使用手順書(合成入力)\n",
        "\n",
        "- 対象読者: 運用者\n",
        "- 実装バージョン: 9.9.9\n",
        "- 最終確認: 2099-12-31(合成)\n",
        "  - 実装バージョン: 8.8.8(行頭が空白=対象外)\n",
        "備考: - 実装バージョン: 7.7.7(行頭でない出現=対象外)\n",
        "\n",
        "## 1. 概要\n",
        "対象行以外のbyteが一切変化しないことの確認用本文。\n",
    );
    let version = env!("CARGO_PKG_VERSION");
    // 期待値は実装変換関数を使わず、対象行全体の完全一致をString::replaceで差し替えて
    // 独立に組み立てる(類似行はhitしない)
    let expected = input.replace(
        "- 実装バージョン: 9.9.9\n",
        &format!("- 実装バージョン: {version}\n"),
    );
    let got = substitute_version_lines(input, version);
    assert_eq!(
        got, expected,
        "対象行のみ置換・他byte不変・末尾改行保持(byte一致)"
    );
    assert!(
        got.contains(&format!("- 実装バージョン: {version}\n")),
        "対象行の値がbinary versionへ置換される: {got:?}"
    );
    assert!(
        got.contains("- 実装バージョン: 8.8.8(行頭が空白=対象外)\n"),
        "行頭が対象prefixで無い類似行は不変: {got:?}"
    );
    assert!(
        got.contains("備考: - 実装バージョン: 7.7.7(行頭でない出現=対象外)\n"),
        "行頭でない出現は対象外のまま不変: {got:?}"
    );
    assert!(
        !got.contains("9.9.9"),
        "合成version literalは残らない: {got:?}"
    );
    assert!(
        got.ends_with("確認用本文。\n"),
        "入力末尾の改行が保持される: {got:?}"
    );
}

/// [covers:docs.usage-version-substituted]
/// 変換関数unit(コードレビュー指摘対応の検出力補強): 対象行が入力末尾にある場合の
/// 改行の扱い。対象行の改行の有無は置換後もそのまま保持される=置換時の無条件改行
/// 追加・改行欠落の両退行を検出する。期待値はテスト側で独立に組み立てる
/// (実装の変換関数を使わない=最初の合成入力testと同じ手法)。
#[test]
fn substitute_version_lines_keeps_target_line_newline_presence_and_absence() {
    let version = env!("CARGO_PKG_VERSION");
    // (a) 対象行だけが改行なし(対象行が入力末尾): 置換後も末尾改行なし
    // (置換時に無条件で改行を追加する退行を検出)
    let input_no_nl = concat!(
        "# 合成入力(対象行が末尾・改行なし)\n",
        "- 対象読者: 運用者\n",
        "- 実装バージョン: 9.9.9",
    );
    let expected_no_nl = input_no_nl.replace(
        "- 実装バージョン: 9.9.9",
        &format!("- 実装バージョン: {version}"),
    );
    let got = substitute_version_lines(input_no_nl, version);
    assert_eq!(
        got, expected_no_nl,
        "対象行の改行なしは置換後も保持される(無条件改行追加の退行を検出)"
    );
    assert!(!got.ends_with('\n'), "末尾に改行は追加されない: {got:?}");
    assert!(
        got.ends_with(&format!("- 実装バージョン: {version}")),
        "末尾の対象行の値は置換される: {got:?}"
    );
    assert!(
        !got.contains("9.9.9"),
        "合成version literalは残らない: {got:?}"
    );

    // (b) 対象行が入力末尾で改行付き: 置換後も末尾改行あり(改行欠落の退行を検出)
    let input_nl = concat!(
        "# 合成入力(対象行が末尾・改行付き)\n",
        "- 対象読者: 運用者\n",
        "- 実装バージョン: 9.9.9\n",
    );
    let expected_nl = input_nl.replace(
        "- 実装バージョン: 9.9.9\n",
        &format!("- 実装バージョン: {version}\n"),
    );
    let got = substitute_version_lines(input_nl, version);
    assert_eq!(
        got, expected_nl,
        "対象行の改行付きは置換後も保持される(改行欠落の退行を検出)"
    );
    assert!(
        got.ends_with(&format!("- 実装バージョン: {version}\n")),
        "末尾改行付きの対象行として置換される: {got:?}"
    );
}

/// [covers:docs.usage-doc-version-sync]
/// repo正本docs/usage_backup.mdの実装バージョンliteralはCARGO_PKG_VERSIONと一致し、
/// かつ`- 実装バージョン:`行はfile中にちょうど1行である(release時のdocs更新忘れ=
/// version置換の前提崩れと、対象行の重複化による置換の不定挙動をfailで検出)。
#[test]
fn usage_backup_version_literal_syncs_with_cargo_pkg_version() {
    const DOC: &str = include_str!("../docs/usage_backup.md");
    let targets: Vec<&str> = DOC
        .lines()
        .filter(|l| l.starts_with("- 実装バージョン:"))
        .collect();
    assert_eq!(targets.len(), 1, "対象行はfile中にちょうど1行: {targets:?}");
    assert_eq!(
        targets[0],
        format!("- 実装バージョン: {}", env!("CARGO_PKG_VERSION")),
        "repo正本のversion literalはCARGO_PKG_VERSIONと一致(release時docs更新忘れの検出)"
    );
}

/// [covers:docs.llm-usage-exit-code-table-consistent]
/// docs/llm/usage.mdの終了コード一覧とdocs/usage_backup.md終了コード一覧の一致を
/// 次の全点で検証する(design doc 3.5):
/// (1)各表のcode集合が固定の期待集合{0,1,2,3,4,5,64}と完全一致(集合比較のみでは
///    2と4の意味交換や両表の抽出失敗=空集合同士を通すため固定期待を置く)
/// (2)code→区分(成功/内部エラー/設定・環境/対象/検証/転送/使用法)の対応が両doc間で一致
/// (3)同一表内の同一code重複行の拒否
/// (4)抽出が空集合(表の不在・形式変更)の拒否
/// 検出対象は終了コード表の乖離のみ(表以外の事実の文言乖離は対象外=update-docs運用)。
#[test]
fn exit_code_tables_are_consistent_between_usage_docs() {
    const HUMAN_DOC: &str = include_str!("../docs/usage_backup.md");
    let expected_codes: std::collections::BTreeSet<u32> =
        [0u32, 1, 2, 3, 4, 5, 64].into_iter().collect();
    let mut tables: Vec<std::collections::BTreeMap<u32, String>> = Vec::new();
    for (name, doc) in [
        ("usage_backup.md", HUMAN_DOC),
        ("llm/usage.md", LLM_USAGE_DOC),
    ] {
        let rows = extract_exit_code_rows(doc);
        // (4)抽出が空集合(表の不在・形式変更)は拒否
        assert!(
            !rows.is_empty(),
            "{name}: 終了コード一覧の表から行を抽出できる(空集合=表の不在・形式変更は拒否)"
        );
        // (3)同一表内の同一code重複行は拒否
        let mut seen = std::collections::BTreeSet::new();
        for (code, _) in &rows {
            assert!(
                seen.insert(*code),
                "{name}: 同一code({code})の重複行は拒否: {rows:?}"
            );
        }
        // (1)code集合==固定期待集合{0,1,2,3,4,5,64}
        let codes: std::collections::BTreeSet<u32> = rows.iter().map(|(c, _)| *c).collect();
        assert_eq!(
            codes, expected_codes,
            "{name}: 終了code集合は固定期待集合{{0,1,2,3,4,5,64}}と完全一致"
        );
        tables.push(rows.into_iter().collect());
    }
    // (2)code→区分対応が両doc間で一致(codeと区分の交換を検出)
    assert_eq!(
        tables[0], tables[1],
        "code→区分の対応がusage_backup.mdとllm/usage.mdで一致"
    );
}

/// [covers:docs.pages-stdout-verbatim-from-repository]
/// docs usage・docs llm usage・docs llm skill・docs llm snippetは各正本
/// (docs/usage_backup.md〔version行置換後〕・docs/llm/usage.md・skill.md・snippet.md)の
/// 内容をstdoutへそのまま(byte一致)出力し exit 0。行追加・装飾なし。設定に非依存
/// (AMAZING_BACKUP_HOMEを空dirへ固定しても動作し、stderrに設定・S3系の表示なし)。
#[test]
fn docs_pages_output_repository_files_verbatim() {
    const USAGE_BACKUP_DOC: &str = include_str!("../docs/usage_backup.md");
    let expected_usage =
        expected_usage_doc_with_version(USAGE_BACKUP_DOC, env!("CARGO_PKG_VERSION"));
    for (name, expected) in [
        ("usage", expected_usage.as_str()),
        ("llm usage", LLM_USAGE_DOC),
        ("llm skill", LLM_SKILL_DOC),
        ("llm snippet", LLM_SNIPPET_DOC),
    ] {
        let args: &[&str] = match name {
            "usage" => &["docs", "usage"],
            "llm usage" => &["docs", "llm", "usage"],
            "llm skill" => &["docs", "llm", "skill"],
            _ => &["docs", "llm", "snippet"],
        };
        let (code, stdout, stderr) = run_docs(args);
        assert_eq!(code, 0, "docs {name} はexit 0: {stdout}{stderr}");
        assert_eq!(
            stdout, expected,
            "docs {name} のstdoutは正本とbyte一致(usageはversion置換後・行追加・装飾なし)"
        );
        assert!(
            stderr.is_empty(),
            "設定・S3系の表示なし(設定に非依存): {stderr}"
        );
    }
}

/// [covers:docs.usage-version-substituted]
/// integration側: docs usage出力の「実装バージョン:」行の値がbinary自身のversion
/// (clap --versionと同一source=CARGO_PKG_VERSION)と一致(正本から当該行が消えた・
/// 書式が変わって置換が作用しなくなったらfail)。
#[test]
fn docs_usage_output_version_line_matches_binary_version() {
    let (code, stdout, stderr) = run_docs(&["docs", "usage"]);
    assert_eq!(code, 0, "docs usageはexit 0: {stdout}{stderr}");
    let targets: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("- 実装バージョン:"))
        .collect();
    assert_eq!(targets.len(), 1, "出力中のversion行は1行: {targets:?}");
    assert_eq!(
        targets[0],
        format!("- 実装バージョン: {}", env!("CARGO_PKG_VERSION")),
        "version行の値はbinary自身のversion(--versionと同一source)と一致"
    );
}

/// [covers:docs.config-marker-replaced]
/// docs configは pipeline.example.toml全文+区切り行(`# ---- aws.toml ----`)+
/// aws.example.toml(marker `__CREDENTIALS_FILE__` を固定例示path
/// `~/.config/amazing-backup/aws-credentials.toml` へ置換したもの)の全文を出力する。
/// 出力中にmarker残存なし。期待値のaws側はconfig initと同一経路の変換関数
/// render_init_template(TASK-1実装)をtestから呼んで組み立てる(固定例示path・
/// XDG等の実行環境に依存しない)。
#[test]
fn docs_config_outputs_pipeline_and_marker_replaced_aws() {
    let aws_rendered = amazing_backup::config::render_init_template(
        AWS_EXAMPLE,
        Path::new("~/.config/amazing-backup/aws-credentials.toml"),
    )
    .expect("期待値のaws側組立(render_init_templateの適用)");
    let expected = format!("{PIPELINE_EXAMPLE}# ---- aws.toml ----\n{aws_rendered}");
    let (code, stdout, stderr) = run_docs(&["docs", "config"]);
    assert_eq!(code, 0, "docs configはexit 0: {stdout}{stderr}");
    assert_eq!(
        stdout, expected,
        "pipeline全文+区切り行+aws(marker置換後)全文とbyte一致"
    );
    assert!(
        !stdout.contains("__CREDENTIALS_FILE__"),
        "出力中にmarker残存なし: {stdout}"
    );
    assert!(
        stdout.contains("credentials_file = \"~/.config/amazing-backup/aws-credentials.toml\""),
        "全profileのcredentials_fileが固定例示pathへ置換されている: {stdout}"
    );
}

/// [covers:docs.llm-skill-skills-md-form]
/// docs llm skillの出力はSKILL.mdとして有効(design doc 3.6): 先頭行が`---`・
/// frontmatterが2つ目の`---`で閉じname: amazing-backupとtriggerを含むdescriptionを
/// 持つ・本文に`docs llm usage`参照とS3非干渉(aws CLI禁止)・索引・registryを更新する
/// 系の同時実行禁止の原則を含む(~/.claude/skills/amazing-backup/SKILL.mdへredirect
/// 配置可能な最終形)。
#[test]
fn docs_llm_skill_is_valid_skills_md() {
    let (code, stdout, stderr) = run_docs(&["docs", "llm", "skill"]);
    assert_eq!(code, 0, "docs llm skillはexit 0: {stdout}{stderr}");
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("---"), "先頭行はfrontmatter開始の`---`");
    let mut frontmatter: Vec<&str> = Vec::new();
    let mut body: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines {
        if !closed {
            if line.trim_end() == "---" {
                closed = true;
                continue;
            }
            frontmatter.push(line);
        } else {
            body.push(line);
        }
    }
    assert!(closed, "frontmatterは2つ目の`---`で閉じる: {stdout}");
    assert!(
        frontmatter
            .iter()
            .any(|l| l.trim_start().starts_with("name: amazing-backup")),
        "frontmatterにname: amazing-backup: {stdout}"
    );
    assert!(
        frontmatter
            .iter()
            .any(|l| l.trim_start().starts_with("description:")),
        "frontmatterにtriggerを含むdescription: {stdout}"
    );
    let body_text = body.join("\n");
    assert!(
        body_text.contains("docs llm usage"),
        "本文に`docs llm usage`参照(契約取得pointer): {stdout}"
    );
    assert!(
        body_text.contains("aws CLI"),
        "本文にS3非干渉の原則(aws CLI禁止): {stdout}"
    );
    assert!(
        body_text.contains("同時実行"),
        "本文に索引・registryを更新する系の同時実行禁止の原則: {stdout}"
    );
}

/// [covers:docs.llm-snippet-pointer-form]
/// docs llm snippetの出力はCLAUDE.md/AGENTS.mdへ追記して使うpointer型の数行で、
/// amazing-backup docs llm usage(契約)とdocs usage(手順)の参照を含む
/// (契約本文の内包なし=終了コード表等のllm usage本文要素を含まない短い出力)。
#[test]
fn docs_llm_snippet_is_short_pointer() {
    let (code, stdout, stderr) = run_docs(&["docs", "llm", "snippet"]);
    assert_eq!(code, 0, "docs llm snippetはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("docs llm usage"),
        "契約参照(docs llm usage)を含む: {stdout}"
    );
    assert!(
        stdout.contains("docs usage"),
        "手順参照(docs usage)を含む: {stdout}"
    );
    let line_count = stdout.lines().count();
    assert!(
        line_count <= 8,
        "pointer型の短い出力(数行)である: {line_count}行: {stdout}"
    );
    assert!(
        !stdout.contains("| 0 |"),
        "契約本文(終了コード表)の内包なし: {stdout}"
    );
}

/// [covers:docs.page-list-no-args]
/// docs引数なしはページ一覧(usage/config/llm usage/llm skill/llm snippetの各説明)を
/// stdoutへ出力し exit 0(design doc 3.7の設計済み一覧)。docs llm引数なしはllm配下
/// 3ページの一覧+exit 0。
#[test]
fn docs_no_args_lists_pages() {
    let (code, stdout, stderr) = run_docs(&["docs"]);
    assert_eq!(code, 0, "docs(引数なし)はexit 0: {stdout}{stderr}");
    // 各ページ名とその説明が同じ行に並ぶ(設計3.7の一覧形式)
    for (page, desc) in [
        ("usage", "使用手順書"),
        ("config", "設定"),
        ("llm usage", "LLM"),
        ("llm skill", "SKILL.md"),
        ("llm snippet", "追記"),
    ] {
        assert!(
            stdout.lines().any(|l| l.contains(page) && l.contains(desc)),
            "ページ({page})とその説明({desc})を含む行がある: {stdout}"
        );
    }

    let (code, stdout, stderr) = run_docs(&["docs", "llm"]);
    assert_eq!(code, 0, "docs llm(引数なし)はexit 0: {stdout}{stderr}");
    for (page, desc) in [("usage", "LLM"), ("skill", "SKILL.md"), ("snippet", "追記")] {
        assert!(
            stdout.lines().any(|l| l.contains(page) && l.contains(desc)),
            "llm配下ページ({page})とその説明({desc})を含む行がある: {stdout}"
        );
    }
}

/// [covers:docs.invalid-syntax-usage-error]
/// docs系の構文誤りはusage error 64で本文(正本)を出力しない: 未知ページ
/// (docs unknown・docs llm unknown)と余計な引数(docs usage extra・docs config --flag)
/// はいずれもclapのInvalidSubcommand/TooManyValues等をcli::runの64変換経路で終了する。
/// docsコマンド実装済みの前提(正常系がexit 0)を先に確認する(実装前はこの前提が
/// failする=テスト先行の赤)。
#[test]
fn docs_invalid_syntax_is_usage_error_64() {
    let (ok, ok_stdout, ok_stderr) = run_docs(&["docs", "usage"]);
    assert_eq!(
        ok, 0,
        "前提: docs usageはexit 0(docsコマンド実装済み): {ok_stdout}{ok_stderr}"
    );
    let body_head = ok_stdout.lines().next().unwrap_or_default();
    for args in [
        &["docs", "unknown"][..],
        &["docs", "llm", "unknown"][..],
        &["docs", "usage", "extra"][..],
        &["docs", "config", "--flag"][..],
    ] {
        let (code, stdout, stderr) = run_docs(args);
        assert_eq!(
            code, 64,
            "{args:?} はusage error 64(未知ページ・余計な引数の拒否): {stdout}{stderr}"
        );
        assert!(
            !stderr.trim().is_empty(),
            "{args:?} はusage error表示(stderr)あり: {stderr}"
        );
        assert!(
            !stdout.contains(body_head),
            "{args:?} は正本本文をstdoutへ出力しない: {stdout}"
        );
    }
}

/// [covers:docs.output-write-error-exit-4]
/// 書き込み失敗のうちBrokenPipe(`docs usage | head`等の先読み終了)は正常終了扱い:
/// exit 0かつエラー表示なし(bin実stdoutへの契約。相手側をcloseしたsocketpairを
/// stdoutへ渡してwriteをEPIPEで失敗させる。Rust runtimeはSIGPIPEをignoreするため
/// write_allはErr(BrokenPipe)を返す。TASK-2 completionsと同一手法)。
#[test]
fn docs_usage_broken_pipe_is_silent_exit_0() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    let tmp = TestDir::new("docs-epipe");
    let home = tmp.path().display().to_string();
    let (ours, theirs) = UnixStream::pair().expect("socketpair");
    drop(theirs); // 相手側close→childのstdoutへの書込はEPIPE
    let fd: OwnedFd = ours.into();
    let bin = env!("CARGO_BIN_EXE_amazing-backup");
    let out = std::process::Command::new(bin)
        .args(["docs", "usage"])
        .env("AMAZING_BACKUP_HOME", &home)
        .stdout(std::process::Stdio::from(std::fs::File::from(fd)))
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("amazing-backup bin 実行");
    assert_eq!(
        out.status.code(),
        Some(0),
        "BrokenPipeは正常終了扱い(exit 0): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "BrokenPipe時のエラー表示は行わない: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// [covers:docs.output-write-error-exit-4]
/// BrokenPipe以外の書き込み失敗(ENOSPC等)はstderrへエラー表示+exit 4(bin実stdout
/// への契約。/dev/fullはwriteをENOSPCで失敗させる。TASK-2 completionsと同一手法)。
#[test]
fn docs_usage_enospc_prints_error_and_exits_4() {
    let tmp = TestDir::new("docs-enospc");
    let home = tmp.path().display().to_string();
    // /dev/fullはwrite権限でopenする(読み取り専用fdでは書込失敗がstdのstdout経路で
    // 表面化しないため。write(2)は常にENOSPCを返す)
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full open(Linux)");
    let bin = env!("CARGO_BIN_EXE_amazing-backup");
    let out = std::process::Command::new(bin)
        .args(["docs", "usage"])
        .env("AMAZING_BACKUP_HOME", &home)
        .stdout(std::process::Stdio::from(full))
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("amazing-backup bin 実行");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code(),
        Some(4),
        "ENOSPC書込失敗はexit 4(処理エラー): {stderr}"
    );
    assert!(stderr.contains("error"), "stderrへエラー表示: {stderr}");
}
