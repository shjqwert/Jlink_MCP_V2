use std::{collections::BTreeMap, fmt::Write as _};

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value, json};
use sha2::{Digest, Sha256};

use crate::{AccessPlan, ErrorCode, FirmwareIdentityPlan, JlinkError};

const HSS_TIMESTAMP_BYTES: u32 = 4;
const HSS_CAPS_TIMESTAMP_FLAG: u32 = 2;
const HSS_SOURCE_TIMESTAMP_FREQUENCY_HZ: u32 = 1_000;
const HSS_SOURCE_TIMESTAMP_RESOLUTION_US: u32 = 1_000;

/// Minimum supported fixed capture duration in seconds.
pub const HSS_MIN_DURATION_S: u32 = 1;
/// Maximum supported fixed capture duration in seconds.
pub const HSS_MAX_DURATION_S: u32 = 300;
/// Minimum requested HSS sample frequency.
pub const HSS_MIN_RATE_HZ: u32 = 1;
/// Maximum requested HSS sample frequency proven by F0-A.
pub const HSS_MAX_RATE_HZ: u32 = 1_000;
/// Maximum number of Agent-supplied top-level selectors.
pub const HSS_MAX_TOP_LEVEL_SELECTORS: usize = 10;
/// Maximum expanded sample payload proven by the F0-A 10x32-bit route.
pub const HSS_MAX_EXPANDED_SAMPLE_BYTES: u32 = 40;

/// Default per-block flags verified by the F0-A J-Link 6.98a mainline.
pub const HSS_BLOCK_FLAGS_DEFAULT: u32 = 0;
/// Start flags for the supported J-Link 6.98a millisecond timestamp mode.
pub const HSS_START_FLAGS_698A_MAINLINE: i32 = 0;
/// Experimental microsecond timestamp flag observed in F0-A but not supported by V1.
pub const HSS_START_FLAG_TIMESTAMP_US_EXPERIMENTAL: i32 = 1;

/// Frozen byte layout for one sequence of J-Link HSS records.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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

/// Determines when the MCP caller receives the result of a fixed capture.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssReturnWhen {
    /// Return after the Worker has started and owns the capture.
    Started,
    /// Wait for the same fixed capture to reach its terminal result.
    Completed,
}

/// Direction retained by one declarative threshold-crossing rule.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssCrossingDirection {
    /// Match only upward crossings.
    Up,
    /// Match only downward crossings.
    Down,
    /// Match either crossing direction.
    Either,
}

/// One normalized start-time rule retained with the raw capture request.
///
/// Evaluation remains a query concern; retaining the typed rule here prevents
/// capture-key idempotency from silently ignoring different rule sets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HssThresholdRule {
    /// Match an absolute adjacent-value delta.
    AbsDeltaGte {
        /// Stable caller-provided rule identity.
        id: String,
        /// Exact leaf path or array wildcard path evaluated later by the query engine.
        path: String,
        /// Closed-contract typed threshold value.
        value: Value,
    },
    /// Match values outside an inclusive numeric interval.
    Outside {
        /// Stable caller-provided rule identity.
        id: String,
        /// Exact leaf path or array wildcard path evaluated later by the query engine.
        path: String,
        /// Inclusive lower bound.
        min: Number,
        /// Inclusive upper bound.
        max: Number,
    },
    /// Match equality with one closed-contract typed value.
    Equals {
        /// Stable caller-provided rule identity.
        id: String,
        /// Exact leaf path or array wildcard path evaluated later by the query engine.
        path: String,
        /// Complete comparison value.
        value: Value,
    },
    /// Match one directional threshold crossing.
    Crosses {
        /// Stable caller-provided rule identity.
        id: String,
        /// Exact leaf path or array wildcard path evaluated later by the query engine.
        path: String,
        /// Complete comparison value.
        value: Value,
        /// Accepted crossing direction.
        direction: HssCrossingDirection,
    },
}

