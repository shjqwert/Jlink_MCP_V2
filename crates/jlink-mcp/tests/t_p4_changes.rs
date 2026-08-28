//! Primary T-P4-CHANGES MCP routing, strict Schema, and immutable query assertions.

use std::{fs, io::Cursor, path::PathBuf};

use jlink_capture::{CapturePhase, CaptureStore};
use jlink_domain::{
    AccessLayout, AccessPlan, FirmwareIdentityPlan, HssDataIntegrity, HssDrainTiming,
    HssQualityTracker, HssReturnWhen, HssRunSnapshot, HssRunState, HssStartPlan, HssThresholdRule,
    ScalarEncoding, TargetConnectionSpec, TargetInterface, VariableSelector, probe_identity_hash,
};
use jlink_mcp::{
    config::{ConfigFile, ConfigPaths, FirmwareConfig, JlinkConfig, ProbeConfig, TargetConfig},
    mcp::serve,
    runtime::Runtime,
};
use serde_json::{Value, json};

const PROBE_SERIAL: u32 = 260_106_173;

fn start_rule() -> HssThresholdRule {
    serde_json::from_value(json!({
        "kind": "crosses",
        "id": "r-start",
        "path": "fixture.value",
        "value": 10,
        "direction": "up"
    }))
    .expect("start rule fixture")
}

fn plan() -> HssStartPlan {
    let firmware: FirmwareIdentityPlan = serde_json::from_value(json!({
        "elf_sha256": "11".repeat(32),
        "segments": [{
            "address": 0,
            "length": 4,
            "sha256": "22".repeat(32)
        }]
    }))
    .expect("firmware fixture");
    let access = AccessPlan::new(
        "11".repeat(32),
        VariableSelector::new("fixture.value", None).expect("selector"),
        0x2000_0000,
        4,
        None,
        false,
        AccessLayout::Scalar {
            name: "uint32_t".to_owned(),
            byte_size: 4,
            encoding: ScalarEncoding::Unsigned,
        },
    );
    HssStartPlan::new(
        "mcp-changes-key",
        1,
        3,
        HssReturnWhen::Completed,
        vec![access],
        vec![start_rule()],
        firmware,
    )
    .expect("start plan")
}

fn target() -> TargetConnectionSpec {
    TargetConnectionSpec::new(
        "S32K144",
        TargetInterface::Swd,
        4_000,
        Some(PROBE_SERIAL),
        None,
    )
    .expect("target fixture")
}

fn runtime_fixture() -> (tempfile::TempDir, Runtime) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let project = temporary.path().join("jlink-mcp.toml");
    let user = temporary.path().join("user.toml");
    fs::write(
        &project,
        toml::to_string_pretty(&ConfigFile {
            target: Some(TargetConfig {
                device: Some("S32K144".to_owned()),
                interface: Some(TargetInterface::Swd),
                speed_khz: Some(4_000),
            }),
            firmware: Some(FirmwareConfig::default()),
            jlink: Some(JlinkConfig {
                dll_path: Some(PathBuf::from("unused-by-read-only-query.dll")),
                version: Some("6.98a".to_owned()),
                sha256: Some("0".repeat(64)),
            }),
            ..ConfigFile::default()
        })
        .expect("project TOML"),
    )
    .expect("project config");
    fs::write(
        &user,
        toml::to_string_pretty(&ConfigFile {
            probe: Some(ProbeConfig {
                serial: Some(PROBE_SERIAL),
            }),
            ..ConfigFile::default()
        })
        .expect("user TOML"),
    )
    .expect("user config");

    let lease_root = temporary.path().join("leases");
    let store_root = lease_root
        .join("captures")
        .join(probe_identity_hash(&PROBE_SERIAL.to_string()).expect("probe identity hash"));
    let store = CaptureStore::open(store_root).expect("capture store");
    let plan = plan();
    let payload = [
        0_u32.to_le_bytes(),
        8_u32.to_le_bytes(),
        1_u32.to_le_bytes(),
        11_u32.to_le_bytes(),
        2_u32.to_le_bytes(),
        9_u32.to_le_bytes(),
    ]
    .concat();
    let mut tracker = HssQualityTracker::new(&plan, 0);
    tracker
        .observe_complete_records(plan.frame_layout(), &payload, 2_000)
        .expect("quality fixture");
    let status = HssRunSnapshot {
        capture_id: "cap-mcp-changes".to_owned(),
        state: HssRunState::Completed,
        integrity: HssDataIntegrity::Unknown,
        elapsed_us: 3_000,
        complete_records: 3,
        drain: HssDrainTiming::default(),
        quality: tracker.summary(0),
        writes: Vec::new(),
        failure_code: None,
        partial_available: false,
        reason: None,
        recoverable: None,
        recovery_notifications: Vec::new(),
    };
    let mut writer = store
        .create_writer("cap-mcp-changes", &target(), &plan, 16 * 1024 * 1024)
        .expect("capture writer");
    writer
        .append(2_000, CapturePhase::Live, &payload)
        .expect("checksummed payload");
    writer.finish(&status).expect("immutable completion");

    let runtime = Runtime::new(
        ConfigPaths::new(project, user),
        temporary.path().join("worker-must-not-run.exe"),
        lease_root,
    );
    (temporary, runtime)
}

