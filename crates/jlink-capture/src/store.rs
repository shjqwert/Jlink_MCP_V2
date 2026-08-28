use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{BufWriter, ErrorKind, Read, Seek, SeekFrom, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crc32fast::hash as crc32;
use jlink_domain::{
    ErrorCode, HssCaptureState, HssDataIntegrity, HssDrainTiming, HssRecoveryNotification,
    HssRunSnapshot, HssStartPlan, JlinkError, TargetConnectionSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use windows_sys::Win32::{Foundation::GetLastError, Storage::FileSystem::GetDiskFreeSpaceExW};

const FILE_MAGIC: &[u8; 8] = b"JMCPV101";
const BLOCK_MAGIC: &[u8; 4] = b"BLK1";
const TERMINAL_MAGIC: &[u8; 4] = b"END1";
const FORMAT_VERSION: u32 = 1;
const FILE_HEADER_LEN: usize = 20;
const BLOCK_HEADER_REST_LEN: usize = 20;
const TERMINAL_HEADER_REST_LEN: usize = 8;
const FILE_HEADER_BYTES: u64 = 20;
const BLOCK_HEADER_BYTES: u64 = 24;
const TERMINAL_HEADER_BYTES: u64 = 12;
const MAX_BLOCK_BYTES: usize = 16 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const SYNC_INTERVAL_BYTES: u64 = 4 * 1024 * 1024;
const TERMINAL_ESTIMATE_BYTES: u64 = 1024 * 1024;

/// Default maximum bytes accepted for one capture when project configuration omits it.
pub const DEFAULT_CAPTURE_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// Capture phase retained with each independently checksummed append block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapturePhase {
    /// Bytes drained before the internal Stop call.
    Live,
    /// Bytes drained after Stop while closing the DLL tail.
    Tail,
}

impl CapturePhase {
    const fn as_u32(self) -> u32 {
        match self {
            Self::Live => 0,
            Self::Tail => 1,
        }
    }
}

/// Conservative storage estimate checked before any HSS Start call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureEstimate {
    raw: u64,
    storage: u64,
    available: u64,
    limit: u64,
}

impl CaptureEstimate {
    /// Returns the requested fixed-duration raw-frame upper bound.
    #[must_use]
    pub const fn raw_bytes(self) -> u64 {
        self.raw
    }

    /// Returns the conservative file-size upper bound including per-sample block overhead.
    #[must_use]
    pub const fn storage_bytes(self) -> u64 {
        self.storage
    }

    /// Returns bytes available to the current user at preflight time.
    #[must_use]
    pub const fn available_bytes(self) -> u64 {
        self.available
    }

    /// Returns the effective configured single-capture limit.
    #[must_use]
    pub const fn max_bytes(self) -> u64 {
        self.limit
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CaptureHeader {
    capture_id: String,
    capture_key: String,
    request_fingerprint: String,
    created_unix_us: u64,
    target: TargetConnectionSpec,
    plan: HssStartPlan,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CaptureManifest {
    snapshot: HssRunSnapshot,
    blocks: u64,
    payload_bytes: u64,
    raw_sha256: String,
}

/// Read-only verified identity of one atomically published capture file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureSnapshot {
    path: PathBuf,
    header: CaptureHeader,
    manifest: CaptureManifest,
}

impl CaptureSnapshot {
    /// Returns the immutable completed resource path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the stable capture identity.
    #[must_use]
    pub fn capture_id(&self) -> &str {
        &self.header.capture_id
    }

    /// Returns the Agent-provided recovery key.
    #[must_use]
    pub fn capture_key(&self) -> &str {
        &self.header.capture_key
    }

    /// Returns the normalized HSS plan embedded in the self-describing file.
    #[must_use]
    pub const fn plan(&self) -> &HssStartPlan {
        &self.header.plan
    }

    /// Returns the complete target connection identity bound to this capture.
    #[must_use]
    pub const fn target(&self) -> &TargetConnectionSpec {
        &self.header.target
    }

    /// Returns the verified terminal Worker snapshot.
    #[must_use]
    pub const fn status(&self) -> &HssRunSnapshot {
        &self.manifest.snapshot
    }

    /// Returns complete payload bytes covered by block CRC and terminal SHA-256.
    #[must_use]
    pub const fn payload_bytes(&self) -> u64 {
        self.manifest.payload_bytes
    }

    /// Returns the terminal SHA-256 of concatenated raw block payloads.
    #[must_use]
    pub fn raw_sha256(&self) -> &str {
        &self.manifest.raw_sha256
    }

    /// Reads the concatenated raw HSS payload after re-verifying the immutable file.
    ///
    /// # Errors
    ///
    /// Returns a stable identity, frame, CRC, digest, or local storage error.
    pub(crate) fn read_verified_payload(&self) -> Result<Vec<u8>, JlinkError> {
        let scan = scan_capture(&self.path, true)?;
        self.verify_scan_identity(&scan)?;
        Ok(scan.raw_payload)
    }

    /// Reads the complete immutable, self-describing capture resource after
    /// re-verifying its header, block CRCs, raw digest, and terminal manifest.
    ///
    /// # Errors
    ///
    /// Returns a stable identity, frame, CRC, digest, size, or local storage error.
    pub fn read_verified_resource(&self) -> Result<Vec<u8>, JlinkError> {
        let (mut file, file_len) = open_capture_file(&self.path)?;
        let scan = scan_capture_file(&mut file, file_len, false)?;
        self.verify_scan_identity(&scan)?;
        let resource_len = usize::try_from(file_len)
            .map_err(|_| storage_error("Capture Store 资源大小无法表示为 usize"))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| storage_error(format!("无法定位原始 capture 资源：{error}")))?;
        let mut resource = Vec::with_capacity(resource_len);
        file.read_to_end(&mut resource)
            .map_err(|error| storage_error(format!("无法读取原始 capture 资源：{error}")))?;
        if resource.len() != resource_len {
            return Err(invalid_store("原始 capture 资源长度在读取期间发生变化"));
        }
        Ok(resource)
    }

    fn verify_scan_identity(&self, scan: &CaptureScan) -> Result<(), JlinkError> {
        if scan.header != self.header || scan.manifest.as_ref() != Some(&self.manifest) {
            return Err(invalid_store(
                "查询期间 Capture Store 自描述身份或终态清单发生变化",
            ));
        }
        Ok(())
    }
}

/// Startup classification of one file left with a `.partial` suffix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureRecovery {
    /// A complete terminal manifest was durable before rename and is now published.
    Published(CaptureSnapshot),
    /// No verified terminal manifest exists; valid blocks remain available as aborted data.
    Aborted {
        /// Stable identity from the header or partial filename.
        capture_id: String,
        /// Capture key when the self-describing header remains valid.
        capture_key: Option<String>,
        /// Self-describing start plan when the header remains valid.
        plan: Option<HssStartPlan>,
        /// Complete target connection identity when the header remains valid.
        target: Option<TargetConnectionSpec>,
        /// Aborted/unknown status with recovery facts.
        status: HssRunSnapshot,
        /// Undeleted partial path retained for later inspection.
        path: PathBuf,
    },
}

