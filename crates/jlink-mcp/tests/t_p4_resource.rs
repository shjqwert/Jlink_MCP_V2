//! T-P4-RESOURCE primary coverage for immutable raw MCP capture resources.

use std::{fs, io::Cursor, path::PathBuf};

use data_encoding::BASE64;
use jlink_capture::{CapturePhase, CaptureStore};
use jlink_domain::{
    AccessLayout, AccessPlan, FirmwareIdentityPlan, HssDataIntegrity, HssDrainTiming,
    HssQualityTracker, HssReturnWhen, HssRunSnapshot, HssRunState, HssStartPlan, HssWriteKind,
    HssWriteResult, HssWriteTiming, ScalarEncoding, TargetConnectionSpec, TargetInterface,
    VariableSelector, probe_identity_hash,
};
use jlink_mcp::{
    config::{ConfigFile, ConfigPaths, JlinkConfig, ProbeConfig, TargetConfig},
    mcp::{RAW_CAPTURE_MIME, serve},
    runtime::Runtime,
};
use serde_json::{Value, json};

const CAPTURE_ID: &str = "cap-resource";
const PROBE_SERIAL: u32 = 260_106_173;

#[test]
fn t_p4_resource_reads_the_complete_self_describing_capture_while_disconnected() {
    let (_temporary, mut runtime, capture_path) = runtime_fixture();
    let uri = format!("jlink-mcp://capture/{CAPTURE_ID}/raw");
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "resources/read",
        "params": { "uri": uri }
    });
    let responses = exchange(&[request.clone(), request], &mut runtime);
    let first = &responses[0]["result"]["contents"][0];
    let second = &responses[1]["result"]["contents"][0];

    assert_eq!(first["uri"], uri);
    assert_eq!(first["mimeType"], RAW_CAPTURE_MIME);
    assert_eq!(first["blob"], second["blob"]);
    assert!(first.get("text").is_none());
    assert!(
        !first["mimeType"]
            .as_str()
            .expect("resource MIME")
            .starts_with("image/")
    );

    let decoded = BASE64
        .decode(first["blob"].as_str().expect("base64 blob").as_bytes())
        .expect("standard base64");
    assert_eq!(decoded, fs::read(&capture_path).expect("published capture"));
    assert_eq!(&decoded[..8], b"JMCPV101");

    let store = CaptureStore::open_existing(
        capture_path
            .parent()
            .expect("capture has one store directory"),
    )
    .expect("existing store")
    .expect("store remains available after disconnect");
    let snapshot = store.open_snapshot(CAPTURE_ID).expect("resource verifies");
    assert_eq!(snapshot.capture_id(), CAPTURE_ID);
    assert_eq!(snapshot.target(), &target());
    assert_eq!(snapshot.plan().firmware().elf_sha256(), "11".repeat(32));
    assert_eq!(snapshot.status().complete_records, 3);
    assert_eq!(snapshot.raw_sha256().len(), 64);
}

#[test]
fn t_p4_resource_rejects_noncanonical_and_missing_resources_without_fallback() {
    let (_temporary, mut runtime, _capture_path) = runtime_fixture();
    let responses = exchange(
        &[
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "resources/read",
                "params": { "uri": "jlink-mcp://capture/cap-resource/preview" }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "resources/read",
                "params": { "uri": "jlink-mcp://capture/missing/raw" }
            }),
        ],
        &mut runtime,
    );

    for response in responses {
        assert_eq!(response["error"]["code"], -32_002);
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("resource error message")
                .contains("VALUE_INVALID")
        );
        assert!(response.get("result").is_none());
    }
}

#[test]
fn t_p4_resource_rejects_bytes_that_no_longer_match_the_persisted_checksums() {
    let (_temporary, mut runtime, capture_path) = runtime_fixture();
    let payload = resource_payload();
    let mut resource = fs::read(&capture_path).expect("published capture");
    let payload_offset = resource
        .windows(payload.len())
        .position(|window| window == payload)
        .expect("raw payload remains present in the self-describing resource");
    resource[payload_offset] ^= 0xff;
    fs::write(&capture_path, resource).expect("corrupt isolated test capture");

    let responses = exchange(
        &[json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": { "uri": format!("jlink-mcp://capture/{CAPTURE_ID}/raw") }
        })],
        &mut runtime,
    );

    assert_eq!(responses[0]["error"]["code"], -32_002);
    assert!(
        responses[0]["error"]["message"]
            .as_str()
            .expect("resource error message")
            .contains("FRAME_INVALID")
    );
    assert!(responses[0].get("result").is_none());
}

