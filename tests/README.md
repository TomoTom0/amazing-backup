# tests/ — テスト構成の索引

backup-remote(バックアップ暗号化・S3 Glacier uploadパイプライン)のテスト構成。
`tests/design/`の条件書が正本であり、生成の向きは一方向:
設計(tmp/tasks/tm-1/design_backup_pipeline.md 8章) → 条件書(tests/design/) → テスト(src内`#[cfg(test)]`・tests/*.rs)。
テストコードや実装から条件書を生成しない(test-structure skill)。

2026-10-08の実装言語Rust変更(ユーザー指示)に伴い、旧pytestテスト67件は破棄済み。
2026-10-09: コードレビュー指摘12件の対応で条件書を61条件→73条件へ追加・更新し、
実装・テストとも全件PASS・verify-conditions.py PASS([meta]検証記録済み)。
2026-10-09: TASK-6(CLI一時上書き)対応でUPL-015〜019を追加。続くコードレビュー指摘5件対応で
UPL-003/004(skip判定行列の3行簡素化)・UPL-018(前後空白付き値)を更新し、
CONF-008・CLI-006を追加(80条件)。
2026-10-10: TASK-8(RESTORE_README.txt同梱)対応でUPL-020〜022を追加しUPL-008のdry-run表示へ
README配置予定を追記(83条件。テスト先行のため実装前の新条件テストは実行failを許容)。
2026-10-10: コードレビュー指摘5件対応でUPL-023/024を追加(85条件)。
2026-10-11: TASK-2(age_size完全一致化+verify構造検査recipient照合)対応でテスト先行の更新。
VER-002を完全一致(未満・超過いずれも不一致)へ文言更新しVER-013
(verify.structure-recipients-bijection)を追加(86条件)。共用verify_fixtureの.ageを
合成ageヘッダ(v1.2形式ssh-ed25519 stanza+`--- `終端)へ・記録指紋をSHA256:<b64(32byte)>
実形式へ再構成。実装前のため新期待値のテスト(VER-002の未満case・VER-013の誤recipient系
case)は実行failを許容。
2026-10-11: TASK-5(restore系3サブコマンド)対応でテスト先行の更新。restoreエリアを新設
(tests/design/restore.toml・RES-001〜024の24条件・計110条件)しtests/restore.rsを新設。
fake aws(write_fake_aws)をtests/common/mod.rsへ共用化しrestore系へ拡張(restore-object・
list-objects-v2・head-objectのRestoreヘッダ応答・download方向cp・失敗注入2種)。
UPL-020のテンプレート内容要件期待値へrestoreコマンドの1行を追加(内容要件6)。
実装前(src/restore.rs未作成)のためrestore系テストとUPL-020追加期待値のテストは
実行failを許容(cargo test --no-runのコンパイルは通る)。
2026-10-13: TASK-9(AWS SDK移行)対応でテスト先行の更新
(tmp/tasks/tm-9/design_sdk_migration.md 10章・テスト再構成方針)。
aws CLI(subprocess)依存のテスト構造をfake seamへ再構成:
- fake差替え機構をPATH差替えのfake aws shell script(write_fake_aws・FAKE_S3_STATE)から
  bin内seam(env BACKUP_REMOTE_FAKE_S3_STATE・src/s3.rsのFakeS3・debug build専用gate)へ変更。
  state構造(objects/・restore/・calls.log相当のAPI呼び出し記録)と失敗注入env 4種
  (FAKE_S3_FAIL_CP_FOR・FAKE_S3_FAIL_RESTORE_FOR・FAKE_S3_FAIL_HEAD_FOR・FAKE_S3_HEAD_404_FOR)は維持
- 条件書13+7件をargv/CLI引数期待値からSDK API呼び出し参数期待値へ改訂
  (upload: UPL-005/008/009/012/013/015〜022、restore: RES-001/003/007/014/015/018/020)。
  --profile廃止(UPL-015/016/019・RES-020から削除)
- 新規10条件追加: config 4件(credentials_file必須・credentialファイルschema・fail-closed・
  credential値非露出)・cli 2件(--profile廃止・fake seam)・upload 4件(multipart閾値・
  multipart checksum・part失敗時abort・AWS_ENDPOINT_URL系env警告)。計120条件・excluded 11件
- tests/s3_backend.rsを新設(S3 backend・credential系の結合test。10新規条件のcover)
- capture_request(aws-smithy test-util)によるRealS3参数検証は実装taskで追加
  (upload.toml excludedに記録)
実装前(src/s3.rs・load_credentials未作成)のため新期待値のテストは実行failを許容
(cargo test --no-runのコンパイルは通る)。
2026-10-14: TASK-10(S3ストレージ設計)対応でテスト先行の条件書更新
(tmp/tasks/tm-10/design_s3_storage.md 9章素案+設計1〜8章本文)。
- 新規feature 4ファイル: layout(9条件)・index(7条件)・pin(5条件)・retention(6条件)
- 既存への追加: cli 24件・config 4件・encrypt 1件(計170条件・excluded 24件)
- 素案外で設計本文から補足条件化7件(layout.rel-path-normalization・cli.backup-push-integrated-run・
  pin.add-writes-pin-key-from-version・cli.get-sha-mismatch-error・cli.list-versions-missing-name-error・
  retention.prune-category-required・cli.disaster-recovery-reads)
- 既存条件はPhase A(案a: 旧サブコマンド凍結)では無変更。棚卸し表(設計9章)対象の7条件+補足の
  encrypt.name-override-pathsへsource欄追記(Phase Bの廃止/文言改訂予定を記録)、restore/uploadは
  ヘッダーへ棚卸しを記録。excludedへ13件追加(docs運用系・IAM・Object Lock不採用・移行手順等)
- 同日: 01f4af5(RESTORE_README placement削除)での条件削除(upload 5件・restore 1件)が
  本READMEの条件数に未反映だったため実態へ合わせて修正
- 同日: TASK-10段階5(テスト作成)で56新条件のテストコードを追加。新規6ファイル
  tests/layout.rs・tests/index_storage.rs・tests/pin.rs・tests/retention.rs・
  tests/storage_query.rs・tests/storage_backup.rs(bin subprocess・fake seam)+src/config.rsの
  unit test 3件。src/layout.rs・src/index.rsは実装前のため直接参照するunit testは書かず
  dry-run表示・索引objectの復号parse・calls.logのAPI呼び出し記録で観測する構成。
  common/mod.rsへStorageFixture(new schemaのpipeline/aws.toml組立・索引の復号/再暗号化・
  pty実行helper)とFAKE_S3_VERSIONING_DISABLED・FAKE_S3_OBJECT_SIZE_FOR注入を追加
  (設計8.2のversioning模擬等を先取り。5GB超pinのsize分岐はsize上書き注入で駆動し
  5GB実体は作らない)。
  実装前のため新条件のテストは実行failを許容(cargo test --no-runのコンパイルは通る。
  既存テストは全件pass維持)

## 構成

| path | 役割 | 状態 |
|---|---|---|
| tests/design/*.toml | テスト設計条件書(正本) | 作成済み(12ファイル・170条件・excluded 24件)。TASK-10 S3ストレージ設計(9章素案+本文)を反映済み |
| tests/common/mod.rs | 結合test共用(age/ssh-keygen有無判定・鍵pair生成・tmp dir fixture・fake seam用helper(credentialファイル・aws.toml組立・API呼び出し記録読取)等) | 作成済み |
| tests/age_roundtrip.rs | 実ageでの暗号化roundtrip・ヘッダ照合(結合) | 作成済み |
| tests/encrypt_batch.rs | registry/manifestを含むbatch単位・fake seam(BACKUP_REMOTE_FAKE_S3_STATE)でのupload(結合) | 作成済み(TASK-9でPATH差替えからseamへ更新) |
| tests/verify_roundtrip.rs | verify --roundtrip系(結合) | 作成済み |
| tests/restore.rs | restore系3サブコマンド(request/status/download)の復元・検証・復号(結合・TASK-5) | 作成済み(TASK-9でPATH差替えからseamへ更新) |
| tests/s3_backend.rs | S3 backend・credentialファイル系(fake seam・multipart・abort・endpoint env警告・--profile廃止。結合・TASK-9) | 作成済み(テスト先行。src/s3.rs実装前は実行failを許容) |
| tests/cli_bin.rs | bin entrypoint系(--version・lib/bin parity。結合) | 作成済み |
| tests/layout.rs | TASK-10 layoutエリア(key生成・分類解決・衝突・検証。dry-run表示とfake seamで観測) | 作成済み(テスト先行。実装前は実行failを許容) |
| tests/index_storage.rs | TASK-10 indexエリア(索引schema・registry再構築・STANDARD・recipients・version_id・manifest再構築・pin反映) | 作成済み(テスト先行) |
| tests/pin.rs | TASK-10 pinエリア(pin add/rm・write-once・backup時pin) | 作成済み(テスト先行) |
| tests/retention.rs | TASK-10 retentionエリア(lifecycle NCVE/MPU abort・prune dry-run/保護/削除条件) | 作成済み(テスト先行) |
| tests/storage_query.rs | TASK-10照会系CLI(query/get/list/diff。版特定・復元job・災害時読み取りの近傍) | 作成済み(テスト先行) |
| tests/storage_backup.rs | TASK-10操作系CLI(backup push統合/dry-run・pull確認・versioning・索引不在・災害時動作)+keep_artifacts掃除+registry基準skip | 作成済み(テスト先行) |
| src/**/*.rs 内 #[cfg(test)] | cargo単体test(S3実行はS3Backend fake(FakeS3)差替) | 作成済み |

cargo testの階層: unit=src/各モジュール内`#[cfg(test)] mod tests`、
integration=cargoの結合testとして`tests/`直下`*.rs`(各ファイルが独立crateとしてlibを`backup_remote::...`で参照)。
結合testはbinをsubprocess実行するものが中心(fake seamの`BACKUP_REMOTE_FAKE_S3_STATE`・
失敗注入envを子processへ渡す)。cargo testはdev profile(debug_assertions有効)で
seamが有効。release profile(`cargo test --release`)ではseam・FakeS3がcompileされず
seam依存testは動作しない(通常の検証は`cargo test`で行う)。

## 条件書(tests/design/)

エリア別に12分割。設計書8章の条件(初回61条件 + 2026-10-08レビュー指摘対応の追加11要件
CLI-005 / PLAN-009 / CONF-007 / ENC-019・020 / FPR-005 / UPL-013・014 / VER-010〜012、
およびmanifest再構築関連の派生条件 + 2026-10-09 TASK-6のUPL-015〜019と
同日レビュー指摘対応のCONF-008・CLI-006 + 2026-10-11 TASK-2のVER-013 + 同日TASK-5の
RES系 + 2026-10-13 TASK-9の改訂と新規10件(CONF-009〜012相当・CLI-007/008相当・
UPL-025〜028相当) + 2026-10-14 TASK-10の新規4エリアと追加29件)を1:1で条件化し、
各設計書のmock・skip方針注記を`[[excluded]]`に記録した(restoreエリアはTASK-5設計書3.9
冒頭注記のmock・skip方針を別entryで記録。TASK-9の実装時unit領域(capture_request等)は
upload/restore/configのexcludedへ、TASK-10のdocs運用系・不採用判断は各エリアのexcludedへ記録)。
2026-10-14の01f4af5(RESTORE_README placement機能削除)でUPL/RESのREADME配置系6条件
(upload 5件・restore 1件)を削除した(上記TASK-8のUPL-020〜024相当は機能削除により消滅)。

| ファイル | feature | source_file(主対象) | 条件数 | 設計対応 |
|---|---|---|---|---|
| plan.toml | plan | src/targets.rs | 9 | PLAN-001〜009 |
| config.toml | config | src/config.rs | 16 | CONF-001〜008 + TASK-9 credential系4件 + TASK-10 layout/prefix/keep_artifacts系4件 |
| encrypt.toml | encrypt | src/crypto.rs | 22 | ENC-001〜020 + manifest復旧派生 + TASK-10 registry基準skip |
| fingerprint.toml | fingerprint | src/fingerprint.rs | 5 | FPR-001〜005 |
| upload.toml | upload | src/uploader.rs | 23 | UPL-001〜019 + TASK-9 multipart/警告系4件(README配置系5件は01f4af5で削除) |
| verify.toml | verify | src/cli.rs | 13 | VER-001〜013 |
| cli.toml | cli | src/cli.rs | 32 | CLI-001〜006 + TASK-9(--profile廃止・fake seam)2件 + TASK-10照会・操作系24件 |
| restore.toml | restore | src/restore.rs | 23 | RES系(README配置系1件は01f4af5で削除。本編8章・正本はtmp/tasks/tm-5/design_restore.md 3.9) |
| layout.toml | layout | src/layout.rs | 9 | TASK-10設計1章(key生成・分類解決・衝突・検証。9章素案+rel_path正規化) |
| index.toml | index | src/index.rs | 7 | TASK-10設計2章(索引schema・再構築・version_id供給) |
| pin.toml | pin | src/cli.rs | 5 | TASK-10設計1.3・3.2(write-once・copy・backup時pin・rm) |
| retention.toml | retention | src/cli.rs | 6 | TASK-10設計3.2・5.2(lifecycle・NCVE/prune共通削除条件) |

- 分割理由: test-structureの`[meta].source_file`(検証対象の実装ファイル・ファイル単位に1つ)が
  7エリアでそれぞれ別moduleに対応し、単一ファイルでは特定できないため。分割によりidの`<area>`部と
  ファイルstem = featureが一致し、id体系とファイル体系が1:1で噛み合う
- 条件idはstable id(`<area>.<kebab-topic>`)。設計8章のPLAN-001等の表idは条件idに使わず、
  各条件の`source`欄に「tmp/tasks/tm-1/design_backup_pipeline.md 8章 PLAN-001(詳細は2.1)」の形で参照する。
  TASK-9の改訂条件もidは付与後不変のため旧idのまま文言のみ更新し、`source`欄へTASK-9設計書を追記
- 各条件のdescription末尾の`(unit)`/`(integration)`/`(unit, fake S3Backend)`/`(integration, fake seam)`は
  テスト階層の目安(設計8章付記。S3実行を含むものはS3Backend traitのfake差替=実networkなし)
- CLI-004(cli.module-entrypoint-parity)はRust化に伴い「cargo run --経由とinstall済みbinの同一挙動」へ読み替え(設計8章)

## 階層対応(test-structure §6)

| 条件の種別 | テスト階層 | 条件書のsource(一次出典) |
|---|---|---|
| (unit) plan / config / cli / fingerprint / verify | src/<module>.rs 内 `#[cfg(test)]` | 設計書詳細設計(2章・3章・4.4・5章) |
| (unit, fake S3Backend) upload / config.aws-required-on-upload | src/uploader.rs 内 `#[cfg(test)]`(S3Backend fake差替・実networkなし) | 設計書2.3・5章・TASK-9設計4章 |
| (unit) layout / index.schema-and-serialization / configのTASK-10追加系 | src/layout.rs・src/index.rs・src/config.rs 内 `#[cfg(test)]`(layout.rs・index.rsはTASK-10 Phase A実装で新設) | TASK-10設計書(tmp/tasks/tm-10/design_s3_storage.md) |
| (integration) encrypt実age系・fingerprint.stanza-tag-matches-age-header・verify roundtrip系 | tests/age_roundtrip.rs 等(実age不在環境ではskip扱い) | 設計書4章・6章・8章 |
| (integration, fake seam) upload系(UPL-015〜019等)・restore系・S3 backend/credential系 | tests/encrypt_batch.rs・tests/restore.rs・tests/s3_backend.rs(bin subprocess・fake seam。実ageを要するtestは不在環境でskip扱い) | TASK-5設計書(tmp/tasks/tm-5/design_restore.md)・TASK-9設計書(tmp/tasks/tm-9/design_sdk_migration.md) |
| (integration, fake seam) TASK-10のpin / retention / cli追加系 / index構築系 | tests/*.rs(bin subprocess・fake seam。TASK-10段階5のテスト作成で新設) | TASK-10設計書(tmp/tasks/tm-10/design_s3_storage.md) |

テストは条件書の各条件に`[covers:<id>]`タグで紐付ける。Rustテストではテスト関数の
doc commentとして`/// [covers:<stable-id>]`を1行で付ける(設計8章・複数条件は複数行)。
verify scriptが`tests/**/*.rs`と`src/**/*.rs`を走査し、`source_lines`を自動同期する。

## test_command

```console
$ cargo test
```

全件PASSを確認した後、各条件書の`[meta]`に検証記録
(verified = true / verified_at / verified_by / test_command / source_hash)を書く。

## 検証script

```console
$ uv run python scripts/design/verify-conditions.py
```

機械検査の内容(test-structure §4):

- (a) `[covers:<id>]`タグが実在する条件idを指すこと(dangling tag検出)
- (b) 全条件が1つ以上のタグでcoverされていること(網羅検査)。
  テストファイル(tests/**/test_*.py・tests/**/*.rs・src/**/*.rs)が1つも無い段階は
  警告表示のみでfailしない。テストコード作成後はfailする。
  2026-10-14 TASK-10段階5(テスト作成)後: 170条件すべてcover済みで(b)もpass
  (56新条件は全てcoversタグ付き。(a)(c)(d)(e)もpass)
- (c) 条件書tomlのschema検査(id形式・必須フィールド・expect_*の存在・givenの実質性)
- (d) `source_lines`をタグ走査結果へ自動同期(自動メタデータ・手書きしない)
- (e) `[[excluded]]`の構造検査(item/source/reasonの空欄をfail)

オプション: `--strict-source`(source欄へ出典参照形式を含むかの検査。将来用)

機械検証のpassは網羅・正当性の証明ではない。出典(設計8章・TASK-5 3.9・TASK-9 10章・
TASK-10 9章+設計1〜8章)×条件の内容照合はレビュー・検収時に行う(test-structure §5)。
