//! Layered, validated configuration for the local MCP process.

use std::{
    collections::BTreeMap,
    env,
    ffi::c_void,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    ptr,
    time::{SystemTime, UNIX_EPOCH},
};

pub use jlink_capture::DEFAULT_CAPTURE_MAX_BYTES;
use jlink_domain::{
    ErrorCode, FlashProfile, JlinkError, MemoryRegion, MemoryRegionKind, ProfileSource,
    ProfileSourceKind, TargetCapabilities, TargetInterface,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The source from which a resolved field was selected.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    /// A value supplied directly by the current request.
    Request,
    /// A value retained only for the current MCP process lifecycle.
    Session,
    /// A value supplied by the per-user configuration.
    User,
    /// A value supplied by the project configuration.
    Project,
    /// A value discovered from the local environment.
    Discovered,
    /// A safe built-in default.
    Default,
}

/// A resolved value together with its provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedField<T> {
    /// The selected value.
    pub value: T,
    /// The layer which supplied the selected value.
    pub source: ConfigSource,
}

/// A partial target configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TargetConfig {
    /// A concrete device name such as `S32K144`.
    pub device: Option<String>,
    /// The physical debug interface.
    pub interface: Option<TargetInterface>,
    /// The debug clock in kHz.
    pub speed_khz: Option<u32>,
}

/// A partial symbols configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SymbolsConfig {
    /// The ELF image used for symbol and DWARF lookup.
    pub elf: Option<PathBuf>,
}

/// A partial firmware configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FirmwareConfig {
    /// An optional firmware image used by programming operations.
    pub image: Option<PathBuf>,
}

/// A partial J-Link DLL identity configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JlinkConfig {
    /// The absolute path to the J-Link DLL.
    pub dll_path: Option<PathBuf>,
    /// The expected Windows file version, for example `6.98a`.
    pub version: Option<String>,
    /// The expected SHA-256 digest in hexadecimal form.
    pub sha256: Option<String>,
}

/// A partial probe configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProbeConfig {
    /// The J-Link probe serial number.
    pub serial: Option<u32>,
}

/// A partial capture configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptureConfig {
    /// The maximum capture-store size in bytes.
    pub max_bytes: Option<u64>,
}

/// One configured target address range.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRegionConfig {
    /// First byte address.
    pub address: u64,
    /// Non-zero length in bytes.
    pub length: u64,
}

/// Optional vendor-neutral Flash/RAM safety metadata.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FlashProfileConfig {
    /// Declared Flash ranges.
    pub flash_regions: Vec<ProfileRegionConfig>,
    /// RAM ranges allowed for raw HSS reads.
    pub readable_ram: Vec<ProfileRegionConfig>,
    /// Final work RAM selected for the Flash loader.
    pub loader_ram: Option<ProfileRegionConfig>,
    /// Explicitly supported target observations.
    pub capabilities: TargetCapabilities,
}

/// A partial configuration layer. Missing nested tables and fields are preserved.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    /// Target connection fields.
    pub target: Option<TargetConfig>,
    /// Symbol lookup fields.
    pub symbols: Option<SymbolsConfig>,
    /// Firmware programming fields.
    pub firmware: Option<FirmwareConfig>,
    /// J-Link DLL identity fields.
    pub jlink: Option<JlinkConfig>,
    /// Probe fields.
    pub probe: Option<ProbeConfig>,
    /// Capture-store fields.
    pub capture: Option<CaptureConfig>,
    /// Vendor-neutral Flash/RAM safety metadata.
    pub profile: Option<FlashProfileConfig>,
}

/// Paths for the project and user configuration layers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigPaths {
    /// Project-local configuration path.
    pub project: PathBuf,
    /// Per-user configuration path.
    pub user: PathBuf,
}

impl ConfigPaths {
    /// Creates paths for an explicit project and user configuration.
    #[must_use]
    pub fn new(project: impl Into<PathBuf>, user: impl Into<PathBuf>) -> Self {
        Self {
            project: project.into(),
            user: user.into(),
        }
    }
}

impl Default for ConfigPaths {
    fn default() -> Self {
        let project = PathBuf::from("jlink-mcp.toml");
        let user = env::var_os("LOCALAPPDATA")
            .map_or_else(env::temp_dir, PathBuf::from)
            .join("jlink-mcp")
            .join("config.toml");
        Self { project, user }
    }
}

/// The configuration scope targeted by a partial update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigScope {
    /// The current MCP lifecycle only; never persisted.
    Session,
    /// The project-local layer.
    Project,
    /// The per-user layer.
    User,
}

/// Session state which can make configuration mutation unsafe.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConfigSetState {
    /// Whether a worker connection is currently owned.
    pub connected: bool,
    /// Whether a capture is currently active.
    pub capture_active: bool,
}

/// Resolved target fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTarget {
    /// Concrete target device.
    pub device: ResolvedField<String>,
    /// Selected debug interface.
    pub interface: ResolvedField<TargetInterface>,
    /// Debug clock in kHz.
    pub speed_khz: ResolvedField<u32>,
}

/// Resolved symbol fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSymbols {
    /// ELF path and its source, if configured.
    pub elf: Option<ResolvedField<PathBuf>>,
}

/// Resolved firmware fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFirmware {
    /// Firmware path and its source, if configured.
    pub image: Option<ResolvedField<PathBuf>>,
}

/// Resolved J-Link DLL identity fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedJlink {
    /// DLL path and its source.
    pub dll_path: ResolvedField<PathBuf>,
    /// Expected file version and its source.
    pub version: ResolvedField<String>,
    /// Expected SHA-256 digest and its source.
    pub sha256: ResolvedField<String>,
}

/// Resolved probe fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedProbe {
    /// Probe serial and its source, if configured.
    pub serial: Option<ResolvedField<u32>>,
}

/// Resolved capture fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCapture {
    /// Maximum capture-store size and its source.
    pub max_bytes: ResolvedField<u64>,
}

/// The fully resolved configuration used by runtime operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedConfig {
    /// Resolved target fields.
    pub target: ResolvedTarget,
    /// Resolved symbol fields.
    pub symbols: ResolvedSymbols,
    /// Resolved firmware fields.
    pub firmware: ResolvedFirmware,
    /// Resolved DLL identity fields.
    pub jlink: ResolvedJlink,
    /// Resolved probe fields.
    pub probe: ResolvedProbe,
    /// Resolved capture fields.
    pub capture: ResolvedCapture,
    /// Selected vendor-neutral safety profile.
    pub profile: FlashProfile,
}