/// Root owner for active partial files and immutable completed capture resources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureStore {
    root: PathBuf,
}

impl CaptureStore {
    /// Creates or opens one local store root without modifying completed captures.
    ///
    /// # Errors
    ///
    /// Returns a stable local storage error when the directory cannot be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, JlinkError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|error| {
            storage_error(format!(
                "无法创建 Capture Store {}：{error}",
                root.display()
            ))
        })?;
        Ok(Self { root })
    }

    /// Opens an existing store root without creating directories or capture files.
    ///
    /// # Errors
    ///
    /// Returns a stable local storage error when metadata cannot be inspected or
    /// the existing path is not a directory.
    pub fn open_existing(root: impl Into<PathBuf>) -> Result<Option<Self>, JlinkError> {
        let root = root.into();
        match fs::metadata(&root) {
            Ok(metadata) if metadata.is_dir() => Ok(Some(Self { root })),
            Ok(_) => Err(storage_error(format!(
                "Capture Store 根路径不是目录：{}",
                root.display()
            ))),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(storage_error(format!(
                "无法读取 Capture Store 根路径 {}：{error}",
                root.display()
            ))),
        }
    }

    /// Returns the local store root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Estimates the largest requested file and checks configured and disk limits.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::HssUnsupported`] when the request exceeds the
    /// configured limit or available disk, and a stable storage error if disk
    /// capacity cannot be queried.
    pub fn preflight(
        &self,
        plan: &HssStartPlan,
        max_bytes: u64,
    ) -> Result<CaptureEstimate, JlinkError> {
        plan.validate()?;
        if max_bytes == 0 {
            return Err(capacity_error("capture.max_bytes 必须大于 0"));
        }
        let samples = u64::from(plan.duration_s())
            .checked_mul(u64::from(plan.rate_hz()))
            .ok_or_else(|| capacity_error("HSS 样本数量估算溢出"))?;
        let raw_bytes = samples
            .checked_mul(u64::from(plan.frame_layout().record_bytes()))
            .ok_or_else(|| capacity_error("HSS 原始字节估算溢出"))?;
        let storage_bytes = raw_bytes
            .checked_add(
                samples
                    .checked_mul(BLOCK_HEADER_BYTES)
                    .ok_or_else(|| capacity_error("Capture Store 块开销估算溢出"))?,
            )
            .and_then(|value| value.checked_add(FILE_HEADER_BYTES))
            .and_then(|value| value.checked_add(TERMINAL_ESTIMATE_BYTES))
            .ok_or_else(|| capacity_error("Capture Store 总大小估算溢出"))?;
        let available_bytes = available_disk_bytes(&self.root)?;
        let estimate = CaptureEstimate {
            raw: raw_bytes,
            storage: storage_bytes,
            available: available_bytes,
            limit: max_bytes,
        };
        if storage_bytes > max_bytes {
            return Err(
                capacity_error("预计 Capture Store 文件超过 capture.max_bytes")
                    .with_detail("estimated_bytes", json!(storage_bytes))
                    .with_detail("max_bytes", json!(max_bytes))
                    .with_detail(
                        "recommendation",
                        json!("降低 duration_s/rate_hz/变量宽度，或显式提高工程 capture.max_bytes"),
                    ),
            );
        }
        if storage_bytes > available_bytes {
            return Err(capacity_error("可用磁盘空间不足以启动 HSS 采集")
                .with_detail("estimated_bytes", json!(storage_bytes))
                .with_detail("available_bytes", json!(available_bytes))
                .with_detail("recommendation", json!("释放 Capture Store 所在卷的空间")));
        }
        Ok(estimate)
    }

    /// Creates one append-only `.partial` writer after successful preflight.
    ///
    /// # Errors
    ///
    /// Returns a stable identity, capacity, or local storage error. Existing
    /// partial or completed files are never overwritten.
    pub fn create_writer(
        &self,
        capture_id: &str,
        target: &TargetConnectionSpec,
        plan: &HssStartPlan,
        max_bytes: u64,
    ) -> Result<CaptureWriter, JlinkError> {
        validate_capture_id(capture_id)?;
        target.validate()?;
        self.preflight(plan, max_bytes)?;
        let partial_path = self.partial_path(capture_id);
        let completed_path = self.completed_path(capture_id);
        if completed_path.exists() {
            return Err(storage_error("不可变完成 capture 已存在，拒绝覆盖")
                .with_detail("capture_id", json!(capture_id)));
        }
        let header = CaptureHeader {
            capture_id: capture_id.to_owned(),
            capture_key: plan.capture_key().to_owned(),
            request_fingerprint: plan.request_fingerprint().to_owned(),
            created_unix_us: unix_time_us()?,
            target: target.clone(),
            plan: plan.clone(),
        };
        CaptureWriter::create(partial_path, completed_path, header, max_bytes)
    }

    /// Opens and fully verifies one immutable completed resource.
    ///
    /// # Errors
    ///
    /// Returns a stable identity, frame, CRC, digest, or local storage error.
    pub fn open_snapshot(&self, capture_id: &str) -> Result<CaptureSnapshot, JlinkError> {
        validate_capture_id(capture_id)?;
        let path = self.completed_path(capture_id);
        let scan = scan_capture(&path, false)?;
        if scan.header.capture_id != capture_id {
            return Err(invalid_store("完成 capture 文件名与自描述身份不一致")
                .with_detail("requested_capture_id", json!(capture_id))
                .with_detail("stored_capture_id", json!(scan.header.capture_id)));
        }
        let manifest = scan
            .manifest
            .ok_or_else(|| invalid_store("完成 capture 缺少终态清单"))?;
        if manifest.snapshot.capture_id != capture_id {
            return Err(invalid_store("完成 capture 终态清单与自描述身份不一致")
                .with_detail("capture_id", json!(capture_id))
                .with_detail("manifest_capture_id", json!(manifest.snapshot.capture_id)));
        }
        Ok(CaptureSnapshot {
            path,
            header: scan.header,
            manifest,
        })
    }

    /// Finds one immutable completed capture by stable identity without creating it.
    ///
    /// # Errors
    ///
    /// Returns a stable identity, frame, CRC, digest, or local storage error.
    pub fn find_snapshot(&self, capture_id: &str) -> Result<Option<CaptureSnapshot>, JlinkError> {
        validate_capture_id(capture_id)?;
        let path = self.completed_path(capture_id);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => self.open_snapshot(capture_id).map(Some),
            Ok(_) => Err(invalid_store(format!(
                "完成 capture 路径不是文件：{}",
                path.display()
            ))),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(storage_error(format!(
                "无法读取完成 capture 元数据 {}：{error}",
                path.display()
            ))),
        }
    }

    /// Finds one immutable completed capture by Agent-provided recovery key.
    ///
    /// # Errors
    ///
    /// Returns a stable frame or local storage error. Duplicate keys are rejected
    /// as an invalid immutable index instead of selecting an arbitrary capture.
    pub fn find_snapshot_by_key(
        &self,
        capture_key: &str,
    ) -> Result<Option<CaptureSnapshot>, JlinkError> {
        if capture_key.trim().is_empty() {
            return Err(JlinkError::new(
                ErrorCode::ValueInvalid,
                "capture_key 不能为空或仅包含空白",
                false,
            ));
        }
        let mut found = None;
        for snapshot in self.completed_snapshots()? {
            if snapshot.capture_key() != capture_key {
                continue;
            }
            if found.replace(snapshot).is_some() {
                return Err(invalid_store("同一 Capture Store 存在重复 capture_key"));
            }
        }
        Ok(found)
    }

    /// Opens and verifies every immutable completed capture in stable path order.
    ///
    /// # Errors
    ///
    /// Returns a stable directory, identity, frame, CRC, digest, or local storage error.
    pub fn completed_snapshots(&self) -> Result<Vec<CaptureSnapshot>, JlinkError> {
        let mut paths = fs::read_dir(&self.root)
            .map_err(|error| storage_error(format!("无法扫描 Capture Store：{error}")))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension() == Some(OsStr::new("capture")))
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let capture_id = capture_id_from_store_path(&path, "capture").ok_or_else(|| {
                    invalid_store("完成 capture 文件名不符合 capture-<id>.capture")
                })?;
                let snapshot = self.open_snapshot(&capture_id)?;
                if snapshot.path() != path {
                    return Err(invalid_store("完成 capture 路径与自描述身份不一致")
                        .with_detail("capture_id", json!(capture_id)));
                }
                Ok(snapshot)
            })
            .collect()
    }

    /// Scans every partial file without deleting it and atomically publishes any
    /// file that already contains a valid terminal manifest.
    ///
    /// # Errors
    ///
    /// Returns a stable directory or atomic-publish error. Corrupt individual
    /// partial files are returned as `Aborted` instead of failing the whole scan.
    pub fn recover_partials(&self) -> Result<Vec<CaptureRecovery>, JlinkError> {
        let mut paths = fs::read_dir(&self.root)
            .map_err(|error| storage_error(format!("无法扫描 Capture Store：{error}")))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension() == Some(OsStr::new("partial")))
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| self.recover_one(path))
            .collect()
    }

    fn recover_one(&self, path: PathBuf) -> Result<CaptureRecovery, JlinkError> {
        match scan_capture(&path, false) {
            Ok(scan) => {
                if let Some(manifest) = scan.manifest {
                    let completed_path = self.completed_path(&scan.header.capture_id);
                    if completed_path.exists() {
                        return Err(storage_error(
                            "partial 与不可变完成 capture 同时存在，拒绝覆盖",
                        )
                        .with_detail("capture_id", json!(scan.header.capture_id)));
                    }
                    fs::rename(&path, &completed_path).map_err(|error| {
                        storage_error(format!("无法原子发布恢复 capture：{error}"))
                    })?;
                    return Ok(CaptureRecovery::Published(CaptureSnapshot {
                        path: completed_path,
                        header: scan.header,
                        manifest,
                    }));
                }
                Ok(aborted_recovery(AbortedRecoveryInput {
                    path,
                    capture_id: scan.header.capture_id.clone(),
                    capture_key: Some(scan.header.capture_key.clone()),
                    plan: Some(scan.header.plan.clone()),
                    target: Some(scan.header.target.clone()),
                    valid_payload_bytes: scan.valid_payload_bytes,
                    last_host_elapsed_us: scan.last_host_elapsed_us,
                    trailing_bytes: scan.trailing_bytes,
                    complete_records: scan.valid_payload_bytes
                        / u64::from(scan.header.plan.frame_layout().record_bytes()),
                    reason: "启动扫描发现未完成 Capture Store 文件".to_owned(),
                    recoverable: !scan.crc_error,
                }))
            }
            Err(error) => {
                let capture_id = capture_id_from_partial_path(&path)
                    .unwrap_or_else(|| "unknown-partial".to_owned());
                Ok(aborted_recovery(AbortedRecoveryInput {
                    path,
                    capture_id,
                    capture_key: None,
                    plan: None,
                    target: None,
                    valid_payload_bytes: 0,
                    last_host_elapsed_us: 0,
                    trailing_bytes: 0,
                    complete_records: 0,
                    reason: error.to_string(),
                    recoverable: false,
                }))
            }
        }
    }

    fn partial_path(&self, capture_id: &str) -> PathBuf {
        self.root.join(format!("capture-{capture_id}.partial"))
    }

    fn completed_path(&self, capture_id: &str) -> PathBuf {
        self.root.join(format!("capture-{capture_id}.capture"))
    }
}

