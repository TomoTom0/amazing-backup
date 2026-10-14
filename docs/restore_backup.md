# リストア手順書(バックアップの復元)

S3 Glacier Deep Archiveへ保管した暗号化アーカイブを、`backup-remote restore`コマンドでローカルへ取得・sha256検証・復号するまでの手順書。災害時(バックアップ作成環境の喪失)を含む。

- 対象読者: バックアップを復元する人(平常時の部分復元・災害時の復旧の両方)
- CLI全体の使用手順(セットアップ・build・設定ファイル)は `docs/usage_backup.md`
- 設計の正本: `tmp/tasks/tm-5/design_restore.md`(CLI仕様・検証・終了コードの詳細)。AWS SDK移行(TASK-9)の設計は `tmp/tasks/tm-9/design_sdk_migration.md`
- 実装バージョン: 0.1.0
- 最終確認: 2026-10-14(実装の`--help`出力・動作に基づく)

## 1. 前提

### 1.1 ツールとcredentialファイル

| 要件 | 対象 | 確認 |
|---|---|---|
| credentialファイル(API key) | request / status / download(全て。`aws.toml`の`[aws].credentials_file`が指す先) | fileの存在(作成手順は`docs/usage_backup.md` 2.3) |
| age(v1.2系) | download(復号のため) | `age --version` |
| backup-remote本体 | 全て | `cargo build --release` 済みbinaryまたは `cargo run --quiet --` |

S3接続はbackup-remoteがSDK(aws-sdk-s3)を内蔵するため**aws CLIは不要**(導入・`~/.aws`設定は必要ない)。credentialは`configs/secret/aws-credentials.toml`(API keyファイル)を必須とし、credential chain(`~/.aws`・環境変数・IMDS)へのfallbackは行わない(fail-closed)。

aws CLIがあると1.4のバッチ一覧確認や8章の手動復元に使えるが任意である(導入手順は`docs/usage_backup.md` 2.2の参考手順)。

### 1.2 秘密鍵(いずれか1本のrecipient秘密鍵)

復号には暗号化時に使ったrecipient公開鍵に対応する**SSH ed25519秘密鍵が1本**必要(1-of-N暗号化のため、recipientが複数本ある場合はそのうちの1本でよい)。

- 1本目の秘密鍵: `configs/secret/keys/private/backup_ed25519`
- 災害時はバックアップ作成者がバックアップとは別の場所へ保管している当該鍵を指す(bucketには秘密鍵は含まれない)
- 指定方法: `restore download`の`--key`、または`configs/pipeline.toml`の`[verify].decrypt_key`。`--key`未指定かつpipeline.tomlでも解決できない場合は設定エラー(終了コード2)。災害時のようにpipeline.tomlがない環境では`--key`が必須
- 鍵を紛失すると該当バッチは復元できない。recipient追加前の旧鍵も廃棄しないこと(`docs/usage_backup.md` 7.3)

### 1.3 aws.toml(bucket・region・credentialファイル)

restore系は接続先としてaws.tomlを必須とする。bucket・region・credentialファイル参照(`credentials_file`)の3項目が正しく設定されたファイルが必要:

```toml
[aws]
bucket = "example-backup-bucket"
region = "ap-northeast-1"
credentials_file = "configs/secret/aws-credentials.toml"
```

- 既定pathは`configs/aws.toml`。任意のpathは`--aws-config`で指定できる
- `credentials_file`はcredential(API key)ファイルのpath(作成手順は`docs/usage_backup.md` 2.3)。**災害時の新環境ではaccess keyを入手のうえ当該ファイルも作成する**(credential値を直書きするキーはaws.tomlに存在しない)
- **災害時の新環境ではbucket/region/credentials_fileを記載したaws.tomlとAPI keyファイルと秘密鍵のみでrestoreが動作する**(作成環境の資産は不要。1.4)
- CLI一時上書き(`--bucket`/`--region`)はaws.toml値が正である前提の当該実行限りの上書きであり、**tomlのbucket/regionが空の項目をCLI値で補完する使い方はできない**(空値はCLI指定の有無にかかわらず設定エラー2。`docs/usage_backup.md` 3.2・4.4と同一規定)

**エンドポイント確定性の注記**: SDKは環境変数`AWS_ENDPOINT_URL`/`AWS_ENDPOINT_URL_S3`を解釈して接続先endpointを切替え得る。restore系コマンドは起動時にこれらのenvが設定されていれば警告をstderrへ表示する(処理は継続)。意図した検証用構成か誤設定かは利用者側で確認すること。

