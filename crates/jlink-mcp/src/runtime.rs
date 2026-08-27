//! Process-owned configuration and Worker orchestration behind the MCP boundary.

use std::{fs, path::PathBuf, sync::Arc, thread, time::Duration};

use jlink_capture::{
    CaptureChangesQuery, CaptureEvent, CaptureSnapshot, CaptureStore, CaptureWindow,
    CaptureWindowMode, CaptureWindowQuery, around_event_page, capture_events, changes,
    decode_cursor, encode_cursor, event_change_relations, overview, window,
};
use jlink_domain::{
    AccessPlan, ConnectionState, ControlAfter, ControlRequest, CoreRegister, DebugRequest,
    DebugResult, ElementSlice, ErrorCode, FirmwareIdentityPlan, FirmwareImage, FlashRange,
    HssReturnWhen, HssRunSnapshot, HssRunState, HssStartPlan, HssThresholdRule, JlinkError,
    MemoryRange, ProgramAfter, ProgramRequest, TargetConnectionSpec, TargetInterface,
    ValidationAfter, VariableSelector, WriteVerify, probe_identity_hash,
};
use serde_json::{Map, Value, json};

use crate::{
    config::{
        CaptureConfig, ConfigFile, ConfigPaths, ConfigScope, ConfigSetState, FirmwareConfig,
        JlinkConfig, ProbeConfig, ResolvedConfig, SymbolsConfig, TargetConfig, config_set,
        resolve_config, validate_dll_identity,
    },
    mcp::{ToolCall, ToolDispatcher},
    symbols::{SymbolCache, SymbolIndex},
    worker_client::{WorkerAttachment, WorkerLaunchSpec, attach_or_spawn},
};

/// Single-writer process runtime for configuration and the active Worker attachment.
pub struct Runtime {
    config_paths: ConfigPaths,
    worker_executable: PathBuf,
    lease_root: PathBuf,
    attachment: Option<WorkerAttachment>,
    symbol_cache: SymbolCache,
}

struct CursorPageContext<'a> {
    snapshot: &'a CaptureSnapshot,
    arguments: &'a Value,
    ordering: &'a str,
    offset: usize,
    row_count: usize,
    truncated: bool,
    emitted_series: &'a [String],
    structured: Map<String, Value>,
}

impl Runtime {
    /// Builds the runtime from explicit local paths.
    #[must_use]
    pub const fn new(
        config_paths: ConfigPaths,
        worker_executable: PathBuf,
        lease_root: PathBuf,
    ) -> Self {
        Self {
            config_paths,
            worker_executable,
            lease_root,
            attachment: None,
            symbol_cache: SymbolCache::new(),
        }
    }