/// Append-only active capture writer that publishes exactly one immutable file.
pub struct CaptureWriter {
    writer: Option<BufWriter<File>>,
    partial_path: PathBuf,
    completed_path: PathBuf,
    header: CaptureHeader,
    max_bytes: u64,
    bytes_written: u64,
    bytes_since_sync: u64,
    blocks: u64,
    payload_bytes: u64,
    raw_hasher: Sha256,
}

impl CaptureWriter {
    fn create(
        partial_path: PathBuf,
        completed_path: PathBuf,
        header: CaptureHeader,
        max_bytes: u64,
    ) -> Result<Self, JlinkError> {
        let header_json = serde_json::to_vec(&header)
            .map_err(|error| storage_error(format!("无法编码 Capture Store 头：{error}")))?;
        validate_json_length(header_json.len())?;
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&partial_path)
            .map_err(|error| {
                storage_error(format!(
                    "无法创建活动 Capture Store {}：{error}",
                    partial_path.display()
                ))
            })?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(FILE_MAGIC)
            .and_then(|()| writer.write_all(&FORMAT_VERSION.to_le_bytes()))
            .and_then(|()| {
                writer.write_all(
                    &u32::try_from(header_json.len())
                        .expect("validated header JSON length fits u32")
                        .to_le_bytes(),
                )
            })
            .and_then(|()| writer.write_all(&crc32(&header_json).to_le_bytes()))
            .and_then(|()| writer.write_all(&header_json))
            .map_err(|error| storage_error(format!("无法写入 Capture Store 头：{error}")))?;
        let bytes_written = FILE_HEADER_BYTES
            + u64::try_from(header_json.len()).expect("validated header length fits u64");
        Ok(Self {
            writer: Some(writer),
            partial_path,
            completed_path,
            header,
            max_bytes,
            bytes_written,
            bytes_since_sync: bytes_written,
            blocks: 0,
            payload_bytes: 0,
            raw_hasher: Sha256::new(),
        })
    }

    /// Returns the active `.partial` path.
    #[must_use]
    pub fn partial_path(&self) -> &Path {
        &self.partial_path
    }

    /// Returns raw payload bytes already covered by verified block boundaries.
    #[must_use]
    pub const fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    /// Appends one independently checksummed raw HSS block.
    ///
    /// # Errors
    ///
    /// Returns a stable size, disk-capacity, or local write error. No existing
    /// verified block is removed when a later append fails.
    pub fn append(
        &mut self,
        host_elapsed_us: u64,
        phase: CapturePhase,
        payload: &[u8],
    ) -> Result<(), JlinkError> {
        if payload.is_empty() {
            return Ok(());
        }
        if payload.len() > MAX_BLOCK_BYTES {
            return Err(capacity_error("单个 Capture Store 块超过 16 MiB"));
        }
        let payload_bytes = u64::try_from(payload.len())
            .map_err(|_| capacity_error("单个 Capture Store 块长度无法表示"))?;
        let payload_len = u32::try_from(payload.len())
            .map_err(|_| capacity_error("单个 Capture Store 块长度超出 u32"))?;
        let block_bytes = BLOCK_HEADER_BYTES + payload_bytes;
        let next_bytes = self
            .bytes_written
            .checked_add(block_bytes)
            .and_then(|value| value.checked_add(TERMINAL_HEADER_BYTES))
            .ok_or_else(|| capacity_error("Capture Store 实际大小溢出"))?;
        if next_bytes > self.max_bytes {
            return Err(
                capacity_error("Capture Store 实际写入超过 capture.max_bytes")
                    .with_detail("next_bytes", json!(next_bytes))
                    .with_detail("max_bytes", json!(self.max_bytes)),
            );
        }
        if self.bytes_since_sync >= SYNC_INTERVAL_BYTES {
            self.checkpoint()?;
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| storage_error("Capture Store writer 已终止"))?;
        writer
            .write_all(BLOCK_MAGIC)
            .and_then(|()| writer.write_all(&payload_len.to_le_bytes()))
            .and_then(|()| writer.write_all(&crc32(payload).to_le_bytes()))
            .and_then(|()| writer.write_all(&host_elapsed_us.to_le_bytes()))
            .and_then(|()| writer.write_all(&phase.as_u32().to_le_bytes()))
            .and_then(|()| writer.write_all(payload))
            .map_err(|error| storage_error(format!("无法追加 Capture Store 块：{error}")))?;
        self.bytes_written += block_bytes;
        self.bytes_since_sync += block_bytes;
        self.blocks += 1;
        self.payload_bytes += payload_bytes;
        self.raw_hasher.update(payload);
        Ok(())
    }

    /// Flushes and commits the current verified block boundary to disk.
    ///
    /// # Errors
    ///
    /// Returns a stable local storage error when flush or `sync_data` fails.
    pub fn checkpoint(&mut self) -> Result<(), JlinkError> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| storage_error("Capture Store writer 已终止"))?;
        writer
            .flush()
            .and_then(|()| writer.get_ref().sync_data())
            .map_err(|error| storage_error(format!("无法提交 Capture Store 检查点：{error}")))?;
        self.bytes_since_sync = 0;
        Ok(())
    }

    /// Writes the terminal manifest, verifies durability, and atomically publishes
    /// the immutable capture resource in the same directory.
    ///
    /// # Errors
    ///
    /// Returns a stable size, serialization, sync, rename, CRC, or digest error.
    pub fn finish(mut self, snapshot: &HssRunSnapshot) -> Result<CaptureSnapshot, JlinkError> {
        if snapshot.capture_id != self.header.capture_id {
            return Err(invalid_store("终态 snapshot 与 Capture Store 身份不一致"));
        }
        let manifest = CaptureManifest {
            snapshot: snapshot.clone(),
            blocks: self.blocks,
            payload_bytes: self.payload_bytes,
            raw_sha256: hex_digest(self.raw_hasher.clone().finalize().as_slice()),
        };
        let manifest_json = serde_json::to_vec(&manifest)
            .map_err(|error| storage_error(format!("无法编码 Capture Store 终态：{error}")))?;
        validate_json_length(manifest_json.len())?;
        let manifest_bytes = u64::try_from(manifest_json.len())
            .map_err(|_| capacity_error("Capture Store 终态清单长度无法表示"))?;
        let manifest_len = u32::try_from(manifest_json.len())
            .map_err(|_| capacity_error("Capture Store 终态清单长度超出 u32"))?;
        let terminal_bytes = TERMINAL_HEADER_BYTES + manifest_bytes;
        if self.bytes_written.saturating_add(terminal_bytes) > self.max_bytes {
            return Err(capacity_error(
                "Capture Store 终态清单超过 capture.max_bytes",
            ));
        }
        let mut writer = self
            .writer
            .take()
            .ok_or_else(|| storage_error("Capture Store writer 已终止"))?;
        writer
            .write_all(TERMINAL_MAGIC)
            .and_then(|()| writer.write_all(&manifest_len.to_le_bytes()))
            .and_then(|()| writer.write_all(&crc32(&manifest_json).to_le_bytes()))
            .and_then(|()| writer.write_all(&manifest_json))
            .and_then(|()| writer.flush())
            .and_then(|()| writer.get_ref().sync_all())
            .map_err(|error| storage_error(format!("无法提交 Capture Store 终态：{error}")))?;
        drop(writer);
        let scan = scan_capture(&self.partial_path, false)?;
        let verified_manifest = scan
            .manifest
            .ok_or_else(|| invalid_store("原子发布前缺少已校验终态清单"))?;
        if self.completed_path.exists() {
            return Err(storage_error("不可变完成 capture 已存在，拒绝原子覆盖"));
        }
        fs::rename(&self.partial_path, &self.completed_path)
            .map_err(|error| storage_error(format!("无法原子发布 Capture Store：{error}")))?;
        Ok(CaptureSnapshot {
            path: self.completed_path,
            header: scan.header,
            manifest: verified_manifest,
        })
    }
}