/// A discovered layer. It is intentionally explicit so discovery cannot silently
/// override a configured value.
pub type DiscoveredConfig = ConfigFile;

/// Partial, hardware-free configuration view used by `config_get`.
#[derive(Clone, Debug, Serialize)]
pub struct ConfigInspection {
    /// Every currently selected field, even when other required fields are absent.
    pub effective: BTreeMap<String, Value>,
    /// Selected source for each effective field.
    pub sources: BTreeMap<String, ConfigSource>,
    /// Required fields still absent.
    pub missing: Vec<String>,
    /// Whether each public operation has enough static configuration to proceed.
    pub operations: BTreeMap<String, bool>,
    /// Same-directory x64 selection performed from a configured 32-bit candidate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dll_selection: Option<DllSelection>,
    /// Fully resolved configuration when no required field is missing.
    #[serde(skip)]
    pub resolved: Option<ResolvedConfig>,
}

/// Auditable DLL path normalization from an x86 installation candidate to x64.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DllSelection {
    /// Configured candidate path.
    pub configured_path: PathBuf,
    /// Selected x64 path.
    pub selected_path: PathBuf,
    /// Stable reason code.
    pub reason: String,
}

/// Builds a partial configuration view without opening a DLL, Worker, or target.
///
/// # Errors
///
/// Returns [`ErrorCode::ConfigInvalid`] when a local layer cannot be read or contains
/// invalid fields. Missing fields are reported in the returned inspection instead.
#[allow(clippy::too_many_lines)]
pub fn inspect_config(
    session: &ConfigFile,
    paths: &ConfigPaths,
    discovered: &DiscoveredConfig,
) -> Result<ConfigInspection, JlinkError> {
    validate_layer_scope(session, ConfigScope::Session)?;
    validate_partial(session)?;
    let user = read_config_file(&paths.user)?;
    let project = read_config_file(&paths.project)?;
    if let Some(user) = &user {
        validate_layer_scope(user, ConfigScope::User)?;
        validate_partial(user)?;
    }
    if let Some(project) = &project {
        validate_layer_scope(project, ConfigScope::Project)?;
        validate_partial(project)?;
    }
    validate_partial(discovered)?;

    let mut effective = BTreeMap::new();
    let mut sources = BTreeMap::new();
    macro_rules! insert_field {
        ($name:literal, $value:expr) => {
            if let Some(field) = $value {
                effective.insert(
                    $name.to_owned(),
                    serde_json::to_value(&field.value).map_err(|error| {
                        config_error(format!("cannot serialize {}: {error}", $name))
                    })?,
                );
                sources.insert($name.to_owned(), field.source);
            }
        };
    }
    insert_field!(
        "target.device",
        pick(
            None::<&String>,
            session
                .target
                .as_ref()
                .and_then(|value| value.device.as_ref()),
            None,
            project
                .as_ref()
                .and_then(|value| value.target.as_ref())
                .and_then(|value| value.device.as_ref()),
            discovered
                .target
                .as_ref()
                .and_then(|value| value.device.as_ref())
        )
    );
    insert_field!(
        "target.interface",
        pick_copy(
            None::<TargetInterface>,
            session.target.as_ref().and_then(|value| value.interface),
            None,
            project
                .as_ref()
                .and_then(|value| value.target.as_ref())
                .and_then(|value| value.interface),
            discovered.target.as_ref().and_then(|value| value.interface)
        )
    );
    insert_field!(
        "target.speed_khz",
        pick_copy(
            None::<u32>,
            session.target.as_ref().and_then(|value| value.speed_khz),
            None,
            project
                .as_ref()
                .and_then(|value| value.target.as_ref())
                .and_then(|value| value.speed_khz),
            discovered.target.as_ref().and_then(|value| value.speed_khz)
        )
    );
    insert_field!(
        "symbols.elf",
        pick(
            None::<&PathBuf>,
            session
                .symbols
                .as_ref()
                .and_then(|value| value.elf.as_ref()),
            None,
            project
                .as_ref()
                .and_then(|value| value.symbols.as_ref())
                .and_then(|value| value.elf.as_ref()),
            discovered
                .symbols
                .as_ref()
                .and_then(|value| value.elf.as_ref())
        )
    );
    insert_field!(
        "firmware.image",
        pick(
            None::<&PathBuf>,
            session
                .firmware
                .as_ref()
                .and_then(|value| value.image.as_ref()),
            None,
            project
                .as_ref()
                .and_then(|value| value.firmware.as_ref())
                .and_then(|value| value.image.as_ref()),
            discovered
                .firmware
                .as_ref()
                .and_then(|value| value.image.as_ref())
        )
    );
    insert_field!(
        "jlink.dll_path",
        pick(
            None::<&PathBuf>,
            session
                .jlink
                .as_ref()
                .and_then(|value| value.dll_path.as_ref()),
            None,
            project
                .as_ref()
                .and_then(|value| value.jlink.as_ref())
                .and_then(|value| value.dll_path.as_ref()),
            discovered
                .jlink
                .as_ref()
                .and_then(|value| value.dll_path.as_ref())
        )
    );
    insert_field!(
        "jlink.dll_version",
        pick(
            None::<&String>,
            session
                .jlink
                .as_ref()
                .and_then(|value| value.version.as_ref()),
            None,
            project
                .as_ref()
                .and_then(|value| value.jlink.as_ref())
                .and_then(|value| value.version.as_ref()),
            discovered
                .jlink
                .as_ref()
                .and_then(|value| value.version.as_ref())
        )
    );
    insert_field!(
        "jlink.dll_sha256",
        pick(
            None::<&String>,
            session
                .jlink
                .as_ref()
                .and_then(|value| value.sha256.as_ref()),
            None,
            project
                .as_ref()
                .and_then(|value| value.jlink.as_ref())
                .and_then(|value| value.sha256.as_ref()),
            discovered
                .jlink
                .as_ref()
                .and_then(|value| value.sha256.as_ref())
        )
    );
    insert_field!(
        "probe.serial",
        pick_copy(
            None::<u32>,
            None,
            user.as_ref()
                .and_then(|value| value.probe.as_ref())
                .and_then(|value| value.serial),
            None,
            discovered.probe.as_ref().and_then(|value| value.serial)
        )
    );
    let capture = pick_copy(
        None::<u64>,
        session.capture.as_ref().and_then(|value| value.max_bytes),
        None,
        project
            .as_ref()
            .and_then(|value| value.capture.as_ref())
            .and_then(|value| value.max_bytes),
        discovered
            .capture
            .as_ref()
            .and_then(|value| value.max_bytes),
    )
    .unwrap_or_else(|| resolved(DEFAULT_CAPTURE_MAX_BYTES, ConfigSource::Default));
    effective.insert("capture.max_bytes".to_owned(), json!(capture.value));
    sources.insert("capture.max_bytes".to_owned(), capture.source);

    let dll_selection = effective
        .get("jlink.dll_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .map(|configured| {
            select_x64_dll_candidate(&configured).map(|selected| {
                if selected == configured {
                    None
                } else {
                    effective.insert(
                        "jlink.dll_path".to_owned(),
                        json!(selected.to_string_lossy()),
                    );
                    Some(DllSelection {
                        configured_path: configured,
                        selected_path: selected,
                        reason: "configured_x86_same_install_x64".to_owned(),
                    })
                }
            })
        })
        .transpose()?
        .flatten();
    let required = [
        "target.device",
        "target.interface",
        "target.speed_khz",
        "jlink.dll_path",
        "jlink.dll_version",
        "jlink.dll_sha256",
    ];
    let missing = required
        .iter()
        .filter(|name| !effective.contains_key(**name))
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let resolved = if missing.is_empty() {
        Some(resolve_layers_with_session(
            &ConfigFile::default(),
            Some(session),
            user.as_ref(),
            project.as_ref(),
            Some(discovered),
        )?)
    } else {
        None
    };
    let static_ready = resolved.is_some();
    let probe_ready = effective.contains_key("probe.serial");
    let symbols_ready = effective.contains_key("symbols.elf");
    let mut operations = BTreeMap::new();
    operations.insert("config_get".to_owned(), true);
    operations.insert("connect".to_owned(), static_ready && probe_ready);
    operations.insert("validate".to_owned(), static_ready && probe_ready);
    operations.insert("program".to_owned(), static_ready && probe_ready);
    operations.insert("raw_debug".to_owned(), static_ready && probe_ready);
    operations.insert(
        "symbol_debug".to_owned(),
        static_ready && probe_ready && symbols_ready,
    );
    operations.insert(
        "hss".to_owned(),
        static_ready && probe_ready && symbols_ready,
    );
    Ok(ConfigInspection {
        effective,
        sources,
        missing,
        operations,
        dll_selection,
        resolved,
    })
}