#[test]
fn t_p4_stage_smoke_routes_all_query_views_to_one_immutable_resource() {
    let (_temporary, mut runtime, _capture_path) = runtime_fixture();
    let overview = exchange(
        &[hss_query(json!({
            "capture_id": CAPTURE_ID,
            "view": "overview"
        }))],
        &mut runtime,
    );
    assert!(
        overview[0].get("error").is_none(),
        "overview failed: {}",
        overview[0]
    );
    let resource_uri = overview[0]["result"]["content"][0]["uri"]
        .as_str()
        .expect("overview resource link")
        .to_owned();

    let first_changes = exchange(
        &[hss_query(json!({
            "capture_id": CAPTURE_ID,
            "view": "changes",
            "limit": 1
        }))],
        &mut runtime,
    );
    let cursor = first_changes[0]["result"]["structuredContent"]["next_cursor"]
        .as_str()
        .expect("changes continuation")
        .to_owned();
    let remaining_changes = exchange(
        &[hss_query(json!({
            "capture_id": CAPTURE_ID,
            "cursor": cursor
        }))],
        &mut runtime,
    );

    let remaining = &remaining_changes[0]["result"]["structuredContent"];
    assert_eq!(remaining["truncated"], false);
    assert!(remaining.get("next_cursor").is_none());
    for request in [
        hss_query(json!({
            "capture_id": CAPTURE_ID,
            "view": "window",
            "series": ["fixture.value"],
            "from_us": 0,
            "to_us": 3_000,
            "mode": "raw",
            "limit": 3
        })),
        hss_query(json!({
            "capture_id": CAPTURE_ID,
            "view": "around_event",
            "event_id": "e0",
            "before_us": 1_000,
            "after_us": 3_000,
            "limit": 3
        })),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": { "uri": resource_uri }
        }),
    ] {
        let response = exchange(&[request], &mut runtime);
        assert!(response[0].get("error").is_none());
        assert!(response[0].get("result").is_some());
    }
}

fn runtime_fixture() -> (tempfile::TempDir, Runtime, PathBuf) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let project = temporary.path().join("jlink-mcp.toml");
    let user = temporary.path().join("user.toml");
    write_query_configs(&project, &user);

    let lease_root = temporary.path().join("leases");
    let store_root = lease_root
        .join("captures")
        .join(probe_identity_hash(&PROBE_SERIAL.to_string()).expect("probe identity hash"));
    let capture_path = write_capture(&store_root);
    let runtime = Runtime::new(
        ConfigPaths::new(project, user),
        temporary.path().join("worker-must-not-run.exe"),
        lease_root,
    );
    (temporary, runtime, capture_path)
}

fn write_query_configs(project: &std::path::Path, user: &std::path::Path) {
    fs::write(
        project,
        toml::to_string_pretty(&ConfigFile {
            target: Some(TargetConfig {
                device: Some("S32K144".to_owned()),
                interface: Some(TargetInterface::Swd),
                speed_khz: Some(4_000),
            }),
            jlink: Some(JlinkConfig {
                dll_path: Some(PathBuf::from("unused-by-resource-read.dll")),
                version: Some("6.98a".to_owned()),
                sha256: Some("0".repeat(64)),
            }),
            ..ConfigFile::default()
        })
        .expect("project TOML"),
    )
    .expect("project config");
    fs::write(
        user,
        toml::to_string_pretty(&ConfigFile {
            probe: Some(ProbeConfig {
                serial: Some(PROBE_SERIAL),
            }),
            ..ConfigFile::default()
        })
        .expect("user TOML"),
    )
    .expect("user config");
}

fn write_capture(store_root: &std::path::Path) -> PathBuf {
    let store = CaptureStore::open(store_root).expect("capture store");
    let plan = plan();
    let payload = resource_payload();
    let mut tracker = HssQualityTracker::new(&plan, 0);
    tracker
        .observe_complete_records(plan.frame_layout(), &payload, 2_000)
        .expect("quality fixture");
    let status = HssRunSnapshot {
        capture_id: CAPTURE_ID.to_owned(),
        state: HssRunState::Completed,
        integrity: HssDataIntegrity::Complete,
        elapsed_us: 2_000,
        complete_records: 3,
        drain: HssDrainTiming::default(),
        quality: tracker.summary(0),
        writes: vec![HssWriteTiming {
            request_id: "resource-write".to_owned(),
            kind: HssWriteKind::MemoryWrite,
            requested_at_us: 0,
            started_at_us: 0,
            completed_at_us: 100,
            result: HssWriteResult::Succeeded,
            samples_before: 0,
            samples_after_next_drain: Some(1),
        }],
        failure_code: None,
        partial_available: false,
        reason: None,
        recoverable: None,
        recovery_notifications: Vec::new(),
    };
    let mut writer = store
        .create_writer(CAPTURE_ID, &target(), &plan, 16 * 1024 * 1024)
        .expect("capture writer");
    writer
        .append(2_000, CapturePhase::Live, &payload)
        .expect("checksummed payload");
    writer
        .finish(&status)
        .expect("immutable completion")
        .path()
        .to_owned()
}

fn resource_payload() -> Vec<u8> {
    [
        0_u32.to_le_bytes(),
        7_u32.to_le_bytes(),
        1_u32.to_le_bytes(),
        8_u32.to_le_bytes(),
        2_u32.to_le_bytes(),
        9_u32.to_le_bytes(),
    ]
    .concat()
}

fn plan() -> HssStartPlan {
    let firmware: FirmwareIdentityPlan = serde_json::from_value(json!({
        "elf_sha256": "11".repeat(32),
        "segments": [{ "address": 0, "length": 8, "sha256": "22".repeat(32) }]
    }))
    .expect("firmware fixture");
    let variable = AccessPlan::new(
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
        "resource-key",
        1,
        1_000,
        HssReturnWhen::Completed,
        vec![variable],
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

fn hss_query(arguments: Value) -> Value {
    let Value::Object(mut arguments) = arguments else {
        panic!("HSS query fixture is an object");
    };
    arguments.insert("action".to_owned(), json!("query"));
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "jlink_hss", "arguments": arguments }
    })
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
