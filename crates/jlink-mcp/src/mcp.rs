//! Versioned MCP tool catalog, strict schemas, and stdio JSON-RPC boundary.

use std::io::{self, BufRead, Write};

use data_encoding::BASE64;
use jlink_domain::{ErrorCode, JlinkError};
use serde_json::{Map, Value, json};

/// MIME type reserved for immutable V1 capture resources.
pub const RAW_CAPTURE_MIME: &str = "application/vnd.jlink-mcp.capture.v1+binary";

/// URI template reserved for immutable V1 capture resources.
pub const RAW_CAPTURE_URI_TEMPLATE: &str = "jlink-mcp://capture/{capture_id}/raw";

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const SERVER_INSTRUCTIONS: &str = include_str!("../resources/server-instructions.md");

/// Result produced by the process-owned tool dispatcher.
pub enum ToolCall {
    /// A successful call with authoritative structured content and optional MCP content.
    Success {
        /// Authoritative result validated by the tool output Schema.
        structured_content: Value,
        /// Optional MCP content such as a raw-capture resource link.
        content: Vec<Value>,
    },
    /// A stable domain or device error.
    Error(JlinkError),
    /// A known catalog action whose implementation is not active in this stage.
    Unavailable(String),
}

impl ToolCall {
    /// Creates a minimal successful result without duplicated text content.
    #[must_use]
    pub const fn success(structured_content: Value) -> Self {
        Self::Success {
            structured_content,
            content: Vec::new(),
        }
    }

    /// Creates a successful result containing the immutable raw-capture link.
    #[must_use]
    pub fn with_raw_capture(structured_content: Value, capture_id: &str) -> Self {
        Self::Success {
            structured_content,
            content: vec![raw_capture_resource_link(capture_id)],
        }
    }

    /// Creates one complete binary MCP resource from a verified immutable capture.
    #[must_use]
    pub fn raw_capture_resource(capture_id: &str, resource: &[u8]) -> Self {
        Self::Success {
            structured_content: json!({}),
            content: vec![json!({
                "uri": format!("jlink-mcp://capture/{capture_id}/raw"),
                "mimeType": RAW_CAPTURE_MIME,
                "blob": BASE64.encode(resource)
            })],
        }
    }
}

/// Process-owned execution boundary behind the public MCP contract.
pub trait ToolDispatcher {
    /// Executes one already schema-validated tool call.
    fn call(&mut self, name: &str, arguments: &Value) -> ToolCall;

    /// Reads one immutable resource or reports why it is unavailable.
    fn read_resource(&mut self, uri: &str) -> ToolCall;
}

/// Returns the closed V1 catalog in deterministic order.
#[must_use]
pub fn tool_catalog() -> Vec<Value> {
    vec![
        target_tool(),
        program_tool(),
        inspect_tool(),
        write_tool(),
        control_tool(),
        hss_tool(),
    ]
}

/// Creates the canonical raw-capture resource link for a completed capture.
#[must_use]
pub fn raw_capture_resource_link(capture_id: &str) -> Value {
    json!({
        "type": "resource_link",
        "uri": format!("jlink-mcp://capture/{capture_id}/raw"),
        "name": format!("{capture_id}-raw"),
        "description": "Complete self-describing HSS capture",
        "mimeType": RAW_CAPTURE_MIME
    })
}

/// Serves newline-delimited MCP JSON-RPC messages until the input reaches EOF.
///
/// # Errors
///
/// Returns the underlying input or output error. Protocol failures are returned
/// to the client as JSON-RPC errors and do not terminate the server.
pub fn serve<R, W, D>(reader: R, mut writer: W, dispatcher: &mut D) -> io::Result<()>
where
    R: BufRead,
    W: Write,
    D: ToolDispatcher,
{
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => handle_request(&request, dispatcher),
            Err(error) => Some(protocol_error(
                &Value::Null,
                -32_700,
                format!("Parse error: {error}"),
            )),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut writer, &response)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
    Ok(())
}

fn handle_request<D: ToolDispatcher>(request: &Value, dispatcher: &mut D) -> Option<Value> {
    let Some(object) = request.as_object() else {
        return Some(protocol_error(&Value::Null, -32_600, "Invalid Request"));
    };
    let id = object.get("id").cloned();
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return id.map(|id| protocol_error(&id, -32_600, "Request method is required"));
    };
    id.as_ref()?;
    let id = id.unwrap_or(Value::Null);
    let params = object.get("params").unwrap_or(&Value::Null);
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false }
            },
            "serverInfo": { "name": "jlink-mcp", "version": env!("CARGO_PKG_VERSION") },
            "instructions": SERVER_INSTRUCTIONS
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_catalog() })),
        "tools/call" => call_tool(params, dispatcher),
        "resources/list" => Ok(json!({ "resources": [] })),
        "resources/templates/list" => Ok(json!({
            "resourceTemplates": [{
                "uriTemplate": RAW_CAPTURE_URI_TEMPLATE,
                "name": "jlink-capture-raw",
                "description": "Complete self-describing HSS capture",
                "mimeType": RAW_CAPTURE_MIME
            }]
        })),
        "resources/read" => read_resource(params, dispatcher),
        _ => Err((-32_601, format!("Method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => protocol_error(&id, code, message),
    })
}

fn call_tool<D: ToolDispatcher>(
    params: &Value,
    dispatcher: &mut D,
) -> Result<Value, (i64, String)> {
    let params = params
        .as_object()
        .ok_or_else(|| (-32_602, "tools/call params must be an object".to_owned()))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32_602, "tools/call name is required".to_owned()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let tool = tool_catalog()
        .into_iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| (-32_602, format!("Unknown tool: {name}")))?;
    let input_schema = tool
        .get("inputSchema")
        .expect("catalog tools always contain inputSchema");
    if let Err(error) = jsonschema::validate(input_schema, &arguments) {
        return Err((-32_602, schema_argument_error(name, input_schema, &error)));
    }

    match dispatcher.call(name, &arguments) {
        ToolCall::Success {
            structured_content,
            content,
        } => {
            let action_output_schema = action_output_schema(name, &arguments);
            let output_schema = action_output_schema.as_ref().unwrap_or_else(|| {
                tool.get("outputSchema")
                    .expect("catalog tools always contain outputSchema")
            });
            if let Err(error) = jsonschema::validate(output_schema, &structured_content) {
                return Err((
                    -32_603,
                    format!("Tool {name} produced an invalid structured result: {error}"),
                ));
            }
            Ok(json!({ "content": content, "structuredContent": structured_content }))
        }
        ToolCall::Error(error) => public_tool_error(error).map_err(|message| (-32_603, message)),
        ToolCall::Unavailable(message) => Err((-32_603, message)),
    }
}

fn schema_argument_error(
    name: &str,
    schema: &Value,
    error: &jsonschema::ValidationError<'_>,
) -> String {
    use jsonschema::error::ValidationErrorKind;

    if let Some(message) = discriminator_argument_error(name, schema, error.instance()) {
        return message;
    }
    let error = most_specific_schema_error(error);
    let mut field = error.instance_path().to_string();
    if let ValidationErrorKind::Required { property } = error.kind()
        && let Some(property) = property.as_str()
    {
        field.push('/');
        field.push_str(&escape_json_pointer(property));
    }
    if field.is_empty() {
        field.push('$');
    } else {
        field.insert(0, '$');
    }
    let rule = error.kind().keyword();
    let allowed = schema_allowed_value(schema, error);
    let actual = if matches!(error.kind(), ValidationErrorKind::Required { .. }) {
        "<missing>".to_owned()
    } else {
        bounded_json(error.instance())
    };

    format!(
        "Invalid arguments for {name}: field={field}; rule={rule}; allowed={allowed}; actual={actual}"
    )
}

