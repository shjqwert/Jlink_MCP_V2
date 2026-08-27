//! Primary T-P4-OVERVIEW immutable snapshot and navigation-count assertions.

use std::{fs::OpenOptions, io::Write};

use jlink_capture::{CapturePhase, CaptureSnapshot, CaptureStore, overview};
use jlink_domain::{
    AccessLayout, AccessPlan, ErrorCode, FirmwareIdentityPlan, HssDataIntegrity, HssDrainTiming,
    HssQualityTracker, HssReturnWhen, HssRunSnapshot, HssRunState, HssStartPlan, HssWriteResult,
    HssWriteTiming, ScalarEncoding, TargetConnectionSpec, TargetInterface, VariableSelector,
};
use serde_json::json;

fn access(path: &str, address: u64) -> AccessPlan {
    AccessPlan::new(
        "11".repeat(32),
        VariableSelector::new(path, None).expect("selector"),
        address,
        4,
        None,
        false,
        AccessLayout::Scalar {
            name: "uint32_t".to_owned(),
            byte_size: 4,
            encoding: ScalarEncoding::Unsigned,
        },
    )
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
    HssStartPlan::new(
        "overview-fixture",
        1,
        3,
        HssReturnWhen::Completed,
        vec![
            access("fixture.first", 0x2000_0000),
            access("fixture.second", 0x2000_0004),
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
        Some(260_106_173),
        None,
    )
    .expect("target fixture")
}

fn record(timestamp_ms: u32, first: u32, second: u32) -> Vec<u8> {
    [
        timestamp_ms.to_le_bytes(),
        first.to_le_bytes(),
        second.to_le_bytes(),
    ]
    .concat()
}

fn assert_completed_overview(completed: &CaptureSnapshot) {
    let result = serde_json::to_value(overview(completed).expect("verified overview"))
        .expect("overview JSON");
    assert_eq!(result["capture_id"], "cap-overview");
    assert_eq!(result["from_us"], 10_000);
    assert_eq!(result["to_us"], 678_000);
    assert_eq!(
        result["dictionary"],
        json!({ "s0": "fixture.first", "s1": "fixture.second" })
    );
    assert_eq!(
        result["variables"],
        json!([
            { "series": "s0", "samples": 3, "changes": 1 },
            { "series": "s1", "samples": 3, "changes": 1 }
        ])
    );
    assert_eq!(result["events"], 1);
    assert_eq!(result["quality"]["integrity"], "unknown");
    assert_eq!(result["quality"]["loss"]["evidence"], "unknown");
    assert!(
        result["quality"].get("events").is_none(),
        "empty quality categories must remain omitted"
    );
    assert!(
        result["variables"][0].get("path").is_none(),
        "complete paths are registered only in the dictionary"
    );
}

fn assert_post_open_mutation_rejected(completed: &CaptureSnapshot) {
    OpenOptions::new()
        .append(true)
        .open(completed.path())
        .expect("open completed capture fixture")
        .write_all(&[0])
        .expect("append invalid trailing byte");
    assert_eq!(
        overview(completed)
            .expect_err("query must re-verify an immutable snapshot")
            .code,
        ErrorCode::FrameInvalid
    );
}

#[test]
fn t_p4_overview_reads_only_verified_completed_bytes_and_returns_top_level_counts() {
    let temporary = tempfile::tempdir().expect("temporary store");
    let missing = temporary.path().join("missing");
    assert!(
        CaptureStore::open_existing(&missing)
            .expect("read-only open")
            .is_none()
    );
    assert!(
        !missing.exists(),
        "read-only lookup must not create a store"
    );

    let store = CaptureStore::open(temporary.path()).expect("store opens");
    let plan = plan();
    let first_block = [record(10, 1, 5), record(343, 1, 6)].concat();
    let second_block = record(677, 2, 6);
    let payload = [first_block.clone(), second_block.clone()].concat();
    let mut tracker = HssQualityTracker::new(&plan, 0);
    tracker
        .observe_complete_records(plan.frame_layout(), &payload, 677_000)
        .expect("quality fixture");
    let snapshot = HssRunSnapshot {
        capture_id: "cap-overview".to_owned(),
        state: HssRunState::Completed,
        integrity: HssDataIntegrity::Unknown,
        elapsed_us: 678_000,
        complete_records: 3,
        drain: HssDrainTiming::default(),
        quality: tracker.summary(0),
        writes: vec![HssWriteTiming {
            request_id: "write-1".to_owned(),
            requested_at_us: 100_000,
            started_at_us: 101_000,
            completed_at_us: 102_000,
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
        .create_writer("cap-overview", &target(), &plan, 16 * 1024 * 1024)
        .expect("writer");
    writer
        .append(343_000, CapturePhase::Live, &first_block)
        .expect("first checksummed block");
    writer
        .append(677_000, CapturePhase::Tail, &second_block)
        .expect("second checksummed block");
    let completed = writer.finish(&snapshot).expect("immutable completion");

    assert_eq!(
        store.find_snapshot("cap-overview").expect("lookup by id"),
        Some(completed.clone())
    );
    assert_eq!(
        store
            .find_snapshot_by_key("overview-fixture")
            .expect("lookup by key"),
        Some(completed.clone())
    );
    assert!(
        store
            .find_snapshot_by_key("unknown-key")
            .expect("unknown key is a stable miss")
            .is_none()
    );

    assert_completed_overview(&completed);

    let mut failed = snapshot;
    failed.capture_id = "cap-failed".to_owned();
    failed.state = HssRunState::Failed;
    failed.integrity = HssDataIntegrity::Unknown;
    failed.failure_code = Some(ErrorCode::FrameInvalid);
    failed.partial_available = true;
    let mut failed_writer = store
        .create_writer("cap-failed", &target(), &plan, 16 * 1024 * 1024)
        .expect("failed capture writer");
    failed_writer
        .append(677_000, CapturePhase::Tail, &payload)
        .expect("failed capture payload");
    let failed_snapshot = failed_writer
        .finish(&failed)
        .expect("immutable failed capture");
    assert_eq!(
        overview(&failed_snapshot)
            .expect_err("partial terminal capture is not a complete overview")
            .code,
        ErrorCode::OperationConflict
    );
    assert_post_open_mutation_rejected(&completed);
}
