//! Primary T-P2-DWARF fixture and cache verification.

use std::{env, fs, io::Cursor, path::PathBuf};

use jlink_domain::{
    AccessLayout, ElementSlice, ErrorCode, JlinkError, TargetInterface, VariableSelector,
};
use jlink_mcp::{
    config::{ConfigFile, ConfigPaths, JlinkConfig, SymbolsConfig, TargetConfig},
    mcp::{ToolCall, ToolDispatcher, serve},
    runtime::Runtime,
    symbols::{SymbolCache, SymbolIndex},
};
use serde_json::json;

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

fn external_t26_out_path() -> PathBuf {
    env::var_os("JLINK_MCP_T26_ELF")
        .map(PathBuf::from)
        .expect("JLINK_MCP_T26_ELF must name the external T26 ELF/OUT artifact")
}

fn external_t26_expected_sha256() -> String {
    env::var("JLINK_MCP_T26_ELF_SHA256")
        .expect("JLINK_MCP_T26_ELF_SHA256 must identify the exact external artifact")
        .to_ascii_lowercase()
}

#[test]
fn t_p2_dwarf_runtime_routes_symbols_without_worker_access() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let project = temporary.path().join("jlink-mcp.toml");
    let user = temporary.path().join("user.toml");
    let config = ConfigFile {
        target: Some(TargetConfig {
            device: Some("S32K144".to_owned()),
            interface: Some(TargetInterface::Swd),
            speed_khz: Some(4_000),
        }),
        symbols: Some(SymbolsConfig {
            elf: Some(iar_fixture_path()),
        }),
        jlink: Some(JlinkConfig {
            dll_path: Some(PathBuf::from("unused-by-symbol-search.dll")),
            version: Some("6.98a".to_owned()),
            sha256: Some("0".repeat(64)),
        }),
        ..ConfigFile::default()
    };
    fs::write(&project, toml::to_string_pretty(&config).expect("TOML")).expect("write config");
    let mut runtime = Runtime::new(
        ConfigPaths::new(project, user),
        temporary.path().join("unused-worker.exe"),
        temporary.path().join("leases"),
    );
    let call = runtime.call(
        "jlink_inspect",
        &json!({ "action": "symbols", "query": "gstF0cRoot.stNested" }),
    );
    let ToolCall::Success {
        structured_content,
        content,
    } = call
    else {
        panic!("symbols action must be routed as a success");
    };
    assert!(content.is_empty());
    let symbols = structured_content["symbols"]
        .as_array()
        .expect("symbols array");
    assert!(!symbols.is_empty());
    assert!(symbols.len() <= 20);
    assert!(symbols.iter().all(|path| {
        path.as_str()
            .is_some_and(|path| path.contains("gstF0cRoot.stNested"))
    }));
}

#[test]
fn t_p2_dwarf_builds_exact_static_plans_from_iar_fixture() {
    let data = iar_fixture();
    let index = SymbolIndex::from_elf_bytes(&data).expect("parse IAR fixture");
    assert_eq!(index.dwarf_versions(), vec![4]);
    assert!(
        index
            .producers()
            .iter()
            .any(|producer| producer.contains("IAR ANSI C/C++ Compiler V8.32.3"))
    );

    let selector =
        VariableSelector::new("gstF0cRoot.stNested.awMatrix[1][2]", None).expect("matrix selector");
    let plan = index.access_plan(&selector).expect("matrix plan");
    assert_eq!(plan.address(), 0x2000_000e);
    assert_eq!(plan.byte_size(), 2);
    assert!(plan.is_volatile());

    let selector = VariableSelector::new(
        "gstF0cFlex.aucPayload",
        Some(ElementSlice::new(1, 3).expect("slice")),
    )
    .expect("flex selector");
    let plan = index.access_plan(&selector).expect("flex plan");
    assert_eq!(plan.address(), 0x2000_1003);
    assert_eq!(plan.byte_size(), 3);
    assert!(matches!(
        plan.layout(),
        AccessLayout::Array { count: Some(3), .. }
    ));

    let flex_paths = index.search("gstF0cFlex", 50).expect("flex search");
    assert!(flex_paths.iter().any(|path| path == "gstF0cFlex.uwLength"));
    assert!(!flex_paths.iter().any(|path| path == "gstF0cFlex"));
    assert!(
        !flex_paths
            .iter()
            .any(|path| path == "gstF0cFlex.aucPayload")
    );
    let root = VariableSelector::new("gstF0cFlex", None).expect("flex root selector");
    let error = index
        .access_plan(&root)
        .expect_err("unbounded aggregate must not form a complete plan");
    assert_eq!(error.code(), ErrorCode::TypeUnsupported);
}

