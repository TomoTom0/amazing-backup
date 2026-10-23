# amazing-backup 使用手順書

NASバックアップアーカイブ(zip/7z)をageで暗号化し、S3(versioning有効bucket・Deep Archive)へ投入・管理するCLI `amazing-backup` の使用手順書。書き込み・照会・復元を含む。

- 対象読者: バックアップの書き込み・照会・復元を行う運用者(災害時の復旧担当を含む)
- 正本参照: S3ストレージ構成・CLI体系の設計は `tmp/tasks/tm-10/design_s3_storage.md`。S3キー設計(UUID発行キー+versioning・実体/所在/title語彙・索引schema=2・lifecycle・manage reset)の正本は `tmp/tasks/tm-18/design_key-uuid-versioning.md`。旧パイプライン(暗号化経路・registry・終了コード)の設計は `tmp/tasks/tm-1/design_backup_pipeline.md`、AWS SDK移行の設計は `tmp/tasks/tm-9/design_sdk_migration.md`。実装挙動の正本は `tests/design/`の条件書
- 実装バージョン: 0.1.0
- 最終確認: 2026-10-23(TASK-19 XDG配置移行・get/pull --out必須化後の実装に基づく)

## 1. 概要

### 1.1 コマンド体系

旧サブコマンド(plan/encrypt/upload/verify/restore)は**削除済み**(TASK-11 Phase B)。全操作は次の体系へ集約されている(旧手順からの移行は10章)。TASK-18でS3キー設計がUUID発行キー+versioning体系へ変更され、実体の識別語彙はtitle/uuidへ統一された(旧`--name`/folder語彙は廃止)。

| 系統 | command | 用途 |
|---|---|---|
| 照会 | `list files/versions/pins/runs/jobs` | 実体目録・版一覧・pin・run・復元job状況 |
| 照会 | `query <手がかり>` | title/uuid/所在pathでの探索 |
| 取得 | `get <title|uuid>` | 単一実体の取得(復元job発行を内包) |
| 照会 | `diff` | 索引とS3実態の完全性突合 |
| 書込み | `backup push` | 暗号化・upload・manifest・索引更新を1実行で |
| 復元 | `backup pull` | category/run/全対象の一括復元(確認付き) |
| 検査 | `backup verify` | ローカル成果物の整合検査 |
| 管理 | `pin add/rm` | 世代管理対象外のピン留め版の付与・解除 |
| 管理 | `manage prune/lifecycle/index/location/jobs/reset` | 世代削除・bucket保持設定・索引再構築・旧所在除去・復元job再発行・dev bucket初期化 |
| 設定 | `edit metadata/files/layout/policy` | 設定sectionの確認 |

### 1.2 パイプラインと成果物

`backup push` が1実行で 対象解決(title解決を含む) -> 分類解決 -> age暗号化 -> S3 upload -> manifest生成 -> 索引更新 -> metadata sync までを行う(旧plan/encrypt/uploadの分割手順は廃止)。

| 場所 | 内容 |
|---|---|
| `~/.cache/amazing-backup/<batch_id>/` | run毎の暗号化一時物(`.age`・manifest。TASK-19 XDG配置。既定はrun成功後に自動削除) |
| `~/.local/state/amazing-backup/registry.jsonl` | ローカル累積registry(append-onlyのJSONLイベントログ) |
| `--out`で指定したdir | get/pullの復元先(**`--out`は必須**) |
| `s3://<bucket>/<prefix>data/<uuid>/default.age` | 実体本体(uuidはUUIDv7発行キー。版ごと上書き・versioningで版管理) |
| `s3://<bucket>/<prefix>data/<uuid>/<pin>.age` | ピン留め版(write-once) |
| `s3://<bucket>/<prefix>data/<uuid>/metadata_<title>.json` | 実体の身元票(平文JSON) |
| `s3://<bucket>/<prefix>sys/index.jsonl.age` | 索引(照会系の正本・STORAGE CLASS STANDARD固定) |
| `s3://<bucket>/<prefix>sys/manifests/<batch_id>.jsonl.age` | run毎manifest |

暗号化はSSH ed25519公開鍵の1-of-N(全鍵で暗号化し、いずれか1鍵で復号可能)。S3接続は**AWS SDK(aws-sdk-s3)をbinary内蔵**しておりaws CLIは不要。credential(access key)は`configs/secret/`配下の明示的なAPI keyファイル(2.3)から与え、`~/.aws`や環境変数のcredential chainには依存しない(fail-closed)。

照会系(list/query/get/diff)は**aws.toml(bucket/region/prefix)と復号用秘密鍵のみで動作する**(索引をS3から取得するため。災害時にローカルregistry・cache dirは不要。9章)。

## 2. セットアップ

### 2.1 前提ツール

| ツール | 要件 | 確認 |
|---|---|---|
| age | v1.2系(検証: 1.2.1・`/usr/bin/age`) | `age --version` |
| Rust toolchain | ビルドに必要(cargo) | `cargo --version` |

ageはv1.2形式のstanza識別子(指紋prefix)によるヘッダ照合の前提となるため、v1.2系を使用する。

**aws CLIは不要**: S3接続はSDKをamazing-backupが内蔵する。aws CLIがあるとS3側の確認・手動復元(9.3)に使えるが任意である。

### 2.2 設定ファイルの配置(XDG配置・TASK-19)

設定ファイルはXDG Base Directory準拠の規定pathから読まれる。初回はrepo付属のtemplateを規定pathへcopyして記述する:

| file | 規定path | template |
|---|---|---|
| pipeline.toml | `$XDG_CONFIG_HOME/amazing-backup/pipeline.toml`(未設定なら`~/.config/amazing-backup/pipeline.toml`) | `configs/pipeline.example.toml` |
| aws.toml | `$XDG_CONFIG_HOME/amazing-backup/aws.toml`(同上) | `configs/aws.example.toml` |
| registry.jsonl | `$XDG_STATE_HOME/amazing-backup/registry.jsonl`(未設定なら`~/.local/state/amazing-backup/registry.jsonl`。dirは自動作成) | -(自動生成) |
| cache dir | `$XDG_CACHE_HOME/amazing-backup/`(未設定なら`~/.cache/amazing-backup/`) | -(run毎の暗号化一時物) |

```console
$ mkdir -p ~/.config/amazing-backup
$ cp configs/pipeline.example.toml ~/.config/amazing-backup/pipeline.toml
$ cp configs/aws.example.toml ~/.config/amazing-backup/aws.toml
$ # 規定pathにfileが無い場合、copy案内がエラー表示される
```

