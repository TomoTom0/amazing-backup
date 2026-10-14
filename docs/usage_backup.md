# backup-remote 使用手順書

NASバックアップアーカイブ(zip/7z)をageで暗号化し、S3 Deep Archiveへ投入するCLI `backup-remote` の使用手順書。

- 対象読者: 実際にバックアップを暗号化・uploadする運用者
- 正本参照: 設計の正本は `tmp/tasks/tm-1/design_backup_pipeline.md`(仕様詳細・背景はこちら)。AWS SDK移行(TASK-9)の設計は `tmp/tasks/tm-9/design_sdk_migration.md`。本書は運用に必要な手順・引数・設定の実務参照
- 実装バージョン: 0.1.0
- 最終確認: 2026-10-14(実機の出力・設定ファイル実物に基づく)

## 1. 概要

パイプラインの流れ:

```
plan(対象解決・事前検査)
  -> encrypt(age暗号化 + ローカルregistry/manifest記録)
  -> verify(ローカル整合検証・必要に応じ復号roundtrip)
  -> upload(S3 Deep Archiveへの転送)
```

各段階の成果物:

| 場所 | 内容 |
|---|---|
| `data/out/<YYYY-MM-DD>/<name>.age` | 暗号化済みアーカイブ(age v1形式) |
| `data/out/<YYYY-MM-DD>/manifest.jsonl.age` | バッチ目録(manifest)を同じrecipientで暗号化したもの |
| `data/registry.jsonl` | ローカル累積registry(append-onlyのJSONLイベントログ) |
| `s3://<bucket>/<YYYY-MM-DD>/...` | upload先。キーは `<date>/<name>.age`・`<date>/manifest.jsonl.age` |

暗号化はSSH ed25519公開鍵の1-of-N(全鍵で暗号化し、いずれか1鍵で復号可能)。S3接続は**AWS SDK(aws-sdk-s3)をbinary内蔵**しておりaws CLIは不要。credential(access key)は`configs/secret/`配下の明示的なAPI keyファイル(2.3)から与え、`~/.aws`や環境変数のcredential chainには依存しない(fail-closed)。plan/encrypt/verifyはS3接続なしで完結する。

restore(復元)は実装済み。`restore request`(復元job発行)-> `restore status`(状態確認)-> `restore download`(取得・検証・復号)の3サブコマンドで復元する。手順の正本は `docs/restore_backup.md`。backup-remoteを使わない手動復元の手順は9章。

## 2. セットアップ

### 2.1 前提ツール

| ツール | 要件 | 確認 |
|---|---|---|
| age | v1.2系(検証: 1.2.1・`/usr/bin/age`) | `age --version` |
| Rust toolchain | ビルドに必要(cargo) | `cargo --version` |

ageはv1.2形式のstanza識別子(指紋prefix)によるヘッダ照合の前提となるため、v1.2系を使用する。

**aws CLIは不要**: S3への接続はAWS SDK(aws-sdk-s3・rustls版)をbackup-remoteが内蔵しており、aws CLIの導入・設定は必要ない。aws CLIがあるとS3側の確認(5.7)や手動復元(9章)に使えるが、あくまで任意(2.2)。

### 2.2 AWS SDK内蔵について(aws CLI不要・参考: 導入手順)

S3アクセス(HeadObject・PutObject・ListObjectsV2・RestoreObject・GetObject)はすべてSDK経由でbinary内蔵のため、aws CLIは必須要件から外れた。旧版で案内していたper-user install手順は、S3側の確認用手順(5.7・8章)や手動復元(9章)でaws CLIを使いたい場合の参考として残す:

```console
$ curl "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o awscliv2.zip
$ unzip awscliv2.zip
$ ./aws/install -i ~/.local/share/aws-cli/v2 -b ~/.local/bin --update
$ aws --version
aws-cli/2.37.10 ... (v2.xであればよい)
```

- インストール先: 本体 `~/.local/share/aws-cli/v2/<version>/`、コマンドsymlink `~/.local/bin/aws`
- `~/.local/bin` にPATHが通っていること
- `--update`は既存導入のある環境での上書き更新用。初回導入でも問題なく動作する

### 2.3 AWS credential(API keyファイル)の作成