### 1.4 作成環境の資産は不要(災害時でも動作)

restore系はローカルの`data/registry.jsonl`・`data/out/`・`configs/pipeline.toml`に一切依存しない。対象の発見はS3側(ListObjectsV2の`<date>/`プレフィックス列挙)で行い、検証の正本はS3上のmanifest(4章)である。例外はdownloadの`--key`既定値解決のみpipeline.tomlを参照する(1.2)。

バッチ(日付)一覧の確認はbucket側で行う(任意の確認手段。aws CLIの例):

```console
$ aws s3 ls s3://<bucket>/
```

## 2. 復元の流れの概要

Deep Archiveのオブジェクトは即時取得できないため、復元は「復元job発行 -> 待ち -> 取得」の非同期2段階になる。3サブコマンドをこの順で使う:

```
backup-remote restore request   復元job発行(この段階で取り出し費用が発生)
        |
        v  待ち(Standard: 12時間以内 / Bulk: 48時間以内)
backup-remote restore status    復元状態と取得可能期限(expiry-date)の確認
        |  全keyが restored になったら
        v
backup-remote restore download  取得・sha256検証・復号(data/restore/<date>/へ出力)
```

- `--date YYYY-MM-DD`は全サブコマンドで必須(バッチ=日付単位の指定。「全バックアップの一括復元」という事故的操作を構造的に防ぐため省略不可)
- 特定ファイルだけ復元する場合はrequest/downloadへ`--name`を指定する(取り出し費用を対象分に限定できる)
- 復元jobを発行すると、オブジェクトの復元コピーが`--days`(既定7日)の間取得可能になる。期限(expiry-date)が切れた後の再取得には再発行が必要(5章)

## 3. 各サブコマンドの手順

### 3.1 restore request — 復元job発行

```
backup-remote restore request --date YYYY-MM-DD [--name NAME ...] [--days N] [--tier TIER]
                              [--aws-config PATH] [--bucket NAME] [--region REGION]
                              [--dry-run] [--keep-going]
```

| オプション | 既定 | 意味 |
|---|---|---|
| `--date` | -(必須) | 対象batch_date(S3プレフィックス)。形式不正はusage error 64 |
| `--name` | なし(バッチ全体) | 対象source_name(basename・拡張子付き。repeatable)。バッチ内に存在しないname・重複指定は対象エラー3 |
| `--days` | 7 | 復元コピーの保持日数(1〜365の整数。範囲外・非整数は設定エラー2) |
| `--tier` | Standard | 復元Tier(`Standard`/`Bulk`。allowlist外は設定エラー2) |
| `--aws-config` | `configs/aws.toml` | aws設定(1.3) |
| `--bucket`/`--region` | aws.toml値 | 当該実行限りのCLI一時上書き(usage_backup 4.4と同一仕様) |
| `--dry-run` | なし | S3 API実行なし(ListObjectsV2/HeadObject/RestoreObject)。予定行を表示 |
| `--keep-going` | なし | per-key失敗でも継続(設定エラー2は常に即中止) |

動作:

1. 事前検査: aws.tomlの正検査・credentialファイルの読込・検査・`--days`/`--tier`の値検査(S3 APIはまだ実行しない)
2. ListObjectsV2(`<date>/`プレフィックス)で対象keyを列挙。0件は警告「当該prefixにobjectなし。--date・bucketを確認してください」+終了コード0
3. `--name`指定時は列挙結果と突合し、存在しないnameは発行前に対象エラー3
4. 各keyをHeadObjectで確認し、復元済み(`restored`)・進行中(`ongoing`)はskip、未発行のみ発行する:

   ```
   s3 RestoreObject bucket=<bucket> key=<date>/<name>.age days=<n> tier=<tier>
   ```

   (SDKのRestoreObjectへ`days`と`glacier_job_parameters.tier`を指定して発行する)
5. 進行中jobのTier変更(Standard->Bulk等)は行わない。Tierを変えたい場合は発行済みjobの期限切れ後に再度requestする

出力例:

```
ok    2026-10-08/docs.7z.age
skip  2026-10-08/photo.zip.age (restored)
ok    2026-10-08/manifest.jsonl.age
-- summary: 2 requested, 1 skipped (restored/ongoing), 0 failed
```

`--dry-run`出力例(実機確認済み):

