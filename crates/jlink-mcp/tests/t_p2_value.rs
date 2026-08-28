//! Primary T-P2-VALUE verification for lossless values and compound prevalidation.

use std::{fs, io::Cursor, path::PathBuf};

use jlink_domain::{
    AccessLayout, AccessMember, AccessPlan, BitRange, ElementSlice, ErrorCode, JlinkError,
    ScalarEncoding, VariableSelector, decode_typed_value, encode_typed_value,
};
use jlink_mcp::{
    mcp::{ToolCall, ToolDispatcher, serve, tool_catalog},
    symbols::SymbolIndex,
};
use object::{Object, ObjectSection};
use serde_json::{Value, json};

fn iar_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../validation/evidence/f0-c/F0cDwarfFixture.out")
}

fn iar_fixture() -> Vec<u8> {
    let path = iar_fixture_path();
    fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "frozen IAR F0-C fixture is required at {}: {error}",
            path.display()
        )
    })
}

fn access_plan(index: &SymbolIndex, path: &str, slice: Option<ElementSlice>) -> AccessPlan {
    let selector = VariableSelector::new(path, slice).expect("valid test selector");
    index.access_plan(&selector).expect("fixture access plan")
}

fn plan_bytes(elf: &[u8], plan: &AccessPlan) -> Vec<u8> {
    let object = object::File::parse(elf).expect("parse fixture ELF");
    let end = plan
        .address()
        .checked_add(plan.byte_size())
        .expect("plan range");
    for section in object.sections() {
        let start = section.address();
        let section_end = start.checked_add(section.size()).expect("section range");
        if start <= plan.address() && end <= section_end {
            let data = section.data().expect("fixture section data");
            let offset = usize::try_from(plan.address() - start).expect("section offset");
            let length = usize::try_from(plan.byte_size()).expect("plan length");
            return data
                .get(offset..offset + length)
                .unwrap_or_else(|| panic!("section data is shorter than plan {}", plan.address()))
                .to_vec();
        }
    }
    panic!(
        "fixture has no section for plan range {:#x}..{end:#x}",
        plan.address()
    );
}

#[test]
fn t_p2_value_decodes_and_round_trips_the_iar_fixture() {
    let elf = iar_fixture();
    let index = SymbolIndex::from_elf_bytes(&elf).expect("parse IAR fixture");

    let root_plan = access_plan(&index, "gstF0cRoot", None);
    let root_bytes = plan_bytes(&elf, &root_plan);
    let root = decode_typed_value(&root_plan, &root_bytes).expect("decode root structure");
    assert_eq!(root["stNested"]["ulSequence"], 7);
    assert_eq!(root["stNested"]["awMatrix"][1][2], 3);
    assert_eq!(root["stFlags"]["uiReadyFlg"], 1);
    assert_eq!(root["stFlags"]["iDelta"], -7);
    assert_eq!(root["stFlags"]["uiMode"], 5);
    assert_eq!(root["unPayload"]["$union"]["fPhysicalValue"], 1.0);
    assert_eq!(
        root["ullCounter"],
        json!({
            "$int": "18364758544493064720",
            "bits": 64,
            "signed": false
        })
    );
    assert_eq!(root["llOffset"], -5_124_095_576_030_430_i64);
    let mut writable_root = root.clone();
    writable_root["unPayload"] = json!({
        "$union": {
            "ulRawValue": root["unPayload"]["$union"]["ulRawValue"].clone()
        }
    });
    assert_eq!(
        encode_typed_value(&root_plan, &root_bytes, &writable_root)
            .expect("round-trip root with one explicit union member"),
        root_bytes
    );

    for (path, expected) in [
        (
            "gaunF0cFloatSpecial[0].fPhysicalValue",
            json!({ "$float": "nan" }),
        ),
        (
            "gaunF0cFloatSpecial[1].fPhysicalValue",
            json!({ "$float": "inf" }),
        ),
        (
            "gaunF0cDoubleSpecial[0].dPhysicalValue",
            json!({ "$float": "nan" }),
        ),
        (
            "gaunF0cDoubleSpecial[1].dPhysicalValue",
            json!({ "$float": "inf" }),
        ),
    ] {
        let plan = access_plan(&index, path, None);
        let bytes = plan_bytes(&elf, &plan);
        let value = decode_typed_value(&plan, &bytes).expect("decode non-finite value");
        assert_eq!(value, expected, "{path}");
        assert_eq!(
            encode_typed_value(&plan, &bytes, &value).expect("encode non-finite value"),
            bytes,
            "{path}"
        );
    }
}

