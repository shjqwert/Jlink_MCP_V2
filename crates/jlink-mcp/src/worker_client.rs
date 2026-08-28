//! Attach-first Windows client for the versioned local Worker transport.

use std::{
    fs::File,
    os::windows::{
        ffi::OsStrExt,
        io::{FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use jlink_domain::{
    ControlRequest, DebugRequest, DebugResult, DispatchState, ErrorCode, HssRunSnapshot,
    HssStartPlan, IpcRequest, IpcResponse, JlinkError, ProgramRequest, ProtocolVersion, RequestId,
    SessionCommand, TargetConnectionSpec, ValidationAfter, ValidationReport, WorkerStatus,
    classify_worker_loss, read_ipc_frame, worker_endpoint_name, write_ipc_frame,
};
use serde_json::json;
use windows_sys::Win32::{
    Foundation::{GENERIC_READ, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{CreateFileW, OPEN_EXISTING},
    System::Pipes::WaitNamedPipeW,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);
const ATTACH_POLL: Duration = Duration::from_millis(20);
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Immutable process launch inputs for one probe-specific Worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerLaunchSpec {
    /// Path to the built `jlink-worker.exe`.
    pub executable: PathBuf,
    /// Directory containing stable probe lease files.
    pub lease_root: PathBuf,
    /// Project-local root for capture files, before probe identity partitioning.
    pub capture_root: PathBuf,
    /// Configured probe serial or other unique local identity.
    pub probe_identity: String,
    /// Identity-validated J-Link DLL path.
    pub dll_path: PathBuf,
}

/// Result of attaching to an existing Worker or starting the unique owner.
pub struct WorkerAttachment {
    /// Client bound to the stable probe endpoint.
    pub client: WorkerClient,
    /// Status observed after attachment completed.
    pub status: WorkerStatus,
    /// Whether this call started the authoritative Worker.
    pub spawned: bool,
    child: Option<Child>,
}

impl WorkerAttachment {
    /// Returns the spawned process handle when this call created the Worker.
    pub fn spawned_child_mut(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }

    /// Requests bounded graceful cleanup and waits for an owned Worker to exit.
    ///
    /// # Errors
    ///
    /// Returns a stable cleanup, transport, protocol, or process-wait error.
    pub fn shutdown(&mut self) -> Result<(), JlinkError> {
        self.client.shutdown()?;
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if child
                .try_wait()
                .map_err(|error| {
                    JlinkError::new(
                        ErrorCode::WorkerUnavailable,
                        format!("无法检查关闭中的 jlink-worker：{error}"),
                        false,
                    )
                })?
                .is_some()
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(JlinkError::new(
                    ErrorCode::WorkerUnavailable,
                    "jlink-worker 未在两秒正常关闭边界内退出",
                    false,
                ));
            }
            thread::sleep(ATTACH_POLL);
        }
    }
}

/// Stateless request client for one stable Worker named-pipe endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerClient {
    endpoint: String,
}

impl WorkerClient {
    /// Derives the endpoint without embedding the raw probe identity.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ConfigInvalid`] when the identity is blank.
    pub fn for_probe(identity: &str) -> Result<Self, JlinkError> {
        Ok(Self {
            endpoint: worker_endpoint_name(identity)?,
        })
    }

    /// Returns the internal endpoint for diagnostics and process tests.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Reads the authoritative Worker process identity without touching the DLL.
    ///
    /// # Errors
    ///
    /// Returns a stable transport, protocol, or Worker error.
    pub fn status(&self) -> Result<WorkerStatus, JlinkError> {
        let response = self.request(SessionCommand::Status, |request| request)?;
        response_result(response).and_then(|value| {
            serde_json::from_value(value).map_err(|error| {
                JlinkError::new(
                    ErrorCode::IpcProtocolError,
                    format!("Worker 状态响应无效：{error}"),
                    false,
                )
            })
        })
    }

    /// Establishes the only active target using explicit immutable inputs.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration, connection, recovery, validation, or
    /// transport error without selecting a different probe or interface.
    pub fn connect(&self, target: &TargetConnectionSpec) -> Result<WorkerStatus, JlinkError> {
        let response = self.request(SessionCommand::Connect, |request| {
            request.with_target(target.clone())
        })?;
        response_result(response).and_then(|value| {
            serde_json::from_value(value).map_err(|error| {
                JlinkError::new(
                    ErrorCode::IpcProtocolError,
                    format!("Worker 连接响应无效：{error}"),
                    false,
                )
            })
        })
    }

    /// Performs a fresh explicit environment validation pass.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration, connection, or transport error when a
    /// validation report cannot be completed.
    pub fn validate(
        &self,
        target: &TargetConnectionSpec,
        after: Option<ValidationAfter>,
    ) -> Result<ValidationReport, JlinkError> {
        let response = self.request(SessionCommand::Validate, |request| {
            let request = request.with_target(target.clone());
            match after {
                Some(after) => request.with_validation_after(after),
                None => request,
            }
        })?;
        response_result(response).and_then(|value| {
            serde_json::from_value(value).map_err(|error| {
                JlinkError::new(
                    ErrorCode::IpcProtocolError,
                    format!("Worker 验证响应无效：{error}"),
                    false,
                )
            })
        })
    }

    /// Executes one typed Flash request against the already connected target.
    ///
    /// # Errors
    ///
    /// Returns a stable boundary, conflict, target, verification, transport, or
    /// execution-uncertain error. A successful response must be an empty object.
    pub fn program(
        &self,
        target: &TargetConnectionSpec,
        program: &ProgramRequest,
    ) -> Result<(), JlinkError> {
        let command = match program {
            ProgramRequest::Flash { .. } => SessionCommand::Flash,
            ProgramRequest::Erase { .. } => SessionCommand::Erase,
            ProgramRequest::Verify { .. } => SessionCommand::Verify,
        };
        let value = response_result(self.request(command, |request| {
            request
                .with_target(target.clone())
                .with_program(program.clone())
        })?)?;
        if value.as_object().is_none_or(|object| !object.is_empty()) {
            return Err(JlinkError::new(
                ErrorCode::IpcProtocolError,
                "Worker 烧录响应必须是空对象",
                false,
            ));
        }
        Ok(())
    }

    /// Executes one typed raw-memory or ELF-bound variable request.
    ///
    /// # Errors
    ///
    /// Returns a stable boundary, identity, target, verification, transport, or
    /// execution-uncertain error.
    pub fn debug(
        &self,
        target: &TargetConnectionSpec,
        debug: &DebugRequest,
    ) -> Result<DebugResult, JlinkError> {
        let command = match debug {
            DebugRequest::ReadMemory { .. } => SessionCommand::ReadMemory,
            DebugRequest::WriteMemory { .. } => SessionCommand::WriteMemory,
            DebugRequest::ReadVariable { .. } => SessionCommand::ReadVariable,
            DebugRequest::WriteVariable { .. } => SessionCommand::WriteVariable,
            DebugRequest::ReadRegister { .. } => SessionCommand::ReadRegister,
            DebugRequest::WriteRegister { .. } => SessionCommand::WriteRegister,
        };
        let value = response_result(self.request(command, |request| {
            request
                .with_target(target.clone())
                .with_debug(debug.clone())
        })?)?;
        serde_json::from_value(value).map_err(|error| {
            JlinkError::new(
                ErrorCode::IpcProtocolError,
                format!("Worker 变量/内存响应无效：{error}"),
                false,
            )
        })
    }

    /// Executes one explicit target-control action against the active target.
    ///
    /// # Errors
    ///
    /// Returns a stable conflict, state, recovery, transport, or
    /// execution-uncertain error. Success is always an empty object.
    pub fn control(
        &self,
        target: &TargetConnectionSpec,
        control: ControlRequest,
    ) -> Result<(), JlinkError> {
        let value = response_result(self.request(SessionCommand::Control, |request| {
            request.with_target(target.clone()).with_control(control)
        })?)?;
        if value.as_object().is_none_or(|object| !object.is_empty()) {
            return Err(JlinkError::new(
                ErrorCode::IpcProtocolError,
                "Worker 目标控制响应必须是空对象",
                false,
            ));
        }
        Ok(())
    }

    /// Requests a clean Worker exit after its current response is flushed.
    ///
    /// # Errors
    ///
    /// Returns a stable transport, protocol, or Worker error.
    pub fn disconnect(&self) -> Result<(), JlinkError> {
        let value = response_result(self.request(SessionCommand::Disconnect, |request| request)?)?;
        if value.as_object().is_none_or(|object| !object.is_empty()) {
            return Err(JlinkError::new(
                ErrorCode::IpcProtocolError,
                "Worker 断开响应必须是空对象",
                false,
            ));
        }
        Ok(())
    }

    /// Stops active HSS safely, disconnects the target, and terminates the Worker.
    ///
    /// # Errors
    ///
    /// Returns a stable transport, protocol, HSS cleanup, or target disconnect error.
    pub fn shutdown(&self) -> Result<(), JlinkError> {
        let value = response_result(self.request(SessionCommand::Shutdown, |request| request)?)?;
        if value.as_object().is_none_or(|object| !object.is_empty()) {
            return Err(JlinkError::new(
                ErrorCode::IpcProtocolError,
                "Worker 关闭响应必须是空对象",
                false,
            ));
        }
        Ok(())
    }

    /// Starts one fixed-duration HSS plan or recovers its idempotent identity.
    ///
    /// # Errors
    ///
    /// Returns a stable preflight, start, protocol, transport, or Worker error.
    pub fn start_hss(
        &self,
        target: &TargetConnectionSpec,
        plan: &HssStartPlan,
        capture_max_bytes: u64,
    ) -> Result<HssRunSnapshot, JlinkError> {
        let response = self.request(SessionCommand::HssStart, |request| {
            request
                .with_target(target.clone())
                .with_hss_start(plan.clone())
                .with_capture_max_bytes(capture_max_bytes)
        })?;
        response_result(response).and_then(parse_hss_snapshot)
    }

    /// Polls the internal Worker-owned status for one known capture identity.
    ///
    /// # Errors
    ///
    /// Returns a stable identity, protocol, transport, or Worker error.
    pub fn hss_status(&self, capture_id: &str) -> Result<HssRunSnapshot, JlinkError> {
        let response = self.request(SessionCommand::HssStatus, |request| {
            request.with_capture_id(capture_id)
        })?;
        response_result(response).and_then(parse_hss_snapshot)
    }

    /// Polls the internal Worker-owned status through an Agent recovery key.
    ///
    /// # Errors
    ///
    /// Returns a stable identity, protocol, transport, or Worker error.
    pub fn hss_status_by_key(&self, capture_key: &str) -> Result<HssRunSnapshot, JlinkError> {
        let response = self.request(SessionCommand::HssStatus, |request| {
            request.with_capture_key(capture_key)
        })?;
        response_result(response).and_then(parse_hss_snapshot)
    }

    fn request<F>(&self, command: SessionCommand, configure: F) -> Result<IpcResponse, JlinkError>
    where
        F: FnOnce(IpcRequest) -> IpcRequest,
    {
        let request_id = RequestId::new(format!(
            "{}-{}",
            std::process::id(),
            NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
        ))?;
        let request = configure(IpcRequest::new(
            ProtocolVersion::V1,
            request_id.clone(),
            command,
        ));
        let mut pipe = open_pipe(&self.endpoint, 100)?;
        write_ipc_frame(&mut pipe, &request)
            .map_err(|error| dispatched_request_error(command, &error))?;
        let response: IpcResponse =
            read_ipc_frame(&mut pipe).map_err(|error| dispatched_request_error(command, &error))?;
        response
            .validate()
            .map_err(|error| dispatched_request_error(command, &error))?;
        if response.protocol_version != ProtocolVersion::V1 || response.request_id != request_id {
            let error = JlinkError::new(
                ErrorCode::IpcProtocolError,
                "Worker 响应版本或 request_id 与请求不一致",
                false,
            );
            return Err(dispatched_request_error(command, &error));
        }
        Ok(response)
    }
}

fn parse_hss_snapshot(value: serde_json::Value) -> Result<HssRunSnapshot, JlinkError> {
    serde_json::from_value(value).map_err(|error| {
        JlinkError::new(
            ErrorCode::IpcProtocolError,
            format!("Worker HSS 状态响应无效：{error}"),
            false,
        )
    })
}

/// Attaches to an existing Worker before starting a new process for the probe.
///
/// # Errors
///
/// Returns immediately for non-connectivity protocol errors. After a spawn, it
/// returns [`ErrorCode::WorkerUnavailable`] when the process exits early or the
/// endpoint does not become reachable before the fixed deadline.
pub fn attach_or_spawn(spec: &WorkerLaunchSpec) -> Result<WorkerAttachment, JlinkError> {
    let client = WorkerClient::for_probe(&spec.probe_identity)?;
    match client.status() {
        Ok(status) => {
            ensure_current_parent(&status)?;
            return Ok(WorkerAttachment {
                client,
                status,
                spawned: false,
                child: None,
            });
        }
        Err(error) if error.code != ErrorCode::WorkerUnavailable => return Err(error),
        Err(_) => {}
    }

    let mut command = Command::new(&spec.executable);
    command
        .arg("--lease-root")
        .arg(&spec.lease_root)
        .arg("--capture-root")
        .arg(&spec.capture_root)
        .arg("--probe")
        .arg(&spec.probe_identity)
        .arg("--dll")
        .arg(&spec.dll_path)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|error| {
        JlinkError::new(
            ErrorCode::WorkerUnavailable,
            format!("无法启动 jlink-worker：{error}"),
            true,
        )
    })?;

    let deadline = Instant::now() + ATTACH_TIMEOUT;
    loop {
        match client.status() {
            Ok(status) => {
                if status.worker_pid != child.id() {
                    stop_non_authoritative_child(&mut child)?;
                    ensure_current_parent(&status)?;
                    return Ok(WorkerAttachment {
                        client,
                        status,
                        spawned: false,
                        child: None,
                    });
                }
                return Ok(WorkerAttachment {
                    client,
                    status,
                    spawned: true,
                    child: Some(child),
                });
            }
            Err(error) if error.code != ErrorCode::WorkerUnavailable => return Err(error),
            Err(_) => {}
        }
        if let Some(status) = child.try_wait().map_err(|error| {
            JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("无法检查 jlink-worker 状态：{error}"),
                true,
            )
        })? {
            return Err(JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("jlink-worker 在建立端点前退出：{status}"),
                true,
            ));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(JlinkError::new(
                ErrorCode::WorkerUnavailable,
                "jlink-worker 未在 5 秒内建立本机端点",
                true,
            ));
        }
        thread::sleep(ATTACH_POLL);
    }
}

