//! Primary configuration contract tests for P1 task 2.3.

use std::path::{Path, PathBuf};

use jlink_domain::{ErrorCode, TargetInterface};
use jlink_mcp::config::{
    CaptureConfig, ConfigFile, ConfigPaths, ConfigScope, ConfigSetState, ConfigSource,
    DEFAULT_CAPTURE_MAX_BYTES, DiscoveredConfig, FirmwareConfig, JlinkConfig, ProbeConfig,
    ResolvedJlink, SymbolsConfig, TargetConfig, config_set, resolve_config, resolve_layers,
    validate_dll_identity,
};

const DLL_PATH: &str = r"C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll";
const DLL_VERSION: &str = "6.98a";
const DLL_SHA256: &str = "D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5";

fn complete_config() -> ConfigFile {
    ConfigFile {
        target: Some(TargetConfig {
            device: Some("S32K144".to_owned()),
            interface: Some(TargetInterface::Swd),
            speed_khz: Some(4000),
        }),
        symbols: Some(SymbolsConfig {
            elf: Some(PathBuf::from("firmware.elf")),
        }),
        firmware: Some(FirmwareConfig::default()),
        jlink: Some(JlinkConfig {
            dll_path: Some(PathBuf::from(DLL_PATH)),
            version: Some(DLL_VERSION.to_owned()),
            sha256: Some(DLL_SHA256.to_owned()),
        }),
        probe: None,
        capture: None,
    }
}

#[test]
fn t_p1_cfg_each_field_uses_request_user_project_discovered_priority() {
    let request = ConfigFile {
        target: Some(TargetConfig {
            device: Some("S32K144-request".to_owned()),
            interface: None,
            speed_khz: None,
        }),
        capture: Some(CaptureConfig {
            max_bytes: Some(11),
        }),
        ..ConfigFile::default()
    };
    let user = ConfigFile {
        probe: Some(ProbeConfig { serial: Some(1) }),
        ..ConfigFile::default()
    };
    let mut project = complete_config();
    project.target.as_mut().expect("target").interface = Some(TargetInterface::Swd);
    project.target.as_mut().expect("target").speed_khz = Some(2000);
    project.symbols.as_mut().expect("symbols").elf = Some(PathBuf::from("project.elf"));
    project.capture = Some(CaptureConfig {
        max_bytes: Some(22),
    });
    let discovered = DiscoveredConfig {
        firmware: Some(FirmwareConfig {
            image: Some(PathBuf::from("discovered.bin")),
        }),
        capture: Some(CaptureConfig {
            max_bytes: Some(33),
        }),
        ..DiscoveredConfig::default()
    };

    let resolved = resolve_layers(&request, Some(&user), Some(&project), Some(&discovered))
        .expect("complete layered configuration should resolve");
    assert_eq!(resolved.target.device.value, "S32K144-request");
    assert_eq!(resolved.target.device.source, ConfigSource::Request);
    assert_eq!(resolved.target.interface.value, TargetInterface::Swd);
    assert_eq!(resolved.target.interface.source, ConfigSource::Project);
    assert_eq!(resolved.target.speed_khz.value, 2000);
    assert_eq!(resolved.target.speed_khz.source, ConfigSource::Project);
    assert_eq!(
        resolved.symbols.elf.expect("ELF").source,
        ConfigSource::Project
    );
    assert_eq!(
        resolved.probe.serial.expect("serial").source,
        ConfigSource::User
    );
    assert_eq!(
        resolved.firmware.image.expect("discovered firmware").source,
        ConfigSource::Discovered
    );
    assert_eq!(resolved.capture.max_bytes.value, 11);
    assert_eq!(resolved.capture.max_bytes.source, ConfigSource::Request);
}

#[test]
fn t_p1_cfg_user_scope_rejects_project_fields() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let paths = ConfigPaths::new(
        directory.path().join("project.toml"),
        directory.path().join("user.toml"),
    );
    let patch = ConfigFile {
        jlink: Some(JlinkConfig {
            dll_path: Some(PathBuf::from("forbidden.dll")),
            ..JlinkConfig::default()
        }),
        ..ConfigFile::default()
    };
    let error = config_set(&paths, ConfigScope::User, &patch, ConfigSetState::default())
        .expect_err("user scope must reject DLL identity");
    assert_eq!(error.code, ErrorCode::ConfigInvalid);

    let invalid_user = ConfigFile {
        target: Some(TargetConfig {
            interface: Some(TargetInterface::Jtag),
            ..TargetConfig::default()
        }),
        ..ConfigFile::default()
    };
    assert_eq!(
        resolve_layers(&complete_config(), Some(&invalid_user), None, None)
            .expect_err("user scope must reject target fields")
            .code,
        ErrorCode::ConfigInvalid
    );
    let invalid_project = ConfigFile {
        probe: Some(ProbeConfig { serial: Some(1) }),
        ..ConfigFile::default()
    };
    assert_eq!(
        resolve_layers(&complete_config(), None, Some(&invalid_project), None)
            .expect_err("project scope must reject probe fields")
            .code,
        ErrorCode::ConfigInvalid
    );
}

