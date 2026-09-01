use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use jlink_domain::{
    DebugRequest, ErrorCode, HssWriteKind, IpcRequest, IpcResponse, JlinkError, ProtocolVersion,
    SessionCommand, probe_identity_hash, read_ipc_frame, worker_endpoint_name, write_ipc_frame,
};
use serde_json::json;
use windows_sys::Win32::{
    Foundation::{GetLastError, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

use crate::{
    control::execute_control,
    debug::{ensure_firmware_identity, execute_debug},
    gateway::DllGateway,
    hss::HssCoordinator,
    lease::ProbeLease,
    pipe::PipeServer,
    program::execute_program,
    session::TargetSessionManager,
};

/// Immutable startup inputs for one authoritative Worker process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerOptions {
    /// Directory containing stable probe lock files.
    pub lease_root: PathBuf,
    /// Project-local root for capture files, before probe identity partitioning.
    pub capture_root: PathBuf,
    /// Configured probe serial or other unique local identity.
    pub probe_identity: String,
    /// Identity-validated J-Link x64 DLL path.
    pub dll_path: PathBuf,
    /// MCP process whose unexpected exit bounds this Worker's lifetime.
    pub parent_pid: u32,
}

impl WorkerOptions {
    /// Parses the exact internal Worker command-line flags.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ConfigInvalid`] for missing, duplicate, unknown, or
    /// non-Unicode arguments.
    pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, JlinkError> {
        let arguments = arguments
            .into_iter()
            .map(|value| {
                value.into_string().map_err(|_| {
                    JlinkError::new(
                        ErrorCode::ConfigInvalid,
                        "Worker 参数必须是有效 Unicode",
                        false,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (pairs, remainder) = arguments.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                "Worker 参数必须按 --name value 成对提供",
                false,
            ));
        }
        let mut values = BTreeMap::new();
        for pair in pairs {
            if !matches!(
                pair[0].as_str(),
                "--lease-root" | "--capture-root" | "--probe" | "--dll" | "--parent-pid"
            ) {
                return Err(JlinkError::new(
                    ErrorCode::ConfigInvalid,
                    format!("未知 Worker 参数：{}", pair[0]),
                    false,
                ));
            }
            if values.insert(pair[0].clone(), pair[1].clone()).is_some() {
                return Err(JlinkError::new(
                    ErrorCode::ConfigInvalid,
                    format!("Worker 参数重复：{}", pair[0]),
                    false,
                ));
            }
        }
        let required = |name: &str| {
            values.get(name).cloned().ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::ConfigInvalid,
                    format!("缺少 Worker 参数：{name}"),
                    false,
                )
            })
        };
        let parent_pid = required("--parent-pid")?.parse::<u32>().map_err(|_| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                "Worker --parent-pid 必须是非零 u32",
                false,
            )
        })?;
        if parent_pid == 0 {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                "Worker --parent-pid 必须是非零 u32",
                false,
            ));
        }
        Ok(Self {
            lease_root: PathBuf::from(required("--lease-root")?),
            capture_root: PathBuf::from(required("--capture-root")?),
            probe_identity: required("--probe")?,
            dll_path: PathBuf::from(required("--dll")?),
            parent_pid,
        })
    }
}

struct ParentProcess {
    handle: OwnedHandle,
}

impl ParentProcess {
    fn open(parent_pid: u32) -> Result<Self, JlinkError> {
        // SAFETY: the requested access is read-only synchronization and no raw pointer is passed.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
        if raw.is_null() {
            // SAFETY: GetLastError has no preconditions and is read at the failure site.
            let code = unsafe { GetLastError() };
            return Err(JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("无法监视 MCP 父进程 {parent_pid}（Windows 错误 {code}）"),
                true,
            ));
        }
        Ok(Self {
            // SAFETY: `raw` is a unique valid process handle returned by OpenProcess.
            handle: unsafe { OwnedHandle::from_raw_handle(raw) },
        })
    }

    fn has_exited(&self) -> Result<bool, JlinkError> {
        // SAFETY: the owned process handle remains valid for this non-blocking wait.
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => {
                // SAFETY: GetLastError has no preconditions and is read at the failure site.
                let code = unsafe { GetLastError() };
                Err(JlinkError::new(
                    ErrorCode::WorkerUnavailable,
                    format!("无法检查 MCP 父进程状态（Windows 错误 {code}）"),
                    false,
                ))
            }
            value => Err(JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("MCP 父进程等待返回未知状态 {value}"),
                false,
            )),
        }
    }
}

