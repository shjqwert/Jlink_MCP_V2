use std::{cmp::Ordering, collections::BTreeMap, fmt::Write as _};

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    AccessPlan, ErrorCode, FirmwareIdentityPlan, JlinkError, SelectorStep, TargetConnectionSpec,
    VariableSelector,
};

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

/// Observable lifecycle states shared by the Worker and MCP process.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssRunState {
    /// Hardware start is being prepared but has not completed.
    Starting,
    /// The DLL owns an active fixed-duration capture.
    Running,
    /// Internal Stop completed and the Worker is draining the DLL tail.
    Stopping,
    /// The immutable capture completed normally.
    Completed,
    /// Capture cleanup completed after an execution failure.
    Failed,
    /// A prior process ended without completing the capture.
    Aborted,
}

/// Data-integrity assessment kept independent from capture lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssDataIntegrity {
    /// Available evidence proves the persisted capture is complete.
    Complete,
    /// Quality evidence proves that retained data is incomplete or impaired.
    Degraded,
    /// Available evidence cannot prove either completeness or degradation.
    Unknown,
}

/// Observable recovery facts retained when a capture cannot follow the normal path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HssRecoveryNotification {
    /// Internal Stop completed after an acquisition failure.
    StopCompletedAfterFailure,
    /// Valid bytes were retained instead of being discarded with a failure.
    PartialDataRetained {
        /// Complete records retained at the failure boundary.
        complete_records: u64,
        /// Trailing bytes that cannot yet form a complete record.
        trailing_bytes: u64,
    },
    /// A later startup scan classified an interrupted capture as aborted.
    AbortedCaptureRecovered,
}

/// Pure lifecycle and integrity state machine shared by acquisition and recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HssCaptureState {
    lifecycle: HssRunState,
    integrity: HssDataIntegrity,
    failure_code: Option<ErrorCode>,
    partial_available: bool,
    reason: Option<String>,
    recoverable: Option<bool>,
    recovery_notifications: Vec<HssRecoveryNotification>,
}

impl HssCaptureState {
    /// Creates the only valid initial state.
    #[must_use]
    pub const fn starting() -> Self {
        Self {
            lifecycle: HssRunState::Starting,
            integrity: HssDataIntegrity::Unknown,
            failure_code: None,
            partial_available: false,
            reason: None,
            recoverable: None,
            recovery_notifications: Vec::new(),
        }
    }

    /// Moves a successful hardware Start into `running`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidStateTransition`] unless the state is `starting`.
    pub fn mark_running(&mut self) -> Result<(), JlinkError> {
        self.require(HssRunState::Starting, "HSS 只有 starting 可以进入 running")?;
        self.lifecycle = HssRunState::Running;
        Ok(())
    }

    /// Moves an active capture into internal Stop/tail drain.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidStateTransition`] unless the state is `running`.
    pub fn mark_stopping(&mut self) -> Result<(), JlinkError> {
        self.require(HssRunState::Running, "HSS 只有 running 可以进入 stopping")?;
        self.lifecycle = HssRunState::Stopping;
        Ok(())
    }

    /// Completes tail handling while retaining an independent integrity result.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidStateTransition`] unless the state is `stopping`.
    pub fn mark_completed(&mut self, integrity: HssDataIntegrity) -> Result<(), JlinkError> {
        self.require(
            HssRunState::Stopping,
            "HSS 只有 stopping 可以进入 completed",
        )?;
        self.lifecycle = HssRunState::Completed;
        self.integrity = integrity;
        Ok(())
    }

    /// Records a controlled terminal failure and any retained partial data.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidStateTransition`] for an existing terminal state.
    pub fn mark_failed(
        &mut self,
        code: ErrorCode,
        partial_available: bool,
        notifications: Vec<HssRecoveryNotification>,
    ) -> Result<(), JlinkError> {
        if matches!(
            self.lifecycle,
            HssRunState::Completed | HssRunState::Failed | HssRunState::Aborted
        ) {
            return Err(invalid_hss_transition("HSS 终态不能再次转换为 failed"));
        }
        self.lifecycle = HssRunState::Failed;
        self.integrity = HssDataIntegrity::Unknown;
        self.failure_code = Some(code);
        self.partial_available = partial_available;
        self.recovery_notifications = notifications;
        Ok(())
    }

    /// Classifies an interrupted or recovered partial capture as `aborted`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] for a blank reason, or
    /// [`ErrorCode::InvalidStateTransition`] for an existing terminal state.
    pub fn mark_aborted(
        &mut self,
        reason: impl Into<String>,
        recoverable: bool,
        partial_available: bool,
        mut notifications: Vec<HssRecoveryNotification>,
    ) -> Result<(), JlinkError> {
        if matches!(
            self.lifecycle,
            HssRunState::Completed | HssRunState::Failed | HssRunState::Aborted
        ) {
            return Err(invalid_hss_transition("HSS 终态不能再次转换为 aborted"));
        }
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(hss_value_invalid("aborted reason 不能为空或仅包含空白"));
        }
        notifications.push(HssRecoveryNotification::AbortedCaptureRecovered);
        self.lifecycle = HssRunState::Aborted;
        self.integrity = HssDataIntegrity::Unknown;
        self.partial_available = partial_available;
        self.reason = Some(reason);
        self.recoverable = Some(recoverable);
        self.recovery_notifications = notifications;
        Ok(())
    }

    /// Returns the current lifecycle state.
    #[must_use]
    pub const fn lifecycle(&self) -> HssRunState {
        self.lifecycle
    }

    /// Returns the independent data-integrity state.
    #[must_use]
    pub const fn integrity(&self) -> HssDataIntegrity {
        self.integrity
    }

    /// Returns the terminal failure code, when state is `failed`.
    #[must_use]
    pub const fn failure_code(&self) -> Option<ErrorCode> {
        self.failure_code
    }

    /// Returns whether retained partial data is available.
    #[must_use]
    pub const fn partial_available(&self) -> bool {
        self.partial_available
    }

    /// Returns the interruption reason, when state is `aborted`.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Returns whether an aborted capture can be recovered further.
    #[must_use]
    pub const fn recoverable(&self) -> Option<bool> {
        self.recoverable
    }

    /// Returns ordered recovery facts retained with this state.
    #[must_use]
    pub fn recovery_notifications(&self) -> &[HssRecoveryNotification] {
        &self.recovery_notifications
    }

    fn require(&self, expected: HssRunState, message: &str) -> Result<(), JlinkError> {
        if self.lifecycle == expected {
            Ok(())
        } else {
            Err(invalid_hss_transition(message).with_detail("actual_state", json!(self.lifecycle)))
        }
    }
}