impl HssThresholdRule {
    /// Revalidates the strict rule shape retained across local IPC.
    ///
    /// Path-to-series resolution and rule evaluation are deliberately deferred
    /// to the single query implementation, but identity and public value shape
    /// must already be stable before the request fingerprint is computed.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] for blank identities or paths and for
    /// values outside the public boolean/number/object/array contract.
    pub fn validate(&self) -> Result<(), JlinkError> {
        let (id, path, value) = match self {
            Self::AbsDeltaGte { id, path, value }
            | Self::Equals { id, path, value }
            | Self::Crosses {
                id, path, value, ..
            } => (id, path, Some(value)),
            Self::Outside { id, path, .. } => (id, path, None),
        };
        if id.trim().is_empty() {
            return Err(hss_value_invalid("HSS 规则 id 不能为空或仅包含空白"));
        }
        if path.trim().is_empty() {
            return Err(hss_value_invalid("HSS 规则 path 不能为空或仅包含空白")
                .with_detail("rule_id", json!(id)));
        }
        if value.is_some_and(|item| matches!(item, Value::Null | Value::String(_))) {
            return Err(hss_value_invalid(
                "HSS 规则 value 必须是 boolean、number、object 或 array",
            )
            .with_detail("rule_id", json!(id)));
        }
        Ok(())
    }

    /// Returns the stable rule identity used for deterministic normalization.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::AbsDeltaGte { id, .. }
            | Self::Outside { id, .. }
            | Self::Equals { id, .. }
            | Self::Crosses { id, .. } => id,
        }
    }
}

/// One top-level DWARF selector placed at a fixed offset in every HSS sample.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssVariablePlan {
    plan: AccessPlan,
    sample_offset: u32,
}

impl HssVariablePlan {
    /// Returns the immutable DWARF access plan.
    #[must_use]
    pub const fn access_plan(&self) -> &AccessPlan {
        &self.plan
    }

    /// Returns the byte offset after the source timestamp in each sample.
    #[must_use]
    pub const fn sample_offset(&self) -> u32 {
        self.sample_offset
    }
}

/// Immutable, normalized HSS request built before any DLL or target action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssStartPlan {
    capture_key: String,
    duration_s: u32,
    rate_hz: u32,
    return_when: HssReturnWhen,
    variables: Vec<HssVariablePlan>,
    rules: Vec<HssThresholdRule>,
    firmware: FirmwareIdentityPlan,
    frame_layout: HssFrameLayout,
    request_fingerprint: String,
}

impl HssStartPlan {
    /// Validates and normalizes one fixed-duration HSS request.
    ///
    /// # Errors
    ///
    /// Returns a stable value, type, identity, or HSS capability error when the
    /// key, bounds, plans, ELF binding, 32-bit addresses, or expanded frame are invalid.
    pub fn new(
        capture_key: impl Into<String>,
        duration_s: u32,
        rate_hz: u32,
        return_when: HssReturnWhen,
        plans: Vec<AccessPlan>,
        rules: Vec<HssThresholdRule>,
        firmware: FirmwareIdentityPlan,
    ) -> Result<Self, JlinkError> {
        let capture_key = capture_key.into();
        if capture_key.trim().is_empty() {
            return Err(hss_value_invalid("capture_key 不能为空或仅包含空白"));
        }
        if !(HSS_MIN_DURATION_S..=HSS_MAX_DURATION_S).contains(&duration_s) {
            return Err(hss_value_invalid("duration_s 必须为 1..300"));
        }
        if !(HSS_MIN_RATE_HZ..=HSS_MAX_RATE_HZ).contains(&rate_hz) {
            return Err(hss_value_invalid("rate_hz 必须为 1..1000"));
        }
        if plans.is_empty() || plans.len() > HSS_MAX_TOP_LEVEL_SELECTORS {
            return Err(hss_value_invalid("HSS 顶层变量选择项必须为 1..10 个"));
        }
        firmware.validate()?;

        let mut sample_offset = 0_u32;
        let mut variables = Vec::with_capacity(plans.len());
        let mut byte_counts = Vec::with_capacity(plans.len());
        for (index, plan) in plans.into_iter().enumerate() {
            plan.validate_for_execution()?;
            if plan.elf_sha256() != firmware.elf_sha256() {
                return Err(hss_value_invalid("HSS 变量计划与固件身份不是同一 ELF")
                    .with_detail("selector_index", json!(index)));
            }
            let byte_count = u32::try_from(plan.byte_size()).map_err(|_| {
                hss_unsupported("HSS 变量长度超出冻结 DLL 的 32-bit block ABI")
                    .with_detail("selector_index", json!(index))
            })?;
            let address = u32::try_from(plan.address()).map_err(|_| {
                hss_unsupported("HSS 变量地址超出 Cortex-M 32-bit 地址空间")
                    .with_detail("selector_index", json!(index))
            })?;
            address.checked_add(byte_count).ok_or_else(|| {
                hss_unsupported("HSS 变量范围超出 Cortex-M 地址空间")
                    .with_detail("selector_index", json!(index))
            })?;
            let next_offset = sample_offset.checked_add(byte_count).ok_or_else(|| {
                hss_unsupported("HSS 展开采样帧长度溢出")
                    .with_detail("selector_index", json!(index))
            })?;
            if next_offset > HSS_MAX_EXPANDED_SAMPLE_BYTES {
                return Err(hss_unsupported(format!(
                    "HSS 展开采样载荷 {next_offset} 字节超过已验证上限 {HSS_MAX_EXPANDED_SAMPLE_BYTES} 字节"
                ))
                .with_detail("selector_index", json!(index)));
            }
            variables.push(HssVariablePlan {
                plan,
                sample_offset,
            });
            byte_counts.push(byte_count);
            sample_offset = next_offset;
        }
        let frame_layout = HssFrameLayout::new(&byte_counts)?;
        let rules = normalize_rules(rules)?;
        let request_fingerprint = request_fingerprint(
            duration_s,
            rate_hz,
            return_when,
            &variables,
            &rules,
            &firmware,
        )?;
        Ok(Self {
            capture_key,
            duration_s,
            rate_hz,
            return_when,
            variables,
            rules,
            firmware,
            frame_layout,
            request_fingerprint,
        })
    }

