use jlink_domain::{
    ControlAfter, ControlRequest, ErrorCode, JlinkError, TargetConnectionSpec, TargetState,
    ValidationInvalidation,
};
use serde_json::json;

use crate::{gateway::DllGateway, session::TargetSessionManager};

/// Executes one checked public target-control request.
pub(crate) fn execute_control(
    session: &mut TargetSessionManager,
    gateway: &mut DllGateway,
    target: &TargetConnectionSpec,
    request: ControlRequest,
) -> Result<(), JlinkError> {
    session.ensure_control_allowed(target)?;
    let expected = match request {
        ControlRequest::Halt
        | ControlRequest::Reset {
            after: ControlAfter::Halt,
        }
        | ControlRequest::Step => TargetState::Halted,
        ControlRequest::Resume
        | ControlRequest::Reset {
            after: ControlAfter::Run,
        } => TargetState::Running,
    };
    let actual_result = match request {
        ControlRequest::Halt => gateway.halt_and_observe(),
        ControlRequest::Resume => gateway.resume_and_observe(),
        ControlRequest::Reset {
            after: ControlAfter::Run,
        } => gateway.reset_run_and_observe(),
        ControlRequest::Reset {
            after: ControlAfter::Halt,
        } => gateway.reset_halt_and_observe(),
        ControlRequest::Step => gateway.step_and_observe(),
    };
    let actual = match actual_result {
        Ok(state) => state,
        Err(error) if error.code == ErrorCode::InvalidStateTransition => return Err(error),
        Err(error) => {
            gateway.close_target();
            session.record_execution_uncertain(ValidationInvalidation::ConnectionLost)?;
            return Err(error);
        }
    };
    session.record_control_state(actual);
    if actual != expected {
        return Err(JlinkError::new(
            ErrorCode::TargetRecoveryFailed,
            format!("目标控制未收口到请求状态：期望 {expected:?}，实际 {actual:?}"),
            true,
        )
        .with_detail("expected", json!(expected))
        .with_detail("actual", json!(actual)));
    }
    Ok(())
}
