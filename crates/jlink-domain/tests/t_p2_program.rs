//! Primary test T-P2-PRG for pure PRG-001..PRG-006 programming rules.

use std::path::PathBuf;

use jlink_domain::{
    ErrorCode, ExecutionKind, FirmwareImage, FlashRange, FlashRegion, ProgramAfter, ProgramRequest,
    SessionCommand, VerifyMismatchAccumulator, validate_flash_range, validate_image_flash_ranges,
};

#[test]
fn t_p2_prg_rejects_ranges_outside_or_crossing_device_flash() {
    let regions = [
        FlashRegion::new(0x0000_0000, 0x0008_0000).expect("program Flash"),
        FlashRegion::new(0x1000_0000, 0x0001_0000).expect("data Flash"),
    ];
    let valid =
        FirmwareImage::parse("fixture.bin", &[1, 2, 3, 4], Some(0x0007_fffc)).expect("valid BIN");
    validate_image_flash_ranges(&valid, &regions).expect("segment ends on boundary");

    let crossing = FirmwareImage::parse("fixture.bin", &[1, 2, 3, 4], Some(0x0007_fffe))
        .expect("image format is valid");
    let error = validate_image_flash_ranges(&crossing, &regions)
        .expect_err("segment crosses the Flash boundary");
    assert_eq!(error.code, ErrorCode::FlashRangeInvalid);

    validate_flash_range(&regions, 0x1000_0000, 0x1_0000).expect("whole data Flash range");
    for (address, length) in [(0x0, 0), (0x7_ffff, 2), (u64::MAX, 2)] {
        assert_eq!(
            validate_flash_range(&regions, address, length)
                .expect_err("invalid range")
                .code,
            ErrorCode::FlashRangeInvalid
        );
    }
}

#[test]
fn t_p2_prg_reports_only_first_mismatch_region_and_total_count() {
    let mut mismatches = VerifyMismatchAccumulator::new();
    mismatches
        .compare_segment(0x1000, &[0, 1, 2, 3, 4, 5, 6, 7], &[0, 9, 8, 3, 4, 0, 6, 1])
        .expect("equal lengths");
    let error = mismatches
        .finish()
        .expect("three mismatch regions")
        .into_error();
    assert_eq!(error.code, ErrorCode::VerifyFailed);
    let details = error.details.expect("compact mismatch details");
    assert_eq!(details.len(), 3);
    assert_eq!(details["first_address"], "0x1001");
    assert_eq!(details["first_length"], 2);
    assert_eq!(details["total_regions"], 3);
}

#[test]
fn t_p2_prg_wire_contract_has_explicit_after_and_no_authorization_token() {
    let flash = ProgramRequest::Flash {
        image: PathBuf::from("firmware.bin"),
        base_address: Some(0),
        verify: true,
        after: ProgramAfter::ResetRun,
    };
    let value = serde_json::to_value(&flash).expect("serialize flash request");
    assert_eq!(value["after"], "reset_run");
    assert!(value.get("authorization").is_none());
    assert!(value.get("confirmation_token").is_none());

    let range = FlashRange::new(0x1000_0000, 0x1000).expect("valid erase range");
    let erase = ProgramRequest::Erase {
        range: Some(range),
        after: ProgramAfter::None,
    };
    assert!(erase.modifies_flash());
    assert_eq!(
        SessionCommand::Flash.execution_kind(),
        ExecutionKind::SideEffect
    );
    assert_eq!(
        SessionCommand::Erase.execution_kind(),
        ExecutionKind::SideEffect
    );
    assert_eq!(
        SessionCommand::Verify.execution_kind(),
        ExecutionKind::ReadOnly
    );

    let unknown = serde_json::from_value::<ProgramRequest>(serde_json::json!({
        "action": "erase",
        "after": "none",
        "authorization": "bypass"
    }))
    .expect_err("unknown authorization field is rejected");
    assert!(unknown.to_string().contains("unknown field"));
}
