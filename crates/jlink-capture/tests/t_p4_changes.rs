//! Primary T-P4-CHANGES exact-change, threshold, and wildcard assertions.

use jlink_capture::{CaptureChangesQuery, CapturePhase, CaptureSnapshot, CaptureStore, changes};
use jlink_domain::{
    AccessLayout, AccessMember, AccessPlan, ErrorCode, FirmwareIdentityPlan, HssDataIntegrity,
    HssDrainTiming, HssQualityTracker, HssReturnWhen, HssRunSnapshot, HssRunState, HssStartPlan,
    HssThresholdRule, ScalarEncoding, TargetConnectionSpec, TargetInterface, VariableSelector,
};
use serde_json::{Value, json};

fn rule(value: Value) -> HssThresholdRule {
    serde_json::from_value(value).expect("rule fixture")
}

fn start_rule() -> HssThresholdRule {
    rule(json!({
        "kind": "crosses",
        "id": "r-start",
        "path": "channels[*].temperature",
        "value": 10,
        "direction": "up"
    }))
}

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
    let temperature = AccessLayout::Scalar {
        name: "uint32_t".to_owned(),
        byte_size: 4,
        encoding: ScalarEncoding::Unsigned,
    };
    let channel = AccessLayout::Structure {
        byte_size: 4,
        members: vec![AccessMember::new(
            "temperature".to_owned(),
            0,
            None,
            None,
            None,
            temperature,
        )],
    };
    let access = AccessPlan::new(
        "11".repeat(32),
        VariableSelector::new("channels", None).expect("selector"),
        0x2000_0000,
        8,
        None,
        false,
        AccessLayout::Array {
            element: Box::new(channel),
            count: Some(2),
        },
    );
    HssStartPlan::new(
        "changes-fixture",
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

fn completed_capture() -> (tempfile::TempDir, CaptureSnapshot) {
    let temporary = tempfile::tempdir().expect("temporary store");
    let store = CaptureStore::open(temporary.path()).expect("store");
    let plan = plan();
    let payload = [record(0, 8, 12), record(1, 11, 12), record(2, 11, 9)].concat();
    let mut tracker = HssQualityTracker::new(&plan, 0);
    tracker
        .observe_complete_records(plan.frame_layout(), &payload, 2_000)
        .expect("quality fixture");
    let status = HssRunSnapshot {
        capture_id: "cap-changes".to_owned(),
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
        .create_writer("cap-changes", &target(), &plan, 16 * 1024 * 1024)
        .expect("writer");
    writer
        .append(2_000, CapturePhase::Live, &payload)
        .expect("checksummed payload");
    let snapshot = writer.finish(&status).expect("immutable capture");
    (temporary, snapshot)
}

#[test]
fn t_p4_changes_keeps_exact_facts_separate_from_start_or_query_thresholds() {
    let (_temporary, snapshot) = completed_capture();
    let start_default = changes(
        &snapshot,
        &CaptureChangesQuery::new(None, None, None, None, 200).expect("default query"),
    )
    .expect("changes from start rule");
    let query_equivalent = changes(
        &snapshot,
        &CaptureChangesQuery::new(None, None, None, Some(vec![start_rule()]), 200)
            .expect("query-time rule"),
    )
    .expect("changes from query rule");
    assert_eq!(start_default, query_equivalent);
    assert_eq!(
        start_default.dictionary,
        [
            ("s0.0".to_owned(), "channels[0].temperature".to_owned()),
            ("s0.1".to_owned(), "channels[1].temperature".to_owned()),
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(start_default.changes.len(), 2);
    assert_eq!(start_default.changes[0].series, "s0.0");
    assert_eq!(start_default.changes[0].after_us, 0);
    assert_eq!(start_default.changes[0].observed_by_us, 1_000);
    assert_eq!(start_default.changes[0].from, 8);
    assert_eq!(start_default.changes[0].to, 11);
    assert_eq!(start_default.changes[1].series, "s0.1");
    assert_eq!(start_default.matches.len(), 1);
    assert_eq!(start_default.matches[0].rule, "r-start");
    assert_eq!(start_default.matches[0].series, "s0.0");
    assert!(!start_default.truncated);
}

#[test]
fn t_p4_changes_evaluates_all_closed_rules_in_stable_wildcard_order() {
    let (_temporary, snapshot) = completed_capture();
    let rules = vec![
        rule(json!({
            "kind": "outside", "id": "r-outside",
            "path": "channels[*].temperature", "min": 10, "max": 11
        })),
        rule(json!({
            "kind": "equals", "id": "r-equals",
            "path": "channels[*].temperature", "value": 11
        })),
        rule(json!({
            "kind": "abs_delta_gte", "id": "r-abs",
            "path": "channels[*].temperature", "value": 3
        })),
        rule(json!({
            "kind": "crosses", "id": "r-cross",
            "path": "channels[*].temperature", "value": 10, "direction": "down"
        })),
    ];
    let result = changes(
        &snapshot,
        &CaptureChangesQuery::new(None, None, None, Some(rules), 200).expect("closed rules"),
    )
    .expect("wildcard changes");
    assert_eq!(
        result
            .matches
            .iter()
            .map(|item| (
                item.observed_by_us,
                item.rule.as_str(),
                item.series.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            (1_000, "r-abs", "s0.0"),
            (1_000, "r-equals", "s0.0"),
            (1_000, "r-outside", "s0.1"),
            (2_000, "r-abs", "s0.1"),
            (2_000, "r-cross", "s0.1"),
            (2_000, "r-outside", "s0.1"),
        ]
    );
}

#[test]
fn t_p4_changes_bounds_rows_and_rejects_arbitrary_rule_paths() {
    let (_temporary, snapshot) = completed_capture();
    let selected = changes(
        &snapshot,
        &CaptureChangesQuery::new(
            Some(vec!["channels[1].temperature".to_owned()]),
            None,
            None,
            Some(Vec::new()),
            200,
        )
        .expect("exact leaf query"),
    )
    .expect("selected changes");
    assert_eq!(selected.changes.len(), 1);
    assert_eq!(selected.changes[0].series, "s0.1");

    let bounded = changes(
        &snapshot,
        &CaptureChangesQuery::new(None, None, None, None, 2).expect("bounded query"),
    )
    .expect("bounded changes");
    assert_eq!(bounded.changes.len(), 1);
    assert_eq!(bounded.matches.len(), 1);
    assert!(bounded.truncated);

    let invalid = CaptureChangesQuery::new(
        None,
        None,
        None,
        Some(vec![rule(json!({
            "kind": "equals", "id": "script-like",
            "path": "channels[?].temperature", "value": 11
        }))]),
        200,
    )
    .expect_err("arbitrary path grammar is rejected");
    assert_eq!(invalid.code, ErrorCode::ValueInvalid);
}
