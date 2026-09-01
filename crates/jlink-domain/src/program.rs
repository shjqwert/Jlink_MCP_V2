use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ErrorCode, FirmwareImage, JlinkError, MemoryRegion, TargetState};

/// Stable stages of a Flash-modifying operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramStage {
    /// Reset/halt and target preparation before any download transaction.
    TargetPreparation,
    /// Save/write/readback/restore of the selected Loader RAM.
    LoaderRamPreflight,
    /// `JLINKARM_BeginDownload` dispatch.
    BeginDownload,
    /// One or more image/range chunks submitted to the DLL.
    SegmentCommit,
    /// `JLINKARM_EndDownload` completion.
    EndDownload,
    /// Reset/halt needed before requested readback verification.
    VerifyPreparation,
    /// Exact requested range verification.
    RangeVerification,
    /// Explicit final target state policy.
    FinalState,
}

/// What can be proven about target Flash after a failure.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashModifiedState {
    /// No Flash side effect was dispatched.
    False,
    /// The Flash operation completed successfully.
    True,
    /// A side effect was dispatched but completion could not be proven.
    Unknown,
}

/// One submitted or confirmed half-open Flash range.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramRangeFact {
    /// First byte address.
    pub address: u64,
    /// Non-zero length in bytes.
    pub length: u64,
}

/// Auditable execution facts accumulated in actual stage order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramExecutionFacts {
    /// Last stage proven complete, if any.
    pub last_completed_stage: Option<ProgramStage>,
    /// Stage being attempted when the facts were emitted.
    pub current_stage: ProgramStage,
    /// Whether a target-modifying DLL call was dispatched.
    pub side_effect_dispatched: bool,
    /// Ranges accepted by `WriteMem`, without implying `EndDownload` success.
    pub submitted_ranges: Vec<ProgramRangeFact>,
    /// Ranges confirmed only after a successful `EndDownload`.
    pub confirmed_ranges: Vec<ProgramRangeFact>,
    /// Conservative Flash modification conclusion.
    pub flash_modified: FlashModifiedState,
    /// Last target state observed before uncertainty.
    pub last_trusted_target_state: Option<TargetState>,
    /// Raw stable error code at the failing boundary.
    pub raw_error_code: Option<ErrorCode>,
    /// Side-effect failures are never safe to replay automatically.
    pub retry_safe: bool,
}

impl ProgramExecutionFacts {
    /// Starts a fact record before target preparation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_completed_stage: None,
            current_stage: ProgramStage::TargetPreparation,
            side_effect_dispatched: false,
            submitted_ranges: Vec::new(),
            confirmed_ranges: Vec::new(),
            flash_modified: FlashModifiedState::False,
            last_trusted_target_state: None,
            raw_error_code: None,
            retry_safe: false,
        }
    }

    /// Records one completed stage and advances to the next attempted stage.
    pub fn advance(&mut self, completed: ProgramStage, next: ProgramStage) {
        self.last_completed_stage = Some(completed);
        self.current_stage = next;
    }

    /// Marks a target-modifying call as dispatched.
    pub fn dispatch_side_effect(&mut self) {
        self.side_effect_dispatched = true;
        self.flash_modified = FlashModifiedState::Unknown;
    }

    /// Marks a reversible non-Flash Loader RAM write as dispatched.
    pub fn dispatch_non_flash_side_effect(&mut self) {
        self.side_effect_dispatched = true;
    }

    /// Records one range accepted by the DLL without claiming durable modification.
    pub fn submit(&mut self, address: u64, length: u64) {
        self.submitted_ranges
            .push(ProgramRangeFact { address, length });
    }

    /// Confirms all submitted ranges after `EndDownload` succeeds.
    pub fn confirm_submitted(&mut self) {
        self.confirmed_ranges.clone_from(&self.submitted_ranges);
        self.flash_modified = FlashModifiedState::True;
    }

    /// Converts a dispatched-side-effect failure to the stable uncertainty contract.
    ///
    /// # Panics
    ///
    /// Panics only if this statically serializable fact structure stops serializing as an object.
    #[must_use]
    pub fn uncertain_error(mut self, cause: &JlinkError) -> JlinkError {
        self.raw_error_code = Some(cause.code);
        let value = self.detail_value();
        let object = value
            .as_object()
            .expect("program facts serialize as an object");
        let mut error = JlinkError::new(
            ErrorCode::ExecutionUncertain,
            "Flash execution result is uncertain; do not replay the side effect",
            false,
        );
        for (key, value) in object {
            error = error.with_detail(key, value.clone());
        }
        error
            .with_detail("cause_code", json!(cause.code))
            .with_detail("cause_message", json!(cause.message))
            .with_detail("cause_details", json!(cause.details))
    }

    /// Attaches known stage facts to an error that does not represent unknown execution.
    ///
    /// # Panics
    ///
    /// Panics only if this statically serializable fact structure stops serializing as an object.
    #[must_use]
    pub fn known_error(mut self, mut cause: JlinkError) -> JlinkError {
        self.raw_error_code = Some(cause.code);
        let value = self.detail_value();
        for (key, value) in value
            .as_object()
            .expect("program facts serialize as an object")
        {
            cause = cause.with_detail(key, value.clone());
        }
        cause
    }

    fn detail_value(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("program facts are serializable");
        value
            .as_object_mut()
            .expect("program facts serialize as an object")
            .insert(
                "flash_modified".to_owned(),
                match self.flash_modified {
                    FlashModifiedState::False => json!(false),
                    FlashModifiedState::True => json!(true),
                    FlashModifiedState::Unknown => json!("unknown"),
                },
            );
        value
    }
}

