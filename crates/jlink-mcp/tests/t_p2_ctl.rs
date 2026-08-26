//! MCP boundary portion of primary test T-P2-CTL.

use std::io::Cursor;

use jlink_domain::{ErrorCode, JlinkError};
use jlink_mcp::mcp::{ToolCall, ToolDispatcher, serve, tool_catalog};
use serde_json::{Value, json};

#[test]
fn t_p2_ctl_schema_closes_register_and_control_actions() {
    let catalog = tool_catalog();
    let inspect = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_inspect")
        .expect("inspect tool");
    let write = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_write")
        .expect("write tool");
    let control = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_control")
        .expect("control tool");

    assert!(jsonschema::is_valid(
        &inspect["inputSchema"],
        &json!({"action": "register", "name": "PC"})
    ));
    assert!(!jsonschema::is_valid(
        &inspect["inputSchema"],
        &json!({"action": "register", "name": "PC", "index": 15})
    ));
    assert!(jsonschema::is_valid(
        &write["inputSchema"],
        &json!({"action": "register", "name": "R0", "value": "0xFFFFFFFF"})
    ));
    assert!(!jsonschema::is_valid(
        &write["inputSchema"],
        &json!({
            "action": "register",
            "name": "R0",
            "value": "0x00000001",
            "authorization": "bypass"
        })
    ));
    assert!(jsonschema::is_valid(
        &control["inputSchema"],
        &json!({"action": "reset", "after": "halt"})
    ));
    assert!(!jsonschema::is_valid(
        &control["inputSchema"],
        &json!({"action": "reset"})
    ));
    assert!(!jsonschema::is_valid(
        &control["inputSchema"],
        &json!({"action": "step", "after": "halt"})
    ));
}

struct ControlDispatcher;

impl ToolDispatcher for ControlDispatcher {
    fn call(&mut self, name: &str, arguments: &Value) -> ToolCall {
        match (name, arguments["action"].as_str()) {
            ("jlink_inspect", Some("register")) if arguments["name"] == "PC" => {
                ToolCall::success(json!({"value": "0x08001234"}))
            }
            ("jlink_inspect", Some("register")) => ToolCall::Error(JlinkError::new(
                ErrorCode::RegisterNotFound,
                "目标不支持规范核心寄存器名称",
                false,
            )),
            ("jlink_write", Some("register")) => ToolCall::Error(JlinkError::new(
                ErrorCode::ValueInvalid,
                "核心寄存器 IPSR 是只读视图",
                false,
            )),
            ("jlink_control", Some("step")) => ToolCall::Error(JlinkError::new(
                ErrorCode::InvalidStateTransition,
                "step 要求目标已经 halted",
                true,
            )),
            ("jlink_control", Some("halt")) => ToolCall::Error(JlinkError::new(
                ErrorCode::OperationConflict,
                "活动 HSS 期间不能执行目标运行控制",
                true,
            )),
            ("jlink_control", Some("resume" | "reset")) => ToolCall::success(json!({})),
            _ => ToolCall::Unavailable("unexpected test route".to_owned()),
        }
    }

    fn read_resource(&mut self, uri: &str) -> ToolCall {
        ToolCall::Unavailable(format!("resource unavailable: {uri}"))
    }
}

#[test]
fn t_p2_ctl_returns_minimal_success_and_stable_public_errors() {
    let calls = [
        ("jlink_inspect", json!({"action": "register", "name": "PC"})),
        ("jlink_inspect", json!({"action": "register", "name": "pc"})),
        (
            "jlink_write",
            json!({"action": "register", "name": "IPSR", "value": "0x1"}),
        ),
        ("jlink_control", json!({"action": "step"})),
        ("jlink_control", json!({"action": "halt"})),
        ("jlink_control", json!({"action": "resume"})),
    ];
    let input = calls
        .iter()
        .enumerate()
        .map(|(index, (name, arguments))| {
            json!({
                "jsonrpc": "2.0",
                "id": index + 1,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut output = Vec::new();
    serve(Cursor::new(input), &mut output, &mut ControlDispatcher).expect("stdio server");
    let responses = String::from_utf8(output)
        .expect("UTF-8 output")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSON response"))
        .collect::<Vec<_>>();

    assert_eq!(
        responses[0]["result"]["structuredContent"],
        json!({"value": "0x08001234"})
    );
    for (index, code) in [
        (1, "REGISTER_NOT_FOUND"),
        (2, "VALUE_INVALID"),
        (3, "TARGET_STATE_INVALID"),
        (4, "OPERATION_CONFLICT"),
    ] {
        assert_eq!(
            responses[index]["result"]["structuredContent"]["error"]["code"],
            code
        );
    }
    assert_eq!(responses[5]["result"]["structuredContent"], json!({}));
}
