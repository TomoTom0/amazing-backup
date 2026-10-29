# amazing-backup 開発者向け手順書(repo前提)

amazing-backupのrepoをcloneして開発・検証・repo内運用を行う開発者向けの手順書。install後(repo未clone)の利用者向け使用手順書の正本は `docs/usage_backup.md` で、本書はそれと重複しないrepo前提の内容を扱う。

- 対象読者: repo開発者(clone・build・検証・移行対応を行う者)
- 注記`(旧usage_backup.md <章>)`は、2026-10-29のTASK-3文書再構成でusage_backup.mdから本書へ分離した内容の由来(旧章番号)を示す
- 関連: 実装挙動の正本は`tests/design/`の条件書(テスト構成は`tests/README.md`)。開発ルールは`../../CLAUDE.md`(repo直下)

## 1. repo内運用の設定構成(configs/)

(旧usage_backup.md 2.3・3.2のrepo内運用記述より)

install後運用の正本はXDG規定path(usage_backup.md 2.2)。repoをcloneした環境では、repo内の次の構成を使う運用もできる:

| path | 役割 | git管理 |
|---|---|---|
| `configs/pipeline.example.toml` | pipeline.tomlのtemplate(binary埋め込み・`config init`の生成元) | 対象 |
| `configs/aws.example.toml` | aws.tomlのtemplate(binary埋め込み・`config init`の生成元。`credentials_file`の`__CREDENTIALS_FILE__`markerをcredential雛形の絶対pathへ置換する) | 対象 |
| `configs/pipeline.toml` | repo内実行用のpipeline設定(XDG配置移行時の写し元) | 対象 |
| `configs/aws.toml` | repo内実行用のaws設定(個人のbucket名を含むため.gitignore対象) | 対象外 |
| `configs/secret/` | credential・暗号鍵の集約dir(`aws-credentials/<profile>.toml`・`keys/private/<鍵>`。秘密情報のため.gitignore対象) | 対象外 |

repo内実行時のポイント:

- 設定は`--config configs/pipeline.toml`・`--aws-config configs/aws.toml`で明示指定するか、XDG規定pathへ配置する(共通option・usage_backup.md 5.1)
- 設定内の相対path(public_keys・decrypt_key・credentials_file)は実行時cwd基準。repo内運用ではrepo rootから実行し、`configs/secret/...`のrepo相対pathを記述する
- credentialは`configs/secret/aws-credentials/<profile>.toml`へ置く:

```toml
# configs/secret/aws-credentials/dev.toml(gitignore済み・実運用で作成する秘密ファイル)
[credentials]
access_key_id = "<access key id>"         # 必須
secret_access_key = "<secret access key>" # 必須
# session_token = "<session token>"       # 任意(一時credentialを使う場合のみ)
```

- 作成後`chmod 600`を推奨(秘密情報を含むファイルのため)
- 運用上は`dev.toml`と`prod.toml`の2種類(prod系3profileは同一AWS account運用を想定し`prod.toml`共通1本)
- repo内運用のaws.toml(旧3.2の実物例。`.gitignore`対象のため手元で作成する)は`bucket`に実際の値・`credentials_file`にrepo相対pathを記述する:

```toml
[aws.profiles.dev]              # 開発検証(your-dev-bucket・STORAGE CLASS STANDARD)
purpose = "dev"
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

# baked(your-baked-bucket)・atara(your-atara-bucket)も同型(familyと同じprod.tomlを参照)
```

## 2. ビルドと実行形式

(旧usage_backup.md 2.4より)

```console
$ cargo build --release          # target/release/amazing-backup
$ cargo install --path .         # PATHへ導入する場合(cargo bin)
```

実行形式は `amazing-backup <command> [options]`。repo開発では`cargo run --quiet -- <command>`でも同一挙動。repo build・install版のどちらでも、設定file本体はXDG規定path(または`--config`/`--aws-config`明示指定・1章)から読まれる。

## 3. 検証用bucket(dev: your-dev-bucket)の準備手順