struct CaptureScan {
    header: CaptureHeader,
    manifest: Option<CaptureManifest>,
    valid_blocks: u64,
    valid_payload_bytes: u64,
    last_host_elapsed_us: u64,
    trailing_bytes: u64,
    crc_error: bool,
    raw_payload: Vec<u8>,
}

fn scan_capture(path: &Path, retain_payload: bool) -> Result<CaptureScan, JlinkError> {
    let (mut file, file_len) = open_capture_file(path)?;
    scan_capture_file(&mut file, file_len, retain_payload)
}

fn open_capture_file(path: &Path) -> Result<(File, u64), JlinkError> {
    let file = File::open(path)
        .map_err(|error| storage_error(format!("无法打开 {}：{error}", path.display())))?;
    let file_len = file
        .metadata()
        .map_err(|error| storage_error(format!("无法读取 capture 元数据：{error}")))?
        .len();
    Ok((file, file_len))
}

fn scan_capture_file(
    file: &mut File,
    file_len: u64,
    retain_payload: bool,
) -> Result<CaptureScan, JlinkError> {
    let mut fixed = [0_u8; FILE_HEADER_LEN];
    file.read_exact(&mut fixed)
        .map_err(|error| invalid_store(format!("Capture Store 头不完整：{error}")))?;
    if &fixed[..8] != FILE_MAGIC || u32::from_le_bytes(fixed[8..12].try_into().unwrap()) != 1 {
        return Err(invalid_store("Capture Store magic 或版本不匹配"));
    }
    let header_len = usize::try_from(u32::from_le_bytes(fixed[12..16].try_into().unwrap()))
        .expect("u32 header length fits usize");
    validate_json_length(header_len)?;
    let header_crc = u32::from_le_bytes(fixed[16..20].try_into().unwrap());
    let mut header_json = vec![0; header_len];
    file.read_exact(&mut header_json)
        .map_err(|error| invalid_store(format!("Capture Store 自描述头不完整：{error}")))?;
    if crc32(&header_json) != header_crc {
        return Err(invalid_store("Capture Store 自描述头 CRC 不匹配"));
    }
    let header: CaptureHeader = serde_json::from_slice(&header_json)
        .map_err(|error| invalid_store(format!("Capture Store 自描述头无效：{error}")))?;
    header.plan.validate()?;
    header.target.validate()?;
    if header.capture_id.trim().is_empty()
        || header.capture_key != header.plan.capture_key()
        || header.request_fingerprint != header.plan.request_fingerprint()
    {
        return Err(invalid_store("Capture Store 自描述身份不一致"));
    }
    let mut scan = CaptureScan {
        header,
        manifest: None,
        valid_blocks: 0,
        valid_payload_bytes: 0,
        last_host_elapsed_us: 0,
        trailing_bytes: 0,
        crc_error: false,
        raw_payload: Vec::new(),
    };
    let mut raw_hasher = Sha256::new();
    loop {
        let position = file
            .stream_position()
            .map_err(|error| storage_error(format!("无法定位 Capture Store：{error}")))?;
        if position == file_len {
            return Ok(scan);
        }
        let mut magic = [0_u8; 4];
        if let Err(error) = file.read_exact(&mut magic) {
            if error.kind() == ErrorKind::UnexpectedEof {
                scan.trailing_bytes = file_len.saturating_sub(position);
                return Ok(scan);
            }
            return Err(storage_error(format!("无法读取 Capture Store 块：{error}")));
        }
        if &magic == TERMINAL_MAGIC {
            read_terminal(file, file_len, &mut scan, &raw_hasher)?;
            return Ok(scan);
        }
        if &magic != BLOCK_MAGIC {
            scan.crc_error = true;
            scan.trailing_bytes = file_len.saturating_sub(position);
            return Ok(scan);
        }
        if !read_block(file, file_len, &mut scan, &mut raw_hasher, retain_payload)? {
            return Ok(scan);
        }
    }
}