credential(access key)は**明示的なAPI keyファイル** `configs/secret/aws-credentials.toml`(gitignore済み領域)へ置く。SDKへはこのファイルの値を明示渡しし、`~/.aws`・環境変数・IMDS等のcredential chainへのfallbackは**行わない**(ファイルが無い・不正なら設定エラー2で即終了するfail-closed構成。誤った別環境credentialでの接続を構造的に防ぐ)。

access keyの入手先はAWS IAM(IAMユーザーのaccess key作成。S3への書き込み権限を持つもの。restore運用も見据えるなら読み取り・復元権限も付与)。

ファイルを作成する(schemaは`[credentials]`テーブル配下に`access_key_id`/`secret_access_key`必須・`session_token`省略可。未知キーは設定エラー2):

```console
$ vim configs/secret/aws-credentials.toml
```

```toml
# configs/secret/aws-credentials.toml(gitignore済み・実運用で作成する秘密ファイル)
[credentials]
access_key_id = "<access key id>"       # 必須
secret_access_key = "<secret access key>" # 必須
# session_token = "<session token>"     # 任意(一時credentialを使う場合のみ)
```

- **作成後`chmod 600`を推奨**(秘密情報を含むファイルのため):

  ```console
  $ chmod 600 configs/secret/aws-credentials.toml
  ```

- このファイルのpathを `configs/aws.toml` の `[aws].credentials_file` へ記述する(3.2)。必須キーのため、書き漏れはupload/restore実行時の設定エラー2で即検出される
- 空値・前後空白付き値・未知キーは設定エラー2(値そのものはerror文に表示されない)
- 値を直接aws.tomlへ書くことはできない(`credentials_file`を除くcredential系キーは未知キーとして拒否)

### 2.4 ビルドと実行形式

```console
$ cargo build --release          # target/release/backup-remote
$ cargo install --path .         # PATHへ導入する場合(cargo bin)
```

実行形式は `backup-remote <subcommand> [options]`。`cargo run --quiet -- <subcommand>` でも同一挙動。設定・出力の相対pathはrepo root(cwd)基準で解決されるため、**repo rootで実行する**のが前提。

### 2.5 S3 bucket側のlifecycle設定(推奨)

64MiBを超えるオブジェクトはmultipart uploadで転送する(4.4)が、転送途中の失敗では`AbortMultipartUpload`をbest-effortで発行する(失敗しても警告表示のみで継続する)。このabortに失敗した場合、bucketへ不完全なmultipart uploadが残り、**完了していない部分もDEEP_ARCHIVE相当の課金対象になり得る**。この放置をbucket側で掃除するため、upload先bucketへlifecycle ruleの**AbortIncompleteMultipartUpload(7日経過で不完全MPUを削除)**設定を推奨する(aws CLIでの設定例。任意):

```console
$ aws s3api put-bucket-lifecycle-configuration --bucket <bucket> --lifecycle-configuration '{
  "Rules": [
    {"ID": "abort-incomplete-multipart-upload", "Status": "Enabled",
     "Filter": {}, "AbortIncompleteMultipartUpload": {"DaysAfterInitiation": 7}}
  ]
}'
```

(upload対象のオブジェクト自体へのlifecycle移行(STANDARDからの自動移行)は設定しないこと)

## 3. 設定ファイル

### 3.1 `configs/pipeline.toml`

暗号化・出力・検証の設定(秘密を含まない)。現行の実物:

```toml
[recipients]
# SSH ed25519公開鍵のpath列挙(1-of-N。全鍵で暗号化し、いずれか1鍵で復号可能)
# 相対pathはrepo root(cwd)基準。1つ以上必須。ed25519形式のみ許容
public_keys = [
  "configs/secret/keys/private/backup_ed25519.pub",
]

[output]
# 暗号化成果物の集約dir。配下に <YYYY-MM-DD>/<name>.age ができる
dir = "data/out"
# ローカル累積registry(JSONL)
registry = "data/registry.jsonl"

[behavior]
# per-file失敗時も継続する既定値(CLIの--keep-goingで上書きon)
keep_going = false

[verify]
# --roundtrip検証で使う秘密鍵path(秘密そのものではなくpath参照。configs/secret/はgitignore済み)
decrypt_key = "configs/secret/keys/private/backup_ed25519"
```

各キーの説明:

