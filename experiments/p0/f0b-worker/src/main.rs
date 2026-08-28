use std::env;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use crc32fast::Hasher as Crc32;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const PIPE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const BLOCK_MAGIC: &[u8; 4] = b"F0B1";
const BLOCK_HEADER_BYTES: usize = 12;
const GENERIC_READ_WRITE: u32 = 0xC000_0000;

#[derive(Debug)]
struct Options {
    values: std::collections::BTreeMap<String, String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut values = std::collections::BTreeMap::new();
        let mut index = 0;
        while index < args.len() {
            let name = args[index]
                .strip_prefix("--")
                .with_context(|| format!("expected option, found {}", args[index]))?;
            let value = args
                .get(index + 1)
                .with_context(|| format!("missing value for --{name}"))?;
            ensure!(!value.starts_with("--"), "missing value for --{name}");
            ensure!(
                values.insert(name.to_owned(), value.to_owned()).is_none(),
                "duplicate option --{name}"
            );
            index += 2;
        }
        Ok(Self { values })
    }

    fn required(&self, name: &str) -> Result<&str> {
        self.values
            .get(name)
            .map(String::as_str)
            .with_context(|| format!("missing --{name}"))
    }

    fn u64(&self, name: &str, fallback: Option<u64>) -> Result<u64> {
        match self.values.get(name) {
            Some(value) => value.parse().with_context(|| format!("invalid --{name}")),
            None => fallback.with_context(|| format!("missing --{name}")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureRecord {
    capture_key: String,
    capture_id: String,
    duration_ms: u64,
    worker_pid: u32,
    status: String,
    samples: u64,
    partial_path: PathBuf,
    completed_path: PathBuf,
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryRecord {
    path: PathBuf,
    before_bytes: u64,
    after_bytes: u64,
    valid_blocks: u64,
    truncated_tail: bool,
    crc_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SuiteSummary {
    status: &'static str,
    command: &'static str,
    pipe_name: String,
    probe_id: String,
    parent_pid: u32,
    worker_pid: u32,
    parent_exited_before_capture_completed: bool,
    worker_survived_parent_exit: bool,
    named_pipe_reattached: bool,
    capture_key: String,
    capture_id: String,
    same_capture_id_after_reattach: bool,
    equivalent_request_idempotent: bool,
    non_equivalent_key_reuse_rejected: bool,
    second_worker_lease_rejected: bool,
    recovery: RecoveryRecord,
    completed_capture: CaptureRecord,
    completed_file_sha256: String,
    completed_file_valid_blocks: u64,
    completed_file_crc_error: bool,
    lease_released_after_shutdown: bool,
}

#[derive(Debug)]
struct WorkerState {
    probe_id: String,
    capture: Option<CaptureRecord>,
    recoveries: Vec<RecoveryRecord>,
}

#[derive(Debug)]
struct ProbeLease {
    path: PathBuf,
    _file: File,
}

impl ProbeLease {
    fn acquire(state_dir: &Path, probe_id: &str) -> Result<Self> {
        ensure!(
            probe_id
                .chars()
                .all(|candidate| candidate.is_ascii_alphanumeric()),
            "probe id must be alphanumeric"
        );
        let path = state_dir.join(format!("probe-{probe_id}.lease"));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("PROBE_BUSY: lease already held for {probe_id}"))?;
        writeln!(file, "pid={}", std::process::id())?;
        file.sync_all()?;
        Ok(Self { path, _file: file })
    }
}

impl Drop for ProbeLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let summary_path = option_from_args(&args, "summary").map(PathBuf::from);
    let (value, exit_code) = match run(&args) {
        Ok(Some(value)) => (Some(value), 0),
        Ok(None) => (None, 0),
        Err(error) => {
            eprintln!("{error:#}");
            (
                Some(json!({
                    "status": "error",
                    "error": format!("{error:#}")
                })),
                1,
            )
        }
    };
    if let Some(value) = value {
        let rendered = serde_json::to_string_pretty(&value).unwrap();
        if let Some(path) = summary_path
            && let Err(error) = write_new_file(&path, rendered.as_bytes())
        {
            eprintln!("failed to write summary {}: {error:#}", path.display());
            std::process::exit(1);
        }
        println!("{rendered}");
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

fn run(args: &[String]) -> Result<Option<Value>> {
    let command = args.get(1).context("missing command")?;
    let options = Options::parse(&args[2..])?;
    match command.as_str() {
        "worker" => {
            run_worker(
                Path::new(options.required("state-dir")?),
                options.required("pipe")?,
                options.required("probe")?,
            )?;
            Ok(None)
        }
        "parent" => {
            run_parent(&options)?;
            std::process::exit(0);
        }
        "run-suite" => Ok(Some(serde_json::to_value(run_suite(&options)?)?)),
        _ => bail!("unknown command {command}"),
    }
}

fn run_worker(state_dir: &Path, pipe_name: &str, probe_id: &str) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    let _lease = ProbeLease::acquire(state_dir, probe_id)?;
    let recoveries = recover_partials(state_dir)?;
    let state = Arc::new(Mutex::new(WorkerState {
        probe_id: probe_id.to_owned(),
        capture: None,
        recoveries,
    }));

    let mut keep_running = true;
    while keep_running {
        let mut pipe = accept_pipe(pipe_name)?;
        let request = read_frame(&mut pipe)?;
        let (reply, continue_running) = handle_request(&state, state_dir, &request)?;
        write_frame(&mut pipe, &reply)?;
        pipe.flush()?;
        keep_running = continue_running;
    }
    Ok(())
}

fn handle_request(
    shared: &Arc<Mutex<WorkerState>>,
    state_dir: &Path,
    request: &Value,
) -> Result<(Value, bool)> {
    let action = request
        .get("action")
        .and_then(Value::as_str)
        .context("request action is required")?;
    match action {
        "ping" => {
            let state = shared.lock().unwrap();
            Ok((
                json!({"status": "ok", "workerPid": std::process::id(), "probeId": state.probe_id}),
                true,
            ))
        }
        "recovery" => {
            let state = shared.lock().unwrap();
            Ok((
                json!({"status": "ok", "recoveries": state.recoveries}),
                true,
            ))
        }
        "status" => {
            let state = shared.lock().unwrap();
            Ok((json!({"status": "ok", "capture": state.capture}), true))
        }
        "start" => {
            let capture_key = request
                .get("captureKey")
                .and_then(Value::as_str)
                .context("captureKey is required")?;
            let duration_ms = request
                .get("durationMs")
                .and_then(Value::as_u64)
                .context("durationMs is required")?;
            ensure!(
                (100..=300_000).contains(&duration_ms),
                "durationMs must be 100..300000"
            );

            let mut state = shared.lock().unwrap();
            if let Some(existing) = &state.capture {
                if existing.capture_key == capture_key && existing.duration_ms == duration_ms {
                    return Ok((
                        json!({"status": "ok", "idempotent": true, "capture": existing}),
                        true,
                    ));
                }
                if existing.capture_key == capture_key {
                    return Ok((
                        json!({"status": "conflict", "code": "CAPTURE_KEY_CONFLICT"}),
                        true,
                    ));
                }
                return Ok((json!({"status": "busy", "code": "CAPTURE_ACTIVE"}), true));
            }

            let capture_id = capture_id(&state.probe_id, capture_key, duration_ms);
            let partial_path = state_dir.join(format!("capture-{capture_id}.partial"));
            let completed_path = state_dir.join(format!("capture-{capture_id}.done"));
            let initial = CaptureRecord {
                capture_key: capture_key.to_owned(),
                capture_id,
                duration_ms,
                worker_pid: std::process::id(),
                status: "running".to_owned(),
                samples: 0,
                partial_path,
                completed_path,
                error: None,
            };
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&initial.partial_path)?
                .sync_all()?;
            state.capture = Some(initial.clone());
            drop(state);
            spawn_capture(Arc::clone(shared), initial.clone());
            Ok((
                json!({"status": "ok", "idempotent": false, "capture": initial}),
                true,
            ))
        }
        "shutdown" => Ok((json!({"status": "ok"}), false)),
        _ => bail!("unknown action {action}"),
    }
}

fn spawn_capture(shared: Arc<Mutex<WorkerState>>, initial: CaptureRecord) {
    thread::spawn(move || {
        let result = run_capture(&shared, &initial);
        if let Err(error) = result {
            let mut state = shared.lock().unwrap();
            if let Some(capture) = state.capture.as_mut() {
                capture.status = "failed".to_owned();
                capture.error = Some(format!("{error:#}"));
            }
        }
    });
}

fn run_capture(shared: &Arc<Mutex<WorkerState>>, initial: &CaptureRecord) -> Result<()> {
    let started = Instant::now();
    let mut sequence = 0_u64;
    while started.elapsed() < Duration::from_millis(initial.duration_ms) {
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&sequence.to_le_bytes());
        payload.extend_from_slice(&unix_ms()?.to_le_bytes());
        append_block(&initial.partial_path, &payload)?;
        sequence += 1;
        {
            let mut state = shared.lock().unwrap();
            if let Some(capture) = state.capture.as_mut() {
                capture.samples = sequence;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    fs::rename(&initial.partial_path, &initial.completed_path)?;
    let mut state = shared.lock().unwrap();
    if let Some(capture) = state.capture.as_mut() {
        capture.status = "completed".to_owned();
        capture.samples = sequence;
    }
    Ok(())
}

fn run_parent(options: &Options) -> Result<Value> {
    let current_exe = env::current_exe()?;
    let state_dir = PathBuf::from(options.required("state-dir")?);
    let pipe_name = options.required("pipe")?;
    let probe_id = options.required("probe")?;
    let capture_key = options.required("capture-key")?;
    let duration_ms = options.u64("duration-ms", None)?;
    let receipt_path = PathBuf::from(options.required("receipt")?);

    let child = Command::new(current_exe)
        .arg("worker")
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--pipe")
        .arg(pipe_name)
        .arg("--probe")
        .arg(probe_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let worker_pid = child.id();
    drop(child);

    let ping = send_request(pipe_name, &json!({"action": "ping"}))?;
    ensure!(ping["status"] == "ok", "worker ping failed");
    let start = send_request(
        pipe_name,
        &json!({"action": "start", "captureKey": capture_key, "durationMs": duration_ms}),
    )?;
    ensure!(start["status"] == "ok", "worker start failed: {start}");
    let receipt = json!({
        "parentPid": std::process::id(),
        "workerPid": worker_pid,
        "capture": start["capture"],
    });
    write_new_file(
        &receipt_path,
        serde_json::to_vec_pretty(&receipt)?.as_slice(),
    )?;
    Ok(receipt)
}

fn run_suite(options: &Options) -> Result<SuiteSummary> {
    let evidence_dir = PathBuf::from(options.required("evidence-dir")?);
    let duration_ms = options.u64("duration-ms", Some(3_000))?;
    let probe_id = "260106173";
    let capture_key = "f0b-parent-exit-capture";
    fs::create_dir_all(&evidence_dir)?;
    let state_dir = evidence_dir.join("state-v3");
    fs::create_dir(&state_dir).context("F0-B state directory already exists")?;
    let orphan_path = state_dir.join("orphan.partial");
    create_orphan_partial(&orphan_path)?;

    let pipe_name = format!(
        r"\\.\pipe\jlink-mcp-v2-f0b-{}-{}",
        std::process::id(),
        unix_ms()?
    );
    let receipt_path = evidence_dir.join("parent-receipt-v3.json");
    let parent_status = Command::new(env::current_exe()?)
        .arg("parent")
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--pipe")
        .arg(&pipe_name)
        .arg("--probe")
        .arg(probe_id)
        .arg("--capture-key")
        .arg(capture_key)
        .arg("--duration-ms")
        .arg(duration_ms.to_string())
        .arg("--receipt")
        .arg(&receipt_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    ensure!(parent_status.success(), "parent process failed");
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    let parent_pid = u32::try_from(
        receipt["parentPid"]
            .as_u64()
            .context("missing parent pid")?,
    )?;
    let worker_pid = u32::try_from(
        receipt["workerPid"]
            .as_u64()
            .context("missing worker pid")?,
    )?;
    let capture_id = receipt["capture"]["captureId"]
        .as_str()
        .context("missing capture id")?
        .to_owned();

    thread::sleep(Duration::from_millis(200));
    let status_after_parent = send_request(&pipe_name, &json!({"action": "status"}))?;
    let capture_running = status_after_parent["capture"]["status"] == "running";
    let worker_survived_parent_exit =
        status_after_parent["capture"]["workerPid"].as_u64() == Some(u64::from(worker_pid));
    ensure!(
        capture_running,
        "capture completed before parent-exit observation"
    );
    ensure!(
        worker_survived_parent_exit,
        "worker did not survive parent exit"
    );

    let repeated = send_request(
        &pipe_name,
        &json!({"action": "start", "captureKey": capture_key, "durationMs": duration_ms}),
    )?;
    let repeated_id = repeated["capture"]["captureId"]
        .as_str()
        .context("missing repeated capture id")?;
    ensure!(
        repeated["idempotent"] == true,
        "equivalent request was not idempotent"
    );
    ensure!(
        repeated_id == capture_id,
        "reattached request changed capture id"
    );
    let conflict = send_request(
        &pipe_name,
        &json!({"action": "start", "captureKey": capture_key, "durationMs": duration_ms + 1}),
    )?;
    ensure!(
        conflict["code"] == "CAPTURE_KEY_CONFLICT",
        "non-equivalent capture_key reuse was not rejected"
    );

    let second_worker = Command::new(env::current_exe()?)
        .arg("worker")
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--pipe")
        .arg(format!("{pipe_name}-second"))
        .arg("--probe")
        .arg(probe_id)
        .output()?;
    let lease_error = String::from_utf8_lossy(&second_worker.stderr);
    ensure!(
        !second_worker.status.success() && lease_error.contains("PROBE_BUSY"),
        "second worker was not rejected by the probe lease"
    );

    let recovery_reply = send_request(&pipe_name, &json!({"action": "recovery"}))?;
    let recoveries: Vec<RecoveryRecord> =
        serde_json::from_value(recovery_reply["recoveries"].clone())?;
    let recovery = recoveries
        .into_iter()
        .find(|candidate| candidate.path == orphan_path)
        .context("orphan recovery evidence missing")?;
    ensure!(
        recovery.valid_blocks == 3
            && recovery.truncated_tail
            && !recovery.crc_error
            && recovery.after_bytes < recovery.before_bytes,
        "partial recovery assertions failed: {recovery:?}"
    );

    let wait_started = Instant::now();
    let completed_capture = loop {
        let reply = send_request(&pipe_name, &json!({"action": "status"}))?;
        let capture: CaptureRecord = serde_json::from_value(reply["capture"].clone())?;
        if capture.status == "completed" {
            break capture;
        }
        ensure!(
            capture.status != "failed",
            "capture worker failed: {capture:?}"
        );
        ensure!(
            wait_started.elapsed() < Duration::from_millis(duration_ms + 5_000),
            "capture did not complete before timeout"
        );
        thread::sleep(Duration::from_millis(100));
    };
    let completed_scan = scan_blocks(&completed_capture.completed_path, false)?;
    ensure!(
        !completed_scan.truncated_tail && !completed_scan.crc_error,
        "completed capture invalid"
    );

    let shutdown = send_request(&pipe_name, &json!({"action": "shutdown"}))?;
    ensure!(shutdown["status"] == "ok", "worker shutdown failed");
    let lease_path = state_dir.join(format!("probe-{probe_id}.lease"));
    let shutdown_started = Instant::now();
    while lease_path.exists() && shutdown_started.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(20));
    }
    let lease_released_after_shutdown = !lease_path.exists();
    ensure!(
        lease_released_after_shutdown,
        "probe lease was not released"
    );

    Ok(SuiteSummary {
        status: "ok",
        command: "run-suite",
        pipe_name,
        probe_id: probe_id.to_owned(),
        parent_pid,
        worker_pid,
        parent_exited_before_capture_completed: capture_running,
        worker_survived_parent_exit,
        named_pipe_reattached: status_after_parent["status"] == "ok",
        capture_key: capture_key.to_owned(),
        capture_id: capture_id.clone(),
        same_capture_id_after_reattach: repeated_id == capture_id,
        equivalent_request_idempotent: repeated["idempotent"] == true,
        non_equivalent_key_reuse_rejected: conflict["code"] == "CAPTURE_KEY_CONFLICT",
        second_worker_lease_rejected: !second_worker.status.success()
            && lease_error.contains("PROBE_BUSY"),
        recovery,
        completed_file_sha256: sha256_file(&completed_capture.completed_path)?,
        completed_file_valid_blocks: completed_scan.valid_blocks,
        completed_file_crc_error: completed_scan.crc_error,
        completed_capture,
        lease_released_after_shutdown,
    })
}

fn option_from_args<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == format!("--{name}"))
        .map(|pair| pair[1].as_str())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn accept_pipe(pipe_name: &str) -> Result<File> {
    let wide_name = to_wide(pipe_name);
    let handle = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            std::ptr::null(),
        )
    };
    ensure!(
        handle != INVALID_HANDLE_VALUE,
        "CreateNamedPipeW failed: {}",
        unsafe { GetLastError() }
    );
    let connected = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
    if connected == 0 {
        let error = unsafe { GetLastError() };
        ensure!(
            error == ERROR_PIPE_CONNECTED,
            "ConnectNamedPipe failed: {error}"
        );
    }
    Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
}

fn send_request(pipe_name: &str, request: &Value) -> Result<Value> {
    let started = Instant::now();
    let mut pipe = loop {
        let wide_name = to_wide(pipe_name);
        let handle = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                GENERIC_READ_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            break unsafe { File::from_raw_handle(handle as RawHandle) };
        }
        ensure!(
            started.elapsed() < PIPE_CONNECT_TIMEOUT,
            "named pipe did not become available: {}",
            unsafe { GetLastError() }
        );
        thread::sleep(Duration::from_millis(20));
    };
    write_frame(&mut pipe, request)?;
    pipe.flush()?;
    read_frame(&mut pipe)
}

fn write_frame(stream: &mut File, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_FRAME_BYTES, "frame too large");
    stream.write_all(&u32::try_from(bytes.len())?.to_le_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

fn read_frame(stream: &mut File) -> Result<Value> {
    let mut length_bytes = [0_u8; 4];
    stream.read_exact(&mut length_bytes)?;
    let length = usize::try_from(u32::from_le_bytes(length_bytes))?;
    ensure!(length <= MAX_FRAME_BYTES, "frame too large");
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn capture_id(probe_id: &str, capture_key: &str, duration_ms: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(probe_id.as_bytes());
    hasher.update([0]);
    hasher.update(capture_key.as_bytes());
    hasher.update([0]);
    hasher.update(duration_ms.to_le_bytes());
    format!("{:x}", hasher.finalize())[..24].to_owned()
}

fn append_block(path: &Path, payload: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().append(true).open(path)?;
    file.write_all(BLOCK_MAGIC)?;
    file.write_all(&u32::try_from(payload.len())?.to_le_bytes())?;
    file.write_all(&crc32(payload).to_le_bytes())?;
    file.write_all(payload)?;
    file.flush()?;
    Ok(())
}

fn create_orphan_partial(path: &Path) -> Result<()> {
    OpenOptions::new().create_new(true).write(true).open(path)?;
    for sequence in 0_u64..3 {
        append_block(path, &sequence.to_le_bytes())?;
    }
    let payload = 3_u64.to_le_bytes();
    let mut file = OpenOptions::new().append(true).open(path)?;
    file.write_all(BLOCK_MAGIC)?;
    file.write_all(&u32::try_from(payload.len())?.to_le_bytes())?;
    file.write_all(&crc32(&payload).to_le_bytes())?;
    file.write_all(&payload[..3])?;
    file.sync_all()?;
    Ok(())
}

fn recover_partials(state_dir: &Path) -> Result<Vec<RecoveryRecord>> {
    let mut paths = fs::read_dir(state_dir)?
        .filter_map(|entry| entry.ok().map(|value| value.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("partial"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.iter().map(|path| scan_blocks(path, true)).collect()
}

fn scan_blocks(path: &Path, repair: bool) -> Result<RecoveryRecord> {
    let bytes = fs::read(path)?;
    let before_bytes = u64::try_from(bytes.len())?;
    let mut offset = 0_usize;
    let mut valid_blocks = 0_u64;
    let mut truncated_tail = false;
    let mut crc_error = false;
    while offset < bytes.len() {
        if bytes.len() - offset < BLOCK_HEADER_BYTES {
            truncated_tail = true;
            break;
        }
        if &bytes[offset..offset + 4] != BLOCK_MAGIC {
            crc_error = true;
            break;
        }
        let payload_len = usize::try_from(u32::from_le_bytes(
            bytes[offset + 4..offset + 8].try_into()?,
        ))?;
        let expected_crc = u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into()?);
        let payload_start = offset + BLOCK_HEADER_BYTES;
        let Some(payload_end) = payload_start.checked_add(payload_len) else {
            truncated_tail = true;
            break;
        };
        if payload_end > bytes.len() {
            truncated_tail = true;
            break;
        }
        if crc32(&bytes[payload_start..payload_end]) != expected_crc {
            crc_error = true;
            break;
        }
        valid_blocks += 1;
        offset = payload_end;
    }
    if repair && (truncated_tail || crc_error) {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(u64::try_from(offset)?)?;
        file.sync_all()?;
    }
    let after_bytes = if repair {
        fs::metadata(path)?.len()
    } else {
        before_bytes
    };
    Ok(RecoveryRecord {
        path: path.to_path_buf(),
        before_bytes,
        after_bytes,
        valid_blocks,
        truncated_tail,
        crc_error,
    })
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = Crc32::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn unix_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::capture_id;

    #[test]
    fn capture_identity_is_stable_and_request_sensitive() {
        let first = capture_id("260106173", "key", 3_000);
        assert_eq!(first, capture_id("260106173", "key", 3_000));
        assert_ne!(first, capture_id("260106173", "key", 3_001));
        assert_ne!(first, capture_id("260106174", "key", 3_000));
    }
}