```
[dry-run] 対象key一覧は未確定(--dateバッチ全件想定・--name指定時は当該name+manifest想定・ListObjectsV2/HeadObject未実行)
[dry-run] s3 RestoreObject bucket=example-backup-bucket key=2026-10-08/<name>.age days=7 tier=Standard
[dry-run] s3 RestoreObject bucket=example-backup-bucket key=2026-10-08/manifest.jsonl.age days=7 tier=Standard
-- summary (dry-run): RestoreObject予定(対象key一覧は未確定)
```

manifest.jsonl.ageは`--name`の指定に関わらず対象へ含まれる(downloadの検証正本になるため)。

### 3.2 restore status — 復元状態一覧

```
backup-remote restore status --date YYYY-MM-DD
                             [--aws-config PATH] [--bucket NAME] [--region REGION]
                             [--keep-going]
```

- `--name`・`--dry-run`の定義はない(statusはread-onlyなバッチ全体一覧が目的。指定するとclapがusage error 64で拒否する)
- 各keyのHeadObject `Restore`ヘッダを3状態へ分類して一覧表示する

出力例:

```
== restore status (2026-10-08) ==
  restored        2026-10-08/photo.zip.age  expiry-date="Tue, 20 Oct 2026 12:00:00 GMT"
  ongoing         2026-10-08/docs.7z.age
  not-requested   2026-10-08/manifest.jsonl.age
-- 1 restored, 1 ongoing, 1 not-requested / 3 objects (+1 README), 0 failed
```

| 表示 | 意味 |
|---|---|
| `restored` | 復元済み(取得可能)。`expiry-date`は取得可能期限 |
| `ongoing` | 復元進行中(発行済み・未完了) |
| `not-requested` | 復元job未発行(または期限切れでヘッダ消失) |

expiry-dateはS3応答の原文をGMT(UTC)表記のまま表示する(JSTへの変換は行わない。解釈の際は注意)。

### 3.3 restore download — 取得・検証・復号

```
backup-remote restore download --date YYYY-MM-DD [--name NAME ...] [--out DIR] [--key PATH]
                                [--aws-config PATH] [--bucket NAME] [--region REGION]
                                [--dry-run] [--keep-going]
```

| オプション | 既定 | 意味 |
|---|---|---|
| `--date` | -(必須) | 対象batch_date |
| `--name` | なし(manifest全件) | 対象source_name(repeatable)。manifestに存在しないname・重複指定は対象エラー3 |
| `--out` | `data/restore` | 復元先dir。配下に`<date>/`を作る |
| `--key` | pipeline.tomlの`[verify].decrypt_key` | 復号用SSH ed25519秘密鍵(1.2)。pipeline.tomlがない環境(災害時)では`--key`必須 |
| `--dry-run` | なし | S3 API実行なし(HeadObject/GetObject)・復号もしない。GetObject予定行を表示 |
| その他 | - | requestと同一(3.1) |

動作:

1. 事前検査: aws.toml・credentialファイル・秘密鍵・ageバイナリ
2. **manifest.jsonl.ageを毎回再取得・復号する**(ローカルに既存ファイルがあっても流用しない。manifestが検証の正本であるため、自己検証が構成できないmanifestはS3側を正とする。このため再実行時もmanifestの復元期限が切れていると転送エラー5になる。5章)
3. manifest(JSONL)をparseし、`--name`と突合(1行目は`record_type: "batch"`ヘッダ、以降がファイルレコード)
4. 各ファイル(処理順):
   a. 既存`<out>/<date>/<name>.age`のsha256がmanifest記録`age_sha256`と一致すれば取得をskipし復号へ進む(検証済みローカル成果物はS3側の復元状態・期限に依存しない)
   b. 不一致・不在ならHeadObjectで復元済み確認(未了・未発行は転送エラー5)のうえGetObject(checksum検証付き)で取得し、sha256を`age_sha256`と照合(不一致は検証エラー4)
   c. 既存の平文`<name>`がある場合、sha256が`source_sha256`と一致すれば復号skip。**不一致なら対象エラー3で上書きしない**(ローカルの既存ファイルは保護対象。手動リネーム後に再実行)
   d. `.age`を復号して平文を`.part`書込->renameで生成し、平文sha256を`source_sha256`と照合(不一致は検証エラー4)
5. summary表示

出力例:

```
ok    docs.7z  3435106201  sha256:9f86d081884c7d65
skip  photo.zip (平文sha256一致)
-- summary: 1 downloaded, 1 skipped, 0 failed / out: data/restore/2026-10-08 / manifest: ok
```

