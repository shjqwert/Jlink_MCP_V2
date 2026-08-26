use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use jlink_domain::{
    ErrorCode, IpcRequest, IpcResponse, JlinkError, ProtocolVersion, SessionCommand, WorkerStatus,
    probe_identity_hash, read_ipc_frame, worker_endpoint_name, write_ipc_frame,
};
use serde_json::json;

use crate::{gateway::DllGateway, lease::ProbeLease, pipe::PipeServer};

/// Immutable startup inputs for one authoritative Worker process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerOptions {
    /// Directory containing stable probe lock files.
    pub lease_root: PathBuf,
    /// Configured probe serial or other unique local identity.
    pub probe_identity: String,
    /// Identity-validated J-Link x64 DLL path.
    pub dll_path: PathBuf,
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
            if !matches!(pair[0].as_str(), "--lease-root" | "--probe" | "--dll") {
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
        Ok(Self {
            lease_root: PathBuf::from(required("--lease-root")?),
            probe_identity: required("--probe")?,
            dll_path: PathBuf::from(required("--dll")?),
        })
    }
}

struct WorkerRuntime {
    probe_identity_hash: String,
    _lease: ProbeLease,
    gateway: DllGateway,
}

impl WorkerRuntime {
    fn handle(&self, request: IpcRequest) -> (IpcResponse, bool) {
        let request_id = request.request_id;
        match request.command {
            SessionCommand::Status => {
                let status = WorkerStatus {
                    worker_pid: std::process::id(),
                    probe_identity_hash: self.probe_identity_hash.clone(),
                    dll_loaded: self.gateway.is_loaded(),
                };
                (
                    IpcResponse::success(
                        ProtocolVersion::V1,
                        request_id,
                        serde_json::to_value(status).expect("WorkerStatus is serializable"),
                    ),
                    true,
                )
            }
            SessionCommand::Disconnect => (
                IpcResponse::success(ProtocolVersion::V1, request_id, json!({})),
                false,
            ),
            SessionCommand::Connect | SessionCommand::Validate => (
                IpcResponse::failure(
                    ProtocolVersion::V1,
                    request_id,
                    JlinkError::new(
                        ErrorCode::InvalidStateTransition,
                        "P1 会话状态机尚未接管该命令",
                        false,
                    ),
                ),
                true,
            ),
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
    let endpoint = worker_endpoint_name(&options.probe_identity)?;
    let lease = ProbeLease::acquire(&options.lease_root, &options.probe_identity)?;
    let gateway = DllGateway::load(&options.dll_path)?;
    let runtime = WorkerRuntime {
        probe_identity_hash: probe_identity_hash(&options.probe_identity)?,
        _lease: lease,
        gateway,
    };
    let mut server = PipeServer::new(&endpoint)?;
    let mut keep_running = true;
    while keep_running {
        let mut pipe = server.accept()?;
        let request = match read_ipc_frame::<_, IpcRequest>(&mut pipe) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("拒绝无效 IPC 请求：{error}");
                continue;
            }
        };
        let (response, next) = runtime.handle(request);
        write_ipc_frame(&mut pipe, &response)?;
        keep_running = next;
    }
    Ok(())
}
