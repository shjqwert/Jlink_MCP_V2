//! Optional bounded local HSS diagnostics; never changes the MCP contract.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    sync::{Mutex, OnceLock},
    time::Instant,
};

struct Trace {
    origin: Instant,
    writer: BufWriter<std::fs::File>,
    bytes: usize,
    failed: bool,
}

fn trace() -> Option<&'static Mutex<Trace>> {
    static TRACE: OnceLock<Option<Mutex<Trace>>> = OnceLock::new();
    TRACE
        .get_or_init(|| {
            let path = std::env::var_os("JLINK_MCP_HSS_TRACE")?;
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(file) => Some(Mutex::new(Trace {
                    origin: Instant::now(),
                    writer: BufWriter::with_capacity(65536, file),
                    bytes: 0,
                    failed: false,
                })),
                Err(error) => {
                    eprintln!("hss_trace unavailable: {error}");
                    None
                }
            }
        })
        .as_ref()
}

pub(crate) fn tick() -> Option<u64> {
    let state = trace()?.lock().ok()?;
    Some(u64::try_from(state.origin.elapsed().as_micros()).unwrap_or(u64::MAX))
}

/// Hash exactly the positively returned native bytes, before any adaptation.
pub(crate) fn positive_read_sha256(bytes: &[u8], returned: i32) -> Option<String> {
    let length = usize::try_from(returned).ok()?;
    if length == 0 || length > bytes.len() {
        return None;
    }
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(&bytes[..length]) {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Some(hex)
}

#[cfg(test)]
mod tests {
    #[test]
    fn hash_covers_only_valid_positive_returned_bytes() {
        assert_eq!(
            super::positive_read_sha256(b"abcunused", 3).as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        for invalid in [-1, 0, 10] {
            assert!(super::positive_read_sha256(b"abcunused", invalid).is_none());
        }
    }
}

pub(crate) fn emit(event: &str, start: u64, end: u64, details: Value, flush: bool) {
    let Some(trace) = trace() else { return };
    let Ok(mut state) = trace.lock() else { return };
    if state.failed {
        return;
    }
    let mut record = json!({"event":event,"begin_us":start,"end_us":end});
    record["details"] = details;
    let Ok(mut line) = serde_json::to_vec(&record) else {
        return;
    };
    line.push(b'\n');
    if state.bytes.saturating_add(line.len()) > 16 * 1024 * 1024 {
        let _ = state
            .writer
            .write_all(b"{\"event\":\"trace_limit_reached\"}\n");
        let _ = state.writer.flush();
        state.failed = true;
        return;
    }
    state.bytes += line.len();
    if state
        .writer
        .write_all(&line)
        .and_then(|()| if flush { state.writer.flush() } else { Ok(()) })
        .is_err()
    {
        state.failed = true;
        eprintln!("hss_trace write failed; diagnostic evidence is incomplete");
    }
}