#[test]
fn t_p2_value_requires_an_independent_slice_for_flexible_arrays() {
    let elf = iar_fixture();
    let index = SymbolIndex::from_elf_bytes(&elf).expect("parse IAR fixture");

    let missing = VariableSelector::new("gstF0cFlex.aucPayload", None).expect("selector");
    let error = index
        .access_plan(&missing)
        .expect_err("flexible array requires slice");
    assert_eq!(error.code(), ErrorCode::SliceRequired);

    let indexed = VariableSelector::new("gstF0cFlex.aucPayload[0]", None).expect("selector");
    let error = index
        .access_plan(&indexed)
        .expect_err("path index cannot replace slice");
    assert_eq!(error.code(), ErrorCode::SliceRequired);
    assert_eq!(
        ElementSlice::new(0, 0).expect_err("empty slice").code(),
        ErrorCode::SliceRequired
    );
    assert_eq!(
        ElementSlice::new(u64::MAX, 2)
            .expect_err("overflowing slice")
            .code(),
        ErrorCode::SliceRequired
    );

    let plan = access_plan(
        &index,
        "gstF0cFlex.aucPayload",
        Some(ElementSlice::new(0, 1).expect("single-element slice")),
    );
    assert_eq!(plan.address(), 0x2000_1002);
    let bytes = plan_bytes(&elf, &plan);
    assert_eq!(
        decode_typed_value(&plan, &bytes).expect("slice value"),
        json!([11])
    );
}

#[test]
fn t_p2_value_prevalidates_compound_and_union_writes() {
    let elf = iar_fixture();
    let index = SymbolIndex::from_elf_bytes(&elf).expect("parse IAR fixture");
    let root_plan = access_plan(&index, "gstF0cRoot", None);
    let root_bytes = plan_bytes(&elf, &root_plan);
    let mut invalid = decode_typed_value(&root_plan, &root_bytes).expect("decode root");
    invalid["stNested"]["awMatrix"][1][2] = json!(40_000);
    let error = encode_typed_value(&root_plan, &root_bytes, &invalid)
        .expect_err("invalid nested i16 rejects whole compound value");
    assert_eq!(error.code(), ErrorCode::ValueInvalid);
    assert_eq!(
        error.details.expect("invalid path")["path"],
        "$.stNested.awMatrix[1][2]"
    );
    assert_eq!(
        plan_bytes(&elf, &root_plan),
        root_bytes,
        "prevalidation cannot mutate the caller's current bytes"
    );

    let union_plan = access_plan(&index, "gstF0cRoot.unPayload", None);
    let union_bytes = plan_bytes(&elf, &union_plan);
    let multiple = json!({
        "$union": {
            "ulRawValue": 1,
            "fPhysicalValue": 2.0
        }
    });
    let error = encode_typed_value(&union_plan, &union_bytes, &multiple)
        .expect_err("union write requires one member");
    assert_eq!(error.code(), ErrorCode::ValueInvalid);

    let encoded = encode_typed_value(
        &union_plan,
        &union_bytes,
        &json!({ "$union": { "fPhysicalValue": 2.0 } }),
    )
    .expect("single union member write");
    assert_eq!(encoded, 2.0_f32.to_le_bytes());
    let union = decode_typed_value(&union_plan, &encoded).expect("decode updated union");
    assert_eq!(union["$union"]["fPhysicalValue"], 2.0);
}

#[test]
fn t_p2_value_preserves_pointer_identity() {
    let selector = VariableSelector::new("gpFixture", None).expect("pointer selector");
    let plan = AccessPlan::new(
        "0".repeat(64),
        selector,
        0x2000_2000,
        4,
        None,
        false,
        AccessLayout::Pointer { byte_size: 4 },
    );
    let bytes = 0x2000_1000_u32.to_le_bytes();
    let value = decode_typed_value(&plan, &bytes).expect("decode pointer");
    assert_eq!(value, json!({ "$pointer": "0x20001000" }));
    assert_eq!(
        encode_typed_value(&plan, &bytes, &value).expect("encode pointer"),
        bytes
    );
}

#[test]
fn t_p2_value_preserves_neighbors_of_signed_data_bit_offset_field() {
    let selector = VariableSelector::new("value.field", None).expect("bit-field selector");
    let plan = AccessPlan::new(
        "0".repeat(64),
        selector,
        0x2000_0000,
        1,
        Some(BitRange::new(0, 3)),
        false,
        AccessLayout::Scalar {
            name: "int8_t".to_owned(),
            byte_size: 1,
            encoding: ScalarEncoding::Signed,
        },
    );
    let current = [0xA5];
    assert_eq!(
        decode_typed_value(&plan, &current).expect("decode signed bit-field"),
        json!(-3)
    );
    assert_eq!(
        encode_typed_value(&plan, &current, &json!(-2)).expect("encode signed bit-field"),
        vec![0xA6]
    );
}