fn discriminator_argument_error(name: &str, schema: &Value, instance: &Value) -> Option<String> {
    let object = instance.as_object()?;
    let mut candidates = schema.get("oneOf")?.as_array()?.iter().collect::<Vec<_>>();
    for field in ["action", "scope", "view", "mode"] {
        let Some(actual) = object.get(field) else {
            continue;
        };
        let mut allowed = Vec::new();
        for candidate in &candidates {
            let Some(field_schema) = candidate.pointer(&format!("/properties/{field}")) else {
                continue;
            };
            if let Some(value) = field_schema.get("const")
                && !allowed.contains(value)
            {
                allowed.push(value.clone());
            }
            if let Some(values) = field_schema.get("enum").and_then(Value::as_array) {
                for value in values {
                    if !allowed.contains(value) {
                        allowed.push(value.clone());
                    }
                }
            }
        }
        if !allowed.is_empty() && !allowed.contains(actual) {
            return Some(format!(
                "Invalid arguments for {name}: field=$/{field}; rule=enum; allowed={}; actual={}",
                bounded_json(&Value::Array(allowed)),
                bounded_json(actual)
            ));
        }
        candidates.retain(|candidate| {
            candidate
                .pointer(&format!("/properties/{field}"))
                .is_some_and(|field_schema| {
                    field_schema
                        .get("const")
                        .is_none_or(|expected| expected == actual)
                        && field_schema
                            .get("enum")
                            .and_then(Value::as_array)
                            .is_none_or(|values| values.contains(actual))
                })
        });
    }
    None
}

fn most_specific_schema_error<'error, 'instance>(
    error: &'error jsonschema::ValidationError<'instance>,
) -> &'error jsonschema::ValidationError<'instance> {
    use jsonschema::error::ValidationErrorKind;

    let ValidationErrorKind::OneOfNotValid { context } = error.kind() else {
        return error;
    };
    let matching_groups = context
        .iter()
        .filter(|group| !group.iter().any(is_discriminator_mismatch))
        .collect::<Vec<_>>();
    let groups = if matching_groups.is_empty() {
        context.iter().collect::<Vec<_>>()
    } else {
        matching_groups
    };
    groups
        .into_iter()
        .flat_map(|group| group.iter())
        .map(most_specific_schema_error)
        .max_by_key(|candidate| schema_error_score(candidate))
        .unwrap_or(error)
}

fn is_discriminator_mismatch(error: &jsonschema::ValidationError<'_>) -> bool {
    use jsonschema::error::ValidationErrorKind;

    let path = error.instance_path().to_string();
    matches!(
        error.kind(),
        ValidationErrorKind::Constant { .. } | ValidationErrorKind::Enum { .. }
    ) && ["/action", "/scope", "/view", "/mode", "/kind"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

fn schema_error_score(error: &jsonschema::ValidationError<'_>) -> usize {
    use jsonschema::error::ValidationErrorKind;

    let priority = match error.kind() {
        ValidationErrorKind::Minimum { .. }
        | ValidationErrorKind::Maximum { .. }
        | ValidationErrorKind::ExclusiveMinimum { .. }
        | ValidationErrorKind::ExclusiveMaximum { .. }
        | ValidationErrorKind::MinItems { .. }
        | ValidationErrorKind::MaxItems { .. }
        | ValidationErrorKind::MinLength { .. }
        | ValidationErrorKind::MaxLength { .. }
        | ValidationErrorKind::Pattern { .. }
        | ValidationErrorKind::Type { .. }
        | ValidationErrorKind::Enum { .. }
        | ValidationErrorKind::AdditionalProperties { .. } => 300,
        ValidationErrorKind::Required { .. } => 350,
        ValidationErrorKind::Constant { .. } => 100,
        ValidationErrorKind::OneOfNotValid { .. }
        | ValidationErrorKind::OneOfMultipleValid { .. } => 0,
        _ => 200,
    };
    let depth = error.instance_path().to_string().matches('/').count();
    priority + depth
}

fn schema_allowed_value(schema: &Value, error: &jsonschema::ValidationError<'_>) -> String {
    use jsonschema::error::ValidationErrorKind;

    match error.kind() {
        ValidationErrorKind::Minimum { limit } => format!(">={limit}"),
        ValidationErrorKind::Maximum { limit } => format!("<={limit}"),
        ValidationErrorKind::ExclusiveMinimum { limit } => format!(">{limit}"),
        ValidationErrorKind::ExclusiveMaximum { limit } => format!("<{limit}"),
        ValidationErrorKind::MinItems { limit } => format!("items>={limit}"),
        ValidationErrorKind::MaxItems { limit } => format!("items<={limit}"),
        ValidationErrorKind::MinLength { limit } => format!("length>={limit}"),
        ValidationErrorKind::MaxLength { limit } => format!("length<={limit}"),
        ValidationErrorKind::Enum { options } => bounded_json(options),
        ValidationErrorKind::Constant { expected_value } => bounded_json(expected_value),
        ValidationErrorKind::Pattern { pattern } => pattern.clone(),
        ValidationErrorKind::Required { .. } => "required field".to_owned(),
        ValidationErrorKind::AdditionalProperties { .. } => {
            "only fields declared by this action Schema".to_owned()
        }
        ValidationErrorKind::Type { kind } => format!("{kind:?}"),
        _ => schema
            .pointer(&error.schema_path().to_string())
            .map_or_else(|| error.to_string(), bounded_json),
    }
}

fn bounded_json(value: &Value) -> String {
    const MAX_CHARS: usize = 160;
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| "<unprintable>".to_owned());
    if encoded.chars().count() <= MAX_CHARS {
        return encoded;
    }
    let mut bounded = encoded.chars().take(MAX_CHARS).collect::<String>();
    bounded.push('…');
    bounded
}

fn escape_json_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn read_resource<D: ToolDispatcher>(
    params: &Value,
    dispatcher: &mut D,
) -> Result<Value, (i64, String)> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32_602, "resources/read uri is required".to_owned()))?;
    match dispatcher.read_resource(uri) {
        ToolCall::Success {
            structured_content,
            content,
        } => {
            let mut result = json!({ "contents": content });
            if structured_content
                .as_object()
                .is_none_or(|value| !value.is_empty())
            {
                result
                    .as_object_mut()
                    .expect("resource result is an object")
                    .insert("_meta".to_owned(), structured_content);
            }
            Ok(result)
        }
        ToolCall::Error(error) => Err((-32_002, format!("{}: {}", error.code, error.message))),
        ToolCall::Unavailable(message) => Err((-32_002, message)),
    }
}

fn public_tool_error(error: JlinkError) -> Result<Value, String> {
    let public_code = match error.code {
        ErrorCode::ConfigInvalid
        | ErrorCode::DllArchitectureMismatch
        | ErrorCode::DllLoadFailed => "CONFIG_INVALID",
        ErrorCode::OperationConflict | ErrorCode::ProbeBusy => "OPERATION_CONFLICT",
        ErrorCode::DllNotFound => "DLL_NOT_FOUND",
        ErrorCode::DllVersionMismatch => "DLL_VERSION_MISMATCH",
        ErrorCode::DllHashMismatch => "DLL_HASH_MISMATCH",
        ErrorCode::DllExportMissing => "DLL_EXPORT_MISSING",
        ErrorCode::WorkerUnavailable | ErrorCode::TargetConnectFailed => "TARGET_CONNECT_FAILED",
        ErrorCode::InvalidStateTransition => "TARGET_STATE_INVALID",
        ErrorCode::TargetRecoveryFailed => "TARGET_RECOVERY_FAILED",
        ErrorCode::ValueInvalid => "VALUE_INVALID",
        ErrorCode::FirmwareIdentityUnknown => "FIRMWARE_IDENTITY_UNKNOWN",
        ErrorCode::FirmwareElfMismatch => "FIRMWARE_ELF_MISMATCH",
        ErrorCode::SymbolNotFound => "SYMBOL_NOT_FOUND",
        ErrorCode::SymbolAmbiguous => "SYMBOL_AMBIGUOUS",
        ErrorCode::TypeUnsupported => "TYPE_UNSUPPORTED",
        ErrorCode::DynamicLocationUnsupported => "DYNAMIC_LOCATION_UNSUPPORTED",
        ErrorCode::SliceRequired => "SLICE_REQUIRED",
        ErrorCode::AddressOutOfRange => "ADDRESS_OUT_OF_RANGE",
        ErrorCode::FlashRangeInvalid => "FLASH_RANGE_INVALID",
        ErrorCode::RegisterNotFound => "REGISTER_NOT_FOUND",
        ErrorCode::VerifyFailed => "VERIFY_FAILED",
        ErrorCode::FrameInvalid => "FRAME_INVALID",
        ErrorCode::HssUnsupported => "HSS_UNSUPPORTED",
        ErrorCode::HssStartFailed => "HSS_START_FAILED",
        ErrorCode::CaptureKeyConflict => "CAPTURE_KEY_CONFLICT",
        ErrorCode::CursorInvalid => "CURSOR_INVALID",
        ErrorCode::CursorExpired => "CURSOR_EXPIRED",
        ErrorCode::ExecutionUncertain => "EXECUTION_UNCERTAIN",
        ErrorCode::InvalidRequestId
        | ErrorCode::UnknownProtocolVersion
        | ErrorCode::InvalidResponse
        | ErrorCode::IpcProtocolError => {
            return Err(format!(
                "MCP 内部协议失败（{}）：{}",
                error.code, error.message
            ));
        }
    };
    let text = format!("{public_code}: {}", error.message);
    let mut structured_error = json!({
        "code": public_code,
        "message": error.message,
        "retryable": error.retryable
    });
    if let Some(details) = error.details {
        structured_error
            .as_object_mut()
            .expect("structured error is an object")
            .insert("details".to_owned(), json!(details));
    }
    Ok(json!({
        "isError": true,
        "content": [{ "type": "text", "text": text }],
        "structuredContent": { "error": structured_error }
    }))
}

