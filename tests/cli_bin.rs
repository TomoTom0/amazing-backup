//! CLI entrypoint系の結合test(設計8章CLI-002/004・TASK-11 Phase B・TASK-2 completions)。
//! env!("CARGO_BIN_EXE_amazing-backup")でbinを直接実行して検証する。
//! TASK-11(Phase B)で旧サブコマンド(plan/encrypt/upload/verify/restore)は削除済み。
//! TASK-2(completionsコマンド)のintegration層も本ファイル(語彙は候補定義からの抽出
//! 比較・書込失敗のstderr契約は/dev/full・相手close socketpairで検証。
//! unit層はtests/cli_completions_unit.rs)。

mod common;

use common::*;

/// [covers:cli.version-flag]
/// --versionでversion表示・exit 0となる。
#[test]
fn version_flag_prints_and_exits_zero() {
    let (code, stdout, stderr) = run_bin(&["--version"]);
    assert_eq!(code, 0, "exit 0: {stderr}");
    assert!(
        stdout.contains("amazing-backup"),
        "version文字列にpackage名を含む: {stdout}"
    );
    assert!(!stdout.trim().is_empty(), "stdoutへ表示: {stdout}");
}

/// [covers:cli.module-entrypoint-parity]
/// 同一引数をlib経由(cli::run直接呼出)とbin amazing-backupで実行すると同一の終了コードを返す
/// (TASK-11 Phase B: 旧plan経路の代表ケースはbackup pushの対象指定へ読み替え)。
#[test]
fn bin_and_lib_entrypoint_parity() {
    let tmp = TestDir::new("cli-parity");
    let bad_cfg = tmp.path().join("pipeline.toml");
    write_file(
        &bad_cfg,
        b"[recipients]\npublic_keys = []\n[behavior]\nkeep_going = false\n",
    );
    // 対象エラー3用の正常pipeline(実在する鍵)とaws.toml
    let (public, _secret) = generate_ed25519_keypair(tmp.path(), "k1");
    let ok_cfg = tmp.path().join("pipeline-ok.toml");
    write_file(
        &ok_cfg,
        format!(
            "[recipients]\npublic_keys = [\"{}\"]\n[behavior]\nkeep_going = false\n",
            public.display()
        )
        .as_bytes(),
    );
    let creds = tmp.path().join("creds.toml");
    write_file(
        &creds,
        b"[credentials]\naccess_key_id = \"AKITEST0000000000000A\"\nsecret_access_key = \"parity-not-real\"\n",
    );
    let aws_toml = tmp.path().join("aws.toml");
    write_file(
        &aws_toml,
        format!(
            "[aws]\nactive_profile = \"dev\"\n\n[aws.profiles.dev]\npurpose = \"dev\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{}\"\n",
            creds.display()
        )
        .as_bytes(),
    );

    let cases: Vec<Vec<String>> = vec![
        // 設定エラー2(不正なpipeline.toml: public_keys空でのbackup push)
        vec![
            "backup".into(),
            "push".into(),
            "--config".into(),
            bad_cfg.display().to_string(),
            "x.zip".into(),
        ],
        // 対象エラー3(不在ファイルでのbackup push)
        vec![
            "backup".into(),
            "push".into(),
            "--config".into(),
            ok_cfg.display().to_string(),
            "--aws-config".into(),
            aws_toml.display().to_string(),
            tmp.path().join("absent.zip").display().to_string(),
        ],
        // usage誤り64
        vec!["--no-such-option".into()],
        // version(0)
        vec!["--version".into()],
    ];

    for case in cases {
        let mut bin_args: Vec<&str> = vec!["amazing-backup"];
        bin_args.extend(case.iter().map(|s| s.as_str()));
        let (bin_code, stdout, stderr) = run_bin(&bin_args[1..]);

        let mut lib_argv: Vec<std::ffi::OsString> = vec!["amazing-backup".into()];
        lib_argv.extend(case.iter().map(|s| std::ffi::OsString::from(s)));
        let lib_code = amazing_backup::cli::run(lib_argv);

        assert_eq!(
            bin_code, lib_code,
            "parity: args={case:?} bin_out={stdout} bin_err={stderr}"
        );
        // 出力も(observableなstdoutについて)空/非空の一致を概略検証
        if bin_code == 0 && bin_args[1] == "--version" {
            assert!(!stdout.trim().is_empty());
        }
    }
}

