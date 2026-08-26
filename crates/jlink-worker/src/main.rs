//! Isolated owner of the J-Link DLL, probe session, and active capture state.

use std::{env, time::Instant};

use jlink_worker::{WorkerOptions, run_worker};

fn main() {
    let started = Instant::now();
    eprintln!(
        "event=stage_timing process=jlink-worker stage=process_startup elapsed_us={}",
        started.elapsed().as_micros()
    );
    let result =
        WorkerOptions::parse(env::args_os().skip(1)).and_then(|options| run_worker(&options));
    if let Err(error) = result {
        eprintln!("Worker 启动失败：{error}");
        std::process::exit(1);
    }
}
