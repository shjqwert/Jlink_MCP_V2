//! Bounded, best-effort Worker evidence. It does not infer a crash cause.
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    process::ChildStderr,
    thread,
};

use jlink_domain::{ErrorCode, JlinkError};
use serde_json::{Value, json};
use windows_sys::Win32::{
    Foundation::{GetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    },
};

const MAX_LOG_BYTES: usize = 32 * 1024;
const OMITTED: &[u8] = b"[older worker output omitted]\n";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkerDiagnostics {
    pid: u32,
    path: PathBuf,
    dll_sha256: String,
}

impl WorkerDiagnostics {
    pub(super) fn new(root: &Path, pid: u32, dll_sha256: &str) -> Self {
        Self {
            pid,
            path: root.join(format!("worker-{pid}.log")),
            dll_sha256: dll_sha256.to_owned(),
        }
    }

    pub(super) fn capture_stderr(&self, stderr: ChildStderr) -> Result<(), JlinkError> {
        let prepare = || -> io::Result<File> {
            fs::create_dir_all(self.path.parent().expect("diagnostic root"))?;
            File::create(&self.path)
        };
        let file = prepare().map_err(diagnostic_error)?;
        thread::Builder::new()
            .name(format!("jlink-stderr-{}", self.pid))
            .spawn(move || drain_output(stderr, file))
            .map_err(diagnostic_error)?;
        Ok(())
    }

    pub(super) fn enrich(&self, error: JlinkError) -> JlinkError {
        error
            .with_detail("worker", process_evidence(self.pid))
            .with_detail("dll_sha256", json!(self.dll_sha256))
            .with_detail("worker_log", self.log_evidence())
    }

    fn log_evidence(&self) -> Value {
        let read = || -> io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            File::open(&self.path)?
                .take(u64::try_from(MAX_LOG_BYTES + 1).expect("fixed diagnostic bound"))
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        match read() {
            Ok(bytes) if bytes.len() <= MAX_LOG_BYTES => json!({
                "path": self.path.display().to_string(), "available": true,
                "max_bytes": MAX_LOG_BYTES, "tail": String::from_utf8_lossy(&bytes),
                "collector_may_lag": true,
                "interpretation": "last recorded output only; not proof of the final native call or a DLL crash"
            }),
            Ok(_) => json!({ "available": false, "reason": "diagnostic file exceeded its bound" }),
            Err(error) => {
                json!({ "available": false, "reason": error.to_string(), "path": self.path.display().to_string() })
            }
        }
    }
}

fn diagnostic_error(error: io::Error) -> JlinkError {
    JlinkError::new(
        ErrorCode::WorkerUnavailable,
        format!("Cannot start bounded Worker diagnostics: {error}"),
        false,
    )
}

#[derive(Default)]
struct TailBuffer {
    bytes: Vec<u8>,
    truncated: bool,
}

impl TailBuffer {
    fn append(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        let keep = MAX_LOG_BYTES - OMITTED.len();
        let remove = self.bytes.len().saturating_sub(keep);
        if remove > 0 {
            self.bytes.drain(..remove);
            self.truncated = true;
        }
    }

    fn persist(&self, file: &mut File) -> io::Result<()> {
        file.seek(SeekFrom::Start(0))?;
        let prefix = if self.truncated { OMITTED } else { &[] };
        file.write_all(prefix)?;
        file.write_all(&self.bytes)?;
        file.set_len(
            u64::try_from(prefix.len() + self.bytes.len()).expect("bounded diagnostic bytes"),
        )?;
        file.flush()
    }
}

fn drain_output(mut input: impl Read, file: File) {
    let mut destination = Some(file);
    let mut tail = TailBuffer::default();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        tail.append(&buffer[..count]);
        if let Some(file) = destination.as_mut()
            && tail.persist(file).is_err()
        {
            // Keep consuming stderr even when disk writes fail. Closing the pipe
            // or ceasing to drain it must not block or kill the Worker.
            destination = None;
        }
    }
}

fn process_evidence(pid: u32) -> Value {
    // SAFETY: read-only process query/synchronization; no caller pointers are passed.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        // SAFETY: queried immediately after OpenProcess failed.
        let code = unsafe { GetLastError() };
        return json!({ "pid": pid, "observed_state": "unknown", "inspection_error": code });
    }
    // SAFETY: OpenProcess returned one uniquely owned valid handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: the owned process handle is alive and has synchronization access.
    let wait = unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) };
    match wait {
        WAIT_TIMEOUT => json!({ "pid": pid, "observed_state": "running" }),
        WAIT_OBJECT_0 => {
            let mut exit_code = 0_u32;
            // SAFETY: the process handle is alive and exit_code is writable.
            if unsafe { GetExitCodeProcess(handle.as_raw_handle(), &raw mut exit_code) } == 0 {
                // SAFETY: queried immediately after GetExitCodeProcess failed.
                let code = unsafe { GetLastError() };
                json!({ "pid": pid, "observed_state": "exited", "exit_code": null, "inspection_error": code })
            } else {
                json!({ "pid": pid, "observed_state": "exited", "exit_code": exit_code })
            }
        }
        _ => json!({ "pid": pid, "observed_state": "unknown", "wait_result": wait }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_keep_a_bounded_tail_without_changing_uncertainty() {
        let directory = tempfile::tempdir().unwrap();
        let context =
            WorkerDiagnostics::new(directory.path(), std::process::id(), &"ab".repeat(32));
        let mut bytes = vec![b'x'; MAX_LOG_BYTES * 3];
        bytes.extend_from_slice(b"\nhss native_call=Start boundary=enter\n");
        drain_output(io::Cursor::new(bytes), File::create(&context.path).unwrap());
        let stored = fs::read(&context.path).unwrap();
        assert!(stored.len() <= MAX_LOG_BYTES);
        assert!(stored.starts_with(OMITTED));
        assert!(stored.ends_with(b"boundary=enter\n"));
        let error = context.enrich(JlinkError::new(
            ErrorCode::ExecutionUncertain,
            "lost response",
            false,
        ));
        assert_eq!(error.code, ErrorCode::ExecutionUncertain);
        assert!(!error.retryable);
        let details = error.details.unwrap();
        assert_eq!(details["worker"]["observed_state"], "running");
        assert_eq!(details["worker_log"]["available"], true);
    }

    #[test]
    fn diagnostics_missing_log_remains_unknown_not_an_inferred_crash() {
        let directory = tempfile::tempdir().unwrap();
        let context =
            WorkerDiagnostics::new(directory.path(), std::process::id(), &"ab".repeat(32));
        let error = context.enrich(JlinkError::new(
            ErrorCode::WorkerUnavailable,
            "endpoint missing",
            true,
        ));
        assert_eq!(error.code, ErrorCode::WorkerUnavailable);
        assert!(error.retryable);
        let details = error.details.unwrap();
        assert_eq!(details["worker_log"]["available"], false);
        assert_eq!(details["worker"]["observed_state"], "running");
    }
}