/// [covers:cli.old-subcommands-removed]
/// 旧サブコマンド(plan/encrypt/upload/verify/restore=restore request/status/download含む)は
/// 削除され、未知サブコマンドとしてusage error 64となる。--helpの文言に旧サブコマンドが
/// 現れない(TASK-11 Phase B)。
#[test]
fn old_subcommands_are_removed() {
    for sub in ["plan", "encrypt", "upload", "verify", "restore"] {
        let (code, stdout, stderr) = run_bin(&[sub, "--help"]);
        assert_eq!(
            code, 64,
            "旧サブコマンド{sub}は未知サブコマンド扱いのusage error 64: {stdout}{stderr}"
        );
    }
    for args in [
        vec!["restore", "request", "--date", "2026-10-01"],
        vec!["restore", "status", "--date", "2026-10-01"],
        vec!["restore", "download", "--date", "2026-10-01"],
    ] {
        let (code, stdout, stderr) = run_bin(&args);
        assert_eq!(
            code, 64,
            "旧restore系({args:?})はusage error 64: {stdout}{stderr}"
        );
    }

    // --help文言に旧サブコマンドが現れない
    let (code, stdout, stderr) = run_bin(&["--help"]);
    assert_eq!(code, 0, "--helpはexit 0: {stderr}");
    for sub in ["plan", "encrypt", "upload", "restore"] {
        assert!(
            !stdout.contains(sub),
            "--helpの文言に旧サブコマンド({sub})は現れない: {stdout}"
        );
    }
    // 新体系は現れる(集約先の代表)
    assert!(
        stdout.contains("backup"),
        "backupは--helpに現れる: {stdout}"
    );
    assert!(stdout.contains("get"), "getは--helpに現れる: {stdout}");
}

/// [covers:cli.restore-out-required-usage-error]
/// get/backup pullの--out(出力dir)は必須(TASK-19 XDG配置): 省略時はusage error 64
/// (引数検査は設定読込・S3 API呼び出しより前)。旧既定data/restoreは廃止。
#[test]
fn restore_out_option_is_required_usage_error() {
    for (tag, args) in [
        ("get --outなし", vec!["get", "a.zip"]),
        (
            "backup pull --outなし",
            vec!["backup", "pull", "--all", "--yes"],
        ),
    ] {
        let (code, stdout, stderr) = run_bin(&args);
        assert_eq!(
            code, 64,
            "{tag}はusage error 64(設定読込より前): {stdout}{stderr}"
        );
        assert!(stderr.contains("--out"), "{tag}: --out必須の表示: {stderr}");
    }
}

/// [covers:cli.completions-generates-shells]
/// completions <shell>(bash/zsh/fish)は対応shellの補完scriptをstdoutへ出力しexit 0。
/// 各shellの構造マーカーを含む(bash: _amazing__backup関数定義(clap_completeのbash
/// generatorはbin nameの`-`を`__`へ置換する規約)・zsh: #compdef行・fish: complete -c行)。
/// 設定に非依存: AMAZING_BACKUP_HOMEを空dirへ固定
/// (規定pathにpipeline.toml・aws.toml無し)でも動作し、設定・S3系の表示は出ない。
#[test]
fn completions_generates_each_shell_script() {
    let tmp = TestDir::new("cli-completions-gen");
    let home = tmp.path().display().to_string();
    for (shell, marker) in [
        ("bash", "_amazing__backup"),
        ("zsh", "#compdef amazing-backup"),
        ("fish", "complete -c amazing-backup"),
    ] {
        let (code, stdout, stderr) =
            run_bin_env(&["completions", shell], &[("AMAZING_BACKUP_HOME", &home)]);
        assert_eq!(code, 0, "completions {shell}はexit 0: {stdout}{stderr}");
        assert!(!stdout.trim().is_empty(), "stdoutへ補完scriptを出力");
        assert!(
            stdout.contains(marker),
            "構造マーカー({marker})を含む: 先頭={}",
            stdout.chars().take(120).collect::<String>()
        );
        assert!(
            stderr.trim().is_empty(),
            "stderrに設定・S3系のエラー・警告は出ない: {stderr}"
        );
    }
}

