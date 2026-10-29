# amazing-backup LLM/agent向けusage(機械的契約)

## 1. 位置づけ

本書はLLM/agentが`amazing-backup`を誤用なく使うための機械的契約の参照。人間向けの説明・手順・費用目安は`amazing-backup docs usage`で取得する。本書はbinary埋め込みのため、実行環境のbinary自身の出力が常に正(GitHub上のdocsはHEAD版であり、install時点のbinary versionと異なる場合がある)。

## 2. コマンド体系

| 系統 | command | 用途 |
|---|---|---|
| 照会 | `list files` / `list versions` / `list pins` / `list runs` / `list jobs` | 実体目録・版一覧・pin・run・復元job状況 |
| 照会 | `query <手がかり>` | title/uuid/所在pathでの探索(title部分一致・uuid前方一致・所在path部分一致) |
| 取得 | `get <title|uuid>` | 単一実体の取得(未復元なら復元job発行を内包) |
| 照会 | `diff` | 索引とS3実態の完全性突合 |
| 書込み | `backup push <PATH>...` | 暗号化・upload・manifest・索引更新を1実行で |
| 復元 | `backup pull` | category/run/全対象の一括復元(確認付き) |
| 検査 | `backup verify` | ローカル成果物の整合検査(S3接続なし) |
| 管理 | `pin add` / `pin rm` | ピン留め版(世代管理対象外)の付与・解除 |
| 管理 | `manage prune/lifecycle/index/location/jobs/reset` | 世代削除・bucket保持設定・索引再構築・旧所在除去・復元job再発行・dev bucket初期化 |
| 設定 | `config init` | 設定file群の初期生成(規定pathへtemplate展開・既存file保護) |
| 設定 | `edit metadata/files/layout/policy` | 設定sectionの確認(検証付き) |
| 補完 | `completions <bash|zsh|fish>` | shell補完scriptのstdout出力 |
| 文書 | `docs usage/config` / `docs llm usage/skill/snippet` | 配布ドキュメントのstdout出力(本書を含む) |

主要文法(照会系は末尾に共通option群 `--config`・`--aws-config`・`--key`・`--profile`・`--format human|json` を持つ):

```
amazing-backup get <title|uuid> [--path P] [--version V] [--version-id ID] [--pin <名>]
                     --out DIR [--tier Standard|Bulk] [--days N] [--yes]
amazing-backup backup push [--from-list FILE] [--dry-run] [--pin <版名>] [--keep-going] <PATH> [<PATH> ...]
amazing-backup backup pull (--all | --category C | --run ID) --out DIR [--tier T] [--days N] [--dry-run] [--yes]
amazing-backup backup verify [--checksum] [--roundtrip] [--key PATH]
amazing-backup pin add <title|uuid> [--latest | --version V] --as <版名>
amazing-backup pin rm <title|uuid> --pin <版名>
amazing-backup manage prune [--dry-run | --apply]
amazing-backup manage lifecycle show|apply
amazing-backup manage index rebuild
amazing-backup manage index reconcile [--apply]
amazing-backup manage location rm <source_path> [--uuid <uuid>]
amazing-backup manage jobs reissue <object_key> [--tier T]
amazing-backup manage reset [--apply]
```

`<PATH>`は対象アーカイブ(`.zip`/`.7z`。1個以上必須)。titleは実体の人間用ラベル(64文字以内・UTF-8可。新規発行時必須)。uuidはUUIDv7発行キー(hyphenated lowercase 36字)。

## 3. 出力契約(--format json)

照会系8command(list 5種・query・get・diff)は`--format json`でmachine readable出力をする:

- **stdout全体が単一JSON**(追記行・装飾なし)
- 人間向け通知(警告・案内)はstderrへ出力
- 既定は`human`

## 4. 終了コード一覧

| code | 区分 | 主な発生条件 |
|---|---|---|
| 0 | 成功 | 対象なし・全skip完結・復元job発行の案内(get/pull)を含む |
| 1 | 内部エラー | 予期しない内部エラー(bug) |
| 2 | 設定・環境エラー | 設定file不在・schema違反・credential/秘密鍵/age binary不在・versioning無効bucket・索引不在 |
| 3 | 対象エラー | 対象不在・拡張子対象外・索引上のtitle/uuid/版/pin不在・候補未特定・write-once違反 |
| 4 | 検証エラー | 取得object/復号平文のsha256不一致・verifyの検証不一致 |
| 5 | 転送エラー | S3 APIの失敗(認証・networkを含む) |
| 64 | 使用法エラー | 引数誤り(不明option・PATH 0個・`--out`省略・pullの範囲指定違反・allowlist外の値等) |

- fail-fastが既定(最初のエラーで中止。ただし対象解決段階は全件列挙後に一度に失敗)
- `--keep-going`(backup push)はper-file失敗を捕捉して継続し、終了コードは最初のエラー区分。設定エラー2は`--keep-going`でも常に即中止

