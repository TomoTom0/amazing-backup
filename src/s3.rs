//! S3 backend(TASK-9 AWS SDK移行・tmp/tasks/tm-9/design_sdk_migration.md 4〜8章)。
//! `S3Backend` trait(Repo中立型)とその実装`RealS3`(aws-sdk-s3)・`FakeS3`(test・結合seam)。
//! aws CLI(subprocess)呼び出しを全面置換する。非同期runtimeは`RealS3`内部の
//! `block_on`へ局所化し、呼び出し側(uploader/restore/cli・既存test)は同期のまま。

use crate::errors::{BackupError, BackupResult};
use std::path::Path;
#[cfg(debug_assertions)]
use std::path::PathBuf;

/// HeadObjectの取得結果(repo中立型。SDK型をrepo全域へ波及させない)。None=404(Key不在)。
#[derive(Debug, PartialEq, Eq)]
pub struct HeadInfo {
    pub size: i64,
    pub checksum_sha256: Option<String>,
    pub checksum_type: Option<String>,
    /// `Restore`ヘッダのraw文字列(復元job状態。ヘッダなし=None)。
    /// parseはrestore.rs側で行う(3.8)。upload/README配置の既存呼び出しはこの値を見ない。
    pub restore: Option<String>,
}

/// S3操作の抽象(設計4章)。region・credentialはBackend構築時に固定。
/// bucket/storage_classは「CLI一時上書きで実効値が変わり得る」ため呼び出し引数。
pub trait S3Backend {
    /// HeadObject。`Ok(None)`=404(Key不在)。
    fn head(&self, bucket: &str, key: &str) -> BackupResult<Option<HeadInfo>>;
    /// upload方向。storage_classはrepo側allowlist検査済みString。multipart判定は実装側。
    fn put(&self, bucket: &str, key: &str, src: &Path, storage_class: &str)
        -> BackupResult<()>;
    /// download方向。dest(`.part`)へstream書込(呼び出し側が`.part`→rename規則を維持)。
    fn get_to_file(&self, bucket: &str, key: &str, dest: &Path) -> BackupResult<()>;
    /// ListObjectsV2(paginator内包)。key昇順・重複除去済みを返す。
    fn list_keys(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<String>>;
    /// RestoreObject。days/tierはrepo側allowlist検査済み文字列。
    fn restore(&self, bucket: &str, key: &str, days: u32, tier: &str) -> BackupResult<()>;
}

/// multipart開始閾値(設計5.3)。この閾値を超える対象はmultipart upload。
pub const MULTIPART_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;
/// multipart part size(設計5.3。64MiB×10,000parts上限=640GiB)。
pub const PART_SIZE_BYTES: u64 = 64 * 1024 * 1024;
/// S3 multipartのpart数上限(S3仕様)。
const MAX_PARTS: u64 = 10_000;

/// 対象sizeからpart sizeを決める(設計5.3)。640GiB超の到達困難な巨大sizeでは
/// part数上限を守るためceil(size/10000)へ切り上げる(防御)。
pub fn part_size_for(size: u64) -> u64 {
    if size > MAX_PARTS * PART_SIZE_BYTES {
        size.div_ceil(MAX_PARTS).max(PART_SIZE_BYTES)
    } else {
        PART_SIZE_BYTES
    }
}

/// 対象sizeのmultipart part数。
pub fn multipart_part_count(size: u64) -> u64 {
    size.div_ceil(part_size_for(size))
}

/// SDK呼び出しerrorをBackupErrorへ一元写像する(設計8章)。
/// throttle/network/timeout/認証系・その他ServiceErrorはすべて転送エラー5
/// (SDK標準retryが先に効く。Backend構築失敗のみ設定エラー2)。
/// messageには操作名・bucket・key・SDK由来のerror文を含め、credential値・簽署stringは
/// 含めない(SdkErrorのDisplayはrequest metadataを展開しない。設計3.6・8章)。
fn map_sdk_error(op: &str, bucket: &str, key: &str, err: impl std::fmt::Display) -> BackupError {
    BackupError::Transfer(format!("S3 {op}失敗(bucket={bucket}, key={key}): {err}"))
}

/// 送りfile読込系の失敗(旧`aws s3 cp`のcp失敗相当)。
fn src_read_error(op: &str, key: &str, src: &Path, err: impl std::fmt::Display) -> BackupError {
    BackupError::Transfer(format!(
        "S3 {op}失敗(key={key}, src={}): {err}",
        src.display()
    ))
}

// ---------------------------------------------------------------------------
// RealS3(aws-sdk-s3)

/// aws-sdk-s3による実S3 backend(設計2章案1・7章案A)。
/// tokio runtime(multi_thread)を保持し、trait各メソッド内で`block_on`する
/// (main・cmd_*・run_*・testsは同期のまま。`#[tokio::main]`は導入しない)。
pub struct RealS3 {
    client: aws_sdk_s3::Client,
    rt: tokio::runtime::Runtime,
}

impl RealS3 {
    /// 明示API key(credentialファイル)・regionから構築する(起動時1回。設計4.2)。
    /// credential chain(IMDS/env/~/.aws)は使わない(fail-closed・設計3.5)。
    /// runtime生成失敗は設定・環境エラー2(設計7章)。
    pub fn new(creds: &crate::config::AwsCredentials, region: &str) -> BackupResult<RealS3> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| BackupError::Config(format!("tokio runtimeの生成に失敗: {e}")))?;
        let sdk_creds = aws_sdk_s3::config::Credentials::new(
            creds.access_key_id.clone(),
            creds.secret_access_key.clone(),
            creds.session_token.clone(),
            None,
            "backup-remote-credentials-file",
        );
        // connect_timeoutのみ設定(設計8章)。api_call_timeout等の総時間型timeoutは
        // 大object転送を切断するため設定しない。
        let timeout = aws_sdk_s3::config::timeout::TimeoutConfig::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build();
        // behavior versionは設計2章どおりv2023_11_09へ明示固定(latest未指定時の
        // 非推奨warning回避+挙動変化の版管理。backup toolでは安定性優先。
        // 新major versionの登場により当該versionの明示指定はdeprecated扱いとなるが
        // 旧version指定自体はサポート済みであり、意図的な固定である)
        #[allow(deprecated)]
        let behavior_version = aws_sdk_s3::config::BehaviorVersion::v2023_11_09();
        let cfg = aws_sdk_s3::Config::builder()
            .behavior_version(behavior_version)
            .credentials_provider(sdk_creds)
            .region(aws_sdk_s3::config::Region::new(region.to_string()))
            .timeout_config(timeout)
            .build();
        Ok(RealS3 {
            client: aws_sdk_s3::Client::from_conf(cfg),
            rt,
        })
    }

    /// multipart upload(手動実装・設計5.2)。part失敗時はbest-effortでAbortする
    /// (DEEP_ARCHIVEの不完全MPU課金防止。設計5.7)。`CompleteMultipartUpload`失敗時は
    /// abortしない(「実は成功後に500」の場合に成功物を壊すため。再実行でhead skipが冪等解決)。
    async fn put_multipart(
        &self,
        bucket: &str,
        key: &str,
        src: &Path,
        storage_class: &str,
        size: u64,
    ) -> BackupResult<()> {
        let create = self
            .client
            .create_multipart_upload()
            .bucket(bucket)
            .key(key)
            .storage_class(aws_sdk_s3::types::StorageClass::from(storage_class))
            .checksum_algorithm(aws_sdk_s3::types::ChecksumAlgorithm::Sha256)
            .send()
            .await
            .map_err(|err| map_sdk_error("CreateMultipartUpload", bucket, key, err))?;
        let Some(upload_id) = create.upload_id().map(str::to_string) else {
            return Err(BackupError::Transfer(format!(
                "CreateMultipartUpload応答にupload_idがない(bucket={bucket}, key={key})"
            )));
        };
        let part_size = part_size_for(size);
        let mut parts: Vec<aws_sdk_s3::types::CompletedPart> = Vec::new();
        let mut offset: u64 = 0;
        let mut part_number: i32 = 0;
        while offset < size {
            part_number += 1;
            let length = part_size.min(size - offset);
            let body = aws_sdk_s3::primitives::ByteStream::read_from()
                .path(src)
                .offset(offset)
                .length(aws_sdk_s3::primitives::Length::Exact(length))
                .build()
                .await
                .map_err(|e| src_read_error("UploadPart", key, src, e))?;
            match self
                .client
                .upload_part()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .part_number(part_number)
                .checksum_algorithm(aws_sdk_s3::types::ChecksumAlgorithm::Sha256)
                .body(body)
                .send()
                .await
            {
                Ok(resp) => {
                    parts.push(
                        aws_sdk_s3::types::CompletedPart::builder()
                            .part_number(part_number)
                            .e_tag(resp.e_tag().map(str::to_string).unwrap_or_default())
                            .build(),
                    );
                }
                Err(err) => {
                    self.abort_multipart_best_effort(bucket, key, &upload_id).await;
                    return Err(map_sdk_error("UploadPart", bucket, key, err));
                }
            }
            offset += length;
        }
        // Completeへ渡すCompletedPartはpart_number昇順で組む(設計5.2)
        parts.sort_by_key(|p| p.part_number().unwrap_or(0));
        let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(parts))
            .build();
        self.client
            .complete_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .map_err(|err| map_sdk_error("CompleteMultipartUpload", bucket, key, err))?;
        Ok(())
    }

    /// part失敗時のbest-effort abort(失敗してもwarn表示のみ。設計5.7)。
    async fn abort_multipart_best_effort(&self, bucket: &str, key: &str, upload_id: &str) {
        if let Err(err) = self
            .client
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
        {
            eprintln!(
                "WARNING: AbortMultipartUploadに失敗(不完全MPUが残る可能性。bucket lifecycle設定でのAbortIncompleteMultipartUploadを推奨): {err}"
            );
        }
    }
}

