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
/// (TASK-1: amazing-backup config initで設定file群を生成する案内へ改訂。
/// 案内文はtemplate正本のrepo path(configs/*.example.toml)を併記する)。
/// 明示指定pathの不在は従来どおりpathのみの設定エラー2(案内無し)。
#[test]
fn default_config_missing_shows_init_guidance() {
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

    // (a) 規定pathにpipeline.tomlが無い: backup verifyの--config省略は案内付き設定エラー2。
    // TASK-7でverifyはaws.toml必須(実効profile解決)となるためaws.tomlのみ配置して
    // pipeline.toml不在側の観察とする(aws不在branchは(b)がcover)
    std::fs::copy(&fx.aws_toml, empty_abh.join("aws.toml")).expect("aws.tomlのみ配置");
    let (code, _stdout, stderr) = run_bin_env(&["backup", "verify"], &env);
    assert_eq!(code, 2, "規定pipeline.toml不在は設定エラー2: {stderr}");
    assert!(
        stderr.contains("config init"),
        "config initによる生成の案内: {stderr}"
    );

    // (b) 規定pathにaws.tomlが無い: (a)で配置したaws.tomlを削除し「何も置かない」状態へ
    std::fs::remove_file(empty_abh.join("aws.toml")).expect("(a)で配置したaws.tomlを削除");
    let (code, _stdout, stderr) = run_bin_env(&["list", "files", "--key", "k"], &env);
    assert_eq!(code, 2, "規定aws.toml不在は設定エラー2: {stderr}");
    assert!(
        stderr.contains("config init"),
        "config initによる生成の案内: {stderr}"
    );

    // (c) 明示指定pathの不在は案内無しの設定エラー2(TASK-7でverifyはaws.tomlを先に
    // 読むため、aws.tomlを配置し直して明示指定側(pipeline.toml)の不在のみを観察する)
    std::fs::copy(&fx.aws_toml, empty_abh.join("aws.toml")).expect("(c)用にaws.tomlを再配置");
    let (code, _stdout, stderr) = run_bin_env(
        &["backup", "verify", "--config", "/nonexistent/pipeline.toml"],
        &env,
    );
    assert_eq!(code, 2, "明示指定の不在は設定エラー2: {stderr}");
    assert!(
        !stderr.contains("config init"),
        "明示指定には生成案内を出さない: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// TASK-7(pipeline.toml profile化・tmp/tasks/tm-7/design_pipeline_profile.md
// D-1〜D-3・D-7)の条件群。テスト先行のため実装前の実行failを許容する。

/// [covers:config.pipeline-profiles-schema]
/// pipeline.tomlのtop-levelは[profiles.<name>]のみ(TASK-7 D-1): 現行6sectionは
/// すべて[profiles.<name>]配下へ移動し、top-level共通値は置かない。profile内の
/// 各sectionの必須・省略時既定・検証規則は現行と同一(recipients必須・他section省略可)。
#[test]
fn pipeline_profiles_schema_accepted() {
    // [profiles.dev](全section) + [profiles.family](recipients+verifyのみ=省略可)の
    // 構成が受理され動作する(top-levelにはいかなるsectionも置かない)
    let Some(fx) = StorageFixture::new("cfg-prof-schema", StorageOpts::default()) else {
        return;
    };
    let root = fx.data_root.display();
    fx.rewrite_pipeline_toml(&format!(
        "[profiles.dev.recipients]\npublic_keys = [\"{}\"]\n\
         [profiles.dev.verify]\ndecrypt_key = \"{}\"\n\
         [profiles.dev.layout]\nunfiled_category = \"unfiled\"\nauto_path_roots = [\"{root}\"]\n\
         [profiles.family.recipients]\npublic_keys = [\"{}\"]\n\
         [profiles.family.verify]\ndecrypt_key = \"{}\"\n",
        fx.public.display(),
        fx.secret.display(),
        fx.public.display(),
        fx.secret.display(),
    ));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"profile-schema-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 0, "profile schema構成は受理されdry-runはexit 0: {stderr}");
}

/// [covers:config.pipeline-profile-name-validated]
/// pipeline profile名の規則は ^[a-z0-9][a-z0-9-]{0,63}$(category名規則と同一)。
/// 違反は設定エラー2(第1層=全profile一括検証。未選択profileの違反でも弾く)。
#[test]
fn pipeline_profile_name_validated() {
    let Some(fx) = StorageFixture::new("cfg-prof-name", StorageOpts::default()) else {
        return;
    };
    // (profile名, 期待exit code)
    let over64 = "x".repeat(65);
    let max63 = "x".repeat(63);
    let cases: &[(&str, i32)] = &[
        ("BadName", 2),   // 大文字
        ("-lead", 2),     // 先頭ハイフン
        ("a_b", 2),       // アンダースコア
        (over64.as_str(), 2), // 64文字超
        (max63.as_str(), 0),  // 63文字は適合(未選択でもschema検査は通る)
        ("dev-2", 0),     // 小文字+数字+ハイフン
    ];
    for (i, (name, want)) in cases.iter().enumerate() {
        let Some(fx) = StorageFixture::new(&format!("cfg-prof-name-{i}"), StorageOpts::default())
        else {
            return;
        };
        // aws.tomlのactive_profileはdevのまま=違反名profileは未選択。それでも
        // 第1層(deserialize)の名前規則は全profileへ及ぶ
        fx.rewrite_pipeline_toml(&format!(
            "[profiles.dev.recipients]\npublic_keys = [\"{}\"]\n\
             [profiles.{name}.recipients]\npublic_keys = [\"{}\"]\n",
            fx.public.display(),
            fx.public.display(),
        ));
        let a = fx.data_root.join("a.zip");
        write_file(&a, b"profile-name-plain");
        let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
        assert_eq!(code, *want, "profile名({name})はexit {want}: {stderr}");
        if *want == 2 {
            assert!(
                stderr.contains(name),
                "違反profile名({name})がerror文に現れる: {stderr}"
            );
        }
    }
}

/// [covers:config.aws-profile-name-validated]
/// aws.tomlのprofile名にもpipelineと同一の規則 ^[a-z0-9][a-z0-9-]{0,63}$ を適用する
/// (TASK-7 2.9項目6: 同名対応の構造的保証。違反名は設定エラー2)。
#[test]
fn aws_profile_name_validated() {
    let Some(fx) = StorageFixture::new("cfg-awsname", StorageOpts::default()) else {
        return;
    };
    // active_profileと[aws.profiles.<name>]のkeyのみ違反値へ(purpose等は無関係)
    fx.rewrite_aws_toml(
        &fx.aws_toml_body()
            .replace("active_profile = \"dev\"", "active_profile = \"BadName\"")
            .replace("[aws.profiles.dev]", "[aws.profiles.BadName]"),
    );
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"aws-name-plain");
    // (a) active_profile側の違反
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 2, "違反aws profile名は設定エラー2: {stderr}");
    assert!(
        stderr.contains("BadName"),
        "違反profile名がerror文に現れる: {stderr}"
    );
    // (b) --profileでの違反名指定も同一に設定エラー2(S3 API呼び出し前)
    let (code, _stdout, stderr) = fx.list_env("files", &["--profile", "BadName"], &[]);
    assert_eq!(code, 2, "--profile違反名は設定エラー2: {stderr}");
    assert_eq!(fx.calls(), "", "S3 API呼び出しは行われない: {}", fx.calls());
}