fn protocol_error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn target_tool() -> Value {
    let values_project = non_empty_closed_object(
        vec![
            ("target.device", non_empty_string()),
            ("target.interface", string_enum(&["swd", "jtag"])),
            ("target.speed_khz", positive_integer()),
            ("symbols.elf", non_empty_string()),
            ("firmware.image", non_empty_string()),
            ("jlink.dll_path", non_empty_string()),
            ("jlink.dll_version", non_empty_string()),
            ("jlink.dll_sha256", sha256_schema()),
            ("capture.max_bytes", positive_integer()),
        ],
        &[],
    );
    let values_user = non_empty_closed_object(vec![("probe.serial", positive_integer())], &[]);
    let input = action_union(vec![
        action_object("connect", Vec::new(), &[]),
        action_object("disconnect", Vec::new(), &[]),
        action_object("status", Vec::new(), &[]),
        action_object(
            "validate",
            vec![("after", string_enum(&["run", "halt"]))],
            &[],
        ),
        action_object("config_get", Vec::new(), &[]),
        action_object(
            "config_set",
            vec![
                ("scope", string_enum(&["project", "session"])),
                ("values", values_project),
            ],
            &["scope", "values"],
        ),
        action_object(
            "config_set",
            vec![
                ("scope", json!({ "const": "user" })),
                ("values", values_user),
            ],
            &["scope", "values"],
        ),
    ]);
    tool_definition(
        "jlink_target",
        "Use for: connect/disconnect, target status, validation and layered config. Do not use for: HSS capture status or target execution control. Ambiguity: target status is live connection state; HSS status belongs to jlink_hss.",
        input,
        target_output_schema(),
        annotations(false, false, false),
    )
}

fn program_tool() -> Value {
    let image = non_empty_string();
    let base_address = address_schema();
    let after = string_enum(&["none", "reset_halt", "reset_run"]);
    let input = action_union(vec![
        action_object(
            "flash",
            vec![
                ("image", image.clone()),
                ("base_address", base_address.clone()),
                ("verify", boolean()),
                ("after", after.clone()),
            ],
            &["after"],
        ),
        action_object("erase", vec![("after", after.clone())], &["after"]),
        action_object(
            "erase",
            vec![
                ("address", address_schema()),
                ("length", positive_integer()),
                ("after", after),
            ],
            &["address", "length", "after"],
        ),
        action_object(
            "verify",
            vec![("image", image), ("base_address", base_address)],
            &[],
        ),
    ]);
    tool_definition(
        "jlink_program",
        "Use for: Flash program/erase and whole-image verify. Do not use for: RAM/MMIO writes or write readback. Ambiguity: verify compares an image range; readback belongs to jlink_write.",
        input,
        empty_or_error_output(),
        annotations(false, true, false),
    )
}

fn inspect_tool() -> Value {
    let input = action_union(vec![
        action_object(
            "variable",
            vec![("path", non_empty_string()), ("slice", slice_schema())],
            &["path"],
        ),
        action_object(
            "memory",
            vec![
                ("address", address_schema()),
                ("length", bounded_integer(1, 4_096)),
            ],
            &["address", "length"],
        ),
        action_object("register", vec![("name", non_empty_string())], &["name"]),
        action_object(
            "symbols",
            vec![
                ("query", non_empty_string()),
                ("limit", bounded_integer(1, 50)),
            ],
            &["query"],
        ),
    ]);
    tool_definition(
        "jlink_inspect",
        "Use for: live DWARF variable, memory/register reads and symbol-path discovery. Do not use for: historical capture data. Ambiguity: symbols finds live ELF paths; capture query belongs to jlink_hss.",
        input,
        inspect_output_schema(),
        annotations(true, false, true),
    )
}

fn write_tool() -> Value {
    let verify = string_enum(&["none", "readback"]);
    let input = with_typed_value_definition(action_union(vec![
        action_object(
            "variable",
            vec![
                ("path", non_empty_string()),
                ("slice", slice_schema()),
                ("value", typed_value_schema()),
                ("verify", verify.clone()),
            ],
            &["path", "value"],
        ),
        action_object(
            "memory",
            vec![
                ("address", address_schema()),
                ("data", byte_string_schema()),
                ("verify", verify),
            ],
            &["address", "data"],
        ),
        action_object(
            "register",
            vec![("name", non_empty_string()), ("value", address_schema())],
            &["name", "value"],
        ),
    ]));
    tool_definition(
        "jlink_write",
        "Use for: typed variable, RAM/MMIO or writable-register updates. Do not use for: Flash images. Ambiguity: readback confirms this write; whole-image verify belongs to jlink_program.",
        input,
        empty_or_error_output(),
        annotations(false, true, false),
    )
}

fn control_tool() -> Value {
    let input = action_union(vec![
        action_object("halt", Vec::new(), &[]),
        action_object("resume", Vec::new(), &[]),
        action_object(
            "reset",
            vec![("after", string_enum(&["run", "halt"]))],
            &["after"],
        ),
        action_object("step", Vec::new(), &[]),
    ]);
    tool_definition(
        "jlink_control",
        "Use for: halt/resume/reset/step of the live target. Do not use for: connection status or HSS lifecycle. Ambiguity: target state changes here; capture state changes in jlink_hss.",
        input,
        empty_or_error_output(),
        annotations(false, true, false),
    )
}

#[allow(clippy::too_many_lines)]
fn hss_tool() -> Value {
    let selectors = || {
        json!({
            "type": "array",
            "minItems": 1,
            "maxItems": 10,
            "items": hss_selector_schema()
        })
    };
    let rules = || json!({ "type": "array", "items": threshold_rule_schema() });
    let mut variants = vec![
        action_object(
            "plan",
            vec![
                ("duration_s", bounded_integer(1, 300)),
                ("rate_hz", bounded_integer(1, 1_000)),
                ("variables", selectors()),
            ],
            &["duration_s", "rate_hz", "variables"],
        ),
        action_object(
            "start",
            vec![
                ("capture_key", non_empty_string()),
                ("duration_s", bounded_integer(1, 300)),
                ("rate_hz", bounded_integer(1, 1_000)),
                ("variables", selectors()),
                ("return_when", string_enum(&["started", "completed"])),
                ("rules", rules()),
            ],
            &[
                "capture_key",
                "duration_s",
                "rate_hz",
                "variables",
                "return_when",
            ],
        ),
    ];
    variants.extend(capture_identity_variants("status", &[], &[]));
    variants.extend(capture_identity_variants(
        "query",
        &[("cursor", non_empty_string())],
        &["cursor"],
    ));
    variants.extend(capture_identity_variants(
        "query",
        &[("view", json!({ "const": "overview" }))],
        &["view"],
    ));
    variants.extend(capture_identity_variants(
        "query",
        &[
            ("view", json!({ "const": "changes" })),
            ("series", non_empty_unique_string_array()),
            ("from_us", non_negative_integer()),
            ("to_us", positive_integer()),
            (
                "rules",
                json!({ "type": "array", "items": threshold_rule_schema() }),
            ),
            ("limit", bounded_integer(1, 1_000)),
        ],
        &["view"],
    ));
    variants.extend(capture_identity_variants(
        "query",
        &[
            ("view", json!({ "const": "window" })),
            ("series", non_empty_unique_string_array()),
            ("from_us", non_negative_integer()),
            ("to_us", positive_integer()),
            ("mode", string_enum(&["raw", "transitions"])),
            ("limit", bounded_integer(1, 1_000)),
        ],
        &["view", "series", "from_us", "to_us", "mode"],
    ));
    variants.extend(capture_identity_variants(
        "query",
        &[
            ("view", json!({ "const": "window" })),
            ("series", non_empty_unique_string_array()),
            ("from_us", non_negative_integer()),
            ("to_us", positive_integer()),
            ("mode", string_enum(&["min_max", "first_last"])),
            ("points", bounded_integer(1, 1_000)),
            ("limit", bounded_integer(1, 1_000)),
        ],
        &["view", "series", "from_us", "to_us", "mode", "points"],
    ));
    variants.extend(capture_identity_variants(
        "query",
        &[
            ("view", json!({ "const": "around_event" })),
            ("event_id", non_empty_string()),
            ("before_us", non_negative_integer()),
            ("after_us", non_negative_integer()),
            ("series", non_empty_unique_string_array()),
            ("limit", bounded_integer(1, 1_000)),
        ],
        &["view", "event_id", "before_us", "after_us"],
    ));
    tool_definition(
        "jlink_hss",
        "Use for: plan/start fixed-duration capture; status or query overview/changes/window/around_event. Do not use for: live reads or target status. Ambiguity: use one capture_id/capture_key; cursor continues pages.",
        with_hss_input_definitions(action_union(variants)),
        hss_output_schema(),
        annotations(false, true, true),
    )
}