impl S3Backend for RealS3 {
    fn head(&self, bucket: &str, key: &str) -> BackupResult<Option<HeadInfo>> {
        self.rt.block_on(async {
            match self
                .client
                .head_object()
                .bucket(bucket)
                .key(key)
                .send()
                .await
            {
                Ok(out) => Ok(Some(HeadInfo {
                    size: out.content_length().unwrap_or(-1),
                    checksum_sha256: out.checksum_sha256().map(str::to_string),
                    checksum_type: out
                        .checksum_type()
                        .map(|t| t.as_str().to_string()),
                    restore: out.restore().map(str::to_string),
                })),
                Err(err) => {
                    // 404(NotFound)=Key不在はOk(None)(現行規則の維持・設計1章・8章)
                    let not_found = err
                        .as_service_error()
                        .is_some_and(aws_sdk_s3::operation::head_object::HeadObjectError::is_not_found);
                    if not_found {
                        Ok(None)
                    } else {
                        Err(map_sdk_error("HeadObject", bucket, key, err))
                    }
                }
            }
        })
    }

    fn put(
        &self,
        bucket: &str,
        key: &str,
        src: &Path,
        storage_class: &str,
    ) -> BackupResult<()> {
        let size = std::fs::metadata(src)
            .map_err(|e| src_read_error("PutObject", key, src, e))?
            .len();
        self.rt.block_on(async {
            if size > MULTIPART_THRESHOLD_BYTES {
                self.put_multipart(bucket, key, src, storage_class, size)
                    .await
            } else {
                let body = aws_sdk_s3::primitives::ByteStream::read_from()
                    .path(src)
                    .build()
                    .await
                    .map_err(|e| src_read_error("PutObject", key, src, e))?;
                self.client
                    .put_object()
                    .bucket(bucket)
                    .key(key)
                    .storage_class(aws_sdk_s3::types::StorageClass::from(storage_class))
                    .checksum_algorithm(aws_sdk_s3::types::ChecksumAlgorithm::Sha256)
                    .body(body)
                    .send()
                    .await
                    .map_err(|err| map_sdk_error("PutObject", bucket, key, err))?;
                Ok(())
            }
        })
    }