/// [covers:config.pipeline-validation-layered]
/// 検証は2層(TASK-7 D-1): 第1層(TOML parse・未知key拒否・型不一致・profile名規則)は
/// 全profileへ一括適用(未選択profileの未知keyも設定エラー2)。第2層(recipients必須・
/// 空配列拒否・鍵file存在・ed25519形式・指紋重複)は選択profileのみ(未選択profileの
/// 鍵pathの実在性は検証しない=dummy鍵path・同一path重複を置ける)。
#[test]
fn pipeline_validation_is_layered() {
    // (A) 第1層: 未選択profileの未知keyは設定エラー2
    let Some(fx) = StorageFixture::new("cfg-layer1", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_pipeline_toml(&format!(
        "[profiles.dev.recipients]\npublic_keys = [\"{}\"]\n\
         [profiles.family.recipients]\npublic_keys = [\"{}\"]\nunknown_key = 1\n",
        fx.public.display(),
        fx.public.display(),
    ));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"layer1-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 2, "未選択profileの未知keyは設定エラー2(第1層): {stderr}");
    assert!(
        stderr.contains("unknown_key"),
        "未知キー名がerror文に現れる: {stderr}"
    );

    // (B) 第2層: 未選択profileの鍵は実在検査しない(dummy絶対path・同一path重複を許容)
    let Some(fx) = StorageFixture::new("cfg-layer2", StorageOpts::default()) else {
        return;
    };
    let dummy = "/nonexistent/amazing-backup-it-dummy.pub";
    fx.rewrite_pipeline_toml(&format!(
        "[profiles.dev.recipients]\npublic_keys = [\"{}\"]\n\
         [profiles.family.recipients]\npublic_keys = [\"{dummy}\", \"{dummy}\"]\n\
         [profiles.family.verify]\ndecrypt_key = \"/nonexistent/amazing-backup-it-dummy-key\"\n",
        fx.public.display(),
    ));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"layer2-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(
        code, 0,
        "未選択profileのdummy鍵path・重複は設定エラーにしない(第2層は選択profileのみ): {stderr}"
    );
}

