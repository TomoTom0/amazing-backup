---
name: amazing-backup
description: NASバックアップアーカイブ(zip/7z)の暗号化・S3(versioning+Deep Archive)へのbackup・復元・探索・保持管理をamazing-backupで行うとき、災害時復旧・定期バックアップ運用・amazing-backup管理下のS3 bucketへの言及、「バックアップを取る/戻す/探す」場面で自律的にロードする
---

# amazing-backup(NASバックアップCLI)

`.zip`/`.7z`アーカイブをage(SSH ed25519鍵)で暗号化し、S3(versioning有効bucket・Deep Archive)へ版管理付きで投入・照会・復元するCLI。AWS SDKをbinary内蔵(aws CLI不要)。設定はXDG規定path(`~/.config/amazing-backup/`)。

## 実行原則

- **S3への操作はamazing-backup経由のみ(aws CLI禁止)**。aws CLI・SDK・Consoleからのobject操作は索引・registry・versioning・pin(write-once)の前提を壊す
- **索引・registryを更新する系(backup push・pin add/rm・manage系の実行系)は相互に同時実行しない**(直列に1実行ずつ)
- 破壊的操作(manage prune・index reconcile・reset)はdry-run既定の確認を経て`--apply`で実行する
- 照会系8command(list 5種・query・get・diff)は`--format json`でstdout全体が単一JSON
- get / backup pullの`--out DIR`は必須。`--yes`は既存平文の明示的上書き指示も兼ねる(sha256一致の既存平文はskip冪等・不一致/読込不能は`--yes`なし対象エラー3・あり上書き続行)。既存データを保護する場合は空のdirへ出す

## まず読む

`amazing-backup docs llm usage` を実行して契約(終了コード・`--format json`・環境変数・同時実行規則)を取得する。

## 定期運用

- `backup push`は冪等(変更の無いfileはskip判定。失敗時の再実行で残りが処理される)
- bucket側保持設定は `manage lifecycle apply`

## 非常時

- 照会・復元は aws.toml・API keyファイル・秘密鍵・age binary(v1.2系)で動作する(ローカルregistry・cache dir・pipeline.tomlは不要。pipeline.toml不在時は`--key`必須)
- `config init`直後のpipeline.tomlは実在しないpathのdummy公開鍵を含むため、`--key`指定でも照会は設定エラー2になる(退避済み公開鍵pathへの置換かpipeline.tomlの退避が必要)
- getは未復元(DEEP_ARCHIVE)なら復元job発行でexit 0(冪等)。約12〜48h後に同じcommandを再実行

## 契約の鮮度

docsはbinary埋め込みのため、install済みbinary自身の出力が正。GitHub HEADのdocsでなく`amazing-backup docs llm usage`の取得結果を使う。