    /// Builds the immutable HSS sampling plan without calling the DLL or target.
    ///
    /// This is the production planning boundary consumed by the Worker scheduler
    /// in P3-4.3; it does not report a capture as started.
    ///
    /// # Errors
    ///
    /// Returns stable configuration, image, DWARF, selector, value, identity, or
    /// HSS capability errors when the request cannot form one fixed sampling frame.
    pub fn prepare_hss_start(&mut self, arguments: &Value) -> Result<HssStartPlan, JlinkError> {
        let (index, firmware) = self.load_symbol_planning_context("HSS")?;
        let variables = arguments
            .get("variables")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::ValueInvalid,
                    "HSS variables 必须是非空顶层选择项数组",
                    false,
                )
            })?;
        let mut plans = Vec::with_capacity(variables.len());
        for variable in variables {
            let path = variable
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    JlinkError::new(ErrorCode::ValueInvalid, "HSS 顶层变量缺少 path", false)
                })?;
            let selector = VariableSelector::new(path, element_slice(variable)?)?;
            plans.push(self.symbol_cache.access_plan(&index, &selector)?);
        }
        let capture_key = arguments
            .get("capture_key")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                JlinkError::new(ErrorCode::ValueInvalid, "HSS 缺少 capture_key", false)
            })?;
        let duration_s = required_u32(arguments, "duration_s")?;
        let rate_hz = required_u32(arguments, "rate_hz")?;
        let return_when = match arguments.get("return_when").and_then(Value::as_str) {
            Some("started") => HssReturnWhen::Started,
            Some("completed") => HssReturnWhen::Completed,
            _ => {
                return Err(JlinkError::new(
                    ErrorCode::ValueInvalid,
                    "HSS return_when 必须为 started 或 completed",
                    false,
                ));
            }
        };
        let rules = match arguments.get("rules") {
            None => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    serde_json::from_value::<HssThresholdRule>(item.clone()).map_err(|error| {
                        JlinkError::new(
                            ErrorCode::ValueInvalid,
                            format!("HSS rules[{index}] 结构无效：{error}"),
                            false,
                        )
                        .with_detail("rule_index", json!(index))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(JlinkError::new(
                    ErrorCode::ValueInvalid,
                    "HSS rules 必须是数组",
                    false,
                ));
            }
        };
        HssStartPlan::new(
            capture_key,
            duration_s,
            rate_hz,
            return_when,
            plans,
            rules,
            firmware,
        )
    }

    /// Derives the sibling Worker executable and default configuration locations.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::WorkerUnavailable`] when the current executable path
    /// has no parent directory.
    pub fn from_current_process() -> Result<Self, JlinkError> {
        let executable = std::env::current_exe().map_err(|error| {
            JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("无法定位 jlink-mcp 可执行文件：{error}"),
                false,
            )
        })?;
        let directory = executable.parent().ok_or_else(|| {
            JlinkError::new(
                ErrorCode::WorkerUnavailable,
                "jlink-mcp 可执行文件没有父目录",
                false,
            )
        })?;
        let paths = ConfigPaths::default();
        let lease_root = paths
            .user
            .parent()
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("leases");
        Ok(Self::new(
            paths,
            directory.join("jlink-worker.exe"),
            lease_root,
        ))
    }

    fn call_target(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        match arguments
            .get("action")
            .and_then(Value::as_str)
            .expect("MCP Schema guarantees target.action")
        {
            "connect" => self.connect(),
            "disconnect" => self.disconnect(),
            "status" => self.status(),
            "validate" => self.validate(arguments),
            "config_get" => self.config_get(),
            "config_set" => self.config_set(arguments),
            _ => unreachable!("target action was validated against the closed catalog"),
        }
    }

    fn connect(&mut self) -> Result<ToolCall, JlinkError> {
        let resolved = self.resolve()?;
        validate_dll_identity(&resolved.jlink)?;
        self.ensure_attachment(&resolved)?;
        let target = target_spec(&resolved)?;
        let result = self
            .attachment
            .as_ref()
            .expect("attachment was established")
            .client
            .connect(&target);
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::WorkerUnavailable)
        {
            self.attachment = None;
        }
        let status = result?;
        if status.recovery_notifications.is_empty() {
            Ok(ToolCall::success(json!({})))
        } else {
            Ok(ToolCall::success(json!({
                "notices": status.recovery_notifications
            })))
        }
    }

    fn disconnect(&mut self) -> Result<ToolCall, JlinkError> {
        if let Some(attachment) = &self.attachment {
            attachment.client.disconnect()?;
        }
        self.attachment = None;
        Ok(ToolCall::success(json!({})))
    }

    fn status(&mut self) -> Result<ToolCall, JlinkError> {
        let Some(attachment) = &self.attachment else {
            return Ok(ToolCall::success(json!({
                "connection": "disconnected"
            })));
        };
        let result = attachment.client.status();
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::WorkerUnavailable)
        {
            self.attachment = None;
        }
        let status = result?;
        let mut result = Map::new();
        result.insert(
            "connection".to_owned(),
            serde_json::to_value(status.connection_state).map_err(serialization_error)?,
        );
        if status.connection_state != ConnectionState::Disconnected {
            result.insert(
                "state".to_owned(),
                serde_json::to_value(status.target_state).map_err(serialization_error)?,
            );
        }
        Ok(ToolCall::success(Value::Object(result)))
    }

    fn validate(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let resolved = self.resolve()?;
        validate_dll_identity(&resolved.jlink)?;
        self.ensure_attachment(&resolved)?;
        let target = target_spec(&resolved)?;
        let after = arguments
            .get("after")
            .map(|value| match value.as_str() {
                Some("run") => Ok(ValidationAfter::Run),
                Some("halt") => Ok(ValidationAfter::Halt),
                _ => Err(JlinkError::new(
                    ErrorCode::ConfigInvalid,
                    "validate.after 必须是 run 或 halt",
                    false,
                )),
            })
            .transpose()?;
        let result = self
            .attachment
            .as_ref()
            .expect("attachment was established")
            .client
            .validate(&target, after);
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::WorkerUnavailable)
        {
            self.attachment = None;
        }
        let report = result?;
        Ok(ToolCall::success(
            serde_json::to_value(report).map_err(serialization_error)?,
        ))
    }

    fn config_get(&self) -> Result<ToolCall, JlinkError> {
        let resolved = self.resolve()?;
        Ok(ToolCall::success(resolved_config_result(&resolved)?))
    }

    fn config_set(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let scope = match arguments.get("scope").and_then(Value::as_str) {
            Some("project") => ConfigScope::Project,
            Some("user") => ConfigScope::User,
            _ => unreachable!("MCP Schema guarantees config_set.scope"),
        };
        let patch = config_patch(
            arguments
                .get("values")
                .and_then(Value::as_object)
                .expect("MCP Schema guarantees config_set.values"),
        )?;
        let state = self.config_set_state()?;
        config_set(&self.config_paths, scope, &patch, state)?;
        if let Some(attachment) = &self.attachment {
            attachment.client.disconnect()?;
        }
        self.attachment = None;
        Ok(ToolCall::success(json!({})))
    }

    fn resolve(&self) -> Result<ResolvedConfig, JlinkError> {
        resolve_config(
            &ConfigFile::default(),
            &self.config_paths,
            &ConfigFile::default(),
        )
    }

    fn call_inspect(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        match arguments
            .get("action")
            .and_then(Value::as_str)
            .expect("MCP Schema guarantees inspect.action")
        {
            "symbols" => self.inspect_symbols(arguments),
            "memory" => self.inspect_memory(arguments),
            "variable" => self.inspect_variable(arguments),
            "register" => self.inspect_register(arguments),
            action => Ok(ToolCall::Unavailable(format!(
                "jlink_inspect.{action} 已声明 V1 合同，但将在对应 OpenSpec 阶段接通"
            ))),
        }
    }

    fn call_write(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        match arguments
            .get("action")
            .and_then(Value::as_str)
            .expect("MCP Schema guarantees write.action")
        {
            "memory" => self.write_memory(arguments),
            "variable" => self.write_variable(arguments),
            "register" => self.write_register(arguments),
            action => Ok(ToolCall::Unavailable(format!(
                "jlink_write.{action} 已声明 V1 合同，但将在对应 OpenSpec 阶段接通"
            ))),
        }
    }

    fn call_control(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let request = match arguments
            .get("action")
            .and_then(Value::as_str)
            .expect("MCP Schema guarantees control.action")
        {
            "halt" => ControlRequest::Halt,
            "resume" => ControlRequest::Resume,
            "reset" => ControlRequest::Reset {
                after: match arguments
                    .get("after")
                    .and_then(Value::as_str)
                    .expect("MCP Schema guarantees reset.after")
                {
                    "run" => ControlAfter::Run,
                    "halt" => ControlAfter::Halt,
                    _ => unreachable!("MCP Schema guarantees reset.after"),
                },
            },
            "step" => ControlRequest::Step,
            _ => unreachable!("MCP Schema guarantees control.action"),
        };
        self.execute_control(request)?;
        Ok(ToolCall::success(json!({})))
    }

    fn call_program(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let resolved = self.resolve()?;
        validate_dll_identity(&resolved.jlink)?;
        self.ensure_attachment(&resolved)?;
        let target = target_spec(&resolved)?;
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .expect("MCP Schema guarantees program.action");
        let request = match action {
            "flash" => ProgramRequest::Flash {
                image: program_image_path(arguments, &resolved)?,
                base_address: optional_address(arguments, "base_address")?,
                verify: arguments
                    .get("verify")
                    .is_none_or(|value| value.as_bool().expect("Schema boolean")),
                after: program_after(arguments)?,
            },
            "erase" => ProgramRequest::Erase {
                range: arguments
                    .get("address")
                    .map(|value| {
                        let address = parse_address(
                            value.as_str().expect("MCP Schema guarantees erase.address"),
                            "address",
                        )?;
                        let length = arguments
                            .get("length")
                            .and_then(Value::as_u64)
                            .expect("MCP Schema pairs erase.address and length");
                        FlashRange::new(address, length)
                    })
                    .transpose()?,
                after: program_after(arguments)?,
            },
            "verify" => ProgramRequest::Verify {
                image: program_image_path(arguments, &resolved)?,
                base_address: optional_address(arguments, "base_address")?,
            },
            _ => unreachable!("program action was validated against the closed catalog"),
        };
        let result = self
            .attachment
            .as_ref()
            .expect("attachment was established")
            .client
            .program(&target, &request);
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::WorkerUnavailable)
        {
            self.attachment = None;
        }
        result?;
        Ok(ToolCall::success(json!({})))
    }

    fn call_hss(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        match arguments
            .get("action")
            .and_then(Value::as_str)
            .expect("MCP Schema guarantees hss.action")
        {
            "start" => self.start_hss(arguments),
            "status" => self.status_hss(arguments),
            "query" if arguments.get("cursor").is_some() => self.cursor_hss(arguments),
            "query" if arguments.get("view").and_then(Value::as_str) == Some("overview") => {
                self.overview_hss(arguments)
            }
            "query" if arguments.get("view").and_then(Value::as_str) == Some("changes") => {
                self.changes_hss(arguments)
            }
            "query" if arguments.get("view").and_then(Value::as_str) == Some("window") => {
                self.window_hss(arguments)
            }
            "query" if arguments.get("view").and_then(Value::as_str) == Some("around_event") => {
                self.around_event_hss(arguments)
            }
            action => Ok(ToolCall::Unavailable(format!(
                "jlink_hss.{action} 已声明 V1 合同，但将在对应 OpenSpec 阶段接通"
            ))),
        }
    }

    fn start_hss(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let plan = self.prepare_hss_start(arguments)?;
        let resolved = self.resolve()?;
        validate_dll_identity(&resolved.jlink)?;
        self.ensure_attachment(&resolved)?;
        let target = target_spec(&resolved)?;
        let client = self
            .attachment
            .as_ref()
            .expect("attachment was established")
            .client
            .clone();
        let result = client.start_hss(&target, &plan, resolved.capture.max_bytes.value);
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::WorkerUnavailable)
        {
            self.attachment = None;
        }
        let mut snapshot = result?;
        if plan.return_when() == HssReturnWhen::Completed {
            while matches!(
                snapshot.state,
                HssRunState::Starting | HssRunState::Running | HssRunState::Stopping
            ) {
                thread::sleep(Duration::from_millis(10));
                let status = client.hss_status(&snapshot.capture_id);
                if status
                    .as_ref()
                    .is_err_and(|error| error.code == ErrorCode::WorkerUnavailable)
                {
                    self.attachment = None;
                }
                snapshot = status?;
            }
        }
        if snapshot.state == HssRunState::Completed {
            let result = self.completed_overview(snapshot.capture_id.as_str())?;
            let mut result = serde_json::to_value(result)
                .map_err(serialization_error)?
                .as_object()
                .expect("CaptureOverview serializes as an object")
                .clone();
            result.insert("state".to_owned(), json!(snapshot.state));
            result.insert("elapsed_us".to_owned(), json!(snapshot.elapsed_us));
            Ok(ToolCall::with_raw_capture(
                Value::Object(result),
                snapshot.capture_id.as_str(),
            ))
        } else {
            Ok(ToolCall::success(hss_start_result(&snapshot)))
        }
    }

    fn status_hss(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let resolved = self.resolve()?;
        validate_dll_identity(&resolved.jlink)?;
        self.ensure_attachment(&resolved)?;
        let mut result = self.read_hss_status(arguments);
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::WorkerUnavailable)
        {
            self.attachment = None;
            self.ensure_attachment(&resolved)?;
            result = self.read_hss_status(arguments);
        }
        let snapshot = result?;
        let structured = hss_status_result(&snapshot)?;
        if snapshot.state == HssRunState::Completed {
            Ok(ToolCall::with_raw_capture(
                structured,
                snapshot.capture_id.as_str(),
            ))
        } else {
            Ok(ToolCall::success(structured))
        }
    }

    fn overview_hss(&self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let snapshot = self.completed_snapshot(arguments)?;
        let capture_id = snapshot.capture_id().to_owned();
        let result = overview(&snapshot)?;
        Ok(ToolCall::with_raw_capture(
            serde_json::to_value(result).map_err(serialization_error)?,
            &capture_id,
        ))
    }

    fn changes_hss(&self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let snapshot = self.completed_snapshot(arguments)?;
        Self::changes_hss_page(arguments, &snapshot, 0, &[])
    }

    fn changes_hss_page(
        arguments: &Value,
        snapshot: &CaptureSnapshot,
        offset: usize,
        emitted_series: &[String],
    ) -> Result<ToolCall, JlinkError> {
        let series = arguments.get("series").map(|value| {
            value
                .as_array()
                .expect("changes.series passed MCP Schema")
                .iter()
                .map(|item| {
                    item.as_str()
                        .expect("changes.series item passed MCP Schema")
                        .to_owned()
                })
                .collect::<Vec<_>>()
        });
        let rules = arguments
            .get("rules")
            .map(|value| {
                value
                    .as_array()
                    .expect("changes.rules passed MCP Schema")
                    .iter()
                    .cloned()
                    .map(|rule| {
                        serde_json::from_value::<HssThresholdRule>(rule).map_err(|error| {
                            JlinkError::new(
                                ErrorCode::ValueInvalid,
                                format!("changes.rules 结构无效：{error}"),
                                false,
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let limit =
            arguments
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(Ok(200_usize), |value| {
                    usize::try_from(value).map_err(|_| {
                        JlinkError::new(ErrorCode::ValueInvalid, "changes.limit 超出 usize", false)
                    })
                })?;
        let query = CaptureChangesQuery::new(
            series,
            arguments.get("from_us").and_then(Value::as_u64),
            arguments.get("to_us").and_then(Value::as_u64),
            rules,
            limit,
        )?
        .with_offset(offset);
        let result = changes(snapshot, &query)?;
        let row_count = result.changes.len().saturating_add(result.matches.len());
        let page_bounds = result
            .changes
            .iter()
            .map(|item| (item.after_us, item.observed_by_us))
            .chain(
                result
                    .matches
                    .iter()
                    .map(|item| (item.after_us, item.observed_by_us)),
            )
            .reduce(merge_sample_bounds)
            .unwrap_or_else(|| requested_sample_bounds(arguments, snapshot));
        let events = page_events(snapshot, page_bounds.0, page_bounds.1);
        let relations = events
            .iter()
            .flat_map(|event| {
                event_change_relations(
                    event,
                    &result.changes,
                    snapshot.status().quality.clock.mapping_error_us,
                )
            })
            .collect::<Vec<_>>();
        let truncated = result.truncated;
        let mut structured = serde_json::to_value(result)
            .map_err(serialization_error)?
            .as_object()
            .expect("CaptureChanges serializes as an object")
            .clone();
        structured.insert("events".to_owned(), json!(events));
        structured.insert("relations".to_owned(), json!(relations));
        Self::finish_cursor_page(CursorPageContext {
            snapshot,
            arguments,
            ordering: "changes:source-record:exact-before-rule-id-path:v1",
            offset,
            row_count,
            truncated,
            emitted_series,
            structured,
        })
    }

    fn window_hss(&self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let snapshot = self.completed_snapshot(arguments)?;
        Self::window_hss_page(arguments, &snapshot, 0, &[])
    }

    fn window_hss_page(
        arguments: &Value,
        snapshot: &CaptureSnapshot,
        offset: usize,
        emitted_series: &[String],
    ) -> Result<ToolCall, JlinkError> {
        let (query, mode) = parse_window_query(arguments, offset)?;
        let result = window(snapshot, &query)?;
        let (row_count, truncated, page_bounds) = match &result {
            CaptureWindow::Rows(rows) => (
                rows.time_us.len(),
                rows.truncated,
                rows.time_us
                    .first()
                    .zip(rows.time_us.last())
                    .map(|(first, last)| {
                        (
                            *first,
                            last.saturating_add(u64::from(
                                snapshot.status().quality.clock.source_resolution_us,
                            )),
                        )
                    }),
            ),
            CaptureWindow::Buckets(buckets) => (
                buckets.buckets.len(),
                buckets.truncated,
                buckets
                    .buckets
                    .first()
                    .zip(buckets.buckets.last())
                    .map(|(first, last)| (first.from_us, last.to_us)),
            ),
        };
        let ordering = match mode {
            CaptureWindowMode::Raw => "window:raw:source-record:v1",
            CaptureWindowMode::Transitions => "window:transitions:source-record:v1",
            CaptureWindowMode::MinMax { .. } => "window:min-max:fixed-bucket:v1",
            CaptureWindowMode::FirstLast { .. } => "window:first-last:fixed-bucket:v1",
        };
        let mut structured = serde_json::to_value(result)
            .map_err(serialization_error)?
            .as_object()
            .expect("CaptureWindow serializes as an object")
            .clone();
        let page_bounds =
            page_bounds.unwrap_or_else(|| requested_sample_bounds(arguments, snapshot));
        structured.insert(
            "quality".to_owned(),
            json!(page_quality(snapshot, page_bounds.0, page_bounds.1)),
        );
        Self::finish_cursor_page(CursorPageContext {
            snapshot,
            arguments,
            ordering,
            offset,
            row_count,
            truncated,
            emitted_series,
            structured,
        })
    }

    fn around_event_hss(&self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let snapshot = self.completed_snapshot(arguments)?;
        Self::around_event_hss_page(arguments, &snapshot, 0, &[])
    }

    fn around_event_hss_page(
        arguments: &Value,
        snapshot: &CaptureSnapshot,
        offset: usize,
        emitted_series: &[String],
    ) -> Result<ToolCall, JlinkError> {
        let limit =
            arguments
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(Ok(200_usize), |value| {
                    usize::try_from(value).map_err(|_| {
                        JlinkError::new(
                            ErrorCode::ValueInvalid,
                            "around_event.limit 超出 usize",
                            false,
                        )
                    })
                })?;
        let result = around_event_page(
            snapshot,
            arguments["event_id"]
                .as_str()
                .expect("around_event.event_id passed MCP Schema"),
            arguments["before_us"]
                .as_u64()
                .expect("around_event.before_us passed MCP Schema"),
            arguments["after_us"]
                .as_u64()
                .expect("around_event.after_us passed MCP Schema"),
            limit,
            offset,
        )?;
        let row_count = result.changes.len();
        let truncated = result.truncated;
        let structured = serde_json::to_value(result)
            .map_err(serialization_error)?
            .as_object()
            .expect("CaptureAroundEvent serializes as an object")
            .clone();
        Self::finish_cursor_page(CursorPageContext {
            snapshot,
            arguments,
            ordering: "around-event:changes:source-record:v1",
            offset,
            row_count,
            truncated,
            emitted_series,
            structured,
        })
    }

    fn cursor_hss(&self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let cursor = decode_cursor(
            arguments["cursor"]
                .as_str()
                .expect("cursor passed MCP Schema"),
        )?;
        let identity = json!({ "capture_id": cursor.capture_id() });
        let snapshot = self.completed_snapshot(&identity).map_err(|error| {
            if error.code == ErrorCode::ValueInvalid {
                JlinkError::new(
                    ErrorCode::CursorExpired,
                    "游标绑定的不可变 capture 资源已不存在",
                    false,
                )
                .with_detail("capture_id", json!(cursor.capture_id()))
            } else {
                error
            }
        })?;
        cursor.validate_snapshot(&snapshot)?;
        match (
            arguments.get("capture_id").and_then(Value::as_str),
            arguments.get("capture_key").and_then(Value::as_str),
        ) {
            (Some(capture_id), None) if capture_id == cursor.capture_id() => {}
            (None, Some(_)) => {
                let requested = self
                    .completed_snapshot(arguments)
                    .map_err(|_| cursor_invalid("游标与请求的 capture_key 不匹配"))?;
                if requested.capture_id() != cursor.capture_id() {
                    return Err(cursor_invalid("游标与请求的 capture_key 不匹配"));
                }
            }
            _ => return Err(cursor_invalid("游标与请求的 capture 身份不匹配")),
        }
        let query = cursor.query();
        if query.get("action").and_then(Value::as_str) != Some("query")
            || query.get("capture_id").and_then(Value::as_str) != Some(cursor.capture_id())
            || query.get("cursor").is_some()
        {
            return Err(cursor_invalid("游标中的查询身份无效"));
        }
        let ordering = cursor_query_ordering(query)?;
        if ordering != cursor.ordering() {
            return Err(cursor_invalid("游标中的查询排序身份不匹配"));
        }
        let offset = usize::try_from(cursor.position())
            .map_err(|_| cursor_invalid("游标位置超出平台 usize"))?;
        match query.get("view").and_then(Value::as_str) {
            Some("changes") => {
                Self::changes_hss_page(query, &snapshot, offset, cursor.emitted_series())
            }
            Some("window") => {
                Self::window_hss_page(query, &snapshot, offset, cursor.emitted_series())
            }
            Some("around_event") => {
                Self::around_event_hss_page(query, &snapshot, offset, cursor.emitted_series())
            }
            _ => Err(cursor_invalid("游标引用了不支持分页的查询视图")),
        }
    }

    fn finish_cursor_page(mut page: CursorPageContext<'_>) -> Result<ToolCall, JlinkError> {
        let mut all_series = page.emitted_series.to_vec();
        if let Some(dictionary) = page
            .structured
            .get_mut("dictionary")
            .and_then(Value::as_object_mut)
        {
            let page_series = dictionary.keys().cloned().collect::<Vec<_>>();
            dictionary.retain(|series, _| !page.emitted_series.contains(series));
            all_series.extend(page_series);
            all_series.sort();
            all_series.dedup();
        }
        if page.truncated {
            if page.row_count == 0 {
                return Err(cursor_invalid("截断页没有可推进的确定性结果"));
            }
            let next_position = page
                .offset
                .checked_add(page.row_count)
                .ok_or_else(|| cursor_invalid("游标位置溢出"))?;
            let mut normalized = page
                .arguments
                .as_object()
                .expect("validated query arguments are an object")
                .clone();
            normalized.remove("capture_key");
            normalized.remove("cursor");
            normalized.insert("capture_id".to_owned(), json!(page.snapshot.capture_id()));
            let next_cursor = encode_cursor(
                page.snapshot,
                &Value::Object(normalized),
                page.ordering,
                u64::try_from(next_position)
                    .map_err(|_| cursor_invalid("游标位置无法表示为 u64"))?,
                &all_series,
            )?;
            page.structured
                .insert("next_cursor".to_owned(), json!(next_cursor));
        }
        Ok(ToolCall::success(Value::Object(page.structured)))
    }

    fn completed_overview(
        &self,
        capture_id: &str,
    ) -> Result<jlink_capture::CaptureOverview, JlinkError> {
        let arguments = json!({ "capture_id": capture_id });
        overview(&self.completed_snapshot(&arguments)?)
    }

    fn completed_snapshot(&self, arguments: &Value) -> Result<CaptureSnapshot, JlinkError> {
        let resolved = self.resolve()?;
        let probe = resolved.probe.serial.as_ref().ok_or_else(|| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                "查询 HSS capture 前必须配置 probe.serial",
                false,
            )
        })?;
        let identity_hash = probe_identity_hash(&probe.value.to_string())?;
        let root = self.lease_root.join("captures").join(identity_hash);
        let Some(store) = CaptureStore::open_existing(root)? else {
            return Err(capture_not_found(arguments));
        };
        match (
            arguments.get("capture_id").and_then(Value::as_str),
            arguments.get("capture_key").and_then(Value::as_str),
        ) {
            (Some(capture_id), None) => store
                .find_snapshot(capture_id)?
                .ok_or_else(|| capture_not_found(arguments)),
            (None, Some(capture_key)) => store
                .find_snapshot_by_key(capture_key)?
                .ok_or_else(|| capture_not_found(arguments)),
            _ => Err(JlinkError::new(
                ErrorCode::ValueInvalid,
                "HSS 查询必须且只能提供 capture_id 或 capture_key",
                false,
            )),
        }
    }

    fn read_hss_status(&self, arguments: &Value) -> Result<HssRunSnapshot, JlinkError> {
        let client = &self
            .attachment
            .as_ref()
            .expect("attachment was established")
            .client;
        match (
            arguments.get("capture_id").and_then(Value::as_str),
            arguments.get("capture_key").and_then(Value::as_str),
        ) {
            (Some(capture_id), None) => client.hss_status(capture_id),
            (None, Some(capture_key)) => client.hss_status_by_key(capture_key),
            _ => Err(JlinkError::new(
                ErrorCode::ValueInvalid,
                "jlink_hss.status 必须且只能提供 capture_id 或 capture_key",
                false,
            )),
        }
    }

    fn inspect_symbols(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let resolved = self.resolve()?;
        let elf_path = resolved.symbols.elf.ok_or_else(|| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                "symbols.elf 未配置，无法建立 DWARF 索引",
                false,
            )
        })?;
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .expect("MCP Schema guarantees symbols.query");
        let limit =
            arguments
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(Ok(20_usize), |value| {
                    usize::try_from(value).map_err(|_| {
                        JlinkError::new(
                            ErrorCode::ValueInvalid,
                            "symbols.limit 超出平台 usize 范围",
                            false,
                        )
                    })
                })?;
        let index = self.symbol_cache.load_path(&elf_path.value)?;
        let symbols = index.search(query, limit)?;
        Ok(ToolCall::success(json!({ "symbols": symbols })))
    }

    fn inspect_memory(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let address = parse_address(
            arguments
                .get("address")
                .and_then(Value::as_str)
                .expect("MCP Schema guarantees memory.address"),
            "address",
        )?;
        let length = arguments
            .get("length")
            .and_then(Value::as_u64)
            .expect("MCP Schema guarantees memory.length");
        let request = DebugRequest::ReadMemory {
            range: MemoryRange::raw(address, length)?,
        };
        match self.execute_debug(&request)? {
            DebugResult::Memory { data } => Ok(ToolCall::success(json!({
                "data": encode_hex(&data)
            }))),
            DebugResult::Variable { .. } | DebugResult::Register { .. } | DebugResult::Written => {
                Err(debug_response_error(
                    "Worker 对内存读取返回了错误的结果类型",
                ))
            }
        }
    }

    fn inspect_variable(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let (plan, firmware) = self.variable_plan(arguments)?;
        match self.execute_debug(&DebugRequest::ReadVariable { plan, firmware })? {
            DebugResult::Variable { value } => Ok(ToolCall::success(json!({ "value": value }))),
            DebugResult::Memory { .. } | DebugResult::Register { .. } | DebugResult::Written => {
                Err(debug_response_error(
                    "Worker 对变量读取返回了错误的结果类型",
                ))
            }
        }
    }

    fn inspect_register(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let register = CoreRegister::from_canonical(
            arguments
                .get("name")
                .and_then(Value::as_str)
                .expect("MCP Schema guarantees register.name"),
        )?;
        match self.execute_debug(&DebugRequest::ReadRegister { register })? {
            DebugResult::Register { value } => Ok(ToolCall::success(json!({
                "value": format!("0x{value:08X}")
            }))),
            DebugResult::Memory { .. } | DebugResult::Variable { .. } | DebugResult::Written => {
                Err(debug_response_error(
                    "Worker 对寄存器读取返回了错误的结果类型",
                ))
            }
        }
    }

    fn write_memory(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let address = parse_address(
            arguments
                .get("address")
                .and_then(Value::as_str)
                .expect("MCP Schema guarantees memory.address"),
            "address",
        )?;
        let data = decode_hex(
            arguments
                .get("data")
                .and_then(Value::as_str)
                .expect("MCP Schema guarantees memory.data"),
        )?;
        let request = DebugRequest::WriteMemory {
            address,
            data,
            verify: write_verify(arguments)?,
        };
        expect_written(&self.execute_debug(&request)?)
    }

    fn write_variable(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let (plan, firmware) = self.variable_plan(arguments)?;
        let request = DebugRequest::WriteVariable {
            plan,
            firmware,
            value: arguments
                .get("value")
                .expect("MCP Schema guarantees variable.value")
                .clone(),
            verify: write_verify(arguments)?,
        };
        expect_written(&self.execute_debug(&request)?)
    }

    fn write_register(&mut self, arguments: &Value) -> Result<ToolCall, JlinkError> {
        let register = CoreRegister::from_canonical(
            arguments
                .get("name")
                .and_then(Value::as_str)
                .expect("MCP Schema guarantees register.name"),
        )?;
        let parsed = parse_address(
            arguments
                .get("value")
                .and_then(Value::as_str)
                .expect("MCP Schema guarantees register.value"),
            "value",
        )?;
        let value = u32::try_from(parsed).map_err(|_| {
            JlinkError::new(
                ErrorCode::ValueInvalid,
                "核心寄存器 value 必须是 32 位十六进制值",
                false,
            )
        })?;
        let request = DebugRequest::WriteRegister { register, value };
        request.validate()?;
        expect_written(&self.execute_debug(&request)?)
    }

    fn variable_plan(
        &mut self,
        arguments: &Value,
    ) -> Result<(AccessPlan, FirmwareIdentityPlan), JlinkError> {
        let (index, firmware) = self.load_symbol_planning_context("变量操作")?;
        let path = arguments
            .get("path")
            .and_then(Value::as_str)
            .expect("MCP Schema guarantees variable.path");
        let selector = VariableSelector::new(path, element_slice(arguments)?)?;
        let plan = self.symbol_cache.access_plan(&index, &selector)?;
        Ok((plan, firmware))
    }

    fn load_symbol_planning_context(
        &mut self,
        operation: &str,
    ) -> Result<(Arc<SymbolIndex>, FirmwareIdentityPlan), JlinkError> {
        let resolved = self.resolve()?;
        let elf_path = resolved.symbols.elf.ok_or_else(|| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                format!("symbols.elf 未配置，无法执行{operation}"),
                false,
            )
        })?;
        let data = fs::read(&elf_path.value).map_err(|error| {
            JlinkError::new(
                ErrorCode::ValueInvalid,
                format!("无法读取符号 ELF {}：{error}", elf_path.value.display()),
                false,
            )
        })?;
        let file_name = elf_path
            .value
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::ValueInvalid,
                    "symbols.elf 文件名必须是有效 Unicode",
                    false,
                )
            })?;
        let firmware = FirmwareImage::parse(file_name, &data, None)?.symbol_identity_plan()?;
        let index = self.symbol_cache.load_bytes(&data)?;
        Ok((index, firmware))
    }

    fn execute_debug(&mut self, request: &DebugRequest) -> Result<DebugResult, JlinkError> {
        let resolved = self.resolve()?;
        validate_dll_identity(&resolved.jlink)?;
        self.ensure_attachment(&resolved)?;
        let target = target_spec(&resolved)?;
        let result = self
            .attachment
            .as_ref()
            .expect("attachment was established")
            .client
            .debug(&target, request);
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::WorkerUnavailable)
        {
            self.attachment = None;
        }
        result
    }

    fn execute_control(&mut self, request: ControlRequest) -> Result<(), JlinkError> {
        let resolved = self.resolve()?;
        validate_dll_identity(&resolved.jlink)?;
        self.ensure_attachment(&resolved)?;
        let target = target_spec(&resolved)?;
        let result = self
            .attachment
            .as_ref()
            .expect("attachment was established")
            .client
            .control(&target, request);
        if result
            .as_ref()
            .is_err_and(|error| error.code == ErrorCode::WorkerUnavailable)
        {
            self.attachment = None;
        }
        result
    }

    fn ensure_attachment(&mut self, resolved: &ResolvedConfig) -> Result<(), JlinkError> {
        if self.attachment.is_some() {
            return Ok(());
        }
        let probe = resolved.probe.serial.as_ref().ok_or_else(|| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                "检测到的探针无法唯一确定，请配置 probe.serial",
                false,
            )
        })?;
        let launch = WorkerLaunchSpec {
            executable: self.worker_executable.clone(),
            lease_root: self.lease_root.clone(),
            probe_identity: probe.value.to_string(),
            dll_path: resolved.jlink.dll_path.value.clone(),
        };
        self.attachment = Some(attach_or_spawn(&launch)?);
        Ok(())
    }

    fn config_set_state(&self) -> Result<ConfigSetState, JlinkError> {
        let Some(attachment) = &self.attachment else {
            return Ok(ConfigSetState::default());
        };
        let status = attachment.client.status()?;
        Ok(ConfigSetState {
            connected: status.connection_state == ConnectionState::Connected,
            capture_active: status.hss_active,
        })
    }
}