fn ensure_current_parent(status: &WorkerStatus) -> Result<(), JlinkError> {
    let current_pid = std::process::id();
    if status.parent_pid == current_pid {
        return Ok(());
    }
    Err(JlinkError::new(
        ErrorCode::ProbeBusy,
        "该探针由另一个 MCP/Worker 生命周期占用，不支持接管",
        true,
    )
    .with_detail("owner_parent_pid", json!(status.parent_pid))
    .with_detail("requester_pid", json!(current_pid)))
}

/// Ensures a process that lost the endpoint race cannot outlive its attachment attempt.
fn stop_non_authoritative_child(child: &mut Child) -> Result<(), JlinkError> {
    let status = child.try_wait().map_err(|error| {
        JlinkError::new(
            ErrorCode::WorkerUnavailable,
            format!("无法检查非权威 jlink-worker 状态：{error}"),
            true,
        )
    })?;
    if status.is_some() {
        return Ok(());
    }
    child.kill().map_err(|error| {
        JlinkError::new(
            ErrorCode::WorkerUnavailable,
            format!("无法终止非权威 jlink-worker：{error}"),
            true,
        )
    })?;
    child.wait().map_err(|error| {
        JlinkError::new(
            ErrorCode::WorkerUnavailable,
            format!("无法回收非权威 jlink-worker：{error}"),
            true,
        )
    })?;
    Ok(())
}

