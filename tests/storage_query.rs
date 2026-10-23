//! TASK-10照会系CLI(list/query/get/diff・tmp/tasks/tm-10/design_s3_storage.md 3.1)の結合test。
//! fake seam(AMAZING_BACKUP_FAKE_S3_STATE)のbin subprocessで検証する
//! (テスト先行のため実装前の実行failは許容・cargo test --no-runは通る)。

mod common;

use common::*;

/// 照会系test共通のfixture: travel.zip(2版・rel_path付き)・album.zip(photos写像)・
/// dup.zip×2(rel_path a/bで候補複数)・x.zip(rel_pathなし)をpush済みにする。
/// TASK-18: 新規発行はtitle必須(設計6.1)のためpty promptでtitleを与え、travelの
/// 2版はcrash残存re-put経路(設計4.4)で構築する。
fn query_fixture(tag: &str) -> Option<StorageFixture> {
    let fx = StorageFixture::new(
        tag,
        StorageOpts {
            files: "[files.\"album.zip\"]\ncategory = \"photos\"\npath = \"album\"\n".into(),
            ..StorageOpts::default()
        },
    )?;
    let travel = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("travel.zip");
    write_file(&travel, b"query-travel-v1");
    let Some((c, o)) = push_titled_pty(&fx, &[&travel], "travel.zip", &[]) else {
        return None;
    };
    assert_eq!(c, 0, "push(travel v1): {o}");
    let ids = add_version_via_reput(&fx, &travel, &[]);
    assert_eq!(ids.len(), 2, "travelの2版: {ids:?}");
    let album = fx.elsewhere.join("album.zip");
    write_file(&album, b"query-album");
    let Some((c, o)) = push_titled_pty(&fx, &[&album], "album.zip", &[]) else {
        return None;
    };
    assert_eq!(c, 0, "push(album): {o}");
    for (d, body) in [("a", "query-dup-a"), ("b", "query-dup-b")] {
        let p = fx.data_root.join(d).join("dup.zip");
        write_file(&p, body.as_bytes());
        let Some((c, o)) = push_titled_pty(&fx, &[&p], "dup", &[]) else {
            return None;
        };
        assert_eq!(c, 0, "push(dup {d}): {o}");
    }
    let x = fx.elsewhere.join("x.zip");
    write_file(&x, b"query-x");
    let Some((c, o)) = push_titled_pty(&fx, &[&x], "x.zip", &[]) else {
        return None;
    };
    assert_eq!(c, 0, "push(x): {o}");
    Some(fx)
}

/// 索引の指定file entryのversionsへ変更を加える(getの版指定検証用のfixture編集)。
fn mutate_versions(fx: &StorageFixture, name: &str, f: impl Fn(&mut serde_json::Value)) {
    let mut lines = fx.read_index();
    for l in lines.iter_mut() {
        if l.get("record_type").and_then(|r| r.as_str()) == Some("file")
            && l.get("title").and_then(|n| n.as_str()) == Some(name)
        {
            if let Some(vs) = l.get_mut("versions").and_then(|v| v.as_array_mut()) {
                for v in vs.iter_mut() {
                    f(v);
                }
            }
        }
    }
    fx.rewrite_index(&lines);
}

/// [covers:cli.query-partial-match]
/// query <手がかり>(TASK-18 5.11・6.6): 索引を検索し title部分一致 /
/// uuid(完全または前方一致) / location.source_path部分一致 のentity単位要約
/// (uuid・title・locations表示)を表示。--versionsで版一覧。0件は警告+exit 0。
#[test]
fn query_finds_partial_matches() {
    let Some(fx) = titled_query_fixture("q-partial") else {
        return;
    };
    let travel_uuid = entity_uuid_of(
        &fx,
        &fx.data_root
            .join("2020-2023")
            .join("travel")
            .join("旅のしおり2026.zip"),
    );

    // title部分一致: 旅の → 当該entityの要約(uuid・title・locations)
    let (code, stdout, stderr) = fx.query("旅の", &[]);
    assert_eq!(code, 0, "query 旅の はexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("旅のしおり2026.zip"),
        "一致entityのtitle: {stdout}"
    );
    assert!(stdout.contains(&travel_uuid), "候補表示にuuid: {stdout}");
    assert!(
        stdout.contains("2020-2023/travel") || stdout.contains("旅のしおり2026.zip"),
        "所在(source_path)の表示: {stdout}"
    );

    // uuid前方一致
    let (code, stdout, stderr) = fx.query(&travel_uuid[..13], &[]);
    assert_eq!(code, 0, "query <uuid前方>はexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("旅のしおり2026.zip"),
        "uuid前方一致のentity: {stdout}"
    );

    // --versionsで版一覧(2版のversion_idが現れる)
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "uuid", &travel_uuid));
    let (code, stdout, stderr) = fx.query("旅の", &["--versions"]);
    assert_eq!(code, 0, "query --versionsはexit 0: {stdout}{stderr}");
    for vid in &vids {
        assert!(stdout.contains(vid), "版一覧にversion_id({vid}): {stdout}");
    }

    // --category Cで絞り込み(locations基準)
    let (code, stdout, stderr) = fx.query("album", &["--category", "photos"]);
    assert_eq!(code, 0, "--category photosはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("album.zip"),
        "photos絞り込みでalbum.zip: {stdout}"
    );
    let (code, stdout, stderr) = fx.query("album", &["--category", "unfiled"]);
    assert_eq!(code, 0, "不一致categoryもexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("0件") || stdout.contains("なし"),
        "0件は警告表示: {stdout}"
    );

    // 不一致手がかりは警告+exit 0
    let (code, stdout, stderr) = fx.query("zzz-no-match", &[]);
    assert_eq!(code, 0, "0件はexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("0件") || stdout.contains("なし"),
        "0件は警告表示: {stdout}"
    );
}