impl ToolDispatcher for Runtime {
    fn call(&mut self, name: &str, arguments: &Value) -> ToolCall {
        match name {
            "jlink_target" => self.call_target(arguments).unwrap_or_else(ToolCall::Error),
            "jlink_program" => self.call_program(arguments).unwrap_or_else(ToolCall::Error),
            "jlink_inspect" => self.call_inspect(arguments).unwrap_or_else(ToolCall::Error),
            "jlink_write" => self.call_write(arguments).unwrap_or_else(ToolCall::Error),
            "jlink_control" => self.call_control(arguments).unwrap_or_else(ToolCall::Error),
            "jlink_hss" => self.call_hss(arguments).unwrap_or_else(ToolCall::Error),
            _ => ToolCall::Unavailable(format!(
                "工具 {name} 已声明 V1 合同，但其 action 将在对应 OpenSpec 阶段接通"
            )),
        }
    }

    fn read_resource(&mut self, uri: &str) -> ToolCall {
        ToolCall::Unavailable(format!(
            "资源 {uri} 的占位合同已建立，读取实现将在任务 5.5 接通"
        ))
    }
}

fn hss_start_result(snapshot: &HssRunSnapshot) -> Value {
    let mut result = serde_json::Map::from_iter([
        ("capture_id".to_owned(), json!(snapshot.capture_id)),
        (
            "state".to_owned(),
            serde_json::to_value(snapshot.state).expect("HssRunState is serializable"),
        ),
    ]);
    if snapshot.state == HssRunState::Completed {
        result.insert("elapsed_us".to_owned(), json!(snapshot.elapsed_us));
        let mut quality = serde_json::to_value(&snapshot.quality)
            .expect("HssQualitySummary is serializable")
            .as_object()
            .expect("HssQualitySummary serializes as an object")
            .clone();
        quality.insert("integrity".to_owned(), json!(snapshot.integrity));
        result.insert("quality".to_owned(), Value::Object(quality));
    } else if snapshot.state == HssRunState::Failed {
        if let Some(code) = snapshot.failure_code {
            result.insert("failure_code".to_owned(), json!(code.as_str()));
        }
        result.insert(
            "partial_available".to_owned(),
            json!(snapshot.partial_available),
        );
    } else if snapshot.state == HssRunState::Aborted {
        if let Some(reason) = &snapshot.reason {
            result.insert("reason".to_owned(), json!(reason));
        }
        if let Some(recoverable) = snapshot.recoverable {
            result.insert("recoverable".to_owned(), json!(recoverable));
        }
        result.insert(
            "partial_available".to_owned(),
            json!(snapshot.partial_available),
        );
    }
    Value::Object(result)
}