| キー | 意味 |
|---|---|
| `[recipients].public_keys` | recipient公開鍵(SSH ed25519)のpath列挙。1-of-N。全鍵で暗号化し、いずれか1鍵で復号できる。鍵の追加手順は7章 |
| `[output].dir` | 暗号化成果物の集約dir。配下に `<YYYY-MM-DD>/<name>.age` ができる |
| `[output].registry` | ローカルregistry(JSONL・append-only)のpath |
| `[behavior].keep_going` | per-file失敗時も継続する既定値。CLIの`--keep-going`でonに上書き |
| `[verify].decrypt_key` | `verify --roundtrip`で使う秘密鍵のpath参照(値そのものではなくpath) |

検証規則(違反はすべて設定エラー・終了コード2):

- ファイル不在・TOML parseエラー・未知キー・型不一致
- `public_keys`空配列・鍵ファイル不在・`ssh-ed25519`以外の形式(ssh-rsa等)
- `public_keys`内の公開鍵指紋(`SHA256:...`)の重複(同一path重複・異pathで同一鍵blobの両方を検出)
- `[output]`の相対pathはrepo root(cwd)基準

### 3.2 `configs/aws.toml`

S3接続設定(bucket/region/credentialファイル参照/storage class。credential値そのものは不含)。**upload系・restore系でのみ必須**で、plan/encrypt/verifyはこのファイルがなくても動作する。現行の実物(template):

```toml
[aws]
# いずれもupload系でのみ必須(plan/encrypt/verifyはこのファイルがなくても動作)
bucket = ""                    # S3 bucket名(空文字・未設定はupload時設定エラー2)
region = ""                    # region名(同上)
credentials_file = "configs/secret/aws-credentials.toml" # credential(API key)ファイルのpath。必須(2.3)
storage_class = "DEEP_ARCHIVE" # 既定DEEP_ARCHIVE
```

| キー | 意味 |
|---|---|
| `[aws].bucket` | 正本のupload先S3 bucket名。空文字・未設定はupload時の設定エラー(終了コード2) |
| `[aws].region` | region名。空文字・未設定はupload時の設定エラー |
| `[aws].credentials_file` | credentialファイル(2.3で作成したAPI keyファイル)のpath。**必須**(cwd基準の相対path)。欠落・空・指すファイルの不在・不正は設定エラー2 |
| `[aws].storage_class` | uploadのstorage class。空文字なら`DEEP_ARCHIVE` |

`profile`キーは廃止された(credentialは2.3のAPI keyファイル経由に一本化)。旧aws.tomlに`profile`が残っていると未知キーの設定エラー2になるため、`credentials_file`への書き替えが必要。

検証規則(upload/restore開始前の事前検査・違反は設定エラー2):

- `storage_class`はallowlist検証。許容値: `STANDARD` / `STANDARD_IA` / `ONEZONE_IA` / `INTELLIGENT_TIERING` / `GLACIER` / `DEEP_ARCHIVE` / `GLACIER_IR`。allowlist外(打ち間違い等)は設定エラー
- `bucket`/`region`/`credentials_file`の前後空白付き値(例: `" my-bucket "`)は設定エラー(値のtrimは行わず打ち間違いとして弾く)
- `bucket`/`region`が空の項目は、uploadのCLIオプション(`--bucket`/`--region`)指定の有無にかかわらず設定エラー。CLI指定値はaws.toml値が正である前提の一時上書きであり、空値を補完する手段にはならない(4.4)
- credential値の直書きはできない(`credentials_file`を除くcredential系キーは未知キーエラー)。credentialは`credentials_file`が指す外部ファイル(2.3)のみ

**エンドポイント確定性の注記**: SDKは環境変数`AWS_ENDPOINT_URL`/`AWS_ENDPOINT_URL_S3`を解釈して接続先endpointを切替え得る。backup-remoteは起動時にこれらのenvが設定されていれば警告をstderrへ表示する(処理は継続)。意図した検証用構成か誤設定かは利用者側で確認すること。

### 3.3 対象リストtoml(`--from-list`)

対象をファイルで指定する場合は次のschemaのtomlを作る(実運用のリストはgit管理外で都度作成):

```toml
[[targets]]
path = "/mnt/nas/backup/photo.zip"

[[targets]]
path = "/mnt/nas/backup/2025/docs.7z"
name = "docs-2025.7z"   # 任意: 出力名・S3キー名のoverride(basename衝突回避用)
```