fn response_result(response: IpcResponse) -> Result<serde_json::Value, JlinkError> {
    match (response.result, response.error) {
        (Some(value), None) => Ok(value),
        (None, Some(error)) => Err(error),
        _ => Err(JlinkError::new(
            ErrorCode::InvalidResponse,
            "Worker 响应必须且只能包含 result 或 error",
            false,
        )),
    }
}

/// Opens a synchronous byte-mode named pipe after waiting for one server instance.
fn open_pipe(endpoint: &str, timeout_ms: u32) -> Result<File, JlinkError> {
    let wide: Vec<u16> = Path::new(endpoint)
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: `wide` is a valid NUL-terminated pipe path for this synchronous call.
    let available = unsafe { WaitNamedPipeW(wide.as_ptr(), timeout_ms) };
    if available == 0 {
        return Err(last_worker_error("Worker 端点当前不可用"));
    }
    // SAFETY: all pointers are valid, the path remains alive, and ownership of a
    // successful handle is immediately transferred to `OwnedHandle`.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(last_worker_error("无法连接 Worker 端点"));
    }
    // SAFETY: `raw` is a unique valid handle returned by CreateFileW.
    let owned = unsafe { OwnedHandle::from_raw_handle(raw) };
    Ok(File::from(owned))
}

fn last_worker_error(context: &str) -> JlinkError {
    // SAFETY: GetLastError has no preconditions and is called at the failure site.
    let code = unsafe { GetLastError() };
    JlinkError::new(
        ErrorCode::WorkerUnavailable,
        format!("{context}（Windows 错误 {code}）"),
        true,
    )
}

