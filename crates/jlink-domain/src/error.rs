use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable machine-readable error identifiers exposed by the domain contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    /// The request identifier is empty or contains only whitespace.
    InvalidRequestId,
    /// The message uses a protocol version that this build does not support.
    UnknownProtocolVersion,
    /// A session event is not valid for the current connection state.
    InvalidStateTransition,
    /// The worker could not be reached before a side effect was dispatched.
    WorkerUnavailable,
    /// A dispatched side effect may have reached the target, but its result is unknown.
    ExecutionUncertain,
    /// A response contains an invalid combination of result and error fields.
    InvalidResponse,
    /// Configuration data is missing, malformed, or outside the supported range.
    ConfigInvalid,
    /// The requested configuration change conflicts with active session state.
    OperationConflict,
    /// The configured J-Link DLL does not exist.
    DllNotFound,
    /// The configured J-Link DLL is not a Windows x64 PE image.
    DllArchitectureMismatch,
    /// The configured J-Link DLL has an unexpected file version.
    DllVersionMismatch,
    /// The configured J-Link DLL has an unexpected SHA-256 digest.
    DllHashMismatch,
    /// A local IPC frame is malformed or exceeds the frozen transport limit.
    IpcProtocolError,
    /// Another Worker currently owns the requested probe lease.
    ProbeBusy,
    /// Windows could not load the validated J-Link DLL into the Worker.
    DllLoadFailed,
    /// The loaded J-Link DLL does not provide a required V1 export.
    DllExportMissing,
    /// The configured probe or target could not be connected.
    TargetConnectFailed,
    /// The target could not be returned to a stable running state.
    TargetRecoveryFailed,
}

impl ErrorCode {
    /// Returns the stable wire spelling of this error code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequestId => "INVALID_REQUEST_ID",
            Self::UnknownProtocolVersion => "UNKNOWN_PROTOCOL_VERSION",
            Self::InvalidStateTransition => "INVALID_STATE_TRANSITION",
            Self::WorkerUnavailable => "WORKER_UNAVAILABLE",
            Self::ExecutionUncertain => "EXECUTION_UNCERTAIN",
            Self::InvalidResponse => "INVALID_RESPONSE",
            Self::ConfigInvalid => "CONFIG_INVALID",
            Self::OperationConflict => "OPERATION_CONFLICT",
            Self::DllNotFound => "DLL_NOT_FOUND",
            Self::DllArchitectureMismatch => "DLL_ARCHITECTURE_MISMATCH",
            Self::DllVersionMismatch => "DLL_VERSION_MISMATCH",
            Self::DllHashMismatch => "DLL_HASH_MISMATCH",
            Self::IpcProtocolError => "IPC_PROTOCOL_ERROR",
            Self::ProbeBusy => "PROBE_BUSY",
            Self::DllLoadFailed => "DLL_LOAD_FAILED",
            Self::DllExportMissing => "DLL_EXPORT_MISSING",
            Self::TargetConnectFailed => "TARGET_CONNECT_FAILED",
            Self::TargetRecoveryFailed => "TARGET_RECOVERY_FAILED",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stable, serializable error returned by pure domain validation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JlinkError {
    /// Stable machine-readable error code.
    pub code: ErrorCode,
    /// Human-readable, actionable explanation.
    pub message: String,
    /// Whether retrying the same operation is safe and useful.
    pub retryable: bool,
    /// Structured facts that help a caller correct its request, when useful.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, Value>>,
}

impl JlinkError {
    /// Creates an error without optional details.
    pub fn new(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            details: None,
        }
    }

    /// Adds one structured detail and returns the updated error.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: Value) -> Self {
        self.details
            .get_or_insert_with(BTreeMap::new)
            .insert(key.into(), value);
        self
    }

    /// Returns the stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Builds the stable error for an illegal session transition.
    pub(crate) fn invalid_transition(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidStateTransition, message, false)
    }
}

impl fmt::Display for JlinkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for JlinkError {}