- `path`(必須): 対象アーカイブのpath
- `name`(任意): 出力名のoverride。basename形式のみ(`/`を含む・`.`・`..`は対象エラー)
- 位置引数との併用可(和集合)。未知キー・`path`欠損は設定エラー

## 4. サブコマンドリファレンス

### 4.1 plan — 対象解決と事前検査

暗号化・uploadを行わない事前検査。設定(recipient公開鍵)の検証、対象の存在・拡張子(`.zip`/`.7z`のみ・大文字小文字無視)・重複・出力名一意性を確認し、対象一覧・サイズ・recipient指紋・出力先を表示する。aws不要。

```
backup-remote plan [--config PATH] [--from-list FILE] <PATH> [<PATH> ...]
```

| 引数/オプション | 既定 | 意味 |
|---|---|---|
| `<PATH>`(1個以上必須) | - | 対象アーカイブ |
| `--config` | `configs/pipeline.toml` | pipeline設定 |
| `--from-list` | なし | 対象リストtoml(3.3)。位置引数と併用可 |

出力例(実機確認済み・パスは例):

```
== plan ==
   1. /mnt/nas/backup/photo.zip       12.5 GiB  mtime 2026-09-30 22:11 JST
   2. /mnt/nas/backup/docs.7z          3.2 GiB  mtime 2026-10-01 09:00 JST
targets: 2 files, 15.7 GiB total
recipients (1-of-N, N=1):
  - SHA256:oMLg8HW9EvSPC+FAEkD1fDCPQP7b/4UhLSc/Txvw9uI   configs/secret/keys/private/backup_ed25519.pub
output: data/out/2026-10-11/{photo.zip.age, docs.7z.age, manifest.jsonl.age}
s3 prefix: 2026-10-11/
```

終了コード: 成功0 / 設定・環境エラー2 / 対象エラー3。

### 4.2 encrypt — 暗号化とregistry・manifest記録

各アーカイブをageで暗号化し、registryへ記録、バッチmanifestを暗号化生成する。aws不要。

```
backup-remote encrypt [--config PATH] [--from-list FILE] [--out DIR] [--registry PATH] [--keep-going] <PATH> [<PATH> ...]
```

| 引数/オプション | 既定 | 意味 |
|---|---|---|
| `<PATH>`(1個以上必須) | - | 対象アーカイブ |
| `--config` | `configs/pipeline.toml` | pipeline設定 |
| `--from-list` | なし | 対象リストtoml |
| `--out` | 設定`[output].dir` | 成果物dir(テスト隔離用) |
| `--registry` | 設定`[output].registry` | registry path(テスト隔離用) |
| `--keep-going` | なし(設定`[behavior].keep_going`がtrueなら既定on) | per-file失敗でも継続 |

動作の要点:

- skip判定: 出力`.age`が存在し、registryに同一source(path・size・mtime一致)のencrypt記録がある場合は再読込せずskip
- `.age`が存在してもregistry記録がない(書込途中crash等)場合は上書き再暗号化。sourceのsize/mtimeが変化していても再暗号化
- 書込は`.age.part`へ書いて完了後に原子rename。crash途中は`.part`が残るだけで既存`.age`は壊れない
- 暗号化直後に`.age`ヘッダをparseし、recipient鍵の照合(暗号化対象を間違えたまま大量処理する事故の検出)
- 全対象処理後、バッチmanifest(JSONL)を現在のrecipient構成で暗号化して `<out>/<date>/manifest.jsonl.age` へ

出力例(実機確認済み):

```
ok    sample.zip  1.0 MiB -> 1.0 MiB  sha256:182eacd3e289b088
-- summary: 1 encrypted, 0 skipped, 0 failed / out: data/out/2026-10-11 / manifest: data/out/2026-10-11/manifest.jsonl.age
```

再実行時(既に暗号化済みならskip):

```
skip  sample.zip (already encrypted: registry match)
-- summary: 0 encrypted, 1 skipped, 0 failed / out: data/out/2026-10-11
```

終了コード: 成功0 / 設定・環境エラー2(ageバイナリ不在・recipient鍵不正を含む)/ 対象エラー3 / 暗号化処理エラー4。

### 4.3 verify — ローカル整合検証

registryとローカル`.age`の整合を検証する。aws不要。指定した最深の段階まで順に実施する。

```
backup-remote verify [--config PATH] [--date YYYY-MM-DD] [--checksum] [--roundtrip] [--key PATH]
```

