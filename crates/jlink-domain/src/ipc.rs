use std::{
    fmt,
    io::{Read, Write},
    str::FromStr,
};

use serde::de::DeserializeOwned;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{JlinkError, TargetConnectionSpec, state::ExecutionKind};

/// Maximum UTF-8 JSON payload carried by one local IPC frame.
pub const MAX_IPC_FRAME_BYTES: usize = 1024 * 1024;

/// Returns the stable SHA-256 identity used by Worker endpoints and lease files.
///
/// # Errors
///
/// Returns [`crate::ErrorCode::ConfigInvalid`] when the probe identity is blank.
pub fn probe_identity_hash(identity: &str) -> Result<String, JlinkError> {
    if identity.trim().is_empty() {
        return Err(JlinkError::new(
            crate::ErrorCode::ConfigInvalid,
            "探针身份不能为空",
            false,
        ));
    }
    let digest = Sha256::digest(identity.as_bytes());
    let mut output = String::with_capacity(64);
    for byte in digest {
        use fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

/// Returns the stable local named-pipe endpoint for one probe identity.
///
/// # Errors
///
/// Returns [`crate::ErrorCode::ConfigInvalid`] when the probe identity is blank.
pub fn worker_endpoint_name(identity: &str) -> Result<String, JlinkError> {
    Ok(format!(
        r"\\.\pipe\jlink-mcp-v1-{}",
        probe_identity_hash(identity)?
    ))
}

/// Writes one length-prefixed UTF-8 JSON message to a byte-mode transport.
///
/// # Errors
///
/// Returns [`crate::ErrorCode::IpcProtocolError`] when serialization fails, the
/// payload exceeds [`MAX_IPC_FRAME_BYTES`], or the transport cannot accept the
/// complete frame.
pub fn write_ipc_frame<W: Write, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), JlinkError> {
    let payload = serde_json::to_vec(message).map_err(ipc_protocol_error)?;
    if payload.len() > MAX_IPC_FRAME_BYTES {
        return Err(ipc_protocol_error(format!(
            "IPC 负载超过 {MAX_IPC_FRAME_BYTES} 字节"
        )));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| ipc_protocol_error("IPC 负载长度无法表示为 u32"))?;
    writer
        .write_all(&length.to_le_bytes())
        .and_then(|()| writer.write_all(&payload))
        .map_err(ipc_protocol_error)
}

/// Reads one complete length-prefixed UTF-8 JSON message from a byte-mode transport.
///
/// # Errors
///
/// Returns [`crate::ErrorCode::IpcProtocolError`] for truncated, oversized,
/// malformed, unknown-field, or unsupported-version messages.
pub fn read_ipc_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, JlinkError> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix).map_err(ipc_protocol_error)?;
    let length = usize::try_from(u32::from_le_bytes(prefix))
        .map_err(|_| ipc_protocol_error("IPC 负载长度不受支持"))?;
    if length > MAX_IPC_FRAME_BYTES {
        return Err(ipc_protocol_error(format!(
            "IPC 负载超过 {MAX_IPC_FRAME_BYTES} 字节"
        )));
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(ipc_protocol_error)?;
    serde_json::from_slice(&payload).map_err(ipc_protocol_error)
}

fn ipc_protocol_error(error: impl fmt::Display) -> JlinkError {
    JlinkError::new(
        crate::ErrorCode::IpcProtocolError,
        format!("无效 IPC 帧：{error}"),
        false,
    )
}

/// The only wire protocol version accepted by the V1 worker contract.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ProtocolVersion {
    /// Version one, encoded as the JSON number `1`.
    V1 = 1,
}

impl ProtocolVersion {
    /// Returns the numeric wire representation.
    #[must_use]
    pub const fn value(self) -> u8 {
        self as u8
    }
}

impl Serialize for ProtocolVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.value())
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ProtocolVersionVisitor;

        impl de::Visitor<'_> for ProtocolVersionVisitor {
            type Value = ProtocolVersion;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("the numeric protocol version 1")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == u64::from(ProtocolVersion::V1.value()) {
                    Ok(ProtocolVersion::V1)
                } else {
                    Err(E::invalid_value(de::Unexpected::Unsigned(value), &self))
                }
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == i64::from(ProtocolVersion::V1.value()) {
                    Ok(ProtocolVersion::V1)
                } else {
                    Err(E::invalid_value(de::Unexpected::Signed(value), &self))
                }
            }
        }

        deserializer.deserialize_u8(ProtocolVersionVisitor)
    }
}

/// A non-blank identifier used to correlate one IPC request and response.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId(String);

