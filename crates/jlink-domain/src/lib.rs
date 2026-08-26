//! Pure domain values and validation shared by the J-Link MCP processes.

mod error;
mod ipc;
mod state;

pub use error::{ErrorCode, JlinkError};
pub use ipc::{IpcRequest, IpcResponse, ProtocolVersion, RequestId, SessionCommand};
pub use state::{
    ConnectionState, DispatchState, ExecutionKind, SessionEvent, TargetInterface, TargetState,
    classify_worker_loss, transition_session,
};