#[test]
fn t_p1_cfg_project_update_is_atomic_and_conflict_safe() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let project = directory.path().join("project.toml");
    let paths = ConfigPaths::new(&project, directory.path().join("user.toml"));
    let initial = complete_config();
    config_set(
        &paths,
        ConfigScope::Project,
        &initial,
        ConfigSetState::default(),
    )
    .expect("initial project config should persist");
    let before = std::fs::read_to_string(&project).expect("project config should exist");

    let update = ConfigFile {
        target: Some(TargetConfig {
            speed_khz: Some(5000),
            ..TargetConfig::default()
        }),
        ..ConfigFile::default()
    };
    config_set(
        &paths,
        ConfigScope::Project,
        &update,
        ConfigSetState::default(),
    )
    .expect("project update should persist");
    assert_ne!(
        std::fs::read_to_string(&project).expect("updated config"),
        before
    );
    assert!(
        !directory
            .path()
            .read_dir()
            .expect("directory")
            .any(|entry| entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .contains("tmp-"))
    );

    let before_conflict = std::fs::read(&project).expect("project bytes before conflict");
    let conflicting_update = ConfigFile {
        target: Some(TargetConfig {
            speed_khz: Some(6000),
            ..TargetConfig::default()
        }),
        ..ConfigFile::default()
    };
    let conflict = config_set(
        &paths,
        ConfigScope::Project,
        &conflicting_update,
        ConfigSetState {
            connected: true,
            capture_active: false,
        },
    )
    .expect_err("connected session must reject updates");
    assert_eq!(conflict.code, ErrorCode::OperationConflict);
    assert_eq!(
        before_conflict,
        std::fs::read(&project).expect("project bytes after conflict")
    );

    let invalid = ConfigFile {
        target: Some(TargetConfig {
            device: Some("Cortex-M4".to_owned()),
            ..TargetConfig::default()
        }),
        ..ConfigFile::default()
    };
    let invalid_error = config_set(
        &paths,
        ConfigScope::Project,
        &invalid,
        ConfigSetState::default(),
    )
    .expect_err("invalid candidate must be rejected");
    assert_eq!(invalid_error.code, ErrorCode::ConfigInvalid);
    assert_eq!(
        before_conflict,
        std::fs::read(&project).expect("project bytes unchanged")
    );
}

#[test]
fn t_p1_cfg_defaults_and_validation_are_deterministic() {
    let complete = complete_config();
    let resolved = resolve_layers(&complete, None, None, None).expect("base config");
    assert_eq!(resolved.capture.max_bytes.value, DEFAULT_CAPTURE_MAX_BYTES);
    assert_eq!(resolved.capture.max_bytes.source, ConfigSource::Default);

    let mut invalid = complete.clone();
    for generic in ["Cortex-M", "Cortex-M4", "cortex_m4", "cortexm4"] {
        invalid.target.as_mut().expect("target").device = Some(generic.to_owned());
        assert_eq!(
            resolve_layers(&invalid, None, None, None)
                .expect_err("generic device must be rejected")
                .code,
            ErrorCode::ConfigInvalid
        );
    }
    invalid.target.as_mut().expect("target").device = Some("S32K144".to_owned());
    invalid.target.as_mut().expect("target").speed_khz = Some(0);
    assert_eq!(
        resolve_layers(&invalid, None, None, None)
            .expect_err("zero speed must be rejected")
            .code,
        ErrorCode::ConfigInvalid
    );
}

#[test]
fn t_p1_cfg_real_dll_identity_is_verified_before_use() {
    let config = complete_config();
    let jlink = config.jlink.expect("J-Link configuration");
    let resolved = ResolvedJlink {
        dll_path: jlink_mcp::config::ResolvedField {
            value: jlink.dll_path.expect("DLL path"),
            source: ConfigSource::Project,
        },
        version: jlink_mcp::config::ResolvedField {
            value: jlink.version.expect("version"),
            source: ConfigSource::Project,
        },
        sha256: jlink_mcp::config::ResolvedField {
            value: jlink.sha256.expect("SHA-256"),
            source: ConfigSource::Project,
        },
    };
    validate_dll_identity(&resolved).expect("frozen J-Link 6.98a identity should match");
}

#[test]
fn t_p1_cfg_loads_default_paths_and_keeps_example_portable() {
    let paths = ConfigPaths::default();
    assert_eq!(paths.project, Path::new("jlink-mcp.toml"));
    assert!(paths.user.is_absolute());
    let example_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("jlink-mcp.example.toml");
    assert!(
        std::fs::read_to_string(&example_path)
            .expect("portable example")
            .contains("<concrete-device>")
    );
    let example = std::fs::read_to_string(example_path).expect("portable example");
    assert!(!example.contains("260_106_173"));
    assert!(!example.contains(r"C:\Program Files"));
}

#[test]
fn t_p1_cfg_resolve_reads_injected_project_and_user_paths() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let project = directory.path().join("jlink-mcp.toml");
    let user = directory.path().join("config.toml");
    std::fs::write(&project, toml::to_string(&complete_config()).expect("TOML")).expect("project");
    let user_config = ConfigFile {
        probe: Some(ProbeConfig { serial: Some(1) }),
        ..ConfigFile::default()
    };
    std::fs::write(&user, toml::to_string(&user_config).expect("TOML")).expect("user");
    let resolved = resolve_config(
        &ConfigFile::default(),
        &ConfigPaths::new(project, user),
        &DiscoveredConfig::default(),
    )
    .expect("injected paths should load");
    let probe = resolved.probe.serial.expect("probe");
    assert_eq!(probe.value, 1);
    assert_eq!(probe.source, ConfigSource::User);
}
