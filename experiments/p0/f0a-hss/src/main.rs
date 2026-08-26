mod jlink;
mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use jlink::{HssBlock, HssCaps, JlinkSession};
use serde::Serialize;
use serde_json::json;
use store::{CandidateWriter, ScanSummary, benchmark, scan, sha256_file};

const SENTINEL: u8 = 0xA5;
const READ_BUFFER_BYTES: usize = 64 * 1024;
const HSS_FLAG_TIMESTAMP_US: u32 = 1;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreflightSummary {
    status: &'static str,
    command: &'static str,
    dll_path: PathBuf,
    dll_sha256: String,
    dll_version: i32,
    required_exports: [&'static str; 19],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CapsSummary {
    status: &'static str,
    command: &'static str,
    dll_sha256: String,
    get_caps_return_code: i32,
    caps: HssCaps,
    connection: jlink::ConnectEvidence,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadStatistics {
    read_calls: u64,
    empty_reads: u64,
    changed_zero_reads: u64,
    short_reads: u64,
    malformed_reads: u64,
    sample_count: u64,
    tail_sample_count: u64,
    timestamp_collisions: u64,
    timestamp_gap_events: u64,
    timestamp_gap_slots: u64,
    timestamp_regressions: u64,
    first_timestamp_raw: Option<u32>,
    last_timestamp_raw: Option<u32>,
    first_timestamp_us: Option<u64>,
    last_timestamp_us: Option<u64>,
    first_value: Option<u32>,
    last_value: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WriteEvent {
    write_api: &'static str,
    write_strategy: &'static str,
    segment_before_write: u32,
    interim_stop_return_code: Option<i32>,
    restart_return_code: Option<i32>,
    requested_at_us: u64,
    started_at_us: u64,
    completed_at_us: u64,
    value: u32,
    write_return_code: i32,
    readback: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SegmentRestartEvent {
    segment_before_restart: u32,
    requested_at_us: u64,
    started_at_us: u64,
    completed_at_us: u64,
    stop_return_code: i32,
    restart_return_code: i32,
    reconnection: jlink::ConnectEvidence,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureSummary {
    status: &'static str,
    command: &'static str,
    dll_sha256: String,
    output: PathBuf,
    output_sha256: String,
    output_bytes: u64,
    output_blocks: u64,
    output_scan: ScanSummary,
    device: String,
    interface: String,
    speed_khz: i32,
    address: u32,
    addresses: Vec<u32>,
    original_value: u32,
    original_value_restored: bool,
    target_resume_requested_after_capture: bool,
    target_running_after_capture: bool,
    duration_s: u32,
    capture_elapsed_us: u64,
    total_elapsed_us: u64,
    requested_rate_hz: u32,
    hss_flags: u32,
    timestamp_unit: &'static str,
    timestamp_frequency_hz: u32,
    source_timestamp_resolution_us: u32,
    normalized_timestamp_unit: &'static str,
    write_api: &'static str,
    write_strategy: &'static str,
    max_segment_s: u32,
    segment_count: u32,
    periodic_segment_restarts: Vec<SegmentRestartEvent>,
    actual_rate_hz: f64,
    expected_samples: u64,
    missing_samples: u64,
    sample_threshold_met: bool,
    lost_samples_evidence: &'static str,
    lost_samples_basis: &'static str,
    overflow_counter_available: bool,
    start_call_elapsed_us: u64,
    start_return_code: i32,
    stop_return_code: Option<i32>,
    tail_drain_elapsed_us: u64,
    caps: HssCaps,
    connection: jlink::ConnectEvidence,
    writes: Vec<WriteEvent>,
    reads: ReadStatistics,
    error: Option<String>,
}

#[derive(Debug)]
struct Options {
    values: BTreeMap<String, String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut values = BTreeMap::new();
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

    fn value<'a>(&'a self, name: &str, fallback: &'a str) -> &'a str {
        self.values
            .get(name)
            .map(String::as_str)
            .unwrap_or(fallback)
    }

    fn u32(&self, name: &str, fallback: Option<u32>) -> Result<u32> {
        match self.values.get(name) {
            Some(value) => parse_u32(value).with_context(|| format!("invalid --{name}")),
            None => fallback.with_context(|| format!("missing --{name}")),
        }
    }

    fn i32(&self, name: &str, fallback: Option<i32>) -> Result<i32> {
        match self.values.get(name) {
            Some(value) => value.parse().with_context(|| format!("invalid --{name}")),
            None => fallback.with_context(|| format!("missing --{name}")),
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let summary_path = option_from_args(&args, "summary").map(PathBuf::from);
    let (value, exit_code) = match run(&args) {
        Ok(value) => (value, 0),
        Err(error) => {
            let value = json!({
                "status": "error",
                "error": format!("{error:#}"),
                "targetStateUnknown": true
            });
            (value, 1)
        }
    };
    let rendered = serde_json::to_string_pretty(&value).unwrap();
    if let Some(path) = summary_path
        && let Err(error) = write_summary(&path, &rendered)
    {
        eprintln!("failed to write summary {}: {error:#}", path.display());
        println!("{rendered}");
        std::process::exit(1);
    }
    println!("{rendered}");
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

fn run(args: &[String]) -> Result<serde_json::Value> {
    let command = args.get(1).context("missing command")?;
    let options = Options::parse(&args[2..])?;
    match command.as_str() {
        "preflight" => Ok(serde_json::to_value(run_preflight(&options)?)?),
        "getcaps" => Ok(serde_json::to_value(run_getcaps(&options)?)?),
        "capture" => Ok(serde_json::to_value(run_capture(&options)?)?),
        "store-benchmark" => {
            let output = PathBuf::from(options.required("output")?);
            let summary = benchmark(
                &output,
                options.u32("duration-s", Some(300))?,
                options.u32("rate-hz", Some(1000))?,
                options.u32("sample-bytes", Some(40))?,
            )?;
            Ok(serde_json::to_value(summary)?)
        }
        "verify-store" => {
            let input = PathBuf::from(options.required("input")?);
            Ok(serde_json::to_value(scan(&input)?)?)
        }
        _ => bail!("unknown command {command}"),
    }
}

fn option_from_args<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == format!("--{name}"))
        .map(|pair| pair[1].as_str())
}

fn write_summary(path: &Path, rendered: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(rendered.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn run_preflight(options: &Options) -> Result<PreflightSummary> {
    let dll_path = PathBuf::from(options.required("dll")?);
    ensure!(
        dll_path.is_file(),
        "DLL does not exist: {}",
        dll_path.display()
    );
    let dll_version = jlink::preflight(&dll_path)?;
    Ok(PreflightSummary {
        status: "ok",
        command: "preflight",
        dll_sha256: sha256_file(&dll_path)?,
        dll_path,
        dll_version,
        required_exports: [
            "JLINKARM_Open",
            "JLINKARM_Close",
            "JLINKARM_ExecCommand",
            "JLINKARM_TIF_Select",
            "JLINKARM_SetSpeed",
            "JLINKARM_Connect",
            "JLINKARM_EMU_SelectByUSBSN",
            "JLINKARM_GetSN",
            "JLINKARM_GetId",
            "JLINKARM_IsHalted",
            "JLINKARM_Go",
            "JLINKARM_ReadMem",
            "JLINKARM_ReadMemU32",
            "JLINKARM_WriteMem",
            "JLINKARM_WriteU32",
            "JLINK_HSS_GetCaps",
            "JLINK_HSS_Start",
            "JLINK_HSS_Read",
            "JLINK_HSS_Stop",
        ],
    })
}

fn run_getcaps(options: &Options) -> Result<CapsSummary> {
    let dll_path = PathBuf::from(options.required("dll")?);
    let session = connect(options, &dll_path)?;
    let (get_caps_return_code, caps) = session.get_caps()?;
    session.ensure_running()?;
    Ok(CapsSummary {
        status: "ok",
        command: "getcaps",
        dll_sha256: sha256_file(&dll_path)?,
        get_caps_return_code,
        caps,
        connection: session.evidence.clone(),
    })
}

fn run_capture(options: &Options) -> Result<CaptureSummary> {
    let dll_path = PathBuf::from(options.required("dll")?);
    let output = PathBuf::from(options.required("output")?);
    let duration_s = options.u32("duration-s", None)?;
    let requested_rate_hz = options.u32("rate-hz", None)?;
    let address = options.u32("address", None)?;
    let mut addresses = vec![address];
    addresses.extend(parse_u32_list(options.value("additional-addresses", ""))?);
    ensure!((1..=300).contains(&duration_s), "duration-s must be 1..300");
    ensure!(
        (1..=1000).contains(&requested_rate_hz),
        "rate-hz must be 1..1000"
    );
    ensure!(addresses.len() <= 10, "at most 10 addresses are supported");
    ensure!(
        addresses.iter().all(|candidate| candidate % 4 == 0),
        "every address must be 4-byte aligned"
    );
    ensure!(
        addresses.iter().copied().collect::<BTreeSet<_>>().len() == addresses.len(),
        "addresses must be unique"
    );
    let write_values = parse_write_values(options.value("write-values", ""))?;
    let write_api = match options.value("write-api", "writemem") {
        "writemem" => "writemem",
        "writeu32" => "writeu32",
        _ => bail!("write-api must be writemem or writeu32"),
    };
    let write_strategy = match options.value("write-strategy", "direct") {
        "direct" => "direct",
        "stop-restart" => "stop-restart",
        _ => bail!("write-strategy must be direct or stop-restart"),
    };
    let max_segment_s = options.u32("max-segment-s", Some(0))?;
    ensure!(
        max_segment_s == 0 || max_segment_s < duration_s,
        "max-segment-s must be zero or less than duration-s"
    );
    let hss_flags = options.u32("hss-flags", Some(HSS_FLAG_TIMESTAMP_US))?;
    ensure!(
        hss_flags & !HSS_FLAG_TIMESTAMP_US == 0,
        "only JLINK_HSS_FLAG_TIMESTAMP_US (bit 0) is allowed"
    );
    let (timestamp_unit, timestamp_frequency_hz) = if hss_flags & HSS_FLAG_TIMESTAMP_US != 0 {
        ("us", 1_000_000_u32)
    } else {
        ("ms", 1_000_u32)
    };
    let source_timestamp_resolution_us = 1_000_000_u32 / timestamp_frequency_hz;

    let mut session = connect(options, &dll_path)?;
    let (_, caps) = session.get_caps()?;
    ensure!(
        usize::try_from(caps.max_blocks)? >= addresses.len(),
        "requested block count exceeds DLL maxBlocks"
    );
    ensure!(
        caps.max_frequency_hz >= requested_rate_hz,
        "requested rate exceeds DLL maxFrequencyHz"
    );
    session.ensure_running()?;
    let original_value = session.read_u32(address)?;
    let record_stride = 4_u32
        .checked_add(
            u32::try_from(addresses.len())?
                .checked_mul(4)
                .context("record stride overflow")?,
        )
        .context("record stride overflow")?;
    let mut writer = CandidateWriter::create(
        &output,
        record_stride,
        address,
        requested_rate_hz,
        hss_flags,
        timestamp_frequency_hz,
    )?;
    let mut blocks: Vec<HssBlock> = addresses
        .iter()
        .map(|candidate| HssBlock {
            address: *candidate,
            byte_count: 4,
            flags: 0,
            reserved: 0,
        })
        .collect();

    let epoch = Instant::now();
    let start_call = Instant::now();
    let start_return_code = session.start_hss(&mut blocks, requested_rate_hz, hss_flags)?;
    let start_call_elapsed_us = elapsed_us(start_call);
    let capture_started = Instant::now();
    let deadline = capture_started + Duration::from_secs(u64::from(duration_s));
    let mut segment_started = Instant::now();
    let mut reads = ReadStatistics::default();
    let mut writes = Vec::new();
    let mut periodic_segment_restarts = Vec::new();
    let mut next_write = 0_usize;
    let mut buffer = vec![SENTINEL; READ_BUFFER_BYTES];
    let mut last_slot = None;
    let mut capture_error = None;
    let mut segment_index = 0_u32;

    while Instant::now() < deadline {
        if max_segment_s > 0
            && segment_started.elapsed() >= Duration::from_secs(u64::from(max_segment_s))
        {
            let requested_at_us = start_call_elapsed_us
                + u64::from(max_segment_s) * 1_000_000 * u64::from(segment_index + 1);
            let started_at_us = elapsed_us(epoch);
            let stop_return_code = session.stop_hss()?;
            let intersegment_tail_started = Instant::now();
            let mut consecutive_empty = 0_u32;
            while intersegment_tail_started.elapsed() < Duration::from_millis(500)
                && consecutive_empty < 20
            {
                let samples_before = reads.sample_count;
                drain_once(
                    &session,
                    &mut writer,
                    &mut buffer,
                    segment_index,
                    epoch,
                    requested_rate_hz,
                    timestamp_frequency_hz,
                    usize::try_from(record_stride)?,
                    true,
                    &mut last_slot,
                    &mut reads,
                )?;
                if reads.sample_count == samples_before {
                    consecutive_empty += 1;
                    thread::sleep(Duration::from_millis(1));
                } else {
                    consecutive_empty = 0;
                }
            }
            session.prepare_for_reconnect()?;
            drop(session);
            session = connect(options, &dll_path)?;
            let (_, reconnect_caps) = session.get_caps()?;
            ensure!(
                reconnect_caps.max_blocks >= u32::try_from(addresses.len())?
                    && reconnect_caps.max_frequency_hz >= requested_rate_hz,
                "reconnected DLL capabilities no longer satisfy the capture request"
            );
            segment_index += 1;
            let restart_return_code =
                session.start_hss(&mut blocks, requested_rate_hz, hss_flags)?;
            last_slot = None;
            segment_started = Instant::now();
            periodic_segment_restarts.push(SegmentRestartEvent {
                segment_before_restart: segment_index - 1,
                requested_at_us,
                started_at_us,
                completed_at_us: elapsed_us(epoch),
                stop_return_code,
                restart_return_code,
                reconnection: session.evidence.clone(),
            });
        }
        if next_write < write_values.len() {
            let requested_after_start_us =
                (u64::from(duration_s) * 1_000_000 * u64::try_from(next_write + 1)?)
                    / u64::try_from(write_values.len() + 1)?;
            if elapsed_us(capture_started) >= requested_after_start_us {
                let requested_at_us = start_call_elapsed_us + requested_after_start_us;
                let started_at_us = elapsed_us(epoch);
                let interim_stop_return_code = if write_strategy == "stop-restart" {
                    Some(session.stop_hss()?)
                } else {
                    None
                };
                if write_strategy == "stop-restart" {
                    let intersegment_tail_started = Instant::now();
                    let mut consecutive_empty = 0_u32;
                    while intersegment_tail_started.elapsed() < Duration::from_millis(500)
                        && consecutive_empty < 20
                    {
                        let samples_before = reads.sample_count;
                        drain_once(
                            &session,
                            &mut writer,
                            &mut buffer,
                            segment_index,
                            epoch,
                            requested_rate_hz,
                            timestamp_frequency_hz,
                            usize::try_from(record_stride)?,
                            true,
                            &mut last_slot,
                            &mut reads,
                        )?;
                        if reads.sample_count == samples_before {
                            consecutive_empty += 1;
                            thread::sleep(Duration::from_millis(1));
                        } else {
                            consecutive_empty = 0;
                        }
                    }
                }
                match write_u32(&session, write_api, address, write_values[next_write]) {
                    Ok(write_return_code) => writes.push(WriteEvent {
                        write_api,
                        write_strategy,
                        segment_before_write: segment_index,
                        interim_stop_return_code,
                        restart_return_code: None,
                        requested_at_us,
                        started_at_us,
                        completed_at_us: elapsed_us(epoch),
                        value: write_values[next_write],
                        write_return_code,
                        readback: session.read_u32(address)?,
                    }),
                    Err(error) => {
                        capture_error = Some(format!("interleaved write failed: {error:#}"));
                        break;
                    }
                }
                if write_strategy == "stop-restart" {
                    segment_index += 1;
                    match session.start_hss(&mut blocks, requested_rate_hz, hss_flags) {
                        Ok(restart_return_code) => {
                            if let Some(event) = writes.last_mut() {
                                event.restart_return_code = Some(restart_return_code);
                            }
                            last_slot = None;
                            segment_started = Instant::now();
                        }
                        Err(error) => {
                            capture_error = Some(format!("HSS restart failed: {error:#}"));
                            break;
                        }
                    }
                }
                next_write += 1;
            }
        }
        if let Err(error) = drain_once(
            &session,
            &mut writer,
            &mut buffer,
            segment_index,
            epoch,
            requested_rate_hz,
            timestamp_frequency_hz,
            usize::try_from(record_stride)?,
            false,
            &mut last_slot,
            &mut reads,
        ) {
            capture_error = Some(format!("HSS read failed: {error:#}"));
            break;
        }
    }

    let capture_elapsed_us = elapsed_us(capture_started);
    let stop_return_code = match session.stop_hss() {
        Ok(value) => Some(value),
        Err(error) => {
            capture_error.get_or_insert_with(|| format!("HSS stop failed: {error:#}"));
            None
        }
    };
    let tail_started = Instant::now();
    let mut consecutive_empty = 0_u32;
    while tail_started.elapsed() < Duration::from_millis(500) && consecutive_empty < 20 {
        let samples_before = reads.sample_count;
        match drain_once(
            &session,
            &mut writer,
            &mut buffer,
            segment_index,
            epoch,
            requested_rate_hz,
            timestamp_frequency_hz,
            usize::try_from(record_stride)?,
            true,
            &mut last_slot,
            &mut reads,
        ) {
            Ok(()) => {
                if reads.sample_count == samples_before {
                    consecutive_empty += 1;
                    thread::sleep(Duration::from_millis(1));
                } else {
                    consecutive_empty = 0;
                }
            }
            Err(error) => {
                capture_error.get_or_insert_with(|| format!("tail drain failed: {error:#}"));
                break;
            }
        }
    }
    let tail_drain_elapsed_us = elapsed_us(tail_started);

    let original_value_restored = if write_values.is_empty() {
        session.read_u32(address)? == original_value
    } else {
        match write_u32(&session, write_api, address, original_value) {
            Ok(_) => session.read_u32(address)? == original_value,
            Err(error) => {
                capture_error.get_or_insert_with(|| format!("RAM restore failed: {error:#}"));
                false
            }
        }
    };
    if !original_value_restored {
        capture_error.get_or_insert_with(|| "RAM original value was not restored".to_owned());
    }
    let (target_resume_requested_after_capture, target_running_after_capture) =
        match session.ensure_running() {
            Ok(resume_requested) => (resume_requested, true),
            Err(error) => {
                capture_error.get_or_insert_with(|| format!("target resume failed: {error:#}"));
                (true, false)
            }
        };
    let total_elapsed_us = elapsed_us(epoch);
    let (output_bytes, output_blocks) = writer.finish()?;
    let output_scan = scan(&output)?;
    if output_scan.truncated_tail || output_scan.crc_error {
        capture_error.get_or_insert_with(|| {
            format!(
                "capture store verification failed: truncatedTail={}, crcError={}",
                output_scan.truncated_tail, output_scan.crc_error
            )
        });
    }
    let output_sha256 = sha256_file(&output)?;
    let actual_rate_hz = if segment_index > 0 {
        reads.sample_count as f64 * 1_000_000.0 / capture_elapsed_us.max(1) as f64
    } else {
        match (
            reads.first_timestamp_raw,
            reads.last_timestamp_raw,
            reads.sample_count,
        ) {
            (Some(first), Some(last), count) if count > 1 && last > first => {
                (count - 1) as f64 * f64::from(timestamp_frequency_hz) / f64::from(last - first)
            }
            _ => reads.sample_count as f64 * 1_000_000.0 / capture_elapsed_us.max(1) as f64,
        }
    };
    let expected_samples = u64::from(duration_s) * u64::from(requested_rate_hz);
    let missing_samples = expected_samples.saturating_sub(reads.sample_count);
    let sample_threshold_met =
        reads.sample_count.saturating_mul(100) >= expected_samples.saturating_mul(95);
    let unmatched_timestamp_gaps = reads
        .timestamp_gap_slots
        .saturating_sub(reads.timestamp_collisions);
    let (lost_samples_evidence, lost_samples_basis) =
        if reads.short_reads > 0 || reads.malformed_reads > 0 {
            (
                "suspected",
                "short_or_malformed_frame_without_sequence_counter",
            )
        } else if unmatched_timestamp_gaps > 0 {
            (
                "suspected",
                "timestamp_slots_exceed_collisions_without_sequence_counter",
            )
        } else if capture_error.is_none() && missing_samples > 0 {
            (
                "suspected",
                "completed_sample_count_below_requested_duration",
            )
        } else {
            ("unknown", "no_independent_overflow_or_sequence_counter")
        };
    let status = if capture_error.is_none()
        && reads.short_reads == 0
        && reads.malformed_reads == 0
        && reads.timestamp_regressions == 0
        && original_value_restored
    {
        "ok"
    } else {
        "error"
    };

    Ok(CaptureSummary {
        status,
        command: "capture",
        dll_sha256: sha256_file(&dll_path)?,
        output,
        output_sha256,
        output_bytes,
        output_blocks,
        output_scan,
        device: options.value("device", "S32K144").to_owned(),
        interface: options.value("interface", "SWD").to_owned(),
        speed_khz: options.i32("speed-khz", Some(4000))?,
        address,
        addresses,
        original_value,
        original_value_restored,
        target_resume_requested_after_capture,
        target_running_after_capture,
        duration_s,
        capture_elapsed_us,
        total_elapsed_us,
        requested_rate_hz,
        hss_flags,
        timestamp_unit,
        timestamp_frequency_hz,
        source_timestamp_resolution_us,
        normalized_timestamp_unit: "us",
        write_api,
        write_strategy,
        max_segment_s,
        segment_count: segment_index + 1,
        periodic_segment_restarts,
        actual_rate_hz,
        expected_samples,
        missing_samples,
        sample_threshold_met,
        lost_samples_evidence,
        lost_samples_basis,
        overflow_counter_available: false,
        start_call_elapsed_us,
        start_return_code,
        stop_return_code,
        tail_drain_elapsed_us,
        caps,
        connection: session.evidence.clone(),
        writes,
        reads,
        error: capture_error,
    })
}

#[allow(clippy::too_many_arguments)]
fn drain_once(
    session: &JlinkSession,
    writer: &mut CandidateWriter,
    buffer: &mut [u8],
    phase: u32,
    epoch: Instant,
    requested_rate_hz: u32,
    timestamp_frequency_hz: u32,
    record_stride: usize,
    tail: bool,
    last_slot: &mut Option<u64>,
    statistics: &mut ReadStatistics,
) -> Result<()> {
    buffer.fill(SENTINEL);
    let return_code = session.read_hss(buffer)?;
    statistics.read_calls += 1;
    ensure!(
        record_stride >= 8 && record_stride.is_multiple_of(4),
        "invalid HSS record stride"
    );
    let prefix_changed = buffer[..record_stride].iter().any(|byte| *byte != SENTINEL);
    let byte_count = if return_code == 0 && prefix_changed {
        statistics.changed_zero_reads += 1;
        record_stride
    } else {
        usize::try_from(return_code)?
    };
    if byte_count == 0 {
        statistics.empty_reads += 1;
        thread::sleep(Duration::from_millis(1));
        return Ok(());
    }
    if byte_count < record_stride {
        statistics.short_reads += 1;
        return Ok(());
    }
    if byte_count % record_stride != 0 {
        statistics.malformed_reads += 1;
        return Ok(());
    }
    writer.append(elapsed_us(epoch), phase, &buffer[..byte_count])?;
    for record in buffer[..byte_count].chunks_exact(record_stride) {
        let timestamp_raw = u32::from_le_bytes(record[..4].try_into()?);
        let timestamp_us = normalize_timestamp_us(timestamp_raw, timestamp_frequency_hz);
        let value = u32::from_le_bytes(record[4..8].try_into()?);
        observe_timestamp(
            timestamp_raw,
            requested_rate_hz,
            timestamp_frequency_hz,
            last_slot,
            statistics,
        );
        statistics.first_timestamp_raw.get_or_insert(timestamp_raw);
        statistics.last_timestamp_raw = Some(timestamp_raw);
        statistics.first_timestamp_us.get_or_insert(timestamp_us);
        statistics.last_timestamp_us = Some(timestamp_us);
        statistics.first_value.get_or_insert(value);
        statistics.last_value = Some(value);
        statistics.sample_count += 1;
        if tail {
            statistics.tail_sample_count += 1;
        }
    }
    Ok(())
}

fn observe_timestamp(
    timestamp_raw: u32,
    requested_rate_hz: u32,
    timestamp_frequency_hz: u32,
    last_slot: &mut Option<u64>,
    statistics: &mut ReadStatistics,
) {
    let timestamp_us = normalize_timestamp_us(timestamp_raw, timestamp_frequency_hz);
    let slot = (timestamp_us * u64::from(requested_rate_hz) + 500_000) / 1_000_000;
    if let Some(previous) = *last_slot {
        if slot < previous {
            statistics.timestamp_regressions += 1;
        } else if slot == previous {
            statistics.timestamp_collisions += 1;
        } else if slot > previous + 1 {
            statistics.timestamp_gap_events += 1;
            statistics.timestamp_gap_slots += slot - previous - 1;
        }
    }
    *last_slot = Some(slot);
}

/// Normalizes a J-Link source timestamp to the public integer-microsecond unit.
fn normalize_timestamp_us(timestamp_raw: u32, timestamp_frequency_hz: u32) -> u64 {
    debug_assert!(timestamp_frequency_hz > 0);
    u64::from(timestamp_raw) * 1_000_000 / u64::from(timestamp_frequency_hz)
}

fn connect(options: &Options, dll_path: &Path) -> Result<JlinkSession> {
    JlinkSession::connect(
        dll_path,
        options.value("device", "S32K144"),
        options.value("interface", "SWD"),
        options.i32("speed-khz", Some(4000))?,
        options.u32("serial", None)?,
    )
}

fn write_u32(session: &JlinkSession, api: &str, address: u32, value: u32) -> Result<i32> {
    match api {
        "writemem" => session.write_u32(address, value),
        "writeu32" => session.write_u32_direct(address, value),
        _ => bail!("unsupported write API {api}"),
    }
}

fn parse_u32(value: &str) -> Result<u32> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        Ok(u32::from_str_radix(hex, 16)?)
    } else {
        Ok(value.parse()?)
    }
}

fn parse_u32_list(value: &str) -> Result<Vec<u32>> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    value.split(',').map(parse_u32).collect()
}

fn parse_write_values(value: &str) -> Result<Vec<u32>> {
    parse_u32_list(value)
}

fn elapsed_us(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        ReadStatistics, normalize_timestamp_us, observe_timestamp, parse_u32, parse_u32_list,
    };

    #[test]
    fn parses_decimal_hex_and_lists() {
        assert_eq!(parse_u32("42").unwrap(), 42);
        assert_eq!(parse_u32("0x2A").unwrap(), 42);
        assert_eq!(parse_u32_list("1,0x2,3").unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn detects_timestamp_collision_gap_and_regression() {
        let mut statistics = ReadStatistics::default();
        let mut last_slot = None;
        for timestamp_us in [0, 1000, 1000, 3000, 2000] {
            observe_timestamp(
                timestamp_us,
                1000,
                1_000_000,
                &mut last_slot,
                &mut statistics,
            );
        }
        assert_eq!(statistics.timestamp_collisions, 1);
        assert_eq!(statistics.timestamp_gap_events, 1);
        assert_eq!(statistics.timestamp_gap_slots, 1);
        assert_eq!(statistics.timestamp_regressions, 1);
    }

    #[test]
    fn normalizes_millisecond_and_microsecond_source_timestamps() {
        assert_eq!(normalize_timestamp_us(12_345, 1_000), 12_345_000);
        assert_eq!(normalize_timestamp_us(12_345, 1_000_000), 12_345);
    }
}
