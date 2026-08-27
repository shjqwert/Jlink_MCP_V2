//! Primary T-P3-START DWARF planning and closed-contract assertions.

use std::{fs, path::PathBuf};

use jlink_domain::{ErrorCode, TargetInterface};
use jlink_mcp::{
    config::{ConfigFile, ConfigPaths, JlinkConfig, SymbolsConfig, TargetConfig},
    mcp::tool_catalog,
    runtime::Runtime,
};
use serde_json::json;

fn iar_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../validation/evidence/f0-c/F0cDwarfFixture.out")
}

fn runtime() -> (tempfile::TempDir, Runtime) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let project = temporary.path().join("jlink-mcp.toml");
    let user = temporary.path().join("user.toml");
    let config = ConfigFile {
        target: Some(TargetConfig {
            device: Some("S32K144".to_owned()),
            interface: Some(TargetInterface::Swd),
            speed_khz: Some(4_000),
        }),
        symbols: Some(SymbolsConfig {
            elf: Some(iar_fixture_path()),
        }),
        jlink: Some(JlinkConfig {
            dll_path: Some(PathBuf::from("unused-by-hss-planning.dll")),
            version: Some("6.98a".to_owned()),
            sha256: Some("0".repeat(64)),
        }),
        ..ConfigFile::default()
    };
    fs::write(&project, toml::to_string_pretty(&config).expect("TOML"))
        .expect("write project config");
    let runtime = Runtime::new(
        ConfigPaths::new(project, user),
        temporary.path().join("unused-worker.exe"),
        temporary.path().join("leases"),
    );
    (temporary, runtime)
}

#[test]
fn t_p3_start_expands_iar_structure_member_and_explicit_slice_before_hardware() {
    let (_temporary, mut runtime) = runtime();
    let plan = runtime
        .prepare_hss_start(&json!({
            "action": "start",
            "capture_key": "iar-plan",
            "duration_s": 30,
            "rate_hz": 1_000,
            "variables": [
                { "path": "gstF0cRoot.stNested.ulSequence" },
                { "path": "gstF0cFlex.aucPayload", "slice": { "start": 1, "count": 3 } }
            ],
            "return_when": "started"
        }))
        .expect("build static IAR HSS plan");

    assert_eq!(plan.variables().len(), 2);
    assert_eq!(plan.variables()[0].access_plan().address(), 0x2000_0000);
    assert_eq!(plan.variables()[0].access_plan().byte_size(), 4);
    assert_eq!(plan.variables()[0].sample_offset(), 0);
    assert_eq!(plan.variables()[1].access_plan().address(), 0x2000_1003);
    assert_eq!(plan.variables()[1].access_plan().byte_size(), 3);
    assert_eq!(plan.variables()[1].sample_offset(), 4);
    assert_eq!(plan.frame_layout().record_bytes(), 11);
}

#[test]
fn t_p3_start_rejects_unbounded_dwarf_selection_before_worker_or_dll_access() {
    let (_temporary, mut runtime) = runtime();
    let error = runtime
        .prepare_hss_start(&json!({
            "action": "start",
            "capture_key": "unbounded",
            "duration_s": 1,
            "rate_hz": 1,
            "variables": [{ "path": "gstF0cFlex" }],
            "return_when": "completed"
        }))
        .expect_err("unbounded flexible aggregate must fail before hardware");
    assert_eq!(error.code(), ErrorCode::TypeUnsupported);
}

#[test]
fn t_p3_start_schema_counts_top_level_selectors_and_forbids_raw_addresses() {
    let hss = tool_catalog()
        .into_iter()
        .find(|tool| tool["name"] == "jlink_hss")
        .expect("jlink_hss tool");
    let start = hss["inputSchema"]["oneOf"]
        .as_array()
        .expect("HSS variants")
        .iter()
        .find(|variant| variant["properties"]["action"]["const"] == "start")
        .expect("start variant");
    assert_eq!(start["properties"]["variables"]["minItems"], 1);
    assert_eq!(start["properties"]["variables"]["maxItems"], 10);
    assert_eq!(start["properties"]["duration_s"]["minimum"], 1);
    assert_eq!(start["properties"]["duration_s"]["maximum"], 300);
    assert_eq!(start["properties"]["rate_hz"]["minimum"], 1);
    assert_eq!(start["properties"]["rate_hz"]["maximum"], 1_000);
    assert!(
        start["properties"]["variables"]["items"]["properties"]
            .get("address")
            .is_none()
    );
}
