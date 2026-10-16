# docs索引

恒久的に保守される文書(仕様・運用手順・索引・参照資料・changelog)の索引。作業履歴・経緯記録はdocsに置かず、tmタスクのbodyと`tmp/tasks/tm-<id>/`(タスク紐付き設計書等)で管理する。

## 文書一覧

| path | 内容 |
|---|---|
| `usage_backup.md` | backup-remote CLIの使用手順書(セットアップ・設定ファイル・データ配置と保持・全commandリファレンス・終了コード・E2E・復元手順(get/backup pull・費用目安・平文衝突保護)・運用規則(dev/prod profile・prod系3bucket使い分けとbucket単位の混同防止)・災害時手順・旧サブコマンドからの移行ガイド) |

## 参照(docs外の正本)

- S3ストレージ設計書: `tmp/tasks/tm-10/design_s3_storage.md`(TASK-10紐付き。versioning・索引・CLI体系(list/query/get/diff/edit/backup/pin/manage)・保持policy・bucket運用の正本)
- 旧パイプライン設計書: `tmp/tasks/tm-1/design_backup_pipeline.md`(TASK-1紐付き。暗号化経路・registry schema・終了コード設計の正本)
- テスト構成: `tests/README.md`(条件書tests/design/・cargo test階層・verify script。実装挙動の正本は`tests/design/`の条件書)
- 開発ルール: `../CLAUDE.md`
