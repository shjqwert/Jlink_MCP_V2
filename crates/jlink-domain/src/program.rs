use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ErrorCode, FirmwareImage, JlinkError};

/// Target state requested after a Flash-modifying operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramAfter {
    /// Do not reset or deliberately change the state left by the Flash algorithm.
    None,
    /// Reset the target and leave its core halted.
    ResetHalt,
    /// Reset the target and leave its core stably running.
    ResetRun,
}

/// One non-empty Flash range supplied by the J-Link device database.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlashRegion {
    address: u64,
    length: u64,
}

impl FlashRegion {
    /// Creates a checked non-empty half-open Flash region.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::FlashRangeInvalid`] when the length is zero or the
    /// exclusive end cannot be represented.
    pub fn new(address: u64, length: u64) -> Result<Self, JlinkError> {
        checked_end(address, length)?;
        Ok(Self { address, length })
    }

    /// Returns the first address in this region.
    #[must_use]
    pub const fn address(self) -> u64 {
        self.address
    }

    /// Returns the region length in bytes.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }

    /// Returns whether a non-empty range is fully contained in this region.
    #[must_use]
    pub fn contains(self, address: u64, length: u64) -> bool {
        let Some(end) = address.checked_add(length) else {
            return false;
        };
        let Some(region_end) = self.address.checked_add(self.length) else {
            return false;
        };
        length > 0 && address >= self.address && end <= region_end
    }
}

/// One explicit range requested by `jlink_program.erase`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlashRange {
    address: u64,
    length: u64,
}

impl FlashRange {
    /// Creates a checked non-empty erase range.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::FlashRangeInvalid`] when the length is zero or the
    /// exclusive end cannot be represented.
    pub fn new(address: u64, length: u64) -> Result<Self, JlinkError> {
        checked_end(address, length)?;
        Ok(Self { address, length })
    }

    /// Returns the first address in the erase range.
    #[must_use]
    pub const fn address(self) -> u64 {
        self.address
    }

    /// Returns the erase range length in bytes.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }
}

/// Closed set of Flash operations carried from MCP to the authoritative Worker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub enum ProgramRequest {
    /// Program one image, optionally verify it, and apply an explicit final state.
    Flash {
        /// Image path resolved by the MCP configuration owner.
        image: PathBuf,
        /// Required only for a raw BIN image.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_address: Option<u64>,
        /// Whether to compare every programmed segment with target readback.
        verify: bool,
        /// Explicit final target state policy.
        after: ProgramAfter,
    },
    /// Erase the whole device or one explicit checked range.
    Erase {
        /// `None` selects whole-chip erase.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        range: Option<FlashRange>,
        /// Explicit final target state policy.
        after: ProgramAfter,
    },
    /// Compare one image with target Flash without modifying the target.
    Verify {
        /// Image path resolved by the MCP configuration owner.
        image: PathBuf,
        /// Required only for a raw BIN image.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_address: Option<u64>,
    },
}

impl ProgramRequest {
    /// Returns whether this request can modify target Flash.
    #[must_use]
    pub const fn modifies_flash(&self) -> bool {
        matches!(self, Self::Flash { .. } | Self::Erase { .. })
    }
}

/// Compact mismatch facts safe to return through the Agent-facing contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifyMismatch {
    first_address: u64,
    first_length: u64,
    total_regions: u64,
}

impl VerifyMismatch {
    /// Converts the compact mismatch facts into the stable public error.
    #[must_use]
    pub fn into_error(self) -> JlinkError {
        JlinkError::new(
            ErrorCode::VerifyFailed,
            "目标 Flash 与请求镜像不匹配",
            false,
        )
        .with_detail(
            "first_address",
            json!(format!("0x{:X}", self.first_address)),
        )
        .with_detail("first_length", json!(self.first_length))
        .with_detail("total_regions", json!(self.total_regions))
    }
}

/// Streaming mismatch accumulator that never retains or exposes target bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VerifyMismatchAccumulator {
    first_address: Option<u64>,
    first_length: u64,
    total_regions: u64,
}

