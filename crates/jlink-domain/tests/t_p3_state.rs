//! Primary T-P3-STATE lifecycle, integrity, failure, and recovery assertions.

use jlink_domain::{
    ErrorCode, HssCaptureState, HssDataIntegrity, HssRecoveryNotification, HssRunState,
};

#[test]
fn t_p3_state_keeps_completed_lifecycle_independent_from_degraded_integrity() {
    let mut state = HssCaptureState::starting();
    assert_eq!(state.lifecycle(), HssRunState::Starting);
    assert_eq!(state.integrity(), HssDataIntegrity::Unknown);

    state.mark_running().expect("hardware start");
    state.mark_stopping().expect("internal stop");
    state
        .mark_completed(HssDataIntegrity::Degraded)
        .expect("tail drain completed with retained degraded data");

    assert_eq!(state.lifecycle(), HssRunState::Completed);
    assert_eq!(state.integrity(), HssDataIntegrity::Degraded);
    assert_eq!(state.failure_code(), None);
    assert!(!state.partial_available());
}

#[test]
fn t_p3_state_failed_retains_partial_data_and_rejects_terminal_rewrite() {
    let mut state = HssCaptureState::starting();
    state.mark_running().expect("hardware start");
    state
        .mark_failed(
            ErrorCode::FrameInvalid,
            true,
            vec![
                HssRecoveryNotification::StopCompletedAfterFailure,
                HssRecoveryNotification::PartialDataRetained {
                    complete_records: 9,
                    trailing_bytes: 3,
                },
            ],
        )
        .expect("controlled failure");

    assert_eq!(state.lifecycle(), HssRunState::Failed);
    assert_eq!(state.integrity(), HssDataIntegrity::Unknown);
    assert_eq!(state.failure_code(), Some(ErrorCode::FrameInvalid));
    assert!(state.partial_available());
    assert_eq!(state.recovery_notifications().len(), 2);
    assert_eq!(
        state
            .mark_completed(HssDataIntegrity::Complete)
            .expect_err("terminal state is immutable")
            .code,
        ErrorCode::InvalidStateTransition
    );
}

#[test]
fn t_p3_state_aborted_is_unknown_and_carries_recovery_facts() {
    let mut state = HssCaptureState::starting();
    state
        .mark_aborted("recovered truncated partial", true, true, Vec::new())
        .expect("startup recovery classification");

    assert_eq!(state.lifecycle(), HssRunState::Aborted);
    assert_eq!(state.integrity(), HssDataIntegrity::Unknown);
    assert_eq!(state.reason(), Some("recovered truncated partial"));
    assert_eq!(state.recoverable(), Some(true));
    assert!(state.partial_available());
    assert_eq!(
        state.recovery_notifications(),
        [HssRecoveryNotification::AbortedCaptureRecovered]
    );
}