/// Resolves request, user, project, discovery, and safe-default layers.
///
/// # Errors
///
/// Returns [`ErrorCode::ConfigInvalid`] when a layer cannot be read or the
/// effective configuration is incomplete or invalid.
pub fn resolve_config(
    request: &ConfigFile,
    paths: &ConfigPaths,
    discovered: &DiscoveredConfig,
) -> Result<ResolvedConfig, JlinkError> {
    resolve_config_with_session(request, &ConfigFile::default(), paths, discovered)
}

/// Resolves a request with one memory-only session layer.
///
/// # Errors
///
/// Returns [`ErrorCode::ConfigInvalid`] when a layer cannot be read or the
/// effective configuration is incomplete or invalid.
pub fn resolve_config_with_session(
    request: &ConfigFile,
    session: &ConfigFile,
    paths: &ConfigPaths,
    discovered: &DiscoveredConfig,
) -> Result<ResolvedConfig, JlinkError> {
    let user = read_config_file(&paths.user)?;
    let project = read_config_file(&paths.project)?;
    resolve_layers_with_session(
        request,
        Some(session),
        user.as_ref(),
        project.as_ref(),
        Some(discovered),
    )
}

/// Resolves already-loaded layers, which is useful for deterministic tests.
///
/// # Errors
///
/// Returns [`ErrorCode::ConfigInvalid`] when the effective configuration is
/// incomplete or invalid.
#[allow(clippy::too_many_lines)]
pub fn resolve_layers(
    request: &ConfigFile,
    user: Option<&ConfigFile>,
    project: Option<&ConfigFile>,
    discovered: Option<&ConfigFile>,
) -> Result<ResolvedConfig, JlinkError> {
    resolve_layers_with_session(request, None, user, project, discovered)
}