/// Aggregate timing evidence for serialized HSS drain calls.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssDrainTiming {
    /// Number of serialized DLL drain calls.
    pub calls: u64,
    /// Sum of all drain call durations.
    pub total_us: u64,
    /// Longest observed drain call duration.
    pub max_us: u64,
}

/// Strength of one HSS quality conclusion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssQualityEvidence {
    /// A direct signal proves the condition.
    Confirmed,
    /// Observable timing or framing facts indicate the condition without an independent counter.
    Suspected,
    /// The frozen ABI cannot prove or disprove the condition.
    Unknown,
}

/// Stable reason supporting a loss or overflow assessment.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssQualityBasis {
    /// J-Link 6.98a exposes neither a sequence number nor an overflow counter.
    NoIndependentOverflowOrSequenceCounter,
    /// The DLL exposed a recognizable overflow indication.
    DllOverflowSignal,
    /// One or more reads did not end on a frozen record boundary.
    ShortOrMalformedRead,
    /// Source timestamps contain gaps that cannot be reconciled with timestamp collisions.
    SourceTimestampGap,
    /// Source timestamps moved backwards.
    SourceTimestampRegression,
}

/// Loss conclusion that deliberately omits a numeric count when none is provable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssLossAssessment {
    /// Evidence strength for possible lost samples.
    pub evidence: HssQualityEvidence,
    /// Observable basis for the conclusion.
    pub basis: HssQualityBasis,
    /// Exact lost-sample count only when an independent source proves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost_samples: Option<u64>,
}

/// Overflow conclusion without a false zero-event claim.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssOverflowAssessment {
    /// Evidence strength for DLL buffer overflow.
    pub evidence: HssQualityEvidence,
    /// Observable basis for the conclusion.
    pub basis: HssQualityBasis,
    /// Number of direct overflow indications, omitted when the ABI has no signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<u64>,
}

/// Frozen J-Link source timestamp unit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssSourceTimeUnit {
    /// J-Link 6.98a mainline timestamps count milliseconds.
    Milliseconds,
}

/// Unit used by public capture and host timeline fields.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssNormalizedTimeUnit {
    /// Integer microseconds without an implied microsecond source resolution.
    Microseconds,
}

/// Explicit cross-clock mapping used by the Worker.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssClockMappingMethod {
    /// Source elapsed time is anchored to the bounded DLL Start call window.
    CaptureStartCallBound,
}

/// Source and host clock facts retained with every capture.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssClockEvidence {
    /// Unit of the frozen 32-bit source value.
    pub source_unit: HssSourceTimeUnit,
    /// Frequency of the source counter.
    pub source_frequency_hz: u32,
    /// Proven source resolution after normalization.
    pub source_resolution_us: u32,
    /// Unit used by normalized source timestamps and Worker monotonic events.
    pub normalized_unit: HssNormalizedTimeUnit,
    /// Host event domain is monotonic elapsed time since successful Start return.
    pub host_monotonic_since_start: bool,
    /// Deterministic mapping used between source and host domains.
    pub mapping_method: HssClockMappingMethod,
    /// Upper bound from DLL Start-call duration plus source resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mapping_error_us: Option<u64>,
    /// First normalized source timestamp observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_timestamp_us: Option<u64>,
    /// Last normalized source timestamp observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_timestamp_us: Option<u64>,
}

/// Aggregate source interval statistics without floating-point ambiguity.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssIntervalStatistics {
    /// Number of adjacent source timestamp pairs observed.
    pub intervals: u64,
    /// Smallest non-regressing interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_us: Option<u64>,
    /// Largest non-regressing interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_us: Option<u64>,
    /// Sum used to derive an exact rational mean.
    pub total_us: u64,
    /// Adjacent records mapped to the same requested-rate slot.
    pub collisions: u64,
    /// Adjacent records that skipped at least one requested-rate slot.
    pub gap_events: u64,
    /// Total requested-rate slots skipped by those gaps.
    pub gap_slots: u64,
    /// Source timestamps that moved backwards.
    pub regressions: u64,
}

/// Bounded categories emitted by the production quality classifier.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssQualityEventKind {
    /// A direct DLL indication reported buffer overflow.
    BufferOverflow,
    /// A non-empty read was smaller than one complete record.
    ShortFrame,
    /// A read did not end on a record boundary.
    FrameFormat,
    /// Source timestamp spacing differs from the requested-rate slots.
    SampleInterval,
    /// Source timestamps moved backwards.
    ClockRegression,
}

/// Aggregated occurrence range for one quality event category and evidence level.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssQualityEvent {
    /// Stable quality category.
    pub kind: HssQualityEventKind,
    /// Evidence strength for this category.
    pub evidence: HssQualityEvidence,
    /// First Worker monotonic observation time.
    pub first_host_elapsed_us: u64,
    /// Last Worker monotonic observation time.
    pub last_host_elapsed_us: u64,
    /// First affected complete-record index.
    pub first_record: u64,
    /// Last affected complete-record index.
    pub last_record: u64,
    /// Number of observations aggregated into this range.
    pub occurrences: u64,
}

