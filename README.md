# amazing-backup

NAS等のバックアップアーカイブ(zip/7z)を [age](https://age-encryption.org/) で暗号化し、
S3(versioning有効bucket・Deep Archive)へ版管理付きで投入・照会・復元するCLI。

- **発行キー + versioning**: 実体はUUIDv7発行キー(`data/<uuid>/default.age`)で管理し、
  ファイルの移動・改名による実体移動(copy+delete)は不要。同じ内容は自動で同一実体へ合流
- **1-of-N暗号化**: SSH ed25519公開鍵複数で暗号化し、いずれか1鍵で復元可能(鍵喪失耐性)
- **索引・registry**: title/uuid/所在pathでの照会・diff(索引とS3実態の突合)・manifest由来の索引再構築
- **世代管理**: S3 lifecycle(NCVE・MPU abort・delete marker掃除)とprune・pin(世代管理対象外のピン留め)
- **実行形**: 設定はXDG規定path(`~/.config/amazing-backup/`)・repo外のどこからでも実行可能

## Install

```sh
cargo install --git https://github.com/TomoTom0/amazing-backup   # GitHubから直接
mise use -g github:TomoTom0/amazing-backup                       # mise(github backend・Releases binary)
cargo install amazing-backup                                     # crates.io(publish後)
mise use -g cargo:amazing-backup                                 # mise cargo backend(同上)
```

## Quick start

1. 設定を生成(templateはbinary埋め込み・repo無しで実行可能):

```sh
amazing-backup config init
# ~/.config/amazing-backup/ に pipeline.toml・aws.toml・aws-credentials.toml を生成
# aws-credentials.toml: access_key_id / secret_access_key を記述
# aws.toml: bucket等を記述
# pipeline.toml: public_keys にSSH ed25519公開鍵の絶対path・verify.decrypt_key に
#   秘密鍵の絶対pathを記述(照会・復元(list files・get等)は秘密鍵で索引を復号するため)
```

2. credential・鍵を用意(明示API key file・credential chain非依存):

```
config init が生成した ~/.config/amazing-backup/aws-credentials.toml に
access_key_id / secret_access_key を記述(aws.toml の credentials_file が指す先)
SSH ed25519鍵pairが無い場合は ssh-keygen -t ed25519 で生成して
~/.config/amazing-backup/keys/ 等へ置く(手順は docs/usage_backup.md 8.3)
```

3. bucketのversioningを有効化し、lifecycleを適用・初回投入:

```sh
amazing-backup manage lifecycle apply --profile dev
amazing-backup backup push /path/to/archive.zip          # titleは対話入力または--from-list
amazing-backup list files
amazing-backup get <title> --out ~/restored/             # 復元(--out必須)
```

詳細な使用手順は [docs/usage_backup.md](docs/usage_backup.md) を参照。

## Docs

`docs`サブコマンドが使用手順書・設定example・LLM/agent向け配布物をstdoutへ出力する
(正本はbinary埋め込みのため、repo未clone・install直後でも当該versionの内容が取得できる):

```sh
amazing-backup docs usage          # 使用手順書全文
amazing-backup docs config         # 設定example全文(markerは例示pathへ置換済み)
amazing-backup docs llm usage      # LLM/agent向け契約(終了コード・--format json・運用規則)
amazing-backup docs llm skill      # SKILL.md正本(~/.claude/skills/amazing-backup/へ配置)
amazing-backup docs llm snippet    # CLAUDE.md/AGENTS.md追記用pointer
```

## Shell completions

`completions`サブコマンドがbash/zsh/fishの補完scriptをstdoutへ出力する
(binary自身が生成するため、常に当該versionの語彙と一致)。

zsh(fpathに含まれるdirへ配置):

```sh
mkdir -p ~/.zsh/completions
amazing-backup completions zsh > ~/.zsh/completions/_amazing-backup
rm -f ~/.zcompdump && exec zsh   # zcompdump再生成(zsh再起動)
```

`~/.zsh/completions`がfpathに含まれない環境では、`.zshrc`のcompinit呼出より前に
`fpath=(~/.zsh/completions $fpath)`を追記する(compinitより後の追記では当該dirが
読み込まれないため順序が重要)。

bash(bash-completion 2.9+のuser dir。無い環境では`~/.bashrc`でsource):

```sh
mkdir -p ~/.local/share/bash-completion/completions
amazing-backup completions bash > ~/.local/share/bash-completion/completions/amazing-backup
```

fish(規定dirへ置くだけで自動読込):

```sh
mkdir -p ~/.config/fish/completions
amazing-backup completions fish > ~/.config/fish/completions/amazing-backup.fish
```

upgrade時: 配置済みの補完scriptはbinaryのupgradeで陳腐化する(配置fileは自動更新
されない)ため、upgrade後は上記のコマンドを再実行する(zshは`~/.zcompdump`再生成まで)。

## License

MIT