fn exchange(requests: &[Value], runtime: &mut Runtime) -> Vec<Value> {
    let input = requests
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut output = Vec::new();
    serve(Cursor::new(input), &mut output, runtime).expect("stdio server");
    String::from_utf8(output)
        .expect("UTF-8 response")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON-RPC response"))
        .collect()
}

#[test]
fn t_p4_changes_routes_exact_facts_and_rule_matches_through_strict_mcp_schema() {
    let (_temporary, mut runtime) = runtime_fixture();
    let responses = exchange(
        &[
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "jlink_hss", "arguments": {
                    "action": "query", "capture_key": "mcp-changes-key", "view": "changes"
                }}
            }),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "jlink_hss", "arguments": {
                    "action": "query", "capture_id": "cap-mcp-changes", "view": "changes",
                    "series": ["s0"],
                    "rules": [{
                        "kind": "equals", "id": "r-query",
                        "path": "fixture.value", "value": 9
                    }]
                }}
            }),
        ],
        &mut runtime,
    );

    let start_rules = &responses[0]["result"]["structuredContent"];
    assert_eq!(start_rules["dictionary"], json!({ "s0": "fixture.value" }));
    assert_eq!(start_rules["changes"].as_array().map(Vec::len), Some(2));
    assert_eq!(start_rules["matches"].as_array().map(Vec::len), Some(1));
    assert_eq!(start_rules["matches"][0]["rule"], "r-start");
    assert_eq!(start_rules["matches"][0]["observed_by_us"], 1_000);
    assert_eq!(responses[0]["result"]["content"], json!([]));

    let query_rules = &responses[1]["result"]["structuredContent"];
    assert_eq!(query_rules["changes"].as_array().map(Vec::len), Some(2));
    assert_eq!(query_rules["matches"].as_array().map(Vec::len), Some(1));
    assert_eq!(query_rules["matches"][0]["rule"], "r-query");
    assert_eq!(query_rules["matches"][0]["observed_by_us"], 2_000);
}

#[test]
fn t_p4_changes_rejects_unknown_rule_kinds_and_unmatched_paths() {
    let (_temporary, mut runtime) = runtime_fixture();
    let responses = exchange(
        &[
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "jlink_hss", "arguments": {
                    "action": "query", "capture_id": "cap-mcp-changes", "view": "changes",
                    "rules": [{
                        "kind": "script", "id": "bad-kind",
                        "path": "fixture.value", "value": 9
                    }]
                }}
            }),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "jlink_hss", "arguments": {
                    "action": "query", "capture_id": "cap-mcp-changes", "view": "changes",
                    "rules": [{
                        "kind": "equals", "id": "missing",
                        "path": "fixture.missing", "value": 9
                    }]
                }}
            }),
        ],
        &mut runtime,
    );

    assert_eq!(responses[0]["error"]["code"], -32_602);
    assert_eq!(
        responses[1]["result"]["structuredContent"]["error"]["code"],
        "VALUE_INVALID"
    );
}
