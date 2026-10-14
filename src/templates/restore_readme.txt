RESTORE_README.txt - バックアップの復元手順
==========================================

このファイル(RESTORE_README.txt)だけは平文です。読むために復号は不要です。
このREADMEは特定の環境に依存しない固定文面です(bucket名・鍵の保管場所等の
環境依存の情報は含みません)。

■ このバケットとプレフィックスについて

このバケットには、NASバックアップの暗号化アーカイブが日付ごとのバッチ
(プレフィックス `<日付>/`)ごとに格納されています。バッチ内の各ファイルは
暗号化済みであり、そのままでは読めません。以下の手順で復元してください。

■ ファイル形式

- 各ファイル `<名前>.age` は age v1 形式で暗号化されています(拡張子 .age)。
  age コマンドの `age -d` に直接渡せます。
- `manifest.jsonl.age` は、バッチ内ファイルの目録(manifest)を同じ鍵で
  暗号化したものです(ファイルごとのsha256などを含みます)。
- `RESTORE_README.txt`(このファイル)のみ平文です。

■ 復元に必要なもの

1. age コマンド
   - 入手先: https://age-encryption.org
   - GitHub: https://github.com/FiloSottile/age
2. SSH ed25519 秘密鍵
   - バックアップの作成者が、バックアップとは別の場所で保管しています。
     このバケットには秘密鍵は含まれません。鍵を紛失すると復元できません。
3. aws CLI(v2以降)と当該バケットの読み取り権限

■ 復元手順

`<bucket>`・`<日付>`・`<名前>`・`<秘密鍵のpath>` は実際の値に置き換えてください。
`<名前>` はファイル名から拡張子 `.age` を除いた部分です
(例: `photo.zip.age` というファイルなら `photo.zip`)。

1. 対象ファイルを取得する:

     aws s3 cp s3://<bucket>/<日付>/<名前>.age .

2. 秘密鍵で復号する:

     age -d -i <秘密鍵のpath> -o <名前> <名前>.age

バックアップ作成環境に backup-remote コマンドがある場合は、`backup-remote restore request` → `restore status` → `restore download` で上記の取得・復号・検証を一括で行えます(詳細は作成者の手順書)。

■ manifestの取得とchecksum検証

取得した .age が破損・改変されていないか、manifestの記録値と照合できます。

1. manifestを取得して復号する:

     aws s3 cp s3://<bucket>/<日付>/manifest.jsonl.age .
     age -d -i <秘密鍵のpath> -o manifest.jsonl manifest.jsonl.age

2. 取得した .age のsha256を独立に計算する:

     sha256sum <名前>.age

3. 計算結果(16進)が、manifest.jsonl 内の該当ファイルレコードの `age_sha256`
   の値と一致することを確認する。該当レコードは `source_name` の値が
   `<名前>` に一致する行です。