#[test]
fn t_p2_value_union_read_keeps_each_interpretable_member() {
    let selector = VariableSelector::new("unFixture", None).expect("union selector");
    let plan = AccessPlan::new(
        "0".repeat(64),
        selector,
        0x2000_3000,
        16,
        None,
        false,
        AccessLayout::Union {
            byte_size: 16,
            members: vec![
                AccessMember::new(
                    "good".to_owned(),
                    0,
                    None,
                    None,
                    None,
                    AccessLayout::Scalar {
                        name: "uint32_t".to_owned(),
                        byte_size: 4,
                        encoding: ScalarEncoding::Unsigned,
                    },
                ),
                AccessMember::new(
                    "unsupported".to_owned(),
                    0,
                    None,
                    None,
                    None,
                    AccessLayout::Scalar {
                        name: "long double".to_owned(),
                        byte_size: 16,
                        encoding: ScalarEncoding::Float,
                    },
                ),
            ],
        },
    );
    let mut bytes = [0_u8; 16];
    bytes[..4].copy_from_slice(&7_u32.to_le_bytes());
    let decoded = decode_typed_value(&plan, &bytes).expect("decode supported union view");
    assert_eq!(decoded, json!({ "$union": { "good": 7 } }));
}

struct SliceErrorDispatcher;

impl ToolDispatcher for SliceErrorDispatcher {
    fn call(&mut self, _name: &str, _arguments: &Value) -> ToolCall {
        ToolCall::Error(JlinkError::new(
            ErrorCode::SliceRequired,
            "柔性数组需要独立显式 slice",
            false,
        ))
    }

    fn read_resource(&mut self, uri: &str) -> ToolCall {
        ToolCall::Unavailable(format!("resource unavailable: {uri}"))
    }
}

#[test]
fn t_p2_value_exposes_write_slice_and_public_error_code() {
    let catalog = tool_catalog();
    let write = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_write")
        .expect("write tool");
    assert!(jsonschema::is_valid(
        &write["inputSchema"],
        &json!({
            "action": "variable",
            "path": "gstF0cFlex.aucPayload",
            "slice": { "start": 0, "count": 1 },
            "value": [11]
        })
    ));

    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "jlink_inspect",
            "arguments": {
                "action": "variable",
                "path": "gstF0cFlex.aucPayload"
            }
        }
    });
    let mut output = Vec::new();
    serve(
        Cursor::new(format!("{request}\n")),
        &mut output,
        &mut SliceErrorDispatcher,
    )
    .expect("stdio server");
    let response: Value = serde_json::from_slice(&output).expect("JSON response");
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"],
        "SLICE_REQUIRED"
    );
    assert_eq!(response["result"]["isError"], true);
}

#[test]
fn t_p2_value_schema_matches_the_recursive_runtime_contract() {
    let catalog = tool_catalog();
    let write = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_write")
        .expect("write tool");
    let inspect = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_inspect")
        .expect("inspect tool");
    let hss = catalog
        .iter()
        .find(|tool| tool["name"] == "jlink_hss")
        .expect("HSS tool");

    let schema = &write["inputSchema"];
    for value in [
        json!([17, 34]),
        json!({ "rows": [[1, 2], [3, 4]], "enabled": true }),
        json!({ "$int": "18364758544493064720", "bits": 64, "signed": false }),
        json!({ "$float": "nan" }),
        json!({ "$pointer": "0x20001000" }),
        json!({ "$union": { "member": [1, 2] } }),
    ] {
        let request = json!({
            "action": "variable",
            "path": "stFixture",
            "value": value
        });
        assert!(jsonschema::is_valid(schema, &request), "invalid {request}");
    }
    for value in [json!(["17", "34"]), json!("17"), Value::Null] {
        let request = json!({
            "action": "variable",
            "path": "stFixture",
            "value": value
        });
        assert!(!jsonschema::is_valid(schema, &request), "valid {request}");
    }

    let typed_value = &schema["$defs"]["typedValue"];
    assert!(
        !typed_value.is_null(),
        "write Schema exposes typedValue definition"
    );
    assert_eq!(
        inspect["outputSchema"]["$defs"]["typedValue"], *typed_value,
        "variable reads use the same TypedValue definition"
    );
    assert_eq!(
        hss["inputSchema"]["$defs"]["typedValue"], *typed_value,
        "HSS rule values use the same TypedValue definition"
    );
    assert_eq!(
        hss["outputSchema"]["$defs"]["typedValue"], *typed_value,
        "HSS sample values use the same TypedValue definition"
    );
}
