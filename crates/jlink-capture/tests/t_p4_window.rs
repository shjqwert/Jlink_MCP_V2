//! Primary T-P4-WINDOW raw, explicit aggregate, transition, and event-neighborhood assertions.

use jlink_capture::{
    CaptureClock, CaptureEventKind, CapturePhase, CaptureSnapshot, CaptureStore,
    CaptureTimeRelation, CaptureWindow, CaptureWindowMode, CaptureWindowQuery, around_event,
    window,
};
use jlink_domain::{
    AccessLayout, AccessPlan, FirmwareIdentityPlan, HssDataIntegrity, HssDrainTiming,
    HssQualityEvent, HssQualityEventKind, HssQualityEvidence, HssQualityTracker, HssReturnWhen,
    HssRunSnapshot, HssRunState, HssStartPlan, HssWriteKind, HssWriteResult, HssWriteTiming,
    ScalarEncoding, TargetConnectionSpec, TargetInterface, VariableSelector,
};
use serde_json::json;

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
        "window-fixture",
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
        Some(260_106_173),
        None,
    )
    .expect("target fixture")
}

fn record(timestamp_ms: u32, value: u32) -> Vec<u8> {
    [timestamp_ms.to_le_bytes(), value.to_le_bytes()].concat()
}

fn completed_capture() -> (tempfile::TempDir, CaptureSnapshot) {
    let temporary = tempfile::tempdir().expect("temporary store");
    let store = CaptureStore::open(temporary.path()).expect("store");
    let plan = plan();
    let payload = [record(0, 8), record(1, 8), record(2, 12), record(3, 10)].concat();
    let mut tracker = HssQualityTracker::new(&plan, 0);
    tracker
        .observe_complete_records(plan.frame_layout(), &payload, 3_000)
        .expect("quality fixture");
    let mut quality = tracker.summary(0);
    quality.events.push(HssQualityEvent {
        kind: HssQualityEventKind::SampleInterval,
        evidence: HssQualityEvidence::Suspected,
        first_host_elapsed_us: 1_000,
        last_host_elapsed_us: 1_200,
        first_record: 1,
        last_record: 2,
        occurrences: 1,
    });
    let status = HssRunSnapshot {
        capture_id: "cap-window".to_owned(),
        state: HssRunState::Completed,
        integrity: HssDataIntegrity::Unknown,
        elapsed_us: 4_000,
        complete_records: 4,
        drain: HssDrainTiming::default(),
        quality,
        writes: vec![HssWriteTiming {
            request_id: "write-1".to_owned(),
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
        .create_writer("cap-window", &target(), &plan, 16 * 1024 * 1024)
        .expect("writer");
    writer
        .append(3_000, CapturePhase::Live, &payload)
        .expect("checksummed payload");
    let snapshot = writer.finish(&status).expect("immutable capture");
    (temporary, snapshot)
}

fn query(mode: CaptureWindowMode, limit: usize) -> CaptureWindowQuery {
    CaptureWindowQuery::new(vec!["fixture.value".to_owned()], 0, 4_000, mode, limit)
        .expect("window query")
}

#[test]
fn t_p4_window_raw_preserves_repeated_values_and_transitions_are_explicit() {
    let (_temporary, snapshot) = completed_capture();
    let CaptureWindow::Rows(raw) =
        window(&snapshot, &query(CaptureWindowMode::Raw, 10)).expect("raw window")
    else {
        panic!("raw mode returns rows");
    };
    assert_eq!(raw.clock, CaptureClock::Sample);
    assert_eq!(
        raw.dictionary,
        [("s0".to_owned(), "fixture.value".to_owned())].into()
    );
    assert_eq!(raw.time_us, vec![0, 1_000, 2_000, 3_000]);
    assert_eq!(
        raw.values["s0"],
        vec![json!(8), json!(8), json!(12), json!(10)]
    );
    assert!(!raw.truncated);

    let CaptureWindow::Rows(transitions) =
        window(&snapshot, &query(CaptureWindowMode::Transitions, 10)).expect("transition window")
    else {
        panic!("transitions mode returns rows");
    };
    assert_eq!(transitions.time_us, vec![2_000, 3_000]);
    assert_eq!(transitions.values["s0"], vec![json!(12), json!(10)]);

    let CaptureWindow::Rows(bounded) =
        window(&snapshot, &query(CaptureWindowMode::Raw, 3)).expect("bounded raw")
    else {
        panic!("raw mode returns rows");
    };
    assert_eq!(bounded.values["s0"], vec![json!(8), json!(8), json!(12)]);
    assert!(bounded.truncated);
}

#[test]
fn t_p4_window_aggregates_only_after_explicit_mode_selection() {
    let (_temporary, snapshot) = completed_capture();
    let CaptureWindow::Buckets(min_max) = window(
        &snapshot,
        &query(CaptureWindowMode::MinMax { points: 2 }, 10),
    )
    .expect("min max window") else {
        panic!("min_max mode returns buckets");
    };
    assert_eq!(min_max.buckets.len(), 2);
    assert_eq!(min_max.buckets[0].from_us, 0);
    assert_eq!(min_max.buckets[0].to_us, 2_000);
    assert_eq!(min_max.buckets[0].values["s0"], [json!(8), json!(8)]);
    assert_eq!(min_max.buckets[1].values["s0"], [json!(10), json!(12)]);

    let CaptureWindow::Buckets(first_last) = window(
        &snapshot,
        &query(CaptureWindowMode::FirstLast { points: 2 }, 10),
    )
    .expect("first last window") else {
        panic!("first_last mode returns buckets");
    };
    assert_eq!(first_last.buckets[0].values["s0"], [json!(8), json!(8)]);
    assert_eq!(first_last.buckets[1].values["s0"], [json!(12), json!(10)]);
}

#[test]
fn t_p4_window_around_event_reuses_sample_bounds_without_returning_raw_waveform() {
    let (_temporary, snapshot) = completed_capture();
    let mut legacy_status = serde_json::to_value(snapshot.status()).expect("status JSON");
    legacy_status["writes"][0]
        .as_object_mut()
        .expect("write object")
        .remove("kind");
    let legacy_status: HssRunSnapshot =
        serde_json::from_value(legacy_status).expect("legacy write manifest");
    assert_eq!(legacy_status.writes[0].kind, HssWriteKind::TargetWrite);

    let around = around_event(&snapshot, "e0", 0, 0, 10).expect("event neighborhood");
    assert_eq!(around.event.kind, CaptureEventKind::MemoryWrite);
    assert_eq!(around.event.request_id.as_deref(), Some("write-1"));
    assert_eq!(around.window.from_us, 0);
    assert_eq!(around.window.to_us, 3_100);
    assert_eq!(around.changes.len(), 2);
    assert_eq!(around.relations[0].relation, CaptureTimeRelation::Overlaps);
    assert_eq!(
        around.relations[1].relation,
        CaptureTimeRelation::Indeterminate
    );
    assert_eq!(around.quality.len(), 1);
    assert!(!around.truncated);

    let reusable = CaptureWindowQuery::new(
        vec!["s0".to_owned()],
        around.window.from_us,
        around.window.to_us,
        CaptureWindowMode::Raw,
        10,
    )
    .expect("reusable bounds");
    let CaptureWindow::Rows(raw) = window(&snapshot, &reusable).expect("reused raw window") else {
        panic!("raw mode returns rows");
    };
    assert_eq!(raw.time_us, vec![0, 1_000, 2_000, 3_000]);

    let missing = around_event(&snapshot, "e99", 0, 0, 10).expect_err("unknown event ID");
    assert_eq!(missing.code, jlink_domain::ErrorCode::ValueInvalid);
}
