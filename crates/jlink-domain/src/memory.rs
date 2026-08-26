use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AccessPlan, ErrorCode, FirmwareIdentityPlan, JlinkError};

/// Maximum byte count accepted by one public raw-memory request.
pub const MAX_RAW_MEMORY_BYTES: u64 = 4_096;

const CORTEX_M_ADDRESS_SPACE_END: u64 = 1_u64 << 32;
const SAFE_MERGE_ALIGNMENT: u64 = 4;

/// Target address-space classification used by ordinary memory operations.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRegionKind {
    /// Device Flash reported by the selected J-Link device database entry.
    Flash,
    /// Device RAM reported by the selected J-Link device database entry.
    Ram,
    /// Other explicit memory-mapped target addresses.
    Mmio,
}

/// One non-empty known device region in the 32-bit Cortex-M address space.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRegion {
    address: u64,
    length: u64,
    kind: MemoryRegionKind,
}

impl MemoryRegion {
    /// Creates one checked half-open device region.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::AddressOutOfRange`] for an empty, overflowing, or
    /// non-Cortex-M range.
    pub fn new(address: u64, length: u64, kind: MemoryRegionKind) -> Result<Self, JlinkError> {
        checked_end(address, length)?;
        Ok(Self {
            address,
            length,
            kind,
        })
    }

    /// Returns the first byte address in this region.
    #[must_use]
    pub const fn address(self) -> u64 {
        self.address
    }

    /// Returns the region length in bytes.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }

    /// Returns the region classification.
    #[must_use]
    pub const fn kind(self) -> MemoryRegionKind {
        self.kind
    }

    fn end(self) -> u64 {
        self.address + self.length
    }

    fn contains(self, range: MemoryRange) -> bool {
        self.address <= range.address && range.end() <= self.end()
    }

    fn overlaps(self, range: MemoryRange) -> bool {
        self.address < range.end() && range.address < self.end()
    }
}

/// One exact non-empty target memory range.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRange {
    address: u64,
    length: u64,
}

impl MemoryRange {
    /// Creates one checked 32-bit target range.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::AddressOutOfRange`] for an empty, overflowing, or
    /// non-Cortex-M range.
    pub fn new(address: u64, length: u64) -> Result<Self, JlinkError> {
        checked_end(address, length)?;
        Ok(Self { address, length })
    }

    /// Creates a range subject to the public 1–4096 byte transfer limit.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] when `length` exceeds 4096 bytes,
    /// plus the address errors documented by [`Self::new`].
    pub fn raw(address: u64, length: u64) -> Result<Self, JlinkError> {
        if !(1..=MAX_RAW_MEMORY_BYTES).contains(&length) {
            return Err(JlinkError::new(
                ErrorCode::ValueInvalid,
                "原始内存单次长度必须在 1 到 4096 字节之间",
                false,
            ));
        }
        Self::new(address, length)
    }

    /// Returns the first byte address.
    #[must_use]
    pub const fn address(self) -> u64 {
        self.address
    }

    /// Returns the exact byte count.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }

    /// Returns the exclusive end address.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.address + self.length
    }

    fn validate(self) -> Result<(), JlinkError> {
        checked_end(self.address, self.length).map(|_| ())
    }
}

/// Device regions required to classify Flash, RAM, and residual MMIO access.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceMemoryMap {
    regions: Vec<MemoryRegion>,
}

impl DeviceMemoryMap {
    /// Validates a non-overlapping set of device-database Flash and RAM regions.
    ///
    /// Addresses outside those known regions remain explicit MMIO candidates;
    /// their actual accessibility is decided by the serialized target call.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::AddressOutOfRange`] for invalid, overlapping, or
    /// pre-classified MMIO regions.
    pub fn new(mut regions: Vec<MemoryRegion>) -> Result<Self, JlinkError> {
        for region in &regions {
            MemoryRegion::new(region.address, region.length, region.kind)?;
            if region.kind == MemoryRegionKind::Mmio {
                return Err(address_error(
                    "器件数据库区域只能声明 Flash 或 RAM；MMIO 由显式地址访问决定",
                ));
            }
        }
        regions.sort_by_key(|region| region.address);
        if regions
            .windows(2)
            .any(|pair| pair[0].end() > pair[1].address)
        {
            return Err(address_error("器件数据库的 Flash/RAM 区域发生重叠"));
        }
        Ok(Self { regions })
    }