(旧usage_backup.md 2.5より)

dev profileは既存bucket `your-dev-bucket` を使用する(2026-10-16確定・新設しない)。versioning有効が必須・lifecycleなし:

```console
$ aws s3api put-bucket-versioning --bucket your-dev-bucket \
    --versioning-configuration Status=Enabled
```

- lifecycleは `amazing-backup manage lifecycle apply` が行う(任意・usage_backup.md 5.10)
- **全profileのbucketでversioning有効が必須**(利用者向けの一般要件はusage_backup.md 2.4)

## 4. recipient鍵生成・手動復元のrepo前提部

(旧usage_backup.md 8.3のrepo内鍵生成path部分より。手動復元の補足は旧9.3にはrepo固有の記述が無いため、repo内運用向けの補足として新規追加)

repo内運用(configs/secret/)に鍵・credentialを置いている場合の、usage_backup.md手順のrepo前提部分:

- recipient鍵の生成(usage_backup.md 8.3に相当): repo内運用では鍵pairを`configs/secret/keys/private/`配下へ生成する:

  ```console
  $ ssh-keygen -t ed25519 -f configs/secret/keys/private/backup_ed25519_2 -N ""
  ```

  生成した公開鍵(.pub)をpipeline.toml(repo内運用では`configs/pipeline.toml`)の`[recipients].public_keys`へ追加する
- 手動復元(usage_backup.md 9.3)で使う秘密鍵は、repo内運用では`configs/secret/keys/private/`配下の鍵を指定する(`age -d -i configs/secret/keys/private/backup_ed25519 ...`。旧9.3の鍵指定は`<秘密鍵>`汎用のため、この補足はrepo内運用向けの新規追加)。aws CLIの認証はamazing-backupと無関係(credential chain)のため別途設定する

## 5. 設計書・docs外の正本参照

(旧docs索引「参照(docs外の正本)」節・旧usage_backup.md冒頭の正本参照より)

`tmp/`は.gitignore対象の作業dirのため、clone直後のrepoには存在しない(タスク作業時の正本配置場所)。

- S3ストレージ設計書: `tmp/tasks/tm-10/design_s3_storage.md`(TASK-10紐付き。versioning・索引・CLI体系(list/query/get/diff/edit/backup/pin/manage)・保持policy・bucket運用の正本)
- S3キー設計書: `tmp/tasks/tm-18/design_key-uuid-versioning.md`(TASK-18紐付き。UUID発行キー+versioning・実体/所在/title語彙・索引schema=2・lifecycle・manage resetの正本)
- 旧パイプライン設計書: `tmp/tasks/tm-1/design_backup_pipeline.md`(TASK-1紐付き。暗号化経路・registry schema・終了コード設計の正本)
- AWS SDK移行設計書: `tmp/tasks/tm-9/design_sdk_migration.md`(TASK-9紐付き)
- config init設計書: `tmp/tasks/tm-1/design_config_init.md`(TASK-1紐付き)
- completions設計書: `tmp/tasks/tm-2/design_completions.md`(TASK-2紐付き)
- docsサブコマンド設計書: `tmp/tasks/tm-4/design_docs_command.md`(TASK-4紐付き)
- テスト構成: `tests/README.md`(条件書tests/design/・cargo test階層・verify script。実装挙動の正本は`tests/design/`の条件書)
- 開発ルール: `../../CLAUDE.md`(repo直下)

## 6. release手順(version同期)

docsサブコマンドの`docs usage`は埋め込み正本(docs/usage_backup.md)の「実装バージョン」行をbinaryのversion(`CARGO_PKG_VERSION`)へ置換して出力する。release時は次の手順で正本のversion literalを同期する:

1. `docs/usage_backup.md`冒頭の`- 実装バージョン:`行をrelease予定versionへ手動更新する(update-version後・tag作成前)
2. tag作成前に`cargo test`全件のpassを確認する(更新漏れは`docs.usage-doc-version-sync`条件〔正本literal==CARGO_PKG_VERSION・対象行count==1〕がfailして検出される)
