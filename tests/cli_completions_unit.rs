//! TASK-2 completionsコマンドのunit層test(設計tmp/tasks/tm-2/design_completions.md
//! D-1・D-3・D-5: cmd_completions(shell, &mut dyn Write)シグネチャ規定に依存し、
//! 偽Writeを注入して生成内容と書き込み失敗の取扱いを検証する)。
//! テスト先行のため実装前はcompile error(cmd_completions・CompletionShell未実装)を許容。
//! integration層(bin subprocess起動)はtests/cli_bin.rs。

use amazing_backup::cli::{CompletionShell, cmd_completions};

/// writeが常にErr(kind)を返す偽Write(書き込み失敗の注入用。
/// write_allは単一呼出のため初回のErrがそのまま返る)。
struct FailingWrite(std::io::ErrorKind);

impl std::io::Write for FailingWrite {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(self.0))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// [covers:cli.completions-generates-shells]
/// cmd_completionsは各shell(bash/zsh/fish)の補完scriptを注入したWriteへ出力し0を返す
/// (unit層・Vec<u8>注入。構造マーカー: bash=_amazing__backup関数定義(clap_completeの
/// bash generatorはbin nameの`-`を`__`へ置換する規約)・zsh=#compdef amazing-backup行・
/// fish=complete -c amazing-backup行。設定読込を行わない純粋出力のためABH環境は無関係)。
#[test]
fn cmd_completions_writes_each_shell_script_to_injected_write() {
    for (shell, marker) in [
        (CompletionShell::Bash, "_amazing__backup"),
        (CompletionShell::Zsh, "#compdef amazing-backup"),
        (CompletionShell::Fish, "complete -c amazing-backup"),
    ] {
        let mut buf: Vec<u8> = Vec::new();
        let code = cmd_completions(shell, &mut buf);
        assert_eq!(code, 0, "exit 0");
        let script = String::from_utf8_lossy(&buf).into_owned();
        assert!(!script.trim().is_empty(), "補完scriptをWriteへ出力");
        assert!(
            script.contains(marker),
            "構造マーカー({marker})を含む: 先頭={}",
            script.chars().take(120).collect::<String>()
        );
    }
}

/// [covers:cli.completions-output-write-error-exit-4]
/// 書き込み失敗のうちBrokenPipe(completions zsh | head 等の先読み終了で常に起きる
/// 通常運用)は正常終了扱い(exit 0)。表示なしの契約はunit層では観測できず
/// (cmd_completionsのエラー出力はeprintln!=process stderrのため)、integration層
/// (tests/cli_bin.rs)がbin stderrを捕捉して検証する。unit層は戻り値を検証する。
#[test]
fn cmd_completions_broken_pipe_write_returns_zero() {
    let mut w = FailingWrite(std::io::ErrorKind::BrokenPipe);
    let code = cmd_completions(CompletionShell::Zsh, &mut w);
    assert_eq!(code, 0, "BrokenPipeは正常終了扱い(exit 0)");
}

/// [covers:cli.completions-output-write-error-exit-4]
/// BrokenPipe以外の書き込み失敗(ENOSPC等)は処理エラー区分の4を返す(stderrへの
/// エラー表示の契約はunit層では観測できず、integration層(tests/cli_bin.rs)が
/// /dev/full接続のbin stderr捕捉で検証する。unit層はexit codeで検証)。
#[test]
fn cmd_completions_other_write_error_returns_four() {
    let mut w = FailingWrite(std::io::ErrorKind::Other);
    let code = cmd_completions(CompletionShell::Zsh, &mut w);
    assert_eq!(code, 4, "その他の書き込み失敗はexit 4");
}
