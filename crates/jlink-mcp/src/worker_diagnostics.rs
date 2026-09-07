//! Bounded, best-effort Worker evidence. It does not infer a crash cause.
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    process::ChildStderr,
    sync::Arc,
    thread,
};

use jlink_domain::{ErrorCode, JlinkError};
use serde_json::{Value, json};
use windows_sys::Win32::{
    Foundation::{GetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        TerminateProcess, WaitForSingleObject,
    },
};

const MAX_LOG_BYTES: usize = 32 * 1024;
const OMITTED: &[u8] = b"[older worker output omitted]\n";

#[derive(Clone, Debug)]
pub(super) struct WorkerDiagnostics {
    pid: u32,
    path: PathBuf,
    dll_sha256: String,
    owned_process: Option<Arc<OwnedHandle>>,
}

impl PartialEq for WorkerDiagnostics {
    fn eq(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.path == other.path
            && self.dll_sha256 == other.dll_sha256
            && match (&self.owned_process, &other.owned_process) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
    }
}
impl Eq for WorkerDiagnostics {}

impl WorkerDiagnostics {
    pub(super) fn new(root: &Path, pid: u32, dll_sha256: &str) -> Self {
        Self {
            pid,
            path: root.join(format!("worker-{pid}.log")),
            dll_sha256: dll_sha256.to_owned(),
            owned_process: None,
        }
    }

    pub(super) fn retain_child(&mut self, child: &std::process::Child) -> Result<(), JlinkError> {
        self.owned_process = Some(Arc::new(
            child
                .as_handle()
                .try_clone_to_owned()
                .map_err(|error| diagnostic_error(&error))?,
        ));
        Ok(())
    }

    pub(super) fn terminate_timed_out_child(&self) -> Value {
        let Some(process) = &self.owned_process else {
            return json!({"terminated": false, "reason": "no owned process handle; foreign/attached Worker is not terminated"});
        };
        // SAFETY: this duplicated handle identifies the exact child we spawned,
        // never a process found later by a potentially recycled PID.
        let result = unsafe { TerminateProcess(process.as_raw_handle(), 0xE000_0001) };
        if result == 0 {
            return json!({"terminated": false, "error": io::Error::last_os_error().to_string()});
        }
        // SAFETY: the retained process handle has synchronization rights.
        let observed = unsafe { WaitForSingleObject(process.as_raw_handle(), 1_000) };
        json!({"terminated": true, "exit_observed": observed == WAIT_OBJECT_0})
    }

    pub(super) fn capture_stderr(&self, stderr: ChildStderr) -> Result<(), JlinkError> {
        let prepare = || -> io::Result<File> {
            fs::create_dir_all(self.path.parent().expect("diagnostic root"))?;
            File::create(&self.path)
        };
        // Diagnostics are optional; still drain stderr when storage is unavailable.
        let file = prepare().ok();
        thread::Builder::new()
            .name(format!("jlink-stderr-{}", self.pid))
            .spawn(move || drain_output(stderr, file))
            .map_err(|error| diagnostic_error(&error))?;
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

fn diagnostic_error(error: &io::Error) -> JlinkError {
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

fn drain_output(mut input: impl Read, file: Option<File>) {
    let mut destination = file;
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
        drain_output(
            io::Cursor::new(bytes),
            Some(File::create(&context.path).unwrap()),
        );
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

    #[test]
    fn unavailable_log_storage_still_drains_a_real_child_stderr_pipe() {
        use std::{
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let root = tempfile::tempdir().unwrap();
        let blocked = root.path().join("not-a-directory");
        fs::write(&blocked, b"occupied").unwrap();
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-Command",
                "[Console]::Error.Write('x' * 131072)",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let context = WorkerDiagnostics::new(&blocked, child.id(), &"00".repeat(32));
        context
            .capture_stderr(child.stderr.take().unwrap())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("stderr pipe blocked child");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(context.log_evidence()["available"], false);
    }
}