struct WorkerRuntime {
    probe_identity: String,
    parent_pid: u32,
    probe_identity_hash: String,
    _lease: ProbeLease,
    gateway: DllGateway,
    session: TargetSessionManager,
    hss: HssCoordinator,
}

fn validate_request_contract(request: &IpcRequest) -> Result<(), JlinkError> {
    validate_target_and_after(request)?;
    validate_program_payload(request)?;
    validate_debug_payload(request)?;
    validate_control_payload(request)?;
    validate_hss_payload(request)
}

fn validate_target_and_after(request: &IpcRequest) -> Result<(), JlinkError> {
    let unexpected_target_message = match request.command {
        SessionCommand::Status | SessionCommand::HssStatus => Some("只读状态请求不能携带目标配置"),
        SessionCommand::Disconnect | SessionCommand::Shutdown => {
            Some("disconnect/shutdown 请求不能携带目标配置")
        }
        SessionCommand::Connect
        | SessionCommand::Validate
        | SessionCommand::Flash
        | SessionCommand::Erase
        | SessionCommand::Verify
        | SessionCommand::ReadMemory
        | SessionCommand::WriteMemory
        | SessionCommand::ReadVariable
        | SessionCommand::WriteVariable
        | SessionCommand::ReadRegister
        | SessionCommand::WriteRegister
        | SessionCommand::Control
        | SessionCommand::HssStart => None,
    };
    if let (Some(_), Some(message)) = (&request.target, unexpected_target_message) {
        return Err(JlinkError::new(ErrorCode::IpcProtocolError, message, false));
    }
    if request.after.is_some() && request.command != SessionCommand::Validate {
        return Err(JlinkError::new(
            ErrorCode::IpcProtocolError,
            "只有 validate 请求可以携带 after",
            false,
        ));
    }
    Ok(())
}

fn validate_program_payload(request: &IpcRequest) -> Result<(), JlinkError> {
    let program_matches_command = matches!(
        (&request.command, &request.program),
        (
            SessionCommand::Flash,
            Some(jlink_domain::ProgramRequest::Flash { .. })
        ) | (
            SessionCommand::Erase,
            Some(jlink_domain::ProgramRequest::Erase { .. })
        ) | (
            SessionCommand::Verify,
            Some(jlink_domain::ProgramRequest::Verify { .. })
        ) | (
            SessionCommand::Connect
                | SessionCommand::Disconnect
                | SessionCommand::Shutdown
                | SessionCommand::Status
                | SessionCommand::Validate
                | SessionCommand::ReadMemory
                | SessionCommand::WriteMemory
                | SessionCommand::ReadVariable
                | SessionCommand::WriteVariable
                | SessionCommand::ReadRegister
                | SessionCommand::WriteRegister
                | SessionCommand::Control
                | SessionCommand::HssStart
                | SessionCommand::HssStatus,
            None
        )
    );
    if !program_matches_command {
        return Err(JlinkError::new(
            ErrorCode::IpcProtocolError,
            "command 与 program 负载不匹配",
            false,
        ));
    }
    Ok(())
}