## 5. 破壊的操作の規則

- **manage系(prune・index reconcile・reset)はdry-runが既定**。実行は`--apply`。`--dry-run`と`--apply`の併用はusage 64
- backup push / backup pullの`--dry-run`は実行予定のみの表示(pushのdry-runは暗号化せずS3 API呼出も行わない)
- **get / backup pullの`--out DIR`は必須**(省略はusage 64)
- backup pullの確認ガード: `--yes`なしならTTYでyes/no確認。**非TTY実行で`--yes`なしはusage 64**
- **`--yes`はget/backup pull共通の明示的上書き指示を兼ねる**(出力先`<out>/<title>`の既存平文の扱い):
  - 既存平文のsha256が索引記録の`source_sha256`と一致: 取得・復号をskipして終了0(冪等。`--yes`の有無に依らず上書き対象外)
  - sha256不一致: `--yes`なしなら上書きせず対象エラー3。`--yes`ありなら**上書きして続行**
  - 既存平文が読込不能(権限等でsha256検証不能): 不一致と同じ扱い(`--yes`なし対象エラー3・`--yes`あり上書き続行)
  - 既存データを保護する必要がある場合は**空のdirを`--out`へ指定する**(`--yes`を非TTY確認の省略目的だけで使わない)
- `manage reset`はdev bucket限定の全消去(安全弁違反は設定エラー2)

## 6. 同時実行の規則

S3への書込み有無と索引・ローカルregistryの変更有無で4分類する:

- **(a) 索引全体再構築PUT+ローカルregistry appendを伴う系**: `backup push`・`pin add`/`pin rm`・`manage prune --apply`・`manage index rebuild`・`manage index reconcile --apply`・`manage location rm`・`manage reset --apply`。**これらの相互同時実行は禁止**(直列に1実行ずつ)。理由: 索引PUTはlast-writer-winsのため並走runの反映を失わせ得る・run manifest PUTとregistry appendの記録が競合する
- (b) S3へ書込むが索引・registryは不変の系: `manage lifecycle apply`・`manage jobs reissue`・get/backup pullでの復元job発行(RestoreObject)。(a)との並走は可能
- (c) 読み出しのみの系: list/query/diff・list jobs・各dry-run表示
- (d) ローカルのみでS3未接触の系: config init(設定file生成。registry append・索引PUTを行わない)・backup verify・edit・completions・docs。並走制約の対象外

## 7. S3非干渉規則

bucketへの操作はamazing-backup経由のみ。**aws CLI・SDK直接・AWS Consoleからのobject操作は禁止**(索引`sys/index.jsonl.age`・ローカルregistry・versioning・pin(write-once)の前提を壊すため)。手動復元の例外は人間向け手順書9.3(`amazing-backup docs usage`)の範囲のみ。

## 8. 環境変数

| env | 意味 |
|---|---|
| `AWS_ENDPOINT_URL` / `AWS_ENDPOINT_URL_S3` | SDKが解釈して接続先endpointを切替え得る。設定されていれば起動時にstderrへ警告表示するが、解釈の無効化はしない |
| `AMAZING_BACKUP_HOME` | 規定path(pipeline.toml・aws.toml・registry.jsonl・cache/)をすべて`$ABH/`配下へ統合(test/CI用) |
| `AMAZING_BACKUP_FAKE_S3_STATE` | debug build限定のfake S3 mode。指定時はstderr冒頭へ`WARNING: FAKE S3 MODE`表示。release buildでは無効 |

## 9. credential・秘密鍵の扱い

- credential(access key)・秘密鍵の**値を表示・logへ出力しない**(error文でも値は非表示)
- credentialは明示的なAPI key file(`credentials_file`が指すtoml)からのみ読む。credential chain(IMDS・env・`~/.aws`)へのfallbackはしない(fail-closed)

## 10. 災害時の依存資産

照会・復元(list/query/get/diff/backup pull)は**aws.toml(bucket/region/prefix/credentials_file)・API keyファイル・いずれか1本の秘密鍵**で動作する(ローカルregistry・cache dir・pipeline.tomlは不要)。

- **pipeline.tomlが規定pathに無い場合は`--key <秘密鍵path>`が必須**(既定`[verify].decrypt_key`が読めないため)
- 新環境では**age binary v1.2系も必要**(索引・実体の復号に使用)
- **新環境で`config init`を使う場合の注意**: initはpipeline.tomlも生成し、生成直後の`[recipients].public_keys`は実在しないpathのdummyである。照会系は規定pathのpipeline.tomlを「在れば読む」ため、dummyのままでは**`--key`指定でも設定エラー2になる**。退避済み公開鍵pathへの置換かpipeline.tomlの退避が必要
- 復元job発行はexit 0で冪等(約12〜48h後の再実行で取得が進む)

## 11. 詳細参照

実行手順・費用目安・E2E・災害時手順の詳細は`amazing-backup docs usage`で取得する。
