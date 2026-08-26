//! Pure domain values and validation shared by the J-Link MCP processes.

mod error;
mod ipc;
mod state;

pub use error::{ErrorCode, JlinkError};
pub use ipc::{
    IpcRequest, IpcResponse, MAX_IPC_FRAME_BYTES, ProtocolVersion, RequestId, SessionCommand,
    WorkerStatus, probe_identity_hash, read_ipc_frame, worker_endpoint_name, write_ipc_frame,
};
pub use state::{
    ConnectionState, DispatchState, ExecutionKind, SessionEvent, TargetInterface, TargetState,
    classify_worker_loss, transition_session,
};
