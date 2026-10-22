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
    /// 対象object(版)のstorage class(TASK-10: DA系は復元job要否の判定に使う。
    /// fakeはPUT時に記録した値・seed由来など不明はNone=非archive扱い)
    pub storage_class: Option<String>,
}

/// versioning有効bucketでの版情報(ListObjectVersions応答のrepo中立型)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    pub key: String,
    pub version_id: String,
    /// ISO8601相当の文字列(実装由来の表現をそのまま)
    pub last_modified: String,
    /// last_modifiedのepoch秒(版順序のsortはこの数値で行う。文字列sortは
    /// 実装表記差で崩れ得るためL-2)
    pub last_modified_epoch: i64,
    pub size: i64,
    /// 現行versionか
    pub is_latest: bool,
    /// delete markerか
    pub is_delete_marker: bool,
}

/// lifecycle ruleのrepo中立表現(TASK-18 7章。`manage lifecycle apply`が構成:
/// data/一括の3rule=NCVE+MPU abort+DeleteMarkerSweep)。
/// 削除条件はNCVEとpruneの単一定義。STANDARD→DA移行ruleは構成しない。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LifecycleSpec {
    /// data/一括のNoncurrentVersionExpiration(prefix `<prefix>data/`。
    /// 予約領域(sys)・metadataの非現行版も同prefix配下で掃除される)
    Noncurrent {
        prefix: String,
        /// noncurrent版が削除可能になるまでの日数(180=Deep Archive最小保存期間)
        noncurrent_days: i32,
        /// 残すnoncurrent版数(NewerNoncurrentVersions・[aws.lifecycle] keep_versions)
        newer_noncurrent_versions: i32,
    },
    /// AbortIncompleteMultipartUpload(不完全MPU掃除・prefix `<prefix>/`)
    MpuAbort {
        prefix: String,
        days_after_initiation: i32,
    },
    /// Expiration ExpiredObjectDeleteMarker(prefix `<prefix>data/`。
    /// title変更時の旧metadataキーdelete marker・単独delete markerの掃除)
    DeleteMarkerSweep {
        prefix: String,
        /// Expiration.ExpiredObjectDeleteMarker(常にtrue)
        expired_object_delete_marker: bool,
    },
}

