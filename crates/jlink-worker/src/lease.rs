use std::{
    fs::{self, File},
    io::{Seek, Write},
    os::windows::{ffi::OsStrExt, io::AsRawHandle, io::FromRawHandle, io::OwnedHandle},
    path::{Path, PathBuf},
    ptr,
};

use jlink_domain::{ErrorCode, JlinkError, probe_identity_hash};
use windows_sys::Win32::{
    Foundation::{
        ERROR_LOCK_VIOLATION, GENERIC_READ, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, LOCKFILE_EXCLUSIVE_LOCK,
        LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, OPEN_ALWAYS, UnlockFileEx,
    },
    System::IO::OVERLAPPED,
};

/// Exclusive cross-process ownership of one probe identity.
pub(crate) struct ProbeLease {
    file: File,
    _path: PathBuf,
}

impl ProbeLease {
    /// Acquires the first byte of a stable lock file and holds it until drop.
    pub(crate) fn acquire(root: &Path, identity: &str) -> Result<Self, JlinkError> {
        fs::create_dir_all(root).map_err(|error| {
            JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("无法创建探针租约目录 {}：{error}", root.display()),
                true,
            )
        })?;
        let path = root.join(format!("probe-{}.lock", probe_identity_hash(identity)?));
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is NUL-terminated, security attributes are intentionally null,
        // and the returned handle is immediately transferred to `OwnedHandle`.
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_ALWAYS,
                0,
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(last_worker_error("无法打开探针租约文件", true));
        }
        // SAFETY: `raw` is a unique valid handle returned by CreateFileW.
        let owned = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: the handle remains valid and `overlapped` lives for the synchronous call.
        let locked = unsafe {
            LockFileEx(
                owned.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &raw mut overlapped,
            )
        };
        if locked == 0 {
            // SAFETY: GetLastError is read immediately after LockFileEx failed.
            let code = unsafe { GetLastError() };
            if code == ERROR_LOCK_VIOLATION {
                return Err(JlinkError::new(
                    ErrorCode::ProbeBusy,
                    "目标探针正在被另一个 Worker 使用",
                    true,
                ));
            }
            return Err(windows_error("无法取得探针租约", code, true));
        }

        let mut lease = Self {
            file: File::from(owned),
            _path: path,
        };
        lease.file.set_len(0).map_err(|error| {
            JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("无法更新探针租约记录：{error}"),
                true,
            )
        })?;
        lease.file.rewind().map_err(|error| {
            JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("无法定位探针租约记录：{error}"),
                true,
            )
        })?;
        writeln!(lease.file, "pid={}", std::process::id()).map_err(|error| {
            JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("无法写入探针租约记录：{error}"),
                true,
            )
        })?;
        lease.file.sync_all().map_err(|error| {
            JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("无法同步探针租约记录：{error}"),
                true,
            )
        })?;
        Ok(lease)
    }
}

impl Drop for ProbeLease {
    fn drop(&mut self) {
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: this object still owns the locked handle and unlocks the same byte range.
        let _ = unsafe { UnlockFileEx(self.file.as_raw_handle(), 0, 1, 0, &raw mut overlapped) };
    }
}

fn last_worker_error(context: &str, retryable: bool) -> JlinkError {
    // SAFETY: GetLastError has no preconditions and is called at the failure site.
    windows_error(context, unsafe { GetLastError() }, retryable)
}

fn windows_error(context: &str, code: u32, retryable: bool) -> JlinkError {
    JlinkError::new(
        ErrorCode::WorkerUnavailable,
        format!("{context}（Windows 错误 {code}）"),
        retryable,
    )
}
