use jlink_domain::{
    ConnectionState, DebugRequest, ErrorCode, FaultDiagnostics, JlinkError, RecoveryAction,
    RecoveryNotification, SessionEvent, TargetConnectionSpec, TargetState, ValidationAfter,
    ValidationCheckKind, ValidationInvalidation, ValidationReport, WorkerStatus,
    ensure_disconnect_allowed, transition_session,
};
use serde_json::json;

use crate::gateway::DllGateway;

trait RecoveryIo {
    fn resume_and_observe(&mut self) -> Result<TargetState, JlinkError>;
    fn reset_run_and_observe(&mut self) -> Result<TargetState, JlinkError>;
    fn fault_diagnostics(&self) -> FaultDiagnostics;
}

impl RecoveryIo for DllGateway {
    fn resume_and_observe(&mut self) -> Result<TargetState, JlinkError> {
        Self::resume_and_observe(self)
    }

    fn reset_run_and_observe(&mut self) -> Result<TargetState, JlinkError> {
        Self::reset_run_and_observe(self)
    }

    fn fault_diagnostics(&self) -> FaultDiagnostics {
        Self::fault_diagnostics(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ValidationMode {
    Observe,
    Temporary(ValidationAfter),
}

fn validation_mode(
    connection_state: ConnectionState,
    after: Option<ValidationAfter>,
) -> Result<ValidationMode, JlinkError> {
    match (connection_state, after) {
        (ConnectionState::Connected, None) => Ok(ValidationMode::Observe),
        (ConnectionState::Connected, Some(_)) => Err(JlinkError::new(
            ErrorCode::OperationConflict,
            "活动连接中的 validate 不能携带 after",
            false,
        )),
        (ConnectionState::Disconnected, Some(after)) => Ok(ValidationMode::Temporary(after)),
        (ConnectionState::Disconnected, None) => Err(JlinkError::new(
            ErrorCode::ConfigInvalid,
            "断开状态的 validate 必须提供 after: run 或 halt",
            false,
        )),
        _ => Err(JlinkError::new(
            ErrorCode::OperationConflict,
            "当前连接转换期间不能执行 validate",
            true,
        )),
    }
}

pub(crate) struct TargetSessionManager {
    connection_state: ConnectionState,
    target_state: TargetState,
    target_id: Option<u32>,
    hss_active: bool,
    active_target: Option<TargetConnectionSpec>,
    validation_key: Option<TargetConnectionSpec>,
    firmware_identity: Option<String>,
    validation_runs: u64,
    recovery_notifications: Vec<RecoveryNotification>,
    last_invalidation: Option<ValidationInvalidation>,
}

impl TargetSessionManager {
    pub(crate) const fn new() -> Self {
        Self {
            connection_state: ConnectionState::Disconnected,
            target_state: TargetState::Unknown,
            target_id: None,
            hss_active: false,
            active_target: None,
            validation_key: None,
            firmware_identity: None,
            validation_runs: 0,
            recovery_notifications: Vec::new(),
            last_invalidation: None,
        }
    }

    pub(crate) fn status(&self, probe_identity_hash: &str, dll_loaded: bool) -> WorkerStatus {
        WorkerStatus {
            worker_pid: std::process::id(),
            probe_identity_hash: probe_identity_hash.to_owned(),
            dll_loaded,
            connection_state: self.connection_state,
            target_state: self.target_state,
            target_id: self.target_id,
            hss_active: self.hss_active,
            validation_cached: self.validation_key.is_some(),
            validation_runs: self.validation_runs,
            recovery_notifications: self.recovery_notifications.clone(),
        }
    }

    /// Rejects programming before any file, DLL, or target operation is attempted.
    pub(crate) fn ensure_program_allowed(
        &self,
        spec: &TargetConnectionSpec,
    ) -> Result<(), JlinkError> {
        if self.hss_active {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "活动 HSS 期间不能执行 flash、erase 或 verify",
                true,
            ));
        }
        if self.connection_state != ConnectionState::Connected {
            return Err(JlinkError::new(
                ErrorCode::InvalidStateTransition,
                "jlink_program 要求已连接且已验证的目标会话",
                true,
            ));
        }
        if self.active_target.as_ref() != Some(spec) {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "活动连接与当前目标配置不一致，请先断开并重新连接",
                true,
            ));
        }
        Ok(())
    }

    /// Rejects ordinary debug requests that conflict with session or HSS state.
    pub(crate) fn ensure_debug_allowed(
        &self,
        spec: &TargetConnectionSpec,
        request: &DebugRequest,
    ) -> Result<(), JlinkError> {
        request.validate()?;
        if self.hss_active && !request.may_interleave_during_hss() {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "活动 HSS 期间不能执行普通读取或核心寄存器访问；请查询采集数据",
                true,
            ));
        }
        if self.connection_state != ConnectionState::Connected {
            return Err(JlinkError::new(
                ErrorCode::InvalidStateTransition,
                "变量和内存操作要求已连接且已验证的目标会话",
                true,
            ));
        }
        if self.active_target.as_ref() != Some(spec) {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "活动连接与当前目标配置不一致，请先断开并重新连接",
                true,
            ));
        }
        Ok(())
    }

    /// Rejects public target control that conflicts with session or HSS state.
    pub(crate) fn ensure_control_allowed(
        &self,
        spec: &TargetConnectionSpec,
    ) -> Result<(), JlinkError> {
        if self.hss_active {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "活动 HSS 期间不能执行目标运行控制",
                true,
            ));
        }
        if self.connection_state != ConnectionState::Connected {
            return Err(JlinkError::new(
                ErrorCode::InvalidStateTransition,
                "jlink_control 要求已连接且已验证的目标会话",
                true,
            ));
        }
        if self.active_target.as_ref() != Some(spec) {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "活动连接与当前目标配置不一致，请先断开并重新连接",
                true,
            ));
        }
        Ok(())
    }

    /// Records the target state confirmed after one public control action.
    pub(crate) const fn record_control_state(&mut self, state: TargetState) {
        self.target_state = state;
    }

    /// Returns whether this connection already proved one symbol ELF identity.
    pub(crate) fn firmware_identity_cached(&self, elf_sha256: &str) -> bool {
        self.firmware_identity.as_deref() == Some(elf_sha256)
    }

    /// Rejects a first-time symbol identity Flash read while HSS owns the gateway.
    pub(crate) fn ensure_firmware_identity_read_allowed(&self) -> Result<(), JlinkError> {
        if self.hss_active {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "活动 HSS 期间不能首次验证符号 ELF；请在采集开始前完成变量访问",
                true,
            ));
        }
        Ok(())
    }

    /// Caches a successfully verified symbol ELF for this connection only.
    pub(crate) fn record_firmware_identity(&mut self, elf_sha256: &str) {
        self.firmware_identity = Some(elf_sha256.to_owned());
    }

    /// Records the actual final target state and invalidates Flash-derived caches.
    pub(crate) fn record_program_result(&mut self, state: TargetState, flash_modified: bool) {
        self.target_state = state;
        if flash_modified {
            self.invalidate_validation(ValidationInvalidation::FlashModified);
        }
    }

    /// Marks the active connection untrusted after an indeterminate target side effect.
    pub(crate) fn record_execution_uncertain(
        &mut self,
        reason: ValidationInvalidation,
    ) -> Result<(), JlinkError> {
        self.connection_state =
            transition_session(self.connection_state, SessionEvent::WorkerLost)?;
        self.target_state = TargetState::Unknown;
        self.target_id = None;
        self.active_target = None;
        self.recovery_notifications.clear();
        self.invalidate_validation(reason);
        Ok(())
    }

    pub(crate) fn connect(
        &mut self,
        gateway: &mut DllGateway,
        probe_identity: &str,
        spec: &TargetConnectionSpec,
    ) -> Result<(), JlinkError> {
        spec.validate()?;
        if probe_identity != spec.probe_serial().to_string() {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                "IPC 目标探针与 Worker 租约身份不一致",
                false,
            ));
        }
        self.connection_state =
            transition_session(self.connection_state, SessionEvent::ConnectRequested)?;
        let observation = match gateway.open_target(spec) {
            Ok(observation) => observation,
            Err(error) => {
                self.connection_state =
                    transition_session(self.connection_state, SessionEvent::ConnectFailed)?;
                return Err(error);
            }
        };
        match recover_target(gateway, observation.target_state) {
            Ok((target_state, notifications)) => {
                self.target_state = target_state;
                self.recovery_notifications = notifications;
            }
            Err(error) => {
                gateway.close_target();
                self.connection_state =
                    transition_session(self.connection_state, SessionEvent::ConnectFailed)?;
                return Err(error);
            }
        }
        self.validation_runs += 1;
        let report = gateway.validation_report(self.validation_runs);
        if !ordinary_debug_validation_passed(&report) {
            gateway.close_target();
            self.target_state = TargetState::Unknown;
            self.connection_state =
                transition_session(self.connection_state, SessionEvent::ConnectFailed)?;
            return Err(JlinkError::new(
                ErrorCode::TargetConnectFailed,
                "目标连接后的环境验证未通过，请按 checks.recommendation 修正",
                true,
            )
            .with_detail("report", json!(report)));
        }
        self.target_state = report.target_state;
        self.target_id = Some(observation.target_id);
        self.active_target = Some(spec.clone());
        self.validation_key = Some(spec.clone());
        self.last_invalidation = None;
        self.connection_state = transition_session(self.connection_state, SessionEvent::Connected)?;
        Ok(())
    }

    pub(crate) fn validate(
        &mut self,
        gateway: &mut DllGateway,
        probe_identity: &str,
        spec: &TargetConnectionSpec,
        after: Option<ValidationAfter>,
    ) -> Result<ValidationReport, JlinkError> {
        spec.validate()?;
        if probe_identity != spec.probe_serial().to_string() {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                "显式验证的探针与 Worker 租约身份不一致",
                false,
            ));
        }
        match validation_mode(self.connection_state, after)? {
            ValidationMode::Observe => {
                if self.active_target.as_ref() != Some(spec) {
                    return Err(JlinkError::new(
                        ErrorCode::OperationConflict,
                        "活动连接的配置与显式验证请求不同，请先断开",
                        true,
                    ));
                }
                self.validation_runs += 1;
                let report = gateway.validation_report(self.validation_runs);
                self.target_state = report.target_state;
                self.target_id = report.target_id;
                Ok(report)
            }
            ValidationMode::Temporary(after) => self.validate_temporary(gateway, spec, after),
        }
    }

    fn validate_temporary(
        &mut self,
        gateway: &mut DllGateway,
        spec: &TargetConnectionSpec,
        after: ValidationAfter,
    ) -> Result<ValidationReport, JlinkError> {
        let observation = gateway.open_target(spec).map_err(|error| {
            error.with_detail(
                "recommendation",
                json!("检查 DLL、probe.serial、目标供电、器件型号、接口和速度"),
            )
        })?;
        let next_validation_run = self.validation_runs + 1;
        let result = (|| {
            let (running_state, notifications) = recover_target(gateway, observation.target_state)?;
            if running_state != TargetState::Running {
                return Err(JlinkError::new(
                    ErrorCode::TargetRecoveryFailed,
                    format!("validate 恢复后的目标状态不是 running：{running_state:?}"),
                    false,
                ));
            }
            let mut report = gateway.validation_report(next_validation_run);
            let final_state = match after {
                ValidationAfter::Run => running_state,
                ValidationAfter::Halt => gateway.halt_and_observe()?,
            };
            let expected = match after {
                ValidationAfter::Run => TargetState::Running,
                ValidationAfter::Halt => TargetState::Halted,
            };
            if final_state != expected {
                return Err(JlinkError::new(
                    ErrorCode::TargetRecoveryFailed,
                    format!("validate 未收口到请求状态：after={after:?}，实际={final_state:?}"),
                    false,
                ));
            }
            report.target_state = final_state;
            report.recovery_notifications = notifications;
            Ok(report)
        })();
        gateway.close_target();
        match result {
            Ok(report) => {
                self.validation_runs = next_validation_run;
                self.target_state = report.target_state;
                self.target_id = Some(observation.target_id);
                self.recovery_notifications
                    .clone_from(&report.recovery_notifications);
                Ok(report)
            }
            Err(error) => {
                self.target_state = TargetState::Unknown;
                self.target_id = None;
                self.recovery_notifications.clear();
                Err(error)
            }
        }
    }

    pub(crate) fn disconnect(&mut self, gateway: &mut DllGateway) -> Result<(), JlinkError> {
        ensure_disconnect_allowed(self.hss_active)?;
        if matches!(
            self.connection_state,
            ConnectionState::Connected | ConnectionState::Faulted
        ) {
            self.connection_state =
                transition_session(self.connection_state, SessionEvent::DisconnectRequested)?;
        }
        gateway.close_target();
        self.target_state = TargetState::Unknown;
        self.target_id = None;
        self.active_target = None;
        self.recovery_notifications.clear();
        self.invalidate_validation(ValidationInvalidation::ConnectionLost);
        Ok(())
    }

    fn invalidate_validation(&mut self, reason: ValidationInvalidation) {
        self.validation_key = None;
        self.firmware_identity = None;
        self.last_invalidation = Some(reason);
    }
}

