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
2026-10-15: TASK-11(Phase B)テストコード更新(旧サブコマンド削除への追随・テスト先行。
  詳細はtests/design/各ファイルのヘッダコメント)。
- 廃止47条件のテスト削除: 該当[covers:]タグとテスト関数を除去。src内unit testも
  廃止条件を参照するものを削除(cli 2・plan 4・encrypt 6・upload 16・restore 19の対応。
  実装コードは無変更)。tests/common/mod.rsから旧形式helper(aws_toml_body・
  write_pipeline_toml・write_marked_credentials・sha256_b64)を削除
- 文言改訂39条件(id不変)への追随: tests/restore.rs・tests/encrypt_batch.rs・
  tests/s3_backend.rsをStorageFixture+backup push/get/backup pull/list jobs経路へ
  全面書換、tests/verify_roundtrip.rsをbackup verify経路へ、tests/cli_bin.rsの
  parity代表ケースをbackup pushへ置換
- 新規11条件+保留解消1条件(restore.download-plain-conflictのget/pull平文衝突保護への
  文言改訂)+事後条件化4件(L-7/L-8/M-2/M-6)のテストを追加:
  tests/cli_bin.rs(旧サブコマンド削除)・tests/storage_query.rs(--format系・list jobs拡張)・
  tests/phase_b_options.rs(新設: --profile系3件・--aws-config既定・索引GET転送エラー・
  manage jobs reissue)・tests/storage_backup.rs(pull範囲排他)・tests/retention.rs
  (prune併置排他・category guard)・tests/pin.rs(skip対象への--pin)・
  tests/index_storage.rs(version_ts供給源・未知record_type警告)・tests/restore.rs(平文衝突保護)
- 条件数144(廃止47・改訂39・新規11・事後条件化4・保留解消1)。実装前のため
  Phase B実装待ちのテスト(backup verify・--format json・--profile・manage jobs reissue・
  get/pull平文衝突保護・list jobs noncurrent版表示)は実行failを許容
  (cargo test --no-runのコンパイルは通る・verify-conditions.py PASS)

2026-10-15: TASK-11(Phase B)コードレビュー指摘対応(テスト先行。H-1/M-1/M-2/L-1〜L-6の9件)。
- 条件書へ9条件追加し対応テストを作成(実装後にcargo test全件pass・verify-conditions.py PASS):
  cli 2件(同一prefix・別bucket複数profileでのnamespace分離=H-1・FakeS3のRestoreObject事前
  検査=L-3)・verify 1件(掃除済みrunの検査対象外=M-1)・pin 2件(push --pinのwrite-once違反は
  警告+当該file skip=M-2・pin rmのregistry記録失敗時のrebuild案内=L-1)・retention 2件
  (prune部分失敗時の削除済み版記録=L-2・pruneの版順sortはepoch数値比較=L-4)・
  restore 1件(pull平文衝突の読込失敗時保護=L-5)・index 1件(parse失敗時のrebuild案内=L-6)
- common/mod.rsの失敗注入にFAKE_S3_FAIL_DELVER_FOR(当該keyのDeleteObjectVersionのみ失敗。
  <key>:<N>形式は先頭N回成功後に失敗)を追加(L-2の削除loop途中失敗の注入用)
- 条件数153

2026-10-15: TASK-11(Phase B)再レビュー指摘対応(テスト先行。M-R1・L-R1〜L-R5の6件)。
- 条件書へ3条件追加し対応テストを作成(実装後にcargo test全件pass・verify-conditions.py PASS):
  index 2件(索引pins[]構築・復元のbucket一致=M-R1・当該bucketの版もpinも無いentryの索引
  載せ除外=L-R1)・pin 1件(get --pin/pin rmのpin解決は当該実効bucketの索引に限る=M-R1)。
  cli 1件を拡張(旧形式file_upload記録=s3_bucket空文字の正本bucket扱い分岐へ結合test追加=
  L-R3/L-R4。descriptionの規則は既記載のためgiven/expectのみ追記)・restore 1件を文言改訂
  (読込不能平文の--yes付き挙動をget/pullとも上書き続行へ統一=L-R5)・verify 1件のdescription
  誤記修正(keep_artifacts="all"→"always"=L-R2)
- 実装: src/index.rs(build_entriesのpins bucket照合・versions/pins空entry除外)・
  src/storage/manage.rs(rebuildのpins復元へ同一規則)・src/storage/get.rs(平文衝突の
  読込失敗時--yes付き続行)・src/registry.rs(旧upload経路のデッドコード
  pending_uploads_for_bucket/upload_candidatesと単体testを削除=L-R3)
