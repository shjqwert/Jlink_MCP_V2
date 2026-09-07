"""One-shot source edits for the approved repair branch."""
from pathlib import Path
import hashlib


def replace_once(text, before, after):
    if text.count(before) != 1:
        raise RuntimeError(f"Expected exactly one source anchor: {before[:100]!r}")
    return text.replace(before, after, 1)


def read_base(root, path, sha):
    data = (root / path).read_bytes()
    actual = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
    if actual != sha:
        raise RuntimeError(f"Unexpected baseline for {path}: {actual}")
    return data.decode("utf-8")


READINESS = r'''//! Operation-specific static readiness, without opening a DLL or target.
use std::collections::BTreeMap;

use serde::Serialize;

use super::ConfigInspection;

/// Static prerequisites and explicitly unperformed checks for one operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperationReadiness {
    /// True only when this operation has its required static configuration.
    /// This does not attest to DLL compatibility, hardware state or image identity.
    pub static_ready: bool,
    /// Missing configuration fields or unresolved blocking profile conflicts.
    pub missing: Vec<String>,
    /// Request-specific, file-content or live checks that remain unperformed.
    pub pending_checks: Vec<String>,
}

impl ConfigInspection {
    pub(crate) fn refresh_operation_readiness(&mut self) {
        let mut live = self.missing.clone();
        require(&mut live, "probe.serial", self.effective.contains_key("probe.serial"));
        let profile = self.resolved.as_ref().map(|resolved| &resolved.profile);
        let mut flash = live.clone();
        require(&mut flash, "profile.loader_ram", profile.is_some_and(|value| value.loader_ram.is_some()));
        require(&mut flash, "profile.conflicts", profile.is_none_or(|value| !value.has_blocking_conflict()));
        let mut symbols = live.clone();
        require(&mut symbols, "symbols.elf", self.effective.contains_key("symbols.elf"));
        let mut raw_hss = live.clone();
        require(&mut raw_hss, "profile.readable_ram", profile.is_some_and(|value| !value.readable_ram.is_empty()));
        let mut dwarf_plan = self.missing.clone();
        require(&mut dwarf_plan, "symbols.elf", self.effective.contains_key("symbols.elf"));
        let mut raw_plan = self.missing.clone();
        require(&mut raw_plan, "profile.readable_ram", profile.is_some_and(|value| !value.readable_ram.is_empty()));

        let live_checks = "DLL identity and live target/session checks have not been performed";
        let image_checks = "image must be supplied by request or configuration; parsing and Flash range checks remain pending";
        let symbol_checks = "ELF/DWARF parsing and selector resolution remain pending";
        let identity_checks = "strong firmware identity is required; format and target readback checks remain pending";
        let hss_checks = format!(
            "run hss.plan with the intended selectors; combined sample payload must not exceed {} bytes (timestamp excluded)",
            jlink_domain::HSS_MAX_EXPANDED_SAMPLE_BYTES
        );
        let mut readiness = BTreeMap::new();
        insert(&mut readiness, "connect", live.clone(), &[live_checks]);
        insert(&mut readiness, "validate", live.clone(), &[live_checks]);
        insert(&mut readiness, "program.flash", flash.clone(), &[live_checks, image_checks, "loader RAM safety preflight remains pending"]);
        insert(&mut readiness, "program.erase", flash, &[live_checks, "erase range and loader RAM safety preflight remain pending"]);
        insert(&mut readiness, "program.verify", live.clone(), &[live_checks, image_checks]);
        insert(&mut readiness, "inspect.memory", live, &[live_checks, "address and read length checks remain pending"]);
        insert(&mut readiness, "inspect.variable", symbols.clone(), &[live_checks, symbol_checks, "weak identity permits read-only access with a warning, not proof of matching firmware"]);
        insert(&mut readiness, "write.variable", symbols.clone(), &[live_checks, symbol_checks, identity_checks]);
        insert(&mut readiness, "hss.plan.dwarf", dwarf_plan, &[symbol_checks, identity_checks, &hss_checks]);
        insert(&mut readiness, "hss.plan.raw_address", raw_plan, &[&hss_checks, "explicit address/type/length/endianness and declared RAM checks remain pending; no DWARF identity is required"]);
        insert(&mut readiness, "hss.start.dwarf", symbols, &[live_checks, symbol_checks, identity_checks, &hss_checks, "native HSS capability and short-window rate assessment remain pending"]);
        insert(&mut readiness, "hss.start.raw_address", raw_hss, &[live_checks, &hss_checks, "device RAM and native HSS capability/rate checks remain pending; no DWARF identity is required"]);

        // Retain legacy keys, but do not call Flash statically ready without RAM.
        // Precise callers should use the action-specific readiness map.
        self.operations.insert("program".to_owned(), readiness["program.flash"].static_ready);
        self.operations.insert("hss".to_owned(),
            readiness["hss.start.dwarf"].static_ready || readiness["hss.start.raw_address"].static_ready);
        self.readiness = readiness;
    }
}

fn require(missing: &mut Vec<String>, field: &str, present: bool) {
    if !present { missing.push(field.to_owned()); }
}

fn insert(
    map: &mut BTreeMap<String, OperationReadiness>,
    action: &str,
    mut missing: Vec<String>,
    checks: &[&str],
) {
    missing.sort();
    missing.dedup();
    map.insert(action.to_owned(), OperationReadiness {
        static_ready: missing.is_empty(),
        missing,
        pending_checks: checks.iter().map(|check| (*check).to_owned()).collect(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ConfigFile, ConfigPaths, JlinkConfig, ProbeConfig, SymbolsConfig, TargetConfig, inspect_config,
    };
    use jlink_domain::{MemoryRegion, MemoryRegionKind, TargetInterface};
    use std::path::PathBuf;

    fn inspection() -> (tempfile::TempDir, ConfigInspection) {
        let root = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::new(root.path().join("project.toml"), root.path().join("user.toml"));
        let config = ConfigFile {
            target: Some(TargetConfig { device: Some("S32K144".to_owned()), interface: Some(TargetInterface::Swd), speed_khz: Some(1_000) }),
            symbols: Some(SymbolsConfig { elf: Some(PathBuf::from("not-yet-built.out")) }),
            jlink: Some(JlinkConfig { dll_path: Some(PathBuf::from("not-loaded.dll")), version: Some("6.98a".to_owned()), sha256: Some("0".repeat(64)) }),
            ..ConfigFile::default()
        };
        std::fs::write(&paths.project, toml::to_string(&config).unwrap()).unwrap();
        let discovered = ConfigFile {
            probe: Some(ProbeConfig { serial: Some(260_106_173) }),
            ..ConfigFile::default()
        };
        let result = inspect_config(&ConfigFile::default(), &paths, &discovered).unwrap();
        (root, result)
    }

    #[test]
    fn readiness_missing_loader_blocks_flash_but_not_verify_or_reads() {
        let (_root, view) = inspection();
        assert!(!view.operations["program"]);
        assert!(!view.readiness["program.flash"].static_ready);
        assert!(view.readiness["program.flash"].missing.contains(&"profile.loader_ram".to_owned()));
        assert!(!view.readiness["program.erase"].static_ready);
        assert!(view.readiness["program.verify"].static_ready);
        assert!(view.readiness["inspect.variable"].static_ready);
        assert!(!view.readiness["inspect.variable"].pending_checks.iter().any(|check| check.starts_with("strong firmware identity is required")));
        assert!(view.readiness["write.variable"].pending_checks.iter().any(|check| check.starts_with("strong firmware identity is required")));
        assert!(view.readiness["hss.plan.dwarf"].pending_checks.iter().any(|check| check.contains("40 bytes")));
    }

    #[test]
    fn readiness_raw_hss_does_not_require_a_symbol_elf_and_refreshes_profile() {
        let (_root, mut view) = inspection();
        view.effective.remove("symbols.elf");
        let profile = &mut view.resolved.as_mut().unwrap().profile;
        let ram = MemoryRegion::new(0x2000_0000, 0x1000, MemoryRegionKind::Ram).unwrap();
        profile.readable_ram.push(ram);
        profile.loader_ram = Some(ram);
        view.refresh_operation_readiness();
        assert!(view.operations["program"]);
        assert!(view.operations["hss"]);
        assert!(view.readiness["hss.start.raw_address"].static_ready);
        assert!(!view.readiness["hss.start.dwarf"].static_ready);
        assert!(!view.readiness["hss.plan.raw_address"].pending_checks.iter().any(|check| check.starts_with("strong firmware identity is required")));
        assert!(!view.readiness["program.flash"].pending_checks.is_empty());
    }
}
'''