/// Persisted acquisition quality facts used by overview and timeline queries.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssQualitySummary {
    /// Requested rate; never presented as achieved rate.
    pub requested_rate_hz: u32,
    /// Fixed-duration requested sample count.
    pub expected_samples: u64,
    /// Number of complete source records actually observed.
    pub actual_samples: u64,
    /// Rate derived from source timestamp span, in milli-hertz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_rate_millihz: Option<u64>,
    /// Source interval statistics.
    pub intervals: HssIntervalStatistics,
    /// Loss conclusion with no synthetic zero.
    pub loss: HssLossAssessment,
    /// Overflow conclusion with no synthetic zero.
    pub overflow: HssOverflowAssessment,
    /// Explicit source/host clock contract.
    pub clock: HssClockEvidence,
    /// Aggregated quality occurrences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<HssQualityEvent>,
}

impl Default for HssQualitySummary {
    fn default() -> Self {
        Self {
            requested_rate_hz: 0,
            expected_samples: 0,
            actual_samples: 0,
            actual_rate_millihz: None,
            intervals: HssIntervalStatistics::default(),
            loss: HssLossAssessment {
                evidence: HssQualityEvidence::Unknown,
                basis: HssQualityBasis::NoIndependentOverflowOrSequenceCounter,
                lost_samples: None,
            },
            overflow: HssOverflowAssessment {
                evidence: HssQualityEvidence::Unknown,
                basis: HssQualityBasis::NoIndependentOverflowOrSequenceCounter,
                events: None,
            },
            clock: HssClockEvidence {
                source_unit: HssSourceTimeUnit::Milliseconds,
                source_frequency_hz: HSS_SOURCE_TIMESTAMP_FREQUENCY_HZ,
                source_resolution_us: HSS_SOURCE_TIMESTAMP_RESOLUTION_US,
                normalized_unit: HssNormalizedTimeUnit::Microseconds,
                host_monotonic_since_start: true,
                mapping_method: HssClockMappingMethod::CaptureStartCallBound,
                mapping_error_us: None,
                first_timestamp_us: None,
                last_timestamp_us: None,
            },
            events: Vec::new(),
        }
    }
}

/// Pure incremental quality classifier owned by the Worker acquisition state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HssQualityTracker {
    summary: HssQualitySummary,
    previous_timestamp_us: Option<u64>,
    previous_slot: Option<u64>,
    last_host_elapsed_us: u64,
}

impl HssQualityTracker {
    /// Creates quality state for one validated fixed-duration plan.
    #[must_use]
    pub fn new(plan: &HssStartPlan, start_call_elapsed_us: u64) -> Self {
        let mut summary = HssQualitySummary {
            requested_rate_hz: plan.rate_hz(),
            expected_samples: u64::from(plan.duration_s()) * u64::from(plan.rate_hz()),
            ..HssQualitySummary::default()
        };
        summary.clock.mapping_error_us = Some(
            start_call_elapsed_us.saturating_add(u64::from(HSS_SOURCE_TIMESTAMP_RESOLUTION_US)),
        );
        Self {
            summary,
            previous_timestamp_us: None,
            previous_slot: None,
            last_host_elapsed_us: 0,
        }
    }

    /// Records one DLL read boundary without discarding bytes that may complete later.
    pub fn observe_read_shape(
        &mut self,
        read_bytes: usize,
        record_bytes: usize,
        host_elapsed_us: u64,
        first_record: u64,
    ) {
        self.last_host_elapsed_us = self.last_host_elapsed_us.max(host_elapsed_us);
        if read_bytes == 0 {
            return;
        }
        if read_bytes < record_bytes {
            self.record_event(
                HssQualityEventKind::ShortFrame,
                HssQualityEvidence::Suspected,
                host_elapsed_us,
                first_record,
                first_record,
            );
        } else if !read_bytes.is_multiple_of(record_bytes) {
            self.record_event(
                HssQualityEventKind::FrameFormat,
                HssQualityEvidence::Suspected,
                host_elapsed_us,
                first_record,
                first_record
                    .saturating_add(u64::try_from(read_bytes / record_bytes).unwrap_or(u64::MAX)),
            );
        }
    }

    /// Parses complete frozen-layout records and updates normalized time evidence.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::FrameInvalid`] when the bytes do not contain only
    /// complete records for the validated layout.
    pub fn observe_complete_records(
        &mut self,
        layout: HssFrameLayout,
        bytes: &[u8],
        host_elapsed_us: u64,
    ) -> Result<u64, JlinkError> {
        self.last_host_elapsed_us = self.last_host_elapsed_us.max(host_elapsed_us);
        let batch = layout.parse(bytes)?;
        if !batch.incomplete_tail.is_empty() {
            return Err(frame_invalid(
                "quality classifier received bytes outside a complete record boundary",
            ));
        }
        for frame in batch.frames {
            let record_index = self.summary.actual_samples;
            let timestamp_us = normalize_hss_timestamp_us(frame.timestamp_raw);
            self.observe_timestamp(timestamp_us, host_elapsed_us, record_index);
            self.summary.actual_samples = self.summary.actual_samples.saturating_add(1);
        }
        Ok(self.summary.actual_samples)
    }

