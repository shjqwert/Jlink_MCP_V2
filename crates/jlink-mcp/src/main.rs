//! Local stdio MCP server and owner of configuration and capture queries.

use std::time::Instant;

fn main() {
    let started = Instant::now();
    eprintln!(
        "event=stage_timing process=jlink-mcp stage=process_startup elapsed_us={}",
        started.elapsed().as_micros()
    );
}
