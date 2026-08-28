use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ErrorCode, JlinkError};

/// The physical debug interface selected for a target connection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum TargetInterface {
    /// Serial Wire Debug.
    #[serde(rename = "swd")]
    Swd,
    /// Joint Test Action Group debug interface.
    #[serde(rename = "jtag")]
    Jtag,
}

/// The MCP-side lifecycle state of the worker-backed session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    /// No worker connection is currently owned.
    Disconnected,
    /// A worker connection is being established.
    Connecting,
    /// A worker connection is active.
    Connected,
    /// The worker or connection failed and its former state is not trusted.
    Faulted,
}

/// The last observed execution state of the target core.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum TargetState {
    /// The target core is executing instructions.
    #[serde(rename = "running")]
    Running,
    /// The target core is halted.
    #[serde(rename = "halted")]
    Halted,
    /// The target core is in a `HardFault` condition.
    #[serde(rename = "hardfault")]
    HardFault,
    /// The worker has no trustworthy observation of the target core.
    #[serde(rename = "unknown")]
    Unknown,
}

/// Whether an operation can change target or session state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    /// An operation that only observes already available state.
    ReadOnly,
    /// An operation that may produce a device or session side effect.
    SideEffect,
}

/// How far a request progressed through worker dispatch.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    /// The worker did not receive the operation.
    NotDispatched,
    /// The worker received the operation but did not complete it.
    Dispatched,
    /// The worker reported completion for the operation.
    Completed,
}

/// An observed event that can change the connection lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEvent {
    /// Begin establishing a worker connection.
    ConnectRequested,
    /// The worker connection completed successfully.
    Connected,
    /// The worker rejected or failed a connection attempt.
    ConnectFailed,
    /// Begin releasing a worker connection.
    DisconnectRequested,
    /// The worker connection is no longer owned.
    Disconnected,
    /// The worker became unavailable while an operation was in flight.
    WorkerLost,
}

/// Applies one legal session event to a connection state.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidStateTransition`](crate::ErrorCode::InvalidStateTransition)
/// when the event is not legal for `current`.
pub fn transition_session(
    current: ConnectionState,
    event: SessionEvent,
) -> Result<ConnectionState, JlinkError> {
    let next = match (current, event) {
        (
            ConnectionState::Disconnected | ConnectionState::Faulted,
            SessionEvent::ConnectRequested,
        ) => ConnectionState::Connecting,
        (ConnectionState::Connecting, SessionEvent::Connected) => ConnectionState::Connected,
        (ConnectionState::Connecting | ConnectionState::Connected, SessionEvent::WorkerLost) => {
            ConnectionState::Faulted
        }
        (ConnectionState::Connecting, SessionEvent::ConnectFailed)
        | (
            ConnectionState::Connected | ConnectionState::Faulted,
            SessionEvent::DisconnectRequested,
        ) => ConnectionState::Disconnected,
        (ConnectionState::Disconnected, SessionEvent::Disconnected) => {
            return Err(invalid_transition(current, event));
        }
        _ => return Err(invalid_transition(current, event)),
    };
    Ok(next)
}

/// Classifies the consequence of losing the worker during one operation.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidStateTransition`](crate::ErrorCode::InvalidStateTransition)
/// when the operation was already completed and therefore must not be reclassified.
pub fn classify_worker_loss(
    execution: ExecutionKind,
    dispatch: DispatchState,
) -> Result<JlinkError, JlinkError> {
    if dispatch == DispatchState::Completed {
        return Err(invalid_transition(
            ConnectionState::Faulted,
            SessionEvent::WorkerLost,
        ));
    }

    let (code, message, retryable) = match (execution, dispatch) {
        (ExecutionKind::SideEffect, DispatchState::Dispatched) => (
            ErrorCode::ExecutionUncertain,
            "worker lost after a side effect was dispatched; the target result is unknown",
            false,
        ),
        (ExecutionKind::ReadOnly, DispatchState::Dispatched)
        | (_, DispatchState::NotDispatched) => (
            ErrorCode::WorkerUnavailable,
            "worker unavailable before a completed result was observed",
            true,
        ),
        (ExecutionKind::ReadOnly | ExecutionKind::SideEffect, DispatchState::Completed) => {
            unreachable!()
        }
    };

    Ok(JlinkError::new(code, message, retryable)
        .with_detail("dispatch_state", json!(dispatch))
        .with_detail("execution_kind", json!(execution)))
}

fn invalid_transition(current: ConnectionState, event: SessionEvent) -> JlinkError {
    JlinkError::invalid_transition(format!(
        "cannot apply {event:?} while session is {current:?}"
    ))
    .with_detail("current_state", json!(current))
    .with_detail("event", json!(event))
}