/// [covers:config.pipeline-profile-required-for-effective-profile]
/// 実効aws profile名に対応する[profiles.<name>]がpipeline.tomlに無い場合は設定エラー2
/// (利用可能なpipeline profile一覧をerror文へ表示)。--profile省略時(active_profile)・
/// 照会系(pipeline.tomlが在るが実効profileのentryが無い)でも救済しない
/// (タイポの握りつぶし防止=2.9項目5)。
#[test]
fn pipeline_profile_required_for_effective_profile() {
    let Some(fx) = StorageFixture::new("cfg-prof-missing", StorageOpts::default()) else {
        return;
    };
    // pipeline.tomlにはfamilyのみ(実効aws profile=devのentryが無い)
    fx.rewrite_pipeline_toml(&format!(
        "[profiles.family.recipients]\npublic_keys = [\"{}\"]\n",
        fx.public.display()
    ));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"prof-missing-plain");
    // (a) 書き込み系(backup push)
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 2, "実効profile(dev)のentry不在は設定エラー2: {stderr}");
    assert!(
        stderr.contains("family"),
        "利用可能なpipeline profile一覧(family)がerror文に表示される: {stderr}"
    );
    // (b) 照会系(list files)でも--key救済は無く設定エラー2
    let (code, _stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 2, "照会系でも実効profileのentry不在は設定エラー2: {stderr}");
    assert!(
        stderr.contains("family"),
        "照会系でも一覧表示: {stderr}"
    );
    assert_eq!(fx.calls(), "", "S3 API呼び出しは行われない: {}", fx.calls());
}

/// [covers:config.pipeline-extra-profiles-ignored]
/// pipeline.tomlにaws.toml側へ存在しない余分なprofileがあっても、選択されない限り
/// エラーとしない(2.9項目4。第1層の構文・schema検証は及ぶ=config.pipeline-validation-layered)。
#[test]
fn pipeline_extra_profiles_ignored() {
    let Some(fx) = StorageFixture::new("cfg-prof-extra", StorageOpts::default()) else {
        return;
    };
    let dummy = "/nonexistent/amazing-backup-it-dummy.pub";
    fx.rewrite_pipeline_toml(&format!(
        "[profiles.dev.recipients]\npublic_keys = [\"{}\"]\n\
         [profiles.spare.recipients]\npublic_keys = [\"{dummy}\"]\n",
        fx.public.display(),
    ));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"prof-extra-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(
        code, 0,
        "aws.tomlに存在しない余分profile(spare)は選択されない限りエラーにしない: {stderr}"
    );
}