fn capture_identity_variants(
    action: &str,
    fields: &[(&'static str, Value)],
    required: &[&str],
) -> Vec<Value> {
    let mut variant_fields = fields.to_vec();
    variant_fields.push(("capture_id", non_empty_string()));
    variant_fields.push(("capture_key", non_empty_string()));
    let mut variant = action_object(action, variant_fields, required);
    variant
        .as_object_mut()
        .expect("capture identity action Schema is an object")
        .insert(
            "oneOf".to_owned(),
            json!([
                { "required": ["capture_id"] },
                { "required": ["capture_key"] }
            ]),
        );
    vec![variant]
}

fn target_output_schema() -> Value {
    let effective = config_map_schema(&Value::Bool(true));
    let sources = config_map_schema(&string_enum(&[
        "request",
        "session",
        "user",
        "project",
        "discovered",
        "default",
    ]));
    closed_object(
        vec![
            (
                "notices",
                json!({ "type": "array", "items": string_enum(&["resumed_from_halt", "reset_after_fault"]) }),
            ),
            (
                "connection",
                string_enum(&["disconnected", "connecting", "connected", "faulted"]),
            ),
            (
                "state",
                string_enum(&["running", "halted", "hardfault", "unknown"]),
            ),
            ("valid", boolean()),
            (
                "checks",
                json!({ "type": "array", "items": validation_check_schema() }),
            ),
            (
                "target_state",
                string_enum(&["running", "halted", "hardfault", "unknown"]),
            ),
            ("target_id", non_negative_integer()),
            ("validation_runs", non_negative_integer()),
            (
                "recovery_notifications",
                json!({ "type": "array", "items": string_enum(&["resumed_from_halt", "reset_after_fault"]) }),
            ),
            ("profile_validation", Value::Bool(true)),
            ("effective", effective),
            ("sources", sources),
            ("missing", Value::Bool(true)),
            ("operations", Value::Bool(true)),
            ("provenance", Value::Bool(true)),
            ("conflicts", Value::Bool(true)),
            ("diagnostics", Value::Bool(true)),
            ("dll_selection", Value::Bool(true)),
            ("profile", Value::Bool(true)),
            ("error", error_schema()),
        ],
        &[],
    )
}

fn inspect_output_schema() -> Value {
    let variants = vec![
        inspect_success_schema_body("variable"),
        inspect_success_schema_body("memory"),
        inspect_success_schema_body("register"),
        inspect_success_schema_body("symbols"),
        closed_object(vec![("error", error_schema())], &["error"]),
    ];
    with_typed_value_definition(json!({
        "type": "object",
        "properties": {
            "value": {},
            "firmware_identity": {},
            "data": {},
            "symbols": {},
            "error": {}
        },
        "additionalProperties": false,
        "oneOf": variants
    }))
}

fn action_output_schema(name: &str, arguments: &Value) -> Option<Value> {
    match name {
        "jlink_inspect" => Some(inspect_success_schema(
            arguments
                .get("action")
                .and_then(Value::as_str)
                .expect("MCP Schema guarantees inspect.action"),
        )),
        "jlink_hss" => Some(hss_action_output_schema(arguments)),
        _ => None,
    }
}

fn inspect_success_schema(action: &str) -> Value {
    with_typed_value_definition(inspect_success_schema_body(action))
}

fn inspect_success_schema_body(action: &str) -> Value {
    match action {
        "variable" => closed_object(
            vec![
                ("value", typed_value_schema()),
                ("firmware_identity", Value::Bool(true)),
            ],
            &["value"],
        ),
        "memory" => closed_object(vec![("data", byte_string_schema())], &["data"]),
        "register" => closed_object(vec![("value", address_schema())], &["value"]),
        "symbols" => closed_object(vec![("symbols", string_array())], &["symbols"]),
        _ => unreachable!("inspect action was validated against the closed catalog"),
    }
}

fn hss_output_schema() -> Value {
    let body = hss_output_schema_body();
    let variants = body["oneOf"]
        .as_array()
        .expect("HSS output union exposes variants")
        .iter()
        .map(|variant| {
            let is_plan = variant["required"]
                .as_array()
                .is_some_and(|required| required.contains(&json!("duration_s")));
            let properties = variant["properties"]
                .as_object()
                .expect("HSS output variant exposes properties")
                .iter()
                .map(|(name, schema)| {
                    let schema = if is_plan {
                        schema.clone()
                    } else {
                        Value::Bool(true)
                    };
                    (name.clone(), schema)
                })
                .collect::<Map<_, _>>();
            let mut compact = json!({
                "type": "object",
                "properties": properties,
                "required": variant["required"].clone(),
                "additionalProperties": false
            });
            if let Some(all_of) = variant.get("allOf") {
                compact
                    .as_object_mut()
                    .expect("compact HSS output is an object")
                    .insert("allOf".to_owned(), all_of.clone());
            }
            compact
        })
        .collect::<Vec<_>>();
    with_typed_value_definition(closed_schema_union(&variants))
}

fn hss_output_schema_body() -> Value {
    closed_schema_union(&[
        hss_plan_output_schema(),
        hss_status_output_schema(),
        hss_overview_output_schema(),
        hss_changes_output_schema(),
        hss_window_rows_output_schema(),
        hss_window_buckets_output_schema(),
        hss_around_event_output_schema(),
        hss_completed_start_output_schema(),
        closed_object(vec![("error", error_schema())], &["error"]),
    ])
}

fn hss_action_output_schema(arguments: &Value) -> Value {
    let schema = match arguments
        .get("action")
        .and_then(Value::as_str)
        .expect("MCP Schema guarantees hss.action")
    {
        "plan" => hss_plan_output_schema(),
        "start" => closed_schema_union(&[
            hss_status_output_schema(),
            hss_completed_start_output_schema(),
        ]),
        "status" => hss_status_output_schema(),
        "query" if arguments.get("cursor").is_some() => hss_output_schema_body(),
        "query" => match arguments
            .get("view")
            .and_then(Value::as_str)
            .expect("non-cursor query Schema guarantees hss.view")
        {
            "overview" => hss_overview_output_schema(),
            "changes" => hss_changes_output_schema(),
            "window" => match arguments
                .get("mode")
                .and_then(Value::as_str)
                .expect("window query Schema guarantees mode")
            {
                "raw" | "transitions" => hss_window_rows_output_schema(),
                "min_max" | "first_last" => hss_window_buckets_output_schema(),
                _ => unreachable!("window mode passed closed MCP Schema"),
            },
            "around_event" => hss_around_event_output_schema(),
            _ => hss_output_schema_body(),
        },
        _ => unreachable!("HSS action was validated against the closed catalog"),
    };
    with_hss_output_definitions(schema)
}

fn hss_status_output_schema() -> Value {
    let mut schema = closed_object(
        vec![
            ("capture_id", non_empty_string()),
            ("state", hss_state_schema()),
            ("elapsed_us", non_negative_integer()),
            ("complete_records", non_negative_integer()),
            ("from_us", non_negative_integer()),
            ("to_us", non_negative_integer()),
            ("failure_code", non_empty_string()),
            ("partial_available", boolean()),
            ("reason", non_empty_string()),
            ("recoverable", boolean()),
            ("quality", hss_quality_schema()),
        ],
        &["capture_id", "state"],
    );
    schema
        .as_object_mut()
        .expect("HSS status Schema is an object")
        .insert(
            "allOf".to_owned(),
            json!([{
                "if": {
                    "properties": { "state": { "const": "completed" } },
                    "required": ["state"]
                },
                "then": {
                    "required": ["elapsed_us", "complete_records"],
                    "not": {
                        "anyOf": [
                            { "required": ["from_us"] },
                            { "required": ["to_us"] },
                            { "required": ["quality"] }
                        ]
                    }
                }
            }]),
        );
    schema
}

fn hss_overview_output_schema() -> Value {
    closed_object(
        vec![
            ("capture_id", non_empty_string()),
            ("from_us", non_negative_integer()),
            ("to_us", non_negative_integer()),
            (
                "dictionary",
                json!({ "type": "object", "additionalProperties": non_empty_string() }),
            ),
            ("variables", hss_overview_variables_schema()),
            ("events", non_negative_integer()),
            ("quality", hss_quality_schema()),
        ],
        &[
            "capture_id",
            "from_us",
            "to_us",
            "dictionary",
            "variables",
            "events",
        ],
    )
}

fn hss_completed_start_output_schema() -> Value {
    let mut schema = hss_overview_output_schema();
    let object = schema
        .as_object_mut()
        .expect("overview Schema is an object");
    let properties = object
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .expect("overview Schema has properties");
    properties.insert("state".to_owned(), json!({ "const": "completed" }));
    properties.insert("elapsed_us".to_owned(), non_negative_integer());
    let required = object
        .get_mut("required")
        .and_then(Value::as_array_mut)
        .expect("overview Schema has required fields");
    required.push(json!("state"));
    required.push(json!("elapsed_us"));
    schema
}

fn hss_changes_output_schema() -> Value {
    closed_object(
        vec![
            (
                "dictionary",
                json!({ "type": "object", "additionalProperties": non_empty_string() }),
            ),
            (
                "changes",
                json!({ "type": "array", "items": hss_change_item_schema() }),
            ),
            (
                "matches",
                json!({
                    "type": "array",
                    "items": closed_object(
                        vec![
                            ("rule", non_empty_string()),
                            ("series", non_empty_string()),
                            ("after_us", non_negative_integer()),
                            ("observed_by_us", non_negative_integer()),
                        ],
                        &["rule", "series", "after_us", "observed_by_us"],
                    )
                }),
            ),
            (
                "events",
                json!({ "type": "array", "items": hss_capture_event_schema() }),
            ),
            (
                "relations",
                json!({ "type": "array", "items": hss_event_change_relation_schema() }),
            ),
            ("truncated", boolean()),
            ("next_cursor", non_empty_string()),
        ],
        &[
            "dictionary",
            "changes",
            "matches",
            "events",
            "relations",
            "truncated",
        ],
    )
}

fn hss_change_item_schema() -> Value {
    schema_ref("hssChange")
}

fn hss_change_item_definition() -> Value {
    closed_object(
        vec![
            ("series", non_empty_string()),
            ("after_us", non_negative_integer()),
            ("observed_by_us", non_negative_integer()),
            ("from", typed_value_schema()),
            ("to", typed_value_schema()),
        ],
        &["series", "after_us", "observed_by_us", "from", "to"],
    )
}

fn hss_window_rows_output_schema() -> Value {
    closed_object(
        vec![
            ("clock", json!({ "const": "sample" })),
            ("dictionary", hss_series_dictionary_schema()),
            (
                "time_us",
                json!({ "type": "array", "items": non_negative_integer() }),
            ),
            (
                "values",
                json!({
                    "type": "object",
                    "additionalProperties": {
                        "type": "array",
                        "items": typed_value_schema()
                    }
                }),
            ),
            ("quality", hss_quality_events_schema()),
            ("truncated", boolean()),
            ("next_cursor", non_empty_string()),
        ],
        &[
            "clock",
            "dictionary",
            "time_us",
            "values",
            "quality",
            "truncated",
        ],
    )
}

fn hss_window_buckets_output_schema() -> Value {
    closed_object(
        vec![
            ("clock", json!({ "const": "sample" })),
            ("dictionary", hss_series_dictionary_schema()),
            (
                "buckets",
                json!({
                    "type": "array",
                    "items": closed_object(
                        vec![
                            ("from_us", non_negative_integer()),
                            ("to_us", positive_integer()),
                            (
                                "values",
                                json!({
                                    "type": "object",
                                    "additionalProperties": {
                                        "type": "array",
                                        "minItems": 2,
                                        "maxItems": 2,
                                        "items": typed_value_schema()
                                    }
                                }),
                            ),
                        ],
                        &["from_us", "to_us", "values"],
                    )
                }),
            ),
            ("quality", hss_quality_events_schema()),
            ("truncated", boolean()),
            ("next_cursor", non_empty_string()),
        ],
        &["clock", "dictionary", "buckets", "quality", "truncated"],
    )
}

fn hss_around_event_output_schema() -> Value {
    closed_object(
        vec![
            ("event", hss_capture_event_schema()),
            (
                "window",
                closed_object(
                    vec![
                        ("from_us", non_negative_integer()),
                        ("to_us", positive_integer()),
                    ],
                    &["from_us", "to_us"],
                ),
            ),
            ("dictionary", hss_series_dictionary_schema()),
            (
                "changes",
                json!({ "type": "array", "items": hss_change_item_schema() }),
            ),
            (
                "relations",
                json!({ "type": "array", "items": hss_event_change_relation_schema() }),
            ),
            ("quality", hss_quality_events_schema()),
            ("truncated", boolean()),
            ("next_cursor", non_empty_string()),
        ],
        &[
            "event",
            "window",
            "dictionary",
            "changes",
            "relations",
            "quality",
            "truncated",
        ],
    )
}

fn hss_capture_event_schema() -> Value {
    schema_ref("hssEvent")
}

fn hss_capture_event_definition() -> Value {
    let host_time = || {
        closed_object(
            vec![
                ("clock", json!({ "const": "host" })),
                ("us", non_negative_integer()),
            ],
            &["clock", "us"],
        )
    };
    closed_object(
        vec![
            ("id", non_empty_string()),
            (
                "kind",
                string_enum(&[
                    "target_write",
                    "memory_write",
                    "variable_write",
                    "quality_buffer_overflow",
                    "quality_short_frame",
                    "quality_frame_format",
                    "quality_sample_interval",
                    "quality_clock_regression",
                    "recovery_stop_completed_after_failure",
                    "recovery_partial_data_retained",
                    "recovery_aborted_capture",
                ]),
            ),
            ("start", host_time()),
            ("end", host_time()),
            ("request_id", non_empty_string()),
            ("outcome", string_enum(&["succeeded", "failed"])),
            ("error_code", non_empty_string()),
            (
                "sample_relation",
                string_enum(&["before", "after", "overlaps", "indeterminate"]),
            ),
            ("mapping_uncertainty_us", non_negative_integer()),
        ],
        &["id", "kind", "start", "end", "sample_relation"],
    )
}

fn hss_event_change_relation_schema() -> Value {
    schema_ref("hssRelation")
}

fn hss_event_change_relation_definition() -> Value {
    closed_object(
        vec![
            ("event", non_empty_string()),
            ("series", non_empty_string()),
            ("after_us", non_negative_integer()),
            ("observed_by_us", non_negative_integer()),
            (
                "relation",
                string_enum(&["before", "after", "overlaps", "indeterminate"]),
            ),
            ("mapping_uncertainty_us", non_negative_integer()),
        ],
        &["event", "series", "after_us", "observed_by_us", "relation"],
    )
}

fn hss_series_dictionary_schema() -> Value {
    json!({ "type": "object", "additionalProperties": non_empty_string() })
}

fn hss_overview_variables_schema() -> Value {
    json!({
        "type": "array",
        "items": closed_object(
            vec![
                ("series", non_empty_string()),
                ("evidence", string_enum(&["dwarf", "raw_address"])),
                ("samples", non_negative_integer()),
                ("changes", non_negative_integer()),
            ],
            &["series", "evidence", "samples", "changes"],
        )
    })
}

fn hss_plan_output_schema() -> Value {
    closed_object(
        vec![
            ("duration_s", positive_integer()),
            ("rate_hz", positive_integer()),
            ("expected_samples", positive_integer()),
            ("sample_bytes", positive_integer()),
            ("record_bytes", positive_integer()),
            ("estimated_storage_bytes", positive_integer()),
            (
                "variables",
                json!({
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 10,
                    "items": hss_plan_variable_schema()
                }),
            ),
            ("reduction_suggestions", string_array()),
        ],
        &[
            "duration_s",
            "rate_hz",
            "expected_samples",
            "sample_bytes",
            "record_bytes",
            "estimated_storage_bytes",
            "variables",
            "reduction_suggestions",
        ],
    )
}

fn hss_plan_variable_schema() -> Value {
    let leaf_fields = json!({
        "type": "array",
        "minItems": 1,
        "items": closed_object(
            vec![
                ("path", non_empty_string()),
                ("byte_offset", non_negative_integer()),
                ("byte_length", positive_integer()),
                ("type", non_empty_string()),
                (
                    "encoding",
                    string_enum(&["signed", "unsigned", "boolean", "float", "other"]),
                ),
            ],
            &["path", "byte_offset", "byte_length", "type"],
        )
    });
    let common = || {
        vec![
            ("series", non_empty_string()),
            ("address", address_schema()),
            (
                "byte_length",
                bounded_integer(1, u64::from(jlink_domain::HSS_MAX_EXPANDED_SAMPLE_BYTES)),
            ),
            (
                "sample_offset",
                bounded_integer(
                    0,
                    u64::from(jlink_domain::HSS_MAX_EXPANDED_SAMPLE_BYTES - 1),
                ),
            ),
            ("leaf_fields", leaf_fields.clone()),
        ]
    };

    let mut dwarf = common();
    dwarf.push(("evidence", json!({ "const": "dwarf" })));

    let mut raw = common();
    raw.extend([
        ("evidence", json!({ "const": "raw_address" })),
        (
            "type",
            string_enum(&[
                "bytes", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32", "f64",
            ]),
        ),
        ("endianness", string_enum(&["little", "big"])),
        (
            "allowed_region",
            closed_object(
                vec![
                    ("address", non_negative_integer()),
                    ("length", positive_integer()),
                    ("kind", json!({ "const": "ram" })),
                ],
                &["address", "length", "kind"],
            ),
        ),
    ]);

    closed_schema_union(&[
        closed_object(
            dwarf,
            &[
                "evidence",
                "series",
                "address",
                "byte_length",
                "sample_offset",
                "leaf_fields",
            ],
        ),
        closed_object(
            raw,
            &[
                "evidence",
                "series",
                "address",
                "byte_length",
                "sample_offset",
                "type",
                "endianness",
                "allowed_region",
                "leaf_fields",
            ],
        ),
    ])
}

fn hss_state_schema() -> Value {
    string_enum(&[
        "starting",
        "running",
        "stopping",
        "completed",
        "failed",
        "aborted",
    ])
}

fn hss_quality_schema() -> Value {
    schema_ref("hssQuality")
}

fn hss_quality_definition() -> Value {
    closed_object(
        vec![
            (
                "integrity",
                string_enum(&["complete", "degraded", "unknown"]),
            ),
            ("requested_rate_hz", non_negative_integer()),
            ("expected_samples", non_negative_integer()),
            ("actual_samples", non_negative_integer()),
            ("actual_rate_millihz", non_negative_integer()),
            ("rate_assessment", Value::Bool(true)),
            ("intervals", hss_interval_schema()),
            ("loss", hss_assessment_schema("lost_samples")),
            ("overflow", hss_assessment_schema("events")),
            ("clock", hss_clock_schema()),
            ("usable_for_period_estimation", boolean()),
            ("usable_for_runtime_estimation", boolean()),
            ("proves_no_sample_loss", boolean()),
            (
                "reason_codes",
                json!({
                    "type": "array",
                    "items": string_enum(&[
                        "stable_intervals_available",
                        "runtime_bounds_available",
                        "insufficient_samples",
                        "incomplete_frame",
                        "clock_regression",
                        "source_timestamp_gap",
                        "confirmed_overflow",
                        "no_independent_loss_evidence",
                    ])
                }),
            ),
            ("events", hss_quality_events_schema()),
        ],
        &[
            "integrity",
            "requested_rate_hz",
            "expected_samples",
            "actual_samples",
            "intervals",
            "loss",
            "overflow",
            "clock",
            "usable_for_period_estimation",
            "usable_for_runtime_estimation",
            "proves_no_sample_loss",
            "reason_codes",
        ],
    )
}

fn hss_interval_schema() -> Value {
    closed_object(
        vec![
            ("intervals", non_negative_integer()),
            ("min_us", non_negative_integer()),
            ("max_us", non_negative_integer()),
            ("total_us", non_negative_integer()),
            ("collisions", non_negative_integer()),
            ("gap_events", non_negative_integer()),
            ("gap_slots", non_negative_integer()),
            ("regressions", non_negative_integer()),
        ],
        &[
            "intervals",
            "total_us",
            "collisions",
            "gap_events",
            "gap_slots",
            "regressions",
        ],
    )
}

fn hss_assessment_schema(count_name: &'static str) -> Value {
    closed_object(
        vec![
            ("evidence", hss_quality_evidence_schema()),
            (
                "basis",
                string_enum(&[
                    "no_independent_overflow_or_sequence_counter",
                    "dll_overflow_signal",
                    "short_or_malformed_read",
                    "source_timestamp_gap",
                    "source_timestamp_regression",
                ]),
            ),
            (count_name, non_negative_integer()),
        ],
        &["evidence", "basis"],
    )
}

fn hss_clock_schema() -> Value {
    closed_object(
        vec![
            ("source_unit", json!({ "const": "milliseconds" })),
            ("source_frequency_hz", non_negative_integer()),
            ("source_resolution_us", non_negative_integer()),
            ("normalized_unit", json!({ "const": "microseconds" })),
            ("host_monotonic_since_start", boolean()),
            (
                "mapping_method",
                json!({ "const": "capture_start_call_bound" }),
            ),
            ("mapping_error_us", non_negative_integer()),
            ("first_timestamp_us", non_negative_integer()),
            ("last_timestamp_us", non_negative_integer()),
        ],
        &[
            "source_unit",
            "source_frequency_hz",
            "source_resolution_us",
            "normalized_unit",
            "host_monotonic_since_start",
            "mapping_method",
        ],
    )
}

fn hss_quality_events_schema() -> Value {
    schema_ref("hssQualityEvents")
}

fn hss_quality_events_definition() -> Value {
    json!({
        "type": "array",
        "items": closed_object(
            vec![
                (
                    "kind",
                    string_enum(&[
                        "buffer_overflow",
                        "short_frame",
                        "frame_format",
                        "sample_interval",
                        "clock_regression",
                    ]),
                ),
                ("evidence", hss_quality_evidence_schema()),
                ("first_host_elapsed_us", non_negative_integer()),
                ("last_host_elapsed_us", non_negative_integer()),
                ("first_record", non_negative_integer()),
                ("last_record", non_negative_integer()),
                ("occurrences", non_negative_integer()),
            ],
            &[
                "kind",
                "evidence",
                "first_host_elapsed_us",
                "last_host_elapsed_us",
                "first_record",
                "last_record",
                "occurrences",
            ],
        )
    })
}

fn hss_quality_evidence_schema() -> Value {
    string_enum(&["confirmed", "suspected", "unknown"])
}

fn empty_or_error_output() -> Value {
    closed_object(vec![("error", error_schema())], &[])
}

fn config_map_schema(value_schema: &Value) -> Value {
    let keys = [
        "target.device",
        "target.interface",
        "target.speed_khz",
        "symbols.elf",
        "firmware.image",
        "jlink.dll_path",
        "jlink.dll_version",
        "jlink.dll_sha256",
        "probe.serial",
        "capture.max_bytes",
    ];
    json!({
        "type": "object",
        "propertyNames": { "enum": keys },
        "additionalProperties": value_schema
    })
}

fn error_schema() -> Value {
    closed_object(
        vec![
            ("code", non_empty_string()),
            ("message", non_empty_string()),
            ("retryable", boolean()),
            ("details", json!({ "type": "object" })),
        ],
        &["code", "message", "retryable"],
    )
}

fn validation_check_schema() -> Value {
    closed_object(
        vec![
            (
                "kind",
                string_enum(&[
                    "dll_identity",
                    "required_exports",
                    "probe_identity",
                    "target_identity",
                    "interface",
                    "background_access",
                    "hss_capability",
                ]),
            ),
            ("passed", boolean()),
            ("evidence", string_enum(&["executed", "reused"])),
            ("detail", non_empty_string()),
            ("recommendation", non_empty_string()),
        ],
        &["kind", "passed", "evidence", "detail"],
    )
}

fn hss_selector_schema() -> Value {
    closed_schema_union(&[
        closed_object(
            vec![("path", non_empty_string()), ("slice", slice_schema())],
            &["path"],
        ),
        tagged_object(
            "kind",
            "raw_address",
            vec![
                ("address", address_schema()),
                (
                    "type",
                    string_enum(&[
                        "bytes", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32", "f64",
                    ]),
                ),
                ("length", bounded_integer(1, 40)),
                ("endianness", string_enum(&["little", "big"])),
            ],
            &["address", "type", "length", "endianness"],
        ),
    ])
}

fn slice_schema() -> Value {
    closed_object(
        vec![
            ("start", non_negative_integer()),
            ("count", positive_integer()),
        ],
        &["start", "count"],
    )
}

fn threshold_rule_schema() -> Value {
    schema_ref("thresholdRule")
}

fn threshold_rule_definition() -> Value {
    tagged_union(
        "kind",
        vec![
            tagged_object(
                "kind",
                "abs_delta_gte",
                vec![
                    ("id", non_empty_string()),
                    ("path", non_empty_string()),
                    ("value", typed_value_schema()),
                ],
                &["id", "path", "value"],
            ),
            tagged_object(
                "kind",
                "outside",
                vec![
                    ("id", non_empty_string()),
                    ("path", non_empty_string()),
                    ("min", json!({ "type": "number" })),
                    ("max", json!({ "type": "number" })),
                ],
                &["id", "path", "min", "max"],
            ),
            tagged_object(
                "kind",
                "equals",
                vec![
                    ("id", non_empty_string()),
                    ("path", non_empty_string()),
                    ("value", typed_value_schema()),
                ],
                &["id", "path", "value"],
            ),
            tagged_object(
                "kind",
                "crosses",
                vec![
                    ("id", non_empty_string()),
                    ("path", non_empty_string()),
                    ("value", typed_value_schema()),
                    ("direction", string_enum(&["up", "down", "either"])),
                ],
                &["id", "path", "value", "direction"],
            ),
        ],
    )
}

fn action_union(variants: Vec<Value>) -> Value {
    let mut properties = Map::new();
    for variant in &variants {
        if let Some(variant_properties) = variant.get("properties").and_then(Value::as_object) {
            for name in variant_properties.keys() {
                properties.entry(name.clone()).or_insert_with(|| json!({}));
            }
        }
    }
    let mut schema = Map::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), json!(["action"]));
    schema.insert("additionalProperties".to_owned(), json!(false));
    schema.insert("oneOf".to_owned(), Value::Array(variants));
    Value::Object(schema)
}