/// S3操作の抽象(設計4章・TASK-10 8.1拡張)。region・credentialはBackend構築時に固定。
/// bucket/storage_classは「CLI一時上載せで実効値が変わり得る」ため呼び出し引数。
pub trait S3Backend {
    /// HeadObject。`Ok(None)`=404(Key不在)。
    fn head(&self, bucket: &str, key: &str) -> BackupResult<Option<HeadInfo>>;
    /// upload方向。storage_classはrepo側allowlist検査済みString。multipart判定は実装側。
    /// 戻り値は当該uploadのVersionId(versioning有効bucket前提。TASK-10 8.1)。
    fn put(&self, bucket: &str, key: &str, src: &Path, storage_class: &str)
    -> BackupResult<String>;
    /// download方向。dest(`.part`)へstream書込(呼び出し側が`.part`→rename規則を維持)。
    fn get_to_file(&self, bucket: &str, key: &str, dest: &Path) -> BackupResult<()>;
    /// download方向の不在許容版。`Ok(false)`=404(Key不在)・他の失敗はエラー伝播。
    /// head→get二段のAPI呼び出し増を避けるため、不在を許容する読込で使用する。
    fn get_to_file_checked(&self, bucket: &str, key: &str, dest: &Path) -> BackupResult<bool>;
    /// ListObjectsV2(paginator内包)。key昇順・重複除去済みを返す。
    fn list_keys(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<String>>;
    /// RestoreObject。days/tierはrepo側allowlist検査済み文字列。
    fn restore(&self, bucket: &str, key: &str, days: u32, tier: &str) -> BackupResult<()>;
    /// HeadObjectのversion指定版(現行版はversion_idに現行vidを渡す)。
    fn head_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> BackupResult<Option<HeadInfo>>;
    /// GetObjectのversion指定版。
    fn get_version_to_file(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
        dest: &Path,
    ) -> BackupResult<()>;
    /// ListObjectVersions(paginator内包。key昇順・同じkey内では新しい版が先)。
    fn list_versions(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<VersionInfo>>;
    /// RestoreObjectのversion指定版。
    fn restore_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
        days: u32,
        tier: &str,
    ) -> BackupResult<()>;
    /// CopyObject(後付けpin写し)。5GiB超は実装側でUploadPartCopyへ分岐する。
    fn copy_object(
        &self,
        bucket: &str,
        src_key: &str,
        src_version_id: Option<&str>,
        dest_key: &str,
        storage_class: &str,
    ) -> BackupResult<()>;
    /// DeleteObject(versioning bucketではdelete markerを作る)。
    fn delete_object(&self, bucket: &str, key: &str) -> BackupResult<()>;
    /// DeleteObjectVersion(当該版を削除。manage pruneが使う)。
    fn delete_object_version(&self, bucket: &str, key: &str, version_id: &str) -> BackupResult<()>;
    /// GetBucketVersioning(Enabled=true。backup pushの事前確認)。
    fn get_bucket_versioning(&self, bucket: &str) -> BackupResult<bool>;
    /// PutBucketLifecycleConfiguration(rule一式の置換・manage lifecycle apply)。
    fn put_lifecycle(&self, bucket: &str, rules: &[LifecycleSpec]) -> BackupResult<()>;
    /// GetBucketLifecycleConfiguration(未設定None)。
    fn get_lifecycle(&self, bucket: &str) -> BackupResult<Option<Vec<LifecycleSpec>>>;
}

/// multipart開始閾値(設計5.3)。この閾値を超える対象はmultipart upload。
pub const MULTIPART_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;
/// CopyObjectの1回上限(S3仕様。超過はUploadPartCopyで分割・設計1.3)
pub const COPY_OBJECT_MAX_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// S3 keyのpath部をcopy_source用にpercent-encodeする('/'は区切りとして保持し、
/// unreserved文字[A-Za-z0-9-._~]以外を%XXへ)
fn encode_s3_path(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for byte in key.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
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
    ) -> BackupResult<String> {
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
                    self.abort_multipart_best_effort(bucket, key, &upload_id)
                        .await;
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
        let resp = self
            .client
            .complete_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .map_err(|err| map_sdk_error("CompleteMultipartUpload", bucket, key, err))?;
        // Complete応答のVersionId(versioning有効bucket。無い場合は空文字)
        Ok(resp.version_id().map(str::to_string).unwrap_or_default())
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
                .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
                .send()
                .await
            {
                Ok(out) => Ok(Some(HeadInfo {
                    size: out.content_length().unwrap_or(-1),
                    checksum_sha256: out.checksum_sha256().map(str::to_string),
                    checksum_type: out.checksum_type().map(|t| t.as_str().to_string()),
                    restore: out.restore().map(str::to_string),
                    storage_class: out.storage_class().map(|s| s.as_str().to_string()),
                })),
                Err(err) => {
                    // 404(NotFound)=Key不在はOk(None)(現行規則の維持・設計1章・8章)
                    let not_found = err.as_service_error().is_some_and(
                        aws_sdk_s3::operation::head_object::HeadObjectError::is_not_found,
                    );
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
    ) -> BackupResult<String> {
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
                let resp = self
                    .client
                    .put_object()
                    .bucket(bucket)
                    .key(key)
                    .storage_class(aws_sdk_s3::types::StorageClass::from(storage_class))
                    .checksum_algorithm(aws_sdk_s3::types::ChecksumAlgorithm::Sha256)
                    .body(body)
                    .send()
                    .await
                    .map_err(|err| map_sdk_error("PutObject", bucket, key, err))?;
                // PutObject応答のVersionId(versioning有効bucket。無い場合は空文字)
                Ok(resp.version_id().map(str::to_string).unwrap_or_default())
            }
        })
    }

    fn get_to_file(&self, bucket: &str, key: &str, dest: &Path) -> BackupResult<()> {
        if self.get_to_file_checked(bucket, key, dest)? {
            Ok(())
        } else {
            // GetObjectのkey不在はdownload側のkey不在案内を維持(設計8章)
            Err(BackupError::Transfer(format!(
                "対象keyがS3に存在しない(key={key})"
            )))
        }
    }

    fn get_to_file_checked(&self, bucket: &str, key: &str, dest: &Path) -> BackupResult<bool> {
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
                    let no_such_key = err.as_service_error().is_some_and(
                        aws_sdk_s3::operation::get_object::GetObjectError::is_no_such_key,
                    );
                    if no_such_key {
                        return Ok(false);
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
            Ok(true)
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
                let page =
                    page.map_err(|err| map_sdk_error("ListObjectsV2", bucket, prefix, err))?;
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

    fn head_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> BackupResult<Option<HeadInfo>> {
        self.rt.block_on(async {
            match self
                .client
                .head_object()
                .bucket(bucket)
                .key(key)
                .version_id(version_id)
                .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
                .send()
                .await
            {
                Ok(out) => Ok(Some(HeadInfo {
                    size: out.content_length().unwrap_or(-1),
                    checksum_sha256: out.checksum_sha256().map(str::to_string),
                    checksum_type: out.checksum_type().map(|t| t.as_str().to_string()),
                    restore: out.restore().map(str::to_string),
                    storage_class: out.storage_class().map(|s| s.as_str().to_string()),
                })),
                Err(err) => {
                    let not_found = err.as_service_error().is_some_and(
                        aws_sdk_s3::operation::head_object::HeadObjectError::is_not_found,
                    );
                    if not_found {
                        Ok(None)
                    } else {
                        Err(map_sdk_error("HeadObject", bucket, key, err))
                    }
                }
            }
        })
    }

    fn get_version_to_file(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
        dest: &Path,
    ) -> BackupResult<()> {
        self.rt.block_on(async {
            let output = match self
                .client
                .get_object()
                .bucket(bucket)
                .key(key)
                .version_id(version_id)
                .checksum_mode(aws_sdk_s3::types::ChecksumMode::Enabled)
                .send()
                .await
            {
                Ok(out) => out,
                Err(err) => {
                    let no_such_key = err.as_service_error().is_some_and(
                        aws_sdk_s3::operation::get_object::GetObjectError::is_no_such_key,
                    );
                    if no_such_key {
                        return Err(BackupError::Transfer(format!(
                            "対象keyがS3に存在しない(key={key}, version_id={version_id})"
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

    fn list_versions(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<VersionInfo>> {
        self.rt.block_on(async {
            let mut versions: Vec<VersionInfo> = Vec::new();
            // このSDK版のlist_object_versionsにはpaginatorがないためmarker手動loop
            let mut key_marker: Option<String> = None;
            let mut version_id_marker: Option<String> = None;
            loop {
                let mut req = self
                    .client
                    .list_object_versions()
                    .bucket(bucket)
                    .prefix(prefix);
                if let Some(km) = &key_marker {
                    req = req.key_marker(km);
                }
                if let Some(vm) = &version_id_marker {
                    req = req.version_id_marker(vm);
                }
                let page = req
                    .send()
                    .await
                    .map_err(|err| map_sdk_error("ListObjectVersions", bucket, prefix, err))?;
                for v in page.versions() {
                    versions.push(VersionInfo {
                        key: v.key().map(str::to_string).unwrap_or_default(),
                        version_id: v.version_id().map(str::to_string).unwrap_or_default(),
                        last_modified: v.last_modified().map(|t| t.to_string()).unwrap_or_default(),
                        last_modified_epoch: v.last_modified().map(|t| t.secs()).unwrap_or(0),
                        size: v.size().unwrap_or(-1),
                        is_latest: v.is_latest().unwrap_or(false),
                        is_delete_marker: false,
                    });
                }
                for m in page.delete_markers() {
                    versions.push(VersionInfo {
                        key: m.key().map(str::to_string).unwrap_or_default(),
                        version_id: m.version_id().map(str::to_string).unwrap_or_default(),
                        last_modified: m.last_modified().map(|t| t.to_string()).unwrap_or_default(),
                        last_modified_epoch: m.last_modified().map(|t| t.secs()).unwrap_or(0),
                        size: 0,
                        is_latest: m.is_latest().unwrap_or(false),
                        is_delete_marker: true,
                    });
                }
                if page.is_truncated().unwrap_or(false) {
                    key_marker = page.next_key_marker().map(str::to_string);
                    version_id_marker = page.next_version_id_marker().map(str::to_string);
                    if key_marker.is_none() {
                        break;
                    }
                } else {
                    break;
                }
            }
            // key昇順・同じkey内では新しい版が先(epoch秒降順)
            versions.sort_by(|a, b| {
                a.key
                    .cmp(&b.key)
                    .then(b.last_modified_epoch.cmp(&a.last_modified_epoch))
                    .then(b.version_id.cmp(&a.version_id))
            });
            Ok(versions)
        })
    }

    fn restore_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
        days: u32,
        tier: &str,
    ) -> BackupResult<()> {
        self.rt.block_on(async {
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
                .version_id(version_id)
                .restore_request(request)
                .send()
                .await
                .map_err(|err| map_sdk_error("RestoreObject", bucket, key, err))?;
            Ok(())
        })
    }

    fn copy_object(
        &self,
        bucket: &str,
        src_key: &str,
        src_version_id: Option<&str>,
        dest_key: &str,
        storage_class: &str,
    ) -> BackupResult<()> {
        // 写し元のsizeでCopyObject(5GiB上限)かUploadPartCopyかを分岐(設計1.3)
        let head = match src_version_id {
            Some(vid) => self.head_version(bucket, src_key, vid)?,
            None => self.head(bucket, src_key)?,
        };
        let Some(head) = head else {
            return Err(BackupError::Transfer(format!(
                "CopyObjectの写し元がS3に存在しない(key={src_key})"
            )));
        };
        let size = head.size.max(0) as u64;
        // copy_sourceはURL形式のためkey部をpercent-encodeする(M-5: 日本語・空白等の
        // 非unreserved文字を含むkeyで実S3のCopyObjectが失敗するのを防ぐ)
        let encoded_key = encode_s3_path(src_key);
        let copy_source = match src_version_id {
            Some(vid) => format!("{bucket}/{encoded_key}?versionId={vid}"),
            None => format!("{bucket}/{encoded_key}"),
        };
        self.rt.block_on(async move {
            if size <= COPY_OBJECT_MAX_BYTES {
                self.client
                    .copy_object()
                    .bucket(bucket)
                    .key(dest_key)
                    .copy_source(&copy_source)
                    .storage_class(aws_sdk_s3::types::StorageClass::from(storage_class))
                    .send()
                    .await
                    .map_err(|err| map_sdk_error("CopyObject", bucket, dest_key, err))?;
                return Ok(());
            }
            // 5GiB超: UploadPartCopyで分割写し(part sizeは既存規則に従う)
            let create = self
                .client
                .create_multipart_upload()
                .bucket(bucket)
                .key(dest_key)
                .storage_class(aws_sdk_s3::types::StorageClass::from(storage_class))
                .send()
                .await
                .map_err(|err| map_sdk_error("CreateMultipartUpload", bucket, dest_key, err))?;
            let Some(upload_id) = create.upload_id().map(str::to_string) else {
                return Err(BackupError::Transfer(format!(
                    "CreateMultipartUpload応答にupload_idがない(bucket={bucket}, key={dest_key})"
                )));
            };
            let part_size = part_size_for(size);
            let mut parts: Vec<aws_sdk_s3::types::CompletedPart> = Vec::new();
            let mut offset: u64 = 0;
            let mut part_number: i32 = 0;
            while offset < size {
                part_number += 1;
                let length = part_size.min(size - offset).min(COPY_OBJECT_MAX_BYTES);
                let range = format!("bytes={offset}-{}", offset + length.saturating_sub(1));
                let resp = self
                    .client
                    .upload_part_copy()
                    .bucket(bucket)
                    .key(dest_key)
                    .upload_id(&upload_id)
                    .part_number(part_number)
                    .copy_source(&copy_source)
                    .copy_source_range(&range)
                    .send()
                    .await;
                match resp {
                    Ok(r) => {
                        let e_tag = r
                            .copy_part_result()
                            .and_then(|c| c.e_tag().map(str::to_string))
                            .unwrap_or_default();
                        parts.push(
                            aws_sdk_s3::types::CompletedPart::builder()
                                .part_number(part_number)
                                .e_tag(e_tag)
                                .build(),
                        );
                    }
                    Err(err) => {
                        let _ = self
                            .client
                            .abort_multipart_upload()
                            .bucket(bucket)
                            .key(dest_key)
                            .upload_id(&upload_id)
                            .send()
                            .await;
                        return Err(map_sdk_error("UploadPartCopy", bucket, dest_key, err));
                    }
                }
                offset += length;
            }
            parts.sort_by_key(|p| p.part_number().unwrap_or(0));
            let completed = aws_sdk_s3::types::CompletedMultipartUpload::builder()
                .set_parts(Some(parts))
                .build();
            self.client
                .complete_multipart_upload()
                .bucket(bucket)
                .key(dest_key)
                .upload_id(&upload_id)
                .multipart_upload(completed)
                .send()
                .await
                .map_err(|err| map_sdk_error("CompleteMultipartUpload", bucket, dest_key, err))?;
            Ok(())
        })
    }

    fn delete_object(&self, bucket: &str, key: &str) -> BackupResult<()> {
        self.rt.block_on(async {
            self.client
                .delete_object()
                .bucket(bucket)
                .key(key)
                .send()
                .await
                .map_err(|err| map_sdk_error("DeleteObject", bucket, key, err))?;
            Ok(())
        })
    }

    fn delete_object_version(&self, bucket: &str, key: &str, version_id: &str) -> BackupResult<()> {
        self.rt.block_on(async {
            self.client
                .delete_object()
                .bucket(bucket)
                .key(key)
                .version_id(version_id)
                .send()
                .await
                .map_err(|err| map_sdk_error("DeleteObjectVersion", bucket, key, err))?;
            Ok(())
        })
    }

    fn get_bucket_versioning(&self, bucket: &str) -> BackupResult<bool> {
        self.rt.block_on(async {
            let out = self
                .client
                .get_bucket_versioning()
                .bucket(bucket)
                .send()
                .await
                .map_err(|err| map_sdk_error("GetBucketVersioning", bucket, "", err))?;
            Ok(out.status().is_some_and(|s| s.as_str() == "Enabled"))
        })
    }

    fn put_lifecycle(&self, bucket: &str, rules: &[LifecycleSpec]) -> BackupResult<()> {
        self.rt.block_on(async {
            let mut builders = Vec::new();
            for spec in rules {
                let mut rule = aws_sdk_s3::types::LifecycleRule::builder()
                    .status(aws_sdk_s3::types::ExpirationStatus::Enabled);
                rule = match spec {
                    LifecycleSpec::Noncurrent {
                        prefix,
                        noncurrent_days,
                        newer_noncurrent_versions,
                    } => rule
                        .filter(
                            aws_sdk_s3::types::builders::LifecycleRuleFilterBuilder::default()
                                .prefix(prefix.clone())
                                .build(),
                        )
                        .noncurrent_version_expiration(
                            aws_sdk_s3::types::NoncurrentVersionExpiration::builder()
                                .noncurrent_days(*noncurrent_days)
                                .newer_noncurrent_versions(*newer_noncurrent_versions)
                                .build(),
                        ),
                    LifecycleSpec::MpuAbort {
                        prefix,
                        days_after_initiation,
                    } => rule
                        .filter(
                            aws_sdk_s3::types::builders::LifecycleRuleFilterBuilder::default()
                                .prefix(prefix.clone())
                                .build(),
                        )
                        .abort_incomplete_multipart_upload(
                            aws_sdk_s3::types::AbortIncompleteMultipartUpload::builder()
                                .days_after_initiation(*days_after_initiation)
                                .build(),
                        ),
                    LifecycleSpec::DeleteMarkerSweep {
                        prefix,
                        expired_object_delete_marker,
                    } => rule
                        .filter(
                            aws_sdk_s3::types::builders::LifecycleRuleFilterBuilder::default()
                                .prefix(prefix.clone())
                                .build(),
                        )
                        .expiration(
                            aws_sdk_s3::types::LifecycleExpiration::builder()
                                .expired_object_delete_marker(*expired_object_delete_marker)
                                .build(),
                        ),
                };
                let rule = rule
                    .build()
                    .map_err(|e| BackupError::Config(format!("lifecycle ruleの組立に失敗: {e}")))?;
                builders.push(rule);
            }
            let config = aws_sdk_s3::types::BucketLifecycleConfiguration::builder()
                .set_rules(Some(builders))
                .build()
                .map_err(|e| BackupError::Config(format!("lifecycle設定の組立に失敗: {e}")))?;
            self.client
                .put_bucket_lifecycle_configuration()
                .bucket(bucket)
                .lifecycle_configuration(config)
                .send()
                .await
                .map_err(|err| map_sdk_error("PutBucketLifecycleConfiguration", bucket, "", err))?;
            Ok(())
        })
    }

    fn get_lifecycle(&self, bucket: &str) -> BackupResult<Option<Vec<LifecycleSpec>>> {
        self.rt.block_on(async {
            let out = match self
                .client
                .get_bucket_lifecycle_configuration()
                .bucket(bucket)
                .send()
                .await
            {
                Ok(out) => out,
                Err(err) => {
                    // 未設定(NoSuchLifecycleConfiguration)はNone
                    if err.to_string().contains("NoSuchLifecycleConfiguration") {
                        return Ok(None);
                    }
                    return Err(map_sdk_error(
                        "GetBucketLifecycleConfiguration",
                        bucket,
                        "",
                        err,
                    ));
                }
            };
            let mut specs = Vec::new();
            for rule in out.rules() {
                let prefix = rule
                    .filter()
                    .and_then(|f| f.prefix().map(str::to_string))
                    .unwrap_or_default();
                if let Some(ncve) = rule.noncurrent_version_expiration() {
                    specs.push(LifecycleSpec::Noncurrent {
                        prefix,
                        noncurrent_days: ncve.noncurrent_days().unwrap_or(0),
                        newer_noncurrent_versions: ncve.newer_noncurrent_versions().unwrap_or(0),
                    });
                } else if let Some(mpu) = rule.abort_incomplete_multipart_upload() {
                    specs.push(LifecycleSpec::MpuAbort {
                        prefix,
                        days_after_initiation: mpu.days_after_initiation().unwrap_or(0),
                    });
                } else if let Some(exp) = rule.expiration()
                    && exp.expired_object_delete_marker().unwrap_or(false)
                {
                    specs.push(LifecycleSpec::DeleteMarkerSweep {
                        prefix,
                        expired_object_delete_marker: true,
                    });
                }
            }
            Ok(Some(specs))
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
/// - `FAKE_S3_FAIL_DELVER_FOR`  当該keyのDeleteObjectVersionのみ失敗
///   (`<key>:<N>`形式は先頭N回のみ成功させたうえで失敗=削除loop途中失敗の注入)
#[cfg(debug_assertions)]
pub struct FakeS3 {
    state_dir: PathBuf,
    fail_cp: Option<String>,
    fail_restore: Option<String>,
    fail_head: Option<String>,
    head_404: Option<String>,
    omit_checksum: bool,
    /// GetBucketVersioning応答を無効化(FAKE_S3_VERSIONING_DISABLED・cli.versioning-disabled-error用)
    versioning_disabled: bool,
    /// 当該keyのHeadObject応答size上書き(FAKE_S3_OBJECT_SIZE_FOR=<key>:<bytes>。
    /// CopyObject 5GiB分岐の駆動用。object実体の長さは変えない)
    object_size_for: Option<(String, u64)>,
    /// DeleteObjectVersionの失敗注入(FAKE_S3_FAIL_DELVER_FOR=<key>[:<成功回数N>]。
    /// 先頭N回のみ成功させたうえで失敗。N省略時は毎回失敗)
    fail_delver: Option<(String, u64)>,
}

/// fake内部の版情報1行(`versions/<key>`の1行)。
#[cfg(debug_assertions)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FakeVersionLine {
    version_id: String,
    size: u64,
    last_modified: String,
    /// put=実体のある版・marker=delete marker
    kind: &'static str,
    /// PUT時のstorage class(HeadInfo.storage_classの供給源)
    storage_class: String,
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
            versioning_disabled: false,
            object_size_for: None,
            fail_delver: None,
        }
    }

    /// 結合seam用: env `BACKUP_REMOTE_FAKE_S3_STATE`選択時に失敗注入env 5種+TASK-10
    /// のversioning系env 2種を解釈する。
    pub fn from_env(state_dir: impl Into<PathBuf>) -> FakeS3 {
        let env_key = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        // FAKE_S3_OBJECT_SIZE_FOR=<key>:<bytes>(カンマ区切りで複数可)
        let object_size_for = env_key("FAKE_S3_OBJECT_SIZE_FOR").and_then(|spec| {
            spec.split(',')
                .filter_map(|entry| entry.split_once(':'))
                .filter_map(|(k, v)| v.parse::<u64>().ok().map(|b| (k.to_string(), b)))
                .next()
        });
        // FAKE_S3_FAIL_DELVER_FOR=<key>[:<成功回数>](成功回数省略時は毎回失敗)
        let fail_delver = env_key("FAKE_S3_FAIL_DELVER_FOR").map(|spec| {
            let (k, n) = spec.split_once(':').unwrap_or((spec.as_str(), "0"));
            (k.to_string(), n.parse::<u64>().unwrap_or(0))
        });
        FakeS3 {
            state_dir: state_dir.into(),
            fail_cp: env_key("FAKE_S3_FAIL_CP_FOR"),
            fail_restore: env_key("FAKE_S3_FAIL_RESTORE_FOR"),
            fail_head: env_key("FAKE_S3_FAIL_HEAD_FOR"),
            head_404: env_key("FAKE_S3_HEAD_404_FOR"),
            omit_checksum: false,
            versioning_disabled: env_key("FAKE_S3_VERSIONING_DISABLED").is_some(),
            object_size_for,
            fail_delver,
        }
    }

    /// GetBucketVersioning応答を無効化(FAKE_S3_VERSIONING_DISABLED相当)。
    pub fn versioning_disabled(mut self) -> FakeS3 {
        self.versioning_disabled = true;
        self
    }

    /// 当該keyのHeadObject応答sizeを上書き(FAKE_S3_OBJECT_SIZE_FOR相当)。
    pub fn object_size_for(mut self, key: &str, bytes: u64) -> FakeS3 {
        self.object_size_for = Some((key.to_string(), bytes));
        self
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

    // ---- versioning模擬(TASK-10設計8.2) ----
    // 状態: `versions/<key>` に行 `vid\tsize\tlast_modified\tkind` を時系列順に追記
    // (最終行=現行version。kind: put|marker)。現行の実体は objects/<key>・
    // 過去の実体は object-versions/<key>/<vid> に置く(既存の objects/<key> 互換を維持)。

    fn versions_file(&self, key: &str) -> PathBuf {
        self.state_dir.join("versions").join(key)
    }

    fn object_version_path(&self, key: &str, vid: &str) -> PathBuf {
        self.state_dir.join("object-versions").join(key).join(vid)
    }

    fn version_lines(&self, key: &str) -> Vec<FakeVersionLine> {
        let text = std::fs::read_to_string(self.versions_file(key)).unwrap_or_default();
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| {
                let mut parts = l.split('\t');
                Some(FakeVersionLine {
                    version_id: parts.next()?.to_string(),
                    size: parts.next()?.parse().ok()?,
                    last_modified: parts.next()?.to_string(),
                    kind: match parts.next()? {
                        "marker" => "marker",
                        _ => "put",
                    },
                    // 5列目はPUT時のstorage class(旧state file由来で無い場合は空)
                    storage_class: parts.next().unwrap_or_default().to_string(),
                })
            })
            .collect()
    }

    fn append_version_line(&self, key: &str, line: &FakeVersionLine) {
        let path = self.versions_file(key);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(
                f,
                "{}\t{}\t{}\t{}\t{}",
                line.version_id, line.size, line.last_modified, line.kind, line.storage_class
            );
        }
    }

    /// 現行versionの(vid, 実体bytes)。
    fn current_version(&self, key: &str) -> Option<(String, Vec<u8>)> {
        let lines = self.version_lines(key);
        let last = lines.last()?;
        if last.kind == "marker" {
            return None; // delete markerが現行=objectsから見えない
        }
        let bytes = std::fs::read(self.object_path(key)).ok()?;
        Some((last.version_id.clone(), bytes))
    }

    /// 新版の格納(旧現行をobject-versionsへ退避→objectsへ最新を置き→行追記)。
    /// 戻り値は新しいversion_id("v{N}"・N=通算版数)。
    fn store_new_version(&self, key: &str, data: &[u8], kind: &'static str, sc: &str) -> String {
        let lines = self.version_lines(key);
        if let Some(last) = lines.last() {
            if last.kind == "put" {
                if let Ok(bytes) = std::fs::read(self.object_path(key)) {
                    let path = self.object_version_path(key, &last.version_id);
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(path, &bytes);
                }
            }
        }
        let vid = format!("v{}", lines.len() + 1);
        self.append_version_line(
            key,
            &FakeVersionLine {
                version_id: vid.clone(),
                size: if kind == "marker" {
                    0
                } else {
                    data.len() as u64
                },
                last_modified: chrono::Local::now().to_rfc3339(),
                kind,
                storage_class: sc.to_string(),
            },
        );
        if kind == "put" {
            self.write_object(key, data);
        } else {
            let _ = std::fs::remove_file(self.object_path(key));
        }
        vid
    }

    /// 指定versionの実体bytes(markerはNone)。
    fn version_bytes(&self, key: &str, vid: &str) -> Option<Vec<u8>> {
        let lines = self.version_lines(key);
        let last = lines.last()?;
        if last.kind == "marker" {
            return None;
        }
        if last.version_id == vid {
            std::fs::read(self.object_path(key)).ok()
        } else {
            std::fs::read(self.object_version_path(key, vid)).ok()
        }
    }

    /// HeadObject応答size(env FAKE_S3_OBJECT_SIZE_FORの上書きを適用)。
    fn head_size(&self, key: &str, actual: u64) -> u64 {
        match &self.object_size_for {
            Some((k, bytes)) if k == key => *bytes,
            _ => actual,
        }
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

    /// RestoreObject共通の事前検査(L-3・実S3相当): key(版指定時は当該version)の実在と
    /// archive storage class(GLACIER/DEEP_ARCHIVE/GLACIER_IR)を検査する。
    /// 不在keyはNoSuchKey/NoSuchVersion相当・archive系以外はInvalidObjectState相当の
    /// 転送エラー(不在keyやSTANDARD objectへの復元job発行がfakeで成功扱いにならない)。
    fn restore_precheck(
        &self,
        bucket: &str,
        key: &str,
        version_id: Option<&str>,
    ) -> BackupResult<()> {
        let lines = self.version_lines(key);
        let storage_class = match version_id {
            Some(vid) => match lines
                .iter()
                .find(|l| l.version_id == vid && l.kind != "marker")
            {
                Some(l) => l.storage_class.clone(),
                None => {
                    // 版stateの無いkey(seed配置等)はobjects実体のみで判断
                    if std::fs::read(self.object_path(key)).is_err() {
                        return Err(BackupError::Transfer(format!(
                            "S3 RestoreObject失敗(bucket={bucket}, key={key}): 対象keyがS3に存在しない(NoSuchKey)"
                        )));
                    }
                    String::new()
                }
            },
            None => {
                if std::fs::read(self.object_path(key)).is_err() {
                    return Err(BackupError::Transfer(format!(
                        "S3 RestoreObject失敗(bucket={bucket}, key={key}): 対象keyがS3に存在しない(NoSuchKey)"
                    )));
                }
                // 現行版のstorage_class(版stateの無いseed配置は空=STANDARD相当)
                lines
                    .last()
                    .filter(|l| l.kind == "put")
                    .map(|l| l.storage_class.clone())
                    .unwrap_or_default()
            }
        };
        if !matches!(
            storage_class.as_str(),
            "GLACIER" | "DEEP_ARCHIVE" | "GLACIER_IR"
        ) {
            return Err(BackupError::Transfer(format!(
                "S3 RestoreObject失敗(bucket={bucket}, key={key}): 対象objectはarchive storage classでないため復元jobを発行できません(InvalidObjectState: storage_class={storage_class})"
            )));
        }
        Ok(())
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
            size: self.head_size(key, bytes.len() as u64) as i64,
            checksum_sha256: if self.omit_checksum {
                None
            } else {
                Some(Self::sha256_b64(&bytes))
            },
            checksum_type: Some("FULL_OBJECT".to_string()),
            restore: self.restore_header(key),
            storage_class: self
                .version_lines(key)
                .last()
                .filter(|l| l.kind == "put")
                .map(|l| l.storage_class.clone())
                .filter(|sc| !sc.is_empty()),
        }))
    }

    fn put(
        &self,
        bucket: &str,
        key: &str,
        src: &Path,
        storage_class: &str,
    ) -> BackupResult<String> {
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
            let data = std::fs::read(src).map_err(|e| src_read_error("UploadPart", key, src, e))?;
            for part_number in 1..=multipart_part_count(size) {
                self.record(&format!(
                    "s3 UploadPart bucket={bucket} key={key} part_number={part_number} checksum=SHA256"
                ));
                if self.fail_cp.as_deref() == Some(key) {
                    // part失敗 → Abort発火(Completeは実行しない)
                    self.record(&format!(
                        "s3 AbortMultipartUpload bucket={bucket} key={key}"
                    ));
                    return Err(fail());
                }
            }
            self.record(&format!(
                "s3 CompleteMultipartUpload bucket={bucket} key={key}"
            ));
            Ok(self.store_new_version(key, &data, "put", storage_class))
        } else {
            self.record(&format!(
                "s3 PutObject bucket={bucket} key={key} src={} storage_class={storage_class} checksum=SHA256",
                src.display()
            ));
            if self.fail_cp.as_deref() == Some(key) {
                return Err(fail());
            }
            let data = std::fs::read(src).map_err(|e| src_read_error("PutObject", key, src, e))?;
            Ok(self.store_new_version(key, &data, "put", storage_class))
        }
    }

    fn get_to_file(&self, bucket: &str, key: &str, dest: &Path) -> BackupResult<()> {
        if self.get_to_file_checked(bucket, key, dest)? {
            Ok(())
        } else {
            Err(BackupError::Transfer(format!(
                "対象keyがS3に存在しない(key={key})"
            )))
        }
    }

    fn get_to_file_checked(&self, bucket: &str, key: &str, dest: &Path) -> BackupResult<bool> {
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
            Err(_) => return Ok(false),
        };
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(dest, &bytes).map_err(|e| {
            BackupError::Transfer(format!(
                "S3 GetObject失敗(key={key}, dest={}): 取得先fileの書込に失敗: {e}",
                dest.display()
            ))
        })?;
        Ok(true)
    }

    fn list_keys(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<String>> {
        self.record(&format!("s3 ListObjectsV2 bucket={bucket} prefix={prefix}"));
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
        self.restore_precheck(bucket, key, None)?;
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

    fn head_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
    ) -> BackupResult<Option<HeadInfo>> {
        self.record(&format!(
            "s3 HeadObject bucket={bucket} key={key} version_id={version_id}"
        ));
        if self.fail_head.as_deref() == Some(key) {
            return Err(BackupError::Transfer(format!(
                "S3 HeadObject失敗(bucket={bucket}, key={key}): FAKE_S3_FAIL_HEAD_FOR による失敗注入"
            )));
        }
        let Some(bytes) = self.version_bytes(key, version_id) else {
            return Ok(None); // 404
        };
        Ok(Some(HeadInfo {
            size: self.head_size(key, bytes.len() as u64) as i64,
            checksum_sha256: if self.omit_checksum {
                None
            } else {
                Some(Self::sha256_b64(&bytes))
            },
            checksum_type: Some("FULL_OBJECT".to_string()),
            restore: self.restore_header(key),
            storage_class: self
                .version_lines(key)
                .iter()
                .find(|l| l.version_id == version_id)
                .map(|l| l.storage_class.clone())
                .filter(|sc| !sc.is_empty()),
        }))
    }

    fn get_version_to_file(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
        dest: &Path,
    ) -> BackupResult<()> {
        self.record(&format!(
            "s3 GetObject bucket={bucket} key={key} dest={} version_id={version_id} checksum_mode=ENABLED",
            dest.display()
        ));
        if self.fail_cp.as_deref() == Some(key) {
            return Err(BackupError::Transfer(format!(
                "S3 GetObject失敗(bucket={bucket}, key={key}): FAKE_S3_FAIL_CP_FOR による失敗注入"
            )));
        }
        let Some(bytes) = self.version_bytes(key, version_id) else {
            return Err(BackupError::Transfer(format!(
                "対象keyがS3に存在しない(key={key}, version_id={version_id})"
            )));
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

    fn list_versions(&self, bucket: &str, prefix: &str) -> BackupResult<Vec<VersionInfo>> {
        self.record(&format!(
            "s3 ListObjectVersions bucket={bucket} prefix={prefix}"
        ));
        let mut versions = Vec::new();
        let base = self.state_dir.join("versions");
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
        let mut keys = Vec::new();
        walk(&base, &base, &mut keys);
        // objects配下にのみ存在するkey(seed_object等・版stateなし)も現行版として列挙
        let mut objects_keys = Vec::new();
        let objects = self.objects_dir();
        walk(&objects, &objects, &mut objects_keys);
        for k in objects_keys {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        keys.sort();
        keys.dedup();
        for key in keys {
            if !key.starts_with(prefix) {
                continue;
            }
            let lines = self.version_lines(&key);
            if lines.is_empty() {
                // 版stateなし: objectsの実体を単一現行版として列挙する
                if let Ok(meta) = std::fs::metadata(self.object_path(&key)) {
                    let epoch = meta
                        .modified()
                        .ok()
                        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    versions.push(VersionInfo {
                        key: key.clone(),
                        version_id: String::new(),
                        last_modified: String::new(),
                        last_modified_epoch: epoch,
                        size: self.head_size(&key, meta.len()) as i64,
                        is_latest: true,
                        is_delete_marker: false,
                    });
                }
                continue;
            }
            // 現行版(最終行)がputなのにobjects/<key>実体が無い(外部から実体のみ削除された)
            // 場合は、その版は存在しない扱いとする(現行版以外はobject-versionsに実体がある)
            let mut lines = lines;
            if lines
                .last()
                .is_some_and(|l| l.kind == "put" && !self.object_path(&key).is_file())
            {
                lines.pop();
            }
            if lines.is_empty() {
                continue;
            }
            // 同じkey内では新しい版が先(epoch秒降順・実装間で同一順序)。
            // 現行version=先頭要素
            lines.sort_by(|a, b| {
                let ea = chrono::DateTime::parse_from_rfc3339(&a.last_modified)
                    .map(|d| d.timestamp())
                    .unwrap_or(0);
                let eb = chrono::DateTime::parse_from_rfc3339(&b.last_modified)
                    .map(|d| d.timestamp())
                    .unwrap_or(0);
                eb.cmp(&ea).then_with(|| b.version_id.cmp(&a.version_id))
            });
            for (i, line) in lines.iter().enumerate() {
                versions.push(VersionInfo {
                    key: key.clone(),
                    version_id: line.version_id.clone(),
                    last_modified: line.last_modified.clone(),
                    last_modified_epoch: chrono::DateTime::parse_from_rfc3339(&line.last_modified)
                        .map(|d| d.timestamp())
                        .unwrap_or(0),
                    size: if line.kind == "marker" {
                        0
                    } else {
                        self.head_size(&key, line.size) as i64
                    },
                    is_latest: i == 0,
                    is_delete_marker: line.kind == "marker",
                });
            }
        }
        Ok(versions)
    }

    fn restore_version(
        &self,
        bucket: &str,
        key: &str,
        version_id: &str,
        days: u32,
        tier: &str,
    ) -> BackupResult<()> {
        self.record(&format!(
            "s3 RestoreObject bucket={bucket} key={key} version_id={version_id} days={days} tier={tier}"
        ));
        if self.fail_restore.as_deref() == Some(key) {
            return Err(BackupError::Transfer(format!(
                "S3 RestoreObject失敗(bucket={bucket}, key={key}): FAKE_S3_FAIL_RESTORE_FOR による失敗注入"
            )));
        }
        self.restore_precheck(bucket, key, Some(version_id))?;
        // 版単位の復元状態はkey単位のmetaに記録する(fakeの簡略化・HEAD系は同じ値を返す)
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

    fn copy_object(
        &self,
        bucket: &str,
        src_key: &str,
        src_version_id: Option<&str>,
        dest_key: &str,
        _storage_class: &str,
    ) -> BackupResult<()> {
        let src_vid = src_version_id
            .map(str::to_string)
            .or_else(|| self.current_version(src_key).map(|(vid, _)| vid));
        let Some(src_vid) = src_vid else {
            return Err(BackupError::Transfer(format!(
                "CopyObjectの写し元がS3に存在しない(key={src_key})"
            )));
        };
        let Some(bytes) = self.version_bytes(src_key, &src_vid) else {
            return Err(BackupError::Transfer(format!(
                "CopyObjectの写し元版がS3に存在しない(key={src_key}, version_id={src_vid})"
            )));
        };
        let size = self.head_size(src_key, bytes.len() as u64);
        if size > COPY_OBJECT_MAX_BYTES {
            // 5GiB超: UploadPartCopyで分割写し(設計1.3。sizeはFAKE_S3_OBJECT_SIZE_FORで駆動)
            let parts = size.div_ceil(COPY_OBJECT_MAX_BYTES);
            self.record(&format!(
                "s3 CreateMultipartUpload bucket={bucket} key={dest_key} storage_class={_storage_class} checksum=SHA256"
            ));
            for part_number in 1..=parts {
                self.record(&format!(
                    "s3 UploadPartCopy bucket={bucket} key={dest_key} part_number={part_number}"
                ));
            }
            self.record(&format!(
                "s3 CompleteMultipartUpload bucket={bucket} key={dest_key}"
            ));
        } else {
            self.record(&format!(
                "s3 CopyObject bucket={bucket} key={dest_key} src={src_key} src_version_id={src_vid} storage_class={_storage_class}"
            ));
        }
        self.store_new_version(dest_key, &bytes, "put", _storage_class);
        Ok(())
    }

    fn delete_object(&self, bucket: &str, key: &str) -> BackupResult<()> {
        self.record(&format!("s3 DeleteObject bucket={bucket} key={key}"));
        // versioning bucket相当: 現行版の実体を退避しdelete markerを現行にする
        self.store_new_version(key, &[], "marker", "");
        Ok(())
    }

    fn delete_object_version(&self, bucket: &str, key: &str, version_id: &str) -> BackupResult<()> {
        self.record(&format!(
            "s3 DeleteObjectVersion bucket={bucket} key={key} version_id={version_id}"
        ));
        // FAKE_S3_FAIL_DELVER_FOR=<key>[:<成功回数N>]: 当該keyのDeleteObjectVersionの
        // うち先頭N回のみ成功させたうえで失敗させる(削除loop途中失敗の注入)
        if let Some((fail_key, ok_count)) = &self.fail_delver
            && fail_key == key
        {
            // record済みのためcalls.logの当該行数=今回を含む呼び出し回数
            let done = std::fs::read_to_string(self.state_dir.join("calls.log"))
                .map(|t| {
                    t.lines()
                        .filter(|l| {
                            l.starts_with("s3 DeleteObjectVersion")
                                && l.contains(&format!("key={key}"))
                        })
                        .count() as u64
                })
                .unwrap_or(0);
            if done > *ok_count {
                return Err(BackupError::Transfer(format!(
                    "S3 DeleteObjectVersion失敗(bucket={bucket}, key={key}, version_id={version_id}): FAKE_S3_FAIL_DELVER_FOR による失敗注入"
                )));
            }
        }
        let mut lines = self.version_lines(key);
        let Some(pos) = lines.iter().position(|l| l.version_id == version_id) else {
            return Ok(()); // 対象版なしは黙って成功(S3相当の寛容さ)
        };
        let is_current = pos == lines.len() - 1;
        lines.remove(pos);
        // 行の書換(追記fileの再構成)
        let path = self.versions_file(key);
        if lines.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::File::create(&path) {
                for line in &lines {
                    // 5列構造(vid/size/mtime/kind/storage_class)を保持して書戻す(L-11)
                    let _ = writeln!(
                        f,
                        "{}\t{}\t{}\t{}\t{}",
                        line.version_id,
                        line.size,
                        line.last_modified,
                        line.kind,
                        line.storage_class
                    );
                }
            }
        }
        let _ = std::fs::remove_file(self.object_version_path(key, version_id));
        if is_current {
            // 削除したのが現行版: 直前の残版を昇格させる
            if let Some(prev) = lines.last() {
                if prev.kind == "put" {
                    if let Some(bytes) =
                        std::fs::read(self.object_version_path(key, &prev.version_id)).ok()
                    {
                        self.write_object(key, &bytes);
                        let _ =
                            std::fs::remove_file(self.object_version_path(key, &prev.version_id));
                    }
                } else {
                    let _ = std::fs::remove_file(self.object_path(key));
                }
            } else {
                let _ = std::fs::remove_file(self.object_path(key));
            }
        }
        Ok(())
    }

    fn get_bucket_versioning(&self, bucket: &str) -> BackupResult<bool> {
        self.record(&format!("s3 GetBucketVersioning bucket={bucket}"));
        Ok(!self.versioning_disabled)
    }

    fn put_lifecycle(&self, bucket: &str, rules: &[LifecycleSpec]) -> BackupResult<()> {
        self.record(&format!(
            "s3 PutBucketLifecycleConfiguration bucket={bucket}"
        ));
        let path = self.state_dir.join("lifecycle.json");
        let body = serde_json::to_string(rules)
            .map_err(|e| BackupError::Internal(format!("lifecycle設定のserializeに失敗: {e}")))?;
        std::fs::write(path, body).map_err(|e| {
            BackupError::Transfer(format!(
                "S3 PutBucketLifecycleConfiguration失敗(bucket={bucket}): 記録に失敗: {e}"
            ))
        })
    }

    fn get_lifecycle(&self, bucket: &str) -> BackupResult<Option<Vec<LifecycleSpec>>> {
        self.record(&format!(
            "s3 GetBucketLifecycleConfiguration bucket={bucket}"
        ));
        match std::fs::read_to_string(self.state_dir.join("lifecycle.json")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(BackupError::Transfer(format!(
                "S3 GetBucketLifecycleConfiguration失敗(bucket={bucket}): 記録の読込に失敗: {e}"
            ))),
            Ok(t) => serde_json::from_str(&t)
                .map(Some)
                .map_err(|e| BackupError::Internal(format!("lifecycle設定のparseに失敗: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{TempDir, write_file};

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
        assert_eq!(multipart_part_count(MULTIPART_THRESHOLD_BYTES * 3), 3);
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

        assert_eq!(
            fake.head("b", "2026-10-01/a.zip.age").expect("head"),
            None,
            "未配置は404"
        );
        fake.put("b", "2026-10-01/a.zip.age", &local, "DEEP_ARCHIVE")
            .expect("put");
        let head = fake
            .head("b", "2026-10-01/a.zip.age")
            .expect("head")
            .expect("配置済み");
        assert_eq!(head.size, b"age-bytes".len() as i64);
        assert_eq!(
            head.checksum_sha256.as_deref(),
            Some(sha256_b64(b"age-bytes").as_str())
        );
        assert_eq!(head.checksum_type.as_deref(), Some("FULL_OBJECT"));
        assert_eq!(head.restore, None, "復元job未発行");

        fake.restore("b", "2026-10-01/a.zip.age", 3, "Bulk")
            .expect("restore");
        let head = fake
            .head("b", "2026-10-01/a.zip.age")
            .expect("head")
            .expect("配置済み");
        assert_eq!(
            head.restore.as_deref(),
            Some(r#"ongoing-request="false", expiry-date="Wed, 14 Oct 2026 00:00:00 GMT""#),
            "restore発行済み(復元済み状態)"
        );

        let keys = fake.list_keys("b", "2026-10-01/").expect("list");
        assert_eq!(keys, vec!["2026-10-01/a.zip.age".to_string()]);

        let calls = fake.calls();
        assert!(
            calls.contains("s3 PutObject bucket=b key=2026-10-01/a.zip.age"),
            "{calls}"
        );
        assert!(
            calls.contains("s3 RestoreObject bucket=b key=2026-10-01/a.zip.age days=3 tier=Bulk"),
            "{calls}"
        );
        assert!(
            calls.contains("s3 ListObjectsV2 bucket=b prefix=2026-10-01/"),
            "{calls}"
        );
        assert!(
            calls.contains("s3 HeadObject bucket=b key=2026-10-01/a.zip.age"),
            "{calls}"
        );
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
            fake.put("b", key, &local, "DEEP_ARCHIVE")
                .unwrap_err()
                .exit_code(),
            5
        );
        // restore失敗注入
        let fake = FakeS3::new(&state).fail_restore(key);
        assert_eq!(
            fake.restore("b", key, 7, "Standard")
                .unwrap_err()
                .exit_code(),
            5
        );
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
        let e = fake
            .get_to_file("b", "2026-10-01/missing.age", &dest)
            .unwrap_err();
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