    /// Records a direct overflow indication from an ABI that exposes one.
    pub fn record_confirmed_overflow(
        &mut self,
        host_elapsed_us: u64,
        first_record: u64,
        last_record: u64,
    ) {
        self.last_host_elapsed_us = self.last_host_elapsed_us.max(host_elapsed_us);
        self.record_event(
            HssQualityEventKind::BufferOverflow,
            HssQualityEvidence::Confirmed,
            host_elapsed_us,
            first_record,
            last_record,
        );
        let events = self.summary.overflow.events.unwrap_or(0).saturating_add(1);
        self.summary.overflow = HssOverflowAssessment {
            evidence: HssQualityEvidence::Confirmed,
            basis: HssQualityBasis::DllOverflowSignal,
            events: Some(events),
        };
    }

    /// Records a read result that cannot be interpreted as the frozen frame format.
    pub fn record_frame_format_error(&mut self, host_elapsed_us: u64, record_index: u64) {
        self.last_host_elapsed_us = self.last_host_elapsed_us.max(host_elapsed_us);
        self.record_event(
            HssQualityEventKind::FrameFormat,
            HssQualityEvidence::Confirmed,
            host_elapsed_us,
            record_index,
            record_index,
        );
    }

    /// Produces current or terminal quality facts without consuming the tracker.
    #[must_use]
    pub fn summary(&self, final_tail_bytes: usize) -> HssQualitySummary {
        let mut summary = self.summary.clone();
        let timestamp_span = summary
            .clock
            .first_timestamp_us
            .zip(summary.clock.last_timestamp_us)
            .and_then(|(first, last)| last.checked_sub(first));
        summary.actual_rate_millihz = (summary.intervals.regressions == 0)
            .then_some(timestamp_span)
            .flatten()
            .and_then(|span| {
                (span > 0 && summary.actual_samples > 1).then(|| {
                    summary
                        .actual_samples
                        .saturating_sub(1)
                        .saturating_mul(1_000_000_000)
                        / span
                })
            });
        if final_tail_bytes > 0 {
            aggregate_quality_event(
                &mut summary.events,
                HssQualityEventKind::ShortFrame,
                HssQualityEvidence::Confirmed,
                self.last_host_elapsed_us,
                summary.actual_samples,
                summary.actual_samples,
            );
        }
        summary.loss = loss_assessment(&summary, final_tail_bytes);
        summary
    }

    /// Returns the strongest integrity conclusion supported by current quality facts.
    #[must_use]
    pub fn integrity(&self, final_tail_bytes: usize) -> HssDataIntegrity {
        let unmatched_gaps = self
            .summary
            .intervals
            .gap_slots
            .saturating_sub(self.summary.intervals.collisions);
        if final_tail_bytes > 0
            || self.summary.overflow.evidence == HssQualityEvidence::Confirmed
            || self.summary.intervals.regressions > 0
            || unmatched_gaps > 0
        {
            HssDataIntegrity::Degraded
        } else {
            HssDataIntegrity::Unknown
        }
    }

    fn observe_timestamp(&mut self, timestamp_us: u64, host_elapsed_us: u64, record_index: u64) {
        self.summary
            .clock
            .first_timestamp_us
            .get_or_insert(timestamp_us);
        self.summary.clock.last_timestamp_us = Some(timestamp_us);
        let slot = timestamp_us
            .saturating_mul(u64::from(self.summary.requested_rate_hz))
            .saturating_add(500_000)
            / 1_000_000;
        if let Some(previous) = self.previous_timestamp_us {
            if timestamp_us < previous {
                self.summary.intervals.regressions =
                    self.summary.intervals.regressions.saturating_add(1);
                self.record_event(
                    HssQualityEventKind::ClockRegression,
                    HssQualityEvidence::Confirmed,
                    host_elapsed_us,
                    record_index.saturating_sub(1),
                    record_index,
                );
            } else {
                let delta = timestamp_us - previous;
                self.summary.intervals.intervals =
                    self.summary.intervals.intervals.saturating_add(1);
                self.summary.intervals.total_us =
                    self.summary.intervals.total_us.saturating_add(delta);
                self.summary.intervals.min_us = Some(
                    self.summary
                        .intervals
                        .min_us
                        .map_or(delta, |current| current.min(delta)),
                );
                self.summary.intervals.max_us = Some(
                    self.summary
                        .intervals
                        .max_us
                        .map_or(delta, |current| current.max(delta)),
                );
            }
        }
        if let Some(previous_slot) = self.previous_slot {
            if slot < previous_slot {
                // Timestamp regression already records the stronger event above.
            } else if slot == previous_slot {
                self.summary.intervals.collisions =
                    self.summary.intervals.collisions.saturating_add(1);
                self.record_event(
                    HssQualityEventKind::SampleInterval,
                    HssQualityEvidence::Unknown,
                    host_elapsed_us,
                    record_index.saturating_sub(1),
                    record_index,
                );
            } else if slot > previous_slot.saturating_add(1) {
                self.summary.intervals.gap_events =
                    self.summary.intervals.gap_events.saturating_add(1);
                self.summary.intervals.gap_slots = self
                    .summary
                    .intervals
                    .gap_slots
                    .saturating_add(slot - previous_slot - 1);
                self.record_event(
                    HssQualityEventKind::SampleInterval,
                    HssQualityEvidence::Suspected,
                    host_elapsed_us,
                    record_index.saturating_sub(1),
                    record_index,
                );
            }
        }
        self.previous_timestamp_us = Some(timestamp_us);
        self.previous_slot = Some(slot);
    }

    fn record_event(
        &mut self,
        kind: HssQualityEventKind,
        evidence: HssQualityEvidence,
        host_elapsed_us: u64,
        first_record: u64,
        last_record: u64,
    ) {
        aggregate_quality_event(
            &mut self.summary.events,
            kind,
            evidence,
            host_elapsed_us,
            first_record,
            last_record,
        );
    }
}