fn validate_debug_payload(request: &IpcRequest) -> Result<(), JlinkError> {
    let debug_matches_command = matches!(
        (&request.command, &request.debug),
        (
            SessionCommand::ReadMemory,
            Some(jlink_domain::DebugRequest::ReadMemory { .. })
        ) | (
            SessionCommand::WriteMemory,
            Some(jlink_domain::DebugRequest::WriteMemory { .. })
        ) | (
            SessionCommand::ReadVariable,
            Some(jlink_domain::DebugRequest::ReadVariable { .. })
        ) | (
            SessionCommand::WriteVariable,
            Some(jlink_domain::DebugRequest::WriteVariable { .. })
        ) | (
            SessionCommand::ReadRegister,
            Some(jlink_domain::DebugRequest::ReadRegister { .. })
        ) | (
            SessionCommand::WriteRegister,
            Some(jlink_domain::DebugRequest::WriteRegister { .. })
        ) | (
            SessionCommand::Connect
                | SessionCommand::Disconnect
                | SessionCommand::Shutdown
                | SessionCommand::Status
                | SessionCommand::Validate
                | SessionCommand::Flash
                | SessionCommand::Erase
                | SessionCommand::Verify
                | SessionCommand::Control
                | SessionCommand::HssStart
                | SessionCommand::HssStatus,
            None
        )
    );
    if !debug_matches_command {
        return Err(JlinkError::new(
            ErrorCode::IpcProtocolError,
            "command 与 debug 负载不匹配",
            false,
        ));
    }
    Ok(())
}

fn validate_control_payload(request: &IpcRequest) -> Result<(), JlinkError> {
    if matches!(
        (&request.command, &request.control),
        (SessionCommand::Control, Some(_))
            | (
                SessionCommand::Connect
                    | SessionCommand::Disconnect
                    | SessionCommand::Shutdown
                    | SessionCommand::Status
                    | SessionCommand::Validate
                    | SessionCommand::Flash
                    | SessionCommand::Erase
                    | SessionCommand::Verify
                    | SessionCommand::ReadMemory
                    | SessionCommand::WriteMemory
                    | SessionCommand::ReadVariable
                    | SessionCommand::WriteVariable
                    | SessionCommand::ReadRegister
                    | SessionCommand::WriteRegister
                    | SessionCommand::HssStart
                    | SessionCommand::HssStatus,
                None
            )
    ) {
        Ok(())
    } else {
        Err(JlinkError::new(
            ErrorCode::IpcProtocolError,
            "command 与 control 负载不匹配",
            false,
        ))
    }
}

fn validate_hss_payload(request: &IpcRequest) -> Result<(), JlinkError> {
    let valid = match request.command {
        SessionCommand::HssStart => {
            request.hss_start.is_some()
                && request
                    .capture_max_bytes
                    .is_some_and(|max_bytes| max_bytes > 0)
                && request.capture_id.is_none()
                && request.capture_key.is_none()
        }
        SessionCommand::HssStatus => {
            request.hss_start.is_none()
                && request.capture_max_bytes.is_none()
                && match (&request.capture_id, &request.capture_key) {
                    (Some(capture_id), None) => !capture_id.trim().is_empty(),
                    (None, Some(capture_key)) => !capture_key.trim().is_empty(),
                    _ => false,
                }
        }
        _ => {
            request.hss_start.is_none()
                && request.capture_max_bytes.is_none()
                && request.capture_id.is_none()
                && request.capture_key.is_none()
        }
    };
    if valid {
        Ok(())
    } else {
        Err(JlinkError::new(
            ErrorCode::IpcProtocolError,
            "command 与 HSS 负载不匹配",
            false,
        ))
    }
}

impl WorkerRuntime {
    fn handle_status(&self, request_id: jlink_domain::RequestId) -> (IpcResponse, bool) {
        let status = self.session.status(
            self.parent_pid,
            &self.probe_identity_hash,
            self.gateway.is_loaded(),
        );
        (
            IpcResponse::success(
                ProtocolVersion::V1,
                request_id,
                serde_json::to_value(status).expect("WorkerStatus is serializable"),
            ),
            true,
        )
    }

    fn graceful_shutdown(&mut self) -> Result<(), JlinkError> {
        if self.hss.shutdown(&mut self.gateway)? {
            self.session.record_hss_completed();
        }
        self.session.disconnect(&mut self.gateway)
    }