    fn get_to_file(&self, bucket: &str, key: &str, dest: &Path) -> BackupResult<()> {
        self.rt.block_on(async {
            let output = match self
                .client
                .get_object()
                .bucket(bucket)
                .key(key)
                .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
                .send()
                .await
            {
                Ok(out) => out,
                Err(err) => {
                    // GetObjectのkey不在はdownload側のkey不在案内を維持(設計8章)
                    let no_such_key = err
                        .as_service_error()
                        .is_some_and(aws_sdk_s3::operation::get_object::GetObjectError::is_no_such_key);
                    if no_such_key {
                        return Err(BackupError::Transfer(format!(
                            "対象keyがS3に存在しない(key={key})"
                        )));
                    }
                    return Err(map_sdk_error("GetObject", bucket, key, err));
                }
            };
            let mut reader = output.body.into_async_read();
            let mut writer = match tokio::fs::File::create(dest).await {
                Ok(w) => w,
                Err(e) => {
                    return Err(BackupError::Transfer(format!(
                        "S3 GetObject失敗(key={key}, dest={}): 取得先fileの作成に失敗: {e}",
                        dest.display()
                    )));
                }
            };
            // GetObjectはsend()成功後の応答body読込中にも失敗しうる(checksum不一致・
            // 転送切断)。読込完了まで監視しstream由来のerrorも転送エラー5へ写像する
            //(設計6章・8章)
            if let Err(e) = tokio::io::copy(&mut reader, &mut writer).await {
                return Err(BackupError::Transfer(format!(
                    "S3 GetObject失敗(key={key}): 応答bodyの読込・書込に失敗: {e}"
                )));
            }
            if let Err(e) = tokio::io::AsyncWriteExt::flush(&mut writer).await {
                return Err(BackupError::Transfer(format!(
                    "S3 GetObject失敗(key={key}): 取得先fileへの書込完了に失敗: {e}"
                )));
            }
            Ok(())
        })
    }

