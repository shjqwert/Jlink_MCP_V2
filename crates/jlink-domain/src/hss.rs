use serde_json::json;

use crate::{ErrorCode, JlinkError};

const HSS_TIMESTAMP_BYTES: u32 = 4;

/// Default per-block flags verified by the F0-A J-Link 6.98a mainline.
pub const HSS_BLOCK_FLAGS_DEFAULT: u32 = 0;
/// Start flags for the supported J-Link 6.98a millisecond timestamp mode.
pub const HSS_START_FLAGS_698A_MAINLINE: i32 = 0;
/// Experimental microsecond timestamp flag observed in F0-A but not supported by V1.
pub const HSS_START_FLAG_TIMESTAMP_US_EXPERIMENTAL: i32 = 1;

/// Frozen byte layout for one sequence of J-Link HSS records.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HssFrameLayout {
    sample_bytes: u32,
    record_bytes: u32,
}

impl HssFrameLayout {
    /// Builds a layout from the byte count of each HSS block.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::FrameInvalid`] when there are no blocks, one block
    /// is empty, or the timestamp plus sample payload exceeds `u32`.
    pub fn new(block_byte_counts: &[u32]) -> Result<Self, JlinkError> {
        if block_byte_counts.is_empty() {
            return Err(frame_invalid("HSS 帧布局至少需要一个采样块"));
        }
        let mut sample_bytes = 0_u32;
        for (index, byte_count) in block_byte_counts.iter().copied().enumerate() {
            if byte_count == 0 {
                return Err(frame_invalid("HSS 采样块长度不能为 0")
                    .with_detail("block_index", json!(index)));
            }
            sample_bytes = sample_bytes.checked_add(byte_count).ok_or_else(|| {
                frame_invalid("HSS 采样块总长度溢出").with_detail("block_index", json!(index))
            })?;
        }
        let record_bytes = HSS_TIMESTAMP_BYTES
            .checked_add(sample_bytes)
            .ok_or_else(|| frame_invalid("HSS 时间戳与采样载荷总长度溢出"))?;
        Ok(Self {
            sample_bytes,
            record_bytes,
        })
    }

    /// Returns the concatenated sample payload length after the timestamp.
    #[must_use]
    pub const fn sample_bytes(self) -> u32 {
        self.sample_bytes
    }

    /// Returns the complete record length, including the 32-bit timestamp.
    #[must_use]
    pub const fn record_bytes(self) -> u32 {
        self.record_bytes
    }

    /// Parses every complete little-endian record and preserves an incomplete tail.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::FrameInvalid`] if the validated layout cannot be
    /// applied without truncating a complete record.
    pub fn parse(self, bytes: &[u8]) -> Result<HssFrameBatch<'_>, JlinkError> {
        let record_bytes = self.record_bytes as usize;
        let complete_bytes = bytes.len() / record_bytes * record_bytes;
        let complete = bytes
            .get(..complete_bytes)
            .ok_or_else(|| frame_invalid("HSS 完整帧边界超出输入"))?;
        let mut frames = Vec::with_capacity(complete.len() / record_bytes);
        for record in complete.chunks_exact(record_bytes) {
            let timestamp_bytes: [u8; 4] = record
                .get(..HSS_TIMESTAMP_BYTES as usize)
                .ok_or_else(|| frame_invalid("HSS 完整帧缺少时间戳"))?
                .try_into()
                .map_err(|_| frame_invalid("HSS 时间戳长度无效"))?;
            let sample = record
                .get(HSS_TIMESTAMP_BYTES as usize..)
                .ok_or_else(|| frame_invalid("HSS 完整帧缺少采样载荷"))?;
            frames.push(HssRawFrame {
                timestamp_raw: u32::from_le_bytes(timestamp_bytes),
                sample,
            });
        }
        let incomplete_tail = bytes
            .get(complete_bytes..)
            .ok_or_else(|| frame_invalid("HSS 尾部边界超出输入"))?;
        Ok(HssFrameBatch {
            frames,
            incomplete_tail,
        })
    }
}

/// One complete raw HSS frame with the frozen 32-bit source timestamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HssRawFrame<'a> {
    /// Source timestamp exactly as returned by J-Link.
    pub timestamp_raw: u32,
    /// Concatenated block payload in declared block order.
    pub sample: &'a [u8],
}

/// Complete frames plus any trailing bytes that require quality classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HssFrameBatch<'a> {
    /// Every complete frame in source order.
    pub frames: Vec<HssRawFrame<'a>>,
    /// Bytes that do not form a complete frame; never silently discarded.
    pub incomplete_tail: &'a [u8],
}

fn frame_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::FrameInvalid, message, false)
}