- 規定pathに設定fileが無い状態でコマンドを実行すると設定エラー2+copy案内が表示される
- `--config PATH` / `--aws-config PATH` で明示指定した場合はcwd基準の相対pathも可(規定pathより優先)
- `AMAZING_BACKUP_HOME`環境変数(テスト/CI用)を指定すると規定pathをすべて`$ABH/`配下へ統合できる(`$ABH/pipeline.toml`・`$ABH/aws.toml`・`$ABH/registry.jsonl`・`$ABH/cache/`)
- pipeline.toml内の相対path(public_keys・decrypt_key)とaws.tomlの`credentials_file`の相対pathは**実行時のcwd基準**で解決される(XDG配置後も維持)

### 2.3 AWS credential(API keyファイル)の作成

credential(access key)は**明示的なAPI keyファイル** `configs/secret/aws-credentials/<profile>.toml`(gitignore済み領域)へ置く。SDKへはこのファイルの値を明示渡しし、credential chainへのfallbackは**行わない**(ファイルが無い・不正なら設定エラー2で即終了するfail-closed構成)。

access keyの入手先はAWS IAM(IAMユーザーのaccess key作成。書き込み・復元・保持管理を担う権限)。

```toml
# configs/secret/aws-credentials/dev.toml(gitignore済み・実運用で作成する秘密ファイル)
[credentials]
access_key_id = "<access key id>"         # 必須
secret_access_key = "<secret access key>" # 必須
# session_token = "<session token>"       # 任意(一時credentialを使う場合のみ)
```

- 作成後`chmod 600`を推奨(秘密情報を含むファイルのため)
- このファイルのpathを aws.toml(規定path)の各profile `credentials_file` へ記述する(3.2)。運用上は`dev.toml`と`prod.toml`の2種類(prod系3profileは同一AWS account運用を想定し`prod.toml`共通1本)
- 空値・前後空白付き値・未知キーは設定エラー2(値そのものはerror文に表示されない)
- 値を直接aws.tomlへ書くことはできない(`credentials_file`を除くcredential系キーは未知キーとして拒否)

### 2.4 ビルドと実行形式

```console
$ cargo build --release          # target/release/amazing-backup
$ cargo install --path .         # PATHへ導入する場合(cargo bin)
```

実行形式は `amazing-backup <command> [options]`。`cargo run --quiet -- <command>` でも同一挙動。設定file本体はXDG規定path(2.2)から読まれるが、設定内の相対path(public_keys・decrypt_key・credentials_file)は**実行時のcwd基準**で解決される(`--config`/`--aws-config`明示指定もcwd基準の相対path可)。

### 2.5 検証用bucket(dev: your-dev-bucket)の準備手順

dev profileは既存bucket `your-dev-bucket` を使用する(2026-10-16確定・新設しない)。versioning有効が必須・lifecycleなし:

```console
$ aws s3api put-bucket-versioning --bucket your-dev-bucket \
    --versioning-configuration Status=Enabled
```

- lifecycleは `amazing-backup manage lifecycle apply` が行う(任意・5.10)
- **全profileのbucketでversioning有効が必須**。書き込み系(backup push・pin add/rm・manage)は起動時に確認し、無効なら設定エラー2(有効化commandを表示)

## 3. 設定ファイル

### 3.1 `pipeline.toml`(規定path: `~/.config/amazing-backup/pipeline.toml`)

暗号化・分類・metadataの設定(秘密を含まない)。全項目を掲げる(`[layout]`/`[files]`/`[metadata]`/`keep_artifacts`は省略時に既定で動く)。**`[output]`sectionはTASK-19(XDG配置)で廃止**(存在したら設定エラー2。registry・暗号化一時物dirのpathはXDG規定path固定・2.2)。`[categories]`はTASK-18で廃止(保持数はaws.toml `[aws.lifecycle]`へ移管・3.2):

```toml
[recipients]
# SSH ed25519公開鍵のpath列挙(1-of-N。全鍵で暗号化し、いずれか1鍵で復号可能)
# 相対pathは実行時cwd基準。1つ以上必須。ed25519形式のみ許容
public_keys = [
  "configs/secret/keys/private/backup_ed25519.pub",
]

[behavior]
# per-file失敗時も継続する既定値(backup pushの--keep-goingで上書きon)
keep_going = false
# 暗号化一時物(cache dir配下の.age)の保持policy。
# "run"(run成功後に当該run分を掃除・既定) | "always"(全保持)
# keep_artifacts = "run"

[verify]
# 復号検証で使う秘密鍵path(秘密そのものではなくpath参照。相対pathは実行時cwd基準)
decrypt_key = "configs/secret/keys/private/backup_ed25519"

[metadata]
# metadata_<title>.jsonのlatest_source_path掲載形式
# "plain"(path文字列そのまま・既定) | "encrypted"(age暗号化object) | "none"(null・掲載しない)
latest_source_path = "plain"

[layout]
# 写像/roots不一致時のcategory(既定"unfiled")
unfiled_category = "unfiled"
# source_path相対化のroot列挙(順序優先・絶対path。省略可)
auto_path_roots = ["/home/tomo/data"]

[files."travel.zip"]     # 手動写像(省略可)
category = "photos"      # category名規則を満たす任意の文字列(定義管理は廃止・記録値として自由化)
path = "2020-2023/travel" # rel_path(""でrel_pathなし)
```

各キーの説明:

| キー | 意味 |
|---|---|
| `[recipients].public_keys` | recipient公開鍵(SSH ed25519)のpath列挙。1-of-N。鍵の追加手順は8.3 |
| `[behavior].keep_going` | per-file失敗時も継続する既定値。`--keep-going`でonに上書き |
| `[behavior].keep_artifacts` | cache dir(`~/.cache/amazing-backup/`)配下の`.age`保持policy。`run`(既定: run成功後に当該run分を掃除)/`always`(全保持)。TASK-19で`[output]`廃止に伴い`[behavior]`へ移設 |
| `[verify].decrypt_key` | 復号用秘密鍵のpath参照。get/pull/verifyの`--key`省略時の既定 |
| `[metadata].latest_source_path` | metadata_<title>.jsonの最新所在掲載形式。`plain`(既定)/`encrypted`/`none` |
| `[layout].unfiled_category` | 分類解決のfallback先category(既定`unfiled`) |
| `[layout].auto_path_roots` | source_pathを相対化するroot列挙(最初に一致したrootを採用) |
| `[files."<basename>"]` | 手動写像。`category`(必須・category名規則の文字列検査のみ)と`path`(rel_path) |

省略時の既定: `[layout]`全体省略時はunfiled_category="unfiled"+auto_path_roots=[]・`[metadata]`省略時はlatest_source_path="plain"・`keep_artifacts`省略時は"run"。`[categories]`・`[output]`セクションの存在は未知キーとして設定エラー2(静的に廃止を確定。registry・cache dirのpathはXDG規定path固定)。

検証規則(違反はすべて設定エラー・終了コード2):

- ファイル不在・TOML parseエラー・未知キー・型不一致
- `public_keys`空配列・鍵ファイル不在・`ssh-ed25519`以外の形式・指紋の重複
- category名規則違反(`^[a-z0-9][a-z0-9-]{0,63}$`。予約領域`sys`・旧表記`_sys`は拒否。files写像とunfiled_categoryに適用)
- auto_path_rootsが絶対pathでない・keep_artifacts列挙値外・latest_source_path列挙値外・`[output]`sectionの存在

