//! Read-only verifier for one completed P3 Capture Store resource.

use std::{env, path::PathBuf};

use jlink_capture::CaptureStore;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let store_root = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("缺少 Capture Store 根目录")?;
    let capture_id = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or("缺少 UTF-8 capture_id")?;
    if arguments.next().is_some() {
        return Err("只允许提供 Capture Store 根目录和 capture_id".into());
    }

    let store = CaptureStore::open(store_root)?;
    let capture = store.open_snapshot(&capture_id)?;
    let plan = capture.plan();
    println!(
        "{}",
        serde_json::to_string(&json!({
            "capture_id": capture.capture_id(),
            "capture_key": capture.capture_key(),
            "target": capture.target(),
            "duration_s": plan.duration_s(),
            "rate_hz": plan.rate_hz(),
            "selector_count": plan.variables().len(),
            "sample_bytes": plan.frame_layout().sample_bytes(),
            "record_bytes": plan.frame_layout().record_bytes(),
            "payload_bytes": capture.payload_bytes(),
            "raw_sha256": capture.raw_sha256(),
            "status": capture.status(),
        }))?
    );
    Ok(())
}