fn read_block(
    file: &mut File,
    file_len: u64,
    scan: &mut CaptureScan,
    raw_hasher: &mut Sha256,
    retain_payload: bool,
) -> Result<bool, JlinkError> {
    let mut header = [0_u8; BLOCK_HEADER_REST_LEN];
    if let Err(error) = file.read_exact(&mut header) {
        if error.kind() == ErrorKind::UnexpectedEof {
            scan.trailing_bytes = file_len.saturating_sub(
                file.stream_position()
                    .map_err(|seek_error| storage_error(seek_error.to_string()))?
                    .saturating_sub(u64::try_from(header.len()).unwrap_or(0) + 4),
            );
            return Ok(false);
        }
        return Err(storage_error(format!(
            "无法读取 Capture Store 块头：{error}"
        )));
    }
    let payload_len = usize::try_from(u32::from_le_bytes(header[..4].try_into().unwrap()))
        .expect("u32 payload length fits usize");
    if payload_len > MAX_BLOCK_BYTES {
        scan.crc_error = true;
        return Ok(false);
    }
    let expected_crc = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let host_elapsed_us = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let phase = u32::from_le_bytes(header[16..20].try_into().unwrap());
    if phase > CapturePhase::Tail.as_u32() {
        scan.crc_error = true;
        return Ok(false);
    }
    let mut payload = vec![0; payload_len];
    if let Err(error) = file.read_exact(&mut payload) {
        if error.kind() == ErrorKind::UnexpectedEof {
            scan.trailing_bytes = u64::try_from(payload_len).unwrap_or(u64::MAX);
            return Ok(false);
        }
        return Err(storage_error(format!(
            "无法读取 Capture Store payload：{error}"
        )));
    }
    if crc32(&payload) != expected_crc {
        scan.crc_error = true;
        return Ok(false);
    }
    raw_hasher.update(&payload);
    if retain_payload {
        scan.raw_payload.extend_from_slice(&payload);
    }
    scan.valid_blocks += 1;
    scan.valid_payload_bytes += u64::try_from(payload_len).expect("bounded payload fits u64");
    scan.last_host_elapsed_us = host_elapsed_us;
    Ok(true)
}