    /// Classifies one complete range without allowing known-region crossing.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::AddressOutOfRange`] when a range overlaps but is not
    /// wholly contained by exactly one known Flash or RAM region.
    pub fn classify(&self, range: MemoryRange) -> Result<MemoryRegionKind, JlinkError> {
        range.validate()?;
        if let Some(region) = self.regions.iter().find(|region| region.contains(range)) {
            return Ok(region.kind);
        }
        if self.regions.iter().any(|region| region.overlaps(range)) {
            return Err(
                address_error("内存访问跨越已知 Flash/RAM 区域边界，必须拆成独立请求")
                    .with_detail("address", json!(format!("0x{:X}", range.address)))
                    .with_detail("length", json!(range.length)),
            );
        }
        Ok(MemoryRegionKind::Mmio)
    }

    /// Rejects ordinary writes to known Flash before any target side effect.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::AddressOutOfRange`] with the required replacement
    /// tool when the range is Flash.
    pub fn ensure_ordinary_write(
        &self,
        range: MemoryRange,
    ) -> Result<MemoryRegionKind, JlinkError> {
        let kind = self.classify(range)?;
        if kind == MemoryRegionKind::Flash {
            return Err(
                address_error("普通内存写入不能修改 Flash，请使用 jlink_program")
                    .with_detail("region", json!("flash"))
                    .with_detail("use_tool", json!("jlink_program")),
            );
        }
        Ok(kind)
    }
}

/// Explicit ordinary-write verification policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteVerify {
    /// Do not perform an additional read after the write.
    #[default]
    None,
    /// Read the exact range after writing and compare every byte.
    Readback,
}

/// Typed debug operation executed by the unique Worker gateway.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum DebugRequest {
    /// Read one exact raw target range.
    ReadMemory {
        /// Validated public raw-memory range.
        range: MemoryRange,
    },
    /// Write one exact raw RAM or MMIO range.
    WriteMemory {
        /// First target byte to modify.
        address: u64,
        /// Complete bytes in ascending target-address order.
        data: Vec<u8>,
        /// Optional post-write verification policy.
        verify: WriteVerify,
    },
    /// Read and decode one immutable DWARF access plan.
    ReadVariable {
        /// Immutable selector, address, and typed layout.
        plan: AccessPlan,
        /// Symbol ELF identity that must match target Flash first.
        firmware: FirmwareIdentityPlan,
    },
    /// Prevalidate, encode, and write one immutable DWARF access plan.
    WriteVariable {
        /// Immutable selector, address, and typed layout.
        plan: AccessPlan,
        /// Symbol ELF identity that must match target Flash first.
        firmware: FirmwareIdentityPlan,
        /// Complete requested V1 typed value.
        value: Value,
        /// Optional post-write verification policy.
        verify: WriteVerify,
    },
}

