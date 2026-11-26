# amazing-backup 使用手順書

NASバックアップアーカイブ(zip/7z)をageで暗号化し、S3(versioning有効bucket・Deep Archive)へ投入・管理するCLI `amazing-backup` の使用手順書。書き込み・照会・復元を含む。

- 対象読者: バックアップの書き込み・照会・復元を行う運用者(災害時の復旧担当を含む)。amazing-backupを**install済み(repoのcloneは不要)**の環境を前提とする。repoでの開発・検証やrepo内運用の手順は `docs/dev/dev_backup.md` を参照
- 正本参照: 本書の正本はGitHub repo(https://github.com/TomoTom0/amazing-backup)のdocs/。実装挙動の正本は同repoの`tests/design/`の条件書。設計書などdocs外の正本の参照一覧は `docs/dev/dev_backup.md`
- 実装バージョン: 0.1.0
- 最終確認: 2026-11-05(TASK-9: local catalog DB・serve(Web frontend+API)・passkey認証・pending catalog・diff UI・manage catalog/auth系を含む実装に基づく)

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
| 管理 | `manage catalog sync/rebuild/scan` | local catalog DB(2.2)の同期・再構築・アップロード前カタログscan(5.10) |
| 管理 | `manage auth list/remove/api-key` | passkey credential・api-keyの一覧・失効・生成(5.10・serveの認証管理) |
| 閲覧 | `serve` | Web frontend+APIの起動(passkey認証・5.14) |
| 設定 | `config init` | 設定file群(pipeline.toml・aws.toml・credential雛形)の初期生成 |
| 設定 | `edit metadata/files/layout/policy` | 設定sectionの確認 |
| 補完 | `completions <shell>` | shell補完script(bash/zsh/fish)のstdout出力(5.12) |
| 文書 | `docs usage/config` / `docs llm usage/skill/snippet` | 配布ドキュメントのstdout出力(使用手順書・設定example・LLM/agent向け。5.13) |

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

暗号化はSSH ed25519公開鍵の1-of-N(全鍵で暗号化し、いずれか1鍵で復号可能)。S3接続は**AWS SDK(aws-sdk-s3)をbinary内蔵**しておりaws CLIは不要。credential(access key)は明示的なAPI keyファイル(2.3。規定pathでは`~/.config/amazing-backup/aws-credentials.toml`)から与え、`~/.aws`や環境変数のcredential chainには依存しない(fail-closed)。

照会系(list/query/get/diff)は**aws.toml(bucket/region/prefix)と復号用秘密鍵のみで動作する**(索引をS3から取得するため。災害時にローカルregistry・cache dirは不要。9章)。

## 2. セットアップ

### 2.1 前提ツールとinstall

| ツール | 要件 | 確認 |
|---|---|---|
| amazing-backup | install済み(README.md Install節: mise(ubi backend)・`cargo install --git`等) | `amazing-backup --version` |
| age | v1.2系(検証: 1.2.1・`/usr/bin/age`) | `age --version` |

ageはv1.2形式のstanza識別子(指紋prefix)によるヘッダ照合の前提となるため、v1.2系を使用する。

**aws CLIは不要**: S3接続はSDKをamazing-backupが内蔵する。aws CLIがあるとS3側の確認・手動復元(9.3)に使えるが任意である。

repoのclone・buildは不要。設定templateはbinary埋め込みのため、install直後の環境で本書の手順が完結する。

### 2.2 設定ファイルの配置(XDG配置・TASK-19・config init)

設定ファイルはXDG Base Directory準拠の規定pathから読まれる。初回は `amazing-backup config init` で設定file群を生成する(templateはbinary埋め込みのためrepo無し・install直後の環境で実行可能):

| file | 規定path | 生成元 |
|---|---|---|
| pipeline.toml | `$XDG_CONFIG_HOME/amazing-backup/pipeline.toml`(未設定なら`~/.config/amazing-backup/pipeline.toml`) | `configs/pipeline.example.toml`(binary埋め込み・そのまま生成) |
| aws.toml | `$XDG_CONFIG_HOME/amazing-backup/aws.toml`(同上) | `configs/aws.example.toml`(binary埋め込み。`credentials_file`は生成したcredential雛形の絶対pathへ置換) |
| aws-credentials.toml | `$XDG_CONFIG_HOME/amazing-backup/aws-credentials.toml`(同上) | -(空値のcredential雛形・permission 600。2.3) |
| server.toml | `$XDG_CONFIG_HOME/amazing-backup/server.toml`(同上) | `configs/server.example.toml`(binary埋め込み・そのまま生成。serveのserver共通設定・3.4) |
| registry.jsonl | `$XDG_STATE_HOME/amazing-backup/registry.jsonl`(未設定なら`~/.local/state/amazing-backup/registry.jsonl`。dirは自動作成) | -(自動生成) |
| catalog.db | `$XDG_STATE_HOME/amazing-backup/catalog.db`(未設定なら`~/.local/state/amazing-backup/catalog.db`。dir 0700・file 0600で作成) | -(自動生成・local catalog DB) |
| cache dir | `$XDG_CACHE_HOME/amazing-backup/`(未設定なら`~/.cache/amazing-backup/`) | -(run毎の暗号化一時物) |

```console
$ amazing-backup config init
生成: ~/.config/amazing-backup/pipeline.toml
生成: ~/.config/amazing-backup/aws.toml
生成: ~/.config/amazing-backup/aws-credentials.toml (permission 600)
生成: ~/.config/amazing-backup/server.toml (serve設定)
次ステップ:
  1. aws-credentials.toml に access_key_id / secret_access_key を記述
  2. aws.toml の bucket を実際の値へ記述
  3. pipeline.toml の public_keys に SSH ed25519 公開鍵の絶対pathを記述
  4. pipeline.toml のprofileはaws.tomlのprofile名と同名対応です([profiles.<name>]をaws.tomlの[aws.profiles.<name>]と同じ名前で並べます)
  詳細: amazing-backup docs usage(使用手順書)・amazing-backup docs config(設定例)
```

- 生成対象fileが1つでも既に存在する場合(通常file・dangling symlink含む)は、対象毎のerror+復旧案内を表示してall-or-nothingで中止する(exit 2・既存fileは一切変更しない)
- 規定pathに設定fileが無い状態でコマンドを実行すると設定エラー2+`config init`による生成案内が表示される
- 初回の鍵準備: 暗号鍵(SSH ed25519鍵pair)をまだ持っていない場合は、8.3の手順1で鍵pairを生成する。生成したらpipeline.toml(規定path)の`[profiles.<name>.recipients].public_keys`を**生成した公開鍵(.pub)のpathへ置換**する(生成直後の雛形は存在しないpathのdummyのため、置換しないままでは設定エラー2)。あわせて`[profiles.<name>.verify].decrypt_key`へ生成した秘密鍵のpathを記述する(照会・復元の`--key`省略時の既定。`<name>`は実効aws profile名と同名)
- `--config PATH` / `--aws-config PATH` で明示指定した場合はcwd基準の相対pathも可(規定pathより優先)
- `AMAZING_BACKUP_HOME`環境変数(テスト/CI用)を指定すると規定pathをすべて`$ABH/`配下へ統合できる(`$ABH/pipeline.toml`・`$ABH/aws.toml`・`$ABH/server.toml`・`$ABH/registry.jsonl`・`$ABH/catalog.db`・`$ABH/cache/`)

local catalog DB(`catalog.db`)はremote索引のlocal複製(復号後の平文view)とローカル書込の導出indexを保持するSQLite DB(照会系のS3 HEAD/GET削減・5.2)。真正はregistry.jsonl(JSONL)とS3索引のままで、DBは`manage catalog rebuild`(5.10)で常に再構築可能。**平文を含む**ため権限0600・専用dir 0700で作成され、DB fileのbackupは利用者の責任(取扱注意)。
- pipeline.toml内の相対path(public_keys・decrypt_key)とaws.tomlの`credentials_file`の相対pathは**実行時のcwd基準**で解決される(XDG配置後も維持。生成直後の設定はcwd非依存の絶対pathで書かれる)

### 2.3 AWS credential(API keyファイル)の作成

credential(access key)は**明示的なAPI keyファイル**へ置く。SDKへはこのファイルの値を明示渡しし、credential chainへのfallbackは**行わない**(ファイルが無い・不正なら設定エラー2で即終了するfail-closed構成)。access keyの入手先はAWS IAM(IAMユーザーのaccess key作成。書き込み・復元・保持管理を担う権限)。

`config init`を実行すると、空値のcredential雛形`~/.config/amazing-backup/aws-credentials.toml`(permission 600)が生成され、aws.tomlの全profileの`credentials_file`がこの絶対pathを指す(2.2)。この雛形へaccess_key_id/secret_access_keyを記述するだけでよい:

```toml
# ~/.config/amazing-backup/aws-credentials.toml(config initが生成・permission 600)
[credentials]
access_key_id = "<access key id>"         # 必須
secret_access_key = "<secret access key>" # 必須
# session_token = "<session token>"       # 任意(一時credentialを使う場合のみ)
```

- dev/prodでcredentialを分けたい場合はこのファイルを複製・編集して、各profileの`credentials_file`へ個別のpathを記述する(3.2)
- 空値・前後空白付き値・未知キーは設定エラー2(値そのものはerror文に表示されない)
- 値を直接aws.tomlへ書くことはできない(`credentials_file`を除くcredential系キーは未知キーとして拒否)
- 手動で別pathへcredentialファイルを作った場合は`chmod 600`を推奨(秘密情報を含むファイルのため。config init生成物は既に600)

### 2.4 bucket要件(versioning有効)

- **運用上の必須要件: 全profileのbucketでversioningを有効にしておくこと**(版管理・誤DELETE対策の前提・4.3)
- **CLIが起動時に検査する操作は`backup push`(dry-runを除く実書込み時)と`pin add`/`pin rm`のみ**(無効なら有効化commandを表示して設定エラー2)。`manage`系コマンドは検査しないため、versioning無効bucketでの誤操作防止は上記要件の順守で担保する
- versioningの有効化はAWS Consoleまたはaws CLI(任意)で行う:

  ```console
  $ aws s3api put-bucket-versioning --bucket <bucket> \
      --versioning-configuration Status=Enabled
  ```

- lifecycle設定(世代管理のbucket側rule)は `amazing-backup manage lifecycle apply` が行う(任意・5.10)

### 2.5 実行形式

実行形式は `amazing-backup <command> [options]`。任意のdirから実行できる(設定file本体はXDG規定path・2.2から読まれる)。設定内の相対path(public_keys・decrypt_key・credentials_file)は**実行時のcwd基準**で解決される(`--config`/`--aws-config`明示指定もcwd基準の相対path可)。

## 3. 設定ファイル

### 3.1 `pipeline.toml`(規定path: `~/.config/amazing-backup/pipeline.toml`)

暗号化・分類・metadataの設定(秘密を含まない)。**TASK-7でprofile化**: top-levelは`[profiles.<name>]`のみを持ち、現行6section(`[recipients]`/`[behavior]`/`[verify]`/`[metadata]`/`[layout]`/`[files]`)はすべて`[profiles.<name>]`配下に置く(profile毎に独立した暗号鍵・分類設定=namespace毎の暗号学的人格)。**profile名はaws.tomlの`[aws.profiles.<name>]`と同名対応**(実効aws profile名で選択される。`--profile`はaws.tomlとpipeline.tomlの両方の選択を兼ねる・5.1)。全項目を掲げる(`[layout]`/`[files]`/`[metadata]`/`keep_artifacts`は省略時に既定で動く。`[recipients]`のみ必須)。**`[output]`sectionはTASK-19(XDG配置)で廃止**(存在したら設定エラー2。registry・暗号化一時物dirのpathはXDG規定path固定・2.2)。`[categories]`はTASK-18で廃止(保持数はaws.toml `[aws.lifecycle]`へ移管・3.2):

```toml
[profiles.dev]

[profiles.dev.recipients]
# SSH ed25519公開鍵のpath列挙(1-of-N。全鍵で暗号化し、いずれか1鍵で復号可能)
# 相対pathは実行時cwd基準。1つ以上必須。ed25519形式のみ許容(profile毎に独立)
public_keys = [
  "/home/yourname/.config/amazing-backup/keys/dev_ed25519.pub",
]

[profiles.dev.behavior]
# per-file失敗時も継続する既定値(backup pushの--keep-goingで上書きon)
keep_going = false
# 暗号化一時物(cache dir配下の.age)の保持policy。
# "run"(run成功後に当該run分を掃除・既定) | "always"(全保持)
# keep_artifacts = "run"

[profiles.dev.verify]
# 復号検証で使う秘密鍵path(秘密そのものではなくpath参照。相対pathは実行時cwd基準)
decrypt_key = "/home/yourname/.config/amazing-backup/keys/dev_ed25519"

[profiles.dev.metadata]
# metadata_<title>.jsonのlatest_source_path掲載形式
# "plain"(path文字列そのまま・既定) | "encrypted"(age暗号化object) | "none"(null・掲載しない)
latest_source_path = "plain"

[profiles.dev.layout]
# 写像/roots不一致時のcategory(既定"unfiled")
unfiled_category = "unfiled"
# source_path相対化のroot列挙(順序優先・絶対path。省略可)
auto_path_roots = ["/home/yourname/data"]

# [profiles.dev.catalog.scan](省略可=アップロード前カタログscan対象の明示登録。
# manage catalog scan・serveの未対応一覧(pending絞り込み)がこのroot配下をlocal FS走査する):
# [profiles.dev.catalog.scan]
# roots = ["/home/yourname/data"]      # 走査root(絶対path・1つ以上必須)。symlinkは辿らない
# extensions = [".zip"]                # 対象拡張子(省略・空=全regular file)
# exclude = ["tmp"]                    # 除外(root基準相対path部分一致。省略可)

# [profiles.dev.files."travel.zip"]  # 手動写像(省略可)
# category = "photos"                 # category名規則を満たす任意の文字列(記録値として自由化)
# label = "2020-2023/travel"           # label(""でlabelなし)

# 第2profile例(aws.tomlの[aws.profiles.family]と対。追加profileは[profiles.<name>]を
# 複製して鍵等を差し替える)
[profiles.family]

[profiles.family.recipients]
public_keys = [
  "/home/yourname/.config/amazing-backup/keys/family_ed25519.pub",
]

[profiles.family.verify]
decrypt_key = "/home/yourname/.config/amazing-backup/keys/family_ed25519"

[profiles.family.metadata]
latest_source_path = "encrypted"

[profiles.family.layout]
unfiled_category = "unfiled"
auto_path_roots = ["/mnt/nas/family"]
```

各キーの説明:

| キー | 意味 |
|---|---|
| `[profiles.<name>]` | profile定義のroot。`<name>`は`^[a-z0-9][a-z0-9-]{0,63}$`(category名規則と同一・aws.tomlのprofile名と同名対応) |
| `[profiles.<name>.recipients].public_keys` | 当該profileのrecipient公開鍵(SSH ed25519)のpath列挙。1-of-N。鍵の追加手順は8.3 |
| `[profiles.<name>.behavior].keep_going` | per-file失敗時も継続する既定値。`--keep-going`でonに上書き |
| `[profiles.<name>.behavior].keep_artifacts` | cache dir(`~/.cache/amazing-backup/`)配下の`.age`保持policy。`run`(既定: run成功後に当該run分を掃除)/`always`(全保持)。TASK-19で`[output]`廃止に伴い`[behavior]`へ移設 |
| `[profiles.<name>.verify].decrypt_key` | 復号用秘密鍵のpath参照。get/pull/verifyの`--key`省略時の既定(**実効profileのもの**が使われる) |
| `[profiles.<name>.metadata].latest_source_path` | metadata_<title>.jsonの最新所在掲載形式。`plain`(既定)/`encrypted`/`none` |
| `[profiles.<name>.layout].unfiled_category` | 分類解決のfallback先category(既定`unfiled`) |
| `[profiles.<name>.layout].auto_path_roots` | source_pathを相対化するroot列挙(最初に一致したrootを採用) |
| `[profiles.<name>.files."<basename>"]` | 手動写像。`category`(必須・category名規則の文字列検査のみ)と`label`(所在label・空文字列=なし) |
| `[profiles.<name>.catalog.scan]` | アップロード前カタログscan(5.10 `manage catalog scan`・serveの未対応一覧(pending絞り込み))の対象登録。省略可。`roots`(必須: 走査rootの絶対path列挙。この配下のみ走査しsymlinkは辿らない)・`extensions`(対象拡張子`.zip`形式の列挙。省略・空=全regular file)・`exclude`(走査root基準の相対path部分一致による除外。一致dir配下も対象外。省略可)。**`auto_path_roots`は転用しない**(分類・相対化用の設定でありbackup対象の登録ではないため。誤った範囲拡大防止) |

省略時の既定: `[layout]`全体省略時はunfiled_category="unfiled"+auto_path_roots=[]・`[metadata]`省略時はlatest_source_path="plain"・`keep_artifacts`省略時は"run"。`[recipients]`のみ必須(選択profileに対して)。`[categories]`・`[output]`セクションの存在は(top-level・profile内のいずれでも)未知キーとして設定エラー2(静的に廃止を確定。registry・cache dirのpathはXDG規定path固定)。

**旧形式(top-level 6section)からの移行手順**(手動編集。移行支援commandは無い。旧形式のまま読み込むと専用の移行案内error(設定エラー2)になる):

1. 現行の6section(`[recipients]`/`[behavior]`/`[verify]`/`[metadata]`/`[layout]`/`[files]`)を、現在のaws.tomlの実効profile名(通常は`active_profile`の値)の`[profiles.<name>]`配下へネストする(例: `[recipients]` → `[profiles.dev.recipients]`。error文の案内もこの形式)
2. 追加profileを作る場合は`[profiles.<name>]`を複製して鍵等を差し替える(aws.toml側へ同名の`[aws.profiles.<name>]`を併せて用意・3.2)
3. 旧sectionが1つでもtop-levelに残っている状態(部分的な移行)は同じく移行案内errorになるため、すべてのsectionを移動し切ること

検証規則(違反はすべて設定エラー・終了コード2)。検証は2層:

- 第1層(全profile一括・deserialize時): ファイル不在・TOML parseエラー・未知キー・型不一致・profile名規則(未選択profileの違反でも弾く)
- 第2層(選択profileのみ): `[recipients]`必須・`public_keys`空配列・鍵ファイル不在・`ssh-ed25519`以外の形式・指紋の重複(検査範囲は選択profile内限定。未選択profileの鍵pathの実在性は検証しない=dummy鍵pathを置ける)
- 値検査(選択profileのみ): category名規則違反(`^[a-z0-9][a-z0-9-]{0,63}$`。予約領域`sys`・旧表記`_sys`は拒否。files写像とunfiled_categoryに適用)・auto_path_rootsが絶対pathでない・keep_artifacts列挙値外・latest_source_path列挙値外・`[catalog.scan]`の値検査(roots空配列・rootsが絶対pathでない・extensionsが拡張子形式(`.zip`等・先頭ドット)でない・extensions/excludeの空文字列は設定エラー2)
- 実効aws profile名に対応する`[profiles.<name>]`が無い場合は設定エラー2(利用可能なpipeline profile一覧がerror文に表示される。pipeline.tomlが単一profileでも名前一致は必須。aws.toml側に存在しない余分なprofileは選択されない限り無視)

### 3.2 `aws.toml`(規定path: `~/.config/amazing-backup/aws.toml`)

S3接続設定(credential値そのものは不含)。profile構成(dev=開発検証1profile + prod=実運用3bucket使い分け)。`[aws.lifecycle]`はlifecycle 3ruleの値(全profile共通・表末尾と4.4)。初回は`config init`で生成する(2.2。template正本はrepoの`configs/aws.example.toml`・initは全profileの`credentials_file`を生成したcredential雛形の絶対pathへ置換するが、bucketは`<dev-bucket>`等のplaceholderのまま)。生成後にbucket等を実際の値へ書き換えて使う。下記はconfig init生成物を書き換えた記述例(dev+prod系3bucket使い分けまで含む形。initの生成物はdev+prod系1profile(family)の2profile構成のため、prod系bucketを増やす場合は`[aws.profiles.<name>]`を追加する):

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
bucket = "your-dev-bucket"
region = "ap-northeast-1"
prefix = "amazing-backup/"
storage_class = "STANDARD"
credentials_file = "/home/yourname/.config/amazing-backup/aws-credentials.toml"

[aws.profiles.family]           # 実運用(prod系3bucket・DEEP_ARCHIVE)
purpose = "prod"
bucket = "your-family-bucket"
region = "ap-northeast-1"
prefix = "amazing-backup/"
storage_class = "DEEP_ARCHIVE"
credentials_file = "/home/yourname/.config/amazing-backup/aws-credentials.toml"

[aws.profiles.baked]            # 実運用(prod系)
purpose = "prod"
bucket = "your-baked-bucket"
region = "ap-northeast-1"
prefix = "amazing-backup/"
storage_class = "DEEP_ARCHIVE"
credentials_file = "/home/yourname/.config/amazing-backup/aws-credentials.toml"

[aws.profiles.atara]            # 実運用(prod系)
purpose = "prod"
bucket = "your-atara-bucket"
region = "ap-northeast-1"
prefix = "amazing-backup/"
storage_class = "DEEP_ARCHIVE"
credentials_file = "/home/yourname/.config/amazing-backup/aws-credentials.toml"
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
- **prod系3profileのcredentials_fileは共通1本**(同一AWS account運用を想定し同一ファイルを参照。2.3の複製・編集でbucket別credentialへ分割する場合もschema上profile毎に独立したkeyのためaws.tomlの書換えのみで対応できる)
- **test profileは廃止**(旧`test`/共用bucket your-dev-bucket構成の解除)
- `[aws.lifecycle]`の3キーは必須(省略は設定エラー2・コード内に既定値なし)。profile横断で同一構成のため`[aws]`直下に一元化
- **aws profile名にもpipelineと同一の名前規則 `^[a-z0-9][a-z0-9-]{0,63}$` を適用する**(TASK-7: pipeline.tomlの`[profiles.<name>]`と同名対応の構造的保証。違反名は設定エラー2)。規則違反名を持つ既存aws.tomlは新binaryで設定エラー2になる(破壊的変更)ため、次の手順で改名する: (1) `[aws.profiles.<name>]`のkeyを規則適合名へ変更 (2) `[aws].active_profile`の値を変更 (3) `--profile`指定・env `AMAZING_BACKUP_AWS_PROFILE`の指定値を変更 (4) pipeline.tomlの`[profiles.<name>]`も同じ新名へ変更(credentialファイルのpath・bucket等は不変)
- **profile追加時はpipeline.tomlへ対応する`[profiles.<name>]`entryを併せて作る**(3.1。aws.tomlだけprofileを増やしpipeline側entryを置かないと、当該profile選択時に設定エラー2になる)
- 複数profileが同一bucketを参照する場合は警告をstderrへ表示する(処理は継続・8.2の運用規則を参照)
- profileの切替は`active_profile`書換えまたは`--profile`オプション(5.1。当該実行限りの一時上書き・aws.tomlとpipeline.tomlの両方を選択)。prod系の書き込みは`--profile family`等の明示指定を推奨(8.2)
- 検証規則(設定エラー2): prefix形式違反・storage_class allowlist外・bucket/region/credentials_fileの空・前後空白付き値・credentialファイルの不在・不正・lifecycle値の範囲違反(keep_versions 1〜100・noncurrent_days/mpu_abort_days 1以上・DEEP_ARCHIVE profileでnoncurrent_days<180)・profile名規則違反(`[aws.profiles]`のkey・active_profile・`--profile`/env指定値のすべてに適用)

**エンドポイント確定性の注記**: SDKは環境変数`AWS_ENDPOINT_URL`/`AWS_ENDPOINT_URL_S3`を解釈して接続先endpointを切替え得る。amazing-backupは起動時にこれらのenvが設定されていれば警告をstderrへ表示する(処理は継続)。意図した検証用構成か誤設定かは利用者側で確認すること。

### 3.3 対象リストtoml(`--from-list`)

backup pushの対象をファイルで指定する場合は次のschemaのtomlを作る(実運用のリストは都度作成・任意のpathに置ける):

```toml
[[targets]]
path = "/mnt/nas/backup/photo.zip"
title = "旅行写真2026"          # 任意・実体のtitle(新規発行時に必須・5.6)

[[targets]]
path = "/mnt/nas/backup/2025/docs.7z"   # title省略可(合流時・5.6)
```

- `path`(必須): 対象アーカイブのpath
- `title`(任意): 実体のtitle。新規uuid発行が確定した対象で必須(非ttyでの省略は当該対象の失敗・対象エラー3)。合流・再PUT時は省略なら現行titleを継続(指定が異なればtitle変更扱い)
- 位置引数との併用可(和集合)。単独指定可(PATH 0個+--from-listのみでも実行)。[[targets]]0件の空リストは警告+終了0。未知キー・`path`欠損は設定エラー
- 旧仕様の`name`(出力名override)は**廃止**(指定は拒否)。basenameは常に元ファイル名(拡張子付き)

### 3.4 `server.toml`(規定path: `~/.config/amazing-backup/server.toml`)

serve(5.14)のserver共通設定(秘密を含まない)。backup profile(pipeline.toml・aws.toml)とは分離しており、serveは既存のprofile解決chain(`--profile` > env > purpose > active_profile > default)で単一の実効profileを解決する(API対象scope・復号鍵は実効profileに従う)。初回は`config init`で生成され(2.2)、各キー省略時・file自体が無い場合も既定値で起動する(file不在時はstderrへnotice表示。server.tomlは秘密値を持たないため既定値起動が安全側):

```toml
[server]
listen_addr = "127.0.0.1"           # listen address(v1はlocalhost既定。LAN IP直結は不可=3.4注記)
port = 40510                        # listen port(40510=本番serve・40512=Vite dev server)
rp_id = "localhost"                 # WebAuthn RP ID(外部公開時は当該domainへ変更)
rp_origin = "http://localhost:40510" # WebAuthn RP origin(port変更時は合わせて変更)
```

| キー | 既定 | 意味 |
|---|---|---|
| `listen_addr` | `127.0.0.1` | listen address。v1はlocalhost既定(公開範囲は5.14) |
| `port` | `40510` | listen port |
| `rp_id` | `localhost` | passkey認証のWebAuthn RP ID |
| `rp_origin` | `http://localhost:40510` | passkey認証のRP origin |

- LAN IP直結はWebAuthnのsecure context違反のため不可(localhostはhttp可。外部公開はcf-tunnel等のdomain+https経由・5.14)。同一passkeyはRP ID違いで別credentialになる(localhostとdomainの両用には各originで登録)
- `[server]`内の未知キーはtypo検出のため設定エラー2

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

- 索引は照会系(list/query/get/diff)の正本。schema=3で実体(uuid・title・所在locations)単位のentry構成(title重複は許容・一意化はuuid/所在指定)。run毎書込み・backup push完了時と索引に影響する操作後(pin add/rm・manage prune/location rm)に全体再構築してPUTされる(冪等)
- 索引の旧版はS3 versioningのnoncurrent versionとして残る(小容量のため蓄積を許容)
- metadata_<title>.jsonはregistry reduceから導かれる正規形(現行版のsha/size/version_id・pins・最新所在latest_source_path)。内容が変わったときのみPUTされ、title変更時は新キーPUT+旧キーにdelete marker
- storage classは索引とmetadataがSTANDARD固定、それ以外(default.age・pin・manifest)はprofileのstorage_class(prod=DEEP_ARCHIVE)

### 4.2 分類解決・title・合流

分類(category/label)はキー構成には影響せず、所在(location)の記録値として索引・manifestに記録される(絞り込み・表示の語彙)。分類は次の優先順位で解決される:

1. **files写像**(最優先): `[files."<basename>"]`の`category`と`label`をそのまま用いる
2. **auto_path_roots相対化**: source_pathがいずれかのroot配下なら、最初に一致したroot配下のdirectory側path(ファイル名を除く)を`label`とし、categoryは`unfiled_category`
3. **fallback**: 一致rootなし -> labelなし・category=`unfiled_category`

- 実体の識別はuuid(発行キー)とtitle(人間用ラベル・64文字以内・UTF-8可。`/`・改行・制御文字は拒否で違反はusage 64)。title重複(別uuid同title)は許容+警告で、一意化はuuidまたは`--source-path <source_path>`(所在完全一致)指定で行う
- 平文sha256が既存実体と一致する対象は**合流**する(所在追加のみ・uploadなし・版増加なし)。移動+内容不変は合流、移動+内容変更は合流せず別entityとして新規発行(title必須)
- labelは`.`/`..`要素の排除・正規化のみ適用(実source treeの要素名をそのまま用いる)
- 生成keyがS3のobject key上限1024 byteを超える場合は対象エラー3(prefix長またはtitle長で対処)
- category名規則は`^[a-z0-9][a-z0-9-]{0,63}$`・予約語`sys`(旧表記`_sys`も)拒否。pin名は`^[a-z0-9][a-z0-9-]{0,62}$`(63文字以内)で`default`は予約
- **tag・collection(serve 5.14)はlocal語彙**: 分類(category/label)が索引・manifestに記録されるのに対し、tag(entityへの検索語彙・複数付け可)とcollection(名前付きグループ)はcatalog DBのlocal tableのみに保存され索引・manifestには入らない(索引はpush毎に再構築されるためlocal置きしないと消失する)。tag名規則はcategoryと同一の`^[a-z0-9][a-z0-9-]{0,63}$`。付与・編集はserveのUI/APIから行う(CLIからの付与手段は無い)

### 4.3 versioningと索引

- bucketのversioningは必須(2.4)。`default.age`への上書きuploadごとに新VersionIdが払い出され、索引に版として記録される(date・version_ts・version_id・sha256×2・size)
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
| `--config PATH` | XDG規定path(2.2) | pipeline.toml。省略時は`~/.config/amazing-backup/pipeline.toml`(在れば読む)。照会系・backup pullは規定pathに無くても動作(9章。`--key`必須)。backup push・pin・manageの一部(prune/index/location rm/reset)・backup verify・editは規定path読込が必須(不在は`config init`生成案内付き設定エラー2) |
| `--aws-config PATH` | XDG規定path(2.2) | aws.toml。省略時は`~/.config/amazing-backup/aws.toml`(backup verifyも含め全コマンドで読み込まれる・5.8) |
| `--key PATH` | 実効profileの`[profiles.<name>.verify].decrypt_key` | 復号用SSH ed25519秘密鍵。索引の復号に使うためlist/query/get/diff・pull・pinのすべてで使用(pipeline.tomlが規定pathに無い場合・実効profileのentryが読めない場合は必須・9.1)。どちらにも無ければ設定エラー2 |
| `--profile NAME` | aws.toml `[aws].active_profile` | 当該実行限りprofileを上書き(aws.tomlへの書戻しはしない)。**aws.tomlとpipeline.tomlの両方のprofileを選択する**(実効pipeline profileは常に実効aws profile名と同一のため「aws=dev・pipeline=family」のような交叉は構造的に起きない・3.1) |
| `--format human|json` | `human` | 照会系8コマンド(list 5種・query・get・diff)の出力形式。`json`はstdout全体が単一JSON(人間向け通知はstderr) |
| `--refresh` | なし | 索引cacheを検証せずS3から再取得してDBを更新(`list files/versions/pins/runs`・`query`が対応・5.2) |
| `--dry-run` | なし | 実行予定のみ表示(backup push/pullが対応。manage系は`--apply`方式でdry-runが既定・5.10) |

backup verifyのみ旧形式の`--config`/`--key`/`--checksum`/`--roundtrip`を扱う(5.8。`--aws-config`/`--profile`も受理する)。

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

索引cache(2.2のcatalog.db): `files`/`versions`/`pins`/`runs`と`query`(5.3)は、起動時に索引objectのHEAD 1回でvalidator(ETag+VersionId)を確認し、検証済みfreshならS3 GET・復号を行わずlocal catalog DBから回答する(書込系実行直後の同一process内は、upload時の応答VersionIdがHEADの現行version_idと一致した場合に限りfresh化する。upload→HEAD確認の間に他端末が索引を書換えていた場合はfresh化せず当該scopeを失効扱いにする=他端末の索引と自端末の旧cache内容の組み合わせで回答しない。versioning無効bucketでupload応答がVersionIdを返さない場合は従来どおりHEAD応答のみで判断)。validator不一致時は従来どおりS3 GET+復号で回答しDBを更新する。`--refresh`でvalidatorに関係なく再取得を強制できる。`get`・`diff`・`jobs`は実態突合・per-object headが必要なため従来どおりS3直接(cache不使用)。

旧`list files --tree`・`list categories`は廃止。0件は警告表示+終了コード0。

### 5.3 query — 探索

```
amazing-backup query <手がかり> [--category C] [--versions]
```

title部分一致・uuid前方一致・所在(source_path)部分一致で索引を検索。既定は実体単位要約、`--versions`で版一覧も表示。title重複で候補が複数ある場合は候補一覧(uuid・所在source_path)を表示し`get <title> --source-path <source_path>`での特定を案内する。0件は警告+終了コード0。索引cacheの挙動はlist系と同一(5.2。`--refresh`対応)。

### 5.4 get — 単一実体の取得

```
amazing-backup get <title|uuid> [--source-path P] [--version V] [--version-id ID] [--pin 名]
                     [--out DIR] [--tier Standard|Bulk] [--days N] [--yes]
```

| 引数/option | 既定 | 意味 |
|---|---|---|
| `<title|uuid>` | -(必須) | 対象実体の識別子(title完全一致・uuid完全一致) |
| `--source-path` | なし | 同名(title重複)候補の所在特定(所在のsource_path完全一致)。候補複数で`--source-path`なしは対象エラー3 |
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
amazing-backup backup push [--from-list FILE] [--dry-run] [--pin <版名>] [--keep-going] [<PATH> ...]
```

| 引数/option | 既定 | 意味 |
|---|---|---|
| `<PATH>` | - | 対象アーカイブ(`.zip`/`.7z`)。`--from-list`とのいずれかが必要(両方0個はusage 64) |
| `--from-list` | なし | 対象リストtoml(3.3)。単独指定可・位置引数と併用可(和集合)。title欄は新規発行時の実体ラベル |
| `--dry-run` | なし | 予測表示のみ(旧plan相当。対象ごとのskip/re-put/merge/new予測と合計sizeを表示。新規発行予測でtitle未指定は警告。暗号化せず平文sha256のみ計算しS3 API呼び出しも行わない) |
| `--pin` | なし | backup時ピン(当該runの版を同時にpin名keyへもupload) |
| `--keep-going` | 設定`[profiles.<name>.behavior].keep_going` | per-file失敗でも継続 |

動作:

1. pin名検証(規則違反はusage 64) -> **aws.toml読込(実効profile解決) -> pipeline.toml読込(実効profileの`[profiles.<name>]`)** -> **接続先表示(実効profile・bucket・prefix。dry-runを含む全実行で表示)とrecipient指紋表示(`recipients(N):`行+指紋全文`SHA256:<base64>`を鍵毎1行。「どこへ・誰の鍵で書くか」を1画面で確認する。dry-runでも表示)** -> 対象解決(存在・拡張子・重複) -> from-listのtitle欄検証 -> 分類解決(4.2)。対象解決結果が0件(from-list空リスト)は警告表示+終了0(S3 API呼出なし)
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
amazing-backup backup verify [--config PATH] [--aws-config PATH] [--profile NAME]
                             [--checksum] [--roundtrip] [--key PATH]
```

registryとcache dir(`~/.cache/amazing-backup/`)のrun成果物(`.age`)の整合を検証する(S3接続なし)。

- **aws.toml必須(TASK-7)**: 実効profile名の解決にaws.tomlを読む(aws側解決chain(env・purpose・active_profile)と同一経路のため、verify経路での交叉事故が構造的に起きない。旧「aws不在でも構造検査可」からの破壊的変更)。credentialファイルの実在は不要(値の検査はS3書き込み系のみで行う)。verify自体はS3接続しない・秘密値を扱わないのは不変
- **検査対象は実効profileのbucketへ帰属するrunのみ**(TASK-7 profile別鍵の下で、別profileの正常な成果物を復号失敗として誤報告しないため)。runのbucket帰属はmanifest/index_upload recordの`s3_bucket`から判定する。bucket帰属が確定できない記録(旧経路encrypt記録・upload未完でfile_writeのみのcrash残存)と他bucket帰属のrunは検査対象外とし、対象外となったbatch idを特定表示した警告を出す(検査対象runが無くてもexit 0)
- `--profile`指定時は当該profileのbucket帰属runのみが検査対象になる

| option | 意味 |
|---|---|
| `--checksum` | `.age`のsha256を再計算してregistry記録と照合 |
| `--roundtrip` | 秘密鍵で復号し平文sha256をregistry記録と照合(平文は書き出さない)。manifest復号+registry突合も行う |
| `--key` | 復号用秘密鍵(`--roundtrip`時必須。省略時は実効profileの`[profiles.<name>.verify].decrypt_key`) |

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
amazing-backup manage catalog sync|rebuild|scan [--sha256]
amazing-backup manage auth list
amazing-backup manage auth remove <credential_id>
amazing-backup manage auth api-key [--name NAME]
```

| subcommand | 動作 |
|---|---|
| `prune` | 世代削除の即時実行(`{prefix}data/`一括)。**dry-runが既定**(削除予定を表示)。`--apply`でDeleteObjectVersionを実行し索引を更新。keepはaws.toml `[aws.lifecycle].keep_versions`(実行時上書き不可)。`--dry-run`と`--apply`の併用はusage 64。現行version・pin key・metadata keyは対象外(構造的保護) |
| `lifecycle show` | bucketの現在のlifecycle設定を表示 |
| `lifecycle apply` | `{prefix}data/`一括の3rule(NCVE+MPU abort+期限切れdelete marker掃除)を適用(rule一式の置換・冪等)。値はすべてaws.toml `[aws.lifecycle]`から |
| `index rebuild` | 索引をmanifest(`sys/manifests/`)から再構築(9.2)。完了時に続けての`index reconcile`実行を案内。旧schema manifest file record(schema≠3・label無し)検出時は設定エラー2で失敗(初期化と再pushが必要)。`--config`必須 |
| `index reconcile` | lifecycle消滅版の索引整合。**dry-runが既定**(件数表示)。`--apply`でsweep記録+索引再構築(S3実在集合へ整合)。`--config`必須 |
| `location rm` | 旧所在の明示除去(`--uuid`でentity絞込可。不在・複数hitは対象エラー3)。location_remove記録+索引再構築+metadata sync。S3・実体には触れない(再backupで所在は復活) |
| `jobs reissue` | 復元jobの再発行(tier変更可。Tierのallowlist外はusage 64)。対象はobject keyで指定 |
| `reset` | dev bucketの初期化(S3全消去・registryはbucket限定・冪等)。**dry-runが既定**(消去対象version数=Versions・DeleteMarkers別・registry退避先path・registry再書込の除去/残行数を表示)。`--apply`で 1) ローカルregistryを`./tmp/<yyyymmdd>_<bucket>_registry_before_reset.jsonl`へ全体退避(証跡。不在ならskip。同日再実行は上書き) 2) root prefix配下の全object version(Versions・DeleteMarkers)をDeleteObjectVersionで削除(`{prefix}data/`配下に留まらず`sys/`索引・manifestも消去) 3) ローカルregistryから当該bucket由来行(対象bucket宛file_uploadはuuid共有(非対象bucketにもuploadされたuuid)に関わらず全て除去+S3実体削除後の記録残存はregistry-only skipとの不整合のため。他のupload系記録と対象bucketのみにuploadされたuuidのfile_write)のみ除去して再書込(temp+fsync+rename+親dir fsync。他bucket宛記録・共有uuidのfile_write・crash残存file_write・prune/sweep等は温存。torn write最終行は破棄。残0行ならfile削除。cache dirは対象外・手動掃除) 4) 再list_versionsで0件確認。安全弁(違反は設定エラー2+案内・S3 API呼び出しなし): 実効profileのpurpose=dev限定。`--config`必須 |
| `catalog sync` | remote索引をlocal catalog DB(2.2)へ同期(遅延refresh)。HEAD 1回でvalidator(ETag+VersionId)を検証し、変化(初回含む)あればGET+復号でDBへ反映・変化なければGETを省略。反映の置換時にversions/locationsのmtime基線(`catalog scan`の分類基準)は既存DB行から引き継がれる(local backup記録の基線がsyncで失われず、未変更fileがchangedに誤分類されない)。書込実績の無いnamespaceは空cacheとして確定(S3問い合わせなし)。索引object不在は`index rebuild`案内の設定エラー2(未書込namespace例外は除く) |
| `catalog rebuild` | ローカルregistry(JSONL)からcatalog DBのcache class(entities/versions/pins/locations/sync_state/pending_items)を置換再構築(冪等)。永続class(credential・api-key・実行記録・tag/collections/audit_log=UI書込操作記録)は保持・削除しない(tag・collectionは索引から再構築できないlocal語彙のためrebuildで消失しない・4.2)。cacheは常にregistry+S3から再構築可能な導出index(2.2) |
| `catalog scan` | アップロード前カタログscan。pipeline.toml `[profiles.<name>.catalog.scan]`(3.1)の登録root配下をlocal FS走査し(S3接続不要・symlink不追従)、各fileをcatalog DBの保存値とsize+mtime比較で4状態(unbacked=未backup・changed=変更あり・ok=backup済み・unknown=確認不能)へ分類してpending_itemsを全置換(再scanで前回結果は残らない)。`--sha256`指定時のみsha256を再計算しsha一致合流(registryのsha_uuid_lookup)でunknownをokへ確定する(既定はsize+mtime比較のみ)。serveの未対応一覧(pending絞り込み)(5.14)は同一scanをボタン実行できる |
| `auth list` | 登録済みcredential一覧(passkey・api-keyの統合表示: credential_id・kind=passkey/api_key・name・created_at)。catalog DB(2.2)のcredentials tableとapi_keys table(永続class)を直接読むlocal操作でS3接続不要。0件案内(bootstrap案内)は両table合計0件時のみ(5.14) |
| `auth remove <credential_id>` | credentialの失効(削除)。credential_id完全一致の1行をcredentials table→api_keys tableの順に検索して削除するlocal操作(S3接続不要。nameは一意性が無いため対象指定に使わない。passkey・api-keyいずれのcredential_idでも削除可)。両table合計削除0行(存在しないcredential_id)は対象エラー3。削除後に両table合計0件となった場合は再登録のための`serve --bootstrap`案内を表示する(合計0件ではlogin手段が無く・passkey登録はbootstrap token提示時のみ開く=fail-closed。api-key失効は既存sessionを破棄しない。5.14) |
| `auth api-key [--name NAME]` | serve用api-keyの生成。32byte乱数から`abk_`前置+base64url(43文字)のkeyを生成し**1回だけstdoutへ表示**する(再表示不可・hashから復元不可。SHA-256 hexのみapi_keys tableへ保存し平文keyは保存しない)。`--name`はlist表示での確認用表示名(既定`api-key`・空文字指定はusage error)。S3接続不要のlocal操作。受け渡し・有効条件は5.14 |

`prune`/`index`/`location rm`/`reset`/`catalog`は`--config`必須。`lifecycle`/`jobs reissue`/`auth`はconfig不要。

### 5.11 edit — 設定の確認

```
amazing-backup edit metadata|files|layout|policy
```

対応設定sectionの現在値を表示する(検証付き読込)。各対象の出力1行目に**実効profile名**を表示する(TASK-7: 表示中のsectionが`[profiles.<実効profile名>]`配下のものであることを明示)。`metadata`は`[profiles.<n>.metadata].latest_source_path`の掲載形式、`policy`は`[profiles.<n>.behavior].keep_artifacts`(cache dirの掃除policy。保持数はaws.toml `[aws.lifecycle]`側で管理)。編集自体はpipeline.tomlの直接編集で行う(保存時に検証される)。対象外の指定はusage 64。

### 5.12 completions — shell補完script

```
amazing-backup completions <bash|zsh|fish>
```

指定shell(bash/zsh/fishの3つ)の補完scriptをstdoutへ出力する。設定読込を行わない純粋出力commandのため、設定不在環境でも動作する。生成はbinary自身が行うため、常に当該versionのcommand語彙と一致する。

- 補完scriptの配置手順(zshのfpath・bash-completion・fishの規定dir)とupgrade時の再実行はREADME.mdのShell completions節を参照
- 対象外のshell指定はusage 64。`| head`等の先読み終了(BrokenPipe)は正常終了0、その他の出力失敗は処理エラー4

### 5.13 docs — 配布ドキュメント

```
amazing-backup docs [usage|config|llm <usage|skill|snippet>]
```

配布ドキュメントをstdoutへ出力する。正本はrepo file(docs/・configs/)をbinary埋め込みしたもので、設定読込を行わない純粋出力commandのため設定不在環境でも動作する(completionsと同じ。binary自身のversionに対応した内容が出される=GitHub上のdocs(HEAD版)と異なる場合がある点を解消する経路)。

| page | 出力 |
|---|---|
| `usage` | 本書(使用手順書)全文。冒頭の「実装バージョン」行はbinary自身のversionへ置換される |
| `config` | 設定example全文(pipeline.toml+aws.tomlの2file。marker `__CREDENTIALS_FILE__` は例示path `~/.config/amazing-backup/aws-credentials.toml` へ置換済み。config initと同一の変換経路) |
| `llm usage` | LLM/agent向けusage(コマンド契約・終了コード一覧・運用規則の機械的参照。`docs/llm/usage.md`) |
| `llm skill` | SKILL.md正本(frontmatter付き。`amazing-backup docs llm skill > ~/.claude/skills/amazing-backup/SKILL.md` で配置可能) |
| `llm snippet` | CLAUDE.md/AGENTS.mdへ追記して使うpointer型の数行 |

- 引数なし(`docs`・`docs llm`)はページ一覧をstdoutへ表示して終了0
- `| head`等の先読み終了(BrokenPipe)は正常終了0、その他の出力失敗は処理エラー4(completionsと同一契約)。未知ページ・余計な引数はusage 64

### 5.14 serve — Web frontend+API起動

```
amazing-backup serve [--bootstrap] [--config PATH] [--aws-config PATH] [--profile NAME]
```

Web frontend(埋め込み静的配信)と読取API(`/api/*`・axum)を起動する常駐command。passkey認証(passkey/WebAuthn対応browserが必要)でloginすると閲覧系APIが開放される。

**起動と設定**:

- listen先はserver.toml(3.4・規定path)。server.tomlが無い場合も既定値(`127.0.0.1:40510`)で起動し、stderrへnotice表示をする(初期serve構成はP2で確定)
- 起動時にremote索引の遅延sync(HEAD 1回のvalidator検証・`manage catalog sync`と同一処理)をbackground実行する
- 設定読込失敗は設定エラー2で起動しない。SIGTERM/SIGINTでgraceful shutdown(起動時syncの完了待ちをしてから停止)
- frontend静的配信はrelease buildでbinary埋め込み(rust-embed)。**dist欠落build(cargo install等のcrates.io/git package由来を含む)ではAPIのみで起動し**、warning+frontend build手順を表示する(静的配信なしでもAPI・認証は動作)

**passkey登録(初回・`--bootstrap`)**:

1. `amazing-backup serve --bootstrap` を実行する。stdoutに**1回限りのbootstrap token**(128bit乱数のbase64)が表示される(tokenはin-memory保持・**期限10分**・1回消費・serve再起動で失効。失効・期限切れ時は再度`--bootstrap`で再発行)
2. browserで `http://localhost:40510` を開き、LoginViewのbootstrap token欄へ表示されたtokenを入力してpasskeyを登録する(登録UIはtoken提示時のみ開く。credential 0かつtoken不一致なら登録は閉鎖=fail-closed)
3. 登録済み状態での追加登録もlogin済み+bootstrap tokenが必要。credentialの一覧・失効は `manage auth list` / `manage auth remove`(5.10)

**passkey login**: LoginViewからpasskey認証でloginする(challengeは発行時のsessionと結合・session期限24時間)。login済みのsessionで次の閲覧が開放される:

- entity一覧・詳細・版一覧(`GET /api/entities`・`/api/entities/{uuid}`・`/api/entities/{uuid}/versions`)
- pin一覧(`GET /api/pins`)・location一覧(`GET /api/locations`)
- アップロード前カタログ(`GET /api/pending`)とscan実行(`POST /api/pending/scan`)
- sync状態(`GET /api/sync`)と手動refresh(`POST /api/sync`)
- diff実行(`POST /api/diff`)と最終diff結果参照(`GET /api/diff`)
- backup実行run一覧(`GET /api/runs`・batch単位の集約)
- 取得jobの発行・進行・取消(`POST /api/jobs/get`・`GET /api/jobs`・`GET /api/jobs/{id}`・`POST /api/jobs/{id}/cancel`。下記「取得job(転送タブ)」)
- tag一覧(`GET /api/tags`)・folder一覧(`GET /api/folders`=所在labelの階層集約)・collection一覧と詳細(`GET /api/collections`・`/api/collections/{id}`)
- tag編集(`PUT /api/entities/{uuid}/tags`)とcollectionの作成・改名・削除・構成の追加・除去(`POST`/`PUT`/`DELETE /api/collections`・`POST /api/collections/{id}/items`・`DELETE /api/collections/{id}/items/{uuid}`)
- 名前変更と所在の分類変更(`POST /api/entities/{uuid}/rename`・`POST /api/entities/{uuid}/location`・4.2の語彙。registry.jsonlへのevent追記を真正として索引全体再構築+PUTを行うため、変更後にCLIの`list files`/`query`でも新title・新分類が見える)

これらの閲覧・操作はfrontendの各画面(5タブ構成: ホーム=sync状態・前回backup・未対応・diff・実行中jobのサマリ(実行中jobは一覧実データで追従)、バックアップ=folder構造(tag付けされたlabel階層)/tag/collectionの3ビュー切替+検索box・pager(3ビュー共通)・pending/diff絞り込み表示・行のtag列(chip)・entity詳細=所在・pins・版一覧(版行ごとの「取得」button=転送タブの取得formへprefill)・tag編集・所属collection・名前変更formと所在行ごとの分類変更form(2段階確認部品=「変更」→「確認して実行」で実行)、転送=取得formとjob一覧(下記「取得job(転送タブ)」。upload(push)は今後の版)、設定・管理=今後の版で整備)から操作できる(CLIの`query`/`list`と同一の索引語彙で表示)。実行中jobがある間はnavの転送tabにstatus dotが付く(進行の気づき)。

未認証の閲覧系APIは401で拒否される(空dataを返さない)。backup pull・pin・manage系のfile操作APIは提供しない(操作はCLI既存のまま)。UIから書き込めるのは(1)catalog DBのlocal語彙(4.2: tag・collection)と(2)名前変更・所在の分類変更(registry.jsonlへのrename/location_update event追記を真正とし、索引全体再構築+PUT・metadata配置替え(rename時)を行う=8.1の(a)系に相当するためCLI書込系との同時実行は不可)と(3)取得job・upload(push)jobの発行・取消(下記。server側background実行で取得または暗号化+S3 uploadを行う。push jobは発行時にaudit_logへ発行bodyと実効profileのrecipients指紋一覧も記録する)で、書込APIは成功・失敗双方をaudit_log tableへ記録する(UI書込操作のみが対象でCLI操作は記録しない。履歴のUI表示は今後の版。jobの発行・実行履歴はjobs tableが正本)。

**api-key認証(接続元LAN内限定)**: passkeyが使えない端末・browser向けに、api-keyでの認証をもう一つのlogin手段として提供する(passkey認証に追加する第二手段・接続元がLAN内の場合のみ有効):

1. `amazing-backup manage auth api-key [--name NAME]`(5.10)でapi-keyを生成する。**keyはこの時のstdout表示に1回限り**で現れる(再表示不可・SHA-256 hashのみcatalog DBへ保存のため紛失時は再生成)
2. browserのLoginView(api-key欄は未認証時に常時表示)へkeyを入力して認証する(`POST /api/auth/api-key`)。成功するとpasskey loginと同一のsession(24時間)へ合流し、以降は同じ閲覧APIが開放される
3. **有効条件は接続元address**: 接続元がLAN内(loopback・RFC1918 private・link-local。IPv6はloopback・ULA・link-local。`::ffff:a.b.c.d`のv4-mappedはIPv4へ正規化して判定)の場合のみ受理し、LAN外はkey検証の前に403で受理自体を拒否する。**passkey認証にはこの条件を掛けない**(passkeyは接続元を問わず従来どおり)
4. 失効は `manage auth remove <credential_id>`(5.10・listで確認したapi-keyのcredential_id=`ak-`前置)。失効後の新規loginは同一keyでも401になる。**失効は既存sessionを破棄しない**(in-memory 24hで自然失効・passkey失効と同一挙動)

- LAN内の他端末から使うには、server.toml(3.4)の`listen_addr`を`0.0.0.0`等へ変更してserveを起動する。**LAN IP直結ではWebAuthnのsecure context違反のためpasskeyの登録・loginはできない**(localhostはhttp可)ため、この構成ではapi-keyが主認証手段になる(passkeyの初回登録はlocalhost接続のbrowserで行えば併用可)
- **既知の帰結(cf-tunnel等でのdomain公開時)**: tunnel接続はcloudflaredからlocalhostへの接続として現れるためremote addressがloopback(=LAN内)と判定され、api-key認証がinternet経由でも有効になる。これは「接続元address判定」による有効条件の帰結であり、tunnel運用時の無効化機構は設けない。**tunnelでdomain公開して運用する場合はapi-keyの失効(remove)を推奨する**(passkey認証は影響なし)

**APIの実装確定値**:

- `GET /api/entities`は`query`/`category`/`label`/`tag`/`collection`/`scope`絞り込み+`page`でpaging(tag絞り込み=当該tagを持つentity・collection絞り込み=当該collectionの構成entity。folder絞り込み用のparamは設けず既存`label=`で扱う)。**1page=50件・pageは1始点**(0・非数値は400)。応答は`{"entities":[...],"page":n,"total_pages":n}`(total_pagesは0件でも1)。一覧の各要素と詳細応答にはtag一覧(`tags`・昇順)が付き、詳細応答には所属collection名一覧(`collections`・name昇順)も付く
- tag/folder/collection読取APIの応答: `GET /api/tags`は`{"tags":[{tag,entity_count}]}`(tag名昇順)・`GET /api/folders`はlabelのdirectory区切りで階層導出した`{"folders":[{label,path,entity_count}]}`(中間nodeも1行・path昇順・entity_countは子folder配下の合計)・`GET /api/collections`は`{"collections":[{id,name,entity_count}]}`(name昇順)・`GET /api/collections/{id}`は`{"collection":{id,name,uuids}}`(構成uuid一覧・追加順)
- tag/collection系の書込API: `PUT /api/entities/{uuid}/tags`(body `{"tags":[...]}`でtag一覧を丸ごと置換・冪等・空配列は全削除。tag名規則はcategoryと同一の`^[a-z0-9][a-z0-9-]{0,63}$`で違反は422)・`POST`/`PUT`/`DELETE /api/collections`(作成・改名・削除。同(scope,name)重複は409。削除は構成参照の解除のみでentity・tagへ影響しない)・`POST /api/collections/{id}/items`(body `{"uuids":[...]}`で一括追加・追加済みuuidの再追加は冪等に成功)・`DELETE /api/collections/{id}/items/{uuid}`(除去)。書込応答は操作後の対象状態(読取系と同一schemaのため操作後に再GETは不要)
- 名前変更・所在変更API: `POST /api/entities/{uuid}/rename`(body `{"title"}`。title規則はCLIと同一=64文字以内・`/`・改行・制御文字拒否で違反は422。titleが他entityと重複する場合は許容のうえ応答へ`warning` fieldを付ける=CLIの重複警告と同一文言)・`POST /api/entities/{uuid}/location`(body `{"source_path","category","label"}`。対象所在は(uuid, source_path)完全一致で特定し不一致は404。categoryはcategory名規則・labelはlabel正規化規則(3.1)違反で422・空label=ラベルなしは正当)。対象uuidはローカルregistry由来の現行stateに存在する必要があり(不在は404)。実行はrename/location_update eventをregistry.jsonlへ追記してから索引全体再構築+PUT・catalog DB反映を行い(rename時は加えてmetadataの新titleキーPUT+旧キー掃除)、応答は操作後の対象状態(読取系と同一schema)。所在の分類変更を恒続させるにはpipeline.toml(3.1)の分類規則側の変更が必要(変更後に同fileを再pushすると分類規則の解決結果が記録され直すため=記録順序どおり後勝ち)
- job系・push予測API(TASK-27/28・TASK-17 W4/W5): `POST /api/jobs/get`(body `{"uuid","version_id"?,"pin"?,"out","tier"?,"days"?,"overwrite"?}`。必須field欠落・型違反は400・値域規則違反(tier/days/空uuid/空out)は422。応答は`{"job":{...}}`=status queued)・`POST /api/jobs/push`(body `{"targets":[{"path","title"?}]}`。pathはserver側の絶対path。空targets/空path/相対path/path重複/title規則違反は422。応答は`{"job":{...}}`=kind push・status queued)・`POST /api/push/dry-run`(同body。S3 API・暗号化・registry追記無しの同期予測で`{"connection":{"profile","bucket","prefix","recipients"},"targets":[{"path","name","size","action","uuid","title","warning","merge_target_index"?,"error"?}],"summary":{"count","total_size","skip","re_put","merge","new","failed"}}`を即時応答。actionは`skip`/`re_put`/`merge`/`new`と対象解決失敗の`error`)。job objectは`{"id","kind","status","params","progress","result","error","created_at","updated_at"}`(progressは`{phase,total,done,current_path}`・phaseはgetがqueued→resolve→restore→fetch、pushがqueued→resolve→upload→finalize・waiting_restore中のみnext_check_at付き・resultはget成功時`{name,dest}`、push完了時`{files:[{path,name,action,uuid,status,error?}],summary:{ok,skip,merge,failed}}`・errorは失敗時`{code,message}`)で、`GET /api/jobs`(一覧=進行中優先・各group内はid降順。`?status=`/`?kind=`絞り込みで不正値は400)・`GET /api/jobs/{id}`(詳細)・`POST /api/jobs/{id}/cancel`(queuedのみ受理=status cancelledで応答。実行中・完了済みは409)が同じ形式で応答する。jobの状態の正本はcatalog DBのjobs table(永続class=`manage catalog rebuild`でも保持)
- error応答は`{"error":{"code","message"}}`形式(HTTP statusはerror code種別に対応: 400 invalid_request・401 unauthenticated・404 not_found・409 duplicate・422 validation_failed・502 upstream・503 configuration等)
- serve経由でS3へ書き込むのは名前変更・所在変更(索引`sys/index.jsonl.age`の再構築PUTとmetadata objectの配置替え)とupload(push)job(暗号化+data/manifest/索引/metadata PUT・8.1の(a)系)。tag/collection系書込はcatalog DB(local語彙・4.2)への書込でS3・索引には書かない

**取得job(転送タブ・server側background実行)**: DEEP_ARCHIVE復元待ち(12〜48時間)を伴うfileの取得を、CLIの`get`(5.4)の代わりにserver側のbackground jobとして発行・追従できる:

1. 転送タブの取得formへ対象uuid(詳細画面の版行「取得」buttonからprefill可)・版(省略=最新・version_id/pin指定)・出力dir(server側path・必須)・tier(Standard/Bulk)・days・overwriteを入力して「取得を開始」を押す(`POST /api/jobs/get`)。**応答は即時**(jobはqueuedで発行され、解決・衝突検査はjob実行時に行われる=対象不在などはjobのfailedとして一覧に表示される)
2. job一覧(転送タブ・ホームの実行中job区画。自動更新間隔は設定タブのUI更新挙動・既定10秒)で状態・進行(N/M件・現在path)・結果・errorを確認できる。状態は queued(待機中)→ running(解決・衝突検査)→ waiting_restore(復元job発行済み・定期確認中)→ running(取得・sha照合・復号・配置)→ succeeded/failed と遷移する
3. **DEEP_ARCHIVE・GLACIER系対象は復元待ちをserverが自動追従する**: 復元job発行後もserveは起動し続け、timer(既定15分間隔)がhead-objectのRestore headerを確認し、復元済みになると取得を再開する(CLI `get`の「完了後に再実行してください」運用の置き換え。復元の費用・所要時間自体は7.3の目安のまま)
4. 取得した平文はCLI `get`と同一の検証(取得objectのsha256照合・復号平文のsha256照合)を経て`<out>/<title>`へ配置される。出力先にsha不一致の既存平文がある場合はoverwrite指定なしだと上書きせずjobがfailedになる(7.4の平文衝突保護と同一規則=衝突時はoverwriteを指定して再発行)
5. **server再起動時の追従**: 再起動すると前回起動時の未完了job(queued/running/waiting_restore)は一括して`interrupted`になる(**自動再開はしない**。getは冪等設計のためjob一覧の「再実行」buttonで安全に回復できる。意図しない復元費用の再発行を避けるため手動再実行としている)。待機中(queued)のjobは「取り消す」buttonで取消できる(実行中・完了済みの取消は409)
6. jobの実行は書込APIと同じsingle-flight(semaphore 1)で直列化され・job実行中は書込API(sync・scan・tag編集等)が順番待ちになる。ただし**復元待ち(waiting_restore)の間はsemaphoreを解放する**ため・復元待ちの最大48時間が他の操作を塞ぐことはない
7. **upload(push)job(TASK-28/TASK-17 W5)**: 転送タブのupload form(pending一覧のunbacked行「upload」導線`/transfer?upload=<path>`でpath・title入力済みprefill可)へserver上の対象pathとtitleを列挙し、「予測・接続先を確認」(`POST /api/push/dry-run`)で実効profile・bucket・prefix・recipients指紋一覧とskip/re_put/merge/new予測を確認してから、2段階確認(「uploadを開始」→「確認してupload」)で`POST /api/jobs/push`を発行する。対象rowを変更するとdry-run確認は失効する(再度確認するまで発行できない)。実行はCLI `backup push`と同一の記録形式・順序で対象file単位に進行(`N/M`・現在path)を更新し、server jobでは`keep_going=true`固定で一部対象が失敗しても残りを最後まで処理して失敗対象行とerror codeを一覧表示する(冪等のため修正後の「再実行」で成功済み対象はskipされる)。発行時にはaudit_logへ`action=job_push`としてrecipients指紋一覧も記録する

**pending scanの手順(serve画面)**: pipeline.tomlの`[profiles.<name>.catalog.scan]`(3.1)へ対象rootを登録した上で、pending画面(pending)のscanボタン(`POST /api/pending/scan`)で`manage catalog scan`と同一の走査・分類(unbacked/changed/ok/unknown)を実行できる。

**開発時(dev・Vite dev server)**: frontendの開発確認はVite dev serverを直接見る(release buildの埋め込み配信は使わない):

```sh
pnpm --dir frontend dev    # Vite dev server(port 40512)が/apiをserve(40510)へproxy
```

**外部公開(公開範囲)**: v1はlocalhost(127.0.0.1)listenが既定。LAN IP直結はWebAuthnのsecure context違反のため不可。外部公開はcf-tunnel(Cloudflare Tunnel)等のdomain+https終端を前提とし、server.tomlの`rp_id`/`rp_origin`を当該domainへ切り替える(3.4。localhost用に登録したpasskeyはRP ID違いで別credentialになるため、公開originでも登録し直す)。

### 5.15 終了コード一覧

| code | 区分 | 主な発生条件 |
|---|---|---|
| 0 | 成功 | 対象なし・全skip完結・--from-list空リスト(対象0件)の警告終了・復元job発行の案内(get/pull)も含む |
| 1 | 内部エラー | 予期しない内部エラー(bug) |
| 2 | 設定・環境エラー | toml不在・schema違反・recipient鍵不正・ageバイナリ不在・aws.toml不正・credentialファイル不在/不正・秘密鍵不在・versioning無効bucket・索引不在(9.2) |
| 3 | 対象エラー | 対象ファイル不在・directory指定・拡張子対象外・重複・key長超過・索引上のtitle/uuid/版/pin不在・候補未特定(getの--source-pathなし等)・新規発行時のtitle無し(非tty)・write-once違反・sha不一致の既存平文(7.4) |
| 4 | 検証エラー | 取得object/復号平文のsha256不一致・verifyの検証不一致(検査対象ファイル欠損を含む) |
| 5 | 転送エラー | S3 APIの失敗(認証・network含む)・索引GETの失敗 |
| 64 | 使用法エラー | 引数誤り(不明オプション・backup pushのPATH/--from-list両方0個・pullの範囲指定違反・`--tier`/pin名/日付形式不正等) |

fail-fastが既定(最初のエラーで中止。ただし対象解決段階のエラーは全件列挙してから一度に失敗)。`--keep-going`指定時はper-file失敗を捕捉して継続し、終了コードは最初に発生したエラーの区分。設定エラー2は`--keep-going`でも常に即中止。

## 6. 通常運用の流れ(E2E)

初回の一通りの流れ。任意のdirから実行できる(設定はXDG規定path・2.2から読まれる)。`<PATH>`は対象アーカイブ(拡張子`.zip`/`.7z`)。

1. **設定準備**: `config init`で設定file群を生成(2.2)し、credentialを記述(2.3)・aws.tomlのbucket等を実際の値へ記述(3.2)・pipeline.tomlに鍵pathを記述する。bucketのversioningを有効化(2.4)
2. **予定確認(dry-run)**:

   ```console
   $ amazing-backup backup push --dry-run --from-list targets.toml
   ```

   対象ごとの`skip`/`re-put`/`merge`(合流先uuid)/`new`(新規発行・title必須状態を含む)予測と合計sizeを確認する(旧plan相当。エラーはこの段階で全件出る)

3. **書き込み実行**:

   ```console
   $ amazing-backup backup push --from-list targets.toml
   ```

   per-fileの`ok`/`skip`/`merge`/`re-put`行とrun summary(`run <batch_id>: ok=N skip=N failed=N`)が出る。大容量(数十GiB以上)では時間がかかる(進捗はper-file完了行単位)。中断した場合は再実行で再開(skip判定・crash残存のuuid再利用により二重暗号化・二重uploadは起きない)

4. **ローカル整合検査(任意・S3接続なし。aws.tomlは必須)**:

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

書き込み系command(**backup push・pin add/rm・manage系・serveの名前変更/所在変更API・push job**)の並列実行は禁止する(registry append・索引書換えの競合防止。lock file等の自動直列化は個人運用に対し過剰のため不採用。serve内の書込API・job実行はsync/scanとsingle-flightで直列化されるが、CLI書込系commandとの並走はこの規則で防ぐ)。書き込み系を実行中は、他の書き込み系commandを起動しないこと。照会系(list/query/get/diff)は並列に実行してよい。

### 8.2 profile運用(dev/prod・prod系3bucket)

- dev: 開発検証(STORAGE CLASS STANDARD・復元job待ちがない。DA経路の試験はprodで実施)
- prod系3bucket: 実運用(DEEP_ARCHIVE)。用途別にprofileを使い分ける(3.2):
  - `family` -> `your-family-bucket`
  - `baked` -> `your-baked-bucket`
  - `atara` -> `your-atara-bucket`
- TASK-7 profile化: pipeline.tomlの`[profiles.<name>]`がaws.tomlの各profileと同名対応し、profile毎に独立したrecipient鍵・分類設定を持つ(3.1)。切替は`active_profile`の書換え(恒久)または`--profile <name>`(当該実行限り・**aws/pipeline両fileの選択を兼ねる**)。**誤投入防止のため、prod系への書き込みは`--profile family`等を明示した実行を推奨**(active_profileはdev固定運用)
- **backup pushは実行開始時に接続先(実効profile・bucket・prefix)とrecipient指紋を表示**する(5.6。dry-runでも表示)。表示を確認してから書き込みを続けることで誤bucket・誤鍵組への投入に気づける
- **名前空間の分離はbucket単位で働く(混同防止)**。prod系3bucketは同一prefix(`amazing-backup/`)を共有するが、ローカルregistry(`~/.local/state/amazing-backup/registry.jsonl`)は単一でも記録の参照(skip判定・索引versions/pins構築・未書込namespace判定・pin解決)はすべて**bucket一致で分離**される。他bucketの書込実績では誤ってskipしない・索引へ他bucketの版やpinが混入しない・未書込救済判定に他bucket実績が採用されない、が構造的に保証される(bucket横断で1byteもuploadされないrunは作れない)。`manage reset`(5.10)も同様にregistry操作を当該bucket由来行に限定し、prod系bucketの記録は温存される(prod投入とdev掃除の並行運用が可能。2026-10-26 TASK-25)
- **運用規則: 同一(bucket, prefix)を別recipient構成(別pipeline profile)で書かないこと**。索引object keyはbucket+prefixで決まり、索引は毎push時に実行時のrecipientsで全体再暗号化されて同keyへ上書きされるため、別recipient構成が交互に書くと他方の秘密鍵で索引が読めなくなる。1bucket(=1namespace)に対するrecipient構成は1つのprofileで管理する。aws.toml内で複数profileが同一bucketを参照する場合は起動時に警告する(検出できるのは同一aws.toml内のbucket重複のみ。同一profileでの鍵差し替え・別aws設定fileからの書込みは検出できないため、最終検知はpush開始時のrecipients指紋表示による)

**dev検証用の環境分離(AMAZING_BACKUP_HOME)**

dev実機検証(lifecycle apply・push・reset等)は`AMAZING_BACKUP_HOME`(ABH)で設定・registryを実運用から物理分離して実施できる(設定path解決が`$ABH/{pipeline.toml,aws.toml,registry.jsonl,cache/}`へ一括切り替わるため。repo付属の`scripts/amazing-backup-dev` wrapperが`AMAZING_BACKUP_HOME`未指定時に`~/.amazing-backup-dev`へ分離して起動する):

```sh
# 初期setup(一度): ~/.amazing-backup-dev へdev profile(purpose=dev)の
# pipeline.toml・aws.tomlを配置(credentials_fileが絶対path参照ならcredentialは共用可)
amazing-backup-dev backup push /path/to/archive.zip   # 以降のdev実機検証はすべてwrapper経由
```

dev検証のregistryは`$ABH/registry.jsonl`へ分離されprod実運用registryに触れない。reset(5.10)自体も混在下で安全だが、ABH分離によりdev検証の事故面を更に遮断でき、dev bucket掃除もwrapper経由の`manage reset --apply`で完結する。

### 8.3 recipient鍵の管理

初回の鍵導入(雛形のdummy path置換と`[profiles.<name>.verify].decrypt_key`の記述)は2.2の手順で行う。以下は運用中への鍵の追加・管理の手順である。鍵の追加・確認は**対象profileの`[profiles.<name>]`に対して**行う(profile毎に鍵pairが独立・3.1)。

1. 新規ed25519鍵pairを生成する(passphraseなし。配置先は任意の絶対path。例では`~/.config/amazing-backup/keys/`):

   ```console
   $ mkdir -p ~/.config/amazing-backup/keys
   $ ssh-keygen -t ed25519 -f ~/.config/amazing-backup/keys/backup_ed25519_2 -N ""
   ```

2. pipeline.toml(規定path)の対象profile `[profiles.<name>.recipients].public_keys` へ公開鍵(.pub)のpathを追加する
3. `amazing-backup backup push --dry-run --profile <対象profile>` を実行して設定エラーが出ないことと、表示される`recipients(N):`の指紋列が意図した鍵組であることを確認する(鍵ファイルの不在・形式違い・指紋の重複は設定読込時の設定エラー2で検出される。指紋はload時検証で計算済みのためdry-run(暗号化しない)でも全文表示される)

追加以降の書き込みは新recipient構成で暗号化される(索引・manifestも同様)。

- **遡及**: S3へ投入済みのobjectは暗号化時のrecipient構成のまま変わらない。全量を新構成にしたい場合は新たなrunで再pushする(旧objectの削除は`manage prune`や手運用)
- **旧鍵の温存**: 鍵を追加しても旧秘密鍵を廃棄すると、旧recipient構成で暗号化されたobjectを復元できなくなる。追加後も旧鍵は保管し続けること。独立鍵導入後は全profileを復旧するのに各profileが必要とする鍵の揃い(現行鍵+旧鍵+第二保管鍵)が必要になる(9.1)
- **第2recipientの運用要件**: 「S3と秘密鍵のみ」の災害前提が成立するには秘密鍵が単一machineにしか無い状態を解消する必要がある。第2recipient(秘密鍵の地理的第二保管場所)の登録・保管を運用要件とする(**profile単位で満たす**: 各profileのpublic_keysに第二保管場所の公開鍵を列挙する)

## 9. 災害時の手順

### 9.1 前提(作成環境の資産は不要)

照会・復元(list/query/get/diff/backup pull)は**aws.toml(bucket/region/prefix/credentials_file)とAPI keyファイルといずれか1本の秘密鍵のみ**で動作する。最初に`sys/index.jsonl.age`を取得・復号し、以降の操作は索引+S3 APIで完結する(ローカルregistry・cache dir・pipeline.tomlは不要)。pipeline.tomlが規定pathに無い場合は実効profileの`[profiles.<name>.verify].decrypt_key`が読めないため、**list/query/get/diff・backup pullのすべてで`--key <秘密鍵path>`が必須**になる。

- 災害時の新環境では`--profile <name> --key <path>`の組でaws/pipeline両方の選択が一貫する(`--profile`が両fileを選択するため、片方だけ間違える経路が存在しない・8.3)
- **「秘密鍵1本」は条件付き**: 復元対象の世代が旧recipient構成で暗号化されていれば旧秘密鍵も必要(8.3の旧鍵温存)。独立鍵導入後は**全profileを復旧するには各profileが必要とする鍵の揃い(現行鍵+旧鍵+第二保管鍵)が必要**になる(対象profileの現行recipient構成の鍵だけでは旧世代・他profileの復元はできない)
- backup push・pin・manageの一部・backup verify・editはpipeline.toml必須の操作(5.1)のため災害時前提の対象外(現行と同じ位置づけ)

新環境での必要資産: amazing-backup binary(README.md Install節のinstall手順)・age binary v1.2系(2.1。索引・実体の復号に使用)・aws.toml(規定path`~/.config/amazing-backup/aws.toml`へ配置・`--aws-config`での明示指定も可)・credentialファイル・秘密鍵。repoのcloneは不要。

**新環境で`config init`を使う場合の注意**: `config init`はpipeline.tomlも同時に生成し、生成直後のpipeline.tomlは存在しないpathの公開鍵(dummy)をprofile構成の中に含む。照会系は規定pathのpipeline.tomlを「在れば読む」ため、dummyのままでは`--key`を指定しても設定エラー2になる。次のいずれかを行うこと:

- pipeline.tomlの対象profile(`[profiles.<name>.recipients]`。`<name>`は復元対象のaws profile名と同名)の`public_keys`を退避済み公開鍵のpathへ置換する(あわせて`[profiles.<name>.verify].decrypt_key`へ秘密鍵pathを記述すれば`--key`省略可)
- pipeline.tomlを規定pathから退避(移動・改名)する(照会系は規定pathに無ければ読まずに動作する)

### 9.2 索引が読めない場合

照会系で索引objectの取得に失敗した(不在・復号失敗)場合は設定・環境エラー2となり、専用案内が表示される。例外として、ローカルregistryに当該prefixへの書込記録が1件も無い「未書込namespace」(dev/prod切り替え直後等)は0件+終了0で扱われる。

索引を復旧する:

```console
$ amazing-backup manage index rebuild   # pipeline.tomlは規定path(2.2)
```

`sys/manifests/`のmanifest(DEEP_ARCHIVE)から索引を再構築する。未復元manifestへはBulk復元jobを発行する(発行したら案内+終了0。**最大48時間の待ちの後に再実行**で継続)。開始時に費用の目安(7.3)が表示される。完了すると索引がPUTされ、以降の照会が復旧する(完了時に続けての`manage index reconcile`実行が案内される・5.10)。旧schemaのmanifest file record(schema≠3・label無し)を検出した場合は設定エラー2で失敗する(分類labelを欠いた索引を作らず、初期化と再pushで作り直す)。

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
- 旧配置object(旧形式`<date>/<name>.age`・旧`<category>/`階層)は新構成へ変換せず`manage reset --apply`(5.10・registry退避付きの全消去)で削除する。your-dev-bucketは引き続きrepo開発環境のdev profile bucketとして使用する(repo開発者向けの手順は `docs/dev/dev_backup.md`)