    /// Revalidates a plan after local IPC transport.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::new`] or a value error if serialized
    /// derived fields do not match the normalized request.
    pub fn validate(&self) -> Result<(), JlinkError> {
        let rebuilt = Self::new(
            self.capture_key.clone(),
            self.duration_s,
            self.rate_hz,
            self.return_when,
            self.variables
                .iter()
                .map(|variable| variable.plan.clone())
                .collect(),
            self.rules.clone(),
            self.firmware.clone(),
        )?;
        if rebuilt == *self {
            Ok(())
        } else {
            Err(hss_value_invalid("HSS 启动计划的派生字段不一致"))
        }
    }

    /// Returns the Agent-provided idempotency key.
    #[must_use]
    pub fn capture_key(&self) -> &str {
        &self.capture_key
    }

    /// Returns the fixed capture duration in seconds.
    #[must_use]
    pub const fn duration_s(&self) -> u32 {
        self.duration_s
    }

    /// Returns the requested target sample frequency.
    #[must_use]
    pub const fn rate_hz(&self) -> u32 {
        self.rate_hz
    }

    /// Returns the rounded DLL period in microseconds used by F0-A.
    #[must_use]
    pub const fn period_us(&self) -> u32 {
        (1_000_000 + self.rate_hz / 2) / self.rate_hz
    }

    /// Returns the caller wait policy retained in the normalized request.
    #[must_use]
    pub const fn return_when(&self) -> HssReturnWhen {
        self.return_when
    }

    /// Returns top-level selectors in submitted order with fixed sample offsets.
    #[must_use]
    pub fn variables(&self) -> &[HssVariablePlan] {
        &self.variables
    }

    /// Returns start-time rules in deterministic rule-id order.
    #[must_use]
    pub fn rules(&self) -> &[HssThresholdRule] {
        &self.rules
    }

    /// Returns the symbol ELF identity that must be verified before HSS starts.
    #[must_use]
    pub const fn firmware(&self) -> &FirmwareIdentityPlan {
        &self.firmware
    }

    /// Returns the frozen raw-frame layout.
    #[must_use]
    pub const fn frame_layout(&self) -> HssFrameLayout {
        self.frame_layout
    }