(sizeはbyte数。sha256は先頭16桁のprefix表示)

### 3.4 Tierと取り出し費用・時間の目安

| Tier | 復元完了までの目安 | データ取り出し料金の目安(GBあたり) |
|---|---|---|
| `Standard`(既定) | 12時間以内 | 約0.02 USD |
| `Bulk` | 48時間以内 | 約0.0025 USD(Standardの1/8) |

- `Expedited`はDeep Archiveでは選択不可のため、本CLIの`--tier`の許容値に含まれない
- 上記料金は米国リージョン(us-east-1)の参考値(2026-10-12時点のAWS公開値を基準)。**リージョンにより異なる**(東京regionはStandardが約1割高い)。実行するregionの正確な値はAWS公式料金ページで確認すること
- 試算例(取り出しデータ料のみ・参考): 500GBを一括取り出しする場合、Standardで約10 USD、Bulkで約1.25 USD
- このほかに復元リクエスト単価(件あたり)・復元期間中の復元コピー(`--days`日間)の保管料・通常のGETリクエスト料・データ転送費が発生する。数百万オブジェクト規模でなければデータ取り出し料が支配的
- 費用を抑えるには、(a) `--name`で必要なファイルだけ取り出す、(b) 緊急度が低ければ`--tier Bulk`を使う、の2つが有効

参照(2026-10-12確認):

- 復元Tierと所要時間: https://docs.aws.amazon.com/AmazonS3/latest/userguide/restoring-objects-retrieval-options.html
- 料金: https://aws.amazon.com/s3/pricing/

## 4. 検証の構成(manifestが正本・4層)

検証の期待値はS3上の`manifest.jsonl.age`(復号後のmanifest)を正本とする。manifestは各ファイルレコードに`source_name`/`age_sha256`/`source_sha256`を含むため、作成環境のregistryがなくても検証が完結する(1.4)。

| 層 | 対象 | 方法 | 失敗時 |
|---|---|---|---|
| L1 転送整合 | GetObject取得 | GetObjectのchecksum検証有効化(SDKがGET応答のchecksumをS3側値と照合。転送経路の破損を検出) | 転送エラー5 |
| L2 暗号文完全性 | 取得`.age` | ローカルsha256再計算とmanifest記録`age_sha256`の照合 | 検証エラー4 |
| L3 平文完全性 | 復号平文 | 復号ストリームのsha256とmanifest記録`source_sha256`の照合 | 検証エラー4 |
| L4 AEAD完全性 | `.age` | age v1のChaCha20-Poly1305(改変があれば復号自体が失敗) | 復号失敗=検証エラー4 |

- S3側の`ChecksumSHA256`(HeadObject)はrestoreの検証に使わない。multipart upload時にCOMPOSITE(部分checksum結合)になるためローカル計算値との直接比較が成立しない場合があるため(設計上の制約)
- manifest平文自体のsha256はそれを記録した元が存在しないため照合対象外(L4のAEAD完全性とparse成功で担保する)

## 5. 出力先と冪等再実行

### 5.1 出力構成

