//! Pure session-contract cases owned by primary test T-P1-SES.

use jlink_domain::{
    ErrorCode, ExecutionKind, IpcRequest, ProtocolVersion, RequestId, SessionCommand,
    TargetConnectionSpec, TargetInterface, ValidationAfter, ensure_disconnect_allowed,
};

#[test]
fn t_p1_ses_requires_one_explicit_probe_and_concrete_target() {
    let missing_probe =
        TargetConnectionSpec::new("S32K144", TargetInterface::Swd, 4_000, None, None)
            .expect_err("probe selection must be explicit");
    assert_eq!(missing_probe.code, ErrorCode::ConfigInvalid);

    let generic = TargetConnectionSpec::new(
        "Cortex-M4",
        TargetInterface::Swd,
        4_000,
        Some(260_106_173),
        None,
    )
    .expect_err("generic device must be rejected");
    assert_eq!(generic.code, ErrorCode::ConfigInvalid);
}

#[test]
fn t_p1_ses_connect_payload_is_explicit_and_strict() {
    let target = TargetConnectionSpec::new(
        "S32K144",
        TargetInterface::Swd,
        4_000,
        Some(260_106_173),
        Some("3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF".into()),
    )
    .expect("target spec");
    let request = IpcRequest::new(
        ProtocolVersion::V1,
        RequestId::new("t-p1-ses").expect("request ID"),
        SessionCommand::Connect,
    )
    .with_target(target);
    let value = serde_json::to_value(&request).expect("serialize request");
    assert_eq!(value["target"]["interface"], "swd");
    assert_eq!(value["target"]["probe_serial"], 260_106_173_u32);

    let mut invalid = value;
    invalid["target"]["fallback_interface"] = serde_json::json!("jtag");
    assert!(serde_json::from_value::<IpcRequest>(invalid).is_err());
}

#[test]
fn t_p1_ses_disconnect_is_rejected_during_hss() {
    assert_eq!(
        ensure_disconnect_allowed(true)
            .expect_err("active capture owns cleanup")
            .code,
        ErrorCode::OperationConflict
    );
    ensure_disconnect_allowed(false).expect("idle session may disconnect");
}

#[test]
fn t_p1_ses_disconnected_validation_after_is_explicit_and_strict() {
    let target = TargetConnectionSpec::new(
        "S32K144",
        TargetInterface::Swd,
        4_000,
        Some(260_106_173),
        None,
    )
    .expect("target spec");
    let request = IpcRequest::new(
        ProtocolVersion::V1,
        RequestId::new("t-p1-ses-validate").expect("request ID"),
        SessionCommand::Validate,
    )
    .with_target(target)
    .with_validation_after(ValidationAfter::Run);
    let value = serde_json::to_value(&request).expect("serialize request");
    assert_eq!(value["after"], "run");
    assert_eq!(
        SessionCommand::Validate.execution_kind(),
        ExecutionKind::SideEffect
    );

    let mut invalid = value;
    invalid["after"] = serde_json::json!("reset");
    assert!(serde_json::from_value::<IpcRequest>(invalid).is_err());
}
