//! Primary T-P4-TIMELINE cross-clock relation and immutable cursor assertions.

use std::{fs, io::Cursor, path::PathBuf};

use jlink_capture::{CapturePhase, CaptureStore};
use jlink_domain::{
    AccessLayout, AccessPlan, FirmwareIdentityPlan, HssDataIntegrity, HssDrainTiming,
    HssQualityEvent, HssQualityEventKind, HssQualityEvidence, HssQualityTracker, HssReturnWhen,
    HssRunSnapshot, HssRunState, HssStartPlan, HssWriteKind, HssWriteResult, HssWriteTiming,
    ScalarEncoding, TargetConnectionSpec, TargetInterface, VariableSelector, probe_identity_hash,
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
            "length": 8,
            "sha256": "22".repeat(32)
        }]
    }))
    .expect("firmware fixture");
    let scalar = || AccessLayout::Scalar {
        name: "uint32_t".to_owned(),
        byte_size: 4,
        encoding: ScalarEncoding::Unsigned,
    };
    let variable = |path: &str, address| {
        AccessPlan::new(
            "11".repeat(32),
            VariableSelector::new(path, None).expect("selector"),
            address,
            4,
            None,
            false,
            scalar(),
        )
    };
    HssStartPlan::new(
        "timeline-key",
        1,
        1_000,
        HssReturnWhen::Completed,
        vec![
            variable("fixture.first", 0x2000_0000),
            variable("fixture.second", 0x2000_0004),
        ],
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
    write_query_configs(&project, &user);

    let lease_root = temporary.path().join("leases");
    let store_root = lease_root
        .join("captures")
        .join(probe_identity_hash(&PROBE_SERIAL.to_string()).expect("probe identity hash"));
    let store = CaptureStore::open(store_root).expect("capture store");
    let plan = plan();
    let payload = [
        0_u32.to_le_bytes(),
        0_u32.to_le_bytes(),
        0_u32.to_le_bytes(),
        1_u32.to_le_bytes(),
        1_u32.to_le_bytes(),
        0_u32.to_le_bytes(),
        2_u32.to_le_bytes(),
        1_u32.to_le_bytes(),
        1_u32.to_le_bytes(),
    ]
    .concat();
    let mut tracker = HssQualityTracker::new(&plan, 0);
    tracker
        .observe_complete_records(plan.frame_layout(), &payload, 2_000)
        .expect("quality fixture");
    let mut quality = tracker.summary(0);
    quality.clock.mapping_error_us = Some(100);
    quality.events.push(HssQualityEvent {
        kind: HssQualityEventKind::SampleInterval,
        evidence: HssQualityEvidence::Suspected,
        first_host_elapsed_us: 1_500,
        last_host_elapsed_us: 1_500,
        first_record: 1,
        last_record: 2,
        occurrences: 1,
    });
    let status = HssRunSnapshot {
        capture_id: "cap-timeline".to_owned(),
        state: HssRunState::Completed,
        integrity: HssDataIntegrity::Unknown,
        elapsed_us: 3_000,
        complete_records: 3,
        drain: HssDrainTiming::default(),
        quality,
        writes: vec![HssWriteTiming {
            request_id: "timeline-write".to_owned(),
            kind: HssWriteKind::MemoryWrite,
            requested_at_us: 0,
            started_at_us: 0,
            completed_at_us: 100,
            result: HssWriteResult::Succeeded,
            samples_before: 1,
            samples_after_next_drain: Some(2),
        }],
        failure_code: None,
        partial_available: false,
        reason: None,
        recoverable: None,
        recovery_notifications: Vec::new(),
    };
    let mut writer = store
        .create_writer("cap-timeline", &target(), &plan, 16 * 1024 * 1024)
        .expect("capture writer");
    writer
        .append(2_000, CapturePhase::Live, &payload)
        .expect("checksummed payload");
    let snapshot = writer.finish(&status).expect("immutable completion");
    let capture_path = snapshot.path().to_owned();

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

fn changes_request() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "jlink_hss", "arguments": {
            "action": "query", "capture_id": "cap-timeline", "view": "changes",
            "rules": [], "limit": 1
        }}
    })
}

#[test]
fn t_p4_timeline_pages_an_immutable_snapshot_with_incremental_dictionary() {
    let (_temporary, mut runtime, _capture_path) = runtime_fixture();
    let first = exchange(&[changes_request()], &mut runtime);
    let first = &first[0]["result"]["structuredContent"];
    assert_eq!(first["changes"].as_array().map(Vec::len), Some(1));
    assert_eq!(first["dictionary"], json!({ "s0": "fixture.first" }));
    assert_eq!(first["events"].as_array().map(Vec::len), Some(1));
    assert_eq!(first["events"][0]["kind"], "memory_write");
    assert_eq!(first["events"][0]["sample_relation"], "overlaps");
    assert_eq!(first["relations"][0]["relation"], "overlaps");
    assert_eq!(first["truncated"], true);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("truncated first page cursor")
        .to_owned();

    let second = exchange(
        &[json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "jlink_hss", "arguments": {
                "action": "query", "capture_id": "cap-timeline", "cursor": cursor
            }}
        })],
        &mut runtime,
    );
    let second = &second[0]["result"]["structuredContent"];
    assert_eq!(second["changes"].as_array().map(Vec::len), Some(1));
    assert_eq!(second["dictionary"], json!({ "s1": "fixture.second" }));
    assert_eq!(second["events"].as_array().map(Vec::len), Some(1));
    assert_eq!(second["events"][0]["kind"], "quality_sample_interval");
    assert_eq!(second["relations"][0]["relation"], "overlaps");
    assert_eq!(second["truncated"], false);
    assert!(second.get("next_cursor").is_none());
}

