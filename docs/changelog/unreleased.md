# Unreleased (次期 0.2.0)

## 0.2.0(未確定・TASK-20)

### 破壊的変更
- 分類語彙の改名: `rel_path` → `label`(索引・registry・manifest・query JSON出力のkey/field)
- config: `[files."<basename>"]`の`path` keyを`label`へ変更(旧keyは設定エラー2)
- CLI: `get`の`--path` flagを`--source-path`へ変更(照合対象=location.source_path完全一致は不変。旧flagはusage 64)
- 索引schemaを2から3へ(旧schema索引の読込・変換は行わない。`manage index rebuild`で再構築)
- registry file_write record schemaを2から3へ(旧形式行はparse失敗。初期化は0.2.0適用前に旧binary(v0.1.0)で`manage reset --apply`を実行)
- manifest file record schemaを2から3へ(batch header行はschema 2のまま。旧manifestが残る環境では`manage index rebuild`不可・0.2.0適用前に初期化と再pushが必要)
- 移行手順(旧registry/旧manifestが残る環境): 【旧binary(v0.1.0)でreset→config書換([files].path→label)→0.2.0適用】の順序(0.2.0 binaryのresetは削除前にregistryを読み込むため旧形式registryに対して実行できず・旧manifestからのrebuildはlabel欠損のため不可)

### 旧語彙→新語彙対応表
| 旧 | 新 |
|---|---|
| rel_path(field/key・表示) | label |
| [files].path(config key) | [files].label |
| get --path | get --source-path |
| query JSON "rel_path" | "label" |
| INDEX_SCHEMA=2 | INDEX_SCHEMA=3 |
| file_write schema 2 | schema 3 |
| manifest file record schema 2 | schema 3 |

- 維持(改名対象外): auto_path_roots・unfiled_category・category・source_path・latest_source_path・identity_hash・metadata schema=1。正規化規則・category名規則も不変(名称のみ変更)
- 内部: normalize_rel_path→normalize_label・ManifestV2File→ManifestV3File等の関数・構造体改名(外部影響なし)

# 0.1.0
- 初版(v0.1.0として公開)
