use serde::{Deserialize, Serialize};

use crate::{ErrorCode, JlinkError, TargetInterface, TargetState};

/// Immutable inputs that identify one target connection and validation cache key.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConnectionSpec {
    device: String,
    interface: TargetInterface,
    speed_khz: u32,
    probe_serial: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    elf_sha256: Option<String>,
}

impl TargetConnectionSpec {
    /// Validates and creates one explicit target selection.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ConfigInvalid`] for a missing probe selection,
    /// blank or generic device, zero speed, or malformed ELF digest.
    pub fn new(
        device: impl Into<String>,
        interface: TargetInterface,
        speed_khz: u32,
        probe_serial: Option<u32>,
        elf_sha256: Option<String>,
    ) -> Result<Self, JlinkError> {
        let value = Self {
            device: device.into(),
            interface,
            speed_khz,
            probe_serial: probe_serial.ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::ConfigInvalid,
                    "检测到的探针无法唯一确定，请配置 probe.serial",
                    false,
                )
            })?,
            elf_sha256,
        };
        value.validate()?;
        Ok(value)
    }

    /// Revalidates deserialized session inputs before hardware access.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ConfigInvalid`] when any frozen input is invalid.
    pub fn validate(&self) -> Result<(), JlinkError> {
        let normalized = self.device.trim().to_ascii_lowercase();
        let compact: String = normalized
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        if normalized.is_empty()
            || compact == "arm"
            || compact.starts_with("cortexm")
            || compact.starts_with("armcortexm")
            || compact == "generic"
            || normalized.contains("generic cortex")
        {
            return Err(config_error("target.device 必须是具体器件型号"));
        }
        if self.speed_khz == 0 {
            return Err(config_error("target.speed_khz 必须大于零"));
        }
        if let Some(digest) = &self.elf_sha256
            && (digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(config_error("ELF SHA-256 必须是 64 位十六进制摘要"));
        }
        Ok(())
    }

    /// Returns the concrete J-Link device name.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// Returns the selected physical interface without fallback semantics.
    #[must_use]
    pub const fn interface(&self) -> TargetInterface {
        self.interface
    }

    /// Returns the configured debug clock in kHz.
    #[must_use]
    pub const fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    /// Returns the only permitted probe serial.
    #[must_use]
    pub const fn probe_serial(&self) -> u32 {
        self.probe_serial
    }

    /// Returns the optional ELF identity included in the validation cache key.
    #[must_use]
    pub fn elf_sha256(&self) -> Option<&str> {
        self.elf_sha256.as_deref()
    }
}

/// Events that invalidate an otherwise reusable validation result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationInvalidation {
    /// The target connection was lost or explicitly closed.
    ConnectionLost,
    /// The Worker exited unexpectedly.
    WorkerExited,
    /// Flash content was programmed, erased, or otherwise changed.
    FlashModified,
    /// The validated DLL identity changed.
    DllChanged,
    /// The ELF identity changed.
    ElfChanged,
    /// The target, interface, or core configuration changed.
    TargetConfigurationChanged,
}

/// Recovery operations attempted in their actual execution order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Resume the currently halted core.
    Resume,
    /// Reset the target after resume failure or `HardFault`.
    Reset,
    /// Start execution after reset.
    RunAfterReset,
}

/// Successful automatic recovery reported to callers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryNotification {
    /// The target was halted and remained running after resume.
    ResumedFromHalt,
    /// Reset and run restored a target that could not be resumed safely.
    ResetAfterFault,
}

/// Explicit target state required after a disconnected validation session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationAfter {
    /// Leave the target stably running after validation.
    Run,
    /// Leave the target explicitly halted after validation.
    Halt,
}

/// Best-effort Cortex-M fault facts captured when recovery fails.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FaultDiagnostics {
    /// Program counter, when readable.
    pub pc: Option<u32>,
    /// Active exception number derived from ICSR.VECTACTIVE, when readable.
    pub ipsr: Option<u32>,
    /// Configurable Fault Status Register, when readable.
    pub cfsr: Option<u32>,
    /// `HardFault` Status Register, when readable.
    pub hfsr: Option<u32>,
    /// Debug Fault Status Register, when readable.
    pub dfsr: Option<u32>,
    /// Diagnostic fields that the target did not permit reading.
    pub unavailable: Vec<String>,
}

/// Stable names for the explicit validation checklist.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationCheckKind {
    /// DLL identity supplied by the MCP configuration boundary.
    DllIdentity,
    /// Required V1 exports resolved by the unique gateway.
    RequiredExports,
    /// Selected and connected probe serial.
    ProbeIdentity,
    /// Concrete target device and observed target identifier.
    TargetIdentity,
    /// Requested SWD or JTAG interface and clock.
    Interface,
    /// Read access while the target is running.
    BackgroundAccess,
    /// J-Link HSS capability entry point and non-zero limits.
    HssCapability,
}

/// One completed validation observation with actionable failure advice.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationCheck {
    /// Stable checklist item.
    pub kind: ValidationCheckKind,
    /// Whether the observed condition met the V1 requirement.
    pub passed: bool,
    /// Actual observed result.
    pub detail: String,
    /// Corrective action for a failed check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
}

/// Result of one fresh, side-effect-bounded environment validation pass.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationReport {
    /// True only when every completed check passed.
    pub valid: bool,
    /// Checks in deterministic dependency order.
    pub checks: Vec<ValidationCheck>,
    /// Actual target state after observation or explicit validation cleanup.
    pub target_state: TargetState,
    /// Target identifier observed after connection.
    pub target_id: Option<u32>,
    /// Monotonic number of complete validation passes in this Worker.
    pub validation_runs: u64,
    /// Automatic recovery actions performed by this validation pass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_notifications: Vec<RecoveryNotification>,
}

/// Enforces the session rule that an active HSS capture owns disconnect cleanup.
///
/// # Errors
///
/// Returns [`ErrorCode::OperationConflict`] while `hss_active` is true.
pub fn ensure_disconnect_allowed(hss_active: bool) -> Result<(), JlinkError> {
    if hss_active {
        return Err(JlinkError::new(
            ErrorCode::OperationConflict,
            "HSS 采集活动期间不能断开目标",
            true,
        ));
    }
    Ok(())
}

fn config_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ConfigInvalid, message, false)
}