#[test]
fn t_p4_timeline_cursor_preserves_raw_rows_and_omits_unchanged_dictionary() {
    let (_temporary, mut runtime, _capture_path) = runtime_fixture();
    let first = exchange(
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "jlink_hss", "arguments": {
                "action": "query", "capture_key": "timeline-key", "view": "window",
                "series": ["s0", "s1"], "from_us": 0, "to_us": 3_000,
                "mode": "raw", "limit": 2
            }}
        })],
        &mut runtime,
    );
    let first = &first[0]["result"]["structuredContent"];
    assert_eq!(first["time_us"], json!([0, 1_000]));
    assert_eq!(
        first["dictionary"],
        json!({ "s0": "fixture.first", "s1": "fixture.second" })
    );
    assert_eq!(first["truncated"], true);
    assert_eq!(first["quality"].as_array().map(Vec::len), Some(1));
    let cursor = first["next_cursor"]
        .as_str()
        .expect("raw first page cursor");

    let second = exchange(
        &[json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "jlink_hss", "arguments": {
                "action": "query", "capture_key": "timeline-key", "cursor": cursor
            }}
        })],
        &mut runtime,
    );
    let second = &second[0]["result"]["structuredContent"];
    assert_eq!(second["time_us"], json!([2_000]));
    assert_eq!(second["values"]["s0"], json!([1]));
    assert_eq!(second["values"]["s1"], json!([1]));
    assert_eq!(second["dictionary"], json!({}));
    assert_eq!(second["quality"], json!([]));
    assert_eq!(second["truncated"], false);
    assert!(second.get("next_cursor").is_none());
}

#[test]
fn t_p4_timeline_cursor_continues_event_neighborhood_changes() {
    let (_temporary, mut runtime, _capture_path) = runtime_fixture();
    let first = exchange(
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "jlink_hss", "arguments": {
                "action": "query", "capture_id": "cap-timeline", "view": "around_event",
                "event_id": "e0", "before_us": 0, "after_us": 2_000, "limit": 1
            }}
        })],
        &mut runtime,
    );
    let first = &first[0]["result"]["structuredContent"];
    assert_eq!(first["changes"].as_array().map(Vec::len), Some(1));
    assert_eq!(first["dictionary"], json!({ "s0": "fixture.first" }));
    assert_eq!(first["truncated"], true);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("around_event first page cursor");

    let second = exchange(
        &[json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "jlink_hss", "arguments": {
                "action": "query", "capture_id": "cap-timeline", "cursor": cursor
            }}
        })],
        &mut runtime,
    );
    let second = &second[0]["result"]["structuredContent"];
    assert_eq!(second["changes"].as_array().map(Vec::len), Some(1));
    assert_eq!(second["dictionary"], json!({ "s1": "fixture.second" }));
    assert_eq!(second["event"]["id"], "e0");
    assert_eq!(second["relations"][0]["relation"], "before");
    assert_eq!(second["truncated"], false);
    assert!(second.get("next_cursor").is_none());
}

#[test]
fn t_p4_timeline_rejects_tampered_and_expired_cursors_without_restart_fallback() {
    let (_temporary, mut runtime, capture_path) = runtime_fixture();
    let first = exchange(&[changes_request()], &mut runtime);
    let cursor = first[0]["result"]["structuredContent"]["next_cursor"]
        .as_str()
        .expect("first page cursor")
        .to_owned();
    let mut tampered = cursor.clone();
    let replacement = if tampered.ends_with('0') { "1" } else { "0" };
    tampered.replace_range(tampered.len() - 1.., replacement);
    let invalid = exchange(
        &[json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "jlink_hss", "arguments": {
                "action": "query", "capture_id": "cap-timeline", "cursor": tampered
            }}
        })],
        &mut runtime,
    );
    assert_eq!(
        invalid[0]["result"]["structuredContent"]["error"]["code"],
        "CURSOR_INVALID"
    );

    let wrong_identity = exchange(
        &[json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "jlink_hss", "arguments": {
                "action": "query", "capture_id": "different-capture", "cursor": cursor.clone()
            }}
        })],
        &mut runtime,
    );
    assert_eq!(
        wrong_identity[0]["result"]["structuredContent"]["error"]["code"],
        "CURSOR_INVALID"
    );

    fs::remove_file(capture_path).expect("remove temporary immutable capture");
    let expired = exchange(
        &[json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "jlink_hss", "arguments": {
                "action": "query", "capture_id": "cap-timeline", "cursor": cursor
            }}
        })],
        &mut runtime,
    );
    assert_eq!(
        expired[0]["result"]["structuredContent"]["error"]["code"],
        "CURSOR_EXPIRED"
    );
}
