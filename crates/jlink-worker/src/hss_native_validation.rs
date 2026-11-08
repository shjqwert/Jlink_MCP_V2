//! Explicitly invoked local hardware experiment; never part of normal tests.
use crate::{
    debug::ensure_firmware_identity, gateway::DllGateway, lease::ProbeLease,
    program::execute_program, session::TargetSessionManager,
};
use jlink_domain::{
    ErrorCode, HssStartPlan, JlinkError, ProgramRequest, TargetConnectionSpec, probe_identity_hash,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

/// Same gateway, ABI, lease, session recovery and identity checks as the Worker.
/// Removes IPC dispatch, coordinator, quality tracking and streaming disk writes.
#[test]
#[ignore = "requires explicit current hardware authorization and frozen local inputs"]
fn native_matrix_without_coordinator() -> Result<(), JlinkError> {
    let input =
        PathBuf::from(std::env::var_os("JLINK_NATIVE_VALIDATION_INPUT").expect("explicit input"));
    let config: Value = serde_json::from_slice(&fs::read(&input).expect("read frozen inputs"))
        .expect("JSON inputs");
    let parent_pid = config["parent_pid"].as_u64().expect("parent pid") as u32;
    let parent = super::ParentProcess::open(parent_pid)?;
    let output = input.parent().expect("input directory");
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
    let target: TargetConnectionSpec =
        serde_json::from_value(config["target"].clone()).expect("target");
    let identity = target.probe_serial().to_string();
    let lease_root =
        PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("real user LOCALAPPDATA"))
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
    let mut summaries = Vec::new();
    for item in config["plans"].as_array().expect("plans") {
        let plan: HssStartPlan = serde_json::from_value(item.clone()).expect("persisted plan");
        plan.validate()?;
        assert_eq!((plan.rate_hz(), plan.duration_s()), (500, 5));
        let count = plan.variables().len();
        assert!([3, 5, 7, 10].contains(&count));
        for repeat in 0..2 {
            assert!(!parent.has_exited()?);
            session.ensure_hss_start_allowed(&target)?;
            ensure_firmware_identity(
                &mut session,
                &mut gateway,
                &target,
                plan.firmware().expect("DWARF"),
                true,
            )?;
            gateway.hss_capabilities()?.validate_start(&plan)?;
            let rb = plan.frame_layout().record_bytes() as usize;
            let mut buffer = vec![0; 64 * 1024];
            let mut raw = Vec::with_capacity(320_000);
            let tag = format!("native-{count}-{repeat}");
            if let Some(t) = crate::hss_trace::tick() {
                crate::hss_trace::emit(
                    "formal_start",
                    t,
                    t,
                    json!({"capture_id":tag,"requested_rate_hz":500}),
                    false,
                );
            }
            gateway.start_hss(&plan)?;
            session.record_hss_started();
            let started = Instant::now();
            let live_result = (|| -> Result<usize, JlinkError> {
                loop {
                    let now = Instant::now();
                    if parent.has_exited()? {
                        return Err(JlinkError::new(
                            ErrorCode::WorkerUnavailable,
                            "validation parent exited",
                            false,
                        ));
                    }
                    let n = gateway.read_hss(&mut buffer, rb)?;
                    raw.extend_from_slice(&buffer[..n]);
                    if raw.len() > 16 * 1024 * 1024 {
                        return Err(JlinkError::new(
                            ErrorCode::FrameInvalid,
                            "bounded experiment buffer exceeded",
                            false,
                        ));
                    }
                    if now.duration_since(started) >= Duration::from_secs(5) {
                        return Ok(raw.len());
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            })();
            if let Err(error) = gateway.stop_hss() {
                gateway.quarantine();
                return Err(error);
            }
            let live_bytes = live_result?;
            let tail_started = Instant::now();
            let mut empty = 0;
            while empty < 20 && tail_started.elapsed() < Duration::from_millis(500) {
                let n = gateway.read_hss(&mut buffer, rb)?;
                raw.extend_from_slice(&buffer[..n]);
                empty = if n == 0 { empty + 1 } else { 0 };
                thread::sleep(Duration::from_millis(1));
            }
            session.record_hss_completed();
            if empty < 20 {
                return Err(JlinkError::new(
                    ErrorCode::FrameInvalid,
                    "tail drain did not reach 20 empty reads",
                    false,
                ));
            }
            if let Some(t) = crate::hss_trace::tick() {
                crate::hss_trace::emit("tail_complete", t, t, json!({"capture_id":tag}), true);
            }
            let destination = output.join(format!("{tag}.bin"));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .expect("new evidence file");
            std::io::Write::write_all(&mut file, &raw).expect("persist raw evidence after Stop");
            summaries.push(
                json!({"id":tag,"variables":count,"record_bytes":rb,"live_bytes":live_bytes,
                "tail_bytes":raw.len()-live_bytes,"trailing_bytes":raw.len()%rb,
                "raw_sha256":super::sha256_file(&destination)?,"empty_tail_reads":empty}),
            );
        }
    }
    session.disconnect(&mut gateway)?;
    fs::write(
        output.join("native-results.json"),
        serde_json::to_vec_pretty(&summaries).unwrap(),
    )
    .expect("summary");
    println!("disconnected=true runs={}", summaries.len());
    Ok(())
}
