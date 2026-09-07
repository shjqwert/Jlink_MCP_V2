//! Operation-specific static readiness, without opening a DLL or target.
use std::collections::BTreeMap;

use serde::Serialize;

use super::ConfigInspection;

/// Static prerequisites and explicitly unperformed checks for one operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationReadiness {
    /// True only when this operation has its required static configuration.
    /// This does not attest to DLL compatibility, hardware state or image identity.
    pub static_ready: bool,
    /// Missing configuration fields or unresolved blocking profile conflicts.
    pub missing: Vec<String>,
    /// Request-specific, file-content or live checks that remain unperformed.
    pub pending_checks: Vec<String>,
}

impl ConfigInspection {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn refresh_operation_readiness(&mut self) {
        let mut live = self.missing.clone();
        require(
            &mut live,
            "probe.serial",
            self.effective.contains_key("probe.serial"),
        );
        let profile = self.resolved.as_ref().map(|resolved| &resolved.profile);
        let mut flash = live.clone();
        require(
            &mut flash,
            "profile.loader_ram",
            profile.is_some_and(|value| value.loader_ram.is_some()),
        );
        require(
            &mut flash,
            "profile.conflicts",
            profile.is_none_or(|value| !value.has_blocking_conflict()),
        );
        let mut symbols = live.clone();
        require(
            &mut symbols,
            "symbols.elf",
            self.effective.contains_key("symbols.elf"),
        );
        let mut raw_hss = live.clone();
        require(
            &mut raw_hss,
            "profile.readable_ram",
            profile.is_some_and(|value| !value.readable_ram.is_empty()),
        );
        let mut dwarf_plan = self.missing.clone();
        require(
            &mut dwarf_plan,
            "symbols.elf",
            self.effective.contains_key("symbols.elf"),
        );
        let mut raw_plan = self.missing.clone();
        require(
            &mut raw_plan,
            "profile.readable_ram",
            profile.is_some_and(|value| !value.readable_ram.is_empty()),
        );

        let live_checks = "DLL identity and live target/session checks have not been performed";
        let image_checks = "image must be supplied by request or configuration; parsing and Flash range checks remain pending";
        let symbol_checks = "ELF/DWARF parsing and selector resolution remain pending";
        let identity_checks = "strong firmware identity is required; format and target readback checks remain pending";
        let hss_checks = format!(
            "run hss.plan with the intended selectors; combined sample payload must not exceed {} bytes (timestamp excluded)",
            jlink_domain::HSS_MAX_EXPANDED_SAMPLE_BYTES
        );
        let mut readiness = BTreeMap::new();
        insert(&mut readiness, "connect", live.clone(), &[live_checks]);
        insert(&mut readiness, "validate", live.clone(), &[live_checks]);
        insert(
            &mut readiness,
            "program.flash",
            flash.clone(),
            &[
                live_checks,
                image_checks,
                "loader RAM safety preflight remains pending",
            ],
        );
        insert(
            &mut readiness,
            "program.erase",
            flash,
            &[
                live_checks,
                "erase range and loader RAM safety preflight remain pending",
            ],
        );
        insert(
            &mut readiness,
            "program.verify",
            live.clone(),
            &[live_checks, image_checks],
        );
        insert(
            &mut readiness,
            "inspect.memory",
            live,
            &[live_checks, "address and read length checks remain pending"],
        );
        insert(
            &mut readiness,
            "inspect.variable",
            symbols.clone(),
            &[
                live_checks,
                symbol_checks,
                "weak identity permits read-only access with a warning, not proof of matching firmware",
            ],
        );
        insert(
            &mut readiness,
            "write.variable",
            symbols.clone(),
            &[live_checks, symbol_checks, identity_checks],
        );
        insert(
            &mut readiness,
            "hss.plan.dwarf",
            dwarf_plan,
            &[symbol_checks, identity_checks, &hss_checks],
        );
        insert(
            &mut readiness,
            "hss.plan.raw_address",
            raw_plan,
            &[
                &hss_checks,
                "explicit address/type/length/endianness and declared RAM checks remain pending; no DWARF identity is required",
            ],
        );
        insert(
            &mut readiness,
            "hss.start.dwarf",
            symbols,
            &[
                live_checks,
                symbol_checks,
                identity_checks,
                &hss_checks,
                "native HSS capability and short-window rate assessment remain pending",
            ],
        );
        insert(
            &mut readiness,
            "hss.start.raw_address",
            raw_hss,
            &[
                live_checks,
                &hss_checks,
                "device RAM and native HSS capability/rate checks remain pending; no DWARF identity is required",
            ],
        );