    fn handle_shutdown(&mut self, request_id: jlink_domain::RequestId) -> (IpcResponse, bool) {
        match self.graceful_shutdown() {
            Ok(()) => (
                IpcResponse::success(ProtocolVersion::V1, request_id, json!({})),
                false,
            ),
            Err(error) => (
                IpcResponse::failure(ProtocolVersion::V1, request_id, error),
                false,
            ),
        }
    }

    fn handle_control(
        &mut self,
        request_id: jlink_domain::RequestId,
        target: Option<jlink_domain::TargetConnectionSpec>,
        control: Option<jlink_domain::ControlRequest>,
    ) -> (IpcResponse, bool) {
        let result = target.ok_or_else(|| {
            JlinkError::new(ErrorCode::ConfigInvalid, "目标控制请求缺少目标配置", false)
        });
        let result = result.and_then(|target| {
            let control = control.ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::IpcProtocolError,
                    "目标控制请求缺少 control 负载",
                    false,
                )
            })?;
            execute_control(&mut self.session, &mut self.gateway, &target, control)
        });
        match result {
            Ok(()) => (
                IpcResponse::success(ProtocolVersion::V1, request_id, json!({})),
                true,
            ),
            Err(error) => (
                IpcResponse::failure(ProtocolVersion::V1, request_id, error),
                true,
            ),
        }
    }

    fn handle_debug(
        &mut self,
        request_id: jlink_domain::RequestId,
        queued_at: Instant,
        target: Option<jlink_domain::TargetConnectionSpec>,
        debug: Option<jlink_domain::DebugRequest>,
    ) -> (IpcResponse, bool) {
        let result = target.ok_or_else(|| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                "变量或内存请求缺少目标配置",
                false,
            )
        });
        let result = result.and_then(|target| {
            let debug = debug.ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::IpcProtocolError,
                    "变量或内存请求缺少 debug 负载",
                    false,
                )
            })?;
            let write_kind = match &debug {
                DebugRequest::WriteMemory { .. } => Some(HssWriteKind::MemoryWrite),
                DebugRequest::WriteVariable { .. } => Some(HssWriteKind::VariableWrite),
                _ => None,
            };
            let token = if let Some(kind) = write_kind {
                self.hss
                    .begin_write(request_id.as_str(), kind, queued_at, Instant::now())?
            } else {
                None
            };
            let result = execute_debug(&mut self.session, &mut self.gateway, &target, debug);
            self.hss.finish_write(
                token,
                Instant::now(),
                result.as_ref().map(|_| ()).map_err(|error| error.code),
            );
            result
        });
        match result {
            Ok(result) => (
                IpcResponse::success(
                    ProtocolVersion::V1,
                    request_id,
                    serde_json::to_value(result).expect("DebugResult is serializable"),
                ),
                true,
            ),
            Err(error) => (
                IpcResponse::failure(ProtocolVersion::V1, request_id, error),
                true,
            ),
        }
    }

    fn handle_hss_start(
        &mut self,
        request_id: jlink_domain::RequestId,
        target: Option<jlink_domain::TargetConnectionSpec>,
        plan: Option<jlink_domain::HssStartPlan>,
        capture_max_bytes: Option<u64>,
    ) -> (IpcResponse, bool) {
        let result = target
            .ok_or_else(|| {
                JlinkError::new(ErrorCode::ConfigInvalid, "HSS start 缺少目标配置", false)
            })
            .and_then(|target| {
                let plan = plan.ok_or_else(|| {
                    JlinkError::new(ErrorCode::IpcProtocolError, "HSS start 缺少启动计划", false)
                })?;
                let capture_max_bytes = capture_max_bytes.ok_or_else(|| {
                    JlinkError::new(
                        ErrorCode::IpcProtocolError,
                        "HSS start 缺少 Capture Store 上限",
                        false,
                    )
                })?;
                let session = &mut self.session;
                let outcome = self.hss.start(
                    &self.probe_identity,
                    &target,
                    plan.clone(),
                    capture_max_bytes,
                    &mut self.gateway,
                    |gateway| {
                        session.ensure_hss_start_allowed(&target)?;
                        ensure_firmware_identity(session, gateway, plan.firmware())?;
                        gateway.hss_capabilities()?.validate_start(&plan)
                    },
                )?;
                if outcome.started_new {
                    session.record_hss_started();
                }
                Ok(outcome.snapshot)
            });
        match result {
            Ok(snapshot) => (
                IpcResponse::success(
                    ProtocolVersion::V1,
                    request_id,
                    serde_json::to_value(snapshot).expect("HssRunSnapshot is serializable"),
                ),
                true,
            ),
            Err(error) => (
                IpcResponse::failure(ProtocolVersion::V1, request_id, error),
                true,
            ),
        }
    }

    fn handle_hss_status(
        &self,
        request_id: jlink_domain::RequestId,
        capture_id: Option<String>,
        capture_key: Option<String>,
    ) -> (IpcResponse, bool) {
        let result = match (capture_id, capture_key) {
            (Some(capture_id), None) => self.hss.status(&capture_id, Instant::now()),
            (None, Some(capture_key)) => self.hss.status_by_key(&capture_key, Instant::now()),
            _ => Err(JlinkError::new(
                ErrorCode::IpcProtocolError,
                "HSS status 必须且只能提供 capture_id 或 capture_key",
                false,
            )),
        };
        match result {
            Ok(snapshot) => (
                IpcResponse::success(
                    ProtocolVersion::V1,
                    request_id,
                    serde_json::to_value(snapshot).expect("HssRunSnapshot is serializable"),
                ),
                true,
            ),
            Err(error) => (
                IpcResponse::failure(ProtocolVersion::V1, request_id, error),
                true,
            ),
        }
    }

    fn handle_program(
        &mut self,
        request_id: jlink_domain::RequestId,
        target: Option<jlink_domain::TargetConnectionSpec>,
        program: Option<jlink_domain::ProgramRequest>,
    ) -> (IpcResponse, bool) {
        let result = target.ok_or_else(|| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                "jlink_program 请求缺少目标配置",
                false,
            )
        });
        let result = result.and_then(|target| {
            let program = program.ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::IpcProtocolError,
                    "jlink_program 请求缺少 program 负载",
                    false,
                )
            })?;
            execute_program(&mut self.session, &mut self.gateway, &target, program)
        });
        match result {
            Ok(()) => (
                IpcResponse::success(ProtocolVersion::V1, request_id, json!({})),
                true,
            ),
            Err(error) => (
                IpcResponse::failure(ProtocolVersion::V1, request_id, error),
                true,
            ),
        }
    }

    fn handle(&mut self, request: IpcRequest, queued_at: Instant) -> (IpcResponse, bool) {
        if let Err(error) = validate_request_contract(&request) {
            return (
                IpcResponse::failure(ProtocolVersion::V1, request.request_id, error),
                true,
            );
        }
        let request_id = request.request_id;
        match request.command {
            SessionCommand::Status => self.handle_status(request_id),
            SessionCommand::Shutdown => self.handle_shutdown(request_id),
            SessionCommand::Disconnect => match self.session.disconnect(&mut self.gateway) {
                Ok(()) => (
                    IpcResponse::success(ProtocolVersion::V1, request_id, json!({})),
                    false,
                ),
                Err(error) => (
                    IpcResponse::failure(ProtocolVersion::V1, request_id, error),
                    true,
                ),
            },
            SessionCommand::Connect => {
                let result = request.target.ok_or_else(|| {
                    JlinkError::new(ErrorCode::ConfigInvalid, "connect 请求缺少目标配置", false)
                });
                match result.and_then(|target| {
                    self.session
                        .connect(&mut self.gateway, &self.probe_identity, &target)?;
                    Ok(self.session.status(
                        self.parent_pid,
                        &self.probe_identity_hash,
                        self.gateway.is_loaded(),
                    ))
                }) {
                    Ok(status) => (
                        IpcResponse::success(
                            ProtocolVersion::V1,
                            request_id,
                            serde_json::to_value(status).expect("WorkerStatus is serializable"),
                        ),
                        true,
                    ),
                    Err(error) => (
                        IpcResponse::failure(ProtocolVersion::V1, request_id, error),
                        true,
                    ),
                }
            }
            SessionCommand::Validate => {
                let result = request.target.ok_or_else(|| {
                    JlinkError::new(ErrorCode::ConfigInvalid, "validate 请求缺少目标配置", false)
                });
                match result.and_then(|target| {
                    self.session.validate(
                        &mut self.gateway,
                        &self.probe_identity,
                        &target,
                        request.after,
                    )
                }) {
                    Ok(report) => (
                        IpcResponse::success(
                            ProtocolVersion::V1,
                            request_id,
                            serde_json::to_value(report).expect("ValidationReport is serializable"),
                        ),
                        true,
                    ),
                    Err(error) => (
                        IpcResponse::failure(ProtocolVersion::V1, request_id, error),
                        true,
                    ),
                }
            }
            SessionCommand::Flash | SessionCommand::Erase | SessionCommand::Verify => {
                self.handle_program(request_id, request.target, request.program)
            }
            SessionCommand::ReadMemory
            | SessionCommand::WriteMemory
            | SessionCommand::ReadVariable
            | SessionCommand::WriteVariable
            | SessionCommand::ReadRegister
            | SessionCommand::WriteRegister => {
                self.handle_debug(request_id, queued_at, request.target, request.debug)
            }
            SessionCommand::Control => {
                self.handle_control(request_id, request.target, request.control)
            }
            SessionCommand::HssStart => self.handle_hss_start(
                request_id,
                request.target,
                request.hss_start,
                request.capture_max_bytes,
            ),
            SessionCommand::HssStatus => {
                self.handle_hss_status(request_id, request.capture_id, request.capture_key)
            }
        }
    }
}

