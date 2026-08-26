//! Framing and identity rules owned by primary test T-P1-IPC.

use std::io::{self, Cursor, Read};

use jlink_domain::{
    ErrorCode, IpcRequest, MAX_IPC_FRAME_BYTES, ProtocolVersion, RequestId, SessionCommand,
    probe_identity_hash, read_ipc_frame, worker_endpoint_name, write_ipc_frame,
};

struct OneByteReader<R>(R);

impl<R: Read> Read for OneByteReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = buffer.len().min(1);
        self.0.read(&mut buffer[..length])
    }
}

fn status_request() -> IpcRequest {
    IpcRequest::new(
        ProtocolVersion::V1,
        RequestId::new("t-p1-ipc").expect("request ID"),
        SessionCommand::Status,
    )
}

#[test]
fn t_p1_ipc_frame_survives_partial_byte_reads() {
    let mut bytes = Vec::new();
    write_ipc_frame(&mut bytes, &status_request()).expect("encode frame");
    let decoded: IpcRequest =
        read_ipc_frame(&mut OneByteReader(Cursor::new(bytes))).expect("partial frame");
    assert_eq!(decoded, status_request());
}

#[test]
fn t_p1_ipc_frame_rejects_truncation_unknown_fields_versions_and_oversize() {
    let mut truncated = Cursor::new([8_u8, 0, 0, 0, b'{']);
    assert_eq!(
        read_ipc_frame::<_, IpcRequest>(&mut truncated)
            .expect_err("truncated payload")
            .code,
        ErrorCode::IpcProtocolError
    );

    for invalid in [
        br#"{"protocol_version":1,"request_id":"x","command":"status","extra":true}"#.as_slice(),
        br#"{"protocol_version":2,"request_id":"x","command":"status"}"#.as_slice(),
        b"not-json".as_slice(),
    ] {
        let mut bytes = Vec::from(u32::try_from(invalid.len()).expect("length").to_le_bytes());
        bytes.extend_from_slice(invalid);
        assert_eq!(
            read_ipc_frame::<_, IpcRequest>(&mut Cursor::new(bytes))
                .expect_err("invalid request")
                .code,
            ErrorCode::IpcProtocolError
        );
    }

    let oversized = u32::try_from(MAX_IPC_FRAME_BYTES + 1)
        .expect("limit")
        .to_le_bytes();
    assert_eq!(
        read_ipc_frame::<_, IpcRequest>(&mut Cursor::new(oversized))
            .expect_err("oversized frame")
            .code,
        ErrorCode::IpcProtocolError
    );
}

#[test]
fn t_p1_ipc_probe_identity_is_stable_and_not_embedded() {
    let first = probe_identity_hash("260106173").expect("probe identity");
    let second = probe_identity_hash("260106173").expect("probe identity");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    assert!(!first.contains("260106173"));
    let endpoint = worker_endpoint_name("260106173").expect("endpoint");
    assert!(endpoint.starts_with(r"\\.\pipe\jlink-mcp-v1-"));
    assert!(!endpoint.contains("260106173"));
    assert_eq!(
        probe_identity_hash(" ").expect_err("blank identity").code,
        ErrorCode::ConfigInvalid
    );
}
