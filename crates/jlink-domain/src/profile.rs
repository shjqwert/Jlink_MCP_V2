use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ErrorCode, JlinkError, MemoryRange, MemoryRegion, MemoryRegionKind};

/// Strength of one declared target capability.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    /// The selected profile explicitly supports the capability.
    Supported,
    /// The selected profile explicitly does not support the capability.
    Unsupported,
    /// No trusted source supplied enough information.
    #[default]
    Unknown,
}

/// Profile-controlled target observations. Unknown devices never inherit another device's registers.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetCapabilities {
    /// Whether a stable target identity observation is available.
    pub target_identity: CapabilityState,
    /// Whether protection/security state can be observed safely.
    pub protection_state: CapabilityState,
    /// Whether reset reason can be observed safely.
    pub reset_reason: CapabilityState,
    /// Whether a device UID can be read. The default is unknown, not enabled.
    pub uid: CapabilityState,
}

/// Kind of evidence that contributed to a Flash profile.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSourceKind {
    /// In-memory MCP-lifecycle configuration.
    Session,
    /// Project-local `jlink-mcp.toml`.
    Project,
    /// Native IDE or pack metadata.
    ProjectNative,
    /// SEGGER installation or project metadata.
    Segger,
    /// Environment discovery.
    Environment,
}

/// One non-executable source record retained for provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSource {
    /// Source category used for precedence and display.
    pub kind: ProfileSourceKind,
    /// Bounded display locator such as a project-relative file path.
    pub locator: String,
}

/// Severity of a field-level profile conflict.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileConflictSeverity {
    /// The selected precedence is safe but remains visible.
    Trace,
    /// Programming must fail closed until the conflict is resolved.
    Blocking,
}

/// One field-level conflict between trusted configuration candidates.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConflict {
    /// Stable dotted field path.
    pub field: String,
    /// Whether the conflict blocks programming.
    pub severity: ProfileConflictSeverity,
    /// Selected or first observed value.
    pub selected: String,
    /// Conflicting value.
    pub rejected: String,
    /// Source locators that established the conflict.
    pub sources: Vec<String>,
}

/// Vendor-neutral Flash and RAM safety profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FlashProfile {
    /// Concrete selected target name.
    pub device: String,
    /// Equivalent device spellings observed from trusted sources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Declared target Flash regions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flash_regions: Vec<MemoryRegion>,
    /// Declared RAM regions that raw HSS is allowed to read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readable_ram: Vec<MemoryRegion>,
    /// Final work RAM selected for a Flash loader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loader_ram: Option<MemoryRegion>,
    /// Device-specific observations explicitly supported by the profile.
    pub capabilities: TargetCapabilities,
    /// Non-executable provenance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<ProfileSource>,
    /// Field-level conflicts retained after precedence resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<ProfileConflict>,
}

impl FlashProfile {
    /// Validates a vendor-neutral profile without guessing device-specific facts.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ConfigInvalid`] for a blank device, region kind mismatch,
    /// overlapping RAM/Flash regions, or Loader RAM outside readable RAM.
    pub fn validate(&self) -> Result<(), JlinkError> {
        if self.device.trim().is_empty() {
            return Err(profile_error("profile.device must not be empty"));
        }
        for region in &self.flash_regions {
            if region.kind() != MemoryRegionKind::Flash {
                return Err(profile_error(
                    "profile.flash_regions must contain only flash regions",
                ));
            }
        }
        for region in &self.readable_ram {
            if region.kind() != MemoryRegionKind::Ram {
                return Err(profile_error(
                    "profile.readable_ram must contain only RAM regions",
                ));
            }
        }
        let mut all = self.flash_regions.clone();
        all.extend(self.readable_ram.iter().copied());
        all.sort_by_key(|region| region.address());
        if all
            .windows(2)
            .any(|pair| pair[0].address().saturating_add(pair[0].length()) > pair[1].address())
        {
            return Err(profile_error("profile RAM and Flash regions overlap"));
        }
        if let Some(loader) = self.loader_ram
            && (loader.kind() != MemoryRegionKind::Ram
                || !self.contains_readable_ram(loader.address(), loader.length()))
        {
            return Err(profile_error(
                "profile.loader_ram must be fully inside readable RAM",
            ));
        }
        Ok(())
    }

    /// Returns whether a non-empty range is wholly inside one declared readable RAM region.
    #[must_use]
    pub fn contains_readable_ram(&self, address: u64, length: u64) -> bool {
        let Ok(range) = MemoryRange::new(address, length) else {
            return false;
        };
        self.readable_ram.iter().any(|region| {
            region.address() <= range.address()
                && range.end() <= region.address().saturating_add(region.length())
        })
    }

    /// Returns true when any high-risk profile conflict remains unresolved.
    #[must_use]
    pub fn has_blocking_conflict(&self) -> bool {
        self.conflicts
            .iter()
            .any(|conflict| conflict.severity == ProfileConflictSeverity::Blocking)
    }
}

/// Canonicalizes a target spelling for safe alias comparison without vendor-specific tables.
#[must_use]
pub fn canonical_device_name(device: &str) -> String {
    device
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_uppercase)
        .collect()
}

fn profile_error(message: &str) -> JlinkError {
    JlinkError::new(ErrorCode::ConfigInvalid, message, false).with_detail("field", json!("profile"))
}

#[cfg(test)]
mod tests {
    use super::{FlashProfile, TargetCapabilities, canonical_device_name};
    use crate::{MemoryRegion, MemoryRegionKind};

    #[test]
    fn canonical_name_ignores_only_punctuation_and_case() {
        assert_eq!(canonical_device_name("z20k-146_mc"), "Z20K146MC");
        assert_ne!(
            canonical_device_name("Z20K146M"),
            canonical_device_name("Z20K146MC")
        );
    }

    #[test]
    fn loader_ram_must_stay_inside_declared_readable_ram() {
        let profile = FlashProfile {
            device: "device".to_owned(),
            aliases: Vec::new(),
            flash_regions: vec![
                MemoryRegion::new(0, 0x1000, MemoryRegionKind::Flash).expect("flash"),
            ],
            readable_ram: vec![
                MemoryRegion::new(0x2000_0000, 0x1000, MemoryRegionKind::Ram).expect("RAM"),
            ],
            loader_ram: Some(
                MemoryRegion::new(0x2000_0ff0, 0x20, MemoryRegionKind::Ram).expect("loader"),
            ),
            capabilities: TargetCapabilities::default(),
            sources: Vec::new(),
            conflicts: Vec::new(),
        };
        assert!(profile.validate().is_err());
        assert!(profile.contains_readable_ram(0x2000_0000, 4));
        assert!(!profile.contains_readable_ram(0x1fff_ffff, 4));
    }
}