### 3.2 `aws.toml`(規定path: `~/.config/amazing-backup/aws.toml`)

S3接続設定(credential値そのものは不含)。profile構成(dev=開発検証1profile + prod=実運用3bucket使い分け)。`[aws.lifecycle]`はlifecycle 3ruleの値(全profile共通・表末尾と4.4)。repoの`configs/aws.example.toml`を規定pathへcopyして記述する(2.2)。`credentials_file`の相対pathは実行時cwd基準。現行の実物:

```toml
[aws]
active_profile = "dev"

# lifecycle(manage lifecycle applyが適用する3ruleの値・TASK-18)
[aws.lifecycle]
noncurrent_days = 180
keep_versions = 10
mpu_abort_days = 7

[aws.profiles.dev]              # 開発検証(versioning有効bucket・STORAGE CLASS STANDARD)
purpose = "dev"
# dev bucketは既存your-dev-bucketを流用(2026-10-16確定)
bucket = "your-dev-bucket"
region = "ap-northeast-1"
prefix = "amazing-backup/"
storage_class = "STANDARD"
credentials_file = "configs/secret/aws-credentials/dev.toml"

[aws.profiles.family]           # 実運用(prod系3bucket・DEEP_ARCHIVE)
purpose = "prod"
bucket = "your-family-bucket"
region = "ap-northeast-1"
prefix = "amazing-backup/"
storage_class = "DEEP_ARCHIVE"
credentials_file = "configs/secret/aws-credentials/prod.toml"

[aws.profiles.baked]            # 実運用(prod系)
purpose = "prod"
bucket = "your-baked-bucket"
region = "ap-northeast-1"
prefix = "amazing-backup/"
storage_class = "DEEP_ARCHIVE"
credentials_file = "configs/secret/aws-credentials/prod.toml"

[aws.profiles.atara]            # 実運用(prod系)
purpose = "prod"
bucket = "your-atara-bucket"
region = "ap-northeast-1"
prefix = "amazing-backup/"
storage_class = "DEEP_ARCHIVE"
credentials_file = "configs/secret/aws-credentials/prod.toml"
```

| キー | 意味 |
|---|---|
| `[aws].active_profile` | 既定で使用するprofile名 |
| `[aws.profiles.<name>].purpose` | profileの用途(`dev`/`prod`) |
| `bucket` / `region` | 接続先S3 bucket・region |
| `prefix` | 名前空間root prefix。`/`終端・空でない・先頭`/`禁止・`[a-z0-9-]`と`/`区切り。省略時は`amazing-backup/` |
| `storage_class` | file objectのstorage class。`STANDARD`/`DEEP_ARCHIVE`等(allowlist検証) |
| `credentials_file` | credentialファイル(2.3)のpath。必須(cwd基準の相対path可) |
| `[aws.lifecycle].noncurrent_days` | NCVE NoncurrentDays。DEEP_ARCHIVE profileでは180以上必須 |
| `[aws.lifecycle].keep_versions` | NCVE NewerNoncurrentVersions(保持するnoncurrent版数。1〜100) |
| `[aws.lifecycle].mpu_abort_days` | AbortIncompleteMultipartUploadの日数 |

- **prod系3bucketは用途別profile**(your-family-bucket/your-baked-bucket/your-atara-bucket。同一region)。profile名はbucket名の接尾辞(family/baked/atara)。`purpose`は`dev`/`prod`の2値のまま(3bucketはいずれもprod)
- **prod系3profileのcredentials_fileは共通1本**(configs/secret/aws-credentials/prod.toml。同一AWS account運用を想定)。将来bucket別credentialへ分割する場合もschema上profile毎に独立したkeyのためaws.tomlの書換えのみで対応できる
- **test profileは廃止**(旧`test`/共用bucket your-dev-bucket構成の解除)
- `[aws.lifecycle]`の3キーは必須(省略は設定エラー2・コード内に既定値なし)。profile横断で同一構成のため`[aws]`直下に一元化
- profileの切替は`active_profile`書換えまたは`--profile`オプション(5.1。当該実行限りの一時上書き)。prod系の書き込みは`--profile family`等の明示指定を推奨(8.2)
- 検証規則(設定エラー2): prefix形式違反・storage_class allowlist外・bucket/region/credentials_fileの空・前後空白付き値・credentialファイルの不在・不正・lifecycle値の範囲違反(keep_versions 1〜100・noncurrent_days/mpu_abort_days 1以上・DEEP_ARCHIVE profileでnoncurrent_days<180)

**エンドポイント確定性の注記**: SDKは環境変数`AWS_ENDPOINT_URL`/`AWS_ENDPOINT_URL_S3`を解釈して接続先endpointを切替え得る。amazing-backupは起動時にこれらのenvが設定されていれば警告をstderrへ表示する(処理は継続)。意図した検証用構成か誤設定かは利用者側で確認すること。

### 3.3 対象リストtoml(`--from-list`)

backup pushの対象をファイルで指定する場合は次のschemaのtomlを作る(実運用のリストはgit管理外で都度作成):

```toml
[[targets]]
path = "/mnt/nas/backup/photo.zip"
title = "旅行写真2026"          # 任意・実体のtitle(新規発行時に必須・5.6)

[[targets]]
path = "/mnt/nas/backup/2025/docs.7z"   # title省略可(合流時・5.6)
```

- `path`(必須): 対象アーカイブのpath
- `title`(任意): 実体のtitle。新規uuid発行が確定した対象で必須(非ttyでの省略は当該対象の失敗・対象エラー3)。合流・再PUT時は省略なら現行titleを継続(指定が異なればtitle変更扱い)
- 位置引数との併用可(和集合)。未知キー・`path`欠損は設定エラー
- 旧仕様の`name`(出力名override)は**廃止**(指定は拒否)。basenameは常に元ファイル名(拡張子付き)

## 4. データ配置と保持

### 4.1 S3側の配置

キーはpathにも内容にも依存しないUUIDv7発行キー(uuid・hyphenated lowercase 36字)基準。実体(entity)単位で`data/<uuid>/`配下に実体本体・pin・身元票が集まる:

```
s3://<bucket>/<prefix>/
  data/<uuid>/
    default.age                    # 実体本体。固定key・版ごと上書き・versioningで版管理
    <pin-name>.age                 # ピン留め版(write-once)
    metadata_<title>.json          # 実体の身元票(平文JSON・STORAGE CLASS STANDARD固定)
  sys/                             # 予約領域(category名として不許可)
    index.jsonl.age                # 索引(STORAGE CLASS STANDARD固定)
    manifests/<batch_id>.jsonl.age # run毎manifest
```

