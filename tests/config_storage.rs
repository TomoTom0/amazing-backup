//! config エリア(TASK-18・tmp/tasks/tm-18/design_key-uuid-versioning.md 5.15・2.2・7章)の
//! 結合test。TASK-18追加分([metadata]3値・[categories]拒否・[aws.lifecycle]必須/範囲)と
//! 改訂2件([categories]廃止・from-list title欄)をbin subprocessの終了コードで観測する
//! (src/config.rs/src/targets.rsのunit testは実装時に追従。テスト先行のため実行failは
//! 許容・cargo test --no-runは通る)。

mod common;

use common::*;

/// [covers:config.metadata-latest-source-path-values]
/// [metadata].latest_source_pathは列挙値 "plain"(既定)|"encrypted"|"none"(TASK-18 2.2)。
/// その他の値は設定エラー2。キー・セクション省略時はplain。
#[test]
fn metadata_latest_source_path_config_values() {
    // (値, 期待exit code。dry-runは新規発行予測でもtitle無し警告のみでexit 0)
    let cases: &[(&str, &str, i32)] = &[
        ("plain", "[metadata]\nlatest_source_path = \"plain\"\n", 0),
        (
            "encrypted",
            "[metadata]\nlatest_source_path = \"encrypted\"\n",
            0,
        ),
        ("none", "[metadata]\nlatest_source_path = \"none\"\n", 0),
        (
            "monthly(不正値)",
            "[metadata]\nlatest_source_path = \"monthly\"\n",
            2,
        ),
        ("キー省略", "[metadata]\n", 0),
    ];
    for (i, (tag, section, want)) in cases.iter().enumerate() {
        let Some(fx) = StorageFixture::new(
            &format!("cfg-meta-{i}"),
            StorageOpts {
                metadata_section: section.to_string(),
                no_categories: true,
                ..StorageOpts::default()
            },
        ) else {
            return;
        };
        let a = fx.data_root.join("a.zip");
        write_file(&a, b"metadata-config-plain");
        let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
        assert_eq!(code, *want, "[metadata]({tag}): {stderr}");
    }
    // セクション自体の省略もplain(既定)として受理される
    let Some(fx) = StorageFixture::new(
        "cfg-meta-default",
        StorageOpts {
            no_categories: true,
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"metadata-config-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 0, "[metadata]セクション省略はplain既定: {stderr}");
}

/// [covers:config.categories-section-rejected]
/// [categories]セクションは廃止(TASK-18 5.15): pipeline.tomlに[categories]が存在したら
/// 設定エラー2(deny_unknown_fieldsによる未知キー扱いで静的に廃止を確定させる)。
#[test]
fn categories_section_is_rejected() {
    let Some(fx) = StorageFixture::new("cfg-cat-reject", StorageOpts::default()) else {
        return;
    };
    // 既定のStorageOptsはTASK-18後の要件([categories]無し)で組むため、[categories]を
    // 明示的に追加したpipeline.tomlで検証する
    fx.rewrite_pipeline_toml(&format!(
        "{}[categories.unfiled]\nkeep = 5\n",
        fx.pipeline_body()
    ));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"categories-reject-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 2, "[categories]存在時は設定エラー2: {stderr}");
    assert_eq!(fx.calls(), "", "S3 API呼び出しは行われない: {}", fx.calls());
}

/// [covers:config.aws-lifecycle-required-and-ranged]
/// [aws.lifecycle]の3キー(noncurrent_days・keep_versions・mpu_abort_days)はすべて必須
/// (省略は設定エラー2・コード内defaultなし)。範囲検証: keep_versionsは1..=100・
/// noncurrent_daysは1以上・mpu_abort_daysは1以上。DEEP_ARCHIVE profileで
/// noncurrent_days<180は設定エラー2(Deep Archive最低保存180日)。
#[test]
fn aws_lifecycle_keys_required_and_ranged() {
    let valid = aws_lifecycle_toml(180, 10, 7);
    // (tag, [aws.lifecycle]断片, storage_class, 期待exit code)
    let cases: &[(&str, String, &str, i32)] = &[
        (
            "keep_versions欠落",
            "[aws.lifecycle]\nnoncurrent_days = 180\nmpu_abort_days = 7\n".into(),
            "STANDARD",
            2,
        ),
        (
            "noncurrent_days欠落",
            "[aws.lifecycle]\nkeep_versions = 10\nmpu_abort_days = 7\n".into(),
            "STANDARD",
            2,
        ),
        (
            "mpu_abort_days欠落",
            "[aws.lifecycle]\nnoncurrent_days = 180\nkeep_versions = 10\n".into(),
            "STANDARD",
            2,
        ),
        ("セクション無し", String::new(), "STANDARD", 2),
        (
            "keep_versions=0",
            aws_lifecycle_toml(180, 0, 7),
            "STANDARD",
            2,
        ),
        (
            "keep_versions=101",
            aws_lifecycle_toml(180, 101, 7),
            "STANDARD",
            2,
        ),
        (
            "noncurrent_days=0",
            aws_lifecycle_toml(0, 10, 7),
            "STANDARD",
            2,
        ),
        (
            "mpu_abort_days=0",
            aws_lifecycle_toml(180, 10, 0),
            "STANDARD",
            2,
        ),
        (
            "DEEP_ARCHIVEでnoncurrent_days=179",
            aws_lifecycle_toml(179, 10, 7),
            "DEEP_ARCHIVE",
            2,
        ),
        ("適正値(180/10/7)", valid.clone(), "STANDARD", 0),
        ("DEEP_ARCHIVEで180", valid.clone(), "DEEP_ARCHIVE", 0),
    ];
    for (i, (tag, lifecycle, storage_class, want)) in cases.iter().enumerate() {
        let Some(fx) = StorageFixture::new(
            &format!("cfg-awslyf-{i}"),
            StorageOpts {
                storage_class,
                aws_lifecycle: lifecycle.clone(),
                no_categories: true,
                ..StorageOpts::default()
            },
        ) else {
            return;
        };
        let a = fx.data_root.join("a.zip");
        write_file(&a, b"lifecycle-config-plain");
        let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
        assert_eq!(code, *want, "[aws.lifecycle]({tag})はexit {want}: {stderr}");
        if *want == 2 {
            assert_eq!(fx.calls(), "", "{tag}: S3 API呼び出しは行われない");
        }
    }
}

/// [covers:config.layout-and-categories-validation]
/// [layout]/[files]の検証(TASK-18 5.15改訂): auto_path_rootsは絶対path・files.categoryと
/// [layout].unfiled_categoryはvalidate_category_nameによる文字列検証のみ
/// ([categories]定義存在検証は廃止・category語彙は記録値として自由化)・
/// keepの検証(1..=100)はaws.toml [aws.lifecycle]側へ移行。
/// files写像label先頭のsys衝突検証は廃止。TASK-20: [files.<basename>]のschema keyは
/// labelへ改名(旧path keyはdeny_unknown_fieldsの未知key=設定エラー2)。
#[test]
fn layout_and_files_validation_after_categories_removal() {
    // (tag, files断片, unfiled_category書換え値, 期待exit code)
    let cases: &[(&str, &str, Option<&str>, i32)] = &[
        ("auto_path_roots相対path", "", None, 2),
        (
            "files.categoryの[categories]未定義値(受理)",
            "[files.\"x.zip\"]\ncategory = \"docs\"\nlabel = \"a\"\n",
            None,
            0,
        ),
        (
            "files写像labelの_sys先頭(受理)",
            "[files.\"x.zip\"]\ncategory = \"unfiled\"\nlabel = \"_sys/x\"\n",
            None,
            0,
        ),
        (
            "[files]の旧path key(TASK-20改名前・未知key)",
            "[files.\"x.zip\"]\ncategory = \"docs\"\npath = \"a\"\n",
            None,
            2,
        ),
        ("unfiled_category規則違反", "", Some("Unfiled"), 2),
    ];
    for (i, (tag, files, unfiled, want)) in cases.iter().enumerate() {
        let Some(fx) = StorageFixture::new(
            &format!("cfg-layout-{i}"),
            StorageOpts {
                files: files.to_string(),
                no_categories: true,
                ..StorageOpts::default()
            },
        ) else {
            return;
        };
        if *tag == "auto_path_roots相対path" {
            fx.rewrite_pipeline_toml(&fx.pipeline_body().replace(
                &format!("auto_path_roots = [\"{}\"]", fx.data_root.display()),
                "auto_path_roots = [\"relative/path\"]",
            ));
        }
        if let Some(v) = unfiled {
            fx.rewrite_pipeline_toml(&fx.pipeline_body().replace(
                "unfiled_category = \"unfiled\"",
                &format!("unfiled_category = \"{v}\""),
            ));
        }
        let a = fx.data_root.join("x.zip");
        write_file(&a, b"layout-config-plain");
        let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
        assert_eq!(code, *want, "{tag}はexit {want}: {stderr}");
    }
}

/// [covers:config.from-list-schema-validation]
/// from-list toml(TASK-18 5.6改訂): path欠落・未知キー(name)は設定エラー。
/// [[targets]]にはtitle欄(任意文字列・validate_title)を追加でき、省略可。
#[test]
fn from_list_schema_with_title_field() {
    let Some(fx) = StorageFixture::new(
        "cfg-fromlist",
        StorageOpts {
            no_categories: true,
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let seed = fx.data_root.join("seed.zip");
    write_file(&seed, b"from-list-seed");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&seed], "from-list-seed", &[]) else {
        return;
    };
    assert_eq!(code, 0, "seedの初回push: {stdout}");
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"from-list-plain");
    let dir = fx.cfg.parent().expect("tmp dir");

    // (tag, from-list本文, 期待exit code)
    let cases: &[(&str, String, i32)] = &[
        ("path無しentry", "[[targets]]\ntitle = \"t\"\n".into(), 2),
        (
            // name欄はschema上の既知キーだが新経路(backup push)では拒否される
            // (対象エラー3・設計5.6「name欄の拒否(新経路)は現行どおり」)
            "name欄(新経路拒否・対象エラー3)",
            format!(
                "[[targets]]\npath = \"{}\"\nname = \"x.zip\"\n",
                a.display()
            ),
            3,
        ),
        (
            "title欄付き",
            format!(
                "[[targets]]\npath = \"{}\"\ntitle = \"list-title\"\n",
                a.display()
            ),
            0,
        ),
        (
            "title省略",
            format!("[[targets]]\npath = \"{}\"\n", a.display()),
            0,
        ),
    ];
    for (i, (tag, body, want)) in cases.iter().enumerate() {
        let list = dir.join(format!("from-list-cfg-{i}.toml"));
        write_file(&list, body.as_bytes());
        let list_s = list.display().to_string();
        // PATH>=1のためskip対象のseedをpositionalへ添える(dry-runのためtitle評価は警告まで)
        let (code, _stdout, stderr) =
            fx.push(&[&seed], &["--from-list", &list_s, "--dry-run"], &[]);
        assert_eq!(code, *want, "from-list({tag})はexit {want}: {stderr}");
    }
}

/// [covers:config.output-section-rejected]
/// [output]sectionは廃止(TASK-19 XDG配置): pipeline.tomlに[output]が存在したら
/// 設定エラー2(deny_unknown_fieldsによる未知キー扱い。registry・cache dirのpathは
/// XDG規定path固定で設定fileからは指定不可)。
#[test]
fn output_section_is_rejected() {
    for (tag, section) in [
        (
            "dir/registry",
            "[output]\ndir = \"out\"\nregistry = \"reg.jsonl\"\n",
        ),
        ("keep_artifacts", "[output]\nkeep_artifacts = \"run\"\n"),
    ] {
        let Some(fx) = StorageFixture::new("cfg-out-reject", StorageOpts::default()) else {
            return;
        };
        fx.rewrite_pipeline_toml(&format!("{}{}", fx.pipeline_body(), section));
        let a = fx.data_root.join("a.zip");
        write_file(&a, b"output-reject-plain");
        let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
        assert_eq!(code, 2, "[output]存在({tag})は設定エラー2: {stderr}");
        assert!(
            stderr.contains("output"),
            "{tag}: 未知キー名がmessageに含まれる: {stderr}"
        );
    }
}

/// [covers:config.default-config-missing-guidance]
/// 設定fileが規定pathに無い場合(--config/--aws-config省略時)は設定エラー2+案内表示
/// (TASK-19 XDG配置: repoのconfigs/*.example.tomlを規定pathへcopyする案内)。
/// 明示指定pathの不在は従来どおりpathのみの設定エラー2(案内無し)。
#[test]
fn default_config_missing_shows_copy_guidance() {
    let Some(fx) = StorageFixture::new("cfg-guidance", StorageOpts::default()) else {
        return;
    };
    let state_s = fx.state.display().to_string();
    let empty_abh = fx.elsewhere.join("guidance-abh");
    std::fs::create_dir_all(&empty_abh).expect("empty ABH dir");
    let abh_s = empty_abh.display().to_string();
    let env = [
        ("AMAZING_BACKUP_FAKE_S3_STATE", state_s.as_str()),
        ("AMAZING_BACKUP_HOME", abh_s.as_str()),
    ];

    // (a) 規定pathにpipeline.tomlが無い: backup verifyの--config省略は案内付き設定エラー2
    let (code, _stdout, stderr) = run_bin_env(&["backup", "verify"], &env);
    assert_eq!(code, 2, "規定pipeline.toml不在は設定エラー2: {stderr}");
    assert!(
        stderr.contains("configs/pipeline.example.toml"),
        "pipeline template copyの案内: {stderr}"
    );

    // (b) 規定pathにaws.tomlが無い: --aws-config省略は案内付き設定エラー2
    let (code, _stdout, stderr) = run_bin_env(&["list", "files", "--key", "k"], &env);
    assert_eq!(code, 2, "規定aws.toml不在は設定エラー2: {stderr}");
    assert!(
        stderr.contains("configs/aws.example.toml"),
        "aws template copyの案内: {stderr}"
    );

    // (c) 明示指定pathの不在は案内無しの設定エラー2
    let (code, _stdout, stderr) = run_bin_env(
        &["backup", "verify", "--config", "/nonexistent/pipeline.toml"],
        &env,
    );
    assert_eq!(code, 2, "明示指定の不在は設定エラー2: {stderr}");
    assert!(
        !stderr.contains("example.toml"),
        "明示指定にはcopy案内を出さない: {stderr}"
    );
}
