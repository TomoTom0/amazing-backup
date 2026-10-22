# backup-remote

## 開発進行上のルール

- 後戻りコストが大きい設計決定(実装言語・framework・永続化形式等)は、検討結果の報告だけで次段階に進まず、ユーザーの承認を得てから進める(報告は承認ではない)
  - 経緯: TASK-1で実装言語(Python)の検討結果を報告のみで設計以降に進め、ユーザー承認を経ていなかったことが実装中断後に判明し、Rustへ切り替えた(2026-10-08)
- commitはagentの判断で適切なタイミング(タスク完結・全検証pass後の区切り等)に実行してよい(alcom finishを使用。2026-10-20ユーザー指示)。push・PR作成は引き続きユーザー依頼時のみ

## S3・AWS操作の経路

- agentはaws CLI(`aws s3api`等)を実行しない。`aws login`/`aws configure`等の認証構築の案内もしない
- agentがS3に触るのはbackup-remoteのコマンド経由のみ(SDK+`configs/aws.toml`のcredentials_file明示API key。TASK-9構成・credential chain非依存)
- 経緯: 2026-10-13のSDK移行指摘(TASK-9)後も、2026-10-22にagentがaws CLIを実行し`aws login`案内までしたことで再指摘を受け、記載に至った