impl RequestId {
    /// Validates and constructs an identifier without normalizing its contents.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidRequestId`](crate::ErrorCode::InvalidRequestId)
    /// when `value` is empty or contains only whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self, JlinkError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(JlinkError::new(
                crate::ErrorCode::InvalidRequestId,
                "request_id must not be empty or whitespace",
                false,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for RequestId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for RequestId {
    type Err = JlinkError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for RequestId {
    type Error = JlinkError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for RequestId {
    type Error = JlinkError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RequestId> for String {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

impl Serialize for RequestId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|error| D::Error::custom(error.message))
    }
}

/// The closed set of session commands carried by V1 IPC.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCommand {
    /// Request a new worker connection.
    Connect,
    /// Release the current worker connection.
    Disconnect,
    /// Read the worker's observed session status.
    Status,
    /// Perform an observational or explicitly finalized validation pass.
    Validate,
    /// Program one image through the selected device Flash algorithm.
    Flash,
    /// Erase the whole device or one checked Flash range.
    Erase,
    /// Compare an image with target Flash without modifying it.
    Verify,
    /// Read one exact raw-memory range.
    ReadMemory,
    /// Write one exact raw-memory range.
    WriteMemory,
    /// Read one ELF-bound typed variable.
    ReadVariable,
    /// Write one ELF-bound typed variable.
    WriteVariable,
}

impl SessionCommand {
    /// Returns whether executing this command can change target or session state.
    #[must_use]
    pub const fn execution_kind(self) -> ExecutionKind {
        match self {
            Self::Status | Self::Verify | Self::ReadMemory | Self::ReadVariable => {
                ExecutionKind::ReadOnly
            }
            Self::Connect
            | Self::Disconnect
            | Self::Validate
            | Self::Flash
            | Self::Erase
            | Self::WriteMemory
            | Self::WriteVariable => ExecutionKind::SideEffect,
        }
    }
}

/// A versioned command sent from the MCP process to the worker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IpcRequest {
    /// Explicit protocol version used to decode this message.
    pub protocol_version: ProtocolVersion,
    /// Correlation identifier echoed by the worker.
    pub request_id: RequestId,
    /// The closed-set session operation.
    pub command: SessionCommand,
    /// Immutable target inputs required by connect and validate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetConnectionSpec>,
    /// Required final state for disconnected validation and forbidden otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<crate::ValidationAfter>,
    /// Typed Flash operation payload required by flash, erase, and verify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<crate::ProgramRequest>,
    /// Typed ordinary debug payload required by memory and variable commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<crate::DebugRequest>,
}

impl IpcRequest {
    /// Creates a request with an already validated identifier.
    #[must_use]
    pub const fn new(
        protocol_version: ProtocolVersion,
        request_id: RequestId,
        command: SessionCommand,
    ) -> Self {
        Self {
            protocol_version,
            request_id,
            command,
            target: None,
            after: None,
            program: None,
            debug: None,
        }
    }

    /// Attaches immutable target inputs to connect or validate.
    #[must_use]
    pub fn with_target(mut self, target: TargetConnectionSpec) -> Self {
        self.target = Some(target);
        self
    }

    /// Attaches the explicit final state for a disconnected validation pass.
    #[must_use]
    pub const fn with_validation_after(mut self, after: crate::ValidationAfter) -> Self {
        self.after = Some(after);
        self
    }

    /// Attaches the typed Flash operation payload.
    #[must_use]
    pub fn with_program(mut self, program: crate::ProgramRequest) -> Self {
        self.program = Some(program);
        self
    }

    /// Attaches one typed ordinary memory or variable operation.
    #[must_use]
    pub fn with_debug(mut self, debug: crate::DebugRequest) -> Self {
        self.debug = Some(debug);
        self
    }
}

/// A versioned worker response containing either a result or a stable error.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IpcResponse {
    /// Explicit protocol version used to encode this message.
    pub protocol_version: ProtocolVersion,
    /// Correlation identifier copied from the request.
    pub request_id: RequestId,
    /// Successful command result, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Stable command error, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JlinkError>,
}

/// Read-only process identity returned by the Worker status command.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerStatus {
    /// Operating-system process identifier of the authoritative Worker.
    pub worker_pid: u32,
    /// Stable hash used by the endpoint and probe lease without exposing the serial.
    pub probe_identity_hash: String,
    /// Whether the validated DLL is currently held by the unique gateway.
    pub dll_loaded: bool,
    /// Last authoritative connection lifecycle state.
    pub connection_state: crate::ConnectionState,
    /// Last target execution state observed by the Worker.
    pub target_state: crate::TargetState,
    /// Target identifier observed after a successful connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<u32>,
    /// Whether an HSS capture currently prevents disconnect.
    pub hss_active: bool,
    /// Whether normal device operations may reuse successful validation.
    pub validation_cached: bool,
    /// Number of complete validation passes in this Worker process.
    pub validation_runs: u64,
    /// Recovery notifications retained for the active connection.
    pub recovery_notifications: Vec<crate::RecoveryNotification>,
}

impl IpcResponse {
    /// Creates a successful response with a JSON result.
    #[must_use]
    pub const fn success(
        protocol_version: ProtocolVersion,
        request_id: RequestId,
        result: Value,
    ) -> Self {
        Self {
            protocol_version,
            request_id,
            result: Some(result),
            error: None,
        }
    }

    /// Creates an error response with no result payload.
    #[must_use]
    pub const fn failure(
        protocol_version: ProtocolVersion,
        request_id: RequestId,
        error: JlinkError,
    ) -> Self {
        Self {
            protocol_version,
            request_id,
            result: None,
            error: Some(error),
        }
    }

    /// Verifies that exactly one response branch is present.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidResponse`](crate::ErrorCode::InvalidResponse)
    /// when both branches are present or both are absent.
    pub fn validate(&self) -> Result<(), JlinkError> {
        match (self.result.is_some(), self.error.is_some()) {
            (true, false) | (false, true) => Ok(()),
            _ => Err(JlinkError::new(
                crate::ErrorCode::InvalidResponse,
                "IPC response must contain exactly one of result or error",
                false,
            )),
        }
    }
}