DIAGNOSTICS = r'''//! Bounded, best-effort Worker evidence. It does not infer a crash cause.
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    process::ChildStderr,
    thread,
};

use jlink_domain::{ErrorCode, JlinkError};
use serde_json::{Value, json};
use windows_sys::Win32::{
    Foundation::{GetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

const MAX_LOG_BYTES: usize = 32 * 1024;
const OMITTED: &[u8] = b"[older worker output omitted]\n";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WorkerDiagnostics {
    pid: u32,
    path: PathBuf,
    dll_sha256: String,
}

impl WorkerDiagnostics {
    pub(super) fn new(root: &Path, pid: u32, dll_sha256: &str) -> Self {
        Self { pid, path: root.join(format!("worker-{pid}.log")), dll_sha256: dll_sha256.to_owned() }
    }

    pub(super) fn capture_stderr(&self, stderr: ChildStderr) -> Result<(), JlinkError> {
        let prepare = || -> io::Result<File> {
            fs::create_dir_all(self.path.parent().expect("diagnostic root"))?;
            File::create(&self.path)
        };
        let file = prepare().map_err(diagnostic_error)?;
        thread::Builder::new().name(format!("jlink-stderr-{}", self.pid))
            .spawn(move || drain_output(stderr, file))
            .map_err(diagnostic_error)?;
        Ok(())
    }

    pub(super) fn enrich(&self, error: JlinkError) -> JlinkError {
        error
            .with_detail("worker", process_evidence(self.pid))
            .with_detail("dll_sha256", json!(self.dll_sha256))
            .with_detail("worker_log", self.log_evidence())
    }

    fn log_evidence(&self) -> Value {
        let read = || -> io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            File::open(&self.path)?.take(u64::try_from(MAX_LOG_BYTES + 1).expect("fixed diagnostic bound")).read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        match read() {
            Ok(bytes) if bytes.len() <= MAX_LOG_BYTES => json!({
                "path": self.path.display().to_string(), "available": true,
                "max_bytes": MAX_LOG_BYTES, "tail": String::from_utf8_lossy(&bytes),
                "collector_may_lag": true,
                "interpretation": "last recorded output only; not proof of the final native call or a DLL crash"
            }),
            Ok(_) => json!({ "available": false, "reason": "diagnostic file exceeded its bound" }),
            Err(error) => json!({ "available": false, "reason": error.to_string(), "path": self.path.display().to_string() }),
        }
    }
}

fn diagnostic_error(error: io::Error) -> JlinkError {
    JlinkError::new(ErrorCode::WorkerUnavailable, format!("Cannot start bounded Worker diagnostics: {error}"), false)
}

#[derive(Default)]
struct TailBuffer {
    bytes: Vec<u8>,
    truncated: bool,
}

impl TailBuffer {
    fn append(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        let keep = MAX_LOG_BYTES - OMITTED.len();
        let remove = self.bytes.len().saturating_sub(keep);
        if remove > 0 {
            self.bytes.drain(..remove);
            self.truncated = true;
        }
    }

    fn persist(&self, file: &mut File) -> io::Result<()> {
        file.seek(SeekFrom::Start(0))?;
        let prefix = if self.truncated { OMITTED } else { &[] };
        file.write_all(prefix)?;
        file.write_all(&self.bytes)?;
        file.set_len(u64::try_from(prefix.len() + self.bytes.len()).expect("bounded diagnostic bytes"))?;
        file.flush()
    }
}

fn drain_output(mut input: impl Read, file: File) {
    let mut destination = Some(file);
    let mut tail = TailBuffer::default();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        tail.append(&buffer[..count]);
        if let Some(file) = destination.as_mut()
            && tail.persist(file).is_err()
        {
            // Keep consuming stderr even when disk writes fail. Closing the pipe
            // or ceasing to drain it must not block or kill the Worker.
            destination = None;
        }
    }
}

fn process_evidence(pid: u32) -> Value {
    // SAFETY: read-only process query/synchronization; no caller pointers are passed.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid) };
    if raw.is_null() {
        // SAFETY: queried immediately after OpenProcess failed.
        let code = unsafe { GetLastError() };
        return json!({ "pid": pid, "observed_state": "unknown", "inspection_error": code });
    }
    // SAFETY: OpenProcess returned one uniquely owned valid handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: the owned process handle is alive and has synchronization access.
    let wait = unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) };
    match wait {
        WAIT_TIMEOUT => json!({ "pid": pid, "observed_state": "running" }),
        WAIT_OBJECT_0 => {
            let mut exit_code = 0_u32;
            // SAFETY: the process handle is alive and exit_code is writable.
            if unsafe { GetExitCodeProcess(handle.as_raw_handle(), &raw mut exit_code) } == 0 {
                // SAFETY: queried immediately after GetExitCodeProcess failed.
                let code = unsafe { GetLastError() };
                json!({ "pid": pid, "observed_state": "exited", "exit_code": null, "inspection_error": code })
            } else {
                json!({ "pid": pid, "observed_state": "exited", "exit_code": exit_code })
            }
        }
        _ => json!({ "pid": pid, "observed_state": "unknown", "wait_result": wait }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_keep_a_bounded_tail_without_changing_uncertainty() {
        let directory = tempfile::tempdir().unwrap();
        let context = WorkerDiagnostics::new(directory.path(), std::process::id(), &"ab".repeat(32));
        let mut bytes = vec![b'x'; MAX_LOG_BYTES * 3];
        bytes.extend_from_slice(b"\nhss native_call=Start boundary=enter\n");
        drain_output(io::Cursor::new(bytes), File::create(&context.path).unwrap());
        let stored = fs::read(&context.path).unwrap();
        assert!(stored.len() <= MAX_LOG_BYTES);
        assert!(stored.starts_with(OMITTED));
        assert!(stored.ends_with(b"boundary=enter\n"));
        let error = context.enrich(JlinkError::new(ErrorCode::ExecutionUncertain, "lost response", false));
        assert_eq!(error.code, ErrorCode::ExecutionUncertain);
        assert!(!error.retryable);
        let details = error.details.unwrap();
        assert_eq!(details["worker"]["observed_state"], "running");
        assert_eq!(details["worker_log"]["available"], true);
    }

    #[test]
    fn diagnostics_missing_log_remains_unknown_not_an_inferred_crash() {
        let directory = tempfile::tempdir().unwrap();
        let context = WorkerDiagnostics::new(directory.path(), std::process::id(), &"ab".repeat(32));
        let error = context.enrich(JlinkError::new(ErrorCode::WorkerUnavailable, "endpoint missing", true));
        assert_eq!(error.code, ErrorCode::WorkerUnavailable);
        assert!(error.retryable);
        let details = error.details.unwrap();
        assert_eq!(details["worker_log"]["available"], false);
        assert_eq!(details["worker"]["observed_state"], "running");
    }
}
'''


