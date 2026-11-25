# Unreleased (次期 0.2.0)

## 0.2.0(未確定・TASK-20)

### 破壊的変更
- 分類語彙の改名: `rel_path` → `label`(索引・registry・manifest・query JSON出力のkey/field)
- config: `[files."<basename>"]`の`path` keyを`label`へ変更(旧keyは設定エラー2)
- CLI: `get`の`--path` flagを`--source-path`へ変更(照合対象=location.source_path完全一致は不変。旧flagはusage 64)
- 索引schemaを2から3へ(旧schema索引の読込・変換は行わない。`manage index rebuild`で再構築)
- registry file_write record schemaを2から3へ(旧形式行はparse失敗。初期化は0.2.0適用前に旧binary(v0.1.0)で`manage reset --apply`を実行)
- manifest file record schemaを2から3へ(batch header行はschema 2のまま。旧manifestが残る環境では`manage index rebuild`は旧record検出時に設定エラー2で失敗する(黙って続行しない)・0.2.0適用前に初期化と再pushが必要)
- 移行手順(旧registry/旧manifestが残る環境): 【旧binary(v0.1.0)でreset→config書換([files].path→label)→0.2.0適用】の順序(0.2.0 binaryのresetは削除前にregistryを読み込むため旧形式registryに対して実行できず・旧manifestからのrebuildはlabel欠損のため不可)

### 旧語彙→新語彙対応表
| 旧 | 新 |
|---|---|
| rel_path(field/key・表示) | label |
| [files].path(config key) | [files].label |
| get --path | get --source-path |
| query JSON "rel_path" | "label" |
| INDEX_SCHEMA=2 | INDEX_SCHEMA=3 |
| file_write schema 2 | schema 3 |
| manifest file record schema 2 | schema 3 |

- 維持(改名対象外): auto_path_roots・unfiled_category・category・source_path・latest_source_path・identity_hash・metadata schema=1。正規化規則・category名規則も不変(名称のみ変更)
- 内部: normalize_rel_path→normalize_label・ManifestV2File→ManifestV3File等の関数・構造体改名(外部影響なし)

## 0.2.0(未確定・TASK-9/13/16/18/19: local catalog DB・serve+Web frontend)

### 追加(TASK-9)

