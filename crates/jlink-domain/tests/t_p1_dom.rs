//! Primary P1 domain contract and execution-boundary integration tests.

use jlink_domain::{
    ConnectionState, DispatchState, ErrorCode, ExecutionKind, IpcRequest, IpcResponse, JlinkError,
    ProtocolVersion, RequestId, SessionCommand, SessionEvent, TargetInterface,
    classify_worker_loss, transition_session,
};
use serde_json::json;

#[test]
fn t_p1_dom_versioned_ipc_roundtrip_is_explicit() {
    let request_id = RequestId::new("dom-roundtrip-1").expect("request ID should be valid");
    let request = IpcRequest::new(ProtocolVersion::V1, request_id, SessionCommand::Status);
    let encoded = serde_json::to_string(&request).expect("request should serialize");
    assert_eq!(
        encoded,
        r#"{"protocol_version":1,"request_id":"dom-roundtrip-1","command":"status"}"#
    );
    let decoded: IpcRequest = serde_json::from_str(&encoded).expect("request should roundtrip");
    assert_eq!(decoded, request);

    let response = IpcResponse::success(
        ProtocolVersion::V1,
        RequestId::new("dom-roundtrip-1").expect("request ID should be valid"),
        json!({"connection":"connected"}),
    );
    let response_json = serde_json::to_value(&response).expect("response should serialize");
    assert_eq!(response_json["protocol_version"], json!(1));
    assert_eq!(response_json["request_id"], json!("dom-roundtrip-1"));
    assert!(response.validate().is_ok());
    let decoded: IpcResponse = serde_json::from_value(response_json).expect("response roundtrip");
    assert_eq!(decoded, response);

    let mut neither = response.clone();
    neither.result = None;
    assert_eq!(
        neither
            .validate()
            .expect_err("empty response must be rejected")
            .code,
        ErrorCode::InvalidResponse
    );
    let mut both = response;
    both.error = Some(JlinkError::new(
        ErrorCode::InvalidResponse,
        "invalid response",
        false,
    ));
    assert_eq!(
        both.validate()
            .expect_err("ambiguous response must be rejected")
            .code,
        ErrorCode::InvalidResponse
    );
}

#[test]
fn t_p1_dom_wire_objects_reject_unknown_fields_and_versions() {
    let unknown_field =
        r#"{"protocol_version":1,"request_id":"r1","command":"status","extra":true}"#;
    assert!(serde_json::from_str::<IpcRequest>(unknown_field).is_err());
    assert!(
        serde_json::from_str::<IpcRequest>(
            r#"{"protocol_version":2,"request_id":"r1","command":"status"}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<IpcRequest>(
            r#"{"protocol_version":1,"request_id":" ","command":"status"}"#
        )
        .is_err()
    );
}

#[test]
fn t_p1_dom_stable_error_shape_uses_screaming_snake_case_code() {
    let empty = JlinkError::new(ErrorCode::WorkerUnavailable, "worker is unavailable", true);
    assert_eq!(
        serde_json::to_value(&empty).expect("error should serialize"),
        json!({
            "code":"WORKER_UNAVAILABLE",
            "message":"worker is unavailable",
            "retryable":true
        })
    );
    assert_eq!(
        empty.to_string(),
        "WORKER_UNAVAILABLE: worker is unavailable"
    );
    assert_eq!(ErrorCode::WorkerUnavailable.as_str(), "WORKER_UNAVAILABLE");
    assert_eq!(
        ErrorCode::TargetConnectFailed.as_str(),
        "TARGET_CONNECT_FAILED"
    );

    let error = JlinkError::new(ErrorCode::WorkerUnavailable, "worker is unavailable", true)
        .with_detail("attempt", json!(1));
    let value = serde_json::to_value(&error).expect("error should serialize");
    assert_eq!(
        value,
        json!({
            "code":"WORKER_UNAVAILABLE",
            "message":"worker is unavailable",
            "retryable":true,
            "details":{"attempt":1}
        })
    );
    assert!(
        serde_json::from_value::<JlinkError>(json!({
            "code":"WORKER_UNAVAILABLE",
            "message":"worker is unavailable",
            "retryable":true,
            "details":{},
            "unknown":true
        }))
        .is_err()
    );
}

#[test]
fn t_p1_dom_jtag_roundtrip_does_not_fallback_to_swd() {
    let encoded = serde_json::to_string(&TargetInterface::Jtag).expect("JTAG should serialize");
    assert_eq!(encoded, r#""jtag""#);
    let decoded: TargetInterface = serde_json::from_str(&encoded).expect("JTAG should parse");
    assert_eq!(decoded, TargetInterface::Jtag);
    assert!(serde_json::from_str::<TargetInterface>(r#""swd""#).is_ok());
}

#[test]
fn t_p1_dom_session_transitions_accept_only_legal_edges() {
    assert_eq!(
        transition_session(
            ConnectionState::Disconnected,
            SessionEvent::ConnectRequested
        ),
        Ok(ConnectionState::Connecting)
    );
    assert_eq!(
        transition_session(ConnectionState::Connecting, SessionEvent::Connected),
        Ok(ConnectionState::Connected)
    );
    assert_eq!(
        transition_session(
            ConnectionState::Connected,
            SessionEvent::DisconnectRequested
        ),
        Ok(ConnectionState::Disconnected)
    );
    assert_eq!(
        transition_session(ConnectionState::Connected, SessionEvent::WorkerLost),
        Ok(ConnectionState::Faulted)
    );

    let error = transition_session(ConnectionState::Disconnected, SessionEvent::Connected)
        .expect_err("disconnected session cannot become connected directly");
    assert_eq!(error.code, ErrorCode::InvalidStateTransition);
}

#[test]
fn t_p1_dom_worker_loss_preserves_execution_boundary() {
    assert_eq!(
        SessionCommand::Validate.execution_kind(),
        ExecutionKind::SideEffect
    );
    let uncertain = classify_worker_loss(ExecutionKind::SideEffect, DispatchState::Dispatched)
        .expect("dispatched side effects must be classified");
    assert_eq!(uncertain.code, ErrorCode::ExecutionUncertain);
    assert!(!uncertain.retryable);

    let retryable = classify_worker_loss(ExecutionKind::SideEffect, DispatchState::NotDispatched)
        .expect("undispatched side effects are retryable");
    assert_eq!(retryable.code, ErrorCode::WorkerUnavailable);
    assert!(retryable.retryable);

    let readonly = classify_worker_loss(ExecutionKind::ReadOnly, DispatchState::Dispatched)
        .expect("dispatched read-only calls are retryable");
    assert_eq!(readonly.code, ErrorCode::WorkerUnavailable);
    assert!(readonly.retryable);

    let completed = classify_worker_loss(ExecutionKind::SideEffect, DispatchState::Completed)
        .expect_err("completed operations must not be reclassified");
    assert_eq!(completed.code, ErrorCode::InvalidStateTransition);
}
