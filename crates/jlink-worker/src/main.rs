//! Isolated owner of the J-Link DLL, probe session, and active capture state.

use std::time::Instant;

fn main() {
    let started = Instant::now();
    eprintln!(
        "event=stage_timing process=jlink-worker stage=process_startup elapsed_us={}",
        started.elapsed().as_micros()
    );
}
