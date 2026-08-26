use std::{fmt, str::FromStr};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;

use crate::{JlinkError, state::ExecutionKind};

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
#[serde(rename_all = "lowercase")]
pub enum SessionCommand {
    /// Request a new worker connection.
    Connect,
    /// Release the current worker connection.
    Disconnect,
    /// Read the worker's observed session status.
    Status,
    /// Perform a side-effect-free validation pass.
    Validate,
}

impl SessionCommand {
    /// Returns whether executing this command can change target or session state.
    #[must_use]
    pub const fn execution_kind(self) -> ExecutionKind {
        match self {
            Self::Status | Self::Validate => ExecutionKind::ReadOnly,
            Self::Connect | Self::Disconnect => ExecutionKind::SideEffect,
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
        }
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