fn action_object(
    action: &str,
    mut properties: Vec<(&'static str, Value)>,
    required: &[&str],
) -> Value {
    properties.push(("action", json!({ "const": action })));
    let mut required_fields = Vec::with_capacity(required.len() + 1);
    required_fields.push("action");
    required_fields.extend_from_slice(required);
    closed_object(properties, &required_fields)
}

fn tagged_union(tag: &str, variants: Vec<Value>) -> Value {
    let mut properties = Map::new();
    for variant in &variants {
        if let Some(variant_properties) = variant.get("properties").and_then(Value::as_object) {
            for name in variant_properties.keys() {
                properties.entry(name.clone()).or_insert_with(|| json!({}));
            }
        }
    }
    let mut schema = Map::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), json!([tag]));
    schema.insert("additionalProperties".to_owned(), json!(false));
    schema.insert("oneOf".to_owned(), Value::Array(variants));
    Value::Object(schema)
}

fn closed_schema_union(variants: &[Value]) -> Value {
    let mut properties = Map::new();
    for variant in variants {
        if let Some(variant_properties) = variant.get("properties").and_then(Value::as_object) {
            for name in variant_properties.keys() {
                properties.entry(name.clone()).or_insert_with(|| json!({}));
            }
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
        "oneOf": variants
    })
}