| 引数/オプション | 既定 | 意味 |
|---|---|---|
| `--config` | `configs/pipeline.toml` | pipeline設定 |
| `--date` | なし(全件) | 当該batch_date分に限定。`YYYY-MM-DD`形式(形式不正はusage error 64) |
| `--checksum` | なし | `.age`のsha256を再計算してregistry記録と照合(全読込発生) |
| `--roundtrip` | なし | 秘密鍵で復号し平文sha256をregistry記録と照合(平文は書き出さない)。manifest復号+registry突合も行う |
| `--key` | 設定`[verify].decrypt_key` | 復号用秘密鍵(`--roundtrip`時必須) |

検証段階:

1. 構造検査(既定): registry全行のparse・schema検証。`.age`実ファイルのsizeが記録`age_size`と完全一致すること。`.age`ヘッダのrecipient照合(stanza識別子とregistry記録の指紋の全単射対応)。各バッチのmanifestファイルの存在
2. `--checksum`: 各`.age`のsha256再計算と`age_sha256`照合
3. `--roundtrip`: 復号して平文sha256が`source_sha256`と一致すること。manifest復号+registry突合

出力例(実機確認済み):

```
== verify == ok (1) records, checksum=false, roundtrip=true
```

不一致の詳細(該当recordのsource_pathと期待/実測)は stderr に出る。終了コード: 成功0 / 設定・環境エラー2(秘密鍵不在・age不在を含む)/ 検証エラー4(検査対象ファイル欠損を含む全種類の不一致)。

### 4.4 upload — S3 Deep Archiveへの転送

registryから未uploadの`.age`とmanifestを列挙してS3へ転送し、各バッチプレフィックスへ平文README(6章)を配置する。S3接続はSDK内蔵(2.2・aws CLI不要)。credentialは`[aws].credentials_file`が指すAPI keyファイル(2.3)を使用する。

```
backup-remote upload [--config PATH] [--aws-config PATH] [--date YYYY-MM-DD]
                     [--dry-run] [--keep-going]
                     [--storage-class CLASS] [--bucket NAME] [--region REGION]
```

| 引数/オプション | 既定 | 意味 |
|---|---|---|
| `--config` | `configs/pipeline.toml` | pipeline設定 |
| `--aws-config` | `configs/aws.toml` | aws設定 |
| `--date` | なし(全未upload分) | 当該batch_date分に限定。`YYYY-MM-DD`形式(形式不正はusage error 64) |
| `--dry-run` | なし | S3 API(PutObject・HeadObject)を実行せず、実行予定とskip想定を表示 |
| `--keep-going` | 設定`[behavior].keep_going` | per-object失敗でも継続 |
| `--storage-class` | 設定`[aws].storage_class` | storage classの当該実行限りの上書き |
| `--bucket` | 設定`[aws].bucket` | S3 bucketの当該実行限りの上書き |
| `--region` | 設定`[aws].region` | regionの当該実行限りの上書き |

**CLI一時上書きオプションと優先順位**: `--storage-class` / `--bucket` / `--region` は、当該実行のみ対応するaws.toml値を上書きする一時オプション。**優先順位は「CLI指定値 > aws.toml値」であり、aws.tomlが正本・CLI指定はその実行限りの例外**。aws.tomlへの書戻しは一切行わず、未指定の項目はaws.toml値のまま。CLI上書き値はtoml値と同一基準で検証される:

- `--storage-class`: allowlist外なら設定エラー2。空文字・空白のみは未指定扱い(aws.toml値を採用)
- `--bucket` / `--region`: 空文字・空白のみ・前後空白付きは設定エラー2

なおCLI上書き値はtoml値が正である前提の上書きであり、tomlの`bucket`/`region`が空の項目をCLI値で補完する使い方はできない(toml空値はCLI指定の有無にかかわらず設定エラー)。

動作の要点:

