//! Local stdio MCP server and owner of configuration and capture queries.

use std::{io, time::Instant};

use jlink_mcp::{mcp, runtime::Runtime};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    eprintln!(
        "event=stage_timing process=jlink-mcp stage=process_startup elapsed_us={}",
        started.elapsed().as_micros()
    );
    let mut runtime = Runtime::from_current_process()?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    mcp::serve(stdin.lock(), stdout.lock(), &mut runtime)?;
    Ok(())
}