fn read_terminal(
    file: &mut File,
    file_len: u64,
    scan: &mut CaptureScan,
    raw_hasher: &Sha256,
) -> Result<(), JlinkError> {
    let mut header = [0_u8; TERMINAL_HEADER_REST_LEN];
    file.read_exact(&mut header)
        .map_err(|error| invalid_store(format!("Capture Store 终态头不完整：{error}")))?;
    let manifest_len = usize::try_from(u32::from_le_bytes(header[..4].try_into().unwrap()))
        .expect("u32 manifest length fits usize");
    validate_json_length(manifest_len)?;
    let expected_crc = u32::from_le_bytes(header[4..8].try_into().unwrap());
    let mut manifest_json = vec![0; manifest_len];
    file.read_exact(&mut manifest_json)
        .map_err(|error| invalid_store(format!("Capture Store 终态不完整：{error}")))?;
    if crc32(&manifest_json) != expected_crc {
        return Err(invalid_store("Capture Store 终态 CRC 不匹配"));
    }
    let manifest: CaptureManifest = serde_json::from_slice(&manifest_json)
        .map_err(|error| invalid_store(format!("Capture Store 终态清单无效：{error}")))?;
    let position = file
        .stream_position()
        .map_err(|error| storage_error(format!("无法定位 Capture Store 终态：{error}")))?;
    if position != file_len
        || manifest.snapshot.capture_id != scan.header.capture_id
        || manifest.blocks != scan.valid_blocks
        || manifest.payload_bytes != scan.valid_payload_bytes
        || manifest.raw_sha256 != hex_digest(raw_hasher.clone().finalize().as_slice())
    {
        return Err(invalid_store("Capture Store 终态清单与已校验块不一致"));
    }
    scan.manifest = Some(manifest);
    Ok(())
}

