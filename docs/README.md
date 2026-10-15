# docs索引

恒久的に保守される文書(仕様・運用手順・索引・参照資料・changelog)の索引。作業履歴・経緯記録はdocsに置かず、tmタスクのbodyと`tmp/tasks/tm-<id>/`(タスク紐付き設計書等)で管理する。

## 文書一覧

| path | 内容 |
|---|---|
| `usage_backup.md` | backup-remote CLIの使用手順(セットアップ・設定ファイル・サブコマンド・終了コード・E2E実行手順・鍵管理・bucket移行・手動復元) |
| `restore_backup.md` | リストア手順書(restore request/status/download・費用と時間の目安・検証4層・出力先と冪等再実行・終了コード・トラブルシューティング・手動復元との対応) |

## 参照(docs外の正本)

- 設計書: `tmp/tasks/tm-1/design_backup_pipeline.md`(TASK-1紐付き。Rust構成・registry schema・終了コード設計の正本)
- S3ストレージ設計書: `tmp/tasks/tm-10/design_s3_storage.md`(TASK-10紐付き。versioning・索引・新CLI体系(list/query/get/diff/edit/backup/pin/manage)・保持policy・bucket運用の正本)
- テスト構成: `tests/README.md`(条件書tests/design/・cargo test階層・verify script)
- 開発ルール: `../CLAUDE.md`