1. 事前検査: aws.tomlの正検査(bucket/region空・前後空白付き・storage_class allowlist外は設定エラー2で即終了。S3 APIは実行されない)と、credentialファイル(`credentials_file`が指す2.3のAPI keyファイル)の読込・検査
2. registryからupload対象を列挙: 「`s3_bucket`が当該実行の実効bucket(CLI一時上書き適用後の値)と一致する成功記録が無いもの」。記録bucketが不一致の対象は再upload対象になる。**前回成功記録で除外された対象もHeadObjectでS3側の実在を確認し、削除済み(404)なら再upload対象へ戻す**(S3側で削除された対象の再送担保。`--dry-run`ではこの確認を行わない)
3. per-object skip判定: HeadObject(S3 API)でS3側のobject有無とSizeを確認。404ならupload、Size一致ならskip(「sizeのみ検証」警告付き)、Size不一致なら再upload
4. upload: PutObject(SHA256 checksum指定付き。**64MiB超のオブジェクトは64MiB partのmultipart upload**、失敗時はAbortMultipartUploadをbest-effort発行)。成功後HeadObjectでSize/ChecksumSHA256を取得しregistryへ記録
5. 全ファイル後にmanifestを同じ手順でupload(キー `<date>/manifest.jsonl.age`)
6. (削除: README自動配置機能は廃止)

出力例(`--dry-run`・実機確認済み。pathは例):

```
[dry-run] s3 PutObject bucket=example-backup-bucket key=2026-10-11/sample.zip.age src=data/out/2026-10-11/sample.zip.age storage_class=DEEP_ARCHIVE checksum=SHA256
[dry-run] s3 PutObject bucket=example-backup-bucket key=2026-10-11/manifest.jsonl.age src=data/out/2026-10-11/manifest.jsonl.age storage_class=DEEP_ARCHIVE checksum=SHA256

-- summary (dry-run): 2 objects + README配置予定1件 (HeadObject未実行のためskip想定は未確定・全件upload想定)
```

64MiBを超える対象の行にはmultipart予定の注記(例: ` [multipart: 64MiB x 200]`)が付く。

終了コード: 成功0(upload対象がなくても0)/ 設定・環境エラー2 / 対象エラー3(registryが指す`.age`が不在)/ 転送エラー5(S3 APIの失敗。認証・network含む)。

### 4.5 終了コード一覧

| code | 区分 | 主な発生条件 |
|---|---|---|
| 0 | 成功 | 対象なし・全skip完結も含む |
| 1 | 内部エラー | 予期しない内部エラー(bug) |
| 2 | 設定・環境エラー | toml不在・schema違反・recipient公開鍵不在/形式違反・ageバイナリ不在・aws.toml未設定・credentialファイル不在/不正(`credentials_file`指す先・値の検査失敗) |
| 3 | 対象エラー | 対象ファイル不在・directory指定・拡張子対象外・realpath重複・name衝突・registryが指す`.age`不在(upload時) |
| 4 | 処理エラー | age暗号化/復号の異常終了・ヘッダ照合不一致・verifyの検証不一致(検査対象ファイル欠損を含む) |
| 5 | 転送エラー | S3 API(PutObject・HeadObject等SDK呼び出し)の失敗(認証・network含む) |
| 64 | 使用法エラー | 引数誤り(不明オプション・PATH 0個・`--date`形式不正等) |

fail-fastが既定(最初のエラーで中止。ただし対象解決段階のエラーは全件列挙してから一度に失敗)。`--keep-going`指定時はper-file/per-object失敗を捕捉して継続し、終了コードは最初に発生したエラーの区分。設定エラー2は`--keep-going`でも常に即中止。

## 5. E2E実行手順

初回の一通りの流れ。repo rootで実行する。`<PATH>`は対象アーカイブ(拡張子`.zip`/`.7z`)。

### 5.1 対象指定

位置引数で直接指定するか、対象リストtoml(3.3)を作って`--from-list`で指定する。数が多い場合はリストtomlを推奨:

```console
$ vim targets.toml     # [[targets]] path = "..." を列挙(3.3のschema)
```

### 5.2 plan — 事前検査

```console
$ backup-remote plan --from-list targets.toml
```

対象一覧・合計サイズ・recipient指紋・出力先(s3 prefix)を確認する。エラー(対象不在・拡張子違い・重複等)はこの段階で全件出る(終了コード3)。

### 5.3 encrypt — 暗号化

```console
$ backup-remote encrypt --from-list targets.toml
```

per-fileの`ok`/`skip`行とsummaryが出る。大容量(数十GiB以上)では時間がかかる(進捗はper-file完了行単位)。中断した場合は再実行で再開(skip判定・`.part`の仕様により二重暗号化や壊れた`.age`の混入は起きない)。

### 5.4 verify — 復号roundtrip検証

