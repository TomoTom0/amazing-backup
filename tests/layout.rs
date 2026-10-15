//! layout エリアの結合test(TASK-10 S3ストレージ設計・tmp/tasks/tm-10/design_s3_storage.md 1章)。
//! src/layout.rsは実装前(Phase Aで新設)のため、key生成・分類解決の純関数群は
//! bin subprocessの`backup push --dry-run`(対象・分類・衝突・予定key表示)と
//! 実pushのfake seam(BACKUP_REMOTE_FAKE_S3_STATE)のAPI呼び出し記録で観測する
//! (テスト先行のため実装前の実行failは許容・cargo test --no-runは通る)。

mod common;

use common::*;

/// [covers:layout.key-generation-manual-mapping]
/// files写像は最優先(設計1.2優先順位1): [files.<name>]のcategoryとpathをそのまま用い、
/// key <prefix>/<category>/<rel_path>/<name>/default.age が生成される
/// (対象がauto_path_roots配下にも一致する状態でも写像が勝つ)。
#[test]
fn files_mapping_takes_priority_for_key_generation() {
    let Some(fx) = StorageFixture::new(
        "lay-manual",
        StorageOpts {
            files: "[files.\"travel.zip\"]\ncategory = \"photos\"\npath = \"2020-2023/travel\"\n"
                .into(),
            categories: "[categories.photos]\nkeep = 3\n".into(),
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
    let (code, stdout, stderr) = fx.push(&[&travel], &["--dry-run"], &[]);
    assert_eq!(code, 0, "dry-runはexit 0: {stdout}{stderr}");
    let key = fx.file_key("photos", Some("2020-2023/travel"), "travel.zip");
    assert!(
        stdout.contains(&key),
        "files写像のcategory/pathによる予定key({key})が表示される: {stdout}"
    );
    assert!(
        stdout.contains("photos"),
        "分類(category=photos)が表示される: {stdout}"
    );
    // 対象はauto_path_roots配下にも一致するが、写像category=photosが優先される
    // (unfiled側のkeyにはならない)
    assert!(
        !stdout.contains(&fx.file_key("unfiled", Some("2020-2023/travel"), "travel.zip")),
        "unfiled側のkeyは現れない: {stdout}"
    );
}

/// [covers:layout.key-generation-auto-relativize]
/// auto_path_roots相対化(設計1.2優先順位2): source_pathがいずれかのroot配下なら
/// 最初に一致したrootからの相対pathをrel_pathとし、categoryはunfiled_category(既定unfiled)。
/// rootsは順序優先(先頭root=data_rootから相対化するため2番目root基準にはならない)。
#[test]
fn auto_path_roots_relativize_key_generation() {
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
    let (code, stdout, stderr) = fx.push(&[&travel], &["--dry-run"], &[]);
    assert_eq!(code, 0, "dry-runはexit 0: {stdout}{stderr}");
    // 最初に一致したroot(data_root)からの相対 = 2020-2023/travel
    // (2番目のroot基準なら "travel" になるため、これで順序優先を観測する)
    let key = fx.file_key("unfiled", Some("2020-2023/travel"), "travel.zip");
    assert!(
        stdout.contains(&key),
        "最初に一致したrootからの相対rel_pathによるkey({key}): {stdout}"
    );
    assert!(
        !stdout.contains(&fx.file_key("unfiled", Some("travel"), "travel.zip")),
        "2番目root基準のkeyは現れない: {stdout}"
    );
    assert!(
        stdout.contains("unfiled"),
        "category=unfiled_category(既定値)が使われる: {stdout}"
    );
}

/// [covers:layout.key-generation-fallback-category-only]
/// fallback(設計1.2優先順位3): いずれのrootにも一致しないsource_pathは
/// rel_pathなし・category=unfiled_categoryでcategory直下へ配置される。
#[test]
fn fallback_category_only_key_generation() {
    let Some(fx) = StorageFixture::new("lay-fallback", StorageOpts::default()) else {
        return;
    };
    let outside = fx.elsewhere.join("x.zip");
    write_file(&outside, b"layout-fallback");
    let (code, stdout, stderr) = fx.push(&[&outside], &["--dry-run"], &[]);
    assert_eq!(code, 0, "dry-runはexit 0: {stdout}{stderr}");
    let key = fx.file_key("unfiled", None, "x.zip");
    assert!(
        stdout.contains(&key),
        "roots不一致はcategory直下のkey({key}): {stdout}"
    );
}

/// [covers:layout.key-generation-collision-hash-suffix]
/// 衝突検出(設計1.2・11章F10-a): 同一category+rel_path+nameに異なるrealpathが衝突する
/// 場合は <name>--<hash8>(sha256(realpath)先頭8hex・決定論的)をfolder名へ付与。
/// 無hash名の帰属: (a)registry・索引に既存の無hash保持者があれば維持・新規側にhash付与
/// (b)同一run内で同時新規はrealpath辞書順最小のみ無hash・残りはhash付き。
#[test]
fn collision_appends_deterministic_hash_suffix() {
    let Some(fx) = StorageFixture::new("lay-coll", StorageOpts::default()) else {
        return;
    };
    // auto_path_roots配下に置かない(rel_pathなし・同一category+nameでの衝突構成)
    let a = fx.elsewhere.join("a").join("dup.zip");
    let b = fx.elsewhere.join("b").join("dup.zip");
    let c = fx.elsewhere.join("c").join("dup.zip");
    write_file(&a, b"dup-a");
    write_file(&b, b"dup-b");
    write_file(&c, b"dup-c");

    // (b)同一run内3件同時新規: realpath辞書順最小(/a/.../dup.zip)のみ無hash・残りはhash付き
    let (code, stdout, stderr) = fx.push(&[&a, &b, &c], &["--dry-run"], &[]);
    assert_eq!(code, 0, "dry-runはexit 0: {stdout}{stderr}");
    let plain = fx.file_key("unfiled", None, "dup.zip");
    let hash_b = hash8_of_path(&b);
    let hash_c = hash8_of_path(&c);
    let key_b = fx.file_key("unfiled", None, &format!("dup--{hash_b}"));
    let key_c = fx.file_key("unfiled", None, &format!("dup--{hash_c}"));
    assert!(
        stdout.contains(&plain),
        "辞書順最小は無hashのまま: {stdout}"
    );
    assert!(
        stdout.contains(&key_b),
        "新規側bへhash付きkey({key_b}): {stdout}"
    );
    assert!(
        stdout.contains(&key_c),
        "新規側cへhash付きkey({key_c}): {stdout}"
    );

    // (a)別run: 既存の無hash保持者(/a/.../dup.zip)が維持され、新規参加側(/b/.../dup.zip)にhash付与
    let (code, stdout, stderr) = fx.push(&[&a], &[], &[]);
    assert_eq!(code, 0, "push(a)はexit 0: {stdout}{stderr}");
    let plain_bytes = std::fs::read(fx.object_path(&plain)).expect("無hash保持者のobject");
    let (code, stdout, stderr) = fx.push(&[&b], &[], &[]);
    assert_eq!(code, 0, "push(b)はexit 0: {stdout}{stderr}");
    let calls = fx.calls();
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={key_b}"))),
        "別runの新規参加側はhash付きkeyへPUT: {calls}"
    );
    let plain_puts = calls
        .lines()
        .filter(|l| l.starts_with("s3 PutObject") && l.contains(&format!("key={plain}")))
        .count();
    assert_eq!(
        plain_puts, 1,
        "既存無hash保持者のkeyへのPUTは初回の1回のみ: {calls}"
    );
    assert_eq!(
        std::fs::read(fx.object_path(&plain)).expect("無hash保持者のobject(再読)"),
        plain_bytes,
        "既存無hash保持者のobjectは不変"
    );
    // hash8は決定論的(同一realpathは常に同一hash)
    assert_eq!(hash8_of_path(&b), hash8_of_path(&b));
}

/// [covers:layout.rel-path-normalization]
/// rel_path正規化(設計1.2): `.`/`..`要素の排除・空要素除去・path末尾`/`禁止。
/// いずれの書き方も正規化後のrel_path(a/b)からkeyが生成される。
#[test]
fn rel_path_is_normalized_in_key_generation() {
    let Some(fx) = StorageFixture::new(
        "lay-norm",
        StorageOpts {
            files: "[files.\"n1.zip\"]\ncategory = \"photos\"\npath = \"a/./b/\"\n\
                    [files.\"n2.zip\"]\ncategory = \"photos\"\npath = \"a/b/\"\n\
                    [files.\"n3.zip\"]\ncategory = \"photos\"\npath = \"a//b\"\n"
                .into(),
            categories: "[categories.photos]\nkeep = 3\n".into(),
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
    let (code, stdout, stderr) = fx.push(&refs, &["--dry-run"], &[]);
    assert_eq!(code, 0, "dry-runはexit 0: {stdout}{stderr}");
    for name in ["n1.zip", "n2.zip", "n3.zip"] {
        let key = fx.file_key("photos", Some("a/b"), name);
        assert!(
            stdout.contains(&key),
            "正規化後rel_path(a/b)のkey({key}): {stdout}"
        );
    }
    assert!(
        !stdout.contains("a//") && !stdout.contains("/./") && !stdout.contains("b//"),
        "keyのrel_path部に空要素・`.`要素・末尾`/`は含まれない: {stdout}"
    );
}

/// [covers:layout.category-name-validation]
/// category名規則違反と予約語sys/旧表記_sysの拒否(設計1.1・1.2・4.2): unfiled_category・[categories]・
/// files.categoryの値として不正な名前・予約語(category名・files写像のpath先頭衝突)は
/// 設定エラー2(backup pushの対象解決前に弾かれS3 API呼び出しは行われない)。
#[test]
fn invalid_category_names_are_config_errors() {
    // (tag, [categories]以降へ追記するtoml断片, unfiled_category書換えが必要か)
    let cases: &[(&str, &str, bool)] = &[
        (
            "categoriesの_sys予約",
            "[categories._sys]\nkeep = 5\n",
            false,
        ),
        (
            "categoriesの名前規則違反(大文字)",
            "[categories.\"BadName\"]\nkeep = 5\n",
            false,
        ),
        ("unfiled_category設定値の名前規則違反", "", true),
        (
            "files写像pathの_sys衝突",
            "[files.\"x.zip\"]\ncategory = \"unfiled\"\npath = \"_sys/x\"\n",
            false,
        ),
        (
            "files.categoryの名前規則違反",
            "[categories.\"UPPER\"]\nkeep = 5\n[files.\"x.zip\"]\ncategory = \"UPPER\"\npath = \"a\"\n",
            false,
        ),
    ];
    for (i, (tag, extra, rewrite_unfiled)) in cases.iter().enumerate() {
        let Some(fx) = StorageFixture::new(
            &format!("lay-catval-{i}"),
            StorageOpts {
                categories: extra.to_string(),
                ..StorageOpts::default()
            },
        ) else {
            return;
        };
        if *rewrite_unfiled {
            fx.rewrite_pipeline_toml(&fx.pipeline_body().replace(
                "unfiled_category = \"unfiled\"",
                "unfiled_category = \"Unfiled\"",
            ));
        }
        let target = fx.elsewhere.join("x.zip");
        write_file(&target, b"category-validation");
        let (code, _, stderr) = fx.push(&[&target], &["--dry-run"], &[]);
        assert_eq!(code, 2, "{tag}は設定エラー2: {stderr}");
        assert_eq!(fx.calls(), "", "{tag}: S3 API呼び出しは行われない");
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
    let base = storage_aws_toml_body("it-bucket", "backup-remote/", "STANDARD", &fx.credentials);

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

    // 適正prefixは受理され、予定keyの先頭に現れる
    fx.rewrite_aws_toml(&base);
    let (code, stdout, stderr) = fx.push(&[&target], &["--dry-run"], &[]);
    assert_eq!(code, 0, "適正prefixはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("backup-remote/unfiled/x.zip/default.age"),
        "適正prefixによる予定key: {stdout}"
    );
}

/// [covers:layout.key-length-limit-1024]
/// key長検証(設計1.2・11章S3-1): 生成keyがS3のobject key上限1024 byteを超える場合は
/// 対象エラー3(keyとsource_pathを表示・files写像のpath縮約またはauto_path_roots変更を案内)。
/// 1024 byteちょうどは受理される。
#[test]
fn key_length_over_1024_is_target_error() {
    let Some(fx) = StorageFixture::new("lay-keylen", StorageOpts::default()) else {
        return;
    };
    // key = "backup-remote/" + "unfiled" + "/" + rel_path + "/" + "k.zip" + "/default.age"
    // 固定部 = 14 + 7 + 1 + 5 + 1 + 12 = 40 byte。rel_path長で1024/1025を狙う。
    let fixed =
        "backup-remote/".len() + "unfiled".len() + 1 + "k.zip".len() + 1 + "/default.age".len();
    let base_body = fx.pipeline_body();
    let make_body = |rel_len: usize| {
        let rel = "a".repeat(rel_len);
        format!("{base_body}[files.\"k.zip\"]\ncategory = \"unfiled\"\npath = \"{rel}\"\n")
    };
    let target = fx.elsewhere.join("k.zip");
    write_file(&target, b"key-length");

    // 1025 byte: 対象エラー3
    fx.rewrite_pipeline_toml(&make_body(1025 - fixed));
    let (code, stdout, stderr) = fx.push(&[&target], &["--dry-run"], &[]);
    assert_eq!(code, 3, "1025 byte keyは対象エラー3: {stdout}{stderr}");
    assert!(
        stdout.contains("1024") || stderr.contains("1024"),
        "key上限(1024)の案内表示: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("k.zip") || stderr.contains("k.zip"),
        "対象のname/source_pathが表示される: {stdout}{stderr}"
    );

    // 1024 byteちょうど: 受理される
    fx.rewrite_pipeline_toml(&make_body(1024 - fixed));
    let (code, stdout, stderr) = fx.push(&[&target], &["--dry-run"], &[]);
    assert_eq!(code, 0, "1024 byteちょうどは受理: {stdout}{stderr}");
}