        // Retain legacy keys, but do not call Flash statically ready without RAM.
        // Precise callers should use the action-specific readiness map.
        self.operations.insert(
            "program".to_owned(),
            readiness["program.flash"].static_ready,
        );
        self.operations.insert(
            "hss".to_owned(),
            readiness["hss.start.dwarf"].static_ready
                || readiness["hss.start.raw_address"].static_ready,
        );
        self.readiness = readiness;
    }
}

fn require(missing: &mut Vec<String>, field: &str, present: bool) {
    if !present {
        missing.push(field.to_owned());
    }
}

fn insert(
    map: &mut BTreeMap<String, OperationReadiness>,
    action: &str,
    mut missing: Vec<String>,
    checks: &[&str],
) {
    missing.sort();
    missing.dedup();
    map.insert(
        action.to_owned(),
        OperationReadiness {
            static_ready: missing.is_empty(),
            missing,
            pending_checks: checks.iter().map(|check| (*check).to_owned()).collect(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ConfigFile, ConfigPaths, JlinkConfig, ProbeConfig, SymbolsConfig, TargetConfig,
        inspect_config,
    };
    use jlink_domain::{MemoryRegion, MemoryRegionKind, TargetInterface};
    use std::path::PathBuf;

    fn inspection() -> (tempfile::TempDir, ConfigInspection) {
        let root = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::new(
            root.path().join("project.toml"),
            root.path().join("user.toml"),
        );
        let config = ConfigFile {
            target: Some(TargetConfig {
                device: Some("S32K144".to_owned()),
                interface: Some(TargetInterface::Swd),
                speed_khz: Some(1_000),
            }),
            symbols: Some(SymbolsConfig {
                elf: Some(PathBuf::from("not-yet-built.out")),
            }),
            jlink: Some(JlinkConfig {
                dll_path: Some(PathBuf::from("not-loaded.dll")),
                version: Some("6.98a".to_owned()),
                sha256: Some("0".repeat(64)),
            }),
            ..ConfigFile::default()
        };
        std::fs::write(&paths.project, toml::to_string(&config).unwrap()).unwrap();
        let discovered = ConfigFile {
            probe: Some(ProbeConfig {
                serial: Some(260_106_173),
            }),
            ..ConfigFile::default()
        };
        let result = inspect_config(&ConfigFile::default(), &paths, &discovered).unwrap();
        (root, result)
    }

    #[test]
    fn readiness_missing_loader_blocks_flash_but_not_verify_or_reads() {
        let (_root, view) = inspection();
        assert!(!view.operations["program"]);
        assert!(!view.readiness["program.flash"].static_ready);
        assert!(
            view.readiness["program.flash"]
                .missing
                .contains(&"profile.loader_ram".to_owned())
        );
        assert!(!view.readiness["program.erase"].static_ready);
        assert!(view.readiness["program.verify"].static_ready);
        assert!(view.readiness["inspect.variable"].static_ready);
        assert!(
            !view.readiness["inspect.variable"]
                .pending_checks
                .iter()
                .any(|check| check.starts_with("strong firmware identity is required"))
        );
        assert!(
            view.readiness["write.variable"]
                .pending_checks
                .iter()
                .any(|check| check.starts_with("strong firmware identity is required"))
        );
        assert!(
            view.readiness["hss.plan.dwarf"]
                .pending_checks
                .iter()
                .any(|check| check.contains("40 bytes"))
        );
    }

    #[test]
    fn readiness_raw_hss_does_not_require_a_symbol_elf_and_refreshes_profile() {
        let (_root, mut view) = inspection();
        view.effective.remove("symbols.elf");
        let profile = &mut view.resolved.as_mut().unwrap().profile;
        let ram = MemoryRegion::new(0x2000_0000, 0x1000, MemoryRegionKind::Ram).unwrap();
        profile.readable_ram.push(ram);
        profile.loader_ram = Some(ram);
        view.refresh_operation_readiness();
        assert!(view.operations["program"]);
        assert!(view.operations["hss"]);
        assert!(view.readiness["hss.start.raw_address"].static_ready);
        assert!(!view.readiness["hss.start.dwarf"].static_ready);
        assert!(
            !view.readiness["hss.plan.raw_address"]
                .pending_checks
                .iter()
                .any(|check| check.starts_with("strong firmware identity is required"))
        );
        assert!(!view.readiness["program.flash"].pending_checks.is_empty());
    }
}