struct AbortedRecoveryInput {
    path: PathBuf,
    capture_id: String,
    capture_key: Option<String>,
    plan: Option<HssStartPlan>,
    target: Option<TargetConnectionSpec>,
    valid_payload_bytes: u64,
    last_host_elapsed_us: u64,
    trailing_bytes: u64,
    complete_records: u64,
    reason: String,
    recoverable: bool,
}

fn aborted_recovery(input: AbortedRecoveryInput) -> CaptureRecovery {
    let AbortedRecoveryInput {
        path,
        capture_id,
        capture_key,
        plan,
        target,
        valid_payload_bytes,
        last_host_elapsed_us,
        trailing_bytes,
        complete_records,
        reason,
        recoverable,
    } = input;
    let partial_available = valid_payload_bytes > 0;
    let mut notifications = Vec::new();
    if partial_available {
        notifications.push(HssRecoveryNotification::PartialDataRetained {
            complete_records,
            trailing_bytes,
        });
    }
    let mut state = HssCaptureState::starting();
    state
        .mark_aborted(&reason, recoverable, partial_available, notifications)
        .expect("recovery reason is non-blank and starting is non-terminal");
    CaptureRecovery::Aborted {
        capture_id: capture_id.clone(),
        capture_key,
        plan,
        target,
        status: HssRunSnapshot {
            capture_id,
            state: state.lifecycle(),
            integrity: HssDataIntegrity::Unknown,
            elapsed_us: last_host_elapsed_us,
            complete_records,
            drain: HssDrainTiming::default(),
            quality: jlink_domain::HssQualitySummary::default(),
            writes: Vec::new(),
            failure_code: None,
            partial_available,
            reason: state.reason().map(str::to_owned),
            recoverable: state.recoverable(),
            recovery_notifications: state.recovery_notifications().to_vec(),
        },
        path,
    }
}

fn available_disk_bytes(path: &Path) -> Result<u64, JlinkError> {
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut available = 0_u64;
    let mut total = 0_u64;
    let mut total_free = 0_u64;
    // SAFETY: `wide` is NUL-terminated and all out parameters are writable.
    let result = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut available,
            &raw mut total,
            &raw mut total_free,
        )
    };
    if result == 0 {
        // SAFETY: GetLastError is read immediately after the failed Win32 call.
        let code = unsafe { GetLastError() };
        return Err(storage_error(format!(
            "无法读取 Capture Store 可用空间（Windows 错误 {code}）"
        )));
    }
    Ok(available)
}

fn unix_time_us() -> Result<u64, JlinkError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| storage_error(format!("系统时钟早于 UNIX epoch：{error}")))?;
    u64::try_from(duration.as_micros())
        .map_err(|_| storage_error("Capture Store 创建时间超出 u64 微秒范围"))
}

fn validate_capture_id(capture_id: &str) -> Result<(), JlinkError> {
    if capture_id.is_empty()
        || !capture_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(JlinkError::new(
            ErrorCode::ValueInvalid,
            "capture_id 只能包含 ASCII 字母、数字、连字符或下划线",
            false,
        ));
    }
    Ok(())
}

fn validate_json_length(length: usize) -> Result<(), JlinkError> {
    if length == 0 || length > MAX_JSON_BYTES {
        Err(invalid_store("Capture Store JSON 长度超出 1..16 MiB"))
    } else {
        Ok(())
    }
}

fn capture_id_from_partial_path(path: &Path) -> Option<String> {
    capture_id_from_store_path(path, "partial")
}

fn capture_id_from_store_path(path: &Path, extension: &str) -> Option<String> {
    if path.extension()?.to_str()? != extension {
        return None;
    }
    path.file_stem()?
        .to_str()?
        .strip_prefix("capture-")
        .map(str::to_owned)
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn capacity_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::HssUnsupported, message, false)
}

fn storage_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ExecutionUncertain, message, false)
}