impl Default for ProgramExecutionFacts {
    fn default() -> Self {
        Self::new()
    }
}

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
        /// Final Profile-selected Loader RAM used for the no-Flash preflight.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loader_ram: Option<MemoryRegion>,
    },
    /// Erase the whole device or one explicit checked range.
    Erase {
        /// `None` selects whole-chip erase.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        range: Option<FlashRange>,
        /// Explicit final target state policy.
        after: ProgramAfter,
        /// Final Profile-selected Loader RAM used for the no-Flash preflight.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        loader_ram: Option<MemoryRegion>,
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

#[cfg(test)]
mod execution_fact_tests {
    use super::{FlashModifiedState, ProgramExecutionFacts, ProgramStage};
    use crate::{ErrorCode, JlinkError, TargetState};
    use serde_json::json;

    #[test]
    fn end_download_failure_never_promotes_submitted_ranges_to_confirmed() {
        let mut facts = ProgramExecutionFacts::new();
        facts.advance(
            ProgramStage::TargetPreparation,
            ProgramStage::LoaderRamPreflight,
        );
        facts.advance(
            ProgramStage::LoaderRamPreflight,
            ProgramStage::BeginDownload,
        );
        facts.dispatch_side_effect();
        facts.advance(ProgramStage::BeginDownload, ProgramStage::SegmentCommit);
        facts.submit(0x1000, 16);
        facts.advance(ProgramStage::SegmentCommit, ProgramStage::EndDownload);
        facts.last_trusted_target_state = Some(TargetState::Halted);
        let error = facts.uncertain_error(&JlinkError::new(
            ErrorCode::TargetConnectFailed,
            "EndDownload=-1",
            false,
        ));
        let details = error.details.expect("execution facts");
        assert_eq!(details["last_completed_stage"], json!("segment_commit"));
        assert_eq!(details["current_stage"], json!("end_download"));
        assert_eq!(details["side_effect_dispatched"], json!(true));
        assert_eq!(details["submitted_ranges"][0]["address"], json!(0x1000));
        assert_eq!(details["confirmed_ranges"], json!([]));
        assert_eq!(details["flash_modified"], json!("unknown"));
        assert_eq!(details["retry_safe"], json!(false));
    }

    #[test]
    fn successful_end_download_confirms_exact_submitted_ranges() {
        let mut facts = ProgramExecutionFacts::new();
        facts.dispatch_side_effect();
        facts.submit(0x2000, 32);
        facts.confirm_submitted();
        assert_eq!(facts.confirmed_ranges, facts.submitted_ranges);
        assert_eq!(facts.flash_modified, FlashModifiedState::True);
    }

    #[test]
    fn every_failure_stage_preserves_current_and_last_completed_facts() {
        let stages = [
            ProgramStage::TargetPreparation,
            ProgramStage::LoaderRamPreflight,
            ProgramStage::BeginDownload,
            ProgramStage::SegmentCommit,
            ProgramStage::EndDownload,
            ProgramStage::VerifyPreparation,
            ProgramStage::RangeVerification,
            ProgramStage::FinalState,
        ];
        for (index, current) in stages.into_iter().enumerate() {
            let mut facts = ProgramExecutionFacts::new();
            facts.current_stage = current;
            facts.last_completed_stage = index.checked_sub(1).map(|previous| stages[previous]);
            if index >= 2 {
                facts.dispatch_side_effect();
            }
            let error = if facts.side_effect_dispatched {
                facts.uncertain_error(&JlinkError::new(
                    ErrorCode::TargetConnectFailed,
                    "injected stage failure",
                    false,
                ))
            } else {
                facts.known_error(JlinkError::new(
                    ErrorCode::TargetConnectFailed,
                    "injected stage failure",
                    false,
                ))
            };
            let details = error.details.expect("stage facts");
            assert_eq!(details["current_stage"], json!(current));
            assert_eq!(
                details["last_completed_stage"],
                index
                    .checked_sub(1)
                    .map_or(serde_json::Value::Null, |previous| json!(stages[previous]))
            );
            assert_eq!(details["retry_safe"], json!(false));
        }
    }
}