struct RequestEnvelope {
    request: IpcRequest,
    queued_at: Instant,
    response: mpsc::Sender<DispatchReply>,
}

struct DispatchReply {
    response: IpcResponse,
    keep_running: bool,
}

fn listen_for_requests(
    endpoint: &str,
    requests: &mpsc::Sender<RequestEnvelope>,
) -> Result<(), JlinkError> {
    let mut server = PipeServer::new(endpoint)?;
    loop {
        let mut pipe = server.accept()?;
        let request = match read_ipc_frame::<_, IpcRequest>(&mut pipe) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("拒绝无效 IPC 请求：{error}");
                continue;
            }
        };
        let (response_tx, response_rx) = mpsc::channel();
        if requests
            .send(RequestEnvelope {
                request,
                queued_at: Instant::now(),
                response: response_tx,
            })
            .is_err()
        {
            return Ok(());
        }
        let Ok(reply) = response_rx.recv() else {
            return Ok(());
        };
        if let Err(error) = write_ipc_frame(&mut pipe, &reply.response) {
            eprintln!("Worker 响应写回失败，已保留实际执行结果：{error}");
        }
        if !reply.keep_running {
            return Ok(());
        }
    }
}

/// Runs the single-threaded Worker accept/dispatch loop until disconnect.
///
/// # Errors
///
/// Returns a stable error when the lease, DLL gateway, pipe, or response
/// transport cannot be established. Malformed client frames are rejected by
/// closing that connection while the authoritative Worker remains alive.
pub fn run_worker(options: &WorkerOptions) -> Result<(), JlinkError> {
    let parent = ParentProcess::open(options.parent_pid)?;
    let endpoint = worker_endpoint_name(&options.probe_identity)?;
    let lease = ProbeLease::acquire(&options.lease_root, &options.probe_identity)?;
    let identity_hash = probe_identity_hash(&options.probe_identity)?;
    let hss = HssCoordinator::open(
        options.capture_root.join(&identity_hash),
        &options.probe_identity,
    )?;
    let gateway = DllGateway::load(&options.dll_path)?;
    let mut runtime = WorkerRuntime {
        probe_identity: options.probe_identity.clone(),
        parent_pid: options.parent_pid,
        probe_identity_hash: identity_hash,
        _lease: lease,
        gateway,
        session: TargetSessionManager::new(),
        hss,
    };
    let (request_tx, request_rx) = mpsc::channel();
    let listener = thread::spawn(move || listen_for_requests(&endpoint, &request_tx));
    let mut keep_running = true;
    let mut parent_exit = false;
    while keep_running {
        parent_exit = parent.has_exited()?;
        if parent_exit {
            break;
        }
        if runtime.hss.is_active() && runtime.hss.advance(&mut runtime.gateway)? {
            runtime.session.record_hss_completed();
        }
        let wait = if runtime.hss.is_active() {
            HssCoordinator::next_wait()
        } else {
            Duration::from_millis(100)
        };
        let envelope = match request_rx.recv_timeout(wait) {
            Ok(envelope) => Some(envelope),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let Some(envelope) = envelope else {
            continue;
        };
        let (response, next) = runtime.handle(envelope.request, envelope.queued_at);
        keep_running = next;
        if envelope
            .response
            .send(DispatchReply {
                response,
                keep_running,
            })
            .is_err()
        {
            return Err(JlinkError::new(
                ErrorCode::WorkerUnavailable,
                "Worker 管道监听线程在响应前退出",
                true,
            ));
        }
    }
    if parent_exit {
        return Ok(());
    }
    match listener.join() {
        Ok(result) => result,
        Err(_) => Err(JlinkError::new(
            ErrorCode::WorkerUnavailable,
            "Worker 管道监听线程异常退出",
            false,
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use jlink_domain::{
        ControlRequest, CoreRegister, DebugRequest, MemoryRange, ProgramAfter, ProgramRequest,
        RequestId, TargetConnectionSpec, TargetInterface,
    };

    use super::*;

    #[test]
    fn worker_options_keep_probe_leases_and_project_captures_separate() {
        let options = WorkerOptions::parse([
            OsString::from("--lease-root"),
            OsString::from("user-leases"),
            OsString::from("--capture-root"),
            OsString::from("project-captures"),
            OsString::from("--probe"),
            OsString::from("260106173"),
            OsString::from("--dll"),
            OsString::from("JLink_x64.dll"),
            OsString::from("--parent-pid"),
            OsString::from("42"),
        ])
        .expect("complete Worker startup contract");
        assert_eq!(options.lease_root, PathBuf::from("user-leases"));
        assert_eq!(options.capture_root, PathBuf::from("project-captures"));
    }

    #[test]
    fn program_command_and_payload_must_match_exactly() {
        let target = TargetConnectionSpec::new(
            "S32K144",
            TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target spec");
        let flash = ProgramRequest::Flash {
            image: PathBuf::from("firmware.bin"),
            base_address: Some(0),
            verify: true,
            after: ProgramAfter::ResetRun,
            loader_ram: None,
        };
        let valid = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("valid").expect("request id"),
            SessionCommand::Flash,
        )
        .with_target(target.clone())
        .with_program(flash.clone());
        validate_request_contract(&valid).expect("matching program request");

        let mismatch = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("mismatch").expect("request id"),
            SessionCommand::Verify,
        )
        .with_target(target)
        .with_program(flash);
        assert_eq!(
            validate_request_contract(&mismatch)
                .expect_err("verify cannot carry flash")
                .code,
            ErrorCode::IpcProtocolError
        );
    }

    #[test]
    fn debug_command_and_payload_must_match_exactly() {
        let target = TargetConnectionSpec::new(
            "S32K144",
            TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target spec");
        let read = DebugRequest::ReadMemory {
            range: MemoryRange::raw(0x2000_0000, 4).expect("raw range"),
        };
        let valid = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("debug-valid").expect("request id"),
            SessionCommand::ReadMemory,
        )
        .with_target(target.clone())
        .with_debug(read.clone());
        validate_request_contract(&valid).expect("matching debug request");

        let mismatch = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("debug-mismatch").expect("request id"),
            SessionCommand::WriteMemory,
        )
        .with_target(target)
        .with_debug(read);
        assert_eq!(
            validate_request_contract(&mismatch)
                .expect_err("write command cannot carry read payload")
                .code,
            ErrorCode::IpcProtocolError
        );
    }

    #[test]
    fn register_and_control_commands_require_exact_payloads() {
        let target = TargetConnectionSpec::new(
            "S32K144",
            TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target spec");
        let register = DebugRequest::ReadRegister {
            register: CoreRegister::Pc,
        };
        let read = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("register-read").expect("request id"),
            SessionCommand::ReadRegister,
        )
        .with_target(target.clone())
        .with_debug(register.clone());
        validate_request_contract(&read).expect("matching register request");

        let mismatch = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("register-mismatch").expect("request id"),
            SessionCommand::WriteRegister,
        )
        .with_target(target.clone())
        .with_debug(register);
        assert_eq!(
            validate_request_contract(&mismatch)
                .expect_err("write command cannot carry read-register payload")
                .code,
            ErrorCode::IpcProtocolError
        );

        let control = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("control").expect("request id"),
            SessionCommand::Control,
        )
        .with_target(target.clone())
        .with_control(ControlRequest::Halt);
        validate_request_contract(&control).expect("matching control request");

        let missing = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("control-missing").expect("request id"),
            SessionCommand::Control,
        )
        .with_target(target);
        assert_eq!(
            validate_request_contract(&missing)
                .expect_err("control payload is required")
                .code,
            ErrorCode::IpcProtocolError
        );
    }

    #[test]
    fn hss_status_requires_only_one_non_empty_capture_identity() {
        let valid = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("hss-status").expect("request id"),
            SessionCommand::HssStatus,
        )
        .with_capture_id("cap-1");
        validate_request_contract(&valid).expect("internal status identity");

        let valid_key = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("hss-status-key").expect("request id"),
            SessionCommand::HssStatus,
        )
        .with_capture_key("recover-key");
        validate_request_contract(&valid_key).expect("internal status recovery key");

        let both = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("hss-status-both").expect("request id"),
            SessionCommand::HssStatus,
        )
        .with_capture_id("cap-1")
        .with_capture_key("recover-key");
        assert_eq!(
            validate_request_contract(&both)
                .expect_err("status identity must be exclusive")
                .code,
            ErrorCode::IpcProtocolError
        );

        let missing = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("hss-missing").expect("request id"),
            SessionCommand::HssStatus,
        );
        assert_eq!(
            validate_request_contract(&missing)
                .expect_err("status requires capture id")
                .code,
            ErrorCode::IpcProtocolError
        );

        let leaked = IpcRequest::new(
            ProtocolVersion::V1,
            RequestId::new("ordinary-status").expect("request id"),
            SessionCommand::Status,
        )
        .with_capture_id("cap-1");
        assert_eq!(
            validate_request_contract(&leaked)
                .expect_err("ordinary status cannot carry HSS identity")
                .code,
            ErrorCode::IpcProtocolError
        );
    }

    #[test]
    fn t_p3_recover_parent_exit_is_observable_and_terminates_worker_lifecycle() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 10 127.0.0.1 >nul"])
            .spawn()
            .expect("bounded parent fixture starts");
        let parent = ParentProcess::open(child.id()).expect("fixture process is observable");
        assert!(!parent.has_exited().expect("live parent status"));
        child.kill().expect("fixture process terminates");
        child.wait().expect("fixture process is reaped");
        assert!(parent.has_exited().expect("exited parent status"));
    }
}