fn tagged_object(
    tag: &'static str,
    value: &str,
    mut properties: Vec<(&'static str, Value)>,
    required: &[&str],
) -> Value {
    properties.push((tag, json!({ "const": value })));
    let mut required_fields = Vec::with_capacity(required.len() + 1);
    required_fields.push(tag);
    required_fields.extend_from_slice(required);
    closed_object(properties, &required_fields)
}

fn closed_object(properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let properties: Map<String, Value> = properties
        .into_iter()
        .map(|(name, schema)| (name.to_owned(), schema))
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn non_empty_closed_object(properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let mut schema = closed_object(properties, required);
    schema
        .as_object_mut()
        .expect("closed object schema")
        .insert("minProperties".to_owned(), json!(1));
    schema
}

fn tool_definition(
    name: &str,
    description: &str,
    input_schema: Value,
    output_schema: Value,
    annotations: Value,
) -> Value {
    let mut tool = Map::new();
    tool.insert("name".to_owned(), json!(name));
    tool.insert("description".to_owned(), json!(description));
    tool.insert("inputSchema".to_owned(), input_schema);
    tool.insert("outputSchema".to_owned(), output_schema);
    tool.insert("annotations".to_owned(), annotations);
    Value::Object(tool)
}

fn annotations(read_only: bool, destructive: bool, idempotent: bool) -> Value {
    json!({
        "readOnlyHint": read_only,
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": false
    })
}

fn non_empty_string() -> Value {
    json!({ "type": "string", "minLength": 1 })
}

fn string_enum(values: &[&str]) -> Value {
    json!({ "type": "string", "enum": values })
}

fn string_array() -> Value {
    json!({ "type": "array", "items": non_empty_string() })
}

fn non_empty_unique_string_array() -> Value {
    json!({
        "type": "array",
        "minItems": 1,
        "uniqueItems": true,
        "items": non_empty_string()
    })
}

fn boolean() -> Value {
    json!({ "type": "boolean" })
}

fn positive_integer() -> Value {
    json!({ "type": "integer", "minimum": 1 })
}

fn non_negative_integer() -> Value {
    json!({ "type": "integer", "minimum": 0 })
}

fn bounded_integer(minimum: u64, maximum: u64) -> Value {
    json!({ "type": "integer", "minimum": minimum, "maximum": maximum })
}

fn address_schema() -> Value {
    json!({ "type": "string", "pattern": "^0x[0-9a-fA-F]+$" })
}

fn sha256_schema() -> Value {
    json!({ "type": "string", "pattern": "^[0-9a-fA-F]{64}$" })
}

fn byte_string_schema() -> Value {
    json!({ "type": "string", "pattern": "^(?:[0-9a-fA-F]{2}){1,4096}$" })
}

fn typed_value_schema() -> Value {
    schema_ref("typedValue")
}

fn with_typed_value_definition(mut schema: Value) -> Value {
    schema
        .as_object_mut()
        .expect("root Schema is an object")
        .insert(
            "$defs".to_owned(),
            json!({ "typedValue": typed_value_definition() }),
        );
    schema
}

fn with_hss_input_definitions(mut schema: Value) -> Value {
    schema
        .as_object_mut()
        .expect("root Schema is an object")
        .insert(
            "$defs".to_owned(),
            json!({
                "typedValue": typed_value_definition(),
                "thresholdRule": threshold_rule_definition()
            }),
        );
    schema
}

fn with_hss_output_definitions(mut schema: Value) -> Value {
    schema
        .as_object_mut()
        .expect("root Schema is an object")
        .insert(
            "$defs".to_owned(),
            json!({
                "typedValue": typed_value_definition(),
                "hssQuality": hss_quality_definition(),
                "hssQualityEvents": hss_quality_events_definition(),
                "hssEvent": hss_capture_event_definition(),
                "hssChange": hss_change_item_definition(),
                "hssRelation": hss_event_change_relation_definition()
            }),
        );
    schema
}

fn schema_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/$defs/{name}") })
}

fn typed_value_definition() -> Value {
    let recursive = || json!({ "$ref": "#/$defs/typedValue" });
    json!({
        "oneOf": [
            { "type": ["boolean", "number"] },
            {
                "type": "array",
                "items": recursive()
            },
            {
                "type": "object",
                "oneOf": [
                    {
                        "propertyNames": {
                            "not": { "enum": ["$int", "$float", "$pointer", "$union"] }
                        },
                        "additionalProperties": recursive()
                    },
                    {
                        "properties": {
                            "$int": {
                                "type": "string",
                                "pattern": "^[+-]?[0-9]+$"
                            },
                            "bits": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 64
                            },
                            "signed": { "type": "boolean" }
                        },
                        "required": ["$int", "bits", "signed"],
                        "additionalProperties": false
                    },
                    {
                        "properties": {
                            "$float": { "enum": ["nan", "inf", "-inf"] }
                        },
                        "required": ["$float"],
                        "additionalProperties": false
                    },
                    {
                        "properties": {
                            "$pointer": {
                                "type": "string",
                                "pattern": "^0x[0-9a-fA-F]+$"
                            }
                        },
                        "required": ["$pointer"],
                        "additionalProperties": false
                    },
                    {
                        "properties": {
                            "$union": {
                                "type": "object",
                                "minProperties": 1,
                                "additionalProperties": recursive()
                            }
                        },
                        "required": ["$union"],
                        "additionalProperties": false
                    }
                ]
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use jlink_domain::{ErrorCode, HssQualitySummary, JlinkError};
    use serde_json::json;

    use super::{
        hss_output_schema, hss_plan_output_schema, hss_quality_definition, hss_tool,
        public_tool_error, schema_argument_error, tool_catalog, with_hss_output_definitions,
    };

    #[test]
    fn frame_invalid_remains_a_structured_public_error() {
        let result = public_tool_error(
            JlinkError::new(ErrorCode::FrameInvalid, "HSS frame tail is invalid", false)
                .with_detail("capture_id", serde_json::json!("cap-1")),
        )
        .expect("FRAME_INVALID is part of the public error contract");

        assert_eq!(result["isError"], true);
        assert_eq!(result["content"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(
            result["content"][0]["text"],
            "FRAME_INVALID: HSS frame tail is invalid"
        );
        assert_eq!(
            result["structuredContent"]["error"],
            serde_json::json!({
                "code": "FRAME_INVALID",
                "message": "HSS frame tail is invalid",
                "retryable": false,
                "details": { "capture_id": "cap-1" }
            })
        );
    }

    #[test]
    fn p3_start_errors_remain_structured_and_distinct() {
        for (code, expected) in [
            (ErrorCode::HssUnsupported, "HSS_UNSUPPORTED"),
            (ErrorCode::HssStartFailed, "HSS_START_FAILED"),
            (ErrorCode::CaptureKeyConflict, "CAPTURE_KEY_CONFLICT"),
        ] {
            let result = public_tool_error(JlinkError::new(code, "start rejected", false))
                .expect("P3 start error is public");
            assert_eq!(result["structuredContent"]["error"]["code"], expected);
        }
    }

    #[test]
    fn legacy_quality_defaults_remain_valid_for_overview_results() {
        let mut legacy =
            serde_json::to_value(HssQualitySummary::default()).expect("quality fixture serializes");
        let object = legacy.as_object_mut().expect("quality is an object");
        object.remove("usable_for_period_estimation");
        object.remove("usable_for_runtime_estimation");
        object.remove("proves_no_sample_loss");
        object.remove("reason_codes");

        let restored: HssQualitySummary =
            serde_json::from_value(legacy).expect("V1.0 quality defaults");
        let mut current = serde_json::to_value(restored).expect("restored quality serializes");
        assert_eq!(current["reason_codes"], json!([]));
        current
            .as_object_mut()
            .expect("quality is an object")
            .insert("integrity".to_owned(), json!("unknown"));
        jsonschema::validate(
            &with_hss_output_definitions(hss_quality_definition()),
            &current,
        )
        .expect("V1.0 quality remains valid under the current output contract");
    }

    #[test]
    fn plan_output_schema_closes_raw_routing_metadata() {
        let raw_plan = json!({
            "duration_s": 2,
            "rate_hz": 100,
            "expected_samples": 200,
            "sample_bytes": 4,
            "record_bytes": 8,
            "estimated_storage_bytes": 1600,
            "variables": [{
                "evidence": "raw_address",
                "series": "raw_20000010_u32",
                "address": "0x20000010",
                "byte_length": 4,
                "sample_offset": 0,
                "type": "u32",
                "endianness": "little",
                "allowed_region": {
                    "address": 536_870_912,
                    "length": 4096,
                    "kind": "ram"
                },
                "leaf_fields": [{
                    "path": "raw_20000010_u32",
                    "byte_offset": 0,
                    "byte_length": 4,
                    "type": "u32"
                }]
            }],
            "reduction_suggestions": ["select fewer top-level fields"]
        });
        jsonschema::validate(&hss_plan_output_schema(), &raw_plan)
            .expect("raw plan output matches the closed action contract");

        let catalog = hss_output_schema();
        let plan_variant = catalog["oneOf"]
            .as_array()
            .and_then(|variants| {
                variants.iter().find(|variant| {
                    variant["required"]
                        .as_array()
                        .is_some_and(|required| required.contains(&json!("duration_s")))
                })
            })
            .expect("catalog exposes the plan variant");
        assert!(plan_variant["properties"]["variables"]["items"].is_object());
        assert_eq!(
            plan_variant["properties"],
            hss_plan_output_schema()["properties"]
        );
        let mut invalid = raw_plan;
        invalid["duration_s"] = json!("invalid");
        assert!(jsonschema::validate(&catalog, &invalid).is_err());
        assert!(jsonschema::validate(&hss_plan_output_schema(), &invalid).is_err());
    }

    #[test]
    fn parameter_error_names_field_rule_range_and_actual_value() {
        let schema = hss_tool()["inputSchema"].clone();
        let arguments = json!({
            "action": "plan",
            "duration_s": 0,
            "rate_hz": 100,
            "variables": [{
                "kind": "raw_address",
                "address": "0x20000010",
                "type": "u32",
                "length": 4,
                "endianness": "little"
            }]
        });
        let error = jsonschema::validate(&schema, &arguments).expect_err("duration is below range");
        let message = schema_argument_error("jlink_hss", &schema, &error);

        assert!(message.contains("field=$/duration_s"), "{message}");
        assert!(message.contains("rule=minimum"), "{message}");
        assert!(message.contains("allowed=>=1"), "{message}");
        assert!(message.contains("actual=0"), "{message}");
    }

    #[test]
    fn parameter_error_prefers_matching_action_and_nested_selector_branch() {
        let schema = hss_tool()["inputSchema"].clone();
        for (arguments, expected) in [
            (
                json!({
                    "action": "plan",
                    "duration_s": 1,
                    "rate_hz": 100,
                    "variables": [{
                        "kind": "raw_address",
                        "address": "0x20000010",
                        "type": "u32",
                        "endianness": "little"
                    }]
                }),
                [
                    "field=$/variables/0/length",
                    "rule=required",
                    "actual=<missing>",
                ],
            ),
            (
                json!({ "action": "bogus" }),
                ["field=$/action", "rule=enum", "actual=\"bogus\""],
            ),
            (
                json!({
                    "action": "query",
                    "capture_id": "cap-1",
                    "view": "window",
                    "series": ["motor.speed"],
                    "from_us": 0,
                    "to_us": 1_000,
                    "mode": "average"
                }),
                ["field=$/mode", "rule=enum", "actual=\"average\""],
            ),
        ] {
            let error =
                jsonschema::validate(&schema, &arguments).expect_err("fixture must be invalid");
            let message = schema_argument_error("jlink_hss", &schema, &error);
            for fragment in expected {
                assert!(message.contains(fragment), "{message}");
            }
        }
    }

    #[test]
    fn every_public_tool_description_closes_adjacent_routing_ambiguity() {
        for tool in tool_catalog() {
            let name = tool["name"].as_str().expect("tool name");
            let description = tool["description"].as_str().expect("tool description");
            for marker in ["Use for:", "Do not use for:", "Ambiguity:"] {
                assert!(
                    description.contains(marker),
                    "{name} description is missing {marker}"
                );
            }
        }
    }
}