impl VerifyMismatchAccumulator {
    /// Creates an empty accumulator.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            first_address: None,
            first_length: 0,
            total_regions: 0,
        }
    }

    /// Compares one complete image segment with an equally sized target readback.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] when the readback length differs or
    /// a mismatch address cannot be represented.
    pub fn compare_segment(
        &mut self,
        address: u64,
        expected: &[u8],
        actual: &[u8],
    ) -> Result<(), JlinkError> {
        if expected.len() != actual.len() {
            return Err(JlinkError::new(
                ErrorCode::ValueInvalid,
                "校验读回长度与镜像段长度不一致",
                false,
            ));
        }

        let mut index = 0_usize;
        while index < expected.len() {
            if expected[index] == actual[index] {
                index += 1;
                continue;
            }
            let start = index;
            while index < expected.len() && expected[index] != actual[index] {
                index += 1;
            }
            let run_length = index - start;
            let start = u64::try_from(start).map_err(|_| mismatch_address_error())?;
            let run_length = u64::try_from(run_length).map_err(|_| mismatch_address_error())?;
            let run_address = address
                .checked_add(start)
                .ok_or_else(mismatch_address_error)?;
            self.total_regions = self
                .total_regions
                .checked_add(1)
                .ok_or_else(mismatch_address_error)?;
            if self.first_address.is_none() {
                self.first_address = Some(run_address);
                self.first_length = run_length;
            }
        }
        Ok(())
    }

    /// Returns compact mismatch facts, or `None` when every byte matched.
    #[must_use]
    pub const fn finish(self) -> Option<VerifyMismatch> {
        match self.first_address {
            Some(first_address) => Some(VerifyMismatch {
                first_address,
                first_length: self.first_length,
                total_regions: self.total_regions,
            }),
            None => None,
        }
    }
}

/// Verifies that every image segment is fully inside one known Flash region.
///
/// # Errors
///
/// Returns [`ErrorCode::FlashRangeInvalid`] before target access when any
/// segment is empty, overflows, or crosses a device Flash boundary.
pub fn validate_image_flash_ranges(
    image: &FirmwareImage,
    regions: &[FlashRegion],
) -> Result<(), JlinkError> {
    for segment in image.segments() {
        let length = u64::try_from(segment.data().len())
            .map_err(|_| flash_range_error(segment.address(), u64::MAX, "镜像段长度无法表示"))?;
        validate_flash_range(regions, segment.address(), length)?;
    }
    Ok(())
}

/// Verifies that one non-empty range is fully inside one known Flash region.
///
/// # Errors
///
/// Returns [`ErrorCode::FlashRangeInvalid`] when the range is invalid or
/// crosses a region boundary.
pub fn validate_flash_range(
    regions: &[FlashRegion],
    address: u64,
    length: u64,
) -> Result<(), JlinkError> {
    checked_end(address, length)?;
    if regions
        .iter()
        .any(|region| region.contains(address, length))
    {
        return Ok(());
    }
    Err(flash_range_error(
        address,
        length,
        "请求范围不在任何已知 Flash 区域内",
    ))
}

fn checked_end(address: u64, length: u64) -> Result<u64, JlinkError> {
    if length == 0 {
        return Err(flash_range_error(
            address,
            length,
            "Flash 范围长度必须大于零",
        ));
    }
    address
        .checked_add(length)
        .ok_or_else(|| flash_range_error(address, length, "Flash 范围结束地址溢出"))
}

fn flash_range_error(address: u64, length: u64, message: &str) -> JlinkError {
    JlinkError::new(ErrorCode::FlashRangeInvalid, message, false)
        .with_detail("address", json!(format!("0x{address:X}")))
        .with_detail("length", json!(length))
}

fn mismatch_address_error() -> JlinkError {
    JlinkError::new(
        ErrorCode::ValueInvalid,
        "校验不匹配区域的地址或计数溢出",
        false,
    )
}