```
data/restore/2026-10-08/
|-- photo.zip.age          取得済み暗号文(sha256検証済み)
|-- photo.zip              復号平文(sha256検証済み)
|-- manifest.jsonl.age     取得済みmanifest暗号文
`-- manifest.jsonl         復号済みmanifest(目録)
```

- 平文+暗号文でディスクを約2倍消費する。`.age`は検証済みの中間成果物であり、平文を誤って消しても`.age`からの再復号(再取得不要)で復旧できる。ディスクが逼迫したら検証済み平文のコピーを得たうえで`.age`を手動削除してよい

### 5.2 再実行(冪等)

- downloadの再実行は冪等: 既存`.age`・平文がそれぞれ期待sha256と一致すればskipする。crash後の再開も再実行で吸収する(restore専用の状態ファイルは持たない)
- `.age`が不一致・不在なら再取得して上書き(.part書込->rename)
- 平文が不一致なら上書きせず対象エラー3(3.3-4c)
- 平文だけを消した場合は、download再実行でローカル`.age`から再復号される(S3への再取得は発生しない)

### 5.3 復元期限(expiry-date)切れ後の再実行

- manifestは毎回再取得する(3.3-2)ため、**ローカルの`.age`・平文が全てsha256一致でも、manifestの復元期限が切れているとdownloadは「restore requestが未完了または未発行のため取得不能」の転送エラー5になる**
- 期限切れ後は`restore request`を再発行してから`restore download`を再実行する(requestは未発行のkeyのみ発行するため、復元期限内のkeyに再発行は起きない)

## 6. 終了コード

| code | 区分 | restore系の主な発生条件 |
|---|---|---|
| 0 | 成功 | 全skip完結・対象なし(prefixにobjectなしの警告も含む) |
| 2 | 設定・環境エラー | aws.toml不正(bucket/region/credentials_file空等)・credentialファイル不在/不正・秘密鍵不在・ageバイナリ不在(download)・`--tier`許容値外・`--days`範囲外 |
| 3 | 対象エラー | `--name`がバッチ内に不在(requestはS3列挙結果・downloadはmanifestと突合)・`--name`重複指定・manifest head 404=バッチ不在(`--date`/bucket誤り)・出力先の既存平文が期待sha256と不一致 |
| 4 | 検証エラー | 取得`.age`のsha256不一致・復号平文のsha256不一致・manifest parse失敗・復号失敗 |
| 5 | 転送エラー | S3 API(ListObjectsV2/HeadObject/RestoreObject/GetObject)の失敗(認証・network含む)・復元未了/未発行の対象へのdownload・request時のkey head 404 |
| 64 | 使用法エラー | `--date`形式不正・必須違反・不明オプション(statusの`--dry-run`等) |

fail-fastが既定。`--keep-going`指定時はper-key/per-file失敗で継続し、終了コードは最初に発生したエラーの区分。設定エラー2は`--keep-going`でも常に即中止(usage_backup 4.5と同一)。

## 7. トラブルシューティング

| メッセージ・症状 | code | 対処 |
|---|---|---|
| `manifest.jsonl.ageが存在しない=当該バッチ不在` | 3 | `--date`とbucket(`--aws-config`の中身・CLI一時上書き)が正しいか確認する。バッチ一覧は`aws s3 ls s3://<bucket>/`で確認できる |
| `restore requestが未完了または未発行のため取得不能` | 5 | `restore status`で状態を確認。`not-requested`/`ongoing`のkeyがあれば`restore request`を発行し、完了を待ってから再実行する |
| `取得.ageのsha256がmanifest記録と不一致(転送破損の疑い)` | 4 | downloadの再実行で再取得すれば回復する(5.2) |
| `復号平文のsha256がmanifest記録と不一致` | 4 | 再実行しても不一致が続く場合は記録側の問題の疑い。当該バッチのencrypt時の状況を確認する |
| 復号に失敗(AEAD/鍵不一致) | 4 | 指定した秘密鍵(`--key`)が当該バッチのrecipientに対応するものでない可能性。recipient構成はmanifestまたは作成環境のregistryで確認できる |
| `既存平文のsha256がmanifest記録の不一致のため上書きしません` | 3 | 出力先にある既存ファイルをリネーム・移動してから再実行する(意図的に改変したファイルの場合はそのまま使う判断も可) |
| `--name ... が存在しません`(request/download) | 3 | nameの綴り(拡張子付きbasename)と`--date`を確認する |
| `WARNING: 当該prefixにobjectなし` | 0 | `--date`・bucketを確認する(指定日のバッチが存在しない) |
| 復元期限内にdownloadしきれず期限が切れた | 5 | 5.3のとおり`restore request`再発行->`restore download`再実行(取得済み`.age`は再取得されない) |
| 秘密鍵を紛失した | - | 該当recipient構成で暗号化されたバッチは復元不能。他のrecipient秘密鍵が残っていればそれで復号できる(1-of-N)。以後の運用は`docs/usage_backup.md` 7章 |

## 8. 手動復元(aws CLI + age)との対応

手動復元手順(`aws s3api restore-object`・`aws s3 cp`・`age -d`・`sha256sum`)との対応:

|---|---|
| `aws s3api restore-object`(復元job発行) | `restore request`(HeadObjectによる済み分skip付き) |
| 復元完了の待ち・`head-object`の`Restore`確認 | `restore status`(3状態一覧) |
| `aws s3 cp`(対象の取得) | `restore download`(checksum検証付きGetObject) |
| `age -d`(復号) | `restore download`(平文sha256照合付き復号) |
| `sha256sum`とmanifestの`age_sha256`照合 | `restore download`(L2/L3検証が自動で行う) |

- backup-remoteがない環境(新環境・他者の環境)ではREADMEの手順どおり手動で復元できる。手順の詳細は`docs/usage_backup.md` 9章にも同じ内容がある