- 併せてpins[]の並びをs3_keyでsort(採用源HashMapのiteration orderがprocess毎に不定で
  同一registryからの再構築でも索引内容が変わる=index.rebuild-from-registry-per-runの
  冪等(同一内容)違反。M-R1テストの多pin構成で顕在化したため修正)
- 条件数156

## 構成

| path | 役割 | 状態 |
|---|---|---|
| tests/design/*.toml | テスト設計条件書(正本) | 作成済み(12ファイル・158条件・excluded 23件)。TASK-11 Phase B棚卸し(廃止47・改訂39・新規11・事後条件化4)とレビュー指摘対応(9件+再レビュー3件)とprod 3bucket構成移行(2件)反映済み |
| tests/common/mod.rs | 結合test共用(age/ssh-keygen有無判定・鍵pair生成・tmp dir fixture・StorageFixture(新schema組立・索引の復号/再暗号化)・fake seam用helper等) | 作成済み |
| tests/age_roundtrip.rs | 実ageでの暗号化roundtrip・ヘッダ照合(結合) | 作成済み |
| tests/encrypt_batch.rs | registry/manifestを含むbatch単位・fake seamでの暗号化・upload(backup push統合経路。結合) | 作成済み(TASK-11 Phase Bでbackup push経路へ全面書換) |
| tests/verify_roundtrip.rs | backup verify --roundtrip系(結合) | 作成済み(TASK-11 Phase Bでbackup verify経路へ書換。実装前fail許容) |
| tests/restore.rs | 復元job系の共通経路(tier/days検証・Restore header parse・--key事前検査・get/pull平文衝突保護)をget/backup pull/list jobs経路で検証(結合) | 作成済み(TASK-11 Phase Bで新経路へ全面書換。平文衝突保護は実装前fail許容) |
| tests/s3_backend.rs | S3 backend・credentialファイル系(fake seam・multipart・abort・endpoint env警告)をbackup push経路で検証(結合) | 作成済み(TASK-11 Phase Bでbackup push経路へ全面書換) |
| tests/cli_bin.rs | bin entrypoint系(--version・lib/bin parity・旧サブコマンド削除。結合) | 作成済み(旧サブコマンド削除はPhase B実装前fail許容) |
| tests/layout.rs | TASK-10 layoutエリア(key生成・分類解決・衝突・検証。dry-run表示とfake seamで観測) | 作成済み |
| tests/index_storage.rs | TASK-10/11 indexエリア(索引schema・registry再構築・STANDARD・recipients・version_id・manifest再構築・pin反映・version_ts供給源・未知record_type警告) | 作成済み(後半2件はPhase B実装前fail許容) |
| tests/pin.rs | TASK-10/11 pinエリア(pin add/rm・write-once・backup時pin・skip対象への--pin・pin_removeのbucket照合) | 作成済み |
| tests/retention.rs | TASK-10/11 retentionエリア(lifecycle NCVE/MPU abort・prune dry-run/保護/削除条件・併置排他・category guard) | 作成済み |
| tests/storage_query.rs | 照会系CLI(query/get/list/diff・版特定・復元job・--format human/json) | 作成済み(--format系・list jobs noncurrent版はPhase B実装前fail許容) |
| tests/storage_backup.rs | 操作系CLI(backup push統合/dry-run・pull確認/範囲排他・versioning・索引不在・災害時動作)+keep_artifacts掃除+registry基準skip | 作成済み |
| tests/phase_b_options.rs | TASK-11 Phase BのCLI option系(--profile系3件・--aws-config既定path・索引GET転送エラー・manage jobs reissue)+再レビュー(旧記録bucket扱い・pin/entryのbucket分離)+prod 3bucket構成(3bucket分離・push接続先表示) | 作成済み |
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
2026-10-15のTASK-11(Phase B)棚卸しで旧サブコマンド固有の47条件を廃止し、共通経路39条件を
文言改訂(id不変)、Phase B新規11件+保留解消1件+事後条件化4件を追加(144条件・excluded 23件)。

| ファイル | feature | source_file(主対象) | 条件数 | 設計対応 |
|---|---|---|---|---|
| plan.toml | plan | src/targets.rs | 5 | PLAN-001〜009のうち対象解決共通経路5件(TASK-11でname override系等4件廃止→layout.*へ) |
| config.toml | config | src/config.rs | 16 | CONF-001〜008 + TASK-9 credential系4件 + TASK-10 layout/prefix/keep_artifacts系4件 |
| encrypt.toml | encrypt | src/crypto.rs | 16 | ENC系のうち共通暗号化経路16件(TASK-11で旧encrypt固有6件廃止。backup push経路で検証) |
| fingerprint.toml | fingerprint | src/fingerprint.rs | 5 | FPR-001〜005 |
| upload.toml | upload | src/s3.rs | 7 | UPL系のうちS3転送共通経路7件(TASK-11で旧upload固有16件廃止。backup push経路で検証) |
| verify.toml | verify | src/storage.rs | 13 | VER-001〜013(TASK-11でbackup verify経路へ文言改訂・id不変) |
| cli.toml | cli | src/cli.rs | 45 | CLI-001〜006 + TASK-9/10系 + TASK-11 Phase B(旧サブコマンド削除・--format/--profile・manage jobs reissue・pull範囲排他等)+prod 3bucket構成(3bucket分離・push接続先表示) |
| restore.toml | restore | src/storage.rs | 4 | 復元job系共通経路4件(TASK-11で旧restore固有19件廃止。get/pull/list jobs経路で検証。平文衝突保護含む) |
| layout.toml | layout | src/layout.rs | 9 | TASK-10設計1章(key生成・分類解決・衝突・検証。9章素案+rel_path正規化) |
| index.toml | index | src/index.rs | 13 | TASK-10設計2章 + H-1/H-3 + TASK-11(version_ts供給源・未知record_type警告) + 再レビューM-R1(pins bucket分離)・L-R1(版・pin無しentry除外) |
| pin.toml | pin | src/cli.rs | 8 | TASK-10設計1.3・3.2 + M-4(--config早期検査) + TASK-11 M-6(skip対象への--pin) + 再レビューM-R1(pin解決のbucket分離) |
| retention.toml | retention | src/cli.rs | 8 | TASK-10設計3.2・5.2 + TASK-11 L-8(prune併置排他)・M-2(category guard) |

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
| (unit, fake S3Backend) uploadのmultipart系 / config.aws-required-on-upload | src/s3.rs・src/config.rs 内 `#[cfg(test)]`(S3Backend fake差替・実networkなし。TASK-11 Phase Bで旧src/uploader.rs削除に伴い移設) | 設計書2.3・5章・TASK-9設計4章 |
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
  2026-10-15 TASK-11 Phase Bテスト更新後: 144条件すべてcover済みで(b)もpass。
  同日レビュー指摘対応後は153条件すべてcover済み
  (Phase B実装待ちの条件もテスト先行でcoversタグ付き。(a)(c)(d)(e)もpass)。
  同日再レビュー指摘対応後は156条件すべてcover済み((a)(c)(d)(e)もpass)
2026-10-16: TASK-11(Phase B)prod 3bucket構成移行でテスト先行の条件書更新
(ユーザー指示2026-10-15: prodはyour-family-bucket/your-baked-bucket/your-atara-bucketの3bucket・
profile使い分け・混同配置が構造的に起きないこと)。
- cli.tomlへ2条件追加: cli.profile-multi-bucket-isolation(3bucket以上のprofile構成でも
  bucket単位分離が機能=H-1機構のprofile数非依存検証)・cli.backup-push-connection-dest-shown
  (backup pushの接続先実効profile/bucket/prefix表示。dry-runでも表示)
- tests/phase_b_options.rsへ2test追加(three_bucket_aws_tomlによるdev+prod系2bucket構成)。
  実装(AwsConfigへ実効profile名保持・push開始時の接続先表示)後にcargo test全件pass・
  verify-conditions.py PASS
- 条件数158
- (c) 条件書tomlのschema検査(id形式・必須フィールド・expect_*の存在・givenの実質性)
- (d) `source_lines`をタグ走査結果へ自動同期(自動メタデータ・手書きしない)
- (e) `[[excluded]]`の構造検査(item/source/reasonの空欄をfail)

オプション: `--strict-source`(source欄へ出典参照形式を含むかの検査。将来用)

機械検証のpassは網羅・正当性の証明ではない。出典(設計8章・TASK-5 3.9・TASK-9 10章・
TASK-10 9章+設計1〜8章)×条件の内容照合はレビュー・検収時に行う(test-structure §5)。