fn hss_status_result(snapshot: &HssRunSnapshot) -> Result<Value, JlinkError> {
    let mut result = hss_start_result(snapshot)
        .as_object()
        .expect("HSS result is an object")
        .clone();
    result.insert("elapsed_us".to_owned(), json!(snapshot.elapsed_us));
    result.insert(
        "complete_records".to_owned(),
        json!(snapshot.complete_records),
    );
    if snapshot.quality.requested_rate_hz > 0 || snapshot.complete_records > 0 {
        let mut quality = serde_json::to_value(&snapshot.quality)
            .map_err(serialization_error)?
            .as_object()
            .expect("HssQualitySummary serializes as an object")
            .clone();
        quality.insert("integrity".to_owned(), json!(snapshot.integrity));
        result.insert("quality".to_owned(), Value::Object(quality));
    }
    if let Some((from_us, to_us)) = persisted_range(snapshot)? {
        result.insert("from_us".to_owned(), json!(from_us));
        result.insert("to_us".to_owned(), json!(to_us));
    }
    Ok(Value::Object(result))
}

fn persisted_range(snapshot: &HssRunSnapshot) -> Result<Option<(u64, u64)>, JlinkError> {
    match (
        snapshot.quality.clock.first_timestamp_us,
        snapshot.quality.clock.last_timestamp_us,
    ) {
        (None, None) => Ok(None),
        (Some(from_us), Some(last_us)) => last_us
            .checked_add(u64::from(snapshot.quality.clock.source_resolution_us))
            .map(|to_us| Some((from_us, to_us)))
            .ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::FrameInvalid,
                    "HSS 已持久化范围结束边界溢出",
                    false,
                )
            }),
        _ => Err(JlinkError::new(
            ErrorCode::FrameInvalid,
            "HSS 已持久化范围只有一个时间边界",
            false,
        )),
    }
}

