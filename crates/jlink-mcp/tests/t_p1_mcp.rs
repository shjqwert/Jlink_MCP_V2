//! Primary contract test T-P1-MCP for the six-tool stdio boundary.

use std::{io::Cursor, path::PathBuf};

use jlink_domain::{ErrorCode, JlinkError};
use jlink_mcp::{
    config::ConfigPaths,
    mcp::{RAW_CAPTURE_MIME, ToolCall, ToolDispatcher, serve, tool_catalog},
    runtime::Runtime,
};
use serde_json::{Value, json};

const EXPECTED_TOOLS: [&str; 6] = [
    "jlink_target",
    "jlink_program",
    "jlink_inspect",
    "jlink_write",
    "jlink_control",
    "jlink_hss",
];

#[derive(Default)]
struct ContractFixture {
    calls: usize,
}

impl ToolDispatcher for ContractFixture {
    fn call(&mut self, name: &str, arguments: &Value) -> ToolCall {
        self.calls += 1;
        match (name, arguments.get("action").and_then(Value::as_str)) {
            ("jlink_target", Some("status")) => ToolCall::success(json!({
                "connection": "connected",
                "state": "running"
            })),
            ("jlink_inspect", Some("variable")) if arguments["path"] == "unavailable.value" => {
                ToolCall::Error(JlinkError::new(
                    ErrorCode::WorkerUnavailable,
                    "Worker 端点不可用",
                    true,
                ))
            }
            ("jlink_inspect", Some("variable")) => ToolCall::success(json!({ "value": 3 })),
            ("jlink_inspect", Some("memory")) => ToolCall::success(json!({ "data": "78563412" })),
            ("jlink_inspect", Some("register")) if arguments["name"] == "PC" => {
                ToolCall::success(json!({ "value": "0x08001234" }))
            }
            ("jlink_inspect", Some("register")) => {
                ToolCall::success(json!({ "symbols": ["wrong.result"] }))
            }
            ("jlink_inspect", Some("symbols")) => {
                ToolCall::success(json!({ "symbols": ["motor", "motor.speed"] }))
            }
            ("jlink_program", Some("verify")) => ToolCall::Error(
                JlinkError::new(ErrorCode::VerifyFailed, "target differs", false)
                    .with_detail("first_address", json!("0x1000"))
                    .with_detail("first_length", json!(4))
                    .with_detail("total_regions", json!(2)),
            ),
            ("jlink_program", Some("flash")) => ToolCall::Error(JlinkError::new(
                ErrorCode::FlashRangeInvalid,
                "outside device Flash",
                false,
            )),
            ("jlink_hss", Some("query")) => ToolCall::with_raw_capture(
                json!({
                    "capture_id": "cap_t_p1_mcp",
                    "from_us": 0,
                    "to_us": 1_000,
                    "dictionary": { "s0": "motor.state" },
                    "variables": [{ "series": "s0", "samples": 1, "changes": 0 }],
                    "events": 0
                }),
                "cap_t_p1_mcp",
            ),
            _ => ToolCall::success(json!({})),
        }
    }

    fn read_resource(&mut self, uri: &str) -> ToolCall {
        ToolCall::Success {
            structured_content: json!({}),
            content: vec![json!({
                "uri": uri,
                "mimeType": RAW_CAPTURE_MIME,
                "blob": "VC1QMS1NQ1A="
            })],
        }
    }
}

