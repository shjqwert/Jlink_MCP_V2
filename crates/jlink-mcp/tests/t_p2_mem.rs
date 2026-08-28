//! MCP boundary portion of primary test T-P2-MEM.

use std::{env, fs, io::Cursor, path::PathBuf};

use jlink_domain::{
    DebugRequest, ErrorCode, FirmwareImage, JlinkError, VariableSelector, WriteVerify,
};
use jlink_mcp::mcp::{ToolCall, ToolDispatcher, serve, tool_catalog};
use jlink_mcp::symbols::SymbolIndex;
use serde_json::{Value, json};

#[test]
fn t_p2_mem_schema_keeps_exact_limits_and_has_no_extra_write_authorization() {
    let catalog = tool_catalog();
    let inspect = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_inspect")
        .expect("inspect tool");
    let write = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_write")
        .expect("write tool");

    assert!(jsonschema::is_valid(
        &inspect["inputSchema"],
        &json!({
            "action": "memory",
            "address": "0x20000000",
            "length": 4096
        })
    ));
    assert!(!jsonschema::is_valid(
        &inspect["inputSchema"],
        &json!({
            "action": "memory",
            "address": "0x20000000",
            "length": 4097
        })
    ));
    assert!(jsonschema::is_valid(
        &write["inputSchema"],
        &json!({
            "action": "memory",
            "address": "0x40001000",
            "data": "01000000"
        })
    ));
    assert!(!jsonschema::is_valid(
        &write["inputSchema"],
        &json!({
            "action": "memory",
            "address": "0x40001000",
            "data": "01000000",
            "authorization": "bypass"
        })
    ));
}

#[test]
fn t_p2_mem_variable_execution_payload_binds_plan_to_same_elf_identity() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../validation/evidence/f0-c/F0cDwarfFixture.out");
    let data = fs::read(&path).expect("frozen IAR F0-C fixture");
    let index = SymbolIndex::from_elf_bytes(&data).expect("DWARF index");
    let selector =
        VariableSelector::new("gstF0cRoot.stNested.ulSequence", None).expect("static selector");
    let plan = index.access_plan(&selector).expect("access plan");
    let firmware = FirmwareImage::parse("F0cDwarfFixture.out", &data, None)
        .expect("ELF image")
        .symbol_identity_plan()
        .expect("firmware identity plan");
    assert_eq!(plan.elf_sha256(), firmware.elf_sha256());

    let request = DebugRequest::WriteVariable {
        plan,
        firmware,
        value: json!(7),
        verify: WriteVerify::None,
    };
    request.validate().expect("same-ELF execution payload");
    let encoded = serde_json::to_value(&request).expect("serialize debug request");
    let decoded: DebugRequest = serde_json::from_value(encoded).expect("deserialize debug request");
    decoded.validate().expect("revalidated IPC payload");
}

#[test]
#[ignore = "requires the explicitly selected IAR target OUT"]
fn t_p2_mem_target_fixture_is_retained_and_has_static_dwarf_layout() {
    let path = env::var_os("JLINK_MCP_T_P2_MEM_ELF")
        .map(PathBuf::from)
        .expect("JLINK_MCP_T_P2_MEM_ELF must name the selected IAR OUT");
    let data = fs::read(&path).expect("selected IAR target OUT");
    let index = SymbolIndex::from_elf_bytes(&data).expect("target DWARF index");
    let firmware = FirmwareImage::parse("T26_DCU_APP_NXP.out", &data, None)
        .expect("target ELF image")
        .symbol_identity_plan()
        .expect("target firmware identity plan");
    assert!(firmware.segments().iter().all(|segment| {
        let end = segment.address() + segment.length();
        end <= 0x0008_0000 || (segment.address() >= 0x1000_0000 && end <= 0x1001_0000)
    }));

    let composite = index
        .access_plan(
            &VariableSelector::new("gstAppUserDescJlinkTest", None)
                .expect("composite fixture selector"),
        )
        .expect("retained composite fixture");
    let member = index
        .access_plan(
            &VariableSelector::new("gstAppUserDescJlinkTest.ulSequence", None)
                .expect("composite member selector"),
        )
        .expect("retained composite member");
    let array = index
        .access_plan(
            &VariableSelector::new("gaulAppUserDescHssTest", None)
                .expect("HSS array fixture selector"),
        )
        .expect("retained HSS array fixture");
    let first = index
        .access_plan(
            &VariableSelector::new("gaulAppUserDescHssTest[0]", None)
                .expect("first HSS element selector"),
        )
        .expect("first HSS array element");
    let last = index
        .access_plan(
            &VariableSelector::new("gaulAppUserDescHssTest[9]", None)
                .expect("last HSS element selector"),
        )
        .expect("last HSS array element");
    let writable = index
        .access_plan(
            &VariableSelector::new("gulAppUserDescWritableTest", None)
                .expect("writable fixture selector"),
        )
        .expect("retained writable fixture");

    assert_eq!(composite.byte_size(), 8);
    assert_eq!(member.byte_size(), 4);
    assert_eq!(array.byte_size(), 40);
    assert_eq!(first.byte_size(), 4);
    assert_eq!(last.address() - first.address(), 9 * 4);
    assert_eq!(writable.byte_size(), 4);
    for plan in [&composite, &member, &array, &first, &last, &writable] {
        assert!((0x1FFF_0000..=0x2003_EFFF).contains(&plan.address()));
    }
}