- 索引は照会系(list/query/get/diff)の正本。schema=2で実体(uuid・title・所在locations)単位のentry構成(title重複は許容・一意化はuuid/所在指定)。run毎書込み・backup push完了時と索引に影響する操作後(pin add/rm・manage prune/location rm)に全体再構築してPUTされる(冪等)
- 索引の旧版はS3 versioningのnoncurrent versionとして残る(小容量のため蓄積を許容)
- metadata_<title>.jsonはregistry reduceから導かれる正規形(現行版のsha/size/version_id・pins・最新所在latest_source_path)。内容が変わったときのみPUTされ、title変更時は新キーPUT+旧キーにdelete marker
- storage classは索引とmetadataがSTANDARD固定、それ以外(default.age・pin・manifest)はprofileのstorage_class(prod=DEEP_ARCHIVE)

### 4.2 分類解決・title・合流

分類(category/rel_path)はキー構成には影響せず、所在(location)の記録値として索引・manifestに記録される(絞り込み・表示の語彙)。分類は次の優先順位で解決される:

1. **files写像**(最優先): `[files."<basename>"]`の`category`と`path`をそのまま用いる
2. **auto_path_roots相対化**: source_pathがいずれかのroot配下なら、最初に一致したroot配下のdirectory側path(ファイル名を除く)を`rel_path`とし、categoryは`unfiled_category`
3. **fallback**: 一致rootなし -> rel_pathなし・category=`unfiled_category`

- 実体の識別はuuid(発行キー)とtitle(人間用ラベル・64文字以内・UTF-8可。`/`・改行・制御文字は拒否で違反はusage 64)。title重複(別uuid同title)は許容+警告で、一意化はuuidまたは`--path <source_path>`(所在完全一致)指定で行う
- 平文sha256が既存実体と一致する対象は**合流**する(所在追加のみ・uploadなし・版増加なし)。移動+内容不変は合流、移動+内容変更は合流せず別entityとして新規発行(title必須)
- rel_pathは`.`/`..`要素の排除・正規化のみ適用(実source treeの要素名をそのまま用いる)
- 生成keyがS3のobject key上限1024 byteを超える場合は対象エラー3(prefix長またはtitle長で対処)
- category名規則は`^[a-z0-9][a-z0-9-]{0,63}$`・予約語`sys`(旧表記`_sys`も)拒否。pin名は`^[a-z0-9][a-z0-9-]{0,62}$`(63文字以内)で`default`は予約

### 4.3 versioningと索引

- bucketのversioningは必須(2.5)。`default.age`への上書きuploadごとに新VersionIdが払い出され、索引に版として記録される(date・version_ts・version_id・sha256×2・size)
- 索引のversions[]はdate降順(先頭=最新)。同一日内の複数版はversion_ts(暗号化時刻)で一意化される
- 誤DELETE対策: 読み取り系はlist-object-versionsで実体を把握するためdelete markerの影響を受けない

### 4.4 世代管理(lifecycle・prune)

保持はaws.toml(規定path)の`[aws.lifecycle]`(`noncurrent_days`・`keep_versions`・`mpu_abort_days`)で制御される。削除条件の定義はlifecycle(NCVE)とpruneで共通: **同一keyのnoncurrent版を新しい順に並べたとき、newer noncurrent版の数が`keep_versions`以上となった版を削除**(境界のちょうど`keep_versions`番目は残す)。

- **`manage lifecycle apply`**(bucket側・自動): `{prefix}data/`一括の3rule(1) NCVE(`NoncurrentDays=noncurrent_days`・`NewerNoncurrentVersions=keep_versions`) 2) MPU abort(`mpu_abort_days`日) 3) 期限切れdelete marker掃除)をbucketへ適用(rule一式の置換・冪等。値はすべてaws.tomlから)。Deep Archive最小保存期間(early deletion課金)との整合のため、DEEP_ARCHIVE profileで`noncurrent_days<180`は設定エラー2
- **`manage prune`**(即時実行・dry-run既定): 同一条件で即時にDeleteObjectVersion(`{prefix}data/`一括・`/default.age`終端のみ)。現行version・pin key・metadata keyは構造的保護で対象外

## 5. コマンドリファレンス

### 5.1 共通オプション

新体系の全コマンドが末尾に共通option群を持つ:

| option | 既定 | 意味 |
|---|---|---|
| `--config PATH` | XDG規定path(2.2) | pipeline.toml。省略時は`~/.config/amazing-backup/pipeline.toml`(在れば読む)。照会系は規定pathに無くても動作(9章)。backup push・pin・manageの一部(prune/index/location rm/reset)・backup verify・editは規定path読込が必須(不在はcopy案内付き設定エラー2) |
| `--aws-config PATH` | XDG規定path(2.2) | aws.toml。省略時は`~/.config/amazing-backup/aws.toml` |
| `--key PATH` | pipeline.toml `[verify].decrypt_key` | 復号用SSH ed25519秘密鍵(get/pull・pinの索引読込等)。どちらにも無ければ設定エラー2 |
| `--profile NAME` | aws.toml `[aws].active_profile` | 当該実行限りprofileを上書き(aws.tomlへの書戻しはしない) |
| `--format human|json` | `human` | 照会系8コマンド(list 5種・query・get・diff)の出力形式。`json`はstdout全体が単一JSON(人間向け通知はstderr) |
| `--dry-run` | なし | 実行予定のみ表示(backup push/pullが対応。manage系は`--apply`方式でdry-runが既定・5.10) |

backup verifyのみ旧形式の`--config`/`--key`/`--checksum`/`--roundtrip`を扱う(5.8)。

### 5.2 list — 一覧(照会)

```
amazing-backup list files [--category C]
amazing-backup list versions [--title T] [--category C]
amazing-backup list pins [--title T]
amazing-backup list runs
amazing-backup list jobs
```

| subcommand | 表示内容 |
|---|---|
| `files` | 実体(entity)毎の目録(title・uuid先頭8字・所在数・版数・最新日・最新版size・pins数)。`--category`はいずれかの所在のcategory一致で絞り込み。title重複があれば警告行 |
| `versions` | 実体毎の版一覧(date・version_ts・version_id・size・現行/noncurrent)とpin紐付。`--title`省略時は全実体(指定はtitle/uuid完全一致。索引に不在は対象エラー3) |
| `pins` | pin一覧(title・pin名・付与日・size)。`--title`で実体を絞り込み(title/uuid完全一致) |
| `runs` | run(batch_id)毎の対象実体数(索引由来) |
| `jobs` | 復元job状況(現行/非現行版ごと・pin含む。headのRestore headerをparse) |

旧`list files --tree`・`list categories`は廃止。0件は警告表示+終了コード0。

### 5.3 query — 探索

```
amazing-backup query <手がかり> [--category C] [--versions]
```