#[test]
fn t_p1_mcp_catalog_is_closed_and_action_strict() {
    let catalog = tool_catalog();
    assert_eq!(catalog.len(), EXPECTED_TOOLS.len());
    assert_eq!(
        catalog
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>(),
        EXPECTED_TOOLS
    );
    for tool in &catalog {
        let input = &tool["inputSchema"];
        let output = &tool["outputSchema"];
        assert_eq!(input["type"], "object");
        assert_eq!(input["additionalProperties"], false);
        assert_eq!(output["type"], "object");
        assert_eq!(output["additionalProperties"], false);
        jsonschema::meta::validate(input).expect("valid input Schema");
        jsonschema::meta::validate(output).expect("valid output Schema");
    }

    let target = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_target")
        .expect("target tool");
    assert!(jsonschema::is_valid(
        &target["inputSchema"],
        &json!({ "action": "status" })
    ));
    assert!(!jsonschema::is_valid(
        &target["inputSchema"],
        &json!({ "action": "status", "after": "run" })
    ));
    assert!(!jsonschema::is_valid(
        &target["inputSchema"],
        &json!({ "action": "status", "undeclared": true })
    ));
    assert!(jsonschema::is_valid(
        &target["inputSchema"],
        &json!({ "action": "validate", "after": "halt" })
    ));
    assert!(jsonschema::is_valid(
        &target["inputSchema"],
        &json!({
            "action": "config_set",
            "scope": "project",
            "values": { "target.device": "S32K144" }
        })
    ));
    assert!(!jsonschema::is_valid(
        &target["inputSchema"],
        &json!({
            "action": "config_set",
            "scope": "user",
            "values": { "target.device": "S32K144" }
        })
    ));
    let hss = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_hss")
        .expect("HSS tool");
    let hss_description = hss["description"].as_str().expect("HSS description");
    for required in [
        "fixed-duration",
        "status",
        "overview",
        "changes",
        "window",
        "around_event",
        "capture_id",
        "capture_key",
        "cursor",
    ] {
        assert!(
            hss_description.contains(required),
            "HSS description is missing {required}"
        );
    }
    assert!(hss_description.len() <= 240);
    assert!(!hss_description.contains('{'));
    let window = json!({
        "action": "query",
        "capture_id": "cap_contract",
        "view": "window",
        "series": ["motor.speed"],
        "from_us": 0,
        "to_us": 1000,
        "mode": "min_max"
    });
    assert!(!jsonschema::is_valid(&hss["inputSchema"], &window));
    let mut window_with_points = window;
    window_with_points
        .as_object_mut()
        .expect("window request")
        .insert("points".to_owned(), json!(100));
    assert!(jsonschema::is_valid(
        &hss["inputSchema"],
        &window_with_points
    ));
}

#[test]
fn t_p1_mcp_validation_checks_require_evidence_provenance() {
    let catalog = tool_catalog();
    let target = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_target")
        .expect("target tool");
    let validation_check = &target["outputSchema"]["properties"]["checks"]["items"];
    assert!(
        validation_check["required"]
            .as_array()
            .expect("validation check required fields")
            .contains(&json!("evidence"))
    );
}

#[test]
fn t_p2_prg_schema_requires_after_and_paired_erase_range() {
    let catalog = tool_catalog();
    let program = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_program")
        .expect("program tool");
    for request in [
        json!({ "action": "flash", "image": "firmware.bin", "base_address": "0x0", "after": "reset_run" }),
        json!({ "action": "verify", "image": "firmware.bin", "base_address": "0x10000" }),
        json!({ "action": "erase", "after": "none" }),
        json!({ "action": "erase", "address": "0x10000000", "length": 4096, "after": "reset_halt" }),
    ] {
        assert!(jsonschema::is_valid(&program["inputSchema"], &request));
    }
    for request in [
        json!({ "action": "erase", "base_address": "0x0", "after": "none" }),
        json!({ "action": "erase", "address": "0x10000000", "after": "none" }),
        json!({ "action": "erase", "length": 4096, "after": "none" }),
        json!({ "action": "flash", "image": "firmware.elf" }),
        json!({ "action": "erase" }),
        json!({ "action": "flash", "base_address": "0", "after": "none" }),
        json!({ "action": "erase", "after": "none", "authorization": "bypass" }),
    ] {
        assert!(!jsonschema::is_valid(&program["inputSchema"], &request));
    }
}