fn capture_not_found(arguments: &Value) -> JlinkError {
    let mut error = JlinkError::new(ErrorCode::ValueInvalid, "找不到已完成的 HSS capture", false);
    if let Some(capture_id) = arguments.get("capture_id") {
        error = error.with_detail("capture_id", capture_id.clone());
    }
    if let Some(capture_key) = arguments.get("capture_key") {
        error = error.with_detail("capture_key", capture_key.clone());
    }
    error
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(attachment) = &self.attachment {
            let _ = attachment.client.disconnect();
        }
    }
}

fn target_spec(config: &ResolvedConfig) -> Result<TargetConnectionSpec, JlinkError> {
    TargetConnectionSpec::new(
        config.target.device.value.clone(),
        config.target.interface.value,
        config.target.speed_khz.value,
        config.probe.serial.as_ref().map(|serial| serial.value),
        None,
    )
}

fn program_image_path(arguments: &Value, config: &ResolvedConfig) -> Result<PathBuf, JlinkError> {
    let path = arguments
        .get("image")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| {
            config
                .firmware
                .image
                .as_ref()
                .map(|field| field.value.clone())
        })
        .ok_or_else(|| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                "请求未提供 image，且 firmware.image 未配置",
                false,
            )
        })?;
    if path.is_absolute() {
        return Ok(path);
    }
    std::env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|error| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                format!("无法把固件镜像路径解析为绝对路径：{error}"),
                false,
            )
        })
}