impl DebugRequest {
    /// Revalidates every deserialized range and immutable access plan.
    ///
    /// # Errors
    ///
    /// Returns stable value, address, or type errors before target access.
    pub fn validate(&self) -> Result<(), JlinkError> {
        match self {
            Self::ReadMemory { range } => MemoryRange::raw(range.address, range.length).map(|_| ()),
            Self::WriteMemory { address, data, .. } => {
                let length = u64::try_from(data.len()).map_err(|_| {
                    JlinkError::new(ErrorCode::ValueInvalid, "内存写入长度无法表示", false)
                })?;
                MemoryRange::raw(*address, length).map(|_| ())
            }
            Self::ReadVariable { plan, firmware } | Self::WriteVariable { plan, firmware, .. } => {
                plan.validate_for_execution()?;
                firmware.validate()?;
                if plan.elf_sha256() != firmware.elf_sha256() {
                    return Err(JlinkError::new(
                        ErrorCode::FirmwareIdentityUnknown,
                        "AccessPlan 与固件身份计划不属于同一 ELF",
                        false,
                    ));
                }
                MemoryRange::new(plan.address(), plan.byte_size()).map(|_| ())
            }
        }
    }

    /// Returns whether the operation may change target memory.
    #[must_use]
    pub const fn is_write(&self) -> bool {
        matches!(self, Self::WriteMemory { .. } | Self::WriteVariable { .. })
    }

    /// Returns whether the request depends on a symbol ELF identity.
    #[must_use]
    pub const fn firmware(&self) -> Option<&FirmwareIdentityPlan> {
        match self {
            Self::ReadVariable { firmware, .. } | Self::WriteVariable { firmware, .. } => {
                Some(firmware)
            }
            Self::ReadMemory { .. } | Self::WriteMemory { .. } => None,
        }
    }
}

/// Typed result returned by a debug Worker command.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum DebugResult {
    /// Exact raw bytes in ascending address order.
    Memory {
        /// Complete bytes without truncation.
        data: Vec<u8>,
    },
    /// One losslessly decoded V1 typed value.
    Variable {
        /// Decoded value without request or type metadata.
        value: Value,
    },
    /// A complete ordinary write, including requested readback when present.
    Written,
}

/// Origin retained by the conservative safe-read merge planner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryReadOrigin {
    /// An explicit raw-memory request.
    Raw,
    /// One statically resolved DWARF variable range.
    StaticVariable,
}

/// One candidate read before safe adjacent-range merging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryReadPlan {
    range: MemoryRange,
    region: MemoryRegionKind,
    origin: MemoryReadOrigin,
    is_volatile: bool,
}

impl MemoryReadPlan {
    /// Creates one already classified read candidate.
    #[must_use]
    pub const fn new(
        range: MemoryRange,
        region: MemoryRegionKind,
        origin: MemoryReadOrigin,
        is_volatile: bool,
    ) -> Self {
        Self {
            range,
            region,
            origin,
            is_volatile,
        }
    }

    fn can_merge(self) -> bool {
        let side_effect_free = match self.origin {
            MemoryReadOrigin::Raw => self.region == MemoryRegionKind::Ram,
            MemoryReadOrigin::StaticVariable => self.region != MemoryRegionKind::Mmio,
        };
        side_effect_free
            && !self.is_volatile
            && self.range.address.is_multiple_of(SAFE_MERGE_ALIGNMENT)
            && self.range.length.is_multiple_of(SAFE_MERGE_ALIGNMENT)
    }
}

/// One original read's location inside a merged target call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryReadPart {
    /// Original request index.
    pub index: usize,
    /// Byte offset from the merged read start.
    pub offset: u64,
    /// Exact original byte count.
    pub length: u64,
}

/// One target read and the original ranges decoded from its returned bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergedMemoryRead {
    /// Complete target range read once.
    pub range: MemoryRange,
    /// Stable mapping back to original request order.
    pub parts: Vec<MemoryReadPart>,
}