fn ordinary_debug_validation_passed(report: &ValidationReport) -> bool {
    report
        .checks
        .iter()
        .all(|item| item.kind == ValidationCheckKind::HssCapability || item.passed)
}

fn recover_target<T: RecoveryIo>(
    io: &mut T,
    initial: TargetState,
) -> Result<(TargetState, Vec<RecoveryNotification>), JlinkError> {
    match initial {
        TargetState::Running => Ok((TargetState::Running, Vec::new())),
        TargetState::Halted => {
            let mut actions = vec![RecoveryAction::Resume];
            match io.resume_and_observe() {
                Ok(TargetState::Running) => Ok((
                    TargetState::Running,
                    vec![RecoveryNotification::ResumedFromHalt],
                )),
                Ok(_) => reset_target(io, &mut actions, None),
                Err(error) => reset_target(io, &mut actions, Some(error.to_string())),
            }
        }
        TargetState::HardFault => reset_target(io, &mut Vec::new(), None),
        TargetState::Unknown => Err(JlinkError::new(
            ErrorCode::TargetConnectFailed,
            "连接后无法确定目标运行状态，未执行自动 reset",
            true,
        )),
    }
}

fn reset_target<T: RecoveryIo>(
    io: &mut T,
    actions: &mut Vec<RecoveryAction>,
    resume_error: Option<String>,
) -> Result<(TargetState, Vec<RecoveryNotification>), JlinkError> {
    actions.extend([RecoveryAction::Reset, RecoveryAction::RunAfterReset]);
    let final_result = io.reset_run_and_observe();
    if matches!(final_result, Ok(TargetState::Running)) {
        let mut notifications = Vec::new();
        if actions.contains(&RecoveryAction::Resume) {
            notifications.push(RecoveryNotification::ResumedFromHalt);
        }
        notifications.push(RecoveryNotification::ResetAfterFault);
        return Ok((TargetState::Running, notifications));
    }
    let diagnostics = io.fault_diagnostics();
    let mut error = JlinkError::new(
        ErrorCode::TargetRecoveryFailed,
        "resume/reset 后目标仍未稳定运行，已停止后续目标操作",
        false,
    )
    .with_detail("actions", json!(actions))
    .with_detail("diagnostics", json!(diagnostics));
    if let Some(resume_error) = resume_error {
        error = error.with_detail("resume_error", json!(resume_error));
    }
    match final_result {
        Ok(state) => error = error.with_detail("final_state", json!(state)),
        Err(reset_error) => {
            error = error.with_detail("reset_error", json!(reset_error.to_string()));
        }
    }
    Err(error)
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, env, path::PathBuf};

    use super::*;

    #[test]
    fn t_p3_abi_optional_hss_failure_does_not_block_ordinary_debug_validation() {
        let mut report = ValidationReport {
            valid: false,
            checks: vec![jlink_domain::ValidationCheck {
                kind: ValidationCheckKind::HssCapability,
                passed: false,
                detail: "missing JLINK_HSS_Start".to_owned(),
                recommendation: Some("use a HSS-capable DLL".to_owned()),
            }],
            target_state: TargetState::Running,
            target_id: Some(0x2BA0_1477),
            validation_runs: 1,
            recovery_notifications: Vec::new(),
        };
        assert!(ordinary_debug_validation_passed(&report));

        report.checks.push(jlink_domain::ValidationCheck {
            kind: ValidationCheckKind::BackgroundAccess,
            passed: false,
            detail: "background read failed".to_owned(),
            recommendation: None,
        });
        assert!(!ordinary_debug_validation_passed(&report));
    }

    struct ScriptedRecovery {
        resume: VecDeque<Result<TargetState, JlinkError>>,
        reset: VecDeque<Result<TargetState, JlinkError>>,
    }

    impl RecoveryIo for ScriptedRecovery {
        fn resume_and_observe(&mut self) -> Result<TargetState, JlinkError> {
            self.resume.pop_front().expect("scripted resume")
        }

        fn reset_run_and_observe(&mut self) -> Result<TargetState, JlinkError> {
            self.reset.pop_front().expect("scripted reset")
        }

        fn fault_diagnostics(&self) -> FaultDiagnostics {
            FaultDiagnostics {
                pc: Some(0x1234),
                ipsr: Some(3),
                cfsr: Some(1),
                hfsr: Some(2),
                dfsr: Some(4),
                unavailable: Vec::new(),
            }
        }
    }

    #[test]
    fn halted_target_resumes_without_reset() {
        let mut io = ScriptedRecovery {
            resume: VecDeque::from([Ok(TargetState::Running)]),
            reset: VecDeque::new(),
        };
        let (state, notifications) =
            recover_target(&mut io, TargetState::Halted).expect("resume succeeds");
        assert_eq!(state, TargetState::Running);
        assert_eq!(notifications, [RecoveryNotification::ResumedFromHalt]);
    }

    #[test]
    fn hardfault_uses_reset_and_reports_recovery() {
        let mut io = ScriptedRecovery {
            resume: VecDeque::new(),
            reset: VecDeque::from([Ok(TargetState::Running)]),
        };
        let (state, notifications) =
            recover_target(&mut io, TargetState::HardFault).expect("reset succeeds");
        assert_eq!(state, TargetState::Running);
        assert_eq!(notifications, [RecoveryNotification::ResetAfterFault]);
    }

    #[test]
    fn t_p3_start_reuses_session_resume_then_reset_and_preserves_both_notices() {
        let mut io = ScriptedRecovery {
            resume: VecDeque::from([Ok(TargetState::HardFault)]),
            reset: VecDeque::from([Ok(TargetState::Running)]),
        };
        let (state, notifications) =
            recover_target(&mut io, TargetState::Halted).expect("shared recovery succeeds");
        assert_eq!(state, TargetState::Running);
        assert_eq!(
            notifications,
            [
                RecoveryNotification::ResumedFromHalt,
                RecoveryNotification::ResetAfterFault
            ]
        );
    }

    #[test]
    fn failed_resume_and_reset_return_diagnostics() {
        let mut io = ScriptedRecovery {
            resume: VecDeque::from([Ok(TargetState::HardFault)]),
            reset: VecDeque::from([Ok(TargetState::Halted)]),
        };
        let error = recover_target(&mut io, TargetState::Halted).expect_err("recovery fails");
        assert_eq!(error.code, ErrorCode::TargetRecoveryFailed);
        let details = error.details.expect("recovery details");
        assert_eq!(details["diagnostics"]["pc"], json!(0x1234));
        assert_eq!(
            details["actions"],
            json!(["resume", "reset", "run_after_reset"])
        );
    }

    #[test]
    fn validation_invalidation_and_hss_status_are_owned_by_session() {
        let spec = TargetConnectionSpec::new(
            "S32K144",
            jlink_domain::TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target spec");
        let mut manager = TargetSessionManager::new();
        manager.validation_key = Some(spec);
        manager.hss_active = true;
        let before = manager.status("probe-hash", true);
        assert!(before.hss_active);
        assert!(before.validation_cached);
        assert_eq!(before.validation_runs, 0);
        for reason in [
            ValidationInvalidation::ConnectionLost,
            ValidationInvalidation::WorkerExited,
            ValidationInvalidation::FlashModified,
            ValidationInvalidation::DllChanged,
            ValidationInvalidation::ElfChanged,
            ValidationInvalidation::TargetConfigurationChanged,
        ] {
            manager.validation_key = Some(
                TargetConnectionSpec::new(
                    "S32K144",
                    jlink_domain::TargetInterface::Swd,
                    4_000,
                    Some(260_106_173),
                    None,
                )
                .expect("target spec"),
            );
            manager.invalidate_validation(reason);
            assert!(manager.validation_key.is_none());
            assert_eq!(manager.last_invalidation, Some(reason));
        }
    }

    #[test]
    fn validation_after_contract_depends_on_connection_state() {
        assert_eq!(
            validation_mode(ConnectionState::Connected, None).expect("connected observation"),
            ValidationMode::Observe
        );
        assert_eq!(
            validation_mode(ConnectionState::Disconnected, Some(ValidationAfter::Run))
                .expect("detached run"),
            ValidationMode::Temporary(ValidationAfter::Run)
        );
        assert_eq!(
            validation_mode(ConnectionState::Disconnected, Some(ValidationAfter::Halt))
                .expect("detached halt"),
            ValidationMode::Temporary(ValidationAfter::Halt)
        );
        assert_eq!(
            validation_mode(ConnectionState::Disconnected, None)
                .expect_err("detached validation requires after")
                .code,
            ErrorCode::ConfigInvalid
        );
        assert_eq!(
            validation_mode(ConnectionState::Connected, Some(ValidationAfter::Run))
                .expect_err("connected validation forbids after")
                .code,
            ErrorCode::OperationConflict
        );
    }

    #[test]
    fn active_hss_rejects_programming_before_other_session_checks() {
        let spec = TargetConnectionSpec::new(
            "S32K144",
            jlink_domain::TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target spec");
        let mut manager = TargetSessionManager::new();
        manager.hss_active = true;
        let error = manager
            .ensure_program_allowed(&spec)
            .expect_err("HSS conflict is immediate even while disconnected");
        assert_eq!(error.code, ErrorCode::OperationConflict);
        assert!(manager.hss_active);
    }

    #[test]
    fn active_hss_rejects_reads_but_accepts_validated_memory_writes() {
        let spec = TargetConnectionSpec::new(
            "S32K144",
            jlink_domain::TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target spec");
        let mut manager = TargetSessionManager::new();
        manager.connection_state = ConnectionState::Connected;
        manager.active_target = Some(spec.clone());
        manager.hss_active = true;

        let read = DebugRequest::ReadMemory {
            range: jlink_domain::MemoryRange::raw(0x2000_0000, 4).expect("read range"),
        };
        assert_eq!(
            manager
                .ensure_debug_allowed(&spec, &read)
                .expect_err("ordinary read conflicts with HSS")
                .code,
            ErrorCode::OperationConflict
        );

        let write = DebugRequest::WriteMemory {
            address: 0x2000_0000,
            data: vec![1, 2, 3, 4],
            verify: jlink_domain::WriteVerify::None,
        };
        manager
            .ensure_debug_allowed(&spec, &write)
            .expect("HSS accepts validated RAM/MMIO write for serialized scheduling");

        let register_write = DebugRequest::WriteRegister {
            register: jlink_domain::CoreRegister::R0,
            value: 1,
        };
        assert_eq!(
            manager
                .ensure_debug_allowed(&spec, &register_write)
                .expect_err("HSS rejects core-register writes")
                .code,
            ErrorCode::OperationConflict
        );
        assert_eq!(
            manager
                .ensure_control_allowed(&spec)
                .expect_err("HSS rejects target control")
                .code,
            ErrorCode::OperationConflict
        );
    }

    #[test]
    fn flash_result_invalidates_validation_and_uncertain_result_faults_session() {
        let spec = TargetConnectionSpec::new(
            "S32K144",
            jlink_domain::TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target spec");
        let mut completed = TargetSessionManager::new();
        completed.connection_state = ConnectionState::Connected;
        completed.active_target = Some(spec.clone());
        completed.validation_key = Some(spec.clone());
        completed.record_firmware_identity(&"a".repeat(64));
        completed
            .ensure_program_allowed(&spec)
            .expect("connected validated session");
        completed.record_program_result(TargetState::Halted, true);
        assert_eq!(completed.target_state, TargetState::Halted);
        assert!(completed.validation_key.is_none());
        assert!(!completed.firmware_identity_cached(&"a".repeat(64)));
        assert_eq!(
            completed.last_invalidation,
            Some(ValidationInvalidation::FlashModified)
        );
        completed
            .ensure_program_allowed(&spec)
            .expect("Flash invalidates cache without losing active target identity");

        let mut uncertain = TargetSessionManager::new();
        uncertain.connection_state = ConnectionState::Connected;
        uncertain.active_target = Some(spec.clone());
        uncertain.validation_key = Some(spec);
        uncertain
            .record_execution_uncertain(ValidationInvalidation::FlashModified)
            .expect("connected session becomes faulted");
        assert_eq!(uncertain.connection_state, ConnectionState::Faulted);
        assert_eq!(uncertain.target_state, TargetState::Unknown);
        assert!(uncertain.validation_key.is_none());
        assert_eq!(
            uncertain.last_invalidation,
            Some(ValidationInvalidation::FlashModified)
        );
    }

    #[test]
    fn uncertain_control_invalidates_the_active_session() {
        let spec = TargetConnectionSpec::new(
            "S32K144",
            jlink_domain::TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target spec");
        let mut manager = TargetSessionManager::new();
        manager.connection_state = ConnectionState::Connected;
        manager.target_state = TargetState::Halted;
        manager.target_id = Some(1);
        manager.active_target = Some(spec.clone());
        manager.validation_key = Some(spec.clone());
        manager
            .record_execution_uncertain(ValidationInvalidation::ConnectionLost)
            .expect("connected session becomes faulted");

        let status = manager.status("probe", true);
        assert_eq!(status.connection_state, ConnectionState::Faulted);
        assert_eq!(status.target_state, TargetState::Unknown);
        assert!(status.target_id.is_none());
        assert!(!status.validation_cached);
        assert_eq!(
            manager.last_invalidation,
            Some(ValidationInvalidation::ConnectionLost)
        );
        assert_eq!(
            manager
                .ensure_control_allowed(&spec)
                .expect_err("faulted control state cannot be reused")
                .code,
            ErrorCode::InvalidStateTransition
        );
    }

    #[test]
    #[ignore = "requires the explicitly fingerprinted S32K144 hardware environment"]
    fn hardware_hardfault_recovery_uses_same_gateway_session() -> Result<(), JlinkError> {
        let dll_path = PathBuf::from(required_hardware_env("JLINK_MCP_T_P1_SES_DLL")?);
        let probe_serial = parse_hardware_env_u32("JLINK_MCP_T_P1_SES_PROBE_SERIAL")?;
        let speed_khz = parse_hardware_env_u32("JLINK_MCP_T_P1_SES_SPEED_KHZ")?;
        let device = required_hardware_env("JLINK_MCP_T_P1_SES_DEVICE")?;
        let elf_sha256 = required_hardware_env("JLINK_MCP_T_P1_SES_ELF_SHA256")?;
        let spec = TargetConnectionSpec::new(
            device,
            jlink_domain::TargetInterface::Swd,
            speed_khz,
            Some(probe_serial),
            Some(elf_sha256),
        )?;
        let probe_identity = probe_serial.to_string();
        let mut gateway = DllGateway::load(&dll_path)?;
        let mut manager = TargetSessionManager::new();
        manager.connect(&mut gateway, &probe_identity, &spec)?;

        let original_demcr = gateway.inject_hardfault_for_test()?;
        let recovery_result = (|| {
            let observed = gateway.observe_target_state()?;
            if observed != TargetState::HardFault {
                return Err(hardware_test_error(format!(
                    "同一 gateway 会话未观察到 HardFault：{observed:?}"
                )));
            }
            let (state, notifications) = recover_target(&mut gateway, observed)?;
            if state != TargetState::Running
                || notifications != [RecoveryNotification::ResetAfterFault]
            {
                return Err(hardware_test_error(format!(
                    "生产恢复结果不符合预期：state={state:?}，notifications={notifications:?}"
                )));
            }
            Ok(())
        })();
        let cleanup_result = gateway.finish_hardfault_injection_for_test(original_demcr);
        let disconnect_result = manager.disconnect(&mut gateway);
        cleanup_result?;
        disconnect_result?;
        recovery_result
    }

    fn required_hardware_env(name: &str) -> Result<String, JlinkError> {
        env::var(name).map_err(|_| {
            hardware_test_error(format!("真机测试缺少环境变量 {name}，拒绝使用默认值"))
        })
    }

    fn parse_hardware_env_u32(name: &str) -> Result<u32, JlinkError> {
        let value = required_hardware_env(name)?;
        value
            .parse()
            .map_err(|_| hardware_test_error(format!("真机测试环境变量 {name} 不是 u32")))
    }

    fn hardware_test_error(message: impl Into<String>) -> JlinkError {
        JlinkError::new(ErrorCode::TargetRecoveryFailed, message, false)
    }
}
