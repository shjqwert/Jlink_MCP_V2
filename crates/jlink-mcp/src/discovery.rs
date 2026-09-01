//! Bounded, non-executable discovery of native IDE and debugger metadata.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
};

use jlink_domain::{
    ProfileConflict, ProfileConflictSeverity, TargetCapabilities, TargetInterface,
    canonical_device_name,
};
use serde::Serialize;

use crate::config::{ConfigFile, FlashProfileConfig, ProfileRegionConfig, TargetConfig};

const MAX_DISCOVERY_FILES: usize = 512;
const MAX_DISCOVERY_DEPTH: usize = 8;
const MAX_METADATA_BYTES: u64 = 2 * 1024 * 1024;

/// One field candidate retained for detailed provenance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveryValue {
    /// Stable dotted field path.
    pub field: String,
    /// Parsed value; never interpreted as an instruction.
    pub value: String,
    /// Project-relative path or explicit referenced metadata path.
    pub source: String,
    /// Adapter that parsed the value.
    pub adapter: String,
}

/// One unsupported or bounded discovery observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveryDiagnostic {
    /// Stable reason code.
    pub code: String,
    /// Source path safe for display.
    pub source: String,
    /// Concise explanation.
    pub detail: String,
}

/// Bounded project-native configuration candidates and their provenance.
#[derive(Clone, Debug, Default)]
pub struct ProjectDiscovery {
    /// Lowest-precedence layer passed into ordinary resolution.
    pub config: ConfigFile,
    /// Every parsed candidate, including overridden ones.
    pub provenance: Vec<DiscoveryValue>,
    /// High-risk disagreements between native sources.
    pub conflicts: Vec<ProfileConflict>,
    /// Unsupported adapters and discovery bounds.
    pub diagnostics: Vec<DiscoveryDiagnostic>,
}

/// Discovers supported project-native metadata without executing it.
///
/// The traversal is deterministic and bounded by file count, depth, and file size.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn discover_project(root: &Path) -> ProjectDiscovery {
    let mut discovery = ProjectDiscovery::default();
    let mut files = metadata_files(root, &mut discovery.diagnostics);
    files.sort_by_key(|path| (adapter_priority(path), display_path(root, path)));

    let mut devices = Vec::new();
    let mut interfaces = Vec::new();
    let mut speeds = Vec::new();
    let mut flash_regions = Vec::new();
    let mut referenced_boards = BTreeSet::new();
    for path in files {
        let Some(contents) = read_metadata(&path, &mut discovery.diagnostics) else {
            continue;
        };
        let source = display_path(root, &path);
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "xcl" => parse_xcl(
                &contents,
                &source,
                &mut devices,
                &mut interfaces,
                &mut speeds,
                &mut referenced_boards,
            ),
            "jlink" => {
                if let Some(device) = quoted_assignment(&contents, "Device") {
                    devices.push(candidate("target.device", device, &source, "segger-jlink"));
                }
            }
            "ewp" | "ewd" => {
                for option in ["OGChipSelectEditMenu", "GFPUDeviceSlave", "CDevice"] {
                    if let Some(device) = iar_option_state(&contents, option) {
                        let token = device.split_whitespace().next().unwrap_or_default();
                        if looks_like_concrete_device(token) {
                            devices.push(candidate("target.device", token, &source, "iar-xml"));
                        }
                    }
                }
            }
            "board" => parse_board_ranges(&contents, &source, &mut flash_regions),
            "uvprojx" | "pdsc" => parse_cmsis_metadata(
                &contents,
                &source,
                &mut devices,
                &mut flash_regions,
                &mut discovery.diagnostics,
            ),
            "flm" => discovery.diagnostics.push(DiscoveryDiagnostic {
                code: "external_loader_metadata_only".to_owned(),
                source,
                detail:
                    "CMSIS FLM is recorded as metadata and is not executed as a J-Link algorithm"
                        .to_owned(),
            }),
            "launch" => discovery.diagnostics.push(DiscoveryDiagnostic {
                code: "diagnostic_adapter".to_owned(),
                source,
                detail: "S32DS launch metadata is diagnostic-only in V1.1.0".to_owned(),
            }),
            _ => {}
        }
    }

    for board in referenced_boards {
        let path = PathBuf::from(&board);
        let Some(contents) = read_metadata(&path, &mut discovery.diagnostics) else {
            continue;
        };
        parse_board_ranges(&contents, &board, &mut flash_regions);
        discovery.provenance.push(DiscoveryValue {
            field: "profile.flash_loader_metadata".to_owned(),
            value: "metadata_only".to_owned(),
            source: board,
            adapter: "iar-board".to_owned(),
        });
    }

    discovery.provenance.extend(devices.iter().cloned());
    discovery.provenance.extend(interfaces.iter().cloned());
    discovery.provenance.extend(speeds.iter().cloned());
    let selected_device = distinct_first(&devices);
    discovery.conflicts = device_conflicts(&devices);
    let selected_interface =
        distinct_first(&interfaces).and_then(|value| match value.value.as_str() {
            "swd" => Some(TargetInterface::Swd),
            "jtag" => Some(TargetInterface::Jtag),
            _ => None,
        });
    let selected_speed = distinct_first(&speeds).and_then(|value| value.value.parse::<u32>().ok());
    if selected_device.is_some() || selected_interface.is_some() || selected_speed.is_some() {
        discovery.config.target = Some(TargetConfig {
            device: selected_device.map(|value| value.value),
            interface: selected_interface,
            speed_khz: selected_speed,
        });
    }
    if !flash_regions.is_empty() {
        flash_regions.sort_by_key(|region| (region.address, region.length));
        flash_regions.dedup();
        discovery.config.profile = Some(FlashProfileConfig {
            flash_regions,
            readable_ram: Vec::new(),
            loader_ram: None,
            capabilities: TargetCapabilities::default(),
        });
    }
    discovery
}

