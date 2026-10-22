//! layout エリアの結合test(TASK-10 S3ストレージ設計・tmp/tasks/tm-10/design_s3_storage.md 1章)。
//! src/layout.rsは実装前(Phase Aで新設)のため、key生成・分類解決の純関数群は
//! bin subprocessの`backup push --dry-run`(対象・分類・衝突・予定key表示)と
//! 実pushのfake seam(BACKUP_REMOTE_FAKE_S3_STATE)のAPI呼び出し記録で観測する
//! (テスト先行のため実装前の実行failは許容・cargo test --no-runは通る)。

mod common;

use common::*;

/// [covers:layout.key-generation-manual-mapping]
/// files写像が最優先(設計1.2優先順位1): [files.<name>]のcategoryとpathをそのまま用い、
/// category/rel_pathは所在記録値(IndexLocation・manifest file record)として解決される
/// (TASK-18: keyはUUID体系で生成され分類に依存しない。対象がauto_path_roots配下にも
/// 一致する状態でも写像が勝つ)。
#[test]
fn files_mapping_takes_priority_for_classification() {
    let Some(fx) = StorageFixture::new(
        "lay-manual",
        StorageOpts {
            files: "[files.\"travel.zip\"]\ncategory = \"photos\"\npath = \"2020-2023/travel\"\n"
                .into(),
            no_categories: true,
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let travel = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("travel.zip");
    write_file(&travel, b"layout-manual-mapping");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&travel], "旅のしおり", &[]) else {
        return;
    };
    assert_eq!(code, 0, "pushはexit 0: {stdout}");
    let rec = file_write_of(&fx, &travel);
    assert_eq!(
        rec.get("category").and_then(|c| c.as_str()),
        Some("photos"),
        "files写像のcategoryが記録値になる: {rec}"
    );
    assert_eq!(
        rec.get("rel_path").and_then(|r| r.as_str()),
        Some("2020-2023/travel"),
        "files写像のpathがrel_path記録値になる: {rec}"
    );
}

/// registryのfile_write記録をsource_pathで引く(所在記録値の観測用)。
fn file_write_of(fx: &StorageFixture, path: &std::path::Path) -> serde_json::Value {
    let canonical = std::fs::canonicalize(path).expect("realpath");
    fx.registry_values()
        .into_iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("source_path").and_then(|p| p.as_str())
                    == Some(canonical.to_string_lossy().as_ref())
        })
        .unwrap_or_else(|| panic!("file_write記録({})が無い", canonical.display()))
}

/// [covers:layout.key-generation-auto-relativize]
/// auto_path_roots相対化(設計1.2優先順位2): source_pathがいずれかのroot配下なら
/// 最初に一致したrootからの相対pathをrel_pathとし、categoryはunfiled_category(既定unfiled)。
/// rootsは順序優先(先頭root=data_rootから相対化するため2番目root基準にはならない)。
/// 解決結果は所在記録値として記録される(TASK-18)。
#[test]
fn auto_path_roots_relativize_classification() {
    let Some(fx) = StorageFixture::new("lay-auto", StorageOpts::default()) else {
        return;
    };
    // 2番目のroot = data_root/2020-2023 を追加(data_rootが先頭=最初に一致)
    let second_root = fx.data_root.join("2020-2023");
    std::fs::create_dir_all(&second_root).expect("second root dir");
    fx.rewrite_pipeline_toml(&fx.pipeline_body().replace(
        &format!("auto_path_roots = [\"{}\"]", fx.data_root.display()),
        &format!(
            "auto_path_roots = [\"{0}\", \"{1}\"]",
            fx.data_root.display(),
            second_root.display()
        ),
    ));
    let travel = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("travel.zip");
    write_file(&travel, b"layout-auto-relativize");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&travel], "旅のしおり", &[]) else {
        return;
    };
    assert_eq!(code, 0, "pushはexit 0: {stdout}");
    let rec = file_write_of(&fx, &travel);
    // 最初に一致したroot(data_root)からの相対 = 2020-2023/travel
    // (2番目のroot基準なら "travel" になるため、これで順序優先を観測する)
    assert_eq!(
        rec.get("rel_path").and_then(|r| r.as_str()),
        Some("2020-2023/travel"),
        "最初に一致したrootからの相対rel_path記録値: {rec}"
    );
    assert_eq!(
        rec.get("category").and_then(|c| c.as_str()),
        Some("unfiled"),
        "category=unfiled_category(既定値)の記録値: {rec}"
    );
}