    fn list_keys(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<String>> {
        self.rt.block_on(async {
            let mut keys: Vec<String> = Vec::new();
            let mut paginator = self
                .client
                .list_objects_v2()
                .bucket(bucket)
                .prefix(prefix)
                .into_paginator()
                .send();
            while let Some(page) = paginator.next().await {
                let page = page
                    .map_err(|err| map_sdk_error("ListObjectsV2", bucket, prefix, err))?;
                for obj in page.contents() {
                    if let Some(k) = obj.key() {
                        keys.push(k.to_string());
                    }
                }
            }
            keys.sort();
            keys.dedup();
            Ok(keys)
        })
    }

    fn restore(&self, bucket: &str, key: &str, days: u32, tier: &str) -> BackupResult<()> {
        self.rt.block_on(async {
            // tierはrepo側allowlist(Standard/Bulk)検査済み文字列から変換(設計6章)
            let glacier = aws_sdk_s3::types::GlacierJobParameters::builder()
                .tier(aws_sdk_s3::types::Tier::from(tier))
                .build()
                .map_err(|e| {
                    BackupError::Config(format!("RestoreRequestの組立に失敗(tier={tier}): {e}"))
                })?;
            let request = aws_sdk_s3::types::RestoreRequest::builder()
                .days(days as i32)
                .glacier_job_parameters(glacier)
                .build();
            self.client
                .restore_object()
                .bucket(bucket)
                .key(key)
                .restore_request(request)
                .send()
                .await
                .map_err(|err| map_sdk_error("RestoreObject", bucket, key, err))?;
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// FakeS3(unit test・結合seam用。gateは#[cfg(debug_assertions)]でrelease buildから排除)

/// 状態を持つS3 simulator(設計4.3案1・10.3)。unit testと結合testのseam
/// (env `BACKUP_REMOTE_FAKE_S3_STATE`)で共用する。実network不使用。
///
/// state dir配下の状態構造(旧fake aws scriptと同一・tests/common/mod.rs doc参照):
/// - `objects/<key>`   配置済みobjectの実体
/// - `restore/<key>`   復元job状態のmeta("ongoing"=進行中・それ以外=復元済み)
/// - `calls.log`       API呼び出し記録(1呼び出し1行・設計10.1のAPI形式)
///
/// 失敗注入(結合seamではenvから解釈・unit testではbuilder methodで直接指定):
/// - `FAKE_S3_FAIL_CP_FOR`      当該keyのput(PutObject・UploadPart)・getのみ失敗
/// - `FAKE_S3_FAIL_RESTORE_FOR` 当該keyのRestoreObjectのみ失敗
/// - `FAKE_S3_FAIL_HEAD_FOR`    当該keyのHeadObjectのみ異常失敗(404以外)
/// - `FAKE_S3_HEAD_404_FOR`     当該keyのHeadObjectのみ404扱い(object実体は保持)
#[cfg(debug_assertions)]
pub struct FakeS3 {
    state_dir: PathBuf,
    fail_cp: Option<String>,
    fail_restore: Option<String>,
    fail_head: Option<String>,
    head_404: Option<String>,
    omit_checksum: bool,
}

#[cfg(debug_assertions)]
impl FakeS3 {
    /// 失敗注入なしのfake(state dirのみ指定)。
    pub fn new(state_dir: impl Into<PathBuf>) -> FakeS3 {
        FakeS3 {
            state_dir: state_dir.into(),
            fail_cp: None,
            fail_restore: None,
            fail_head: None,
            head_404: None,
            omit_checksum: false,
        }
    }

    /// 結合seam用: env `BACKUP_REMOTE_FAKE_S3_STATE`選択時に失敗注入env 4種を解釈する。
    pub fn from_env(state_dir: impl Into<PathBuf>) -> FakeS3 {
        let env_key = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
        };
        FakeS3 {
            state_dir: state_dir.into(),
            fail_cp: env_key("FAKE_S3_FAIL_CP_FOR"),
            fail_restore: env_key("FAKE_S3_FAIL_RESTORE_FOR"),
            fail_head: env_key("FAKE_S3_FAIL_HEAD_FOR"),
            head_404: env_key("FAKE_S3_HEAD_404_FOR"),
            omit_checksum: false,
        }
    }

    /// 当該keyのput・getを失敗させる(FAKE_S3_FAIL_CP_FOR相当)。
    pub fn fail_cp(mut self, key: &str) -> FakeS3 {
        self.fail_cp = Some(key.to_string());
        self
    }

    /// 当該keyのRestoreObjectを失敗させる(FAKE_S3_FAIL_RESTORE_FOR相当)。
    pub fn fail_restore(mut self, key: &str) -> FakeS3 {
        self.fail_restore = Some(key.to_string());
        self
    }

    /// 当該keyのHeadObjectを異常失敗させる(FAKE_S3_FAIL_HEAD_FOR相当)。
    pub fn fail_head(mut self, key: &str) -> FakeS3 {
        self.fail_head = Some(key.to_string());
        self
    }

    /// 当該keyのHeadObjectを404扱いにする(object実体は保持。FAKE_S3_HEAD_404_FOR相当)。
    pub fn head_404(mut self, key: &str) -> FakeS3 {
        self.head_404 = Some(key.to_string());
        self
    }

    /// HeadObject応答からChecksumSHA256を省く(UPL-014検証用)。
    pub fn omitting_checksum(mut self) -> FakeS3 {
        self.omit_checksum = true;
        self
    }

    /// API呼び出し記録(calls.log。未実行なら空文字列)。
    pub fn calls(&self) -> String {
        std::fs::read_to_string(self.state_dir.join("calls.log")).unwrap_or_default()
    }

    /// 配置済みobjectの実体(objects/<key>。不在None)。
    pub fn object(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.state_dir.join("objects").join(key)).ok()
    }

    /// 呼び出し記録へ1行appendする。
    fn record(&self, line: &str) {
        use std::io::Write as _;
        if let Some(parent) = self.state_dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::create_dir_all(&self.state_dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state_dir.join("calls.log"))
        {
            let _ = writeln!(f, "{line}");
        }
    }

    fn objects_dir(&self) -> PathBuf {
        self.state_dir.join("objects")
    }

    fn object_path(&self, key: &str) -> PathBuf {
        self.objects_dir().join(key)
    }

    fn write_object(&self, key: &str, data: &[u8]) {
        let path = self.object_path(key);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, data);
    }

    /// HeadObject応答の`Restore`ヘッダraw文字列(restore/<key>の状態から)。
    fn restore_header(&self, key: &str) -> Option<String> {
        let meta = std::fs::read_to_string(self.state_dir.join("restore").join(key)).ok()?;
        if meta.trim() == "ongoing" {
            Some(r#"ongoing-request="true""#.to_string())
        } else {
            // 復元済み(それ以外の値)。expiry-dateは固定文(原文保持・時刻解釈なし)
            Some(
                r#"ongoing-request="false", expiry-date="Wed, 14 Oct 2026 00:00:00 GMT""#
                    .to_string(),
            )
        }
    }

    fn sha256_b64(data: &[u8]) -> String {
        use base64::Engine as _;
        use sha2::Digest;
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(data))
    }
}

#[cfg(debug_assertions)]
impl S3Backend for FakeS3 {
    fn head(&self, bucket: &str, key: &str) -> BackupResult<Option<HeadInfo>> {
        self.record(&format!("s3 HeadObject bucket={bucket} key={key}"));
        if self.fail_head.as_deref() == Some(key) {
            return Err(BackupError::Transfer(format!(
                "S3 HeadObject失敗(bucket={bucket}, key={key}): FAKE_S3_FAIL_HEAD_FOR による失敗注入"
            )));
        }
        if self.head_404.as_deref() == Some(key) {
            return Ok(None);
        }
        let bytes = match std::fs::read(self.object_path(key)) {
            Ok(b) => b,
            Err(_) => return Ok(None), // 404 = Key不在
        };
        Ok(Some(HeadInfo {
            size: bytes.len() as i64,
            checksum_sha256: if self.omit_checksum {
                None
            } else {
                Some(Self::sha256_b64(&bytes))
            },
            checksum_type: Some("FULL_OBJECT".to_string()),
            restore: self.restore_header(key),
        }))
    }

    fn put(
        &self,
        bucket: &str,
        key: &str,
        src: &Path,
        storage_class: &str,
    ) -> BackupResult<()> {
        let fail = || {
            BackupError::Transfer(format!(
                "S3 PutObject失敗(bucket={bucket}, key={key}): FAKE_S3_FAIL_CP_FOR による失敗注入"
            ))
        };
        let size = std::fs::metadata(src)
            .map_err(|e| src_read_error("PutObject", key, src, e))?
            .len();
        if size > MULTIPART_THRESHOLD_BYTES {
            // multipart(Create→UploadPart×N→Complete。part失敗時Abort)
            self.record(&format!(
                "s3 CreateMultipartUpload bucket={bucket} key={key} storage_class={storage_class} checksum=SHA256"
            ));
            let data = std::fs::read(src)
                .map_err(|e| src_read_error("UploadPart", key, src, e))?;
            for part_number in 1..=multipart_part_count(size) {
                self.record(&format!(
                    "s3 UploadPart bucket={bucket} key={key} part_number={part_number} checksum=SHA256"
                ));
                if self.fail_cp.as_deref() == Some(key) {
                    // part失敗 → Abort発火(Completeは実行しない)
                    self.record(&format!("s3 AbortMultipartUpload bucket={bucket} key={key}"));
                    return Err(fail());
                }
            }
            self.record(&format!("s3 CompleteMultipartUpload bucket={bucket} key={key}"));
            self.write_object(key, &data);
            Ok(())
        } else {
            self.record(&format!(
                "s3 PutObject bucket={bucket} key={key} src={} storage_class={storage_class} checksum=SHA256",
                src.display()
            ));
            if self.fail_cp.as_deref() == Some(key) {
                return Err(fail());
            }
            let data = std::fs::read(src)
                .map_err(|e| src_read_error("PutObject", key, src, e))?;
            self.write_object(key, &data);
            Ok(())
        }
    }

    fn get_to_file(&self, bucket: &str, key: &str, dest: &Path) -> BackupResult<()> {
        self.record(&format!(
            "s3 GetObject bucket={bucket} key={key} dest={} checksum_mode=ENABLED",
            dest.display()
        ));
        if self.fail_cp.as_deref() == Some(key) {
            return Err(BackupError::Transfer(format!(
                "S3 GetObject失敗(bucket={bucket}, key={key}): FAKE_S3_FAIL_CP_FOR による失敗注入"
            )));
        }
        let bytes = match std::fs::read(self.object_path(key)) {
            Ok(b) => b,
            Err(_) => {
                return Err(BackupError::Transfer(format!(
                    "対象keyがS3に存在しない(key={key})"
                )));
            }
        };
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(dest, &bytes).map_err(|e| {
            BackupError::Transfer(format!(
                "S3 GetObject失敗(key={key}, dest={}): 取得先fileの書込に失敗: {e}",
                dest.display()
            ))
        })
    }

    fn list_keys(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<String>> {
        self.record(&format!(
            "s3 ListObjectsV2 bucket={bucket} prefix={prefix}"
        ));
        let mut keys = Vec::new();
        let objects = self.objects_dir();
        fn walk(dir: &Path, base: &Path, keys: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, base, keys);
                } else if let Ok(rel) = path.strip_prefix(base) {
                    keys.push(rel.to_string_lossy().into_owned());
                }
            }
        }
        walk(&objects, &objects, &mut keys);
        keys.retain(|k| k.starts_with(prefix));
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    fn restore(&self, bucket: &str, key: &str, days: u32, tier: &str) -> BackupResult<()> {
        self.record(&format!(
            "s3 RestoreObject bucket={bucket} key={key} days={days} tier={tier}"
        ));
        if self.fail_restore.as_deref() == Some(key) {
            return Err(BackupError::Transfer(format!(
                "S3 RestoreObject失敗(bucket={bucket}, key={key}): FAKE_S3_FAIL_RESTORE_FOR による失敗注入"
            )));
        }
        // 発行成功で復元済み状態にする(request_all後のdownload成功前提・tests/common)
        let path = self.state_dir.join("restore").join(key);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, b"restored").map_err(|e| {
            BackupError::Transfer(format!(
                "S3 RestoreObject失敗(bucket={bucket}, key={key}): 復元状態の記録に失敗: {e}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{write_file, TempDir};

    fn sha256_b64(data: &[u8]) -> String {
        use base64::Engine as _;
        use sha2::Digest;
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(data))
    }

    /// [covers:upload.multipart-threshold]
    /// multipart閾値・part数の単位確認(結合側のtestはtests/s3_backend.rsが担う):
    /// 64MiBちょうどは単発・64MiB+1byteはmultipart 2 parts。part sizeは640GiB超でのみ
    /// 切り上げられる(到達困難のため境界値の計算のみ検証)。
    #[test]
    fn multipart_threshold_and_part_count() {
        assert_eq!(MULTIPART_THRESHOLD_BYTES, 64 * 1024 * 1024);
        assert_eq!(part_size_for(1), PART_SIZE_BYTES);
        assert_eq!(part_size_for(MULTIPART_THRESHOLD_BYTES), PART_SIZE_BYTES);
        // 閾値超はmultipart(part数=ceil(size/64MiB))
        assert_eq!(multipart_part_count(MULTIPART_THRESHOLD_BYTES + 1), 2);
        assert_eq!(
            multipart_part_count(MULTIPART_THRESHOLD_BYTES * 3),
            3
        );
        // 640GiB超ではceil(size/10000)へ切り上げ(10,000 parts上限の防御)
        let huge = MAX_PARTS * PART_SIZE_BYTES + 1;
        let ps = part_size_for(huge);
        assert!(ps > PART_SIZE_BYTES, "part size切り上げ");
        assert!(huge.div_ceil(ps) <= MAX_PARTS, "part数は上限内");
    }

    /// [covers:cli.fake-s3-seam]
    /// FakeS3の状態構造の単位確認(put→head・restore→head Restore header・list)。
    /// 呼び出し記録行は設計10.1のAPI形式。
    #[test]
    fn fake_s3_records_calls_and_state() {
        let tmp = TempDir::new("s3-fake");
        let state = tmp.path().join("s3state");
        let fake = FakeS3::new(&state);
        let local = tmp.path().join("a.zip.age");
        write_file(&local, b"age-bytes");

        assert_eq!(fake.head("b", "2026-10-01/a.zip.age").expect("head"), None, "未配置は404");
        fake.put("b", "2026-10-01/a.zip.age", &local, "DEEP_ARCHIVE")
            .expect("put");
        let head = fake.head("b", "2026-10-01/a.zip.age")
            .expect("head")
            .expect("配置済み");
        assert_eq!(head.size, b"age-bytes".len() as i64);
        assert_eq!(head.checksum_sha256.as_deref(), Some(sha256_b64(b"age-bytes").as_str()));
        assert_eq!(head.checksum_type.as_deref(), Some("FULL_OBJECT"));
        assert_eq!(head.restore, None, "復元job未発行");

        fake.restore("b", "2026-10-01/a.zip.age", 3, "Bulk").expect("restore");
        let head = fake.head("b", "2026-10-01/a.zip.age").expect("head").expect("配置済み");
        assert_eq!(
            head.restore.as_deref(),
            Some(r#"ongoing-request="false", expiry-date="Wed, 14 Oct 2026 00:00:00 GMT""#),
            "restore発行済み(復元済み状態)"
        );

        let keys = fake.list_keys("b", "2026-10-01/").expect("list");
        assert_eq!(keys, vec!["2026-10-01/a.zip.age".to_string()]);

        let calls = fake.calls();
        assert!(calls.contains("s3 PutObject bucket=b key=2026-10-01/a.zip.age"), "{calls}");
        assert!(calls.contains("s3 RestoreObject bucket=b key=2026-10-01/a.zip.age days=3 tier=Bulk"), "{calls}");
        assert!(calls.contains("s3 ListObjectsV2 bucket=b prefix=2026-10-01/"), "{calls}");
        assert!(calls.contains("s3 HeadObject bucket=b key=2026-10-01/a.zip.age"), "{calls}");
    }

    /// [covers:cli.fake-s3-seam]
    /// FakeS3の失敗注入4種とget_to_file・list_keysの除外動作の単位確認。
    #[test]
    fn fake_s3_failure_injections() {
        let tmp = TempDir::new("s3-fakefail");
        let state = tmp.path().join("s3state");
        let local = tmp.path().join("a.zip.age");
        write_file(&local, b"age-bytes");
        let key = "2026-10-01/a.zip.age";

        // put失敗注入(転送エラー5)
        let fake = FakeS3::new(&state).fail_cp(key);
        assert_eq!(
            fake.put("b", key, &local, "DEEP_ARCHIVE").unwrap_err().exit_code(),
            5
        );
        // restore失敗注入
        let fake = FakeS3::new(&state).fail_restore(key);
        assert_eq!(fake.restore("b", key, 7, "Standard").unwrap_err().exit_code(), 5);
        // head異常失敗注入(404以外)
        let fake = FakeS3::new(&state).fail_head(key);
        assert_eq!(fake.head("b", key).unwrap_err().exit_code(), 5);
        // head 404注入(404扱い・object実体は保持)
        let fake = FakeS3::new(&state);
        fake.put("b", key, &local, "DEEP_ARCHIVE").expect("put");
        let fake404 = FakeS3::new(&state).head_404(key);
        assert_eq!(fake404.head("b", key).expect("head"), None);
        assert!(fake404.object(key).is_some(), "object実体は保持");

        // get_to_file: 正常取得とkey不在
        let fake = FakeS3::new(&state);
        let dest = tmp.path().join("dl").join("a.zip.age.part");
        fake.get_to_file("b", key, &dest).expect("get");
        assert_eq!(std::fs::read(&dest).unwrap(), b"age-bytes".to_vec());
        let e = fake.get_to_file("b", "2026-10-01/missing.age", &dest).unwrap_err();
        assert_eq!(e.exit_code(), 5, "key不在は転送エラー5: {e}");
        // list_keysはprefix外を除外
        let keys = fake.list_keys("b", "2099-01-01/").expect("list");
        assert!(keys.is_empty(), "prefix外は除外: {keys:?}");
    }

    /// [covers:config.credentials-fail-closed]
    /// RealS3構築はcredential値そのものを要求する(構築後の呼び出しでcredential
    /// chainへfallbackしない構造)。ここでは明示API keyで構築できることのみ検証する
    /// (実network接続は行わない = S3Backendの呼び出しをしない)。
    #[test]
    fn real_s3_constructs_from_explicit_credentials() {
        let creds = crate::config::AwsCredentials {
            access_key_id: "AKIAFAKEUNITTEST00000A".to_string(),
            secret_access_key: "fake-unit-test-not-a-real-secret".to_string(),
            session_token: None,
        };
        let real = RealS3::new(&creds, "ap-northeast-1").expect("RealS3構築");
        let _ = real;
    }
}