fn metadata_files(root: &Path, diagnostics: &mut Vec<DiscoveryDiagnostic>) -> Vec<PathBuf> {
    let mut output = Vec::new();
    let mut pending = VecDeque::from([(root.to_path_buf(), 0_usize)]);
    while let Some((directory, depth)) = pending.pop_front() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if depth < MAX_DISCOVERY_DEPTH
                    && !matches!(name.as_str(), ".git" | "target" | ".jlink-mcp")
                {
                    pending.push_back((path, depth + 1));
                }
                continue;
            }
            if !kind.is_file() || !is_supported_metadata(&path) {
                continue;
            }
            if output.len() == MAX_DISCOVERY_FILES {
                diagnostics.push(DiscoveryDiagnostic {
                    code: "discovery_file_limit".to_owned(),
                    source: root.display().to_string(),
                    detail: format!("project discovery stopped after {MAX_DISCOVERY_FILES} files"),
                });
                return output;
            }
            output.push(path);
        }
    }
    output
}

fn is_supported_metadata(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "ewp"
            | "ewd"
            | "xcl"
            | "jlink"
            | "board"
            | "jflash"
            | "uvprojx"
            | "pdsc"
            | "flm"
            | "launch"
    )
}

fn adapter_priority(path: &Path) -> u8 {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "xcl" => 0,
        "jlink" => 1,
        "ewp" | "ewd" => 2,
        "uvprojx" | "pdsc" => 3,
        _ => 4,
    }
}

fn read_metadata(path: &Path, diagnostics: &mut Vec<DiscoveryDiagnostic>) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > MAX_METADATA_BYTES {
        diagnostics.push(DiscoveryDiagnostic {
            code: "metadata_too_large".to_owned(),
            source: path.display().to_string(),
            detail: format!("metadata exceeds {MAX_METADATA_BYTES} bytes"),
        });
        return None;
    }
    fs::read_to_string(path).ok()
}

fn parse_xcl(
    contents: &str,
    source: &str,
    devices: &mut Vec<DiscoveryValue>,
    interfaces: &mut Vec<DiscoveryValue>,
    speeds: &mut Vec<DiscoveryValue>,
    referenced_boards: &mut BTreeSet<String>,
) {
    if let Some(value) = command_value(contents, "--device=") {
        devices.push(candidate("target.device", value, source, "iar-xcl"));
    }
    if let Some(value) = command_value(contents, "--drv_interface=") {
        let interface = value.to_ascii_lowercase();
        if matches!(interface.as_str(), "swd" | "jtag") {
            interfaces.push(candidate("target.interface", &interface, source, "iar-xcl"));
        }
    }
    if let Some(value) = command_value(contents, "--jlink_initial_speed=")
        && value.parse::<u32>().is_ok()
    {
        speeds.push(candidate("target.speed_khz", value, source, "iar-xcl"));
    }
    if let Some(value) = command_value(contents, "--flash_loader=") {
        referenced_boards.insert(value.to_owned());
    }
}

fn parse_board_ranges(contents: &str, source: &str, output: &mut Vec<ProfileRegionConfig>) {
    for line in contents.lines() {
        let Some(value) = between(line, "<range>", "</range>") else {
            continue;
        };
        let mut parts = value.split_whitespace();
        if parts.next() != Some("CODE") {
            continue;
        }
        let (Some(start), Some(end)) = (
            parts.next().and_then(parse_hex),
            parts.next().and_then(parse_hex),
        ) else {
            continue;
        };
        if let Some(length) = end
            .checked_sub(start)
            .and_then(|value| value.checked_add(1))
        {
            output.push(ProfileRegionConfig {
                address: start,
                length,
            });
        }
    }
    let _ = source;
}