/// Resolves already-loaded layers including an optional memory-only session layer.
///
/// # Errors
///
/// Returns [`ErrorCode::ConfigInvalid`] when the effective configuration is
/// incomplete or invalid.
#[allow(clippy::too_many_lines)]
pub fn resolve_layers_with_session(
    request: &ConfigFile,
    session: Option<&ConfigFile>,
    user: Option<&ConfigFile>,
    project: Option<&ConfigFile>,
    discovered: Option<&ConfigFile>,
) -> Result<ResolvedConfig, JlinkError> {
    if let Some(session_layer) = session {
        validate_layer_scope(session_layer, ConfigScope::Session)?;
    }
    if let Some(user_layer) = user {
        validate_layer_scope(user_layer, ConfigScope::User)?;
    }
    if let Some(project_layer) = project {
        validate_layer_scope(project_layer, ConfigScope::Project)?;
    }
    let target = ResolvedTarget {
        device: required(
            "target.device",
            pick(
                request.target.as_ref().and_then(|v| v.device.as_ref()),
                session
                    .and_then(|v| v.target.as_ref())
                    .and_then(|v| v.device.as_ref()),
                user.and_then(|v| v.target.as_ref())
                    .and_then(|v| v.device.as_ref()),
                project
                    .and_then(|v| v.target.as_ref())
                    .and_then(|v| v.device.as_ref()),
                discovered
                    .and_then(|v| v.target.as_ref())
                    .and_then(|v| v.device.as_ref()),
            ),
        )?,
        interface: required(
            "target.interface",
            pick_copy(
                request.target.as_ref().and_then(|v| v.interface),
                session
                    .and_then(|v| v.target.as_ref())
                    .and_then(|v| v.interface),
                user.and_then(|v| v.target.as_ref())
                    .and_then(|v| v.interface),
                project
                    .and_then(|v| v.target.as_ref())
                    .and_then(|v| v.interface),
                discovered
                    .and_then(|v| v.target.as_ref())
                    .and_then(|v| v.interface),
            ),
        )?,
        speed_khz: required(
            "target.speed_khz",
            pick_copy(
                request.target.as_ref().and_then(|v| v.speed_khz),
                session
                    .and_then(|v| v.target.as_ref())
                    .and_then(|v| v.speed_khz),
                user.and_then(|v| v.target.as_ref())
                    .and_then(|v| v.speed_khz),
                project
                    .and_then(|v| v.target.as_ref())
                    .and_then(|v| v.speed_khz),
                discovered
                    .and_then(|v| v.target.as_ref())
                    .and_then(|v| v.speed_khz),
            ),
        )?,
    };
    validate_device(&target.device.value)?;
    if target.speed_khz.value == 0 {
        return Err(config_error("target.speed_khz must be positive"));
    }

    let symbols = ResolvedSymbols {
        elf: pick(
            request.symbols.as_ref().and_then(|v| v.elf.as_ref()),
            session
                .and_then(|v| v.symbols.as_ref())
                .and_then(|v| v.elf.as_ref()),
            user.and_then(|v| v.symbols.as_ref())
                .and_then(|v| v.elf.as_ref()),
            project
                .and_then(|v| v.symbols.as_ref())
                .and_then(|v| v.elf.as_ref()),
            discovered
                .and_then(|v| v.symbols.as_ref())
                .and_then(|v| v.elf.as_ref()),
        ),
    };
    let firmware = ResolvedFirmware {
        image: pick(
            request.firmware.as_ref().and_then(|v| v.image.as_ref()),
            session
                .and_then(|v| v.firmware.as_ref())
                .and_then(|v| v.image.as_ref()),
            user.and_then(|v| v.firmware.as_ref())
                .and_then(|v| v.image.as_ref()),
            project
                .and_then(|v| v.firmware.as_ref())
                .and_then(|v| v.image.as_ref()),
            discovered
                .and_then(|v| v.firmware.as_ref())
                .and_then(|v| v.image.as_ref()),
        ),
    };
    let mut jlink = ResolvedJlink {
        dll_path: required(
            "jlink.dll_path",
            pick(
                request.jlink.as_ref().and_then(|v| v.dll_path.as_ref()),
                session
                    .and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.dll_path.as_ref()),
                user.and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.dll_path.as_ref()),
                project
                    .and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.dll_path.as_ref()),
                discovered
                    .and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.dll_path.as_ref()),
            ),
        )?,
        version: required(
            "jlink.version",
            pick(
                request.jlink.as_ref().and_then(|v| v.version.as_ref()),
                session
                    .and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.version.as_ref()),
                user.and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.version.as_ref()),
                project
                    .and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.version.as_ref()),
                discovered
                    .and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.version.as_ref()),
            ),
        )?,
        sha256: required(
            "jlink.sha256",
            pick(
                request.jlink.as_ref().and_then(|v| v.sha256.as_ref()),
                session
                    .and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.sha256.as_ref()),
                user.and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.sha256.as_ref()),
                project
                    .and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.sha256.as_ref()),
                discovered
                    .and_then(|v| v.jlink.as_ref())
                    .and_then(|v| v.sha256.as_ref()),
            ),
        )?,
    };
    jlink.dll_path.value = select_x64_dll_candidate(&jlink.dll_path.value)?;
    validate_jlink_fields(&jlink)?;

    let probe = ResolvedProbe {
        serial: pick_copy(
            request.probe.as_ref().and_then(|v| v.serial),
            session
                .and_then(|v| v.probe.as_ref())
                .and_then(|v| v.serial),
            user.and_then(|v| v.probe.as_ref()).and_then(|v| v.serial),
            project
                .and_then(|v| v.probe.as_ref())
                .and_then(|v| v.serial),
            discovered
                .and_then(|v| v.probe.as_ref())
                .and_then(|v| v.serial),
        ),
    };
    let max_bytes = pick_copy(
        request.capture.as_ref().and_then(|v| v.max_bytes),
        session
            .and_then(|v| v.capture.as_ref())
            .and_then(|v| v.max_bytes),
        user.and_then(|v| v.capture.as_ref())
            .and_then(|v| v.max_bytes),
        project
            .and_then(|v| v.capture.as_ref())
            .and_then(|v| v.max_bytes),
        discovered
            .and_then(|v| v.capture.as_ref())
            .and_then(|v| v.max_bytes),
    )
    .map_or_else(
        || {
            Ok(ResolvedField {
                value: DEFAULT_CAPTURE_MAX_BYTES,
                source: ConfigSource::Default,
            })
        },
        Ok,
    )?;
    let capture = ResolvedCapture { max_bytes };
    if capture.max_bytes.value == 0 {
        return Err(config_error("capture.max_bytes must be positive"));
    }

    let profile_config = pick(
        request.profile.as_ref(),
        session.and_then(|value| value.profile.as_ref()),
        user.and_then(|value| value.profile.as_ref()),
        project.and_then(|value| value.profile.as_ref()),
        discovered.and_then(|value| value.profile.as_ref()),
    );
    let profile = build_flash_profile(&target.device, profile_config.as_ref())?;

    Ok(ResolvedConfig {
        target,
        symbols,
        firmware,
        jlink,
        probe,
        capture,
        profile,
    })
}

/// Applies a partial update to one scope and atomically persists the result.
///
/// # Errors
///
/// Returns [`ErrorCode::OperationConflict`] while connected or capturing, or
/// [`ErrorCode::ConfigInvalid`] when the patch cannot be validated or stored.
pub fn config_set(
    paths: &ConfigPaths,
    scope: ConfigScope,
    patch: &ConfigFile,
    state: ConfigSetState,
) -> Result<(), JlinkError> {
    if state.connected || state.capture_active {
        return Err(JlinkError::new(
            ErrorCode::OperationConflict,
            "configuration cannot change while connected or capturing",
            true,
        ));
    }
    validate_layer_scope(patch, scope)?;
    validate_partial(patch)?;
    let config_path = match scope {
        ConfigScope::Session => {
            return Err(config_error(
                "session scope is memory-only and must be applied by the MCP runtime",
            ));
        }
        ConfigScope::Project => &paths.project,
        ConfigScope::User => &paths.user,
    };
    let mut current = read_config_file(config_path)?.unwrap_or_default();
    merge_config(&mut current, patch);
    validate_layer_scope(&current, scope)?;
    validate_partial(&current)?;
    atomic_write_config(config_path, &current)
}