fn program_after(arguments: &Value) -> Result<ProgramAfter, JlinkError> {
    match arguments.get("after").and_then(Value::as_str) {
        Some("none") => Ok(ProgramAfter::None),
        Some("reset_halt") => Ok(ProgramAfter::ResetHalt),
        Some("reset_run") => Ok(ProgramAfter::ResetRun),
        _ => Err(JlinkError::new(
            ErrorCode::ValueInvalid,
            "after 必须是 none、reset_halt 或 reset_run",
            false,
        )),
    }
}

fn optional_address(arguments: &Value, name: &str) -> Result<Option<u64>, JlinkError> {
    arguments
        .get(name)
        .map(|value| {
            parse_address(
                value
                    .as_str()
                    .expect("MCP Schema guarantees hexadecimal address"),
                name,
            )
        })
        .transpose()
}

fn parse_address(value: &str, name: &str) -> Result<u64, JlinkError> {
    let digits = value.strip_prefix("0x").ok_or_else(|| {
        JlinkError::new(
            ErrorCode::ValueInvalid,
            format!("{name} 必须是 0x 十六进制地址"),
            false,
        )
    })?;
    u64::from_str_radix(digits, 16).map_err(|_| {
        JlinkError::new(
            ErrorCode::ValueInvalid,
            format!("{name} 超出 u64 地址范围"),
            false,
        )
    })
}