/// [covers:cli.query-candidate-paths]
/// queryで複数候補がhitした場合は全件表示(uuid・title・locations)+--path特定の案内
/// (TASK-18 5.11)。title重複(別uuid同title)があれば警告行を出す(一意化はuuid/所在指定)。
#[test]
fn query_multiple_candidates_shows_all_with_path_hint() {
    let Some(fx) = titled_query_fixture("q-cand") else {
        return;
    };
    let (code, stdout, stderr) = fx.query("dup", &[]);
    assert_eq!(code, 0, "query dupはexit 0: {stdout}{stderr}");
    let uuids: Vec<String> = fx
        .read_index()
        .iter()
        .filter(|v| v.get("record_type").and_then(|r| r.as_str()) == Some("file"))
        .filter(|v| v.get("title").and_then(|t| t.as_str()) == Some("dup"))
        .filter_map(|v| v.get("uuid").and_then(|u| u.as_str()).map(String::from))
        .collect();
    assert_eq!(uuids.len(), 2, "title=dupの2entity: {uuids:?}");
    for u in &uuids {
        assert!(
            stdout.contains(u.as_str()),
            "一致した全entity(uuid={u})が表示される: {stdout}"
        );
    }
    let dup_a = fx.data_root.join("a").join("dup.zip");
    let dup_b = fx.data_root.join("b").join("dup.zip");
    assert!(
        stdout.contains(dup_a.to_string_lossy().as_ref()),
        "候補aの所在表示: {stdout}"
    );
    assert!(
        stdout.contains(dup_b.to_string_lossy().as_ref()),
        "候補bの所在表示: {stdout}"
    );
    assert!(
        stdout.contains("--path"),
        "--path <source_path>での特定を求める案内: {stdout}"
    );
    assert!(
        stdout.contains("重複") || stdout.contains("duplicate"),
        "title重複の警告行: {stdout}"
    );
}

/// [covers:cli.get-latest-version]
/// [covers:cli.get-output-structure]
/// get <term>(TASK-18 5.9・6.6): 候補はtitle完全一致またはuuid完全一致。
/// 版指定なしは最新版(versions先頭=date降順の最新)を取得・検証・復号。
/// 復元先は<out>/<title>(dest_name=entry.title・rel_path廃止)。
#[test]
fn get_defaults_to_latest_and_reproduces_layout() {
    let Some(fx) = titled_query_fixture("q-getlatest") else {
        return;
    };
    let travel = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("旅のしおり2026.zip");
    let travel_uuid = entity_uuid_of(&fx, &travel);
    let out = fx.elsewhere.join("restore-out");

    // title完全一致
    let (code, stdout, stderr) =
        fx.get("旅のしおり2026.zip", &["--out", &out.display().to_string()]);
    assert_eq!(code, 0, "get <title>はexit 0: {stdout}{stderr}");
    let restored = out.join("旅のしおり2026.zip");
    assert!(
        restored.is_file(),
        "復元先は<out>/<title>: {}",
        restored.display()
    );
    assert_eq!(
        std::fs::read(&restored).expect("復号成果物"),
        b"query-travel".to_vec(),
        "版指定なしの取得・復号"
    );

    // uuid完全一致(同一entity)
    let out2 = fx.elsewhere.join("restore-out2");
    let (code, stdout, stderr) = fx.get(&travel_uuid, &["--out", &out2.display().to_string()]);
    assert_eq!(code, 0, "get <uuid>はexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out2.join("旅のしおり2026.zip")).expect("uuid指定の復号成果物"),
        b"query-travel".to_vec(),
        "uuid完全一致でも同一entityを取得"
    );
}

/// [covers:cli.get-specific-version]
/// [covers:cli.get-version-ambiguous-requires-version-id]
/// getの版特定規則(設計3.1・2.1): --version V(日付)はversions[].dateでfilterし1件なら採用。
/// 同日内に複数版がhitしたら候補(date・version_ts表示)を出して--version-idでの特定を
/// 求める(対象エラー3)。日付形式不正はusage error 64(cli.date-format-usage-errorの引継ぎ)。
#[test]
fn get_version_selection_by_date_and_version_id() {
    let Some(fx) = query_fixture("q-getver") else {
        return;
    };
    let today = jst_today();
    let yesterday = jst_date_offset(-1);
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "title", "travel.zip"));
    assert_eq!(vids.len(), 2, "travel.zipは同日2版: {vids:?}");
    let (latest, older) = (&vids[0], &vids[1]);
    let out = fx.elsewhere.join("getver-out");

    // 同日内複数版のhitは候補表示+--version-id要求で対象エラー3
    let (code, stdout, stderr) = fx.get(
        "travel.zip",
        &["--version", &today, "--out", &out.display().to_string()],
    );
    assert_eq!(
        code, 3,
        "同日複数版の--versionは対象エラー3: {stdout}{stderr}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("--version-id"),
        "--version-idでの特定を求める案内: {combined}"
    );
    for vid in [latest, older] {
        assert!(
            combined.contains(vid.as_str()),
            "候補にversion_id({vid})表示: {combined}"
        );
    }

    // --version-idでの特定: 当該版が取得される(復元先は<out>/<title>・設計5.9)
    let (code, stdout, stderr) = fx.get(
        "travel.zip",
        &["--version-id", older, "--out", &out.display().to_string()],
    );
    assert_eq!(code, 0, "get --version-idはexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out.join("travel.zip")).expect("旧版の復号成果物"),
        b"query-travel-v1".to_vec(),
        "--version-id指定の旧版が取得される"
    );

    // 日付filterで1件のcase: 旧版の日付を昨日へ書き換えた索引で--version <昨日>が1件採用
    mutate_versions(&fx, "travel.zip", &|v: &mut serde_json::Value| {
        if v.get("version_id").and_then(|i| i.as_str()) == Some(older.as_str()) {
            if let Some(obj) = v.as_object_mut() {
                obj.insert("date".into(), serde_json::Value::String(yesterday.clone()));
            }
        }
    });
    let (code, stdout, stderr) = fx.get(
        "travel.zip",
        &["--version", &yesterday, "--out", &out.display().to_string()],
    );
    assert_eq!(
        code, 0,
        "get --version <1件hitの日付>はexit 0: {stdout}{stderr}"
    );
    assert_eq!(
        std::fs::read(out.join("travel.zip")).expect("旧版"),
        b"query-travel-v1".to_vec(),
        "--version日付filterで旧版が採用される"
    );

    // 日付形式不正はusage error 64
    for bad in ["2026-1-1", "20261002"] {
        let (code, _, _) = fx.get(
            "travel.zip",
            &["--version", bad, "--out", &out.display().to_string()],
        );
        assert_eq!(code, 64, "--version {bad}の形式不正はusage error 64");
    }
}

