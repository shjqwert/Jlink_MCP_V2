//! Process-owned configuration and Worker orchestration behind the MCP boundary.

use std::path::PathBuf;

use jlink_domain::{
    ConnectionState, ErrorCode, JlinkError, TargetConnectionSpec, TargetInterface, ValidationAfter,
};
use serde_json::{Map, Value, json};

use crate::{
    config::{
        CaptureConfig, ConfigFile, ConfigPaths, ConfigScope, ConfigSetState, FirmwareConfig,
        JlinkConfig, ProbeConfig, ResolvedConfig, SymbolsConfig, TargetConfig, config_set,
        resolve_config, validate_dll_identity,
    },
    mcp::{ToolCall, ToolDispatcher},
    symbols::SymbolCache,
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

    fn call_inspect(&mut self, arguments: &Value) -> ToolCall {
        match arguments
            .get("action")
            .and_then(Value::as_str)
            .expect("MCP Schema guarantees inspect.action")
        {
            "symbols" => self
                .inspect_symbols(arguments)
                .unwrap_or_else(ToolCall::Error),
            action => ToolCall::Unavailable(format!(
                "jlink_inspect.{action} 已声明 V1 合同，但将在对应 OpenSpec 阶段接通"
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
            "jlink_inspect" => self.call_inspect(arguments),
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

fn config_value_error(name: &str) -> JlinkError {
    JlinkError::new(
        ErrorCode::ConfigInvalid,
        format!("配置字段 {name} 的值无效"),
        false,
    )
}