/// [covers:cli.completions-script-covers-current-vocabulary]
/// 生成される補完scriptの語彙は現行Command定義と一致する。比較は全体部分文字列contains
/// (候補行が欠落しても語が関数名等の別箇所に残っていれば通過する=検出力不足)ではなく、
/// script側の候補定義から抽出した語彙と期待語彙(Cli::command()から実行時取得=固定の
/// 期待リストに頼らないため語彙追加・改名時に自動fail=追随を強制)の語一致で行う:
/// - zsh: コマンド階層ごとに比較。subcommand候補は`..._commands`関数の
///   `local commands; commands=( ... )`配列(関数名`_amazing-backup__subcmd__backup`等から
///   階層pathを復元)・optionは`_arguments`指定行(`case $line[..] in`のbranch labelで
///   pathを追跡しnode帰属を付けた`'--name[...]'`/`'--name=[...]'`行)
/// - bash: `opts="語列"`行の語と期待subcommand名・`--option`名の語一致
/// - fish: `-a "値"`(subcommand候補)・`-l 名`(long option)の語一致
/// 廃止語(plan/encrypt/upload/restore)はサブコマンド候補位置の語として含まれない
/// ことを検証する(help説明文中の英語流入〔backup pushの説明「暗号化→upload→…」等〕は
/// 語彙ではないため対象外。verifyはbackup verify子コマンドとして現行scriptに現れるため
/// 廃止語から除外)。抽出結果が空になる生成形式変更で黙過しないよう、各shellで既知候補の
/// 抽出確認も行う。
#[test]
fn completions_script_covers_current_vocabulary() {
    use amazing_backup::cli::Cli;
    use clap::{Command, CommandFactory};
    use std::collections::HashMap;

    let tmp = TestDir::new("cli-completions-vocab");
    let home = tmp.path().display().to_string();

    // ---- 期待側: Cli::command()からコマンド階層ごとの語彙を収集 -----------------
    // node = (path: 先頭からのsubcommand名列, subs: 直下の非hidden subcommand名,
    //         longs: 当該node自身の非hidden long option名)。hidden subtreeは対象外。
    fn collect_nodes(
        cmd: &Command,
        path: &[String],
        out: &mut Vec<(Vec<String>, Vec<String>, Vec<String>)>,
    ) {
        let subs: Vec<String> = cmd
            .get_subcommands()
            .filter(|s| !s.is_hide_set())
            .map(|s| s.get_name().to_string())
            .collect();
        let longs: Vec<String> = cmd
            .get_arguments()
            .filter(|a| !a.is_hide_set())
            .filter_map(|a| a.get_long().map(str::to_string))
            .collect();
        out.push((path.to_vec(), subs, longs));
        for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
            let mut p = path.to_vec();
            p.push(sub.get_name().to_string());
            collect_nodes(sub, &p, out);
        }
    }
    let mut nodes = Vec::new();
    collect_nodes(&Cli::command(), &[], &mut nodes);
    // 期待側の非空担保(走査方式の変更で空になった場合の黙過防止)
    assert!(
        nodes
            .iter()
            .any(|(p, subs, _)| p.is_empty() && subs.iter().any(|s| s == "completions")),
        "現行語彙にcompletionsが含まれる(実装後のCommand定義が取得できること)"
    );
    let all_longs: Vec<&str> = nodes
        .iter()
        .flat_map(|(_, _, ls)| ls.iter())
        .map(String::as_str)
        .collect();
    for anchor in ["config", "out", "format"] {
        assert!(
            all_longs.contains(&anchor),
            "期待option語彙に--{anchor}が含まれる(走査の非空担保)"
        );
    }

    // ---- zsh: 階層ごとの候補語彙抽出 ------------------------------------------
    let (code, zsh, stderr) =
        run_bin_env(&["completions", "zsh"], &[("AMAZING_BACKUP_HOME", &home)]);
    assert_eq!(code, 0, "completions zshはexit 0: {zsh}{stderr}");

    /// `..._commands`関数名からnode pathを復元(`__subcmd__`区切り・先頭のbin節を除く)。
    fn commands_fn_path(fn_name: &str) -> Vec<String> {
        let body = fn_name.strip_prefix('_').unwrap_or(fn_name);
        let body = body.strip_suffix("_commands").unwrap_or(body);
        let mut parts = body.split("__subcmd__");
        let _bin = parts.next();
        parts.map(str::to_string).collect()
    }

    // subcommand候補: 各nodeの`..._commands`関数内 `local commands; commands=( ... )` 配列
    let mut zsh_subs: HashMap<Vec<String>, Vec<String>> = HashMap::new();
    {
        let mut fn_name: Option<String> = None;
        let mut in_commands = false;
        for line in zsh.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_suffix("() {") {
                fn_name = Some(rest.to_string());
                in_commands = false;
                continue;
            }
            if t == "}" {
                fn_name = None;
                in_commands = false;
                continue;
            }
            let Some(f) = fn_name.as_deref() else {
                continue;
            };
            if t.starts_with("local commands; commands=(") {
                in_commands = f.ends_with("_commands");
                continue;
            }
            if in_commands {
                if t.starts_with(')') {
                    in_commands = false;
                } else if let Some(rest) = t.strip_prefix('\'') {
                    if let Some(name) = rest.split(':').next() {
                        if !name.is_empty() {
                            zsh_subs
                                .entry(commands_fn_path(f))
                                .or_default()
                                .push(name.to_string());
                        }
                    }
                }
            }
        }
    }

    // option語彙: `_arguments`指定行を`case $line[..] in`のbranch labelでpath追跡して
    // node帰属を付ける(`case $state in`のlabelはpathに寄与しない)。blockは`&& ret=0`で閉じる
    let mut zsh_opts: HashMap<Vec<String>, Vec<String>> = HashMap::new();
    {
        enum Frame {
            /// `case $line[N] in`のframe。branch labelがpath構成要素になる
            Component(Option<String>),
            /// `case $state in`のframe。labelはpathに寄与しない
            State,
        }
        let mut frames: Vec<Frame> = Vec::new();
        let mut current: Option<(Vec<String>, Vec<String>)> = None;
        for line in zsh.lines() {
            let t = line.trim();
            match t {
                t if t.starts_with("case $line[") && t.ends_with(" in") => {
                    frames.push(Frame::Component(None));
                    continue;
                }
                "case $state in" => {
                    frames.push(Frame::State);
                    continue;
                }
                "esac" => {
                    frames.pop();
                    current = None;
                    continue;
                }
                t if t.starts_with('(') && t.ends_with(')') => {
                    if let Some(Frame::Component(label)) = frames.last_mut() {
                        *label = Some(t[1..t.len() - 1].to_string());
                    }
                    continue;
                }
                _ => {}
            }
            if t.starts_with("_arguments") {
                let path: Vec<String> = frames
                    .iter()
                    .filter_map(|f| match f {
                        Frame::Component(Some(l)) => Some(l.clone()),
                        _ => None,
                    })
                    .collect();
                current = Some((path, Vec::new()));
                continue;
            }
            if let Some(rest) = t.strip_prefix('\'') {
                if let Some(name) = rest
                    .strip_prefix("--")
                    .and_then(|r| r.split(['[', '=']).next())
                {
                    if let Some((_, opts)) = current.as_mut() {
                        if !name.is_empty() {
                            opts.push(format!("--{name}"));
                        }
                    }
                }
                continue;
            }
            if t.starts_with("&& ret=0") {
                if let Some((p, opts)) = current.take() {
                    if !opts.is_empty() {
                        zsh_opts.entry(p).or_default().extend(opts);
                    }
                }
            }
        }
    }

    // ---- zsh比較: 抽出の非空担保(既知候補)→階層ごとの期待語彙包含 ----------------
    let root_subs = zsh_subs.get(&Vec::new()).cloned().unwrap_or_default();
    for known in ["completions", "backup", "help"] {
        assert!(
            root_subs.iter().any(|s| s == known),
            "zshのroot subcommand候補に{known}が含まれる(抽出の非空担保)"
        );
    }
    let root_opts = zsh_opts.get(&Vec::new()).cloned().unwrap_or_default();
    assert!(
        root_opts.iter().any(|o| o == "--help"),
        "zshのroot option語彙に--helpが含まれる(抽出の非空担保)"
    );
    for (path, exp_subs, exp_longs) in &nodes {
        if !exp_subs.is_empty() {
            let Some(act_subs) = zsh_subs.get(path) else {
                panic!("zsh scriptにnode {path:?}のsubcommand候補定義が無い(生成形式変更の疑い)");
            };
            for s in exp_subs {
                assert!(
                    act_subs.iter().any(|a| a == s),
                    "zsh node {path:?}のsubcommand候補に{s}が無い(候補としての一致)"
                );
            }
        }
        if !exp_longs.is_empty() {
            let Some(act_opts) = zsh_opts.get(path) else {
                panic!("zsh scriptにnode {path:?}のoption定義が無い(生成形式変更の疑い)");
            };
            for l in exp_longs {
                let wanted = format!("--{l}");
                assert!(
                    act_opts.iter().any(|a| *a == wanted),
                    "zsh node {path:?}のoption語彙に--{l}が無い(候補としての一致)"
                );
            }
        }
    }

    // ---- bash: opts="語列"との語一致 -------------------------------------------
    let (code, bash, stderr) =
        run_bin_env(&["completions", "bash"], &[("AMAZING_BACKUP_HOME", &home)]);
    assert_eq!(code, 0, "completions bashはexit 0: {bash}{stderr}");
    let bash_words: Vec<&str> = bash
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            l.starts_with("opts=")
                .then(|| l.split('"').nth(1).unwrap_or(""))
        })
        .flat_map(str::split_whitespace)
        .collect();
    for known in ["completions", "backup", "--help", "--version"] {
        assert!(
            bash_words.contains(&known),
            "bashのopts語列に{known}が含まれる(抽出の非空担保)"
        );
    }
    for (_, exp_subs, exp_longs) in &nodes {
        for s in exp_subs {
            assert!(
                bash_words.contains(&s.as_str()),
                "bashの候補語列にsubcommand語彙({s})が無い(語としての一致)"
            );
        }
        for l in exp_longs {
            let wanted = format!("--{l}");
            assert!(
                bash_words.contains(&wanted.as_str()),
                "bashの候補語列にoption語彙(--{l})が無い(語としての一致)"
            );
        }
    }

    // ---- fish: -a値(subcommand候補)・-l名(long option)との語一致 -----------------
    let (code, fish, stderr) =
        run_bin_env(&["completions", "fish"], &[("AMAZING_BACKUP_HOME", &home)]);
    assert_eq!(code, 0, "completions fishはexit 0: {fish}{stderr}");
    let mut fish_subs: Vec<&str> = Vec::new();
    let mut fish_longs: Vec<&str> = Vec::new();
    for line in fish.lines() {
        if let Some((_, rest)) = line.split_once(" -a \"") {
            if let Some(value) = rest.split('"').next() {
                fish_subs.extend(value.split_whitespace());
            }
        }
        let mut it = line.split_whitespace();
        while let Some(tok) = it.next() {
            if tok == "-l" {
                if let Some(name) = it.next() {
                    fish_longs.push(name);
                }
            }
        }
    }
    for known in ["completions", "backup"] {
        assert!(
            fish_subs.contains(&known),
            "fishの-a候補値に{known}が含まれる(抽出の非空担保)"
        );
    }
    assert!(
        fish_longs.contains(&"help"),
        "fishの-l語彙にhelpが含まれる(抽出の非空担保)"
    );
    for (path, exp_subs, exp_longs) in &nodes {
        for s in exp_subs {
            assert!(
                fish_subs.contains(&s.as_str()),
                "fishの-a候補値にsubcommand語彙({s})が無い(語としての一致)"
            );
        }
        // fishのoption語彙は深さ2まで(path長2以下のnode)のみ比較する: clap_completeの
        // fish generatorは深さ3以上のnode(manage location rm等)のoption行を生成しない
        // ため(生成物の構造上の制約。subcommand候補は全階層で生成される)
        if path.len() <= 2 {
            for l in exp_longs {
                assert!(
                    fish_longs.contains(&l.as_str()),
                    "fishの-l語彙にoption語彙({l})が無い(語としての一致)"
                );
            }
        }
    }

    // ---- 旧トップレベル語彙の回帰検出 -----------------------------------------
    // 廃止語はサブコマンド候補位置の語として現れない(help説明文中の英語流入は語彙で
    // ないため対象外。上記の非空担保により抽出失敗時は黙過しない)
    let zsh_all_candidates: Vec<&str> = zsh_subs
        .values()
        .flat_map(|v| v.iter())
        .map(String::as_str)
        .collect();
    for old in ["plan", "encrypt", "upload", "restore"] {
        assert!(
            !zsh_all_candidates.contains(&old),
            "廃止語({old})はzshのサブコマンド候補に現れない"
        );
        assert!(
            !bash_words.contains(&old),
            "廃止語({old})はbashの候補語列に現れない"
        );
        assert!(
            !fish_subs.contains(&old),
            "廃止語({old})はfishのサブコマンド候補に現れない"
        );
    }
}