```console
$ backup-remote verify --roundtrip
```

秘密鍵(設定`[verify].decrypt_key`)で実際に復号し、平文sha256が記録と一致すること・manifest突合を確認する。upload前に必ず一度実行する。日付を限定する場合は `--date 2026-10-11`。

### 5.5 upload — 予定確認(dry-run)

```console
$ backup-remote upload --dry-run
```

実行予定のS3 API列(4.4の出力例。S3キー・storage class・bucket・multipart要否)を確認する。S3 APIは実行されない(HeadObjectを行わないためskip想定は未確定・全件upload想定で表示。前回成功記録のある対象のS3側実在確認も行われない)。aws.tomlのbucket/region/credentials_fileが未設定だとこの段階で設定エラー2になる(2.3・3.2を設定してから)。

### 5.6 upload — 実行

```console
$ backup-remote upload
```

per-objectのupload・skipとsummaryが表示される。失敗(転送エラー5)した場合は再実行で再開(HeadObjectのSize一致skipにより、upload済み分は再転送されない)。

### 5.7 S3側確認(任意・aws CLIによる確認手順)

upload結果の確認はbackup-remote自体の機能ではないため、aws CLI等の外部ツールで行う(aws CLIは必須ではない。2.2の参考手順で導入できる。S3consoleや任意のS3 clientでもよい):

```console
$ aws s3 ls s3://<bucket>/2026-10-11/
2026-10-11 ... manifest.jsonl.age
2026-10-11 ... sample.zip.age

$ aws s3api head-object --bucket <bucket> --key 2026-10-11/sample.zip.age
```

head-objectで`Size`(ローカル`.age`のサイズと一致)・`StorageClass`(DEEP_ARCHIVE)・`ChecksumSHA256`を確認する。

## 6. RESTORE_README.txt(廃止: 削除済み)

## 7. 鍵の管理

### 7.1 recipient鍵の追加手順

1. 新規ed25519鍵pairを生成する(passphraseなし):

   ```console
   $ ssh-keygen -t ed25519 -f configs/secret/keys/private/backup_ed25519_2 -N ""
   ```

   秘密鍵・公開鍵とも`configs/secret/`配下に置く。秘密鍵はバックアップとは別の場所へ保管する(このdirはgitignore済み)。

2. `configs/pipeline.toml` の `[recipients].public_keys` へ公開鍵(.pub)を追加:

   ```toml
   public_keys = [
     "configs/secret/keys/private/backup_ed25519.pub",
     "configs/secret/keys/private/backup_ed25519_2.pub",
   ]
   ```

3. `backup-remote plan` でrecipient指紋が2本表示されることを確認する(設定ミス・指紋重複はこの段階の設定エラーで検出される)。

追加以降のencryptは2鍵(1-of-2)で暗号化され、いずれか1本の秘密鍵で復号できる。

### 7.2 遡及の可否(upload前後の違い)

- **upload前なら再暗号化で遡及可能**: 既存の`.age`を削除してencryptを再実行すると、新recipient構成で再暗号化される(skip判定は「`.age`が存在しregistry記録と一致」のため、`.age`を消せば必ず再暗号化される)。manifestも再生成される:

  ```console
  $ rm data/out/<date>/<name>.age data/out/<date>/manifest.jsonl.age
  $ backup-remote encrypt --from-list targets.toml
  ```

- **upload後は遡及不可**: S3へ投入済みのオブジェクトは旧recipient構成のままで、upload実行はbucket一致の成功記録がある対象を再uploadしない。recipientを追加した場合は、以降のバッチから新構成で暗号化する。全量を新構成にしたい場合は、別日の新バッチとして再暗号化・uploadし直す(旧オブジェクトの削除は手運用)

### 7.3 旧鍵の温存

既存の`.age`は暗号化時のrecipient構成のまま変わらない。鍵を追加しても**旧秘密鍵を廃棄すると、旧recipient構成で暗号化されたバッチ(追加前にuploadした分)を復元できなくなる**。追加後も旧鍵は保管し続けること。どのバッチがどのrecipient構成かは、registryのencrypt記録`recipients`(指紋リスト)またはmanifestで確認できる。

## 8. 正本bucket変更時の移行手順

正本bucket(`configs/aws.toml`の`[aws].bucket`)を恒久変更する場合の手順。