    /// Returns the stable normalized request fingerprint.
    #[must_use]
    pub fn request_fingerprint(&self) -> &str {
        &self.request_fingerprint
    }
}

/// Frozen 6.98a HSS capability facts used by start preflight.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssCapabilities {
    max_blocks: u32,
    max_frequency_hz: u32,
    source_timestamp_frequency_hz: u32,
    source_timestamp_resolution_us: u32,
    source_timestamp_monotonic: bool,
}

impl HssCapabilities {
    /// Interprets one exact F0-A-compatible `GetCaps` result.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::HssUnsupported`] when limits are zero, capability
    /// flags differ from the frozen 6.98a value, or reserved words are non-zero.
    pub fn frozen_698a(
        max_blocks: u32,
        max_frequency_hz: u32,
        flags: u32,
        reserved: [u32; 5],
    ) -> Result<Self, JlinkError> {
        if max_blocks == 0 || max_frequency_hz == 0 {
            return Err(hss_unsupported("J-Link HSS 能力上限不能为 0"));
        }
        if flags != HSS_CAPS_TIMESTAMP_FLAG || reserved != [0; 5] {
            return Err(
                hss_unsupported("J-Link HSS 能力标志或保留字段与冻结 6.98a 不符")
                    .with_detail("flags", json!(flags))
                    .with_detail("reserved", json!(reserved)),
            );
        }
        Ok(Self {
            max_blocks,
            max_frequency_hz,
            source_timestamp_frequency_hz: HSS_SOURCE_TIMESTAMP_FREQUENCY_HZ,
            source_timestamp_resolution_us: HSS_SOURCE_TIMESTAMP_RESOLUTION_US,
            source_timestamp_monotonic: true,
        })
    }

    /// Validates one normalized request against observed probe/DLL limits.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::HssUnsupported`] when block or frequency limits are insufficient.
    pub fn validate_start(self, plan: &HssStartPlan) -> Result<(), JlinkError> {
        plan.validate()?;
        let requested_blocks = u32::try_from(plan.variables.len())
            .map_err(|_| hss_unsupported("HSS 顶层变量数量无法表示"))?;
        if requested_blocks > self.max_blocks {
            return Err(hss_unsupported("HSS 顶层变量数量超过探针能力")
                .with_detail("requested_blocks", json!(requested_blocks))
                .with_detail("max_blocks", json!(self.max_blocks)));
        }
        if plan.rate_hz > self.max_frequency_hz {
            return Err(hss_unsupported("HSS 请求频率超过探针能力")
                .with_detail("requested_rate_hz", json!(plan.rate_hz))
                .with_detail("max_frequency_hz", json!(self.max_frequency_hz)));
        }
        Ok(())
    }

    /// Returns the maximum accepted top-level block count.
    #[must_use]
    pub const fn max_blocks(self) -> u32 {
        self.max_blocks
    }

    /// Returns the maximum requested sample frequency.
    #[must_use]
    pub const fn max_frequency_hz(self) -> u32 {
        self.max_frequency_hz
    }

    /// Returns the frozen source timestamp frequency.
    #[must_use]
    pub const fn source_timestamp_frequency_hz(self) -> u32 {
        self.source_timestamp_frequency_hz
    }

    /// Returns the frozen source timestamp resolution in microseconds.
    #[must_use]
    pub const fn source_timestamp_resolution_us(self) -> u32 {
        self.source_timestamp_resolution_us
    }

    /// Returns whether F0-A confirmed monotonically ordered source timestamps.
    #[must_use]
    pub const fn source_timestamp_monotonic(self) -> bool {
        self.source_timestamp_monotonic
    }
}

/// Stable identity reserved for one normalized request under one capture key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HssCaptureReservation {
    capture_id: String,
    request_fingerprint: String,
}

impl HssCaptureReservation {
    /// Returns the deterministic public capture identity.
    #[must_use]
    pub fn capture_id(&self) -> &str {
        &self.capture_id
    }

    /// Returns the normalized request fingerprint retained for conflict evidence.
    #[must_use]
    pub fn request_fingerprint(&self) -> &str {
        &self.request_fingerprint
    }
}

