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

1. 設定を配置:

```sh
mkdir -p ~/.config/amazing-backup
cp configs/aws.example.toml ~/.config/amazing-backup/aws.toml
cp configs/pipeline.example.toml ~/.config/amazing-backup/pipeline.toml
# aws.toml: bucket/region/prefix/credentials_file(pipeline.toml: recipients鍵等)を記述
```

2. credential・鍵を用意(明示API key file・credential chain非依存):

```
~/.config/amazing-backup/aws.toml の credentials_file が指す先に
access_key_id / secret_access_key を記述した toml を配置
```

3. bucketのversioningを有効化し、lifecycleを適用・初回投入:

```sh
amazing-backup manage lifecycle apply --profile dev
amazing-backup backup push /path/to/archive.zip          # titleは対話入力または--from-list
amazing-backup list files
amazing-backup get <title> --out ~/restored/             # 復元(--out必須)
```

詳細な使用手順は [docs/usage_backup.md](docs/usage_backup.md) を参照。

## License

MIT
