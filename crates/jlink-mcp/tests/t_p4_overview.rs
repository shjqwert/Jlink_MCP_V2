//! Primary T-P4-OVERVIEW MCP routing, strict Schema, and immutable lookup assertions.

use std::{fs, io::Cursor, path::PathBuf};

use jlink_capture::{CapturePhase, CaptureStore};
use jlink_domain::{
    AccessLayout, AccessPlan, FirmwareIdentityPlan, HssDataIntegrity, HssDrainTiming,
    HssQualityTracker, HssReturnWhen, HssRunSnapshot, HssRunState, HssStartPlan, ScalarEncoding,
    TargetConnectionSpec, TargetInterface, VariableSelector, probe_identity_hash,
};
use jlink_mcp::{
    config::{ConfigFile, ConfigPaths, FirmwareConfig, JlinkConfig, ProbeConfig, TargetConfig},
    mcp::{RAW_CAPTURE_MIME, serve},
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
        "mcp-overview-key",
        1,
        2,
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

fn runtime_fixture() -> (tempfile::TempDir, Runtime, PathBuf) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let project = temporary.path().join("jlink-mcp.toml");
    let user = temporary.path().join("user.toml");
    let project_config = ConfigFile {
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
    };
    let user_config = ConfigFile {
        probe: Some(ProbeConfig {
            serial: Some(PROBE_SERIAL),
        }),
        ..ConfigFile::default()
    };
    fs::write(
        &project,
        toml::to_string_pretty(&project_config).expect("project TOML"),
    )
    .expect("project config");
    fs::write(
        &user,
        toml::to_string_pretty(&user_config).expect("user TOML"),
    )
    .expect("user config");
    let lease_root = temporary.path().join("leases");
    let store_root = lease_root
        .join("captures")
        .join(probe_identity_hash(&PROBE_SERIAL.to_string()).expect("probe identity hash"));
    let store = CaptureStore::open(&store_root).expect("capture store");
    let plan = plan();
    let payload = [
        10_u32.to_le_bytes(),
        1_u32.to_le_bytes(),
        510_u32.to_le_bytes(),
        2_u32.to_le_bytes(),
    ]
    .concat();
    let mut tracker = HssQualityTracker::new(&plan, 0);
    tracker
        .observe_complete_records(plan.frame_layout(), &payload, 510_000)
        .expect("quality fixture");
    let snapshot = HssRunSnapshot {
        capture_id: "cap-mcp-overview".to_owned(),
        state: HssRunState::Completed,
        integrity: HssDataIntegrity::Unknown,
        elapsed_us: 511_000,
        complete_records: 2,
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
        .create_writer("cap-mcp-overview", &target(), &plan, 16 * 1024 * 1024)
        .expect("capture writer");
    writer
        .append(510_000, CapturePhase::Live, &payload)
        .expect("checksummed payload");
    writer.finish(&snapshot).expect("immutable completion");

    let runtime = Runtime::new(
        ConfigPaths::new(project, user),
        temporary.path().join("worker-must-not-run.exe"),
        lease_root,
    );
    (temporary, runtime, store_root)
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
fn t_p4_overview_accepts_id_or_key_and_rejects_unknown_view_before_store_read() {
    let (_temporary, mut runtime, store_root) = runtime_fixture();
    let before = fs::read_dir(&store_root).expect("store entries").count();
    let responses = exchange(
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "jlink_hss",
                    "arguments": {
                        "action": "query",
                        "capture_key": "mcp-overview-key",
                        "view": "overview"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "jlink_hss",
                    "arguments": {
                        "action": "query",
                        "capture_id": "cap-mcp-overview",
                        "view": "overview"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "jlink_hss",
                    "arguments": {
                        "action": "query",
                        "capture_key": "unknown-key",
                        "view": "overview"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "jlink_hss",
                    "arguments": {
                        "action": "query",
                        "capture_id": "cap-mcp-overview",
                        "view": "unknown"
                    }
                }
            }),
        ],
        &mut runtime,
    );

    let by_key = &responses[0]["result"];
    let by_id = &responses[1]["result"];
    assert_eq!(by_key["structuredContent"], by_id["structuredContent"]);
    assert_eq!(
        by_key["structuredContent"]["capture_id"],
        "cap-mcp-overview"
    );
    assert_eq!(by_key["structuredContent"]["from_us"], 10_000);
    assert_eq!(by_key["structuredContent"]["to_us"], 511_000);
    assert_eq!(
        by_key["structuredContent"]["variables"],
        json!([{ "series": "s0", "samples": 2, "changes": 1 }])
    );
    assert_eq!(by_key["content"][0]["type"], "resource_link");
    assert_eq!(by_key["content"][0]["mimeType"], RAW_CAPTURE_MIME);
    assert_eq!(
        by_key["content"][0]["uri"],
        "jlink-mcp://capture/cap-mcp-overview/raw"
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"]["error"]["code"],
        "VALUE_INVALID"
    );
    assert_eq!(responses[3]["error"]["code"], -32_602);
    assert_eq!(
        fs::read_dir(&store_root)
            .expect("unchanged store entries")
            .count(),
        before,
        "unknown identities and views must not create captures"
    );
}
