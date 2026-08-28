use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ErrorCode, JlinkError};

/// Canonical Cortex-M registers exposed by the V1 public contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CoreRegister {
    /// General-purpose register R0.
    R0,
    /// General-purpose register R1.
    R1,
    /// General-purpose register R2.
    R2,
    /// General-purpose register R3.
    R3,
    /// General-purpose register R4.
    R4,
    /// General-purpose register R5.
    R5,
    /// General-purpose register R6.
    R6,
    /// General-purpose register R7.
    R7,
    /// General-purpose register R8.
    R8,
    /// General-purpose register R9.
    R9,
    /// General-purpose register R10.
    R10,
    /// General-purpose register R11.
    R11,
    /// General-purpose register R12.
    R12,
    /// Canonical stack-pointer name.
    Sp,
    /// Canonical link-register name.
    Lr,
    /// Canonical program-counter name.
    Pc,
    /// Composite program status register.
    Xpsr,
    /// Main stack pointer.
    Msp,
    /// Process stack pointer.
    Psp,
    /// Application program status register.
    Apsr,
    /// Execution program status register.
    Epsr,
    /// Interrupt program status register.
    Ipsr,
    /// Interrupt mask register.
    Primask,
    /// Base priority mask register.
    Basepri,
    /// Fault mask register.
    Faultmask,
    /// Thread-mode control register.
    Control,
}

impl CoreRegister {
    /// Ordered V1 register set used for validation and target capability checks.
    pub const ALL: [Self; 26] = [
        Self::R0,
        Self::R1,
        Self::R2,
        Self::R3,
        Self::R4,
        Self::R5,
        Self::R6,
        Self::R7,
        Self::R8,
        Self::R9,
        Self::R10,
        Self::R11,
        Self::R12,
        Self::Sp,
        Self::Lr,
        Self::Pc,
        Self::Xpsr,
        Self::Msp,
        Self::Psp,
        Self::Apsr,
        Self::Epsr,
        Self::Ipsr,
        Self::Primask,
        Self::Basepri,
        Self::Faultmask,
        Self::Control,
    ];

    /// Parses one exact public canonical register name.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::RegisterNotFound`] without case folding, aliases,
    /// or fuzzy matching.
    pub fn from_canonical(name: &str) -> Result<Self, JlinkError> {
        Self::ALL
            .into_iter()
            .find(|register| register.canonical_name() == name)
            .ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::RegisterNotFound,
                    format!("目标不支持规范核心寄存器名称：{name}"),
                    false,
                )
                .with_detail("requested_name", json!(name))
            })
    }

    /// Returns the exact public canonical register name.
    #[must_use]
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::R0 => "R0",
            Self::R1 => "R1",
            Self::R2 => "R2",
            Self::R3 => "R3",
            Self::R4 => "R4",
            Self::R5 => "R5",
            Self::R6 => "R6",
            Self::R7 => "R7",
            Self::R8 => "R8",
            Self::R9 => "R9",
            Self::R10 => "R10",
            Self::R11 => "R11",
            Self::R12 => "R12",
            Self::Sp => "SP",
            Self::Lr => "LR",
            Self::Pc => "PC",
            Self::Xpsr => "XPSR",
            Self::Msp => "MSP",
            Self::Psp => "PSP",
            Self::Apsr => "APSR",
            Self::Epsr => "EPSR",
            Self::Ipsr => "IPSR",
            Self::Primask => "PRIMASK",
            Self::Basepri => "BASEPRI",
            Self::Faultmask => "FAULTMASK",
            Self::Control => "CONTROL",
        }
    }

    /// Returns the exact name expected from the frozen J-Link catalog.
    #[must_use]
    pub const fn jlink_name(self) -> &'static str {
        match self {
            Self::Sp => "R13 (SP)",
            Self::Lr => "R14",
            Self::Pc => "R15 (PC)",
            _ => self.canonical_name(),
        }
    }

    /// Returns whether V1 permits a direct register write.
    #[must_use]
    pub const fn is_writable(self) -> bool {
        !matches!(self, Self::Xpsr | Self::Epsr | Self::Ipsr)
    }

    /// Rejects architecture-defined read-only views before any target write.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] with the canonical register name.
    pub fn ensure_writable(self) -> Result<(), JlinkError> {
        if self.is_writable() {
            return Ok(());
        }
        Err(JlinkError::new(
            ErrorCode::ValueInvalid,
            format!("核心寄存器 {} 是只读视图", self.canonical_name()),
            false,
        )
        .with_detail("register", json!(self.canonical_name()))
        .with_detail("writable", json!(false)))
    }
}

/// Explicit final state requested by one target reset.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAfter {
    /// Leave the core running after reset.
    Run,
    /// Leave the core halted after reset.
    Halt,
}

/// Closed set of target execution controls carried over V1 IPC.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlRequest {
    /// Halt the active core.
    Halt,
    /// Resume the active core.
    Resume,
    /// Reset and converge to the requested explicit state.
    Reset {
        /// Required final state.
        after: ControlAfter,
    },
    /// Execute exactly one instruction from an already halted state.
    Step,
}