/// [covers:config.pipeline-legacy-format-migration-error]
/// 旧top-level形式(現行schema)は専用の移行案内errorで設定エラー2(D-2):
/// 「旧形式です。各sectionを[profiles.<実効profile名>]配下へ移動してください」+該当
/// section名。案内のprofile名は解決済みの実効profile名。旧sectionが1つでもtop-levelに
/// 残っていれば[profiles]併存時でも同じ案内error(部分的な移行状態を許さない)。
/// この判定は--configで指定する任意fileにも同じく適用される。
#[test]
fn pipeline_legacy_format_rejected_with_migration_guidance() {
    let old_format = |fx: &StorageFixture| {
        format!(
            "[recipients]\npublic_keys = [\"{}\"]\n[behavior]\nkeep_going = false\n",
            fx.public.display()
        )
    };
    // (a) 純旧形式(規定pathのpipeline.toml)
    let Some(fx) = StorageFixture::new("cfg-legacy-a", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_pipeline_toml(&old_format(&fx));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"legacy-a-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 2, "旧top-level形式は設定エラー2: {stderr}");
    assert!(
        stderr.contains("旧形式") && stderr.contains("[profiles.dev"),
        "移行案内(旧形式+実効profile名): {stderr}"
    );
    assert!(
        stderr.contains("recipients"),
        "該当section名(recipients)が案内に含まれる: {stderr}"
    );

    // (b) [profiles]併存+top-levelに旧sectionが1つ残っている状態も同一の案内error
    let Some(fx) = StorageFixture::new("cfg-legacy-b", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_pipeline_toml(&format!(
        "{}[behavior]\nkeep_going = false\n",
        fx.pipeline_body()
    ));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"legacy-b-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 2, "部分的な移行状態(併存)は設定エラー2: {stderr}");
    assert!(
        stderr.contains("旧形式") && stderr.contains("behavior"),
        "併存時も旧形式案内+残存section名(behavior): {stderr}"
    );

    // (c) --configで指定する任意fileにも同一判定を適用(単一の--configのみ渡す=
    // StorageFixture::pushは--configを注入するためここでは直接実行)
    let Some(fx) = StorageFixture::new("cfg-legacy-c", StorageOpts::default()) else {
        return;
    };
    let legacy = fx.elsewhere.join("legacy-pipeline.toml");
    write_file(&legacy, old_format(&fx).as_bytes());
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"legacy-c-plain");
    let legacy_s = legacy.display().to_string();
    let a_s = a.display().to_string();
    let aws_s = fx.aws_toml.display().to_string();
    let (code, _stdout, stderr) = fx.bin(
        &[
            "backup",
            "push",
            "--config",
            &legacy_s,
            "--aws-config",
            &aws_s,
            "--dry-run",
            &a_s,
        ],
        &[],
    );
    assert_eq!(code, 2, "--config任意fileの旧形式も設定エラー2: {stderr}");
    assert!(
        stderr.contains("旧形式") && stderr.contains("[profiles.dev"),
        "任意fileにも同一の移行案内を適用: {stderr}"
    );
}

/// [covers:config.aws-same-bucket-multi-profile-warned]
/// aws.tomlで複数profileが同一bucketを参照する場合はstderr警告+処理継続(D-7・2.9項目8。
/// 現行のdev+prod3bucket構成=別bucket構成では発火しない)。警告は同一aws.toml内の
/// bucket重複の検出のみ(別file・recipient差し替えの検出限界は設計D-7の記載どおり)。
#[test]
fn aws_same_bucket_profiles_warned() {
    // (a) 同一bucketを2profileが参照: stderr警告+処理継続(exit 0)
    let Some(fx) = StorageFixture::new("cfg-samebkt", StorageOpts::default()) else {
        return;
    };
    fx.rewrite_aws_toml(&format!(
        "{}\n[aws.profiles.backup2]\npurpose = \"prod\"\nbucket = \"it-bucket\"\nregion = \"ap-northeast-1\"\nprefix = \"amazing-backup/\"\nstorage_class = \"STANDARD\"\ncredentials_file = \"{}\"\n",
        fx.aws_toml_body(),
        fx.credentials.display()
    ));
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"same-bucket-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 0, "警告は処理を止めない(処理継続): {stderr}");
    assert!(
        stderr.contains("同一bucket"),
        "同一bucket複数profile参照のstderr警告: {stderr}"
    );

    // (b) 別bucket構成(現行のdev+prod3bucket相当)では警告しない
    let Some(fx) = StorageFixture::new("cfg-diffbkt", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"diff-bucket-plain");
    let (code, _stdout, stderr) = fx.push(&[&a], &["--dry-run"], &[]);
    assert_eq!(code, 0, "別bucket構成はexit 0: {stderr}");
    assert!(
        !stderr.contains("同一bucket"),
        "別bucket構成では警告しない: {stderr}"
    );
}
