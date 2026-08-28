//! Primary T-P4-WINDOW stdio routing and strict action-result Schema assertions.

use std::{fs, io::Cursor, path::PathBuf};

use jlink_capture::{CapturePhase, CaptureStore};
use jlink_domain::{
    AccessLayout, AccessPlan, FirmwareIdentityPlan, HssDataIntegrity, HssDrainTiming,
    HssQualityTracker, HssReturnWhen, HssRunSnapshot, HssRunState, HssStartPlan, HssWriteKind,
    HssWriteResult, HssWriteTiming, ScalarEncoding, TargetConnectionSpec, TargetInterface,
    VariableSelector, probe_identity_hash,
};
use jlink_mcp::{
    config::{ConfigFile, ConfigPaths, FirmwareConfig, JlinkConfig, ProbeConfig, TargetConfig},
    mcp::serve,
    runtime::Runtime,
};
use serde_json::{Value, json};

const PROBE_SERIAL: u32 = 260_106_173;

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
        "mcp-window-key",
        1,
        4,
        HssReturnWhen::Completed,
        vec![access],
        Vec::new(),
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
        8_u32.to_le_bytes(),
        2_u32.to_le_bytes(),
        12_u32.to_le_bytes(),
        3_u32.to_le_bytes(),
        10_u32.to_le_bytes(),
    ]
    .concat();
    let mut tracker = HssQualityTracker::new(&plan, 0);
    tracker
        .observe_complete_records(plan.frame_layout(), &payload, 3_000)
        .expect("quality fixture");
    let status = HssRunSnapshot {
        capture_id: "cap-mcp-window".to_owned(),
        state: HssRunState::Completed,
        integrity: HssDataIntegrity::Unknown,
        elapsed_us: 4_000,
        complete_records: 4,
        drain: HssDrainTiming::default(),
        quality: tracker.summary(0),
        writes: vec![HssWriteTiming {
            request_id: "write-mcp".to_owned(),
            kind: HssWriteKind::MemoryWrite,
            requested_at_us: 800,
            started_at_us: 900,
            completed_at_us: 1_100,
            result: HssWriteResult::Succeeded,
            samples_before: 2,
            samples_after_next_drain: Some(3),
        }],
        failure_code: None,
        partial_available: false,
        reason: None,
        recoverable: None,
        recovery_notifications: Vec::new(),
    };
    let mut writer = store
        .create_writer("cap-mcp-window", &target(), &plan, 16 * 1024 * 1024)
        .expect("capture writer");
    writer
        .append(3_000, CapturePhase::Live, &payload)
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
fn t_p4_window_routes_raw_aggregate_and_event_neighborhood_through_strict_schemas() {
    let (_temporary, mut runtime) = runtime_fixture();
    let responses = exchange(
        &[
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "jlink_hss", "arguments": {
                    "action": "query", "capture_key": "mcp-window-key", "view": "window",
                    "series": ["fixture.value"], "from_us": 0, "to_us": 4_000,
                    "mode": "raw", "limit": 10
                }}
            }),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "jlink_hss", "arguments": {
                    "action": "query", "capture_id": "cap-mcp-window", "view": "window",
                    "series": ["s0"], "from_us": 0, "to_us": 4_000,
                    "mode": "min_max", "points": 2
                }}
            }),
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "jlink_hss", "arguments": {
                    "action": "query", "capture_id": "cap-mcp-window",
                    "view": "around_event", "event_id": "e0",
                    "before_us": 0, "after_us": 0
                }}
            }),
        ],
        &mut runtime,
    );

    let raw = &responses[0]["result"]["structuredContent"];
    assert_eq!(raw["clock"], "sample");
    assert_eq!(raw["time_us"], json!([0, 1_000, 2_000, 3_000]));
    assert_eq!(raw["values"]["s0"], json!([8, 8, 12, 10]));
    assert_eq!(raw["truncated"], false);

    let aggregate = &responses[1]["result"]["structuredContent"];
    assert_eq!(aggregate["buckets"].as_array().map(Vec::len), Some(2));
    assert_eq!(aggregate["buckets"][1]["values"]["s0"], json!([10, 12]));

    let around = &responses[2]["result"]["structuredContent"];
    assert_eq!(around["event"]["kind"], "memory_write");
    assert_eq!(around["event"]["request_id"], "write-mcp");
    assert_eq!(around["window"], json!({ "from_us": 0, "to_us": 3_100 }));
    assert_eq!(around["changes"].as_array().map(Vec::len), Some(2));
    assert!(
        around.get("time_us").is_none(),
        "around_event must not duplicate raw waveform"
    );
}

#[test]
fn t_p4_window_rejects_implicit_aggregates_and_invalid_ranges() {
    let (_temporary, mut runtime) = runtime_fixture();
    let responses = exchange(
        &[
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "jlink_hss", "arguments": {
                    "action": "query", "capture_id": "cap-mcp-window", "view": "window",
                    "series": ["s0"], "from_us": 0, "to_us": 4_000,
                    "mode": "min_max"
                }}
            }),
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "jlink_hss", "arguments": {
                    "action": "query", "capture_id": "cap-mcp-window", "view": "window",
                    "series": ["s0"], "from_us": 2_000, "to_us": 2_000,
                    "mode": "raw"
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
