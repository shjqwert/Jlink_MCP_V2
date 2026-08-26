//! Pure domain values and validation shared by the J-Link MCP processes.

mod dwarf;
mod error;
mod image;
mod ipc;
mod session;
mod state;

pub use dwarf::{
    ACCESS_PLAN_FORMAT_VERSION, AccessLayout, AccessMember, AccessPlan, BitRange, ElementSlice,
    ScalarEncoding, SelectorStep, VariableSelector,
};
pub use error::{ErrorCode, JlinkError};
pub use image::{
    FirmwareFormat, FirmwareIdentityPlan, FirmwareImage, FirmwareSegment,
    FirmwareSegmentFingerprint,
};
pub use ipc::{
    IpcRequest, IpcResponse, MAX_IPC_FRAME_BYTES, ProtocolVersion, RequestId, SessionCommand,
    WorkerStatus, probe_identity_hash, read_ipc_frame, worker_endpoint_name, write_ipc_frame,
};
pub use session::{
    FaultDiagnostics, RecoveryAction, RecoveryNotification, TargetConnectionSpec, ValidationAfter,
    ValidationCheck, ValidationCheckKind, ValidationInvalidation, ValidationReport,
    ensure_disconnect_allowed,
};
pub use state::{
    ConnectionState, DispatchState, ExecutionKind, SessionEvent, TargetInterface, TargetState,
    classify_worker_loss, transition_session,
};