struct MemoryDispatcher;

impl ToolDispatcher for MemoryDispatcher {
    fn call(&mut self, name: &str, arguments: &Value) -> ToolCall {
        match (name, arguments["action"].as_str()) {
            ("jlink_inspect", Some("memory")) => ToolCall::success(json!({ "data": "0102aaff" })),
            ("jlink_write", Some("memory")) if arguments["address"] == "0x0" => ToolCall::Error(
                JlinkError::new(
                    ErrorCode::AddressOutOfRange,
                    "普通内存写入不能修改 Flash，请使用 jlink_program",
                    false,
                )
                .with_detail("use_tool", json!("jlink_program")),
            ),
            ("jlink_write", Some("memory")) if arguments["address"] == "0x20000000" => {
                ToolCall::Error(
                    JlinkError::new(ErrorCode::ExecutionUncertain, "目标只写入部分字节", false)
                        .with_detail("requested_length", json!(4))
                        .with_detail("actual_length", json!(2)),
                )
            }
            ("jlink_write", Some("variable")) => ToolCall::Error(
                JlinkError::new(ErrorCode::VerifyFailed, "变量读回不一致", false)
                    .with_detail("first_address", json!("0x20000004")),
            ),
            _ => ToolCall::success(json!({})),
        }
    }

    fn read_resource(&mut self, uri: &str) -> ToolCall {
        ToolCall::Unavailable(format!("resource unavailable: {uri}"))
    }
}

#[test]
fn t_p2_mem_returns_minimal_success_and_stable_public_errors() {
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "jlink_inspect",
                "arguments": {
                    "action": "memory",
                    "address": "0x20000000",
                    "length": 4
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "jlink_write",
                "arguments": {
                    "action": "memory",
                    "address": "0x0",
                    "data": "01020304"
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "jlink_write",
                "arguments": {
                    "action": "memory",
                    "address": "0x20000000",
                    "data": "01020304"
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "jlink_write",
                "arguments": {
                    "action": "variable",
                    "path": "ulWritable",
                    "value": 7,
                    "verify": "readback"
                }
            }
        }),
    ];
    let input = requests
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut output = Vec::new();
    serve(Cursor::new(input), &mut output, &mut MemoryDispatcher).expect("stdio server");
    let responses = String::from_utf8(output)
        .expect("UTF-8 output")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSON response"))
        .collect::<Vec<_>>();

    assert_eq!(
        responses[0]["result"]["structuredContent"],
        json!({ "data": "0102aaff" })
    );
    assert_eq!(
        responses[1]["result"]["structuredContent"]["error"]["code"],
        "ADDRESS_OUT_OF_RANGE"
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"]["error"]["code"],
        "EXECUTION_UNCERTAIN"
    );
    assert_eq!(
        responses[2]["result"]["structuredContent"]["error"]["details"]["requested_length"],
        4
    );
    assert_eq!(
        responses[3]["result"]["structuredContent"]["error"]["code"],
        "VERIFY_FAILED"
    );
}
