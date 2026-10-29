# docs索引

恒久的に保守される文書(仕様・運用手順・索引・参照資料・changelog)の索引。作業履歴・経緯記録はdocsに置かず、tmタスクのbodyと`tmp/tasks/tm-<id>/`(タスク紐付き設計書等)で管理する。

## 文書一覧

| path | 内容 |
|---|---|
| `usage_backup.md` | amazing-backup CLIの使用手順書(install後〔repo未clone〕利用者向け。config init起点のセットアップ・XDG配置の設定ファイル・UUID発行キー+versioningのデータ配置と保持・全commandリファレンス(title/uuid語彙・completions・docs)・終了コード・E2E・復元手順(get/backup pull・費用目安・平文衝突保護)・運用規則(dev/prod profile・prod系3bucket使い分けとbucket単位の混同防止)・災害時手順・旧サブコマンドからの移行ガイド。`amazing-backup docs usage`の配布正本〔version行はbinary側で置換〕) |
| `llm/usage.md` | LLM/agent向けusage(`amazing-backup docs llm usage`の配布正本。コマンド体系・`--format json`出力契約・終了コード一覧・破壊的操作と同時実行の規則・S3非干渉規則・環境変数・災害時依存資産の機械的契約。usage_backup.md終了コード表との一致はtestで検証) |
| `llm/skill.md` | SKILL.md正本(`amazing-backup docs llm skill`の配布正本。frontmatter(name/description)+実行原則+契約取得pointer+定期運用/非常時の最小手引き。`~/.claude/skills/amazing-backup/SKILL.md`へredirect配置可能な最終形) |
| `llm/snippet.md` | pointer snippet正本(`amazing-backup docs llm snippet`の配布正本。CLAUDE.md/AGENTS.mdへ追記して使う数行。契約本文は内包せず`docs llm usage`/`docs usage`参照のみ) |
| `dev/dev_backup.md` | repo開発者向け手順書(repo前提。repo内運用のconfigs/構成・buildと実行形式・検証用bucket(dev: your-dev-bucket)の準備手順・repo所在資産を使うrecipient鍵生成と手動復元・設計書などdocs外の正本の参照一覧) |

docs外の正本(タスク紐付き設計書・テスト構成・開発ルール)の参照一覧は `dev/dev_backup.md` にまとめている。