fn parse_cmsis_metadata(
    contents: &str,
    source: &str,
    devices: &mut Vec<DiscoveryValue>,
    flash_regions: &mut Vec<ProfileRegionConfig>,
    diagnostics: &mut Vec<DiscoveryDiagnostic>,
) {
    if let Some(device) = between(contents, "<Device>", "</Device>") {
        devices.push(candidate(
            "target.device",
            device.trim(),
            source,
            "cmsis-keil",
        ));
    }
    for line in contents.lines().filter(|line| line.contains("<memory")) {
        let Some(start) = xml_attribute(line, "start").and_then(parse_hex) else {
            continue;
        };
        let Some(size) = xml_attribute(line, "size").and_then(parse_hex) else {
            continue;
        };
        let id = xml_attribute(line, "id")
            .unwrap_or_default()
            .to_ascii_uppercase();
        if id.contains("ROM") || id.contains("FLASH") {
            flash_regions.push(ProfileRegionConfig {
                address: start,
                length: size,
            });
        }
    }
    if contents.contains("<algorithm") {
        diagnostics.push(DiscoveryDiagnostic {
            code: "external_loader_metadata_only".to_owned(),
            source: source.to_owned(),
            detail: "CMSIS-Pack algorithm metadata is not executed as a J-Link algorithm"
                .to_owned(),
        });
    }
}

fn device_conflicts(devices: &[DiscoveryValue]) -> Vec<ProfileConflict> {
    let mut by_name = BTreeMap::<String, Vec<&DiscoveryValue>>::new();
    for value in devices {
        by_name
            .entry(canonical_device_name(&value.value))
            .or_default()
            .push(value);
    }
    let Some(first) = devices.first() else {
        return Vec::new();
    };
    let selected_name = canonical_device_name(&first.value);
    let selected_sources = by_name
        .get(&selected_name)
        .expect("first candidate was inserted");
    let selected = selected_sources[0].value.clone();
    by_name
        .iter()
        .filter(|(name, _)| **name != selected_name)
        .map(|(_, values)| ProfileConflict {
            field: "target.device".to_owned(),
            severity: ProfileConflictSeverity::Blocking,
            selected: selected.clone(),
            rejected: values[0].value.clone(),
            sources: selected_sources
                .iter()
                .chain(values.iter())
                .map(|value| value.source.clone())
                .collect(),
        })
        .collect()
}

fn distinct_first(values: &[DiscoveryValue]) -> Option<DiscoveryValue> {
    values.first().cloned()
}

fn candidate(field: &str, value: &str, source: &str, adapter: &str) -> DiscoveryValue {
    DiscoveryValue {
        field: field.to_owned(),
        value: value.trim_matches(['"', ' ', '\t']).to_owned(),
        source: source.to_owned(),
        adapter: adapter.to_owned(),
    }
}

fn command_value<'a>(contents: &'a str, name: &str) -> Option<&'a str> {
    let tail = contents.split_once(name)?.1;
    Some(
        tail.trim_start_matches(['"', ' '])
            .split(['"', '\r', '\n'])
            .next()?
            .trim(),
    )
}

fn looks_like_concrete_device(value: &str) -> bool {
    let canonical = canonical_device_name(value);
    canonical.chars().any(|value| value.is_ascii_alphabetic())
        && canonical.chars().any(|value| value.is_ascii_digit())
        && !matches!(canonical.as_str(), "ARM7" | "ARM9" | "ARM11")
}

fn quoted_assignment<'a>(contents: &'a str, name: &str) -> Option<&'a str> {
    let tail = contents.split_once(&format!("{name}=\""))?.1;
    tail.split('"').next()
}

fn iar_option_state<'a>(contents: &'a str, option: &str) -> Option<&'a str> {
    let tail = contents.split_once(&format!("<name>{option}</name>"))?.1;
    between(tail, "<state>", "</state>").map(str::trim)
}

fn xml_attribute<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let tail = line.split_once(&format!("{name}=\""))?.1;
    tail.split('"').next()
}

fn between<'a>(value: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let tail = value.split_once(start)?.1;
    Some(tail.split_once(end)?.0)
}

fn parse_hex(value: &str) -> Option<u64> {
    u64::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok()
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::discover_project;
    use std::fs;

    #[test]
    fn discovers_iar_and_segger_candidates_without_executing_metadata() {
        let root = tempfile::tempdir().expect("temp project");
        fs::write(
            root.path().join("app.xcl"),
            "\"--device=Z20K146M\"\n\"--jlink_initial_speed=1000\"\n\"--drv_interface=SWD\"",
        )
        .expect("xcl");
        fs::write(root.path().join("app.jlink"), "Device=\"Z20K146MC\"").expect("jlink");
        let discovered = discover_project(root.path());
        let target = discovered.config.target.expect("target candidates");
        assert_eq!(target.device.as_deref(), Some("Z20K146M"));
        assert_eq!(target.speed_khz, Some(1000));
        assert_eq!(discovered.conflicts.len(), 1);
        assert!(
            discovered
                .provenance
                .iter()
                .any(|value| value.value == "Z20K146MC")
        );
    }

    #[test]
    fn parses_board_flash_ranges_as_metadata_only() {
        let root = tempfile::tempdir().expect("temp project");
        fs::write(
            root.path().join("device.board"),
            "<flash_board><range>CODE 0x0 0xffff</range></flash_board>",
        )
        .expect("board");
        let discovered = discover_project(root.path());
        let region = discovered.config.profile.expect("profile").flash_regions[0];
        assert_eq!(region.address, 0);
        assert_eq!(region.length, 0x1_0000);
    }
}