/// Converts the frozen J-Link 6.98a millisecond counter to integer microseconds.
#[must_use]
pub fn normalize_hss_timestamp_us(timestamp_ms: u32) -> u64 {
    u64::from(timestamp_ms) * u64::from(HSS_SOURCE_TIMESTAMP_RESOLUTION_US)
}

fn aggregate_quality_event(
    events: &mut Vec<HssQualityEvent>,
    kind: HssQualityEventKind,
    evidence: HssQualityEvidence,
    host_elapsed_us: u64,
    first_record: u64,
    last_record: u64,
) {
    if let Some(event) = events
        .iter_mut()
        .find(|event| event.kind == kind && event.evidence == evidence)
    {
        event.last_host_elapsed_us = host_elapsed_us;
        event.last_record = event.last_record.max(last_record);
        event.occurrences = event.occurrences.saturating_add(1);
        return;
    }
    events.push(HssQualityEvent {
        kind,
        evidence,
        first_host_elapsed_us: host_elapsed_us,
        last_host_elapsed_us: host_elapsed_us,
        first_record,
        last_record,
        occurrences: 1,
    });
}

fn loss_assessment(summary: &HssQualitySummary, final_tail_bytes: usize) -> HssLossAssessment {
    if summary.overflow.evidence == HssQualityEvidence::Confirmed {
        return HssLossAssessment {
            evidence: HssQualityEvidence::Confirmed,
            basis: HssQualityBasis::DllOverflowSignal,
            lost_samples: None,
        };
    }
    if summary.intervals.regressions > 0 {
        return HssLossAssessment {
            evidence: HssQualityEvidence::Suspected,
            basis: HssQualityBasis::SourceTimestampRegression,
            lost_samples: None,
        };
    }
    let unmatched_gaps = summary
        .intervals
        .gap_slots
        .saturating_sub(summary.intervals.collisions);
    if unmatched_gaps > 0 {
        return HssLossAssessment {
            evidence: HssQualityEvidence::Suspected,
            basis: HssQualityBasis::SourceTimestampGap,
            lost_samples: None,
        };
    }
    if final_tail_bytes > 0
        || summary.events.iter().any(|event| {
            matches!(
                event.kind,
                HssQualityEventKind::ShortFrame | HssQualityEventKind::FrameFormat
            )
        })
    {
        return HssLossAssessment {
            evidence: HssQualityEvidence::Suspected,
            basis: HssQualityBasis::ShortOrMalformedRead,
            lost_samples: None,
        };
    }
    HssLossAssessment {
        evidence: HssQualityEvidence::Unknown,
        basis: HssQualityBasis::NoIndependentOverflowOrSequenceCounter,
        lost_samples: None,
    }
}

/// Result retained for one write interleaved with an active capture.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum HssWriteResult {
    /// The write and requested readback completed successfully.
    Succeeded,
    /// The write returned a stable error while capture continued.
    Failed {
        /// Stable error returned to the write caller.
        code: ErrorCode,
    },
}

/// Retained target-write category used by immutable timeline and event queries.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HssWriteKind {
    /// Backward-compatible category for captures created before write kind was retained.
    #[default]
    TargetWrite,
    /// Raw memory or MMIO write.
    MemoryWrite,
    /// DWARF-resolved typed variable write.
    VariableWrite,
}

/// Queue, execution, and next-drain evidence for one interleaved write.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssWriteTiming {
    /// IPC request correlated with this target write.
    pub request_id: String,
    /// Retained write category; old capture manifests default to `target_write`.
    #[serde(default)]
    pub kind: HssWriteKind,
    /// Time since capture start when the listener accepted the request.
    pub requested_at_us: u64,
    /// Time since capture start when the DLL scheduler began the write.
    pub started_at_us: u64,
    /// Time since capture start when the write returned.
    pub completed_at_us: u64,
    /// Stable write outcome retained even when the write failed.
    pub result: HssWriteResult,
    /// Complete records observed before the write began.
    pub samples_before: u64,
    /// Complete records observed immediately after the next drain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples_after_next_drain: Option<u64>,
}