/// [covers:cli.completions-invalid-shell-usage-error]
/// shell引数はbash/zsh/fishのみ受理(D-2): 対象外shell(elvish/powershell)・誤字(zshh)は
/// usage error 64(InvalidValue: 可能値bash/zsh/fishの列挙付きusage表示。
/// cli.usage-error-exit-64と同じcli::runの64変換経路)。補完scriptの出力は行われない。
#[test]
fn completions_invalid_shell_is_usage_error_64() {
    for bad in ["elvish", "powershell", "zshh"] {
        let (code, stdout, stderr) = run_bin(&["completions", bad]);
        assert_eq!(
            code, 64,
            "completions {bad}(対象外・誤字)はusage error 64: {stdout}{stderr}"
        );
        assert!(
            stdout.trim().is_empty(),
            "補完scriptの出力は行われない: {stdout}"
        );
        assert!(
            stderr.contains("invalid value"),
            "invalid value表示: {stderr}"
        );
        assert!(
            stderr.contains("possible values"),
            "可能値の列挙表示: {stderr}"
        );
        for v in ["bash", "zsh", "fish"] {
            assert!(stderr.contains(v), "可能値({v})の列挙: {stderr}");
        }
    }
}

/// [covers:cli.completions-missing-shell-usage-error]
/// shell引数省略はusage error 64(MissingRequiredArgument: 必須引数・usage表示)。
/// 不正値(InvalidValue)とは表示が異なり、possible valuesの列挙は含まれない。
#[test]
fn completions_missing_shell_is_usage_error_64() {
    let (code, stdout, stderr) = run_bin(&["completions"]);
    assert_eq!(code, 64, "引数省略はusage error 64: {stdout}{stderr}");
    assert!(
        stdout.trim().is_empty(),
        "補完scriptの出力は行われない: {stdout}"
    );
    assert!(
        stderr.contains("<SHELL>"),
        "必須引数のusage error表示: {stderr}"
    );
    assert!(
        !stderr.contains("possible values"),
        "省略時のusage表示に可能値の列挙は含まれない(不正値時との表示差): {stderr}"
    );
}

