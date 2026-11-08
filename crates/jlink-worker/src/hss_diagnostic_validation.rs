//! Fixed diagnostic experiments. All lightweight observations are stored after Stop.
use crate::{
    debug::ensure_firmware_identity, gateway::DllGateway, lease::ProbeLease,
    program::execute_program, session::TargetSessionManager,
};
use jlink_domain::{
    ErrorCode, HssReturnWhen, HssStartPlan, JlinkError, ProgramRequest, TargetConnectionSpec,
    probe_identity_hash,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct ReadTotals {
    calls: u64,
    empty_calls: u64,
    zero_returns: u64,
    zero_adaptations: u64,
    bytes: u64,
    native_us: u64,
    wrapper_extra_us: u64,
    sleep_us: u64,
    first_begin_us: Option<u64>,
    last_begin_us: Option<u64>,
    interval_min_us: Option<u64>,
    interval_max_us: u64,
}

impl ReadTotals {
    fn json(&self) -> Value {
        json!({"calls":self.calls,"empty_calls":self.empty_calls,"zero_returns":self.zero_returns,
            "zero_adaptations":self.zero_adaptations,"bytes":self.bytes,"native_us":self.native_us,
            "wrapper_extra_us":self.wrapper_extra_us,"sleep_us":self.sleep_us,
            "first_begin_us":self.first_begin_us,"last_begin_us":self.last_begin_us,
            "interval_min_us":self.interval_min_us,"interval_max_us":self.interval_max_us})
    }
    fn observe(&mut self, row: &[u64; 10]) {
        if let Some(last) = self.last_begin_us {
            let interval = row[0] - last;
            self.interval_min_us = Some(self.interval_min_us.map_or(interval, |v| v.min(interval)));
            self.interval_max_us = self.interval_max_us.max(interval);
        }
        self.first_begin_us.get_or_insert(row[0]);
        self.last_begin_us = Some(row[0]);
        self.calls += 1;
        self.empty_calls += u64::from(row[5] == 0);
        self.zero_returns += u64::from(row[4] == 0);
        self.zero_adaptations += u64::from(row[4] == 0 && row[5] > 0);
        self.bytes += row[5];
        self.native_us += row[2] - row[1];
        self.wrapper_extra_us += row[3] - row[0] - (row[2] - row[1]);
    }
}

struct Observations {
    origin: Instant,
    record_bytes: usize,
    buffer: Vec<u8>,
    raw: Vec<u8>,
    // wrapper begin, native begin/end, wrapper end, native result, accepted,
    // raw offset, tail flag, sleep begin/end. All times share origin, in us.
    reads: Vec<[u64; 10]>,
    read_budget: usize,
    compact: bool,
    totals: [ReadTotals; 2],
    last_row: Option<usize>,
    last_tail: bool,
}

impl Observations {
    fn new(record_bytes: usize, read_budget: usize) -> Self {
        Self {
            origin: Instant::now(),
            record_bytes,
            buffer: vec![0; 64 * 1024],
            raw: Vec::with_capacity(320_000),
            reads: Vec::with_capacity(read_budget),
            read_budget,
            compact: false,
            totals: Default::default(),
            last_row: None,
            last_tail: false,
        }
    }

    fn us(&self, time: Instant) -> u64 {
        u64::try_from(time.duration_since(self.origin).as_micros()).expect("bounded experiment")
    }

    fn read(&mut self, gateway: &mut DllGateway, tail: bool) -> Result<usize, JlinkError> {
        if self.reads.len() >= self.read_budget {
            return Err(JlinkError::new(
                ErrorCode::FrameInvalid,
                format!(
                    "experiment read-row budget exceeded: rows={}, limit={}, raw_bytes={}",
                    self.reads.len(),
                    self.read_budget,
                    self.raw.len()
                ),
                false,
            ));
        }
        let before = Instant::now();
        let bytes = gateway.read_hss(&mut self.buffer, self.record_bytes)?;
        let after = Instant::now();
        let (native_begin, native_end, result) =
            gateway.hss_test_timing.read.expect("native boundaries");
        let row = [
            self.us(before),
            self.us(native_begin),
            self.us(native_end),
            self.us(after),
            u64::try_from(result).expect("successful native read"),
            bytes as u64,
            self.raw.len() as u64,
            u64::from(tail),
            0,
            0,
        ];
        self.totals[usize::from(tail)].observe(&row);
        self.last_tail = tail;
        self.last_row = None;
        if !self.compact || bytes > 0 {
            self.last_row = Some(self.reads.len());
            self.reads.push(row);
        }
        self.raw.extend_from_slice(&self.buffer[..bytes]);
        // Retain the final bounded read as evidence even if it crosses the raw limit.
        if self.raw.len() > 16 * 1024 * 1024 {
            return Err(JlinkError::new(
                ErrorCode::FrameInvalid,
                format!(
                    "experiment raw-byte budget exceeded: bytes={}, limit=16777216, rows={}",
                    self.raw.len(),
                    self.reads.len()
                ),
                false,
            ));
        }
        Ok(bytes)
    }

    fn sleep(&mut self) {
        let begin = self.us(Instant::now());
        thread::sleep(Duration::from_millis(1));
        let end = self.us(Instant::now());
        self.totals[usize::from(self.last_tail)].sleep_us += end - begin;
        if let Some(index) = self.last_row {
            self.reads[index][8] = begin;
            self.reads[index][9] = end;
        }
    }
}

fn write_new(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("new evidence file");
    file.write_all(bytes).expect("persist evidence after Stop");
}

fn schedule() -> [(u32, bool); 6] {
    [
        (100, true),
        (100, true),
        (1000, true),
        (1000, false),
        (1000, false),
        (1000, true),
    ]
}

fn make_plan(base: &HssStartPlan, tag: &str, rate: u32) -> Result<HssStartPlan, JlinkError> {
    HssStartPlan::new(
        tag,
        5,
        rate,
        HssReturnWhen::Completed,
        base.variables()
            .iter()
            .map(|v| v.access_plan().expect("DWARF selector").clone())
            .collect(),
        Vec::new(),
        base.firmware().expect("firmware").clone(),
    )
}

#[test]
fn schedule_keeps_two_low_rate_runs_and_balanced_diagnostic_pairs() {
    assert_eq!(
        schedule(),
        [
            (100, true),
            (100, true),
            (1000, true),
            (1000, false),
            (1000, false),
            (1000, true)
        ]
    );
    crate::hss_trace::set_test_enabled(false);
    assert!(crate::hss_trace::tick().is_none());
    crate::hss_trace::set_test_enabled(true);
}

#[test]
fn compact_totals_preserve_empty_reads_and_zero_return_adaptations() {
    let mut totals = ReadTotals::default();
    for row in [
        [10, 11, 13, 15, 0, 0, 0, 0, 0, 0],
        [20, 21, 24, 26, 8, 8, 0, 0, 0, 0],
        [35, 36, 37, 39, 0, 8, 8, 0, 0, 0],
    ] {
        totals.observe(&row);
    }
    assert_eq!(
        (
            totals.calls,
            totals.empty_calls,
            totals.zero_returns,
            totals.zero_adaptations
        ),
        (3, 1, 2, 1)
    );
    assert_eq!(
        (totals.bytes, totals.native_us, totals.wrapper_extra_us),
        (16, 6, 9)
    );
    assert_eq!(
        (
            totals.first_begin_us,
            totals.last_begin_us,
            totals.interval_min_us,
            totals.interval_max_us
        ),
        (Some(10), Some(35), Some(10), 15)
    );
}

#[test]
#[ignore = "requires explicit hardware authorization, exact binary and frozen inputs"]
fn low_rate_and_diagnostic_pairs() -> Result<(), JlinkError> {
    run_experiment(false)
}

#[test]
#[ignore = "requires explicit hardware authorization, exact binary and frozen inputs"]
fn fixed_wait_vs_no_extra_wait() -> Result<(), JlinkError> {
    run_experiment(true)
}

fn run_experiment(wait_comparison: bool) -> Result<(), JlinkError> {
    let input = PathBuf::from(
        std::env::var_os("JLINK_DIAGNOSTIC_VALIDATION_INPUT").expect("explicit input"),
    );
    let config: Value =
        serde_json::from_slice(&fs::read(&input).expect("read inputs")).expect("JSON");
    let parent_pid =
        u32::try_from(config["parent_pid"].as_u64().expect("parent pid")).expect("valid pid");
    let parent = super::ParentProcess::open(parent_pid)?;
    let output = input.parent().expect("output directory");
    let path = |name: &str| PathBuf::from(config[name].as_str().expect("path"));
    for (name, hash) in [
        ("dll", "dll_sha256"),
        ("elf", "elf_sha256"),
        ("hex", "hex_sha256"),
    ] {
        assert_eq!(
            super::sha256_file(&path(name))?,
            config[hash].as_str().unwrap()
        );
    }
    let base: HssStartPlan = serde_json::from_value(config["plan"].clone()).expect("frozen plan");
    base.validate()?;
    let single = config["single_variable"].as_bool().unwrap_or(false);
    let base = if single {
        HssStartPlan::new(
            "single-base",
            5,
            1000,
            HssReturnWhen::Completed,
            vec![
                base.variables()[0]
                    .access_plan()
                    .expect("DWARF selector")
                    .clone(),
            ],
            Vec::new(),
            base.firmware().expect("firmware").clone(),
        )?
    } else {
        base
    };
    let count = base.variables().len();
    assert!((single && count == 1) || (!single && [3, 5, 7, 10].contains(&count)));
    let target: TargetConnectionSpec =
        serde_json::from_value(config["target"].clone()).expect("target");
    assert_eq!(target.speed_khz(), 1000);
    let identity = target.probe_serial().to_string();
    let lease_root = PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("real user path"))
        .join("jlink-mcp/leases");
    assert_eq!(lease_root, path("lease_root"));
    let _lease = ProbeLease::acquire(&lease_root, &identity)?;
    let mut gateway = DllGateway::load(&path("dll"))?;
    let mut session = TargetSessionManager::new(config["dll_sha256"].as_str().unwrap().to_owned());
    session.connect(&mut gateway, &identity, &target)?;
    println!(
        "connection={}",
        serde_json::to_string(&session.status(parent_pid, &probe_identity_hash(&identity)?, true))
            .unwrap()
    );
    execute_program(
        &mut session,
        &mut gateway,
        &target,
        ProgramRequest::Verify {
            image: path("hex"),
            base_address: None,
            symbols_elf: Some(path("elf")),
        },
    )?;
    let runs: Vec<(u32, bool, bool)> = if wait_comparison {
        [true, false, false, true]
            .into_iter()
            .map(|wait| (1000, false, wait))
            .collect()
    } else {
        schedule()
            .into_iter()
            .map(|(rate, enabled)| (rate, enabled, true))
            .collect()
    };
    let run_count = runs.len();
    for (sequence, (rate, enabled, wait)) in runs.into_iter().enumerate() {
        assert!(!parent.has_exited()?);
        crate::hss_trace::set_test_enabled(enabled);
        let tag = if wait_comparison {
            format!(
                "e3-{count}-{sequence}-{}",
                if wait { "wait1" } else { "wait0" }
            )
        } else {
            format!(
                "e{}-{count}-{sequence}-{}",
                if rate == 100 { 1 } else { 2 },
                if enabled { "on" } else { "off" }
            )
        };
        let plan = make_plan(&base, &tag, rate)?;
        session.ensure_hss_start_allowed(&target)?;
        ensure_firmware_identity(
            &mut session,
            &mut gateway,
            &target,
            plan.firmware().unwrap(),
            true,
        )?;
        gateway.hss_capabilities()?.validate_start(&plan)?;
        let rb = plan.frame_layout().record_bytes() as usize;
        // Both waiting arms reserve the same bounded observation storage before Start.
        // The native buffer remains 64 KiB; only test timing rows need extra capacity.
        let mut o = Observations::new(
            rb,
            if single {
                8192
            } else if wait_comparison {
                2_097_152
            } else {
                8192
            },
        );
        o.compact = single;
        if let Some(t) = crate::hss_trace::tick() {
            crate::hss_trace::emit(
                "formal_start",
                t,
                t,
                json!({"capture_id":tag,"requested_rate_hz":rate}),
                false,
            );
        }
        let wrapper_start_begin = o.us(Instant::now());
        gateway.start_hss(&plan)?;
        let started = Instant::now();
        session.record_hss_started();
        let (t0, t1) = gateway.hss_test_timing.start.expect("Start boundaries");
        let live = (|| -> Result<usize, JlinkError> {
            loop {
                let now = Instant::now();
                if parent.has_exited()? {
                    return Err(JlinkError::new(
                        ErrorCode::WorkerUnavailable,
                        "parent exited",
                        false,
                    ));
                }
                o.read(&mut gateway, false)?;
                if now.duration_since(started) >= Duration::from_secs(5) {
                    return Ok(o.raw.len());
                }
                if wait {
                    o.sleep();
                }
            }
        })();
        if let Err(error) = gateway.stop_hss() {
            gateway.quarantine();
            return Err(error);
        }
        let (t2, t3) = gateway.hss_test_timing.stop.expect("Stop boundaries");
        let live_bytes = match live {
            Ok(bytes) => bytes,
            Err(error) => {
                let raw_path = output.join(format!("{tag}.partial.bin"));
                write_new(&raw_path, &o.raw);
                let result = json!({"id":tag,"complete":false,"error":error.to_string(),
                    "variables":count,"rate_hz":rate,"extra_wait_ms":u8::from(wait),"plan":plan,
                    "read_budget":o.read_budget,"raw_bytes":o.raw.len(),"native_stop_succeeded":true,"tail_drained":false,
                    "T0_us":o.us(t0),"T1_us":o.us(t1),"T2_us":o.us(t2),"T3_us":o.us(t3),
                    "raw_sha256":super::sha256_file(&raw_path)?,"reads":o.reads,"compact_empty_reads":o.compact,"read_totals":[o.totals[0].json(),o.totals[1].json()]});
                write_new(
                    &output.join(format!("{tag}.partial.json")),
                    &serde_json::to_vec(&result).unwrap(),
                );
                return Err(error);
            }
        };
        let tail_started = Instant::now();
        let mut empty = 0;
        while empty < 20 && tail_started.elapsed() < Duration::from_millis(500) {
            let n = o.read(&mut gateway, true)?;
            empty = if n == 0 { empty + 1 } else { 0 };
            if empty < 20 {
                o.sleep();
            }
        }
        let t4 = Instant::now();
        session.record_hss_completed();
        if empty < 20 {
            return Err(JlinkError::new(
                ErrorCode::FrameInvalid,
                "tail drain incomplete",
                false,
            ));
        }
        if let Some(t) = crate::hss_trace::tick() {
            crate::hss_trace::emit("tail_complete", t, t, json!({"capture_id":tag}), true);
        }
        let raw_path = output.join(format!("{tag}.bin"));
        write_new(&raw_path, &o.raw);
        let result = json!({"id":tag,"variables":count,"rate_hz":rate,"diagnostics":enabled,"plan":plan,
            "extra_wait_ms":u8::from(wait),"read_budget":o.read_budget,"compact_empty_reads":o.compact,"read_totals":[o.totals[0].json(),o.totals[1].json()],
            "T0_us":o.us(t0),"T1_us":o.us(t1),"T2_us":o.us(t2),"T3_us":o.us(t3),"T4_us":o.us(t4),
            "wrapper_start_begin_us":wrapper_start_begin,"wrapper_start_end_us":o.us(started),
            "record_bytes":rb,"live_bytes":live_bytes,"tail_bytes":o.raw.len()-live_bytes,"empty_tail_reads":empty,
            "raw_sha256":super::sha256_file(&raw_path)?,
            "read_columns":["wrapper_begin_us","native_begin_us","native_end_us","wrapper_end_us","native_result","accepted_bytes","raw_offset","tail","sleep_begin_us","sleep_end_us"],
            "reads":o.reads});
        write_new(
            &output.join(format!("{tag}.json")),
            &serde_json::to_vec(&result).unwrap(),
        );
        println!("completed id={tag} records={}", o.raw.len() / rb);
    }
    crate::hss_trace::set_test_enabled(true);
    session.disconnect(&mut gateway)?;
    println!("disconnected=true runs={run_count}");
    Ok(())
}
