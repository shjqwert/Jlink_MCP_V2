//! Pure domain values and validation shared by the J-Link MCP processes.

mod core;
mod dwarf;
mod error;
mod hss;
mod image;
mod ipc;
mod memory;
mod profile;
mod program;
mod session;
mod state;
mod typed_value;

pub use core::{ControlAfter, ControlRequest, CoreRegister};
pub use dwarf::{
    ACCESS_PLAN_FORMAT_VERSION, AccessLayout, AccessMember, AccessPlan, BitRange, ElementSlice,
    ScalarEncoding, SelectorStep, VariableSelector,
};
pub use error::{ErrorCode, JlinkError};
pub use hss::{
    HSS_BLOCK_FLAGS_DEFAULT, HSS_MAX_DURATION_S, HSS_MAX_EXPANDED_SAMPLE_BYTES, HSS_MAX_RATE_HZ,
    HSS_MAX_TOP_LEVEL_SELECTORS, HSS_MIN_DURATION_S, HSS_MIN_RATE_HZ,
    HSS_START_FLAG_TIMESTAMP_US_EXPERIMENTAL, HSS_START_FLAGS_698A_MAINLINE, HssCapabilities,
    HssCaptureReservation, HssCaptureState, HssClockEvidence, HssClockMappingMethod,
    HssCrossingDirection, HssDataIntegrity, HssDrainTiming, HssFrameBatch, HssFrameLayout,
    HssIntervalStatistics, HssLossAssessment, HssNormalizedTimeUnit, HssOverflowAssessment,
    HssQualityBasis, HssQualityEvent, HssQualityEventKind, HssQualityEvidence, HssQualitySummary,
    HssQualityTracker, HssRawFrame, HssRecoveryNotification, HssReservationOutcome, HssReturnWhen,
    HssRunSnapshot, HssRunState, HssSourceTimeUnit, HssStartPlan, HssStartRegistry,
    HssThresholdRule, HssVariablePlan, HssWriteKind, HssWriteResult, HssWriteTiming,
    compare_numeric_typed_values, normalize_hss_rules, normalize_hss_timestamp_us,
};
pub use image::{
    FirmwareFormat, FirmwareIdentityPlan, FirmwareImage, FirmwareSegment,
    FirmwareSegmentFingerprint,
};
pub use ipc::{
    IpcRequest, IpcResponse, MAX_IPC_FRAME_BYTES, ProtocolVersion, RequestId, SessionCommand,
    WorkerStatus, probe_identity_hash, read_ipc_frame, worker_endpoint_name, write_ipc_frame,
};
pub use memory::{
    DebugRequest, DebugResult, DeviceMemoryMap, MAX_RAW_MEMORY_BYTES, MemoryRange,
    MemoryReadOrigin, MemoryReadPart, MemoryReadPlan, MemoryRegion, MemoryRegionKind,
    MergedMemoryRead, WriteVerify, merge_safe_memory_reads, validate_write_count,
    verify_memory_readback,
};
pub use profile::{
    CapabilityState, FlashProfile, ProfileConflict, ProfileConflictSeverity, ProfileSource,
    ProfileSourceKind, TargetCapabilities, canonical_device_name,
};
pub use program::{
    FlashRange, FlashRegion, ProgramAfter, ProgramRequest, VerifyMismatch,
    VerifyMismatchAccumulator, validate_flash_range, validate_image_flash_ranges,
};
pub use session::{
    FaultDiagnostics, RecoveryAction, RecoveryNotification, TargetConnectionSpec, ValidationAfter,
    ValidationCheck, ValidationCheckEvidence, ValidationCheckKind, ValidationInvalidation,
    ValidationReport, ensure_disconnect_allowed,
};
pub use state::{
    ConnectionState, DispatchState, ExecutionKind, SessionEvent, TargetInterface, TargetState,
    classify_worker_loss, transition_session,
};
pub use typed_value::{decode_typed_value, encode_typed_value};