def apply(root):
    root = Path(root)
    changed = []
    path = "crates/jlink-mcp/src/config.rs"
    text = read_base(root, path, "8f26a0f017e060a8b2fee5af244c32dd7a8642a4")
    text = replace_once(text, "pub use jlink_capture::DEFAULT_CAPTURE_MAX_BYTES;", """pub use jlink_capture::DEFAULT_CAPTURE_MAX_BYTES;
#[path = "config_readiness.rs"]
mod readiness;
pub use readiness::OperationReadiness;""")
    text = replace_once(text, """    pub operations: BTreeMap<String, bool>,
""", """    pub operations: BTreeMap<String, bool>,
    /// Action-specific static prerequisites; pending checks are not successful validation.
    pub readiness: BTreeMap<String, OperationReadiness>,
""")
    text = replace_once(text, """    Ok(ConfigInspection {
        effective,
        sources,
        missing,
        operations,
        dll_selection,
        resolved,
    })""", """    let mut inspection = ConfigInspection {
        effective,
        sources,
        missing,
        operations,
        readiness: BTreeMap::new(),
        dll_selection,
        resolved,
    };
    inspection.refresh_operation_readiness();
    Ok(inspection)""")
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)

    path = "crates/jlink-mcp/src/runtime.rs"
    text = read_base(root, path, "a8e7ab466ece39b13042d4157b5e4329a09e8493")
    text = replace_once(text, """        let inspection =
            inspect_config(&self.session_config, &self.config_paths, &discovery.config)?;
        let mut result = serde_json::to_value(&inspection)""", """        let mut inspection =
            inspect_config(&self.session_config, &self.config_paths, &discovery.config)?;
        if let Some(resolved) = inspection.resolved.as_mut() {
            apply_discovery_profile(resolved, &discovery);
        }
        inspection.refresh_operation_readiness();
        let mut result = serde_json::to_value(&inspection)""")
    text = replace_once(text, """        if let Some(mut resolved) = inspection.resolved {
            apply_discovery_profile(&mut resolved, &discovery);""", """        if let Some(resolved) = inspection.resolved {""")
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)

    path = "crates/jlink-mcp/src/worker_client.rs"
    text = read_base(root, path, "8bfd178e07ecd7b56dd5bb4460fcac76835f307b")
    text = replace_once(text, "const CREATE_NO_WINDOW: u32", """#[path = "worker_diagnostics.rs"]
mod diagnostics;
use diagnostics::WorkerDiagnostics;

const CREATE_NO_WINDOW: u32""")
    text = replace_once(text, """pub struct WorkerClient {
    endpoint: String,
}""", """pub struct WorkerClient {
    endpoint: String,
    diagnostics: Option<WorkerDiagnostics>,
}""")
    text = replace_once(text, """            endpoint: worker_endpoint_name(identity)?,
""", """            endpoint: worker_endpoint_name(identity)?,
            diagnostics: None,
""")
    start = text.index("    fn request<F>(")
    end = text.index("\nfn parse_hss_snapshot", start)
    block = text[start:end]
    block = block.replace("dispatched_request_error(command, &error)", "self.transport_error(dispatched_request_error(command, &error))")
    block = replace_once(block, """        let mut pipe = open_pipe(&self.endpoint, 100)?;""", """        let mut pipe = open_pipe(&self.endpoint, 100)
            .map_err(|error| self.transport_error(error))?;""")
    assert block.endswith("}\n")
    block = block[:-2] + """
    fn transport_error(&self, error: JlinkError) -> JlinkError {
        match &self.diagnostics {
            Some(diagnostics) => diagnostics.enrich(error),
            None => error,
        }
    }
}
"""
    text = text[:start] + block + text[end:]
    text = replace_once(text, """pub fn attach_or_spawn(spec: &WorkerLaunchSpec) -> Result<WorkerAttachment, JlinkError> {
    let client = WorkerClient::for_probe(&spec.probe_identity)?;""", """#[allow(clippy::too_many_lines)]
pub fn attach_or_spawn(spec: &WorkerLaunchSpec) -> Result<WorkerAttachment, JlinkError> {
    let mut client = WorkerClient::for_probe(&spec.probe_identity)?;
    let diagnostic_root = spec.lease_root.join("diagnostics")
        .join(jlink_domain::probe_identity_hash(&spec.probe_identity)?);""")
    text = replace_once(text, """            ensure_current_parent(&status)?;
            ensure_current_dll(&status, &spec.dll_sha256)?;
            return Ok(WorkerAttachment {""", """            ensure_current_parent(&status)?;
            ensure_current_dll(&status, &spec.dll_sha256)?;
            client.diagnostics = Some(WorkerDiagnostics::new(&diagnostic_root, status.worker_pid, &spec.dll_sha256));
            return Ok(WorkerAttachment {""")
    text = replace_once(text, ".stderr(Stdio::null())", ".stderr(Stdio::piped())")
    text = replace_once(text, """    let deadline = Instant::now() + ATTACH_TIMEOUT;
""", """    let diagnostics = WorkerDiagnostics::new(&diagnostic_root, child.id(), &spec.dll_sha256);
    let stderr = child.stderr.take().expect("Worker was spawned with piped stderr");
    if let Err(error) = diagnostics.capture_stderr(stderr) {
        // No target request has been sent to this newly created Worker.
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    client.diagnostics = Some(diagnostics);
    let deadline = Instant::now() + ATTACH_TIMEOUT;
""")
    text = replace_once(text, """                    ensure_current_parent(&status)?;
                    return Ok(WorkerAttachment {""", """                    ensure_current_parent(&status)?;
                    client.diagnostics = Some(WorkerDiagnostics::new(&diagnostic_root, status.worker_pid, &spec.dll_sha256));
                    return Ok(WorkerAttachment {""")
    text = replace_once(text, """            return Err(JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("jlink-worker 在建立端点前退出：{status}"),
                true,
            ));""", """            return Err(client.transport_error(JlinkError::new(
                ErrorCode::WorkerUnavailable,
                format!("jlink-worker 在建立端点前退出：{status}"),
                true,
            )));""")
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)
    for path, content in [
        ("crates/jlink-mcp/src/config_readiness.rs", READINESS),
        ("crates/jlink-mcp/src/worker_diagnostics.rs", DIAGNOSTICS),
    ]:
        if (root / path).exists():
            raise RuntimeError(f"Refusing to overwrite new module: {path}")
        (root / path).write_text(content, encoding="utf-8", newline="\n")
        changed.append(path)
    return changed