fn element_slice(arguments: &Value) -> Result<Option<ElementSlice>, JlinkError> {
    arguments
        .get("slice")
        .map(|slice| {
            let start = slice
                .get("start")
                .and_then(Value::as_u64)
                .expect("MCP Schema guarantees slice.start");
            let count = slice
                .get("count")
                .and_then(Value::as_u64)
                .expect("MCP Schema guarantees slice.count");
            ElementSlice::new(start, count)
        })
        .transpose()
}

fn required_u32(arguments: &Value, name: &str) -> Result<u32, JlinkError> {
    let value = arguments.get(name).and_then(Value::as_u64).ok_or_else(|| {
        JlinkError::new(
            ErrorCode::ValueInvalid,
            format!("HSS {name} 必须是无符号整数"),
            false,
        )
    })?;
    u32::try_from(value).map_err(|_| {
        JlinkError::new(
            ErrorCode::ValueInvalid,
            format!("HSS {name} 超出 u32 范围"),
            false,
        )
    })
}

fn write_verify(arguments: &Value) -> Result<WriteVerify, JlinkError> {
    match arguments.get("verify").and_then(Value::as_str) {
        None | Some("none") => Ok(WriteVerify::None),
        Some("readback") => Ok(WriteVerify::Readback),
        Some(_) => Err(JlinkError::new(
            ErrorCode::ValueInvalid,
            "verify 必须是 none 或 readback",
            false,
        )),
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>, JlinkError> {
    if value.is_empty() || value.len() > 8_192 || !value.len().is_multiple_of(2) {
        return Err(JlinkError::new(
            ErrorCode::ValueInvalid,
            "memory.data 必须包含 1 到 4096 字节的偶数长度十六进制数据",
            false,
        ));
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).expect("hex pair is ASCII-compatible");
            u8::from_str_radix(text, 16).map_err(|_| {
                JlinkError::new(
                    ErrorCode::ValueInvalid,
                    "memory.data 包含非十六进制字符",
                    false,
                )
            })
        })
        .collect()
}

fn encode_hex(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(data.len() * 2);
    for byte in data {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn expect_written(result: &DebugResult) -> Result<ToolCall, JlinkError> {
    match result {
        DebugResult::Written => Ok(ToolCall::success(json!({}))),
        DebugResult::Memory { .. }
        | DebugResult::Variable { .. }
        | DebugResult::Register { .. } => Err(debug_response_error(
            "Worker 对写入请求返回了错误的结果类型",
        )),
    }
}

fn debug_response_error(message: &str) -> JlinkError {
    JlinkError::new(ErrorCode::IpcProtocolError, message, false)
}

fn config_patch(values: &Map<String, Value>) -> Result<ConfigFile, JlinkError> {
    let interface = values
        .get("target.interface")
        .map(|value| match value.as_str() {
            Some("swd") => Ok(TargetInterface::Swd),
            Some("jtag") => Ok(TargetInterface::Jtag),
            _ => Err(config_value_error("target.interface")),
        })
        .transpose()?;
    let target = if values.keys().any(|key| key.starts_with("target.")) {
        Some(TargetConfig {
            device: optional_string(values, "target.device"),
            interface,
            speed_khz: optional_u32(values, "target.speed_khz")?,
        })
    } else {
        None
    };
    let symbols = values.contains_key("symbols.elf").then(|| SymbolsConfig {
        elf: optional_path(values, "symbols.elf"),
    });
    let firmware = values
        .contains_key("firmware.image")
        .then(|| FirmwareConfig {
            image: optional_path(values, "firmware.image"),
        });
    let jlink = values
        .keys()
        .any(|key| key.starts_with("jlink."))
        .then(|| JlinkConfig {
            dll_path: optional_path(values, "jlink.dll_path"),
            version: optional_string(values, "jlink.dll_version"),
            sha256: optional_string(values, "jlink.dll_sha256"),
        });
    let probe = if values.contains_key("probe.serial") {
        Some(ProbeConfig {
            serial: optional_u32(values, "probe.serial")?,
        })
    } else {
        None
    };
    let capture = values
        .contains_key("capture.max_bytes")
        .then(|| CaptureConfig {
            max_bytes: optional_u64(values, "capture.max_bytes"),
        });
    Ok(ConfigFile {
        target,
        symbols,
        firmware,
        jlink,
        probe,
        capture,
    })
}

fn optional_string(values: &Map<String, Value>, key: &str) -> Option<String> {
    values.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn optional_path(values: &Map<String, Value>, key: &str) -> Option<PathBuf> {
    optional_string(values, key).map(PathBuf::from)
}

fn optional_u32(values: &Map<String, Value>, key: &str) -> Result<Option<u32>, JlinkError> {
    values
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| config_value_error(key))
        })
        .transpose()
}

fn optional_u64(values: &Map<String, Value>, key: &str) -> Option<u64> {
    values.get(key).and_then(Value::as_u64)
}

fn resolved_config_result(config: &ResolvedConfig) -> Result<Value, JlinkError> {
    let mut effective = Map::new();
    let mut sources = Map::new();
    insert_resolved(
        &mut effective,
        &mut sources,
        "target.device",
        &config.target.device,
    )?;
    insert_resolved(
        &mut effective,
        &mut sources,
        "target.interface",
        &config.target.interface,
    )?;
    insert_resolved(
        &mut effective,
        &mut sources,
        "target.speed_khz",
        &config.target.speed_khz,
    )?;
    if let Some(field) = &config.symbols.elf {
        insert_resolved(&mut effective, &mut sources, "symbols.elf", field)?;
    }
    if let Some(field) = &config.firmware.image {
        insert_resolved(&mut effective, &mut sources, "firmware.image", field)?;
    }
    insert_resolved(
        &mut effective,
        &mut sources,
        "jlink.dll_path",
        &config.jlink.dll_path,
    )?;
    insert_resolved(
        &mut effective,
        &mut sources,
        "jlink.dll_version",
        &config.jlink.version,
    )?;
    insert_resolved(
        &mut effective,
        &mut sources,
        "jlink.dll_sha256",
        &config.jlink.sha256,
    )?;
    if let Some(field) = &config.probe.serial {
        insert_resolved(&mut effective, &mut sources, "probe.serial", field)?;
    }
    insert_resolved(
        &mut effective,
        &mut sources,
        "capture.max_bytes",
        &config.capture.max_bytes,
    )?;
    Ok(json!({ "effective": effective, "sources": sources }))
}

fn insert_resolved<T: serde::Serialize>(
    effective: &mut Map<String, Value>,
    sources: &mut Map<String, Value>,
    name: &str,
    field: &crate::config::ResolvedField<T>,
) -> Result<(), JlinkError> {
    effective.insert(
        name.to_owned(),
        serde_json::to_value(&field.value).map_err(serialization_error)?,
    );
    sources.insert(
        name.to_owned(),
        serde_json::to_value(field.source).map_err(serialization_error)?,
    );
    Ok(())
}

fn serialization_error(error: serde_json::Error) -> JlinkError {
    let message = error.to_string();
    drop(error);
    JlinkError::new(
        ErrorCode::InvalidResponse,
        format!("无法序列化 MCP 结果：{message}"),
        false,
    )
}