/// Whether a key reservation created a capture or recovered the existing identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HssReservationOutcome {
    /// The key was unused and now owns this identity.
    Created(HssCaptureReservation),
    /// The same normalized request already owns this key.
    Existing(HssCaptureReservation),
}

/// Worker-owned capture-key index with pure deterministic reservation rules.
#[derive(Debug, Default)]
pub struct HssStartRegistry {
    by_key: BTreeMap<String, HssCaptureReservation>,
}

impl HssStartRegistry {
    /// Creates an empty Worker-owned key index.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            by_key: BTreeMap::new(),
        }
    }

    /// Reserves or recovers one capture identity without starting hardware.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::CaptureKeyConflict`] when the same key already names
    /// a different normalized request, or propagates start-plan validation errors.
    pub fn reserve(
        &mut self,
        probe_identity: &str,
        plan: &HssStartPlan,
    ) -> Result<HssReservationOutcome, JlinkError> {
        plan.validate()?;
        if let Some(existing) = self.by_key.get(plan.capture_key()) {
            if existing.request_fingerprint == plan.request_fingerprint {
                return Ok(HssReservationOutcome::Existing(existing.clone()));
            }
            return Err(JlinkError::new(
                ErrorCode::CaptureKeyConflict,
                "capture_key 已绑定到不同的规范化 HSS 请求",
                false,
            )
            .with_detail("capture_id", json!(existing.capture_id))
            .with_detail(
                "original_request_fingerprint",
                json!(existing.request_fingerprint),
            )
            .with_detail(
                "requested_request_fingerprint",
                json!(plan.request_fingerprint),
            ));
        }
        let reservation = HssCaptureReservation {
            capture_id: capture_id(probe_identity, plan),
            request_fingerprint: plan.request_fingerprint.clone(),
        };
        self.by_key
            .insert(plan.capture_key.clone(), reservation.clone());
        Ok(HssReservationOutcome::Created(reservation))
    }
}

#[derive(Serialize)]
struct FingerprintInput<'a> {
    duration_s: u32,
    rate_hz: u32,
    return_when: HssReturnWhen,
    variables: &'a [HssVariablePlan],
    rules: &'a [HssThresholdRule],
    firmware: &'a FirmwareIdentityPlan,
}

fn request_fingerprint(
    duration_s: u32,
    rate_hz: u32,
    return_when: HssReturnWhen,
    variables: &[HssVariablePlan],
    rules: &[HssThresholdRule],
    firmware: &FirmwareIdentityPlan,
) -> Result<String, JlinkError> {
    let bytes = serde_json::to_vec(&FingerprintInput {
        duration_s,
        rate_hz,
        return_when,
        variables,
        rules,
        firmware,
    })
    .map_err(|error| hss_value_invalid(format!("HSS 请求无法规范化：{error}")))?;
    Ok(sha256(&bytes))
}

fn normalize_rules(mut rules: Vec<HssThresholdRule>) -> Result<Vec<HssThresholdRule>, JlinkError> {
    for rule in &rules {
        rule.validate()?;
    }
    rules.sort_by(|left, right| left.id().cmp(right.id()));
    if let Some(duplicate) = rules.windows(2).find(|pair| pair[0].id() == pair[1].id()) {
        return Err(hss_value_invalid("HSS 规则 id 必须唯一")
            .with_detail("rule_id", json!(duplicate[0].id())));
    }
    Ok(rules)
}

fn capture_id(probe_identity: &str, plan: &HssStartPlan) -> String {
    let mut hasher = Sha256::new();
    hasher.update(probe_identity.as_bytes());
    hasher.update([0]);
    hasher.update(plan.capture_key.as_bytes());
    hasher.update([0]);
    hasher.update(plan.request_fingerprint.as_bytes());
    let digest = encode_sha256(&hasher.finalize());
    format!("cap_{}", digest.chars().take(24).collect::<String>())
}

fn sha256(bytes: &[u8]) -> String {
    encode_sha256(&Sha256::digest(bytes))
}

fn encode_sha256(digest: &[u8]) -> String {
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn frame_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::FrameInvalid, message, false)
}

fn hss_value_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ValueInvalid, message, false)
}

fn hss_unsupported(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::HssUnsupported, message, false)
}