#[test]
fn t_p2_dwarf_searches_stable_usable_paths_and_caches_by_exact_key() {
    let data = iar_fixture();
    let index = SymbolIndex::from_elf_bytes(&data).expect("parse IAR fixture");
    let first = index.search("f0croot", 10).expect("search");
    let second = index.search("F0CROOT", 10).expect("search");
    assert_eq!(first, second);
    assert!(first.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(first.iter().all(|path| path.contains("gstF0cRoot")));
    assert!(first.len() <= 10);

    let mut cache = SymbolCache::new();
    let cached_index = cache.load_bytes(&data).expect("first index");
    let same_index = cache.load_bytes(&data).expect("same index");
    assert!(std::sync::Arc::ptr_eq(&cached_index, &same_index));

    let noncanonical = VariableSelector::new("gstF0cRoot.stNested.awMatrix[01][002]", None)
        .expect("normalized selector");
    let canonical = VariableSelector::new("gstF0cRoot.stNested.awMatrix[1][2]", None)
        .expect("canonical selector");
    let first_plan = cache
        .access_plan(&cached_index, &noncanonical)
        .expect("first plan");
    let second_plan = cache
        .access_plan(&cached_index, &canonical)
        .expect("cached plan");
    assert_eq!(first_plan, second_plan);
    assert_eq!(cache.stats().elf_indexes, 1);
    assert_eq!(cache.stats().access_plans, 1);

    let mut changed = data;
    changed.push(0);
    let changed_index = cache.load_bytes(&changed).expect("changed ELF identity");
    let changed_plan = cache
        .access_plan(&changed_index, &canonical)
        .expect("plan for changed ELF identity");
    assert_ne!(first_plan.elf_sha256(), changed_plan.elf_sha256());
    assert_eq!(cache.stats().elf_indexes, 2);
    assert_eq!(cache.stats().access_plans, 2);
}

#[test]
#[ignore = "requires explicit JLINK_MCP_T26_ELF and JLINK_MCP_T26_ELF_SHA256 inputs"]
fn t_p2_dwarf_indexes_external_t26_ref_sig8_artifact() {
    let path = external_t26_out_path();
    let index = SymbolIndex::from_elf_path(&path).unwrap_or_else(|error| {
        panic!(
            "external T26 ELF/OUT is required at {}: {error}",
            path.display()
        )
    });
    assert_eq!(index.elf_sha256(), external_t26_expected_sha256());
    assert_eq!(index.dwarf_versions(), vec![3, 4]);
    assert!(index.type_unit_count() > 0);
    assert!(index.signature_reference_count() > 0);
    assert!(
        index
            .producers()
            .iter()
            .any(|producer| { *producer == "IAR ANSI C/C++ Compiler V8.32.3.193/W32 for ARM" })
    );
}

struct ErrorDispatcher;

impl ToolDispatcher for ErrorDispatcher {
    fn call(&mut self, _name: &str, arguments: &serde_json::Value) -> ToolCall {
        let (code, message) = match arguments["path"].as_str() {
            Some("dynamic") => (ErrorCode::DynamicLocationUnsupported, "dynamic location"),
            Some("ambiguous") => (ErrorCode::SymbolAmbiguous, "ambiguous symbol"),
            _ => (ErrorCode::SymbolNotFound, "missing symbol"),
        };
        ToolCall::Error(JlinkError::new(code, message, false))
    }

    fn read_resource(&mut self, uri: &str) -> ToolCall {
        ToolCall::Unavailable(format!("resource unavailable: {uri}"))
    }
}

#[test]
fn t_p2_dwarf_public_mcp_errors_preserve_stable_codes() {
    let requests = ["dynamic", "ambiguous"].map(|path| {
        json!({
            "jsonrpc": "2.0",
            "id": path,
            "method": "tools/call",
            "params": {
                "name": "jlink_inspect",
                "arguments": { "action": "variable", "path": path }
            }
        })
    });
    let input = requests
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut output = Vec::new();
    serve(Cursor::new(input), &mut output, &mut ErrorDispatcher).expect("stdio server");
    let responses = String::from_utf8(output).expect("UTF-8 output");
    let responses = responses
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON response"))
        .collect::<Vec<_>>();
    assert_eq!(
        responses[0]["result"]["structuredContent"]["error"]["code"],
        "DYNAMIC_LOCATION_UNSUPPORTED"
    );
    assert_eq!(
        responses[1]["result"]["structuredContent"]["error"]["code"],
        "SYMBOL_AMBIGUOUS"
    );
    assert!(
        responses
            .iter()
            .all(|response| response["result"]["isError"] == true)
    );
}