title部分一致・uuid前方一致・所在(source_path)部分一致で索引を検索。既定は実体単位要約、`--versions`で版一覧も表示。title重複で候補が複数ある場合は候補一覧(uuid・所在source_path)を表示し`get <title> --path <source_path>`での特定を案内する。0件は警告+終了コード0。

### 5.4 get — 単一実体の取得

```
amazing-backup get <title|uuid> [--path P] [--version V] [--version-id ID] [--pin 名]
                     [--out DIR] [--tier Standard|Bulk] [--days N] [--yes]
```

| 引数/option | 既定 | 意味 |
|---|---|---|
| `<title|uuid>` | -(必須) | 対象実体の識別子(title完全一致・uuid完全一致) |
| `--path` | なし | 同名(title重複)候補の所在特定(所在のsource_path完全一致)。候補複数で`--path`なしは対象エラー3 |
| `--version` | なし(最新) | 版の日付(YYYY-MM-DD。形式不正はusage 64)。同日に複数版がhitしたら候補表示+`--version-id`指定要求(対象エラー3) |
| `--version-id` | なし | 版のS3 VersionId指定(最優先) |
| `--pin` | なし | ピン留め版の取得(pin名) |
| `--out` | -(**必須**) | 出力dir。`<out>/<title>`へ配置(拡張子が必要ならtitleに含める運用)。省略はusage 64(旧既定`data/restore`はTASK-19で廃止) |
| `--tier` | `Standard` | 復元Tier(`Standard`/`Bulk`。allowlist外はusage 64) |
| `--days` | `7` | 復元コピー保持日数(1〜365。範囲外は設定エラー2) |
| `--yes` | なし | sha不一致の既存平文への上書きを許可(7.4) |

動作:

1. 事前検査: 秘密鍵file不在・ageバイナリ不在は設定エラー2
2. 索引を取得して対象を解決(索引が正本)
3. 平文衝突保護(7.4。sha一致skipはローカルregistryで判定できる場合S3 API呼び出しなしで確定)
4. 対象が未復元(DEEP_ARCHIVE等)なら復元jobを発行し「進行中・約12〜48h後に再実行」と案内して**終了コード0**(冪等。再実行で取得が進む)
5. 復元済みなら取得(version_id指定) -> `.age`のsha256を索引記録と照合 -> 復号 -> 平文sha256を索引記録と照合 -> `<out>/<title>`へ配置

sha不一致は検証エラー4・S3 API失敗は転送エラー5。

### 5.5 diff — 完全性突合

```
amazing-backup diff [--run ID]
```

索引とS3実態(ListObjectVersions)を突合し、missing(索引に在るがS3に無い)・unexpected(S3に在るが索引に無い)を報告する(`sys/`予約領域と`data/`配下の`metadata_`接頭辞objectは索引管理外のため突合対象外)。`--run ID`指定時は索引・manifest・実態の三方突合(run完否の監査)。不一致があっても報告して終了コード0(照会command)。

### 5.6 backup push — 書き込み

```
amazing-backup backup push [--from-list FILE] [--dry-run] [--pin <版名>] [--keep-going] <PATH> [<PATH> ...]
```

| 引数/option | 既定 | 意味 |
|---|---|---|
| `<PATH>`(1個以上必須) | -(PATH 0個はusage 64) | 対象アーカイブ(`.zip`/`.7z`) |
| `--from-list` | なし | 対象リストtoml(3.3)。位置引数と併用可(和集合)。title欄は新規発行時の実体ラベル |
| `--dry-run` | なし | 予測表示のみ(旧plan相当。対象ごとのskip/re-put/merge/new予測と合計sizeを表示。新規発行予測でtitle未指定は警告。暗号化せず平文sha256のみ計算しS3 API呼び出しも行わない) |
| `--pin` | なし | backup時ピン(当該runの版を同時にpin名keyへもupload) |
| `--keep-going` | 設定`[behavior].keep_going` | per-file失敗でも継続 |

動作:

1. pin名検証(規則違反はusage 64) -> pipeline.toml必須検査 -> **接続先表示(実効profile・bucket・prefix。dry-runを含む全実行で表示)** -> 対象解決(存在・拡張子・重複) -> from-listのtitle欄検証 -> 分類解決(4.2)
2. `--dry-run`なら予測表示で終了0
3. bucketのversioning確認(無効は設定エラー2)
4. per-file: 所在skip判定(registry基準。identity_hash+size+mtime一致のfile_write成功記録と同一uuidのbucket一致file_upload記録があればskip) -> crash残存(file_writeのみ記録済み)は同uuidへ再PUT -> age暗号化 -> sha照合(run内map・registryのbucket一致lookup) -> 一致なら既存実体へ**合流**(file_write追記のみ・uploadなし) / 不一致なら新規発行(UUIDv7発行・title必須: from-listのtitle欄・ttyなら対話入力・非ttyでtitle無しは当該対象の失敗=対象エラー3。記録順序は file_write追記 -> PUT -> file_upload追記) -> `data/<uuid>/default.age`へupload(64MiB超はmultipart)
5. `--pin`指定時: 新規発行・再PUT対象は手元の`.age`をpin keyへ直接upload。skip・合流対象は最新版からCopyObjectでpin付与(未復元版なら復元job発行で案内)。write-once違反(既存pin名への再付与)はrunを中断せずWARNING表示+当該fileのみpin付与をskipしてrunを完結する(manifest・索引更新は実行される。pin rm後に再付与)
6. run manifest(V2)を暗号化生成して`sys/manifests/<batch_id>.jsonl.age`へupload
7. 索引を全体再構築してPUT(常時・冪等。全skipの再実行でも未反映分が拾われる)+ 当該runで触れた実体のmetadata sync(変更時のみPUT・title変更時は旧metadata keyを掃除)
8. `keep_artifacts="run"`(既定)ならcache dir(`~/.cache/amazing-backup/`)の`<batch_id>/`を掃除(run成功時のみ)

再実行は冪等: 変更の無いfileはskipされ、失敗時の再実行で残りが処理される(crash残存のuuid再利用再PUTも同じ)。終了コード: 成功0 / 設定・環境2 / 対象3(不在・拡張子・key長超過・新規発行のtitle無し等) / 処理4 / 転送5 / 使用法64。

### 5.7 backup pull — 一括復元

```
amazing-backup backup pull (--all | --category C | --run ID) --out DIR [--tier T] [--days N]
                          [--dry-run] [--yes]
```

- `--all` / `--category C` / `--run ID` の**いずれか1つが必須**(指定なし・併用はusage 64)。`--out DIR`(出力dir)も**必須**(省略はusage 64)`--all`と`--category`(いずれかの所在のcategory一致)は索引の全実体の最新版+全pin対象、`--run`は当該run(batch_id)の全版
- **確認ガード**(一括復元の事故防止): `--yes`なしならTTYでyes/no確認(非TTY実行で`--yes`なしはusage 64。noなら中止で終了0)
- 各対象についてgetと同一の取得・検証・復号・平文衝突保護を行う(7.4。出力先は`<out>/<title>`)。未復元対象は復元jobを発行してpending扱い(案内表示。約12〜48h後に再実行)
- `--dry-run`は対象一覧の表示のみ
- 対象0件は警告+終了0。失敗があっても継続して処理し、終了コードは最初のエラー区分