#[test]
fn t_p1_mcp_inspect_output_schema_is_strict() {
    let catalog = tool_catalog();
    let inspect = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_inspect")
        .expect("inspect tool");
    let output = &inspect["outputSchema"];
    for valid in [
        json!({ "value": 3 }),
        json!({ "value": { "state": 3 } }),
        json!({ "data": "78563412" }),
        json!({ "value": "0x08001234" }),
        json!({ "symbols": ["motor", "motor.speed"] }),
        json!({ "error": { "code": "TARGET_CONNECT_FAILED", "message": "offline", "retryable": true } }),
    ] {
        assert!(jsonschema::is_valid(output, &valid), "invalid {valid}");
    }
    for invalid in [
        json!({}),
        json!({ "value": 3, "symbols": ["motor"] }),
        json!({ "value": "08001234" }),
        json!({ "data": "xyz" }),
    ] {
        assert!(!jsonschema::is_valid(output, &invalid), "valid {invalid}");
    }
}

#[test]
fn t_p4_hss_catalog_is_bounded_and_around_event_accepts_series() {
    let catalog = tool_catalog();
    let catalog_bytes = serde_json::to_vec(&catalog).expect("serialize tool catalog");
    assert!(
        catalog_bytes.len() <= 32 * 1024,
        "tools/list catalog is {} bytes",
        catalog_bytes.len()
    );
    let hss = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_hss")
        .expect("HSS tool");
    let hss_bytes = serde_json::to_vec(hss).expect("serialize HSS tool");
    assert!(
        hss_bytes.len() <= 22 * 1024,
        "jlink_hss is {} bytes",
        hss_bytes.len()
    );
    assert!(jsonschema::is_valid(
        &hss["inputSchema"],
        &json!({
            "action": "query",
            "capture_id": "cap-1",
            "view": "around_event",
            "event_id": "e0",
            "before_us": 1_000,
            "after_us": 1_000,
            "series": ["s0"],
            "limit": 100
        })
    ));
    assert!(jsonschema::is_valid(
        &hss["outputSchema"],
        &json!({
            "capture_id": "cap-compact",
            "state": "completed",
            "elapsed_us": 60_000_000,
            "complete_records": 59_993
        })
    ));
    assert!(!jsonschema::is_valid(
        &hss["outputSchema"],
        &json!({
            "capture_id": "cap-duplicated",
            "state": "completed",
            "elapsed_us": 60_000_000,
            "complete_records": 59_993,
            "from_us": 0
        })
    ));
}

#[test]
fn t_p1_mcp_stdio_returns_minimal_results_and_public_errors() {
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-11-25" }
        }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "jlink_target", "arguments": { "action": "status" } }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "jlink_target",
                "arguments": { "action": "status", "undeclared": true }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "jlink_inspect",
                "arguments": { "action": "variable", "path": "unavailable.value" }
            }
        }),
    ];
    let mut fixture = ContractFixture::default();
    let responses = exchange(&requests, &mut fixture);

    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-11-25");
    let instructions = responses[0]["result"]["instructions"]
        .as_str()
        .expect("server instructions");
    for required in [
        "exactly six tools",
        "tools/list",
        "structuredContent",
        "EXECUTION_UNCERTAIN",
        "do not repeat the side effect",
    ] {
        assert!(
            instructions.contains(required),
            "server instructions are missing {required}"
        );
    }
    assert!(
        instructions.len() <= 512,
        "server instructions must remain self-contained and short"
    );
    assert_eq!(
        responses[1]["result"]["tools"].as_array().map(Vec::len),
        Some(6)
    );
    assert_eq!(responses[2]["result"]["content"], json!([]));
    assert_eq!(
        responses[2]["result"]["structuredContent"],
        json!({ "connection": "connected", "state": "running" })
    );
    assert_eq!(responses[3]["error"]["code"], -32_602);
    assert_eq!(responses[4]["result"]["isError"], true);
    assert_eq!(
        responses[4]["result"]["structuredContent"]["error"]["code"],
        "TARGET_CONNECT_FAILED"
    );
    assert_eq!(
        fixture.calls, 2,
        "invalid arguments must not reach dispatch"
    );
}