/// Applies a partial memory-only session update.
///
/// # Errors
///
/// Returns [`ErrorCode::OperationConflict`] while connected or capturing, or
/// [`ErrorCode::ConfigInvalid`] when the patch violates session scope.
pub fn apply_session_patch(
    current: &mut ConfigFile,
    patch: &ConfigFile,
    state: ConfigSetState,
) -> Result<(), JlinkError> {
    if state.connected || state.capture_active {
        return Err(JlinkError::new(
            ErrorCode::OperationConflict,
            "configuration cannot change while connected or capturing",
            true,
        ));
    }
    validate_layer_scope(patch, ConfigScope::Session)?;
    validate_partial(patch)?;
    let mut candidate = current.clone();
    merge_config(&mut candidate, patch);
    validate_layer_scope(&candidate, ConfigScope::Session)?;
    validate_partial(&candidate)?;
    *current = candidate;
    Ok(())
}

pub(crate) fn validate_layer_scope(
    config: &ConfigFile,
    scope: ConfigScope,
) -> Result<(), JlinkError> {
    let invalid = match scope {
        ConfigScope::Session | ConfigScope::Project => config.probe.is_some(),
        ConfigScope::User => {
            config.target.is_some()
                || config.symbols.is_some()
                || config.firmware.is_some()
                || config.jlink.is_some()
                || config.capture.is_some()
                || config.profile.is_some()
        }
    };
    if invalid {
        let message = match scope {
            ConfigScope::Session => "session scope does not permit probe.serial configuration",
            ConfigScope::User => "user scope permits only probe.serial configuration",
            ConfigScope::Project => "project scope does not permit probe.serial configuration",
        };
        return Err(config_error(message));
    }
    Ok(())
}

pub(crate) fn validate_partial(config: &ConfigFile) -> Result<(), JlinkError> {
    if let Some(target) = &config.target {
        if let Some(device) = &target.device {
            validate_device(device)?;
        }
        if target.speed_khz == Some(0) {
            return Err(config_error("target.speed_khz must be positive"));
        }
    }
    if let Some(jlink) = &config.jlink {
        if let Some(version) = &jlink.version
            && version.trim().is_empty()
        {
            return Err(config_error("jlink.version must not be empty"));
        }
        if let Some(hash) = &jlink.sha256
            && (hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(config_error(
                "jlink.sha256 must be a 64-digit hexadecimal digest",
            ));
        }
    }
    if let Some(capture) = &config.capture
        && capture.max_bytes == Some(0)
    {
        return Err(config_error("capture.max_bytes must be positive"));
    }
    if let Some(profile) = &config.profile {
        validate_profile_config(profile)?;
    }
    Ok(())
}

fn validate_profile_config(profile: &FlashProfileConfig) -> Result<(), JlinkError> {
    for region in &profile.flash_regions {
        MemoryRegion::new(region.address, region.length, MemoryRegionKind::Flash)?;
    }
    for region in &profile.readable_ram {
        MemoryRegion::new(region.address, region.length, MemoryRegionKind::Ram)?;
    }
    if let Some(region) = profile.loader_ram {
        MemoryRegion::new(region.address, region.length, MemoryRegionKind::Ram)?;
    }
    Ok(())
}