### 5.8 backup verify — ローカル整合検査

```
amazing-backup backup verify [--config PATH] [--checksum] [--roundtrip] [--key PATH]
```

registryとcache dir(`~/.cache/amazing-backup/`)のrun成果物(`.age`)の整合を検証する(S3接続なし・aws不要)。旧経路(encrypt記録)と新経路(file_write記録)の両方を検査する。

| option | 意味 |
|---|---|
| `--checksum` | `.age`のsha256を再計算してregistry記録と照合 |
| `--roundtrip` | 秘密鍵で復号し平文sha256をregistry記録と照合(平文は書き出さない)。manifest復号+registry突合も行う |
| `--key` | 復号用秘密鍵(`--roundtrip`時必須。省略時は設定`[verify].decrypt_key`) |

検証段階は 構造検査(既定: `.age`のsize完全一致・ヘッダrecipient照合・manifest存在) -> `--checksum` -> `--roundtrip` の順。不一致は検証エラー4。秘密鍵・ageバイナリ不在は設定エラー2。

### 5.9 pin — ピン留め版の付与/解除

```
amazing-backup pin add <title|uuid> [--latest | --version V] --as <版名>
amazing-backup pin rm <title|uuid> --pin <版名>
```

対象実体の指定はtitle完全一致またはuuid完全一致(title重複で候補が複数の場合はuuidで一意化)。`pin add`の対象版は`--version V`(日付)指定、省略時は最新版(`--latest`は明示用)。

- `pin add`: 対象版の複写をpin key(`data/<uuid>/<pin名>.age`)へ作成し索引・metadataへ反映。未復元版(DEEP_ARCHIVE)は復元job発行を内包(案内+終了0・再実行で継続)。5GiB超の版は分割copy(UploadPartCopy)で対応
  - pin名規則違反・`--as`欠落はusage 64。**write-once違反(既存pin keyへの上書き)は対象エラー3**(上書きしたい場合は`pin rm`後に再付与)
- `pin rm`: pin objectの版を実体ごと削除(delete marker付与ではなく版削除。DEEP_ARCHIVE配置でも復元job不要で削除可)し索引・metadataへ反映
- 両コマンドとも`--config`(registry記録先)が必須(設定エラー2・S3変更より前に検査)

### 5.10 manage — 保持・索引・lifecycle・job管理

```
amazing-backup manage prune [--dry-run | --apply]
amazing-backup manage lifecycle show|apply
amazing-backup manage index rebuild
amazing-backup manage index reconcile [--apply]
amazing-backup manage location rm <source_path> [--uuid <uuid>]
amazing-backup manage jobs reissue <object_key> [--tier T]
amazing-backup manage reset [--apply]
```

| subcommand | 動作 |
|---|---|
| `prune` | 世代削除の即時実行(`{prefix}data/`一括)。**dry-runが既定**(削除予定を表示)。`--apply`でDeleteObjectVersionを実行し索引を更新。keepはaws.toml `[aws.lifecycle].keep_versions`(実行時上書き不可)。`--dry-run`と`--apply`の併用はusage 64。現行version・pin key・metadata keyは対象外(構造的保護) |
| `lifecycle show` | bucketの現在のlifecycle設定を表示 |
| `lifecycle apply` | `{prefix}data/`一括の3rule(NCVE+MPU abort+期限切れdelete marker掃除)を適用(rule一式の置換・冪等)。値はすべてaws.toml `[aws.lifecycle]`から |
| `index rebuild` | 索引をmanifest(`sys/manifests/`)から再構築(9.2)。完了時に続けての`index reconcile`実行を案内。`--config`必須 |
| `index reconcile` | lifecycle消滅版の索引整合。**dry-runが既定**(件数表示)。`--apply`でsweep記録+索引再構築(S3実在集合へ整合)。`--config`必須 |
| `location rm` | 旧所在の明示除去(`--uuid`でentity絞込可。不在・複数hitは対象エラー3)。location_remove記録+索引再構築+metadata sync。S3・実体には触れない(再backupで所在は復活) |
| `jobs reissue` | 復元jobの再発行(tier変更可。Tierのallowlist外はusage 64)。対象はobject keyで指定 |
| `reset` | dev bucketの初期化(全消去・冪等)。**dry-runが既定**(消去対象version数=Versions・DeleteMarkers別とregistry退避先pathを表示)。`--apply`で 1) ローカルregistryを`./tmp/<yyyymmdd>_registry_before_reset.jsonl`へ退避(不在ならskip) 2) root prefix配下の全object version(Versions・DeleteMarkers)をDeleteObjectVersionで削除(`{prefix}data/`配下に留まらず`sys/`索引・manifestも消去) 3) ローカルregistry削除(cache dirは対象外・手動掃除) 4) 再list_versionsで0件確認。安全弁(違反は設定エラー2+案内・S3 API呼び出しなし): 実効profileのpurpose=dev限定・ローカルregistryに当該bucketと異なるbucket宛のfile_upload記録があれば拒否。`--config`必須 |

`prune`/`index`/`location rm`/`reset`は`--config`必須。`lifecycle`/`jobs reissue`はconfig不要。

### 5.11 edit — 設定の確認

```
amazing-backup edit metadata|files|layout|policy
```

対応設定sectionの現在値を表示する(検証付き読込)。`metadata`は`[metadata].latest_source_path`の掲載形式、`policy`は`[behavior].keep_artifacts`(cache dirの掃除policy。保持数はaws.toml `[aws.lifecycle]`側で管理)。編集自体はpipeline.tomlの直接編集で行う(保存時に検証される)。対象外の指定はusage 64。

### 5.12 終了コード一覧

| code | 区分 | 主な発生条件 |
|---|---|---|
| 0 | 成功 | 対象なし・全skip完結・復元job発行の案内(get/pull)も含む |
| 1 | 内部エラー | 予期しない内部エラー(bug) |
| 2 | 設定・環境エラー | toml不在・schema違反・recipient鍵不正・ageバイナリ不在・aws.toml不正・credentialファイル不在/不正・秘密鍵不在・versioning無効bucket・索引不在(9.2) |
| 3 | 対象エラー | 対象ファイル不在・directory指定・拡張子対象外・重複・key長超過・索引上のtitle/uuid/版/pin不在・候補未特定(getの--pathなし等)・新規発行時のtitle無し(非tty)・write-once違反・sha不一致の既存平文(7.4) |
| 4 | 検証エラー | 取得object/復号平文のsha256不一致・verifyの検証不一致(検査対象ファイル欠損を含む) |
| 5 | 転送エラー | S3 APIの失敗(認証・network含む)・索引GETの失敗 |
| 64 | 使用法エラー | 引数誤り(不明オプション・PATH 0個・pullの範囲指定違反・`--tier`/pin名/日付形式不正等) |