#[test]
fn t_p2_prg_returns_stable_boundary_and_compact_verify_errors() {
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "jlink_program",
                "arguments": { "action": "flash", "image": "outside.elf", "after": "none" }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "jlink_program",
                "arguments": { "action": "verify", "image": "mismatch.elf" }
            }
        }),
    ];
    let mut fixture = ContractFixture::default();
    let responses = exchange(&requests, &mut fixture);
    assert_eq!(
        responses[0]["result"]["structuredContent"]["error"]["code"],
        "FLASH_RANGE_INVALID"
    );
    let verify = &responses[1]["result"]["structuredContent"]["error"];
    assert_eq!(verify["code"], "VERIFY_FAILED");
    assert_eq!(verify["details"]["first_address"], "0x1000");
    assert_eq!(verify["details"]["first_length"], 4);
    assert_eq!(verify["details"]["total_regions"], 2);
    assert_eq!(
        verify["details"].as_object().map(serde_json::Map::len),
        Some(3)
    );
}

#[test]
fn t_p1_mcp_stdio_enforces_inspect_action_results() {
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "jlink_inspect",
                "arguments": { "action": "variable", "path": "motor.state" }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "jlink_inspect",
                "arguments": { "action": "memory", "address": "0x20001000", "length": 4 }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "jlink_inspect",
                "arguments": { "action": "register", "name": "PC" }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "jlink_inspect",
                "arguments": { "action": "symbols", "query": "motor" }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "jlink_inspect",
                "arguments": { "action": "register", "name": "BROKEN" }
            }
        }),
    ];
    let mut fixture = ContractFixture::default();
    let responses = exchange(&requests, &mut fixture);

    assert_eq!(
        responses[0]["result"]["structuredContent"],
        json!({ "value": 3 })
    );
    assert_eq!(
        responses[1]["result"]["structuredContent"],
        json!({ "data": "78563412" })
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"],
        json!({ "value": "0x08001234" })
    );
    assert_eq!(
        responses[3]["result"]["structuredContent"],
        json!({ "symbols": ["motor", "motor.speed"] })
    );
    assert_eq!(responses[4]["error"]["code"], -32_603);
    assert_eq!(fixture.calls, 5);
}

#[test]
fn t_p1_mcp_resource_link_template_and_read_share_one_contract() {
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "jlink_hss",
                "arguments": {
                    "action": "query",
                    "capture_id": "cap_t_p1_mcp",
                    "view": "overview"
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/templates/list"
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": { "uri": "jlink-mcp://capture/cap_t_p1_mcp/raw" }
        }),
    ];
    let mut fixture = ContractFixture::default();
    let responses = exchange(&requests, &mut fixture);

    assert_eq!(
        responses[0]["result"]["content"][0]["type"],
        "resource_link"
    );
    assert_eq!(
        responses[0]["result"]["content"][0]["mimeType"],
        RAW_CAPTURE_MIME
    );
    assert_eq!(
        responses[1]["result"]["resourceTemplates"][0]["uriTemplate"],
        "jlink-mcp://capture/{capture_id}/raw"
    );
    assert_eq!(
        responses[2]["result"]["contents"][0]["blob"],
        "VC1QMS1NQ1A="
    );
    assert_eq!(fixture.calls, 1);
}

#[test]
fn t_p1_mcp_runtime_reports_an_unavailable_unknown_raw_resource() {
    let paths = ConfigPaths::new(
        PathBuf::from("unused-project.toml"),
        PathBuf::from("unused-user.toml"),
    );
    let mut runtime = Runtime::new(
        paths,
        PathBuf::from("unused-worker.exe"),
        PathBuf::from("unused-leases"),
    );
    let responses = exchange(
        &[json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "resources/read",
            "params": {
                "uri": "jlink-mcp://capture/future_capture/raw"
            }
        })],
        &mut runtime,
    );

    assert_eq!(responses[0]["error"]["code"], -32_002);
    assert!(responses[0].get("result").is_none());
}

fn exchange<D: ToolDispatcher>(requests: &[Value], dispatcher: &mut D) -> Vec<Value> {
    let input = requests
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut output = Vec::new();
    serve(Cursor::new(input), &mut output, dispatcher).expect("stdio server");
    let responses: Vec<Value> = String::from_utf8(output)
        .expect("UTF-8 response")
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSON-RPC response"))
        .collect();
    assert_eq!(responses.len(), requests.len());
    responses
}