/// [covers:cli.completions-help-listed]
/// 可視性: トップレベル--helpのsubcommand一覧にcompletionsが掲載され、
/// completions --helpに<SHELL>引数の可能値bash/zsh/fishが表示される
/// (発見可能性のためhiddenにしない)。
#[test]
fn completions_listed_in_help() {
    let (code, stdout, stderr) = run_bin(&["--help"]);
    assert_eq!(code, 0, "--helpはexit 0: {stderr}");
    assert!(
        stdout.contains("completions"),
        "トップレベル--helpのsubcommand一覧にcompletions掲載: {stdout}"
    );

    let (code, stdout, stderr) = run_bin(&["completions", "--help"]);
    assert_eq!(code, 0, "completions --helpはexit 0: {stderr}");
    assert!(stdout.contains("possible values"), "可能値の表示: {stdout}");
    for v in ["bash", "zsh", "fish"] {
        assert!(stdout.contains(v), "可能値({v})の表示: {stdout}");
    }
}

/// [covers:cli.completions-output-write-error-exit-4]
/// 書き込み失敗のうちBrokenPipe(`completions zsh | head`等の先読み終了)は正常終了扱い:
/// exit 0かつエラー表示なし(bin実stdoutへの契約。cmd_completionsのerror出力はeprintln!
/// =process stderrのため、unit層(偽Write注入)は戻り値のみ検証し、表示内容は本層で
/// bin subprocessのstderrを捕捉して検証する)。相手側をcloseしたsocketpairをstdoutへ
/// 渡してwriteをEPIPEで失敗させる(Rust runtimeはSIGPIPEをignoreするためwrite_allは
/// Err(BrokenPipe)を返す。出力sizeに関係なく決定的)。
#[test]
fn completions_broken_pipe_is_silent_exit_0() {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    let tmp = TestDir::new("cli-completions-epipe");
    let home = tmp.path().display().to_string();
    let (ours, theirs) = UnixStream::pair().expect("socketpair");
    drop(theirs); // 相手側close→childのstdoutへの書込はEPIPE
    let fd: OwnedFd = ours.into();
    let bin = env!("CARGO_BIN_EXE_amazing-backup");
    let out = std::process::Command::new(bin)
        .args(["completions", "zsh"])
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

/// [covers:cli.completions-output-write-error-exit-4]
/// BrokenPipe以外の書き込み失敗(ENOSPC等)はstderrへエラー表示+exit 4(bin実stdoutへの
/// 契約。/dev/fullはwriteをENOSPCで失敗させる。表示内容の検証はunit層でなく本層の
/// bin subprocess stderr捕捉による=cmd_completionsのeprintln!出力の実挙動)。
#[test]
fn completions_other_write_error_prints_stderr_and_exits_4() {
    let tmp = TestDir::new("cli-completions-enospc");
    let home = tmp.path().display().to_string();
    // /dev/fullはwrite権限でopenする(読み取り専用fdでは書込失敗がstdのstdout経路で
    // 表面化しないため。write(2)は常にENOSPCを返す)
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("/dev/full open(Linux)");
    let bin = env!("CARGO_BIN_EXE_amazing-backup");
    let out = std::process::Command::new(bin)
        .args(["completions", "zsh"])
        .env("AMAZING_BACKUP_HOME", &home)
        .stdout(std::process::Stdio::from(full))
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("amazing-backup bin 実行");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code(),
        Some(4),
        "ENOSPC書込失敗はexit 4: {stderr}"
    );
    assert!(
        stderr.contains("error:"),
        "stderrへエラー表示(error:前置): {stderr}"
    );
    assert!(
        stderr.contains("補完script"),
        "stderrのエラー文言が補完script出力の失敗を示す: {stderr}"
    );
}