fn build_flash_profile(
    device: &ResolvedField<String>,
    config: Option<&ResolvedField<FlashProfileConfig>>,
) -> Result<FlashProfile, JlinkError> {
    let flash_regions = config
        .map(|field| {
            field
                .value
                .flash_regions
                .iter()
                .map(|region| {
                    MemoryRegion::new(region.address, region.length, MemoryRegionKind::Flash)
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let readable_ram = config
        .map(|field| {
            field
                .value
                .readable_ram
                .iter()
                .map(|region| {
                    MemoryRegion::new(region.address, region.length, MemoryRegionKind::Ram)
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let loader_ram = config
        .and_then(|field| field.value.loader_ram)
        .map(|region| MemoryRegion::new(region.address, region.length, MemoryRegionKind::Ram))
        .transpose()?;
    let capabilities = config.map_or_else(TargetCapabilities::default, |field| {
        field.value.capabilities.clone()
    });
    let source = config.map_or(device.source, |field| field.source);
    let profile = FlashProfile {
        device: device.value.clone(),
        aliases: Vec::new(),
        flash_regions,
        readable_ram,
        loader_ram,
        capabilities,
        sources: vec![ProfileSource {
            kind: profile_source_kind(source),
            locator: source.to_string(),
        }],
        conflicts: Vec::new(),
    };
    profile.validate()?;
    Ok(profile)
}

const fn profile_source_kind(source: ConfigSource) -> ProfileSourceKind {
    match source {
        ConfigSource::Request | ConfigSource::Session => ProfileSourceKind::Session,
        ConfigSource::Project => ProfileSourceKind::Project,
        ConfigSource::Discovered => ProfileSourceKind::ProjectNative,
        ConfigSource::User | ConfigSource::Default => ProfileSourceKind::Environment,
    }
}

fn validate_device(device: &str) -> Result<(), JlinkError> {
    let normalized = device.trim().to_ascii_lowercase();
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
        return Err(config_error(
            "target.device must name a concrete supported device, not a generic Cortex-M",
        ));
    }
    Ok(())
}

fn validate_jlink_fields(jlink: &ResolvedJlink) -> Result<(), JlinkError> {
    if jlink.version.value.trim().is_empty() {
        return Err(config_error("jlink.version must not be empty"));
    }
    if jlink.sha256.value.len() != 64
        || !jlink
            .sha256
            .value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(config_error(
            "jlink.sha256 must be a 64-digit hexadecimal digest",
        ));
    }
    Ok(())
}

fn required<T>(
    name: &str,
    value: Option<ResolvedField<T>>,
) -> Result<ResolvedField<T>, JlinkError> {
    value.ok_or_else(|| config_error(format!("{name} is required")))
}

fn pick<T: Clone>(
    request: Option<&T>,
    session: Option<&T>,
    user: Option<&T>,
    project: Option<&T>,
    discovered: Option<&T>,
) -> Option<ResolvedField<T>> {
    request
        .map(|value| resolved(value.clone(), ConfigSource::Request))
        .or_else(|| session.map(|value| resolved(value.clone(), ConfigSource::Session)))
        .or_else(|| user.map(|value| resolved(value.clone(), ConfigSource::User)))
        .or_else(|| project.map(|value| resolved(value.clone(), ConfigSource::Project)))
        .or_else(|| discovered.map(|value| resolved(value.clone(), ConfigSource::Discovered)))
}

fn pick_copy<T: Copy>(
    request: Option<T>,
    session: Option<T>,
    user: Option<T>,
    project: Option<T>,
    discovered: Option<T>,
) -> Option<ResolvedField<T>> {
    request
        .map(|value| resolved(value, ConfigSource::Request))
        .or_else(|| session.map(|value| resolved(value, ConfigSource::Session)))
        .or_else(|| user.map(|value| resolved(value, ConfigSource::User)))
        .or_else(|| project.map(|value| resolved(value, ConfigSource::Project)))
        .or_else(|| discovered.map(|value| resolved(value, ConfigSource::Discovered)))
}

fn resolved<T>(value: T, source: ConfigSource) -> ResolvedField<T> {
    ResolvedField { value, source }
}

fn config_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ConfigInvalid, message, false)
}

fn read_config_file(path: &Path) -> Result<Option<ConfigFile>, JlinkError> {
    match fs::read_to_string(path) {
        Ok(contents) => toml::from_str(&contents)
            .map(Some)
            .map_err(|error| config_error(format!("cannot parse {}: {error}", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(config_error(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

fn merge_config(base: &mut ConfigFile, patch: &ConfigFile) {
    merge_target(&mut base.target, patch.target.as_ref());
    merge_symbols(&mut base.symbols, patch.symbols.as_ref());
    merge_firmware(&mut base.firmware, patch.firmware.as_ref());
    merge_jlink(&mut base.jlink, patch.jlink.as_ref());
    merge_probe(&mut base.probe, patch.probe.as_ref());
    merge_capture(&mut base.capture, patch.capture.as_ref());
    if patch.profile.is_some() {
        base.profile.clone_from(&patch.profile);
    }
}

fn merge_target(base: &mut Option<TargetConfig>, patch: Option<&TargetConfig>) {
    if let Some(patch) = patch {
        let target = base.get_or_insert_with(TargetConfig::default);
        if patch.device.is_some() {
            target.device.clone_from(&patch.device);
        }
        if patch.interface.is_some() {
            target.interface = patch.interface;
        }
        if patch.speed_khz.is_some() {
            target.speed_khz = patch.speed_khz;
        }
    }
}

fn merge_symbols(base: &mut Option<SymbolsConfig>, patch: Option<&SymbolsConfig>) {
    if let Some(patch) = patch {
        let symbols = base.get_or_insert_with(SymbolsConfig::default);
        if patch.elf.is_some() {
            symbols.elf.clone_from(&patch.elf);
        }
    }
}

fn merge_firmware(base: &mut Option<FirmwareConfig>, patch: Option<&FirmwareConfig>) {
    if let Some(patch) = patch {
        let firmware = base.get_or_insert_with(FirmwareConfig::default);
        if patch.image.is_some() {
            firmware.image.clone_from(&patch.image);
        }
    }
}

fn merge_jlink(base: &mut Option<JlinkConfig>, patch: Option<&JlinkConfig>) {
    if let Some(patch) = patch {
        let jlink = base.get_or_insert_with(JlinkConfig::default);
        if patch.dll_path.is_some() {
            jlink.dll_path.clone_from(&patch.dll_path);
        }
        if patch.version.is_some() {
            jlink.version.clone_from(&patch.version);
        }
        if patch.sha256.is_some() {
            jlink.sha256.clone_from(&patch.sha256);
        }
    }
}

fn merge_probe(base: &mut Option<ProbeConfig>, patch: Option<&ProbeConfig>) {
    if let Some(patch) = patch {
        let probe = base.get_or_insert_with(ProbeConfig::default);
        if patch.serial.is_some() {
            probe.serial = patch.serial;
        }
    }
}

fn merge_capture(base: &mut Option<CaptureConfig>, patch: Option<&CaptureConfig>) {
    if let Some(patch) = patch {
        let capture = base.get_or_insert_with(CaptureConfig::default);
        if patch.max_bytes.is_some() {
            capture.max_bytes = patch.max_bytes;
        }
    }
}

fn atomic_write_config(path: &Path, config: &ConfigFile) -> Result<(), JlinkError> {
    let contents = toml::to_string_pretty(config)
        .map_err(|error| config_error(format!("cannot serialize configuration: {error}")))?;
    if let Some(parent) = non_empty_parent(path) {
        fs::create_dir_all(parent).map_err(|error| {
            config_error(format!("cannot create {}: {error}", parent.display()))
        })?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| config_error(format!("clock error: {error}")))?
        .as_nanos();
    let temporary = path.with_extension(format!("tmp-{}-{stamp}", std::process::id()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                config_error(format!("cannot create temporary configuration: {error}"))
            })?;
        file.write_all(contents.as_bytes()).map_err(|error| {
            config_error(format!("cannot write temporary configuration: {error}"))
        })?;
        file.sync_all().map_err(|error| {
            config_error(format!("cannot fsync temporary configuration: {error}"))
        })?;
        Ok::<(), JlinkError>(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    let replace_result = replace_file(&temporary, path);
    if replace_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    replace_result
}

fn non_empty_parent(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> Result<(), JlinkError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both vectors are NUL-terminated UTF-16 strings that remain alive for the call.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(config_error(format!(
            "cannot atomically replace {}",
            destination.display()
        )))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, destination: &Path) -> Result<(), JlinkError> {
    fs::rename(temporary, destination).map_err(|error| {
        config_error(format!(
            "cannot atomically replace {}: {error}",
            destination.display()
        ))
    })
}

/// Validates a configured J-Link DLL against PE architecture, version, and SHA-256.
///
/// # Errors
///
/// Returns a stable DLL identity error when the file is missing, not PE x64,
/// or does not match the configured version or digest.
pub fn validate_dll_identity(config: &ResolvedJlink) -> Result<(), JlinkError> {
    let path = &config.dll_path.value;
    if !path.is_file() {
        return Err(JlinkError::new(
            ErrorCode::DllNotFound,
            format!("J-Link DLL does not exist: {}", path.display()),
            false,
        ));
    }
    validate_pe_x64(path)?;
    let actual_hash = sha256_file(path)?;
    if !actual_hash.eq_ignore_ascii_case(&config.sha256.value) {
        return Err(JlinkError::new(
            ErrorCode::DllHashMismatch,
            format!("J-Link DLL SHA-256 mismatch: {actual_hash}"),
            false,
        ));
    }
    let actual_version = windows_file_version(path)?;
    if actual_version != config.version.value {
        return Err(JlinkError::new(
            ErrorCode::DllVersionMismatch,
            format!("J-Link DLL version mismatch: {actual_version}"),
            false,
        ));
    }
    Ok(())
}

/// Selects an x64 DLL from the same SEGGER installation when the configured file is x86.
///
/// # Errors
///
/// Returns a stable architecture error when an existing configured PE is x86 and no valid
/// same-directory x64 candidate exists, or when its PE header cannot be inspected.
pub fn select_x64_dll_candidate(path: &Path) -> Result<PathBuf, JlinkError> {
    if !path.is_file() {
        return Ok(path.to_path_buf());
    }
    match pe_machine(path)? {
        0x8664 => Ok(path.to_path_buf()),
        0x014c => {
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            for name in ["JLink_x64.dll", "JLinkARM_x64.dll"] {
                let candidate = parent.join(name);
                if candidate.is_file() && pe_machine(&candidate) == Ok(0x8664) {
                    return Ok(candidate);
                }
            }
            Err(JlinkError::new(
                ErrorCode::DllArchitectureMismatch,
                "configured J-Link DLL is x86 and no same-install x64 DLL was found",
                false,
            ))
        }
        machine => Err(JlinkError::new(
            ErrorCode::DllArchitectureMismatch,
            format!("J-Link DLL has unsupported PE machine 0x{machine:04X}"),
            false,
        )),
    }
}

fn validate_pe_x64(path: &Path) -> Result<(), JlinkError> {
    if pe_machine(path)? != 0x8664 {
        return Err(JlinkError::new(
            ErrorCode::DllArchitectureMismatch,
            "J-Link DLL is not a PE x64 image",
            false,
        ));
    }
    Ok(())
}

fn pe_machine(path: &Path) -> Result<u16, JlinkError> {
    let mut file = File::open(path)
        .map_err(|error| JlinkError::new(ErrorCode::DllNotFound, error.to_string(), false))?;
    let mut dos = [0_u8; 64];
    file.read_exact(&mut dos).map_err(|error| {
        JlinkError::new(ErrorCode::DllArchitectureMismatch, error.to_string(), false)
    })?;
    if dos[0] != b'M' || dos[1] != b'Z' {
        return Err(JlinkError::new(
            ErrorCode::DllArchitectureMismatch,
            "J-Link DLL is not a PE image",
            false,
        ));
    }
    let pe_offset = u64::from(u32::from_le_bytes([
        dos[0x3c], dos[0x3d], dos[0x3e], dos[0x3f],
    ]));
    file.seek(std::io::SeekFrom::Start(pe_offset))
        .map_err(|error| {
            JlinkError::new(ErrorCode::DllArchitectureMismatch, error.to_string(), false)
        })?;
    let mut header = [0_u8; 6];
    file.read_exact(&mut header).map_err(|error| {
        JlinkError::new(ErrorCode::DllArchitectureMismatch, error.to_string(), false)
    })?;
    if &header[..4] != b"PE\0\0" {
        return Err(JlinkError::new(
            ErrorCode::DllArchitectureMismatch,
            "J-Link DLL has an invalid PE signature",
            false,
        ));
    }
    Ok(u16::from_le_bytes([header[4], header[5]]))
}

fn sha256_file(path: &Path) -> Result<String, JlinkError> {
    let mut file = File::open(path)
        .map_err(|error| JlinkError::new(ErrorCode::DllNotFound, error.to_string(), false))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| JlinkError::new(ErrorCode::DllNotFound, error.to_string(), false))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let digest = digest.finalize();
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

#[cfg(windows)]
fn windows_file_version(path: &Path) -> Result<String, JlinkError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut handle = 0_u32;
    // SAFETY: wide is a valid NUL-terminated path and handle is a valid out parameter.
    let size = unsafe { GetFileVersionInfoSizeW(wide.as_ptr(), &raw mut handle) };
    if size == 0 {
        return Err(JlinkError::new(
            ErrorCode::DllVersionMismatch,
            "J-Link DLL has no readable Windows version resource",
            false,
        ));
    }
    let mut block = vec![0_u8; size as usize];
    // SAFETY: block has exactly the size requested by GetFileVersionInfoSizeW.
    let loaded =
        unsafe { GetFileVersionInfoW(wide.as_ptr(), 0, size, block.as_mut_ptr().cast::<c_void>()) };
    if loaded == 0 {
        return Err(JlinkError::new(
            ErrorCode::DllVersionMismatch,
            "J-Link DLL version resource could not be loaded",
            false,
        ));
    }
    let translation_path: Vec<u16> = "\\VarFileInfo\\Translation"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut translation = ptr::null_mut::<c_void>();
    let mut translation_len = 0_u32;
    // SAFETY: block is the loaded version resource and translation_path is NUL-terminated.
    let found = unsafe {
        VerQueryValueW(
            block.as_ptr().cast::<c_void>(),
            translation_path.as_ptr(),
            &raw mut translation,
            &raw mut translation_len,
        )
    };
    if found == 0 || translation.is_null() {
        return Err(JlinkError::new(
            ErrorCode::DllVersionMismatch,
            "J-Link DLL version translation is missing",
            false,
        ));
    }
    let translation_len = usize::try_from(translation_len).map_err(|_| {
        JlinkError::new(
            ErrorCode::DllVersionMismatch,
            "J-Link DLL version translation length is invalid",
            false,
        )
    })?;
    let block_start = block.as_ptr() as usize;
    let block_end = block_start.saturating_add(block.len());
    let translation_start = translation.cast::<u8>() as usize;
    if translation_len > block_end.saturating_sub(translation_start)
        || translation_start < block_start
    {
        return Err(JlinkError::new(
            ErrorCode::DllVersionMismatch,
            "J-Link DLL version translation is outside its resource block",
            false,
        ));
    }
    // SAFETY: the resource bounds above prove this byte range lies inside `block`.
    let translation_bytes =
        unsafe { std::slice::from_raw_parts(translation.cast::<u8>(), translation_len) };
    let (language, code_page) = parse_translation(translation_bytes)?;
    let version_path = format!("\\StringFileInfo\\{language:04x}{code_page:04x}\\FileVersion");
    let version_path: Vec<u16> = version_path.encode_utf16().chain(Some(0)).collect();
    let mut value = ptr::null_mut::<c_void>();
    let mut value_len = 0_u32;
    // SAFETY: version_path is NUL-terminated and the version block remains alive.
    let found = unsafe {
        VerQueryValueW(
            block.as_ptr().cast::<c_void>(),
            version_path.as_ptr(),
            &raw mut value,
            &raw mut value_len,
        )
    };
    if found == 0 || value.is_null() || value_len == 0 {
        return Err(JlinkError::new(
            ErrorCode::DllVersionMismatch,
            "J-Link DLL FileVersion is missing",
            false,
        ));
    }
    // SAFETY: a successful VerQueryValueW call returned value_len UTF-16 code units.
    let value = unsafe { std::slice::from_raw_parts(value.cast::<u16>(), value_len as usize) };
    Ok(String::from_utf16_lossy(value)
        .trim_end_matches('\0')
        .to_owned())
}

#[cfg(not(windows))]
fn windows_file_version(_path: &Path) -> Result<String, JlinkError> {
    Err(JlinkError::new(
        ErrorCode::DllVersionMismatch,
        "Windows DLL version resources are only available on Windows",
        false,
    ))
}

fn parse_translation(bytes: &[u8]) -> Result<(u16, u16), JlinkError> {
    if bytes.len() < 4 {
        return Err(JlinkError::new(
            ErrorCode::DllVersionMismatch,
            "J-Link DLL version translation is truncated",
            false,
        ));
    }
    Ok((
        u16::from_le_bytes([bytes[0], bytes[1]]),
        u16::from_le_bytes([bytes[2], bytes[3]]),
    ))
}

impl fmt::Display for ConfigScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Session => "session",
            Self::Project => "project",
            Self::User => "user",
        })
    }
}

impl fmt::Display for ConfigSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Request => "request",
            Self::Session => "session",
            Self::User => "user",
            Self::Project => "project",
            Self::Discovered => "discovered",
            Self::Default => "default",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigFile, ConfigPaths, ConfigSetState, ConfigSource, JlinkConfig, TargetConfig,
        apply_session_patch, inspect_config, non_empty_parent, parse_translation, resolve_layers,
        resolve_layers_with_session, select_x64_dll_candidate,
    };
    use jlink_domain::{ErrorCode, TargetInterface};
    use std::path::{Path, PathBuf};

    #[test]
    fn translation_parser_rejects_lengths_zero_through_three() {
        for length in 0..4 {
            let error = parse_translation(&[0_u8; 3][..length])
                .expect_err("truncated translation must be rejected");
            assert_eq!(error.code, ErrorCode::DllVersionMismatch);
        }
    }

    #[test]
    fn translation_parser_decodes_little_endian_language_and_code_page() {
        assert_eq!(
            parse_translation(&[0x09, 0x04, 0xb0, 0x04]).expect("translation"),
            (0x0409, 0x04b0)
        );
    }

    #[test]
    fn empty_parent_is_not_created_for_a_relative_config_name() {
        assert!(non_empty_parent(Path::new("jlink-mcp.toml")).is_none());
        assert_eq!(
            non_empty_parent(Path::new("config/jlink-mcp.toml")),
            Some(Path::new("config"))
        );
    }

    #[test]
    fn partial_inspection_and_session_lifecycle_are_explicit() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let project_path = directory.path().join("jlink-mcp.toml");
        std::fs::write(
            &project_path,
            "[target]\ndevice = \"Z20K146MC\"\ninterface = \"swd\"\n",
        )
        .expect("partial project");
        let paths = ConfigPaths::new(project_path, directory.path().join("user.toml"));
        let inspection = inspect_config(&ConfigFile::default(), &paths, &ConfigFile::default())
            .expect("partial inspection");
        assert_eq!(inspection.effective["target.device"], "Z20K146MC");
        assert!(inspection.missing.contains(&"target.speed_khz".to_owned()));
        assert!(!inspection.operations["connect"]);

        let project = complete_config();
        let mut session = ConfigFile::default();
        apply_session_patch(
            &mut session,
            &ConfigFile {
                target: Some(TargetConfig {
                    device: Some("Z20K146MC".to_owned()),
                    ..TargetConfig::default()
                }),
                ..ConfigFile::default()
            },
            ConfigSetState::default(),
        )
        .expect("session patch");
        let selected = resolve_layers_with_session(
            &ConfigFile::default(),
            Some(&session),
            None,
            Some(&project),
            None,
        )
        .expect("session resolution");
        assert_eq!(selected.target.device.source, ConfigSource::Session);
        let next = resolve_layers(&ConfigFile::default(), None, Some(&project), None)
            .expect("new lifecycle");
        assert_eq!(next.target.device.source, ConfigSource::Project);
    }

    #[test]
    fn x86_candidate_selects_same_install_x64_pe() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let x86 = directory.path().join("JLinkARM.dll");
        let x64 = directory.path().join("JLink_x64.dll");
        std::fs::write(&x86, minimal_pe(0x014c)).expect("x86 PE");
        std::fs::write(&x64, minimal_pe(0x8664)).expect("x64 PE");
        assert_eq!(select_x64_dll_candidate(&x86).expect("selection"), x64);
    }

    fn complete_config() -> ConfigFile {
        ConfigFile {
            target: Some(TargetConfig {
                device: Some("S32K144".to_owned()),
                interface: Some(TargetInterface::Swd),
                speed_khz: Some(1_000),
            }),
            jlink: Some(JlinkConfig {
                dll_path: Some(PathBuf::from("missing.dll")),
                version: Some("1".to_owned()),
                sha256: Some("0".repeat(64)),
            }),
            ..ConfigFile::default()
        }
    }

    fn minimal_pe(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0_u8; 70];
        bytes[0..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&64_u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&machine.to_le_bytes());
        bytes
    }
}