/// [covers:layout.key-generation-fallback-category-only]
/// fallback(設計1.2優先順位3): いずれのrootにも一致しないsource_pathは
/// rel_pathなし(空)・category=unfiled_categoryの記録値になる(TASK-18)。
#[test]
fn fallback_category_only_classification() {
    let Some(fx) = StorageFixture::new("lay-fallback", StorageOpts::default()) else {
        return;
    };
    let outside = fx.elsewhere.join("x.zip");
    write_file(&outside, b"layout-fallback");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&outside], "記録", &[]) else {
        return;
    };
    assert_eq!(code, 0, "pushはexit 0: {stdout}");
    let rec = file_write_of(&fx, &outside);
    assert_eq!(
        rec.get("rel_path").and_then(|r| r.as_str()),
        Some(""),
        "roots不一致はrel_pathなし(空)の記録値: {rec}"
    );
    assert_eq!(
        rec.get("category").and_then(|c| c.as_str()),
        Some("unfiled"),
        "category=unfiled_category設定値: {rec}"
    );
}

/// [covers:layout.rel-path-normalization]
/// rel_path正規化(設計1.2): `.`/`..`要素の排除・空要素除去・path末尾`/`禁止。
/// いずれの書き方も正規化後のrel_path(a/b)が所在記録値として記録される(TASK-18:
/// normalize_rel_path維持・記録値の正規化)。
#[test]
fn rel_path_is_normalized_in_records() {
    let Some(fx) = StorageFixture::new(
        "lay-norm",
        StorageOpts {
            files: "[files.\"n1.zip\"]\ncategory = \"photos\"\npath = \"a/./b/\"\n\
                    [files.\"n2.zip\"]\ncategory = \"photos\"\npath = \"a/b/\"\n\
                    [files.\"n3.zip\"]\ncategory = \"photos\"\npath = \"a//b\"\n"
                .into(),
            no_categories: true,
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let paths: Vec<std::path::PathBuf> = ["n1.zip", "n2.zip", "n3.zip"]
        .iter()
        .map(|n| fx.elsewhere.join(n))
        .collect();
    for p in &paths {
        write_file(p, b"normalize");
    }
    let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    let Some((code, stdout)) = push_titled_pty(&fx, &refs, "n1\nn2\nn3", &[]) else {
        return;
    };
    assert_eq!(code, 0, "pushはexit 0: {stdout}");
    for p in &paths {
        let rec = file_write_of(&fx, p);
        assert_eq!(
            rec.get("rel_path").and_then(|r| r.as_str()),
            Some("a/b"),
            "正規化後rel_path(a/b)の記録値: {rec}"
        );
    }
}

/// [covers:layout.category-name-validation]
/// category名規則違反と予約語sys/旧表記_sysの拒否(設計1.1・4.2): unfiled_category・
/// files.categoryの値として不正な名前は設定エラー2(TASK-18: [categories]定義存在検証は
/// 廃止されvalidate_category_nameによる文字列検証のみへ置換。カテゴリ語彙は記録値として
/// 自由化=規則を満たす名前は[categories]未定義でも受理。files写像path先頭のsys衝突検証は
/// 廃止=path値の"_sys"先頭は受理される)。
#[test]
fn invalid_category_names_are_config_errors() {
    // (tag, files断片, 期待exit code)
    let cases: &[(&str, &str, i32)] = &[
        ("unfiled_categoryの名前規則違反", "", 2),
        (
            "files.categoryの_sys予約",
            "[files.\"x.zip\"]\ncategory = \"_sys\"\npath = \"a\"\n",
            2,
        ),
        (
            "files.categoryの名前規則違反",
            "[files.\"x.zip\"]\ncategory = \"BadName\"\npath = \"a\"\n",
            2,
        ),
        (
            "files.categoryの[categories]未定義値(新仕様では受理)",
            "[files.\"x.zip\"]\ncategory = \"docs\"\npath = \"a\"\n",
            0,
        ),
        (
            "files写像path先頭の_sys(新仕様では受理)",
            "[files.\"x.zip\"]\ncategory = \"unfiled\"\npath = \"_sys/x\"\n",
            0,
        ),
    ];
    for (i, (tag, files, want)) in cases.iter().enumerate() {
        let Some(fx) = StorageFixture::new(
            &format!("lay-catval-{i}"),
            StorageOpts {
                files: files.to_string(),
                no_categories: true,
                ..StorageOpts::default()
            },
        ) else {
            return;
        };
        let rewrite_unfiled = *tag == "unfiled_categoryの名前規則違反";
        if rewrite_unfiled {
            fx.rewrite_pipeline_toml(&fx.pipeline_body().replace(
                "unfiled_category = \"unfiled\"",
                "unfiled_category = \"Unfiled\"",
            ));
        }
        let target = fx.elsewhere.join("x.zip");
        write_file(&target, b"category-validation");
        // 受理caseはtitle必須の新規発行のためpty promptで与える(拒否caseは
        // 設定検査が先に弾くためstdinは読まれない)
        let Some((code, stdout)) = push_titled_pty(&fx, &[&target], "検証対象", &[]) else {
            return;
        };
        assert_eq!(code, *want, "{tag}: {stdout}");
        if *want == 2 {
            assert_eq!(fx.calls(), "", "{tag}: S3 API呼び出しは行われない");
        }
    }
}

/// [covers:layout.pin-name-validation-and-default-reserved]
/// pin名規則(設計1.3): ^[a-z0-9][a-z0-9-]{0,63}$。`default`は予約で不許可。
/// 違反はusage error 64(CLI引数の形式検証)。適正名(pre-migration・日付版名)は受理される。
#[test]
fn pin_name_validation_rejects_reserved_and_invalid() {
    let Some(fx) = StorageFixture::new("lay-pinname", StorageOpts::default()) else {
        return;
    };
    let target = fx.data_root.join("x.zip");
    write_file(&target, b"pin-name-validation");
    for bad in ["default", "Abc", "-x"] {
        let (code, _, _) = fx.pin("add", "x.zip", &["--latest", &format!("--as={bad}")]);
        assert_eq!(code, 64, "pin名({bad})はusage error 64");
    }
    // 64文字(先頭1+{0,62}=全体63文字上限の境界外)
    let long_pin = format!("--as={}", "a".repeat(64));
    let (code, _, _) = fx.pin("add", "x.zip", &["--latest", &long_pin]);
    assert_eq!(code, 64, "64文字のpin名はusage error 64");

    // 適正名はusage errorとはならない(以降の段階へ進む=64以外。
    // 実装前は未知サブコマンドで64になるため、このassertは実装後に通る)
    for good in ["pre-migration", "2026-10-14"] {
        let (code, _, _) = fx.pin("add", "x.zip", &["--latest", &format!("--as={good}")]);
        assert_ne!(code, 64, "適正pin名({good})は受理される(usage error以外)");
    }
}

/// [covers:layout.prefix-format-validation]
/// prefix形式検証(設計1.1・4.2): 空でない・`/`終端・先頭`/`禁止・使用文字は[a-z0-9-]区切り`/`。
/// 違反は設定エラー2。適正値はkey生成に使われる(当該prefixの予定key表示)。
#[test]
fn prefix_format_validation_rejects_invalid_prefixes() {
    let Some(fx) = StorageFixture::new("lay-prefix", StorageOpts::default()) else {
        return;
    };
    let target = fx.data_root.join("x.zip");
    write_file(&target, b"prefix-validation");
    // [aws.lifecycle]は必須(設計7章)のため基準本文へ含めた状態でprefixのみ書換える
    let base = format!(
        "{}{}",
        storage_aws_toml_body("it-bucket", "backup-remote/", "STANDARD", &fx.credentials),
        aws_lifecycle_toml(180, 10, 7)
    );

    for (tag, prefix) in [
        ("空", ""),
        ("末尾スラッシュなし", "backup-remote"),
        ("先頭スラッシュ", "/backup-remote/"),
        ("使用文字違反", "Backup_Remote/"),
    ] {
        fx.rewrite_aws_toml(&base.replace(
            "prefix = \"backup-remote/\"",
            &format!("prefix = \"{prefix}\""),
        ));
        let (code, _, stderr) = fx.push(&[&target], &["--dry-run"], &[]);
        assert_eq!(code, 2, "prefix形式違反({tag})は設定エラー2: {stderr}");
        assert_eq!(fx.calls(), "", "{tag}: S3 API呼び出しは行われない");
    }

    // 適正prefixは受理され、接続先表示に現れる(dry-runの新規発行予測keyは
    // uuid未確定のためdata/(新規uuid)/default.age表示・設計4.1)
    fx.rewrite_aws_toml(&base);
    let (code, stdout, stderr) = fx.push(&[&target], &["--dry-run"], &[]);
    assert_eq!(code, 0, "適正prefixはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("prefix=backup-remote/"),
        "適正prefixによる接続先表示: {stdout}"
    );
    assert!(
        stdout.contains("data/(新規uuid)/default.age"),
        "新規発行予測のkey表示: {stdout}"
    );
}

/// [covers:layout.key-length-limit-1024]
/// key長検証(設計1.2・11章S3-1・TASK-18 5.1): 生成keyがS3のobject key上限1024 byteを
/// 超える場合は対象エラー3(keyと対象を表示)。data_object_key・pin_object_key・
/// metadata_object_key(UUID体系キー)にcheck_key_lengthを適用。
/// uuidは36字固定のため境界はprefix長とtitle長(64字上限)の組合せで作る
/// (metadata key = prefix + "data/" + uuid(36) + "/" + "metadata_" + title + ".json"。
/// title=64字で metadata key = prefix長 + 120 byte)。
/// 1024 byteちょうどは受理される。
#[test]
fn key_length_over_1024_is_target_error() {
    let Some(fx) = StorageFixture::new("lay-keylen", StorageOpts::default()) else {
        return;
    };
    let title64 = "t".repeat(64);
    // metadata key = prefix(P byte) + 5 + 36 + 1 + 9 + 64 + 5 = P + 120
    let over_prefix = format!("{}/", "a".repeat(905 - 1)); // 905 byte -> key 1025 byte
    let just_prefix = format!("{}/", "a".repeat(904 - 1)); // 904 byte -> key 1024 byte
    let target = fx.data_root.join("k.zip");
    write_file(&target, b"key-length");
    let base_aws = fx.aws_toml_body();

    // 1025 byte: 対象エラー3(key上限の表示)
    fx.rewrite_aws_toml(&base_aws.replace(
        "prefix = \"backup-remote/\"",
        &format!("prefix = \"{over_prefix}\""),
    ));
    let Some((code, stdout)) = push_titled_pty(&fx, &[&target], &title64, &[]) else {
        return;
    };
    assert_eq!(code, 3, "1025 byte keyは対象エラー3: {stdout}");
    assert!(stdout.contains("1024"), "key上限(1024)の案内表示: {stdout}");

    // 1024 byteちょうど: 受理される
    fx.rewrite_aws_toml(&base_aws.replace(
        "prefix = \"backup-remote/\"",
        &format!("prefix = \"{just_prefix}\""),
    ));
    let Some((code, stdout)) = push_titled_pty(&fx, &[&target], &title64, &[]) else {
        return;
    };
    assert_eq!(code, 0, "1024 byteちょうどは受理: {stdout}");
}

/// [covers:layout.uuid-v7-generation]
/// generate_uuid(TASK-18 5.1): UUIDv7のhyphenated lowercase 36文字列
/// (8-4-4-4-12・version nibble=7)。連続呼び出しの辞書順は呼び出し順と一致
/// (時刻由来の単調増加傾向)。1run内の新規発行3件のuuidで観測する。
#[test]
fn generated_uuids_are_v7_and_monotonic() {
    let Some(fx) = StorageFixture::new("lay-uuid", StorageOpts::default()) else {
        return;
    };
    let paths: Vec<std::path::PathBuf> = ["u1.zip", "u2.zip", "u3.zip"]
        .iter()
        .map(|n| fx.elsewhere.join(n))
        .collect();
    for (i, p) in paths.iter().enumerate() {
        write_file(p, format!("uuid-v7-{i}").as_bytes());
    }
    let refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    let Some((code, stdout)) = push_titled_pty(&fx, &refs, "u1\nu2\nu3", &[]) else {
        return;
    };
    assert_eq!(code, 0, "pushはexit 0: {stdout}");

    // registryのfile_write記録(記録順=発行順)からuuidを取り出す
    let uuids: Vec<String> = fx
        .registry_values()
        .into_iter()
        .filter(|r| r.get("record_type").and_then(|t| t.as_str()) == Some("file_write"))
        .filter_map(|r| r.get("uuid").and_then(|u| u.as_str()).map(String::from))
        .collect();
    assert_eq!(uuids.len(), 3, "3件の新規発行uuid: {uuids:?}");
    let mut prev: Option<&String> = None;
    for (i, u) in uuids.iter().enumerate() {
        assert_eq!(u.len(), 36, "uuid[{i}]は36字: {u}");
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "uuid[{i}]は8-4-4-4-12形式: {u}"
        );
        assert!(
            u.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "uuid[{i}]はhex小文字+hyphen: {u}"
        );
        assert!(
            u.chars().all(|c| !c.is_ascii_uppercase()),
            "uuid[{i}]はlowercase: {u}"
        );
        assert!(
            parts[2].starts_with('7'),
            "uuid[{i}]のversion nibbleは7: {u}"
        );
        if let Some(p) = prev {
            assert!(
                u >= p,
                "発行順に辞書順も逆転しない(単調増加): prev={p} cur={u}"
            );
        }
        prev = Some(u);
    }
}

/// [covers:layout.data-area-object-keys]
/// data領域のkey生成(TASK-18 5.1・1.1): data_object_key={prefix}data/{uuid}/default.age・
/// pin_object_key={prefix}data/{uuid}/{pin}.age・metadata_object_key=
/// {prefix}data/{uuid}/metadata_{title}.json。実pushのAPI呼び出し記録で観測する
/// (pinはpush --pinの直接put・metadataはrun末尾syncのPUT・storage_class=STANDARD)。
#[test]
fn data_area_object_keys_follow_uuid_layout() {
    let Some(fx) = StorageFixture::new(
        "lay-datakey",
        StorageOpts {
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let target = fx.elsewhere.join("旅のしおり2026.zip");
    write_file(&target, b"data-area-keys");
    let Some((code, stdout)) =
        push_titled_pty(&fx, &[&target], "旅のしおり2026", &["--pin", "keep-1"])
    else {
        return;
    };
    assert_eq!(code, 0, "push --pinはexit 0: {stdout}");
    let uuid = file_write_of(&fx, &target)
        .get("uuid")
        .and_then(|u| u.as_str())
        .expect("file_write.uuid")
        .to_string();

    let calls = fx.calls();
    let data_key = fx.data_key(&uuid);
    let pin_key = fx.data_pin_key(&uuid, "keep-1");
    let meta_key = fx.metadata_key(&uuid, "旅のしおり2026");
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={data_key}"))),
        "default.key({data_key})へのPUT: {calls}"
    );
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={pin_key}"))),
        "pin key({pin_key})への直接PUT: {calls}"
    );
    let meta_put = calls
        .lines()
        .find(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={meta_key}")))
        .unwrap_or_else(|| panic!("metadata key({meta_key})へのPUT: {calls}"));
    assert!(
        meta_put.contains("storage_class=STANDARD"),
        "metadataはSTANDARD固定: {meta_put}"
    );
}

/// [covers:layout.title-validation]
/// validate_title(TASK-18 5.1): 空・65文字超過・改行(\\n/\\r)・スラッシュ・制御文字は
/// 拒否(Usage 64・validate_pin_nameと同一区分)。64文字ちょうど・UTF-8日本語は受理
/// (S3コンソール可読性最優先)。from-listのtitle欄を通して観測する。
#[test]
fn title_validation_rejects_invalid_titles() {
    let Some(fx) = StorageFixture::new("lay-title", StorageOpts::default()) else {
        return;
    };
    let companion = fx.data_root.join("companion.zip");
    write_file(&companion, b"title-validation-companion");
    // companionはskip対象にするため先に発行しておく(titleはpromptで与える)
    let Some((code, stdout)) = push_titled_pty(&fx, &[&companion], "companion", &[]) else {
        return;
    };
    assert_eq!(code, 0, "companionの初回push: {stdout}");

    let target = fx.elsewhere.join("t.zip");
    write_file(&target, b"title-validation");
    let dir = fx.cfg.parent().expect("tmp dir");
    // (tag, tomlのtitle欄の値(エスケープ表現), 期待exit code)
    let cases: &[(&str, String, i32)] = &[
        ("空", "\"\"".to_string(), 64),
        ("65文字", format!("\"{}\"", "t".repeat(65)), 64),
        ("改行", "\"a\\nb\"".to_string(), 64),
        ("復帰", "\"a\\rb\"".to_string(), 64),
        ("スラッシュ", "\"a/b\"".to_string(), 64),
        ("制御文字", "\"a\\u0001b\"".to_string(), 64),
        ("64文字ちょうど", format!("\"{}\"", "t".repeat(64)), 0),
        ("日本語64文字", format!("\"{}\"", "あ".repeat(64)), 0),
    ];
    for (i, (tag, title_field, want)) in cases.iter().enumerate() {
        let body = format!(
            "[[targets]]\npath = \"{}\"\ntitle = {title_field}\n",
            target.display()
        );
        let list = dir.join(format!("from-list-{i}.toml"));
        write_file(&list, body.as_bytes());
        let list_s = list.display().to_string();
        // PATH 1個以上必須のためskip対象のcompanionをpositionalへ添える
        // (違反caseはtitle検証が先に弾くためstdinのcompanion再発行は起きない)
        let Some((code, stdout)) =
            push_titled_pty(&fx, &[&companion], "companion-2", &["--from-list", &list_s])
        else {
            return;
        };
        assert_eq!(code, *want, "title({tag})はexit {want}: {stdout}");
    }
}
