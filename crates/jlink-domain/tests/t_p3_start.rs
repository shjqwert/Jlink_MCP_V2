//! Primary T-P3-START request, frame, capability, and idempotency assertions.

use jlink_domain::{
    AccessLayout, AccessPlan, ErrorCode, FirmwareIdentityPlan, HSS_MAX_EXPANDED_SAMPLE_BYTES,
    HssCapabilities, HssReservationOutcome, HssReturnWhen, HssStartPlan, HssStartRegistry,
    HssThresholdRule, ScalarEncoding, VariableSelector,
};
use serde_json::json;

fn firmware() -> FirmwareIdentityPlan {
    serde_json::from_value(json!({
        "elf_sha256": "11".repeat(32),
        "segments": [{
            "address": 0,
            "length": 4,
            "sha256": "22".repeat(32)
        }]
    }))
    .expect("valid firmware identity fixture")
}

fn scalar_plan(index: u32, byte_size: u64) -> AccessPlan {
    AccessPlan::new(
        "11".repeat(32),
        VariableSelector::new(&format!("fixture{index}"), None).expect("selector"),
        0x2000_0000 + u64::from(index) * 0x100,
        byte_size,
        None,
        false,
        AccessLayout::Scalar {
            name: "uint32_t".to_owned(),
            byte_size,
            encoding: ScalarEncoding::Unsigned,
        },
    )
}

fn start_plan(duration_s: u32) -> HssStartPlan {
    start_plan_with_rules(duration_s, Vec::new())
}

fn start_plan_with_rules(duration_s: u32, rules: Vec<HssThresholdRule>) -> HssStartPlan {
    HssStartPlan::new(
        "capture-key",
        duration_s,
        1_000,
        HssReturnWhen::Started,
        (0..10).map(|index| scalar_plan(index, 4)).collect(),
        rules,
        firmware(),
    )
    .expect("valid 10x32-bit start plan")
}

#[test]
fn t_p3_start_builds_fixed_ten_selector_frame_and_frozen_timestamp_preflight() {
    let plan = start_plan(300);
    assert_eq!(plan.variables().len(), 10);
    assert_eq!(plan.frame_layout().sample_bytes(), 40);
    assert_eq!(plan.frame_layout().record_bytes(), 44);
    assert_eq!(plan.period_us(), 1_000);
    assert_eq!(plan.variables()[9].sample_offset(), 36);
    assert_eq!(plan.request_fingerprint().len(), 64);

    let caps = HssCapabilities::frozen_698a(10, 1_000, 2, [0; 5]).expect("frozen 6.98a caps");
    caps.validate_start(&plan).expect("caps cover request");
    assert_eq!(caps.source_timestamp_frequency_hz(), 1_000);
    assert_eq!(caps.source_timestamp_resolution_us(), 1_000);
    assert!(caps.source_timestamp_monotonic());
}

#[test]
fn t_p3_start_rejects_top_level_frame_and_observed_capability_overruns() {
    let too_many = HssStartPlan::new(
        "too-many",
        1,
        1,
        HssReturnWhen::Completed,
        (0..11).map(|index| scalar_plan(index, 1)).collect(),
        Vec::new(),
        firmware(),
    )
    .expect_err("eleven top-level selectors must fail");
    assert_eq!(too_many.code(), ErrorCode::ValueInvalid);

    let too_wide = HssStartPlan::new(
        "too-wide",
        1,
        1,
        HssReturnWhen::Completed,
        vec![scalar_plan(0, u64::from(HSS_MAX_EXPANDED_SAMPLE_BYTES) + 1)],
        Vec::new(),
        firmware(),
    )
    .expect_err("expanded frame beyond F0-A evidence must fail");
    assert_eq!(too_wide.code(), ErrorCode::HssUnsupported);

    let plan = start_plan(1);
    let block_error = HssCapabilities::frozen_698a(9, 1_000, 2, [0; 5])
        .expect("valid but smaller caps")
        .validate_start(&plan)
        .expect_err("observed block limit must be enforced");
    assert_eq!(block_error.code(), ErrorCode::HssUnsupported);
    let rate_error = HssCapabilities::frozen_698a(10, 999, 2, [0; 5])
        .expect("valid but slower caps")
        .validate_start(&plan)
        .expect_err("observed frequency limit must be enforced");
    assert_eq!(rate_error.code(), ErrorCode::HssUnsupported);
    assert_eq!(
        HssCapabilities::frozen_698a(10, 1_000, 1, [0; 5])
            .expect_err("experimental timestamp flags are not the frozen mainline")
            .code(),
        ErrorCode::HssUnsupported
    );
}

#[test]
fn t_p3_start_normalizes_rules_and_binds_them_to_capture_key_idempotency() {
    let rule = |id: &str, value: u32| {
        serde_json::from_value::<HssThresholdRule>(json!({
            "kind": "equals",
            "id": id,
            "path": "fixture0",
            "value": value
        }))
        .expect("typed threshold rule")
    };
    let first = start_plan_with_rules(3, vec![rule("r1", 2), rule("r0", 1)]);
    let reordered = start_plan_with_rules(3, vec![rule("r0", 1), rule("r1", 2)]);
    assert_eq!(first.request_fingerprint(), reordered.request_fingerprint());
    assert_eq!(first.rules()[0].id(), "r0");

    let mut registry = HssStartRegistry::new();
    registry
        .reserve("260106173", &first)
        .expect("reserve normalized rules");
    assert!(matches!(
        registry.reserve("260106173", &reordered),
        Ok(HssReservationOutcome::Existing(_))
    ));

    let conflict = registry
        .reserve(
            "260106173",
            &start_plan_with_rules(3, vec![rule("r0", 9), rule("r1", 2)]),
        )
        .expect_err("different start rule must conflict under the same key");
    assert_eq!(conflict.code(), ErrorCode::CaptureKeyConflict);
}

#[test]
fn t_p3_start_capture_key_is_idempotent_and_conflicts_with_original_fingerprint() {
    let mut registry = HssStartRegistry::new();
    let original = start_plan(3);
    let HssReservationOutcome::Created(created) = registry
        .reserve("260106173", &original)
        .expect("reserve capture key")
    else {
        panic!("first reservation must create");
    };
    let HssReservationOutcome::Existing(existing) = registry
        .reserve("260106173", &original)
        .expect("recover equivalent request")
    else {
        panic!("same request must recover");
    };
    assert_eq!(created, existing);
    assert!(created.capture_id().starts_with("cap_"));

    let conflict = registry
        .reserve("260106173", &start_plan(4))
        .expect_err("different duration under same key must conflict");
    assert_eq!(conflict.code(), ErrorCode::CaptureKeyConflict);
    let details = conflict.details.expect("conflict evidence");
    assert_eq!(details["capture_id"], created.capture_id());
    assert_eq!(
        details["original_request_fingerprint"],
        original.request_fingerprint()
    );
}

#[test]
fn t_p3_start_revalidates_derived_fields_after_ipc_transport() {
    let plan = start_plan(1);
    let mut value = serde_json::to_value(plan).expect("serialize plan");
    value["request_fingerprint"] = json!("00".repeat(32));
    let tampered: HssStartPlan = serde_json::from_value(value).expect("wire shape remains valid");
    assert_eq!(
        tampered
            .validate()
            .expect_err("derived fingerprint tampering must fail")
            .code(),
        ErrorCode::ValueInvalid
    );
}