背景: registryのupload記録はupload先の実効bucketを`s3_bucket`として記録し、upload実行は「`s3_bucket`が当該実行の実効bucketと一致する成功記録が無いもの」を対象に列挙する。ここで、**`s3_bucket`フィールドを持たない旧レコード(s3_bucket記録導入前の記録)は、aws.tomlのbucketと一致するものとして解釈される**(前方互換)。そのためaws.tomlのbucketを書き換えるだけだと、欠損記録の対象が「新bucketで成功済み」と誤帰属され、新bucketへの再uploadが行われないまま漏れが残る。

移行手順:

1. **変更前にregistryの記録を確認する**: upload/manifest_upload記録の`s3_bucket`分布を確認する:

   ```console
   $ jq -r 'select(.record_type=="upload" or .record_type=="manifest_upload")
            | (.s3_bucket // "(欠損)")' data/registry.jsonl | sort | uniq -c
   ```

   `(欠損)`が出る、または旧bucket名の記録がある場合は移行対象。現行実装は常に`s3_bucket`を記録するため、欠損記録は過去の旧バージョン実行分でのみ発生し得る。

2. **aws.tomlは旧bucketのまま、CLI一時上書きで新bucketへuploadする**(正本変更前の移行実行。優先順位は4.4のとおり):

   ```console
   $ backup-remote upload --bucket <新bucket> --dry-run    # 移行対象の予定確認
   $ backup-remote upload --bucket <新bucket>              # 実行
   ```

   この実行では旧bucket記録・欠損記録の対象が列挙され、新bucketへuploadされる(記録には`s3_bucket: <新bucket>`が付く)。新bucketへ既に入っている分はHeadObjectのSize一致でskipされる。

3. **S3側で新bucketの配置を確認する**:

   ```console
   $ aws s3 ls s3://<新bucket>/<date>/
   ```

4. **aws.tomlのbucketを新bucket名へ変更する**(正本の切替)。

5. **変更後、`upload --dry-run`で対象なし(終了コード0)になることを確認する**。全記録が新bucketで成功済みとなり、移行完了。

注意: 旧bucketのオブジェクトは削除しない限り残る。旧bucketの廃止・削除は移行完了を確認してから手運用で行うこと。

## 9. 復元(手動復元・補助手順)

復元は`backup-remote restore`コマンド(実装済み)で行うのが標準手順。`restore request`(復元job発行)-> `restore status`(状態確認)-> `restore download`(取得・sha256検証・復号)の3サブコマンドで構成され、手順・費用の目安・検証構成・トラブルシューティングの正本は `docs/restore_backup.md`。

本章はbackup-remoteを使わない環境(新環境・他者の環境)向けの手動復元手順である。

前提:

- 対象の`.age`(manifest.jsonl.ageを含む)はDeep Archive配置のため、**取得前にGlacier復元job(一時復元)が必要**ただしstorage_classをSTANDARD等へ変更してuploadしたバッチでは復元jobは不要(即時取得可能):

  ```console
  $ aws s3api restore-object --bucket <bucket> --key <date>/<name>.age \
      --restore-request 'Days=7,GlacierJobParameters={Tier=Standard}'
  ```

  Tierは`Standard`(完了まで12時間以内・既定)か`Bulk`(48時間以内・低費用)を選べる(取り出し費用の目安は`docs/restore_backup.md` 3.4)。復元の完了待ちの間、進捗は `aws s3api head-object` の `Restore` フィールドで確認する。

手動復元手順(`<名前>`はファイル名から拡張子`.age`を除いた部分。例: `photo.zip.age`なら`photo.zip`):

1. 対象ファイルを取得する:

   ```console
   $ aws s3 cp s3://<bucket>/<date>/<名前>.age .
   ```

2. 秘密鍵で復号する:

   ```console
   $ age -d -i configs/secret/keys/private/backup_ed25519 -o <名前> <名前>.age
   ```

3. checksum検証: manifestを取得・復号し、取得した`.age`のsha256とmanifest記録値を照合する:

   ```console
   $ aws s3 cp s3://<bucket>/<date>/manifest.jsonl.age .
   $ age -d -i <秘密鍵> -o manifest.jsonl manifest.jsonl.age
   $ sha256sum <名前>.age
   ```

   `sha256sum`の計算結果(16進)が、manifest.jsonl内の`source_name`が`<名前>`に一致する行の`age_sha256`の値と一致することを確認する。