/// [covers:cli.get-pin]
/// get --pin 名(設計3.1): 該当pin object(pins[].s3_key)を取得し、pin記録のsha256で
/// 検証して復号する。
#[test]
fn get_pin_fetches_pin_object() {
    let Some(fx) = query_fixture("q-getpin") else {
        return;
    };
    assert_eq!(
        fx.pin("add", "travel.zip", &["--latest", "--as", "pre-migration"])
            .0,
        0
    );
    let travel = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("travel.zip");
    let pin_key = fx.data_pin_key(&entity_uuid_of(&fx, &travel), "pre-migration");
    let out = fx.elsewhere.join("getpin-out");
    let out_s = out.display().to_string();
    let (code, stdout, stderr) = fx.get("travel.zip", &["--pin", "pre-migration", "--out", &out_s]);
    assert_eq!(code, 0, "get --pinはexit 0: {stdout}{stderr}");
    assert_eq!(
        std::fs::read(out.join("travel.zip")).expect("pin版の復号成果物"),
        b"query-travel-v1".to_vec(),
        "pin objectの内容が検証・復号されて出力される"
    );
    assert!(
        fx.calls()
            .lines()
            .any(|l| l.starts_with("s3 GetObject") && l.contains(&format!("key={pin_key}"))),
        "pin keyのobjectが取得される: {}",
        fx.calls()
    );
}