fn parse_window_query(
    arguments: &Value,
    offset: usize,
) -> Result<(CaptureWindowQuery, CaptureWindowMode), JlinkError> {
    let series = arguments
        .get("series")
        .and_then(Value::as_array)
        .expect("window.series passed MCP Schema")
        .iter()
        .map(|item| {
            item.as_str()
                .expect("window.series item passed MCP Schema")
                .to_owned()
        })
        .collect();
    let mode = match arguments
        .get("mode")
        .and_then(Value::as_str)
        .expect("window.mode passed MCP Schema")
    {
        "raw" => CaptureWindowMode::Raw,
        "transitions" => CaptureWindowMode::Transitions,
        "min_max" => CaptureWindowMode::MinMax {
            points: window_points(arguments)?,
        },
        "first_last" => CaptureWindowMode::FirstLast {
            points: window_points(arguments)?,
        },
        _ => unreachable!("window.mode passed closed MCP Schema"),
    };
    let limit =
        arguments
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(Ok(1_000_usize), |value| {
                usize::try_from(value).map_err(|_| {
                    JlinkError::new(ErrorCode::ValueInvalid, "window.limit 超出 usize", false)
                })
            })?;
    let query = CaptureWindowQuery::new(
        series,
        arguments["from_us"]
            .as_u64()
            .expect("window.from_us passed MCP Schema"),
        arguments["to_us"]
            .as_u64()
            .expect("window.to_us passed MCP Schema"),
        mode,
        limit,
    )?
    .with_offset(offset);
    Ok((query, mode))
}

fn window_points(arguments: &Value) -> Result<usize, JlinkError> {
    usize::try_from(
        arguments["points"]
            .as_u64()
            .expect("window.points passed MCP Schema"),
    )
    .map_err(|_| JlinkError::new(ErrorCode::ValueInvalid, "window.points 超出 usize", false))
}

fn merge_sample_bounds(current: (u64, u64), next: (u64, u64)) -> (u64, u64) {
    (current.0.min(next.0), current.1.max(next.1))
}

fn requested_sample_bounds(arguments: &Value, snapshot: &CaptureSnapshot) -> (u64, u64) {
    let clock = &snapshot.status().quality.clock;
    let capture_from = clock.first_timestamp_us.unwrap_or(0);
    let capture_to = clock
        .last_timestamp_us
        .unwrap_or(capture_from)
        .saturating_add(u64::from(clock.source_resolution_us));
    let from = arguments
        .get("from_us")
        .and_then(Value::as_u64)
        .unwrap_or(capture_from)
        .max(capture_from);
    let to = arguments
        .get("to_us")
        .and_then(Value::as_u64)
        .unwrap_or(capture_to)
        .min(capture_to);
    (from, to.max(from))
}

fn page_events(snapshot: &CaptureSnapshot, from_us: u64, to_us: u64) -> Vec<CaptureEvent> {
    let uncertainty = snapshot.status().quality.clock.mapping_error_us;
    capture_events(snapshot)
        .into_iter()
        .filter(|event| {
            uncertainty.is_none_or(|uncertainty| {
                event.end.us.saturating_add(uncertainty) >= from_us
                    && event.start.us.saturating_sub(uncertainty) < to_us
            })
        })
        .collect()
}

fn page_quality(
    snapshot: &CaptureSnapshot,
    from_us: u64,
    to_us: u64,
) -> Vec<jlink_domain::HssQualityEvent> {
    let uncertainty = snapshot.status().quality.clock.mapping_error_us;
    snapshot
        .status()
        .quality
        .events
        .iter()
        .filter(|event| {
            uncertainty.is_none_or(|uncertainty| {
                event.last_host_elapsed_us.saturating_add(uncertainty) >= from_us
                    && event.first_host_elapsed_us.saturating_sub(uncertainty) < to_us
            })
        })
        .cloned()
        .collect()
}

fn cursor_query_ordering(arguments: &Value) -> Result<&'static str, JlinkError> {
    match arguments.get("view").and_then(Value::as_str) {
        Some("changes") => Ok("changes:source-record:exact-before-rule-id-path:v1"),
        Some("window") => match arguments.get("mode").and_then(Value::as_str) {
            Some("raw") => Ok("window:raw:source-record:v1"),
            Some("transitions") => Ok("window:transitions:source-record:v1"),
            Some("min_max") => Ok("window:min-max:fixed-bucket:v1"),
            Some("first_last") => Ok("window:first-last:fixed-bucket:v1"),
            _ => Err(cursor_invalid("游标中的 window.mode 无效")),
        },
        Some("around_event") => Ok("around-event:changes:source-record:v1"),
        _ => Err(cursor_invalid("游标中的分页视图无效")),
    }
}

fn cursor_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::CursorInvalid, message, false)
}

fn config_value_error(name: &str) -> JlinkError {
    JlinkError::new(
        ErrorCode::ConfigInvalid,
        format!("配置字段 {name} 的值无效"),
        false,
    )
}

#[cfg(test)]
mod hss_state_tests {
    use jlink_domain::{ErrorCode, HssDataIntegrity, HssDrainTiming, HssRunSnapshot, HssRunState};

    use super::{hss_start_result, hss_status_result};

    fn snapshot(state: HssRunState, integrity: HssDataIntegrity) -> HssRunSnapshot {
        HssRunSnapshot {
            capture_id: "cap-state".to_owned(),
            state,
            integrity,
            elapsed_us: 10,
            complete_records: 1,
            drain: HssDrainTiming::default(),
            quality: jlink_domain::HssQualitySummary::default(),
            writes: Vec::new(),
            failure_code: None,
            partial_available: false,
            reason: None,
            recoverable: None,
            recovery_notifications: Vec::new(),
        }
    }

    #[test]
    fn failed_start_result_returns_terminal_facts_without_tool_error() {
        let mut failed = snapshot(HssRunState::Failed, HssDataIntegrity::Unknown);
        failed.failure_code = Some(ErrorCode::FrameInvalid);
        failed.partial_available = true;
        let result = hss_start_result(&failed);
        assert_eq!(result["state"], "failed");
        assert_eq!(result["failure_code"], "FRAME_INVALID");
        assert_eq!(result["partial_available"], true);
    }

    #[test]
    fn completed_degraded_start_result_exposes_integrity_quality() {
        let result = hss_start_result(&snapshot(
            HssRunState::Completed,
            HssDataIntegrity::Degraded,
        ));
        assert_eq!(result["state"], "completed");
        assert_eq!(result["quality"]["integrity"], "degraded");
        assert_eq!(result["quality"]["loss"]["evidence"], "unknown");
        assert!(result["quality"]["loss"].get("lost_samples").is_none());
        assert_eq!(result["quality"]["clock"]["source_resolution_us"], 1_000);
    }

    #[test]
    fn recovered_running_status_exposes_same_identity_and_elapsed_time() {
        let mut running = snapshot(HssRunState::Running, HssDataIntegrity::Unknown);
        running.quality.requested_rate_hz = 1_000;
        running.quality.expected_samples = 1_000;
        running.quality.actual_samples = 1;
        running.quality.clock.first_timestamp_us = Some(10_000);
        running.quality.clock.last_timestamp_us = Some(10_000);
        let result = hss_status_result(&running).expect("status result");
        assert_eq!(result["capture_id"], "cap-state");
        assert_eq!(result["state"], "running");
        assert_eq!(result["elapsed_us"], 10);
        assert_eq!(result["complete_records"], 1);
        assert_eq!(result["from_us"], 10_000);
        assert_eq!(result["to_us"], 11_000);
        assert_eq!(result["quality"]["integrity"], "unknown");
    }
}
