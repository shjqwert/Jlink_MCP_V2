use jlink_domain::{ErrorCode, JlinkError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::CaptureSnapshot;

const CURSOR_PREFIX: &str = "jmc1";
const CURSOR_SCHEMA_VERSION: u32 = 1;
const CURSOR_DIGEST_DOMAIN: &[u8] = b"jlink-mcp:capture-cursor:v1\0";

/// Verified continuation state bound to one immutable capture and normalized query.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptureCursor {
    payload: CursorPayload,
}

impl CaptureCursor {
    /// Returns the immutable capture identity bound to this cursor.
    #[must_use]
    pub fn capture_id(&self) -> &str {
        &self.payload.capture_id
    }

    /// Returns the normalized first-page arguments retained by this cursor.
    #[must_use]
    pub const fn query(&self) -> &Value {
        &self.payload.query
    }

    /// Returns the next deterministic row or bucket position.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.payload.position
    }

    /// Returns stable series IDs whose dictionary entries were already emitted.
    #[must_use]
    pub fn emitted_series(&self) -> &[String] {
        &self.payload.emitted_series
    }

    /// Returns the frozen ordering identity for the query kind.
    #[must_use]
    pub fn ordering(&self) -> &str {
        &self.payload.ordering
    }

    /// Validates that this cursor still names the exact immutable capture snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::CursorExpired`] if the caller opened a different
    /// immutable content identity under the retained capture ID.
    pub fn validate_snapshot(&self, snapshot: &CaptureSnapshot) -> Result<(), JlinkError> {
        if snapshot.capture_id() == self.payload.capture_id
            && snapshot.raw_sha256() == self.payload.snapshot_sha256
        {
            Ok(())
        } else {
            Err(JlinkError::new(
                ErrorCode::CursorExpired,
                "游标绑定的不可变 capture 快照已不存在或内容身份已变化",
                false,
            )
            .with_detail("capture_id", serde_json::json!(self.payload.capture_id)))
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CursorPayload {
    schema_version: u32,
    capture_id: String,
    snapshot_sha256: String,
    query: Value,
    ordering: String,
    position: u64,
    emitted_series: Vec<String>,
}

/// Encodes one opaque continuation cursor for an immutable capture snapshot.
///
/// # Errors
///
/// Returns [`ErrorCode::CursorInvalid`] if normalized query state cannot be serialized.
pub fn encode_cursor(
    snapshot: &CaptureSnapshot,
    query: &Value,
    ordering: &str,
    position: u64,
    emitted_series: &[String],
) -> Result<String, JlinkError> {
    let mut emitted_series = emitted_series.to_vec();
    emitted_series.sort();
    emitted_series.dedup();
    let payload = CursorPayload {
        schema_version: CURSOR_SCHEMA_VERSION,
        capture_id: snapshot.capture_id().to_owned(),
        snapshot_sha256: snapshot.raw_sha256().to_owned(),
        query: canonicalize(query),
        ordering: ordering.to_owned(),
        position,
        emitted_series,
    };
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        JlinkError::new(
            ErrorCode::CursorInvalid,
            format!("无法序列化查询游标：{error}"),
            false,
        )
    })?;
    let digest = cursor_digest(&bytes, &payload.snapshot_sha256);
    Ok(format!(
        "{CURSOR_PREFIX}.{}.{}",
        encode_hex(&bytes),
        encode_hex(&digest)
    ))
}

/// Decodes and verifies one opaque cursor before any capture query is resumed.
///
/// # Errors
///
/// Returns [`ErrorCode::CursorInvalid`] for malformed, corrupted, unsupported,
/// or internally inconsistent cursor state.
pub fn decode_cursor(cursor: &str) -> Result<CaptureCursor, JlinkError> {
    let mut parts = cursor.split('.');
    if parts.next() != Some(CURSOR_PREFIX) {
        return Err(cursor_invalid("游标前缀或版本无效"));
    }
    let payload_hex = parts
        .next()
        .ok_or_else(|| cursor_invalid("游标缺少 payload"))?;
    let digest_hex = parts
        .next()
        .ok_or_else(|| cursor_invalid("游标缺少校验摘要"))?;
    if parts.next().is_some() {
        return Err(cursor_invalid("游标包含多余段"));
    }
    let bytes = decode_hex(payload_hex)?;
    let supplied_digest = decode_hex(digest_hex)?;
    let payload: CursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| cursor_invalid("游标 payload 不是受支持的严格结构"))?;
    if payload.schema_version != CURSOR_SCHEMA_VERSION
        || payload.capture_id.is_empty()
        || payload.snapshot_sha256.len() != 64
        || payload.ordering.is_empty()
        || !payload.query.is_object()
    {
        return Err(cursor_invalid("游标绑定字段无效"));
    }
    let expected = cursor_digest(&bytes, &payload.snapshot_sha256);
    if supplied_digest.as_slice() != expected {
        return Err(cursor_invalid("游标校验摘要不匹配"));
    }
    Ok(CaptureCursor { payload })
}

fn cursor_digest(payload: &[u8], snapshot_sha256: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(CURSOR_DIGEST_DOMAIN);
    digest.update(snapshot_sha256.as_bytes());
    digest.update(payload);
    digest.finalize().into()
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        Value::Object(object) => {
            let mut fields = object.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(left, _)| *left);
            Value::Object(
                fields
                    .into_iter()
                    .map(|(name, value)| (name.clone(), canonicalize(value)))
                    .collect(),
            )
        }
        scalar => scalar.clone(),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn decode_hex(encoded: &str) -> Result<Vec<u8>, JlinkError> {
    if encoded.is_empty() || !encoded.len().is_multiple_of(2) {
        return Err(cursor_invalid("游标十六进制段长度无效"));
    }
    encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = decode_nibble(pair[0])?;
            let low = decode_nibble(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn decode_nibble(byte: u8) -> Result<u8, JlinkError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(cursor_invalid("游标只允许小写十六进制编码")),
    }
}

fn cursor_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::CursorInvalid, message, false)
}