/// Minimal internal status returned while persistence and query views are built.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HssRunSnapshot {
    /// Stable public capture identity.
    pub capture_id: String,
    /// Current capture lifecycle state.
    pub state: HssRunState,
    /// Data-integrity state assessed independently from lifecycle.
    pub integrity: HssDataIntegrity,
    /// Monotonic time since the successful Start call.
    pub elapsed_us: u64,
    /// Number of complete frozen-layout records drained so far.
    pub complete_records: u64,
    /// Aggregate serialized drain timings.
    pub drain: HssDrainTiming,
    /// Source timing, rate, loss, overflow, and framing evidence.
    #[serde(default)]
    pub quality: HssQualitySummary,
    /// Ordered write-interleaving evidence.
    pub writes: Vec<HssWriteTiming>,
    /// Stable failure code present only for controlled `failed` captures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<ErrorCode>,
    /// Whether valid partial data was retained for failed or aborted captures.
    pub partial_available: bool,
    /// Stable interruption reason present only for `aborted` captures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether an aborted partial capture can be recovered further.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recoverable: Option<bool>,
    /// Ordered recovery facts retained with this status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_notifications: Vec<HssRecoveryNotification>,
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
        parse_rule_path(path).map_err(|error| error.with_detail("rule_id", json!(id)))?;
        if value.is_some_and(|item| matches!(item, Value::Null | Value::String(_))) {
            return Err(hss_value_invalid(
                "HSS 规则 value 必须是 boolean、number、object 或 array",
            )
            .with_detail("rule_id", json!(id)));
        }
        match self {
            Self::AbsDeltaGte { value, .. } => {
                if parse_rule_number(value, id, "value")?.is_negative() {
                    return Err(rule_value_invalid(id, "abs_delta_gte.value 不能为负数"));
                }
            }
            Self::Outside { min, max, .. } => {
                if compare_rule_numbers(
                    RuleNumber::from_json_number(min),
                    RuleNumber::from_json_number(max),
                    id,
                )? == Ordering::Greater
                {
                    return Err(rule_value_invalid(id, "outside.min 不能大于 outside.max"));
                }
            }
            Self::Equals { .. } => {}
            Self::Crosses { value, .. } => {
                parse_rule_number(value, id, "value")?;
            }
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

    /// Returns the normalized exact or wildcard leaf path.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::AbsDeltaGte { path, .. }
            | Self::Outside { path, .. }
            | Self::Equals { path, .. }
            | Self::Crosses { path, .. } => path,
        }
    }

    /// Returns whether one normalized concrete DWARF leaf path matches this rule.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] if either persisted rule data or the
    /// candidate path violates its respective closed grammar.
    pub fn matches_path(&self, candidate: &str) -> Result<bool, JlinkError> {
        let pattern = parse_rule_path(self.path())?;
        let selector = VariableSelector::new(candidate, None)?;
        if pattern.root != selector.root() {
            return Ok(false);
        }
        let candidate_steps = selector.steps()?;
        if pattern.steps.len() != candidate_steps.len() {
            return Ok(false);
        }
        Ok(pattern
            .steps
            .iter()
            .zip(candidate_steps)
            .all(|(expected, actual)| expected.matches(&actual)))
    }

    /// Evaluates this rule over one adjacent pair of already decoded leaf values.
    ///
    /// Exact changes remain a separate query fact; this method returns only the
    /// declared threshold match.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] when a numeric rule targets a
    /// non-numeric value or would require an inexact mixed comparison.
    pub fn matches_values(&self, before: &Value, after: &Value) -> Result<bool, JlinkError> {
        self.validate()?;
        match self {
            Self::AbsDeltaGte { id, value, .. } => {
                let before = parse_rule_number(before, id, "before")?;
                let after = parse_rule_number(after, id, "after")?;
                let threshold = parse_rule_number(value, id, "value")?;
                absolute_delta_gte(before, after, threshold, id)
            }
            Self::Outside { id, min, max, .. } => {
                let after = parse_rule_number(after, id, "after")?;
                Ok(
                    compare_rule_numbers(after, RuleNumber::from_json_number(min), id)?
                        == Ordering::Less
                        || compare_rule_numbers(after, RuleNumber::from_json_number(max), id)?
                            == Ordering::Greater,
                )
            }
            Self::Equals { value, .. } => Ok(before != value && after == value),
            Self::Crosses {
                id,
                value,
                direction,
                ..
            } => {
                let before = parse_rule_number(before, id, "before")?;
                let after = parse_rule_number(after, id, "after")?;
                let threshold = parse_rule_number(value, id, "value")?;
                let before_cmp = compare_rule_numbers(before, threshold, id)?;
                let after_cmp = compare_rule_numbers(after, threshold, id)?;
                let up = before_cmp == Ordering::Less && after_cmp != Ordering::Less;
                let down = before_cmp == Ordering::Greater && after_cmp != Ordering::Greater;
                Ok(match direction {
                    HssCrossingDirection::Up => up,
                    HssCrossingDirection::Down => down,
                    HssCrossingDirection::Either => up || down,
                })
            }
        }
    }

    fn normalize_path(&mut self) -> Result<(), JlinkError> {
        let normalized = parse_rule_path(self.path())?.normalized;
        match self {
            Self::AbsDeltaGte { path, .. }
            | Self::Outside { path, .. }
            | Self::Equals { path, .. }
            | Self::Crosses { path, .. } => *path = normalized,
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RulePathPattern {
    root: String,
    steps: Vec<RulePathStep>,
    normalized: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RulePathStep {
    Member(String),
    Index(u64),
    Wildcard,
}

impl RulePathStep {
    fn matches(&self, candidate: &SelectorStep) -> bool {
        match (self, candidate) {
            (Self::Member(expected), SelectorStep::Member(actual)) => expected == actual,
            (Self::Index(expected), SelectorStep::Index(actual)) => expected == actual,
            (Self::Wildcard, SelectorStep::Index(_)) => true,
            (Self::Member(_) | Self::Index(_), _) | (Self::Wildcard, SelectorStep::Member(_)) => {
                false
            }
        }
    }
}

fn parse_rule_path(path: &str) -> Result<RulePathPattern, JlinkError> {
    if path.is_empty() || path.trim() != path || !path.is_ascii() {
        return Err(hss_value_invalid(
            "HSS 规则 path 必须是非空、无首尾空白的 ASCII 路径",
        ));
    }
    let bytes = path.as_bytes();
    let (root, mut cursor) = parse_rule_identifier(path, 0)?;
    let mut normalized = root.clone();
    let mut steps = Vec::new();
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'.' => {
                let (member, next) = parse_rule_identifier(path, cursor + 1)?;
                normalized.push('.');
                normalized.push_str(&member);
                steps.push(RulePathStep::Member(member));
                cursor = next;
            }
            b'[' => {
                let start = cursor + 1;
                let Some(relative_end) = bytes[start..].iter().position(|byte| *byte == b']')
                else {
                    return Err(hss_value_invalid("HSS 规则数组路径缺少右方括号"));
                };
                let end = start + relative_end;
                let token = &path[start..end];
                normalized.push('[');
                if token == "*" {
                    normalized.push('*');
                    steps.push(RulePathStep::Wildcard);
                } else {
                    if token.is_empty() || !token.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(hss_value_invalid(
                            "HSS 规则数组路径只允许非负十进制索引或 *",
                        ));
                    }
                    let index = token
                        .parse::<u64>()
                        .map_err(|_| hss_value_invalid("HSS 规则数组索引超出 u64 范围"))?;
                    normalized.push_str(&index.to_string());
                    steps.push(RulePathStep::Index(index));
                }
                normalized.push(']');
                cursor = end + 1;
            }
            _ => {
                return Err(hss_value_invalid(
                    "HSS 规则 path 只允许成员点号、数组索引和 [*]",
                ));
            }
        }
    }
    Ok(RulePathPattern {
        root,
        steps,
        normalized,
    })
}