fn dispatched_request_error(command: SessionCommand, error: &JlinkError) -> JlinkError {
    classify_worker_loss(command.execution_kind(), DispatchState::Dispatched)
        .expect("dispatched operation has a stable worker-loss classification")
        .with_detail("transport_error", json!(error.to_string()))
}

#[cfg(test)]
mod tests {
    use jlink_domain::{ErrorCode, JlinkError, SessionCommand};

    use super::{WorkerClient, dispatched_request_error};

    #[test]
    fn endpoint_is_stable_without_exposing_probe_identity() {
        let first = WorkerClient::for_probe("260106173").expect("endpoint");
        let second = WorkerClient::for_probe("260106173").expect("endpoint");
        assert_eq!(first, second);
        assert!(first.endpoint().starts_with(r"\\.\pipe\jlink-mcp-v1-"));
        assert!(!first.endpoint().contains("260106173"));
    }

    #[test]
    fn dispatched_side_effect_is_uncertain_but_verify_remains_retryable() {
        let transport = || JlinkError::new(ErrorCode::IpcProtocolError, "pipe closed", false);
        let flash = dispatched_request_error(SessionCommand::Flash, &transport());
        assert_eq!(flash.code, ErrorCode::ExecutionUncertain);
        assert!(!flash.retryable);
        let verify = dispatched_request_error(SessionCommand::Verify, &transport());
        assert_eq!(verify.code, ErrorCode::WorkerUnavailable);
        assert!(verify.retryable);
    }
}