fail-fastが既定(最初のエラーで中止。ただし対象解決段階のエラーは全件列挙してから一度に失敗)。`--keep-going`指定時はper-file失敗を捕捉して継続し、終了コードは最初に発生したエラーの区分。設定エラー2は`--keep-going`でも常に即中止。

## 6. 通常運用の流れ(E2E)

初回の一通りの流れ。repo rootで実行する。`<PATH>`は対象アーカイブ(拡張子`.zip`/`.7z`)。

1. **設定準備**: 規定pathへpipeline.toml(2.2・3.1)・aws.toml(2.2・3.2)を配置しcredentialファイル(2.3)を用意する
2. **予定確認(dry-run)**:

   ```console
   $ amazing-backup backup push --dry-run --from-list targets.toml   # 設定は規定path(2.2)
   ```

   対象ごとの`skip`/`re-put`/`merge`(合流先uuid)/`new`(新規発行・title必須状態を含む)予測と合計sizeを確認する(旧plan相当。エラーはこの段階で全件出る)

3. **書き込み実行**:

   ```console
   $ amazing-backup backup push --from-list targets.toml
   ```

   per-fileの`ok`/`skip`/`merge`/`re-put`行とrun summary(`run <batch_id>: ok=N skip=N failed=N`)が出る。大容量(数十GiB以上)では時間がかかる(進捗はper-file完了行単位)。中断した場合は再実行で再開(skip判定・crash残存のuuid再利用により二重暗号化・二重uploadは起きない)