fn parse_rule_identifier(path: &str, start: usize) -> Result<(String, usize), JlinkError> {
    let bytes = path.as_bytes();
    let Some(first) = bytes.get(start).copied() else {
        return Err(hss_value_invalid("HSS 规则 path 缺少标识符"));
    };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return Err(hss_value_invalid(
            "HSS 规则标识符必须以 ASCII 字母或下划线开头",
        ));
    }
    let mut end = start + 1;
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        end += 1;
    }
    Ok((path[start..end].to_owned(), end))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RuleNumber {
    Integer(i128),
    Float(f64),
}

impl RuleNumber {
    fn from_json_number(number: &Number) -> Self {
        if let Some(value) = number.as_i64() {
            Self::Integer(i128::from(value))
        } else if let Some(value) = number.as_u64() {
            Self::Integer(i128::from(value))
        } else {
            Self::Float(number.as_f64().expect("serde_json number is finite"))
        }
    }

    const fn is_negative(self) -> bool {
        match self {
            Self::Integer(value) => value < 0,
            Self::Float(value) => value < 0.0,
        }
    }
}

fn parse_rule_number(value: &Value, rule_id: &str, field: &str) -> Result<RuleNumber, JlinkError> {
    if let Some(number) = value.as_number() {
        return Ok(RuleNumber::from_json_number(number));
    }
    let Some(object) = value.as_object() else {
        return Err(rule_value_invalid(
            rule_id,
            format!("{field} 必须是数值 TypedValue"),
        ));
    };
    if object.len() != 3 {
        return Err(rule_value_invalid(
            rule_id,
            format!("{field} 的扩展整数必须只含 $int/bits/signed"),
        ));
    }
    let integer = object
        .get("$int")
        .and_then(Value::as_str)
        .ok_or_else(|| rule_value_invalid(rule_id, format!("{field} 缺少 $int 字符串")))?;
    let bits = object
        .get("bits")
        .and_then(Value::as_u64)
        .filter(|bits| (1..=64).contains(bits))
        .ok_or_else(|| rule_value_invalid(rule_id, format!("{field}.bits 必须为 1..64")))?;
    let signed = object
        .get("signed")
        .and_then(Value::as_bool)
        .ok_or_else(|| rule_value_invalid(rule_id, format!("{field}.signed 必须为 boolean")))?;
    let parsed = if signed {
        let parsed = integer
            .parse::<i128>()
            .map_err(|_| rule_value_invalid(rule_id, format!("{field} 的有符号整数无效")))?;
        let minimum = -(1_i128 << (bits - 1));
        let maximum = (1_i128 << (bits - 1)) - 1;
        (minimum..=maximum)
            .contains(&parsed)
            .then_some(parsed)
            .ok_or_else(|| rule_value_invalid(rule_id, format!("{field} 超出声明位宽")))?
    } else {
        let parsed = integer
            .parse::<u128>()
            .map_err(|_| rule_value_invalid(rule_id, format!("{field} 的无符号整数无效")))?;
        let maximum = (1_u128 << bits) - 1;
        let parsed = (parsed <= maximum)
            .then_some(parsed)
            .ok_or_else(|| rule_value_invalid(rule_id, format!("{field} 超出声明位宽")))?;
        i128::try_from(parsed)
            .map_err(|_| rule_value_invalid(rule_id, format!("{field} 无法参与数值比较")))?
    };
    Ok(RuleNumber::Integer(parsed))
}

fn compare_rule_numbers(
    left: RuleNumber,
    right: RuleNumber,
    rule_id: &str,
) -> Result<Ordering, JlinkError> {
    match (left, right) {
        (RuleNumber::Integer(left), RuleNumber::Integer(right)) => Ok(left.cmp(&right)),
        (RuleNumber::Float(left), RuleNumber::Float(right)) => left
            .partial_cmp(&right)
            .ok_or_else(|| rule_value_invalid(rule_id, "阈值浮点比较无序")),
        (RuleNumber::Integer(left), RuleNumber::Float(right)) => {
            compare_mixed_number(left, right, rule_id)
        }
        (RuleNumber::Float(left), RuleNumber::Integer(right)) => {
            compare_mixed_number(right, left, rule_id).map(Ordering::reverse)
        }
    }
}

/// Compares two numeric `TypedValue` instances without silently losing integer precision.
///
/// # Errors
///
/// Returns [`ErrorCode::ValueInvalid`] when either value is non-numeric or an
/// integer/float comparison cannot be represented safely.
pub fn compare_numeric_typed_values(left: &Value, right: &Value) -> Result<Ordering, JlinkError> {
    compare_rule_numbers(
        parse_rule_number(left, "typed-value-comparison", "left")?,
        parse_rule_number(right, "typed-value-comparison", "right")?,
        "typed-value-comparison",
    )
}

fn compare_mixed_number(integer: i128, float: f64, rule_id: &str) -> Result<Ordering, JlinkError> {
    safe_integer_as_f64(integer, rule_id)?
        .partial_cmp(&float)
        .ok_or_else(|| rule_value_invalid(rule_id, "阈值浮点比较无序"))
}

