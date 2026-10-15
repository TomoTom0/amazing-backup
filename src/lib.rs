//! library crate root(設計5章lib.rs)。module宣言と主要型の再export。

pub mod cli;
pub mod config;
pub mod crypto;
pub mod errors;
pub mod fingerprint;
pub mod index;
pub mod layout;
pub mod manifest;
pub mod registry;
pub mod restore;
pub mod s3;
pub mod storage;
pub mod targets;
pub mod uploader;

pub use config::{AwsConfig, PipelineConfig};
pub use errors::{BackupError, BackupResult};
pub use registry::{EncryptRecord, ManifestUploadRecord, Record, Reduced, UploadRecord};

/// 単体test(src内`#[cfg(test)]`)共用の補助。テスト階層専用で実装には含めない。
#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    static PATH_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// テスト終了時に再帰削除する一時dir(std::env::temp_dir配下・repo外)。
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new(tag: &str) -> TempDir {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backup-remote-unit-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("unit test temp dir create");
            TempDir(path)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// PATH環境変数を書換え、Dropで復元するguard(panic時もunwindで復元)。
    /// 使用側は必ず `path_env_lock()` を先に取得する(並列testとの競合防止)。
    pub(crate) struct PathEnvGuard {
        original: String,
    }

    impl Drop for PathEnvGuard {
        fn drop(&mut self) {
            unsafe { std::env::set_var("PATH", &self.original) };
        }
    }

    /// PATH依存の外部コマンドをspawnするtestとPATHを書換えるtestの直列化lock。
    pub(crate) fn path_env_lock() -> MutexGuard<'static, ()> {
        PATH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn set_path_env(value: &str) -> PathEnvGuard {
        let original = std::env::var("PATH").unwrap_or_default();
        unsafe { std::env::set_var("PATH", value) };
        PathEnvGuard { original }
    }

    pub(crate) fn write_file(path: &Path, data: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir create");
        }
        std::fs::write(path, data).expect("test file write");
    }

    /// 既知のed25519公開鍵(throwaway・指紋 SHA256:559q/vywXFjM/NvGcD0pGur4xs/btSeJYjRe+eJtVew)。
    pub(crate) const TEST_ED25519_PUB: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIM/n+OMp5XIhXxpN7m5TDSj7Px+8UfoaTq1wy4lW1DN1 backup-remote-test-embed";

    /// 上記鍵のssh-keygen -lf表示指紋(FPR-001の独立oracle)。
    pub(crate) const TEST_ED25519_FINGERPRINT: &str =
        "SHA256:559q/vywXFjM/NvGcD0pGur4xs/btSeJYjRe+eJtVew";

    /// 対象外形式検証用のssh-rsa公開鍵(throwaway)。
    pub(crate) const TEST_RSA_PUB: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDXNf/oTBwu/lMRdjWkvqj3cmXhj65nCN9tiAgN05bSFKumoG3/0jMTEbJXi2xddDZLeMmnGS+U2QgF1guBC+z8jIQTHwRPozpySrZq4gKrxfw/Gas6/uZZwRaM4MR7/VNLp7VZI7b/OufrFQd5RvGhT90UnYmJObhe5ZH8UwDh5gZQfUjGe4WM+It9aid2plQ4HKZctMFnTsz4LydGDu1RPgXUWhyBOT7f29uBBE4I85VPCUHT5v55DAQ5JRcViavNKtbhYhJOJAJUu/X9777VAERmaHrHIlZk8WZKgWd6Sfhs0lI2UfjR293egMD8LdEQqOZem8/aBz085grFQd backup-remote-test-rsa";

    /// TEST_ED25519_PUBをfileに書き出しそのpathを返す。
    pub(crate) fn write_test_ed25519_pub(dir: &Path) -> PathBuf {
        let p = dir.join("test_ed25519.pub");
        write_file(&p, format!("{TEST_ED25519_PUB}\n").as_bytes());
        p
    }
}
