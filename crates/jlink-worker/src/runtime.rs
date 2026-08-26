use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use jlink_domain::{
    ErrorCode, IpcRequest, IpcResponse, JlinkError, ProtocolVersion, SessionCommand,
    probe_identity_hash, read_ipc_frame, worker_endpoint_name, write_ipc_frame,
};
use serde_json::json;

use crate::{
    gateway::DllGateway, lease::ProbeLease, pipe::PipeServer, program::execute_program,
    session::TargetSessionManager,
};

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
    probe_identity: String,
    probe_identity_hash: String,
    _lease: ProbeLease,
    gateway: DllGateway,
    session: TargetSessionManager,
}

fn validate_request_contract(request: &IpcRequest) -> Result<(), JlinkError> {
    let unexpected_target_message = match request.command {
        SessionCommand::Status => Some("status 请求不能携带目标配置"),
        SessionCommand::Disconnect => Some("disconnect 请求不能携带目标配置"),
        SessionCommand::Connect
        | SessionCommand::Validate
        | SessionCommand::Flash
        | SessionCommand::Erase
        | SessionCommand::Verify => None,
    };
    if request.target.is_some()
        && let Some(message) = unexpected_target_message
    {
        return Err(JlinkError::new(ErrorCode::IpcProtocolError, message, false));
    }
    if request.after.is_some() && request.command != SessionCommand::Validate {
        return Err(JlinkError::new(
            ErrorCode::IpcProtocolError,
            "只有 validate 请求可以携带 after",
            false,
        ));
    }
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
                | SessionCommand::Status
                | SessionCommand::Validate,
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

impl WorkerRuntime {
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

    fn handle(&mut self, request: IpcRequest) -> (IpcResponse, bool) {
        if let Err(error) = validate_request_contract(&request) {
            return (
                IpcResponse::failure(ProtocolVersion::V1, request.request_id, error),
                true,
            );
        }
        let request_id = request.request_id;
        match request.command {
            SessionCommand::Status => {
                let status = self
                    .session
                    .status(&self.probe_identity_hash, self.gateway.is_loaded());
                (
                    IpcResponse::success(
                        ProtocolVersion::V1,
                        request_id,
                        serde_json::to_value(status).expect("WorkerStatus is serializable"),
                    ),
                    true,
                )
            }
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
                    Ok(self
                        .session
                        .status(&self.probe_identity_hash, self.gateway.is_loaded()))
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
    let mut runtime = WorkerRuntime {
        probe_identity: options.probe_identity.clone(),
        probe_identity_hash: probe_identity_hash(&options.probe_identity)?,
        _lease: lease,
        gateway,
        session: TargetSessionManager::new(),
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use jlink_domain::{
        ProgramAfter, ProgramRequest, RequestId, TargetConnectionSpec, TargetInterface,
    };

    use super::*;

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
}