4. **ローカル整合検査(任意・S3不要)**:

   ```console
   $ amazing-backup backup verify --roundtrip
   ```

   `keep_artifacts="run"`(既定)で掃除済みのrun(当該batchのindex_upload成功記録があり成果物dirが不在)は構造検査・`--checksum`・`--roundtrip`の対象外となり、「verify: 掃除済みrun N件(keep_artifacts=\"run\"の成功run成果物のため構造検査対象外)」と報告される(成功run直後のverifyが必ず検証エラー4にならないようにするため。成果物が残っているrunは従来どおり検査対象)

5. **照会**:

   ```console
   $ amazing-backup list files
   $ amazing-backup query travel
   $ amazing-backup diff
   ```

## 7. 復元手順

### 7.1 単一実体の復元(get)

```console
$ amazing-backup get 旅のしおり2026 --out ~/restore   # title(またはuuid)で指定
```

- DEEP_ARCHIVE配置の対象は、初回実行で復元jobを発行して案内表示+終了0(この時点で取り出し費用が発生)。**約12〜48時間後に同じcommandを再実行**すると取得・検証・復号まで行う
- 版指定: `--version 2026-10-02`(日付)または`--version-id <ID>`(list versionsで確認)。省略時は最新版。ピン留め版は`--pin <版名>`
- 出力は`--out`で指定した`<out>/<title>`(拡張子が必要ならtitleに含める運用。`--out`は必須)
- 復元jobの状況は `list jobs` で確認できる(全objectの現行/非現行版ごと)

### 7.2 一括復元(backup pull)

```console
$ amazing-backup backup pull --category main --out ~/restore        # category単位(最新版+pin)
$ amazing-backup backup pull --run 20261015-abcd1234 --out ~/restore # run単位
$ amazing-backup backup pull --all --out ~/restore                   # 全対象(最新版+全pin)
```

確認ガードあり(5.7)。`--out`は必須。大量対象では復元job発行 -> 待ち -> 再実行の2段階になる点はgetと同じ(pending分は再実行で取得される)。

### 7.3 Tierと費用・時間の目安

| Tier | 復元完了までの目安 | 取り出し料金の目安(GBあたり) |
|---|---|---|
| `Standard`(既定) | 12時間以内 | 約0.10 USD(DEEP Archive復元) |
| `Bulk` | 48時間以内 | 約0.02 USD(Standardの約1/5) |

- `Expedited`はDeep Archiveでは選択不可のため、`--tier`の許容値に含まれない
- 上記は概算値(2026-10時点のAWS公開値が基準。リージョンにより異なる)。**実測確認は実運用開始時に実施する**(TASK-4で費用実測を予定)
- このほかに復元リクエスト単価・復元期間中の復元コピー(`--days`日間)の保管料・GETリクエスト料が発生する
- 費用を抑えるには、(a) getで必要なファイルだけ取り出す、(b) 緊急度が低ければ`--tier Bulk`を使う、の2つが有効

### 7.4 平文衝突保護と冪等再実行

get/pullは出力先の既存平文を保護する:

- 出力先に既存平文があり、そのsha256が索引記録の`source_sha256`と**一致**すれば、取得・復号をskipして終了0(冪等。crash後の再開も再実行で吸収する)
- **不一致**なら上書きせず対象エラー3(手動リネーム・移動後に再実行。意図的な上書きは`--yes`)
- 既存平文が**読込不能**(権限等でsha256検証不能)な場合も不一致同様に保護する(`--yes`なしは対象エラー3。`--yes`は明示的上書き指示のためget/pullとも上書きして続行)
- 取得object・復号平文のsha256は常に索引記録と照合される(不一致は検証エラー4)

## 8. 運用規則

### 8.1 書込み系の同時実行禁止

書き込み系command(**backup push・pin add/rm・manage系**)の並列実行は禁止する(registry append・索引書換えの競合防止。lock file等の自動直列化は個人運用に対し過剰のため不採用)。書き込み系を実行中は、他の書き込み系commandを起動しないこと。照会系(list/query/get/diff)は並列に実行してよい。

### 8.2 profile運用(dev/prod・prod系3bucket)

- dev: 開発検証(STORAGE CLASS STANDARD・復元job待ちがない。DA経路の試験はprodで実施)
- prod系3bucket: 実運用(DEEP_ARCHIVE)。用途別にprofileを使い分ける(3.2):
  - `family` -> `your-family-bucket`
  - `baked` -> `your-baked-bucket`
  - `atara` -> `your-atara-bucket`
- 切替は`active_profile`の書換え(恒久)または`--profile <name>`(当該実行限り)。**誤投入防止のため、prod系への書き込みは`--profile family`等を明示した実行を推奨**(active_profileはdev固定運用)
- **backup pushは実行開始時に接続先(実効profile・bucket・prefix)を表示**する(5.6。dry-runでも表示)。表示を確認してから書き込みを続けることで誤bucketへの投入に気づける
- **名前空間の分離はbucket単位で働く(混同防止)**。prod系3bucketは同一prefix(`amazing-backup/`)を共有するが、ローカルregistry(`~/.local/state/amazing-backup/registry.jsonl`)は単一でも記録の参照(skip判定・索引versions/pins構築・未書込namespace判定・pin解決)はすべて**bucket一致で分離**される。他bucketの書込実績では誤ってskipしない・索引へ他bucketの版やpinが混入しない・未書込救済判定に他bucket実績が採用されない、が構造的に保証される(bucket横断で1byteもuploadされないrunは作れない)

### 8.3 recipient鍵の管理

1. 新規ed25519鍵pairを生成する(passphraseなし):

   ```console
   $ ssh-keygen -t ed25519 -f configs/secret/keys/private/backup_ed25519_2 -N ""
   ```

2. pipeline.toml(規定path)の `[recipients].public_keys` へ公開鍵(.pub)を追加する
3. `amazing-backup backup push --dry-run` を実行して設定エラーが出ないことを確認する(鍵ファイルの不在・形式違い・指紋の重複は設定読込時の設定エラー2で検出される。dry-runは暗号化しないため指紋そのものの表示は無い)

追加以降の書き込みは新recipient構成で暗号化される(索引・manifestも同様)。

- **遡及**: S3へ投入済みのobjectは暗号化時のrecipient構成のまま変わらない。全量を新構成にしたい場合は新たなrunで再pushする(旧objectの削除は`manage prune`や手運用)
- **旧鍵の温存**: 鍵を追加しても旧秘密鍵を廃棄すると、旧recipient構成で暗号化されたobjectを復元できなくなる。追加後も旧鍵は保管し続けること
- **第2recipientの運用要件**: 「S3と秘密鍵のみ」の災害前提が成立するには秘密鍵が単一machineにしか無い状態を解消する必要がある。第2recipient(秘密鍵の地理的第二保管場所)の登録・保管を運用要件とする

## 9. 災害時の手順

### 9.1 前提(作成環境の資産は不要)

照会・復元(list/query/get/diff/backup pull)は**aws.toml(bucket/region/prefix/credentials_file)とAPI keyファイルといずれか1本の秘密鍵のみ**で動作する。最初に`sys/index.jsonl.age`を取得・復号し、以降の操作は索引+S3 APIで完結する(ローカルregistry・cache dir・pipeline.tomlは不要。getの`--key`は必須になる)。

新環境での必要資産: binary(または`cargo build`可能なrepo)・aws.toml(規定path`~/.config/amazing-backup/aws.toml`へ配置・`--aws-config`での明示指定も可)・credentialファイル・秘密鍵。

### 9.2 索引が読めない場合

照会系で索引objectの取得に失敗した(不在・復号失敗)場合は設定・環境エラー2となり、専用案内が表示される。例外として、ローカルregistryに当該prefixへの書込記録が1件も無い「未書込namespace」(dev/prod切り替え直後等)は0件+終了0で扱われる。

索引を復旧する:

```console
$ amazing-backup manage index rebuild   # pipeline.tomlは規定path(2.2)
```

`sys/manifests/`のmanifest(DEEP_ARCHIVE)から索引を再構築する。未復元manifestへはBulk復元jobを発行する(発行したら案内+終了0。**最大48時間の待ちの後に再実行**で継続)。開始時に費用の目安(7.3)が表示される。完了すると索引がPUTされ、以降の照会が復旧する(完了時に続けての`manage index reconcile`実行が案内される・5.10)。

それでも復旧しない場合: aws.tomlのbucket/prefixが正しいか確認する(エラー2の案内に含まれる)。aws cliによるS3側の直接確認(`aws s3api list-objects-v2 --bucket <bucket> --prefix <prefix>sys/`)は最後の手段。

### 9.3 手動復元(aws CLI + age・最後の手段)

amazing-backupを使わない環境での復元手順。対象のkey配置(4.1)と索引(または`aws s3api list-object-versions`での列挙)から対象のuuidを特定する:

1. 復元jobを発行する(DEEP_ARCHIVEの場合):

   ```console
   $ aws s3api restore-object --bucket <bucket> \
       --key amazing-backup/data/<uuid>/default.age \
       --restore-request 'Days=7,GlacierJobParameters={Tier=Standard}'
   ```

   version指定時は`--version-id <ID>`を付ける。進捗は`aws s3api head-object`の`Restore`フィールドで確認する

2. 対象を取得する:

   ```console
   $ aws s3 cp s3://<bucket>/amazing-backup/data/<uuid>/default.age .
   ```

3. 秘密鍵で復号する:

   ```console
   $ age -d -i <秘密鍵> -o <title> default.age
   ```

4. checksum検証: 索引(またはmanifest)を取得・復号し、`.age`のsha256が`age_sha256`と、平文のsha256が`source_sha256`と一致することを確認する:

   ```console
   $ aws s3 cp s3://<bucket>/amazing-backup/sys/index.jsonl.age .
   $ age -d -i <秘密鍵> -o index.jsonl index.jsonl.age
   $ sha256sum <title>
   ```

手動操作は索引・versioningの前提知識を要するため、可能な限りget/backup pullを使うこと。

## 10. 旧サブコマンドからの移行ガイド

旧サブコマンド(plan/encrypt/upload/verify/restore)はTASK-11(Phase B)で削除された。旧手順と新commandの対応:

| 旧手順 | 新command |
|---|---|
| `plan`(対象解決・事前検査) | `backup push --dry-run` |
| `encrypt` + `upload`(暗号化して転送) | `backup push`(1実行で暗号化・upload・manifest・索引更新) |
| `verify`(ローカル整合検査) | `backup verify` |
| `restore request`(復元job発行) | `get` / `backup pull`(未復元なら自動発行)・`manage jobs reissue` |
| `restore status`(復元状態確認) | `list jobs` |
| `restore download`(取得・検証・復号) | `get` / `backup pull` |
| `--date`(バッチ=日付指定) | 廃止。版は`--version`/`--version-id`、runは`--run`で指定 |
| basename語彙(`get <name>`・`list versions --name`等) | title/uuid語彙へ変更(`get <title|uuid>`・`--title`。TASK-18) |
| `list files --tree` / `list categories` | 廃止(TASK-18。キー階層・[categories]設定の廃止に伴う) |

その他の変更点:

- 旧手順の書込先(`<date>/<name>.age`配置)から新配置(4.1)へ変更。旧経路のobjectは新索引の管理外
- **旧サブコマンドとの併用は構造的に不可能**(旧commandがbinaryから削除されたため)
- 旧`encrypt`のskip判定(.ageファイル存在基準)はregistry基準へ変更(.age不在でもregistry記録+指紋一致ならskip)
- name override(`--name`・対象リストの`name`キー)は廃止(basenameは常に元ファイル名)。実体のラベルはtitle、同名衝突のfolder分離も廃止(sha一致は合流・別内容は別entity)
- 旧配置object(旧形式`<date>/<name>.age`・旧`<category>/`階層)は新構成へ変換せず`manage reset --apply`(5.10・registry退避付きの全消去)で削除する。your-dev-bucketは引き続きdev profileのbucketとして使用する(2.5)