/// Conservatively merges consecutive adjacent safe reads without reordering.
///
/// MMIO, volatile, cross-region, non-ascending, non-adjacent, or non-4-byte-
/// aligned reads always remain separate target calls.
#[must_use]
pub fn merge_safe_memory_reads(reads: &[MemoryReadPlan]) -> Vec<MergedMemoryRead> {
    let mut merged: Vec<MergedMemoryRead> = Vec::with_capacity(reads.len());
    for (index, read) in reads.iter().copied().enumerate() {
        let can_extend = merged.last().is_some_and(|current| {
            let first = reads[current.parts[0].index];
            first.can_merge()
                && read.can_merge()
                && first.region == read.region
                && current.range.end() == read.range.address
        });
        if can_extend && let Some(current) = merged.last_mut() {
            let offset = current.range.length;
            current.range.length += read.range.length;
            current.parts.push(MemoryReadPart {
                index,
                offset,
                length: read.range.length,
            });
        } else {
            merged.push(MergedMemoryRead {
                range: read.range,
                parts: vec![MemoryReadPart {
                    index,
                    offset: 0,
                    length: read.range.length,
                }],
            });
        }
    }
    merged
}

/// Checks the exact count returned by one ordinary write call.
///
/// # Errors
///
/// Returns [`ErrorCode::ExecutionUncertain`] with requested and actual lengths
/// for every short or failed call; a partial target side effect cannot be retried
/// as though nothing happened.
pub fn validate_write_count(address: u64, requested: usize, actual: i32) -> Result<(), JlinkError> {
    let requested_i32 = i32::try_from(requested).map_err(|_| {
        JlinkError::new(
            ErrorCode::ValueInvalid,
            "内存写入长度超出 DLL 可表示范围",
            false,
        )
    })?;
    if actual == requested_i32 {
        return Ok(());
    }
    let mut error = JlinkError::new(
        ErrorCode::ExecutionUncertain,
        "J-Link 未报告完整内存写入，目标可能只修改了部分字节",
        false,
    )
    .with_detail("address", json!(format!("0x{address:X}")))
    .with_detail("requested_length", json!(requested));
    if actual >= 0 {
        error = error.with_detail("actual_length", json!(actual));
    } else {
        error = error.with_detail("dll_result", json!(actual));
    }
    Err(error)
}

/// Compares an explicitly requested ordinary-write readback.
///
/// # Errors
///
/// Returns [`ErrorCode::VerifyFailed`] with only the first differing address and
/// byte values, or the requested/actual lengths when the readback is incomplete.
pub fn verify_memory_readback(
    address: u64,
    expected: &[u8],
    actual: &[u8],
) -> Result<(), JlinkError> {
    if expected.len() != actual.len() {
        return Err(
            JlinkError::new(ErrorCode::VerifyFailed, "内存写入读回长度不完整", false)
                .with_detail("requested_length", json!(expected.len()))
                .with_detail("actual_length", json!(actual.len())),
        );
    }
    let Some((offset, (expected_byte, actual_byte))) = expected
        .iter()
        .zip(actual)
        .enumerate()
        .find(|(_, (expected, actual))| expected != actual)
    else {
        return Ok(());
    };
    let offset = u64::try_from(offset)
        .map_err(|_| JlinkError::new(ErrorCode::ValueInvalid, "内存读回差异偏移无法表示", false))?;
    let first_address = address
        .checked_add(offset)
        .ok_or_else(|| address_error("内存读回差异地址溢出"))?;
    Err(
        JlinkError::new(ErrorCode::VerifyFailed, "内存写入后的显式读回不一致", false)
            .with_detail("first_address", json!(format!("0x{first_address:X}")))
            .with_detail("expected", json!(format!("{expected_byte:02x}")))
            .with_detail("actual", json!(format!("{actual_byte:02x}"))),
    )
}

fn checked_end(address: u64, length: u64) -> Result<u64, JlinkError> {
    if length == 0 {
        return Err(address_error("内存访问长度必须大于 0"));
    }
    let end = address
        .checked_add(length)
        .ok_or_else(|| address_error("内存访问地址加长度溢出"))?;
    if end > CORTEX_M_ADDRESS_SPACE_END {
        return Err(address_error("内存访问超出 32 位 Cortex-M 地址空间"));
    }
    Ok(end)
}

fn address_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::AddressOutOfRange, message, false)
}