/// [covers:cli.get-issues-restore-job]
/// 未復元版へのget(設計3.1・6章): 復元jobを発行し「進行中・約12〜48h後に再実行」と案内して
/// exit 0(冪等・再実行で再発行しない)。--tier/--daysが反映され省略時はtier=Standard・days=7。
#[test]
fn get_on_unrestored_version_issues_restore_job() {
    let Some(fx) = StorageFixture::new(
        "q-getrestore",
        StorageOpts {
            storage_class: "DEEP_ARCHIVE",
            ..StorageOpts::default()
        },
    ) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.data_root.join("b.zip");
    let c = fx.data_root.join("c.zip");
    write_file(&a, b"get-restore-a");
    write_file(&b, b"get-restore-b");
    write_file(&c, b"get-restore-c");
    for p in [&a, &b, &c] {
        let Some((code, stdout)) =
            push_titled_pty(&fx, &[p], &p.file_name().unwrap().to_string_lossy(), &[])
        else {
            return;
        };
        assert_eq!(code, 0, "初回push: {stdout}");
    }
    let key_a = fx.data_key(&uuid_of_path(&fx, &a));
    let key_b = fx.data_key(&uuid_of_path(&fx, &b));
    let key_c = fx.data_key(&uuid_of_path(&fx, &c));
    // cは復元job進行中(ongoing)状態
    fx.seed_restore(&key_c, "ongoing");
    let out = fx.elsewhere.join("getrestore-out");
    let out_s = out.display().to_string();

    // 明示指定: --days 3 --tier Bulk
    let (code, stdout, stderr) =
        fx.get("a.zip", &["--days", "3", "--tier", "Bulk", "--out", &out_s]);
    assert_eq!(code, 0, "未復元版へのgetはexit 0: {stdout}{stderr}");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("再実行"),
        "進行中・再実行の案内: {combined}"
    );
    let calls = fx.calls();
    let restore_a = calls
        .lines()
        .find(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={key_a}")))
        .unwrap_or_else(|| panic!("a.zipへのRestoreObject: {calls}"));
    assert!(
        restore_a.contains("days=3") && restore_a.contains("tier=Bulk"),
        "--days/--tier指定値の反映: {restore_a}"
    );

    // 省略時: tier=Standard・days=7
    let (code, stdout, stderr) = fx.get("b.zip", &["--out", &out_s]);
    assert_eq!(code, 0, "get b.zipはexit 0: {stdout}{stderr}");
    let calls = fx.calls();
    let restore_b = calls
        .lines()
        .find(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={key_b}")))
        .unwrap_or_else(|| panic!("b.zipへのRestoreObject: {calls}"));
    assert!(
        restore_b.contains("days=7") && restore_b.contains("tier=Standard"),
        "省略時の既定(days=7・tier=Standard): {restore_b}"
    );

    // 進行中の再実行では再発行されない(cのみ案内)
    let (code, stdout, stderr) = fx.get("c.zip", &["--out", &out_s]);
    assert_eq!(code, 0, "進行中のgetはexit 0: {stdout}{stderr}");
    let calls = fx.calls();
    assert!(
        !calls
            .lines()
            .any(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={key_c}"))),
        "進行中オブジェクトへの再発行はしない: {calls}"
    );

    // aの再実行: RestoreObjectの発行は1回のまま(冪等)。fakeは発行でrestoredへ遷移するため
    // 再実行では取得・復号まで進む
    let restores_a = fx
        .calls()
        .lines()
        .filter(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={key_a}")))
        .count();
    let (code, stdout, stderr) =
        fx.get("a.zip", &["--days", "3", "--tier", "Bulk", "--out", &out_s]);
    assert_eq!(code, 0, "再実行はexit 0: {stdout}{stderr}");
    let restores_a_after = fx
        .calls()
        .lines()
        .filter(|l| l.starts_with("s3 RestoreObject") && l.contains(&format!("key={key_a}")))
        .count();
    assert_eq!(
        restores_a_after,
        restores_a,
        "再実行で復元jobを再発行しない: {}",
        fx.calls()
    );
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("復元済み版の取得成果物"),
        b"get-restore-a".to_vec(),
        "復元済み版は再実行で取得・復号される"
    );
}

/// [covers:cli.get-sha-mismatch-error]
/// getのsha256検証は索引が正本(設計3.1): 取得objectのage_sha256が索引記録と不一致、
/// または復号平文のsource_sha256が索引記録と不一致は検証エラー4。
#[test]
fn get_rejects_tampered_object_with_verify_error() {
    let Some(fx) = StorageFixture::new("q-getsha", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"get-sha-original");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    // S3側の対象objectを改変する
    fx.seed_object(
        &fx.data_key(&uuid_of_path(&fx, &a)),
        b"tampered-object-bytes",
    );
    let out = fx.elsewhere.join("getsha-out");
    let (code, stdout, stderr) = fx.get("a.zip", &["--out", &out.display().to_string()]);
    assert_eq!(code, 4, "sha不一致は検証エラー4: {stdout}{stderr}");
}

/// [covers:cli.get-nonversioned-fallback]
/// 索引のversion_idが無いobject(非versioned・移行途中の混在)へのgetは
/// CurrentVersion取得へfall backする(設計8.4)。
#[test]
fn get_falls_back_to_current_version_without_version_id() {
    let Some(fx) = StorageFixture::new("q-getfallback", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    write_file(&a, b"get-fallback-plain");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a], "a.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "初回push: {stdout}");
    // 索引からversion_idを除去(非versioned objectの混在を模倣)
    mutate_versions(&fx, "a.zip", &|v: &mut serde_json::Value| {
        if let Some(obj) = v.as_object_mut() {
            obj.remove("version_id");
        }
    });
    let out = fx.elsewhere.join("getfallback-out");
    let (code, stdout, stderr) = fx.get("a.zip", &["--out", &out.display().to_string()]);
    assert_eq!(
        code, 0,
        "version_id不在でもCurrentVersion取得へfall back: {stdout}{stderr}"
    );
    assert_eq!(
        std::fs::read(out.join("a.zip")).expect("復号成果物"),
        b"get-fallback-plain".to_vec(),
        "現行versionの内容が取得される"
    );
}

/// [covers:cli.list-files-summary]
/// list files(TASK-18 5.11): entity毎に
/// `title  uuid(先頭8字)  所在n  {v}版  latest {date} ({size})  pins=n`の目録表示
/// (値検証: 行の完全一致)。--categoryはlocations filter(いずれかのlocation.category一致)。
/// --treeは廃止。title重複があれば警告行。0件は警告+exit 0。
#[test]
fn list_files_shows_entity_summary() {
    let Some(fx) = titled_query_fixture("q-listfiles") else {
        return;
    };
    let today = jst_today();
    let travel = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("旅のしおり2026.zip");
    let uuid8 = |path: &std::path::Path| entity_uuid_of(&fx, path)[..8].to_string();
    // entity毎の期待行(title  uuid先頭8字  所在n  v版  latest日付 (size B)  pins=n)
    let travel_line = format!(
        "旅のしおり2026.zip  {}  所在1  2版  latest {today} ({} B)  pins=0",
        uuid8(&travel),
        "query-travel".len()
    );
    let album_line = format!(
        "album.zip  {}  所在1  1版  latest {today} ({} B)  pins=0",
        uuid8(&fx.elsewhere.join("album.zip")),
        "query-album".len()
    );
    let dup_a_line = format!(
        "dup  {}  所在1  1版  latest {today} ({} B)  pins=0",
        uuid8(&fx.data_root.join("a").join("dup.zip")),
        "query-dup-a".len()
    );
    let dup_b_line = format!(
        "dup  {}  所在1  1版  latest {today} ({} B)  pins=0",
        uuid8(&fx.data_root.join("b").join("dup.zip")),
        "query-dup-b".len()
    );
    let x_line = format!(
        "x.zip  {}  所在1  1版  latest {today} ({} B)  pins=0",
        uuid8(&fx.elsewhere.join("x.zip")),
        "query-x".len()
    );

    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "list filesはexit 0: {stdout}{stderr}");
    for line in [&travel_line, &album_line, &dup_a_line, &dup_b_line, &x_line] {
        assert!(
            stdout.lines().any(|l| l == *line),
            "entity行の完全一致: 期待={line} 実際={stdout}"
        );
    }
    assert!(
        stdout.contains("重複") || stdout.contains("duplicate"),
        "title重複(dup)の警告行: {stdout}"
    );

    // --category絞り込みはlocations基準: photos=albumのみ
    let (code, stdout, stderr) = fx.list("files", &["--category", "photos"]);
    assert_eq!(code, 0, "list files --category photos: {stdout}{stderr}");
    assert!(
        stdout.lines().any(|l| l == album_line),
        "--category photosはalbumのみ: {stdout}"
    );
    assert!(
        !stdout.contains("旅のしおり2026.zip"),
        "絞り込み外のentityは出ない: {stdout}"
    );
    // 不一致categoryは0件警告
    let (code, stdout, stderr) = fx.list("files", &["--category", "ghost"]);
    assert_eq!(code, 0, "不一致categoryもexit 0: {stdout}{stderr}");
    assert!(stdout.contains("0件"), "0件は警告表示: {stdout}");

    // 空索引(0件)は警告+exit 0
    let mut header = fx
        .read_index()
        .into_iter()
        .find(|v| v.get("record_type").and_then(|r| r.as_str()) == Some("index_header"))
        .expect("index_header");
    if let Some(obj) = header.as_object_mut() {
        obj.insert("entries".into(), serde_json::Value::from(0));
    }
    fx.rewrite_index(&[header]);
    let (code, stdout, stderr) = fx.list("files", &[]);
    assert_eq!(code, 0, "空索引もexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("0件") || stdout.contains("なし"),
        "0件は警告表示: {stdout}"
    );
}

/// [covers:cli.list-versions-missing-name-error]
/// list versions(TASK-18 5.11・6.6): entity毎の版一覧(date・version_id・size・
/// noncurrent/現行・pin紐付)を表示。--title <term>(title完全一致・uuid完全一致も受理)で
/// 指定entityが索引に不在は対象エラー3。
#[test]
fn list_versions_shows_versions_and_rejects_missing_title() {
    let Some(fx) = titled_query_fixture("q-listver") else {
        return;
    };
    let travel = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("旅のしおり2026.zip");
    let travel_uuid = entity_uuid_of(&fx, &travel);
    let (code, stdout, stderr) = fx.list("versions", &[]);
    assert_eq!(code, 0, "list versionsはexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("旅のしおり2026.zip"),
        "entity単位の表示: {stdout}"
    );

    let vids = index_version_ids(index_entity_by(&fx.read_index(), "uuid", &travel_uuid));
    let (code, stdout, stderr) = fx.list("versions", &["--title", "旅のしおり2026.zip"]);
    assert_eq!(code, 0, "list versions --titleはexit 0: {stdout}{stderr}");
    for vid in &vids {
        assert!(
            stdout.contains(vid.as_str()),
            "版一覧のversion_id({vid}): {stdout}"
        );
    }
    // uuid完全一致でも指定できる
    let (code, stdout, stderr) = fx.list("versions", &["--title", &travel_uuid]);
    assert_eq!(
        code, 0,
        "list versions --title <uuid>はexit 0: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("旅のしおり2026.zip"),
        "uuid指定の表示: {stdout}"
    );
    // 版の現行/noncurrent表示
    assert!(
        stdout.contains("現行") && stdout.contains("noncurrent"),
        "現行/noncurrentの別: {stdout}"
    );
    // pin紐付行(pin add後に「pin <名>」行が出る)
    assert!(
        fx.pin(
            "add",
            "旅のしおり2026.zip",
            &["--latest", "--as", "ver-pin"]
        )
        .0 == 0
    );
    let (code, stdout, stderr) = fx.list("versions", &["--title", "旅のしおり2026.zip"]);
    assert_eq!(code, 0, "pin add後のlist versions: {stdout}{stderr}");
    assert!(stdout.contains("pin ver-pin"), "pin紐付の表示: {stdout}");

    // --category絞り込み
    let (code, stdout, stderr) = fx.list("versions", &["--category", "photos"]);
    assert_eq!(code, 0, "list versions --category photos: {stdout}{stderr}");
    assert!(stdout.contains("album.zip"), "photosのentity: {stdout}");
    assert!(
        !stdout.contains("旅のしおり2026.zip"),
        "絞り込み外は出ない: {stdout}"
    );

    let (code, _, stderr) = fx.list("versions", &["--title", "ghost.zip"]);
    assert_eq!(code, 3, "不在titleは対象エラー3: {stderr}");
    assert!(stderr.contains("ghost.zip"), "不在titleの案内: {stderr}");
}

/// [covers:cli.list-jobs]
/// list jobs(設計3.1・TASK-11 Phase B): 復元job状況を全namespaceで表示(headのRestore
/// header parse。旧restore status相当・--dateによるprefix限定なし)。現行versionに加え
/// noncurrent版(version_id付き)の復元状態も表示する(版指定取得の前提)。進行中・復元済み・
/// 未発行の混在一覧。
#[test]
fn list_jobs_shows_restore_status_without_date_filter() {
    let Some(fx) = StorageFixture::new("q-listjobs", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.data_root.join("b.zip");
    let c = fx.data_root.join("c.zip");
    write_file(&a, b"jobs-a");
    write_file(&b, b"jobs-b");
    write_file(&c, b"jobs-c");
    for p in [&a, &b, &c] {
        let Some((code, stdout)) =
            push_titled_pty(&fx, &[p], &p.file_name().unwrap().to_string_lossy(), &[])
        else {
            return;
        };
        assert_eq!(code, 0, "初回push: {stdout}");
    }
    // a.zipをもう1版(crash残存re-put経路でnoncurrent版を作る・同key別version_id)
    let ids = add_version_via_reput(&fx, &a, &[]);
    assert_eq!(ids.len(), 2, "a.zipの2版: {ids:?}");
    let key_a = fx.data_key(&uuid_of_path(&fx, &a));
    let key_b = fx.data_key(&uuid_of_path(&fx, &b));
    let key_c = fx.data_key(&uuid_of_path(&fx, &c));
    fx.seed_restore(&key_a, "ongoing");
    fx.seed_restore(&key_b, "restored");
    // cはrestore metaなし=未発行

    let (code, stdout, stderr) = fx.list("jobs", &[]);
    assert_eq!(code, 0, "list jobsはexit 0: {stdout}{stderr}");
    for (key, state) in [
        (&key_a, "ongoing"),
        (&key_b, "restored"),
        (&key_c, "not-requested"),
    ] {
        let line = stdout
            .lines()
            .find(|l| l.contains(key.as_str()))
            .unwrap_or_else(|| panic!("対象key({key})の行: {stdout}"));
        assert!(line.contains(state), "復元状態({state}): {line}");
    }

    // a.zipは2版(同key): 現行版とnoncurrent版がversion_id付きで並ぶ(get --version-id・
    // pin対象等の版指定取得の前提)。fakeのrestore状態はkey単位のため両行とも同一状態でよい
    let vids = index_version_ids(index_entity_by(&fx.read_index(), "title", "a.zip"));
    assert_eq!(vids.len(), 2, "a.zipは2版: {vids:?}");
    for vid in &vids {
        let line = stdout
            .lines()
            .find(|l| l.contains(key_a.as_str()) && l.contains(vid.as_str()))
            .unwrap_or_else(|| {
                panic!("a.zipの版({vid})の行(現行・noncurrentともversion_id付き): {stdout}")
            });
        assert!(line.contains("ongoing"), "版行にも復元状態: {line}");
    }
}

/// [covers:cli.diff-index-vs-s3]
/// diff(TASK-18 5.11): 索引↔S3実態(ListObjectVersions API)の突合。
/// 突合対象は索引記載key(=default.age・pin key)のみとし、{prefix}data/配下の
/// metadata_接頭辞file名はsys/同様に対象外(索引管理外)。run完否・missing/unexpected/
/// pins存在を報告。不一致ありもexit 0(照会command)。
#[test]
fn diff_reports_index_vs_s3_differences() {
    let Some(fx) = titled_query_fixture("q-diff") else {
        return;
    };
    let x_uuid = entity_uuid_of(&fx, &fx.elsewhere.join("x.zip"));
    let x_key = fx.data_key(&x_uuid);
    // 索引にS3不在のkey(missing)・S3側に索引未知のobject(unexpected)を作る
    std::fs::remove_file(fx.object_path(&x_key)).expect("S3側のx.zip objectを削除");
    let ghost_key = fx.data_key("00000000-0000-7000-8000-0000000000ff");
    fx.seed_object(&ghost_key, b"unexpected-object");
    // metadata objectは存在する(pushのrun末尾syncで配置済み)
    assert!(
        fx.object_path(&fx.metadata_key(&x_uuid, "x.zip")).is_file(),
        "metadata objectが存在する前提"
    );

    let (code, stdout, stderr) = fx.diff(&[]);
    assert_eq!(code, 0, "diffは不一致ありもexit 0: {stdout}{stderr}");
    assert!(stdout.contains("missing"), "missing報告: {stdout}");
    assert!(stdout.contains("unexpected"), "unexpected報告: {stdout}");
    assert!(stdout.contains(&x_key), "missing対象keyの表示: {stdout}");
    assert!(
        stdout.contains(&ghost_key),
        "unexpected対象keyの表示: {stdout}"
    );
    assert!(
        !stdout.contains("metadata_"),
        "metadata_接頭辞keyは突合対象外として報告されない: {stdout}"
    );
}

/// [covers:cli.diff-run-completeness]
/// diff --run ID(設計3.1): 索引・manifest・S3実態の三方突合でrun(batch)の完否を監査(UC-8)。
/// manifest欠損(object不在)も報告のみ(exit 0)。
#[test]
fn diff_run_audits_run_completeness() {
    let Some(fx) = StorageFixture::new("q-diffrun", StorageOpts::default()) else {
        return;
    };
    let a = fx.data_root.join("a.zip");
    let b = fx.data_root.join("b.zip");
    let c = fx.data_root.join("c.zip");
    write_file(&a, b"diff-run-a");
    write_file(&b, b"diff-run-b");
    write_file(&c, b"diff-run-c");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&a, &b], "a.zip\nb.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "run1のpush: {stdout}");
    let Some((code, stdout)) = push_titled_pty(&fx, &[&c], "c.zip", &[]) else {
        return;
    };
    assert_eq!(code, 0, "run2のpush: {stdout}");
    let mut batch_ids: Vec<String> = Vec::new();
    for r in fx.registry_values() {
        if r.get("record_type").and_then(|t| t.as_str()) == Some("file_write") {
            if let Some(id) = r.get("batch_id").and_then(|v| v.as_str()) {
                if !batch_ids.contains(&id.to_string()) {
                    batch_ids.push(id.to_string());
                }
            }
        }
    }
    assert_eq!(batch_ids.len(), 2, "2 run分のbatch_id: {batch_ids:?}");
    let run1 = &batch_ids[0];

    let (code, stdout, stderr) = fx.diff(&["--run", run1]);
    assert_eq!(code, 0, "diff --runはexit 0: {stdout}{stderr}");
    assert!(stdout.contains(run1.as_str()), "run完否の報告: {stdout}");

    // manifest欠損(object不在)のあるrunも報告のみでexit 0
    let manifest_key = format!("{}sys/manifests/{run1}.jsonl.age", fx.prefix);
    std::fs::remove_file(fx.object_path(&manifest_key)).expect("manifest objectの削除");
    let (code, stdout, stderr) = fx.diff(&["--run", run1]);
    assert_eq!(code, 0, "manifest欠損も報告のみでexit 0: {stdout}{stderr}");
    assert!(
        stdout.contains("missing") || stdout.contains("manifest"),
        "manifest欠損の報告: {stdout}"
    );
}

/// [covers:cli.get-candidates-error]
/// get <term>(TASK-18 5.9・6.6): 候補はtitle/uuid完全一致。title重複(別uuid)の
/// 2候補があり--path未指定は対象エラー3(候補一覧表示(source_path付き)+--path特定の案内)。
/// --pathはlocation.source_path完全一致で一意化し当該候補を取得できる
/// (復元先は<out>/<title>)。
#[test]
fn get_multiple_candidates_requires_path() {
    let Some(fx) = titled_query_fixture("q-getcand") else {
        return;
    };
    let dup_a = fx.data_root.join("a").join("dup.zip");
    let dup_b = fx.data_root.join("b").join("dup.zip");
    let out = fx.elsewhere.join("getcand-out");
    let (code, stdout, stderr) = fx.get("dup", &["--out", &out.display().to_string()]);
    assert_eq!(
        code, 3,
        "title重複候補で--pathなしは対象エラー3: {stdout}{stderr}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(combined.contains("--path"), "--path特定の案内: {combined}");
    assert!(
        combined.contains(dup_a.to_string_lossy().as_ref()),
        "候補一覧にsource_path付き表示(a): {combined}"
    );
    assert!(
        combined.contains(dup_b.to_string_lossy().as_ref()),
        "候補一覧にsource_path付き表示(b): {combined}"
    );

    // --path <source_path>完全一致で候補aを特定して取得できる
    let (code, stdout, stderr) = fx.get(
        "dup",
        &[
            "--path",
            &dup_a.to_string_lossy(),
            "--out",
            &out.display().to_string(),
        ],
    );
    assert_eq!(
        code, 0,
        "get --path <aのsource_path>はexit 0: {stdout}{stderr}"
    );
    assert_eq!(
        std::fs::read(out.join("dup")).expect("候補aの復号成果物"),
        b"query-dup-a".to_vec(),
        "--path特定の候補が取得され<out>/<title>へ配置される"
    );
}

/// TASK-18改訂系test共通のfixture: title付きentity群をpush済みにする
/// (tmp/tasks/tm-18/design_key-uuid-versioning.md 6.6の探索語彙)。
/// - 旅のしおり2026.zip: data_root/2020-2023/travel/配下・2版(crash残存re-put経路)
/// - album.zip: files写像photos・elsewhere配下
/// - dup×2: 同一title「dup」の別内容2entity(title重複構成)
/// - x.zip: elsewhere配下
fn titled_query_fixture(tag: &str) -> Option<StorageFixture> {
    let fx = StorageFixture::new(
        tag,
        StorageOpts {
            files: "[files.\"album.zip\"]\ncategory = \"photos\"\npath = \"album\"\n".into(),
            no_categories: true,
            aws_lifecycle: aws_lifecycle_toml(180, 10, 7),
            ..StorageOpts::default()
        },
    )?;
    let travel = fx
        .data_root
        .join("2020-2023")
        .join("travel")
        .join("旅のしおり2026.zip");
    write_file(&travel, b"query-travel");
    let album = fx.elsewhere.join("album.zip");
    write_file(&album, b"query-album");
    let dup_a = fx.data_root.join("a").join("dup.zip");
    let dup_b = fx.data_root.join("b").join("dup.zip");
    write_file(&dup_a, b"query-dup-a");
    write_file(&dup_b, b"query-dup-b");
    let x = fx.elsewhere.join("x.zip");
    write_file(&x, b"query-x");
    let paths = [&travel, &album, &dup_a, &dup_b, &x];
    let path_refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    let Some((code, stdout)) = push_titled_pty(
        &fx,
        &path_refs,
        "旅のしおり2026.zip\nalbum.zip\ndup\ndup\nx.zip",
        &[],
    ) else {
        return None;
    };
    assert_eq!(code, 0, "初回push(titleはprompt): {stdout}");
    // travelへ2版目: 通常runは1実体1版のためcrash残存re-put経路(設計4.4・
    // 各版のfile_upload記録を保持)で同一entity(default.age)へ版を重ねる
    let ids = add_version_via_reput(&fx, &travel, &[]);
    assert_eq!(ids.len(), 2, "travelの2版: {ids:?}");
    Some(fx)
}

/// registryのfile_write記録からsource_pathでuuidを引く。
fn entity_uuid_of(fx: &StorageFixture, path: &std::path::Path) -> String {
    let canonical = std::fs::canonicalize(path).expect("realpath");
    fx.registry_values()
        .into_iter()
        .find(|r| {
            r.get("record_type").and_then(|t| t.as_str()) == Some("file_write")
                && r.get("source_path").and_then(|p| p.as_str())
                    == Some(canonical.to_string_lossy().as_ref())
        })
        .and_then(|r| r.get("uuid").and_then(|u| u.as_str()).map(String::from))
        .unwrap_or_else(|| panic!("file_write.uuid({})", canonical.display()))
}

/// [covers:cli.format-option-default-human]
/// --format human|json(照会系共通・TASK-11 Phase B): --format省略時はhuman
/// (現行の表示形式)。省略時と--format human明示時は同一の出力・終了コード。
#[test]
fn format_option_defaults_to_human() {
    let Some(fx) = query_fixture("q-fmtdef") else {
        return;
    };
    let out1 = fx.elsewhere.join("fmtdef-out1").display().to_string();
    let out2 = fx.elsewhere.join("fmtdef-out2").display().to_string();
    // (tag, --format省略の実行, --format human明示の実行)
    let cases: Vec<(&str, (i32, String, String), (i32, String, String))> = vec![
        (
            "list files",
            fx.list("files", &[]),
            fx.list("files", &["--format", "human"]),
        ),
        (
            "query",
            fx.query("travel", &[]),
            fx.query("travel", &["--format", "human"]),
        ),
        (
            "get",
            fx.get("travel.zip", &["--out", &out1]),
            fx.get("travel.zip", &["--out", &out2, "--format", "human"]),
        ),
        ("diff", fx.diff(&[]), fx.diff(&["--format", "human"])),
    ];
    for (tag, (code_p, out_p, err_p), (code_h, out_h, err_h)) in cases {
        assert_eq!(
            code_p, code_h,
            "{tag}: --format省略とhuman明示は同一終了コード"
        );
        assert_eq!(out_p, out_h, "{tag}: --format省略とhuman明示は同一出力");
        assert_eq!(err_p, err_h, "{tag}: stderrも同一");
        assert_eq!(code_p, 0, "{tag}: 実行自体はexit 0: {out_p}{err_p}");
    }
}

/// [covers:cli.format-json-single-json]
/// --format json(照会系・TASK-11 Phase B・TASK-18 6.5でcategoriesを除去):
/// list files/versions/pins/runs/jobs・query・get・diffのstdout全体が機械可読な単一JSON
/// (parse可能な1つのJSON value)。内容はhuman形式と同一の対象データ。警告・案内等の
/// 人間向け通知(FAKE S3 MODE等)はstderrへ出しstdoutのJSONを壊さない。
/// 終了コードはhumanと同一。0件は空一覧を表す構造。
#[test]
fn format_json_outputs_single_json() {
    let Some(fx) = query_fixture("q-fmtjson") else {
        return;
    };
    let out1 = fx.elsewhere.join("fmtjson-out1").display().to_string();
    let out2 = fx.elsewhere.join("fmtjson-out2").display().to_string();
    // (tag, human実行, json実行)の対・照会系8コマンド(list categoriesは廃止)
    let human: Vec<(&str, (i32, String, String))> = vec![
        ("files", fx.list("files", &[])),
        ("versions", fx.list("versions", &[])),
        ("pins", fx.list("pins", &[])),
        ("runs", fx.list("runs", &[])),
        ("jobs", fx.list("jobs", &[])),
        ("query", fx.query("travel", &[])),
        ("get", fx.get("travel.zip", &["--out", &out1])),
        ("diff", fx.diff(&[])),
    ];
    let json: Vec<(&str, (i32, String, String))> = vec![
        ("files", fx.list("files", &["--format", "json"])),
        ("versions", fx.list("versions", &["--format", "json"])),
        ("pins", fx.list("pins", &["--format", "json"])),
        ("runs", fx.list("runs", &["--format", "json"])),
        ("jobs", fx.list("jobs", &["--format", "json"])),
        ("query", fx.query("travel", &["--format", "json"])),
        (
            "get",
            fx.get("travel.zip", &["--out", &out2, "--format", "json"]),
        ),
        ("diff", fx.diff(&["--format", "json"])),
    ];
    assert_eq!(human.len(), 8, "照会系8コマンド(list categoriesは廃止)");
    for ((tag, (code_h, _, _)), (_, (code_j, out_j, err_j))) in human.iter().zip(json.iter()) {
        assert_eq!(
            code_j, code_h,
            "{tag}: --format jsonの終了コードはhumanと同一: {out_j}{err_j}"
        );
        // stdout全体が単一JSONとしてparseできる(人間向け通知が混ざればparse失敗する)
        let value: serde_json::Value = serde_json::from_str(out_j.trim())
            .unwrap_or_else(|e| panic!("{tag}: stdoutは単一JSON: parse error {e}: {out_j}"));
        // 警告・案内(FAKE S3 MODE等)はstderr側に出る(stdoutのJSONを壊さない)
        assert!(
            err_j.contains("FAKE S3 MODE"),
            "{tag}: fake seam警告はstderrへ: {err_j}"
        );
        // humanと同一の対象データ(代表要素の包含確認。jobsのhuman表示はkey単位の
        // ためdefault.ageの包含で確認・TASK-18 5.11)
        if *tag != "get" {
            let serialized = serde_json::to_string(&value).unwrap();
            assert!(
                serialized.contains("travel.zip")
                    || serialized.contains("unfiled")
                    || serialized.contains("default.age"),
                "{tag}: JSONはhumanと同一データを含む: {serialized}"
            );
        }
    }

    // 0件は空一覧を表す構造(空索引でのlist files --format json)
    let mut header = fx
        .read_index()
        .into_iter()
        .find(|v| v.get("record_type").and_then(|r| r.as_str()) == Some("index_header"))
        .expect("index_header");
    if let Some(obj) = header.as_object_mut() {
        obj.insert("entries".into(), serde_json::Value::from(0));
    }
    fx.rewrite_index(&[header]);
    let (code, stdout, stderr) = fx.list("files", &["--format", "json"]);
    assert_eq!(code, 0, "空索引もexit 0: {stdout}{stderr}");
    let value: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("0件も単一JSONとしてparseできる");
    let serialized = serde_json::to_string(&value).unwrap();
    assert!(
        !serialized.contains("travel.zip"),
        "0件のJSONに対象fileは含まれない: {serialized}"
    );
}
