//! エラーenumとexit code対応(設計5章errors.rs・7.1終了コード表)。

/// crate内で一元化するResult型(設計5章)。
pub type BackupResult<T> = Result<T, BackupError>;

/// エラー区分。7.1の終了コード表が唯一の対応表。
#[derive(Debug)]
pub enum BackupError {
    /// 予期しない内部エラー(bug)。exit_code = 1
    Internal(String),
    /// 設定・環境エラー(設定file・age/awsバイナリ不在)。exit_code = 2
    Config(String),
    /// 対象エラー(不在・拡張子・重複)。exit_code = 3
    Target(String),
    /// 暗号化・検証の処理失敗。exit_code = 4
    Process(String),
    /// aws転送失敗。exit_code = 5
    Transfer(String),
    /// CLI使用法誤り。exit_code = 64
    Usage(String),
}

impl BackupError {
    /// 7.1終了コード表への対応(唯一の対応表)。
    pub fn exit_code(&self) -> i32 {
        match self {
            BackupError::Internal(_) => 1,
            BackupError::Config(_) => 2,
            BackupError::Target(_) => 3,
            BackupError::Process(_) => 4,
            BackupError::Transfer(_) => 5,
            BackupError::Usage(_) => 64,
        }
    }
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::Internal(m) => write!(f, "内部エラー: {m}"),
            BackupError::Config(m) => write!(f, "設定・環境エラー: {m}"),
            BackupError::Target(m) => write!(f, "対象エラー: {m}"),
            BackupError::Process(m) => write!(f, "処理エラー: {m}"),
            BackupError::Transfer(m) => write!(f, "転送エラー: {m}"),
            BackupError::Usage(m) => write!(f, "使用法エラー: {m}"),
        }
    }
}

impl std::error::Error for BackupError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// [covers:cli.exit-code-mapping]
    /// 7.1仕様表どおりのexit code対応(区分×codeの全行)。
    /// 代表ケース経由の検証は cli.rs の usage/exit-code 系テストと uploader.rs のテストが担う。
    #[test]
    fn exit_code_matches_spec_table() {
        assert_eq!(BackupError::Internal("x".into()).exit_code(), 1);
        assert_eq!(BackupError::Config("x".into()).exit_code(), 2);
        assert_eq!(BackupError::Target("x".into()).exit_code(), 3);
        assert_eq!(BackupError::Process("x".into()).exit_code(), 4);
        assert_eq!(BackupError::Transfer("x".into()).exit_code(), 5);
        assert_eq!(BackupError::Usage("x".into()).exit_code(), 64);
    }

    #[test]
    fn display_includes_message() {
        let e = BackupError::Transfer("upload failed: boom".into());
        let s = format!("{e}");
        assert!(s.contains("upload failed: boom"), "display: {s}");
    }
}