- local catalog DB(`~/.local/state/amazing-backup/catalog.db`・SQLite): remote索引のlocal複製(復号後の平文view)とローカル書込の導出index。権限0600・平文を含むため取扱注意。真正はregistry.jsonlとS3索引のまま(`manage catalog rebuild`で常に再構築可能)
- CLI照会のcache利用: `list files/versions/pins/runs`・`query`は起動時HEAD 1回のvalidator(ETag+VersionId)検証でfreshならcatalog DBから回答(S3 GET・復号を省略)。`--refresh`で再取得を強制
- `manage catalog sync|rebuild|scan`: catalog DBの遅延同期・ローカルregistryからの再構築・アップロード前カタログscan(`[profiles.<name>.catalog.scan]`の登録root走査でunbacked/changed/ok/unknownの4状態へ分類。`--sha256`でsha一致合流によりunknownをokへ確定)
- `serve` subcommand: Web frontend(frontend/distをrust-embed埋め込み・欠落buildではAPIのみ起動+warning)+読取API(axum・`/api/*`)を起動。listenはserver.toml(既定`127.0.0.1:40510`・不在でも既定値で起動)。設定fileにserver.tomlを新設(`config init`が生成)
- passkey認証(WebAuthn): `serve --bootstrap`のone-time token(期限10分・1回消費)でpasskey登録。未認証の閲覧系APIは401で拒否(fail-closed)。session期限24時間
- 認証済み閲覧API: entities(1page=50件のpaging)・versions・pins・locations・pending・sync状態・diff(実行+最終結果参照)・pending scan実行。書込操作API(backup push/pull・get・pin・manage系)は無い(操作はCLI既存のまま)
- `manage auth list` / `manage auth remove <credential_id>`: passkey credentialの一覧・失効(local catalog DB操作・S3接続不要。全削除時は`serve --bootstrap`の再bootstrap案内を表示)
- api-key認証(TASK-13): passkeyが使えない端末・browser向けの第二login手段。`manage auth api-key [--name NAME]`でkeyを生成(`abk_`前置・stdout表示は1回限り・SHA-256 hashのみ保存)し、LoginViewのapi-key欄へ入力(接続元LAN内限定で受理・sessionはpasskey loginと同一の24時間)
- serve Web UIの構成とAPI拡張(TASK-16/18/19): 5タブ構成(ホーム/バックアップ/転送/設定/管理)・ホームサマリ(sync状態・前回backup・未対応・diff・実行中job)・`GET /api/runs`(backup実行batchの集約)・`GET /api/jobs`(現版は空応答)・一覧3ビュー(folder/tag/collection切替)+検索+pager・tag/collection編集API(catalog DBのlocal語彙で索引には入らない。書込は成功・失敗双方をaudit_log tableへ記録)・frontend visual design(cold slate+teal・WCAG AA contrast)
- 取得job機構・転送タブ(TASK-27/TASK-17 W4): `POST /api/jobs/get`(body {uuid,version_id?,pin?,out,tier?,days?,overwrite?}。省略既定はCLI getと同一=tier Standard・days 7・overwrite false。即時応答はqueuedのjob object)・`GET /api/jobs`(jobs table正本の一覧。進行中優先・?status=/?kind=絞り込み)・`GET /api/jobs/{id}`(params・progress・result・error)・`POST /api/jobs/{id}/cancel`(queuedのみ受理=取り消し(cancelled)。実行中・完了済みは409)。jobはcatalog DB新tableのjobs(永続class=再構築不能の実行記録)を正本とし、JobManagerの直列worker(同時実行1)がCLI getと同一のstorage層経路で実行(書込APIと同一semaphoreで相互直列化)。DEEP_ARCHIVE対象は復元job発行後にwaiting_restoreへ遷移してsemaphoreを解放し・timer(既定15分)がhead-objectのRestore headerを定期確認して復元済みなら取得を再開する(CLIの「12〜48h後に再実行」運用のserver側自動追従化)。server再起動時にqueued/running/waiting_restoreの行はinterruptedへ一括更新(自動再開しない=再実行はUIから)。転送タブUI(取得form+job一覧・進行N/M・現在path・error・再実行button・prefill=詳細画面の版行「取得」導線・polling既定10秒=UI更新挙動設定)・ホーム実行中job区画の実データ追従・nav転送tabの実行中job status dot
- 名前変更・所在変更API(TASK-22/TASK-17 W3): `POST /api/entities/{uuid}/rename`(body {title}。規則違反422・重複titleは許容+警告応答)・`POST /api/entities/{uuid}/location`(body {source_path,category,label}。所在は完全一致特定・不在404)。書込の真正はregistry.jsonlへの新event(`rename`・`location_update`・schema=1)追記で、索引全体再構築+PUT・catalog DB反映・metadata配置替え(rename時=新titleキーPUT+旧キー掃除)までを1実行で行う(変更後にCLI `list files`/`query`でも新titleが見える。`manage catalog rebuild`・`manage index rebuild`の再構築でも変更は保持)。再push時の合流はrename後のtitleを継続(旧titleへ退行しない)。詳細画面の文脈操作UI(title編集form・所在行ごとのcategory/label編集form・2段階確認部品=「変更」→「確認して実行」。dialog不使用)

### 修正(PR#9レビュー指摘)

- catalog cacheのvalidator束縛: 書込系(push/pin/prune等)の索引upload後のDB反映は、upload応答のVersionIdがHEAD現行version_idと一致した場合のみfresh化する(不一致=upload→HEAD間に他端末が索引objectを置換した場合は当該scopeをstale化して反映中止=他端末のvalidatorと自processの旧entriesの組でfresh化し、以降のlist/queryがcache hitとして旧内容を受け入れるのを防ぐ。versioning無効bucketでupload応答が空の場合は従来どおりHEAD応答のみで判断)
- catalog sync・cache miss再取得でのmtime基線保持: S3索引replica取得反映(`manage catalog sync`・照会系cache miss再取得)は既存DB行(versions/locations)からsource_mtime基線を引き継いで置換する(local backup記録の基線がreplica取得で消え、`manage catalog scan`が未変更fileをfalse changedに分類するのを防ぐ)。`manage index rebuild`後のDB反映もregistryのfile_write記録から基線を供給する
- Web UI詳細画面のentity切替残存解消: entity詳細の要求対象uuidをstoreが管理し、A→B切替時に旧entity(A)の詳細・版一覧・errorを即座にclear+旧uuid向けの遅延応答を棄却する(切替直後の旧詳細表示・編集可能期間の残存=route uuid(B)へAのtagsで置換する事故を構造的に排除)

# 0.1.0
- 初版(v0.1.0として公開)