fn invalid_store(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::FrameInvalid, message, false)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::OpenOptions,
        io::{Seek, SeekFrom, Write},
    };

    use jlink_domain::{
        AccessLayout, AccessPlan, FirmwareIdentityPlan, HssDataIntegrity, HssDrainTiming,
        HssReturnWhen, HssRunSnapshot, HssRunState, HssStartPlan, ScalarEncoding,
        TargetConnectionSpec, TargetInterface, VariableSelector,
    };
    use serde_json::json;
    use tempfile::tempdir;

    use super::{CapturePhase, CaptureRecovery, CaptureStore};

    fn start_plan() -> HssStartPlan {
        let firmware: FirmwareIdentityPlan = serde_json::from_value(json!({
            "elf_sha256": "11".repeat(32),
            "segments": [{
                "address": 0,
                "length": 4,
                "sha256": "22".repeat(32)
            }]
        }))
        .expect("firmware fixture");
        let access = AccessPlan::new(
            "11".repeat(32),
            VariableSelector::new("fixture", None).expect("selector"),
            0x2000_0000,
            4,
            None,
            false,
            AccessLayout::Scalar {
                name: "uint32_t".to_owned(),
                byte_size: 4,
                encoding: ScalarEncoding::Unsigned,
            },
        );
        HssStartPlan::new(
            "store-fixture",
            1,
            1_000,
            HssReturnWhen::Completed,
            vec![access],
            Vec::new(),
            firmware,
        )
        .expect("start plan")
    }

    fn target() -> TargetConnectionSpec {
        TargetConnectionSpec::new(
            "S32K144",
            TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target fixture")
    }

    fn completed_snapshot(capture_id: &str) -> HssRunSnapshot {
        HssRunSnapshot {
            capture_id: capture_id.to_owned(),
            state: HssRunState::Completed,
            integrity: HssDataIntegrity::Unknown,
            elapsed_us: 1_000_000,
            complete_records: 1,
            drain: HssDrainTiming::default(),
            quality: jlink_domain::HssQualitySummary::default(),
            writes: Vec::new(),
            failure_code: None,
            partial_available: false,
            reason: None,
            recoverable: None,
            recovery_notifications: Vec::new(),
        }
    }

    #[test]
    fn t_p3_store_preflights_limit_and_atomically_publishes_immutable_snapshot() {
        let directory = tempdir().expect("temporary store");
        let store = CaptureStore::open(directory.path()).expect("store opens");
        let plan = start_plan();
        let limit_error = store
            .preflight(&plan, 1)
            .expect_err("configured limit is enforced before writer creation");
        assert_eq!(limit_error.code, jlink_domain::ErrorCode::HssUnsupported);

        let estimate = store
            .preflight(&plan, 16 * 1024 * 1024)
            .expect("bounded fixture fits disk and project limit");
        assert_eq!(estimate.raw_bytes(), 8_000);
        assert!(estimate.storage_bytes() > estimate.raw_bytes());

        let mut writer = store
            .create_writer("cap-store", &target(), &plan, 16 * 1024 * 1024)
            .expect("partial writer");
        let partial = writer.partial_path().to_path_buf();
        writer
            .append(
                10,
                CapturePhase::Live,
                &[1_u32.to_le_bytes(), 7_u32.to_le_bytes()].concat(),
            )
            .expect("checksummed block");
        writer.checkpoint().expect("durable block boundary");
        assert!(partial.exists());
        let snapshot = writer
            .finish(&completed_snapshot("cap-store"))
            .expect("atomic publish");
        assert!(!partial.exists());
        assert!(snapshot.path().exists());
        assert_eq!(snapshot.capture_key(), "store-fixture");
        assert_eq!(snapshot.payload_bytes(), 8);
        assert_eq!(snapshot.raw_sha256().len(), 64);
        assert_eq!(
            store
                .open_snapshot("cap-store")
                .expect("immutable snapshot reopens"),
            snapshot
        );
        assert_eq!(
            store.completed_snapshots().expect("completed index scan"),
            [snapshot]
        );
        assert!(
            store
                .create_writer("cap-store", &target(), &plan, 16 * 1024 * 1024)
                .is_err(),
            "completed capture is never overwritten"
        );
    }

    #[test]
    fn t_p3_store_recovers_verified_partial_blocks_as_aborted_unknown() {
        let directory = tempdir().expect("temporary store");
        let store = CaptureStore::open(directory.path()).expect("store opens");
        let plan = start_plan();
        let mut writer = store
            .create_writer("cap-partial", &target(), &plan, 16 * 1024 * 1024)
            .expect("partial writer");
        writer
            .append(
                50,
                CapturePhase::Tail,
                &[1_u32.to_le_bytes(), 7_u32.to_le_bytes()].concat(),
            )
            .expect("checksummed block");
        writer.checkpoint().expect("durable block boundary");
        let partial = writer.partial_path().to_path_buf();
        drop(writer);

        let recovery = store.recover_partials().expect("startup recovery scan");
        assert_eq!(recovery.len(), 1);
        let CaptureRecovery::Aborted {
            capture_id,
            capture_key,
            plan: _,
            target: recovered_target,
            status,
            path,
        } = &recovery[0]
        else {
            panic!("unterminated partial must be aborted");
        };
        assert_eq!(capture_id, "cap-partial");
        assert_eq!(capture_key.as_deref(), Some("store-fixture"));
        assert_eq!(recovered_target.as_ref(), Some(&target()));
        assert_eq!(status.state, HssRunState::Aborted);
        assert_eq!(status.integrity, HssDataIntegrity::Unknown);
        assert!(status.partial_available);
        assert_eq!(status.complete_records, 1);
        assert_eq!(path, &partial);
        assert!(partial.exists(), "recovery never deletes partial evidence");
    }

    #[test]
    fn t_p3_store_crc_corruption_never_becomes_valid_partial_data() {
        let directory = tempdir().expect("temporary store");
        let store = CaptureStore::open(directory.path()).expect("store opens");
        let plan = start_plan();
        let mut writer = store
            .create_writer("cap-corrupt", &target(), &plan, 16 * 1024 * 1024)
            .expect("partial writer");
        writer
            .append(
                50,
                CapturePhase::Live,
                &[1_u32.to_le_bytes(), 7_u32.to_le_bytes()].concat(),
            )
            .expect("checksummed block");
        writer.checkpoint().expect("durable block boundary");
        let partial = writer.partial_path().to_path_buf();
        drop(writer);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&partial)
            .expect("corrupt fixture opens");
        file.seek(SeekFrom::End(-1)).expect("last payload byte");
        file.write_all(&[0xFF]).expect("corrupt payload");
        file.sync_all().expect("corruption durable");
        drop(file);

        let recovery = store.recover_partials().expect("startup recovery scan");
        let CaptureRecovery::Aborted { status, .. } = &recovery[0] else {
            panic!("CRC-corrupt partial must be aborted");
        };
        assert_eq!(status.state, HssRunState::Aborted);
        assert!(!status.partial_available);
        assert_eq!(status.recoverable, Some(false));
    }
}
