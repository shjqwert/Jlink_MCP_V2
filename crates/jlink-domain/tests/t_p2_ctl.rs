//! Primary test T-P2-CTL for pure DBG-004 and DBG-005 rules.

use jlink_domain::{
    ControlAfter, ControlRequest, CoreRegister, DebugRequest, ErrorCode, ExecutionKind,
    SessionCommand,
};

#[test]
fn t_p2_ctl_accepts_only_exact_canonical_register_names() {
    assert_eq!(
        CoreRegister::from_canonical("PC").expect("canonical PC"),
        CoreRegister::Pc
    );
    assert_eq!(CoreRegister::Pc.jlink_name(), "R15 (PC)");
    assert_eq!(CoreRegister::Sp.jlink_name(), "R13 (SP)");
    for invalid in ["pc", "R15", "R13", " PC", "PC ", "unknown"] {
        let error = CoreRegister::from_canonical(invalid).expect_err("name is not canonical");
        assert_eq!(error.code, ErrorCode::RegisterNotFound);
        assert_eq!(
            error.details.expect("requested name")["requested_name"],
            invalid
        );
    }
}

#[test]
fn t_p2_ctl_rejects_read_only_registers_before_execution() {
    for register in [CoreRegister::Xpsr, CoreRegister::Epsr, CoreRegister::Ipsr] {
        let request = DebugRequest::WriteRegister { register, value: 1 };
        let error = request
            .validate()
            .expect_err("read-only register is rejected");
        assert_eq!(error.code, ErrorCode::ValueInvalid);
        let details = error.details.expect("canonical read-only details");
        assert_eq!(details["register"], register.canonical_name());
        assert_eq!(details["writable"], false);
    }
    DebugRequest::WriteRegister {
        register: CoreRegister::R0,
        value: u32::MAX,
    }
    .validate()
    .expect("writable 32-bit register value");
}

#[test]
fn t_p2_ctl_wire_contract_keeps_reset_state_and_execution_classification() {
    let reset = ControlRequest::Reset {
        after: ControlAfter::Halt,
    };
    assert_eq!(
        serde_json::to_value(reset).expect("serialize control"),
        serde_json::json!({"action": "reset", "after": "halt"})
    );
    assert_eq!(
        SessionCommand::ReadRegister.execution_kind(),
        ExecutionKind::ReadOnly
    );
    for command in [SessionCommand::WriteRegister, SessionCommand::Control] {
        assert_eq!(command.execution_kind(), ExecutionKind::SideEffect);
    }
}