fn safe_integer_as_f64(integer: i128, rule_id: &str) -> Result<f64, JlinkError> {
    const JSON_SAFE_INTEGER: u128 = 9_007_199_254_740_991;
    if integer.unsigned_abs() > JSON_SAFE_INTEGER {
        return Err(rule_value_invalid(
            rule_id,
            "超出 JSON 安全整数范围的整数不能与浮点阈值混合比较",
        ));
    }
    integer
        .to_string()
        .parse::<f64>()
        .map_err(|_| rule_value_invalid(rule_id, "JSON 安全整数无法转换为浮点比较值"))
}

fn absolute_delta_gte(
    before: RuleNumber,
    after: RuleNumber,
    threshold: RuleNumber,
    rule_id: &str,
) -> Result<bool, JlinkError> {
    if threshold.is_negative() {
        return Err(rule_value_invalid(
            rule_id,
            "abs_delta_gte.value 不能为负数",
        ));
    }
    let delta = match (before, after) {
        (RuleNumber::Integer(before), RuleNumber::Integer(after)) => RuleNumber::Integer(
            i128::try_from(before.abs_diff(after))
                .map_err(|_| rule_value_invalid(rule_id, "相邻整数绝对差超出可比较范围"))?,
        ),
        (before, after) => {
            let before = rule_number_as_f64(before, rule_id)?;
            let after = rule_number_as_f64(after, rule_id)?;
            let delta = (after - before).abs();
            if !delta.is_finite() {
                return Err(rule_value_invalid(rule_id, "相邻浮点绝对差不是有限值"));
            }
            RuleNumber::Float(delta)
        }
    };
    Ok(compare_rule_numbers(delta, threshold, rule_id)? != Ordering::Less)
}

fn rule_number_as_f64(number: RuleNumber, rule_id: &str) -> Result<f64, JlinkError> {
    match number {
        RuleNumber::Float(value) => Ok(value),
        RuleNumber::Integer(value) => safe_integer_as_f64(value, rule_id),
    }
}

fn rule_value_invalid(rule_id: &str, message: impl Into<String>) -> JlinkError {
    hss_value_invalid(message).with_detail("rule_id", json!(rule_id))
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
        let rules = normalize_hss_rules(rules)?;
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
    target_fingerprint: String,
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

    /// Returns the complete target-connection fingerprint retained for conflict evidence.
    #[must_use]
    pub fn target_fingerprint(&self) -> &str {
        &self.target_fingerprint
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
        target: &TargetConnectionSpec,
        plan: &HssStartPlan,
    ) -> Result<HssReservationOutcome, JlinkError> {
        plan.validate()?;
        target.validate()?;
        if probe_identity != target.probe_serial().to_string() {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                "Worker 探针身份与目标连接中的 probe_serial 不一致",
                false,
            ));
        }
        let target_fingerprint = target_fingerprint(target)?;
        if let Some(existing) = self.by_key.get(plan.capture_key()) {
            if existing.request_fingerprint == plan.request_fingerprint
                && existing.target_fingerprint == target_fingerprint
            {
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
            )
            .with_detail(
                "original_target_fingerprint",
                json!(existing.target_fingerprint),
            )
            .with_detail("requested_target_fingerprint", json!(target_fingerprint)));
        }
        let reservation = HssCaptureReservation {
            capture_id: capture_id(probe_identity, &target_fingerprint, plan),
            request_fingerprint: plan.request_fingerprint.clone(),
            target_fingerprint,
        };
        self.by_key
            .insert(plan.capture_key.clone(), reservation.clone());
        Ok(HssReservationOutcome::Created(reservation))
    }

    /// Resolves a previously reserved capture key without changing registry state.
    #[must_use]
    pub fn capture_id_for_key(&self, capture_key: &str) -> Option<&str> {
        self.by_key
            .get(capture_key)
            .map(HssCaptureReservation::capture_id)
    }

    /// Releases only the exact newly-created reservation after Start failed.
    ///
    /// Existing idempotent reservations are never removed by this operation.
    pub fn rollback_created(&mut self, plan: &HssStartPlan, reservation: &HssCaptureReservation) {
        if self.by_key.get(plan.capture_key()) == Some(reservation) {
            self.by_key.remove(plan.capture_key());
        }
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

/// Validates and sorts one declarative HSS rule set by unique rule ID.
///
/// # Errors
///
/// Returns [`ErrorCode::ValueInvalid`] for invalid paths, thresholds, value
/// shapes, or duplicate rule IDs.
pub fn normalize_hss_rules(
    mut rules: Vec<HssThresholdRule>,
) -> Result<Vec<HssThresholdRule>, JlinkError> {
    for rule in &mut rules {
        rule.validate()?;
        rule.normalize_path()?;
    }
    rules.sort_by(|left, right| left.id().cmp(right.id()));
    if let Some(duplicate) = rules.windows(2).find(|pair| pair[0].id() == pair[1].id()) {
        return Err(hss_value_invalid("HSS 规则 id 必须唯一")
            .with_detail("rule_id", json!(duplicate[0].id())));
    }
    Ok(rules)
}

fn capture_id(probe_identity: &str, target_fingerprint: &str, plan: &HssStartPlan) -> String {
    let mut hasher = Sha256::new();
    hasher.update(probe_identity.as_bytes());
    hasher.update([0]);
    hasher.update(target_fingerprint.as_bytes());
    hasher.update([0]);
    hasher.update(plan.capture_key.as_bytes());
    hasher.update([0]);
    hasher.update(plan.request_fingerprint.as_bytes());
    let digest = encode_sha256(&hasher.finalize());
    format!("cap_{}", digest.chars().take(24).collect::<String>())
}

fn target_fingerprint(target: &TargetConnectionSpec) -> Result<String, JlinkError> {
    serde_json::to_vec(target)
        .map(|bytes| sha256(&bytes))
        .map_err(|error| hss_value_invalid(format!("无法规范化 HSS 目标连接身份：{error}")))
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

fn invalid_hss_transition(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::InvalidStateTransition, message, false)
}
