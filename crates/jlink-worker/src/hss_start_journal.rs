//! Durable start boundaries; these are dispatch intentions, not hardware outcomes.
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use jlink_domain::{ErrorCode, HssRunSnapshot, JlinkError};
use serde_json::json;

const MAX_JOURNAL_BYTES: u64 = 256;
const PREFLIGHT: &str = "preflight_pending\n";
const FORMAL_START: &str = "formal_start_pending\n";

#[derive(Clone, Copy)]
pub(super) enum StartBoundary {
    Preflight,
    FormalStart,
}

impl StartBoundary {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Preflight => "preflight_pending",
            Self::FormalStart => "formal_start_pending",
        }
    }
}

fn journal_path(root: &Path, capture_id: &str) -> Option<PathBuf> {
    if capture_id.is_empty()
        || !capture_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }
    Some(root.join(format!("capture-{capture_id}.start-journal")))
}

pub(super) fn record(
    root: &Path,
    capture_id: &str,
    boundary: StartBoundary,
) -> Result<(), JlinkError> {
    let failure = |message: String| {
        JlinkError::new(ErrorCode::HssStartFailed, message, false)
            .with_detail("capture_id", json!(capture_id))
            .with_detail("start_boundary", json!(boundary.label()))
    };
    let path = journal_path(root, capture_id)
        .ok_or_else(|| failure("Invalid start-journal capture identity".to_owned()))?;
    let mut options = OpenOptions::new();
    options.append(true);
    let bytes = match boundary {
        StartBoundary::Preflight => {
            options.create_new(true);
            PREFLIGHT.as_bytes()
        }
        StartBoundary::FormalStart => FORMAL_START.as_bytes(),
    };
    let mut file = options
        .open(&path)
        .map_err(|error| failure(format!("Cannot open HSS start journal: {error}")))?;
    let length = file
        .metadata()
        .map_err(|error| failure(format!("Cannot inspect HSS start journal: {error}")))?
        .len();
    if length.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX)) > MAX_JOURNAL_BYTES {
        return Err(failure(
            "HSS start journal exceeds its fixed bound".to_owned(),
        ));
    }
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| failure(format!("Cannot persist HSS start boundary: {error}")))?;
    // Logging failure must not turn a diagnostic into a new Worker failure.
    let _ = writeln!(
        std::io::stderr().lock(),
        "hss capture_id={capture_id} boundary={}",
        boundary.label()
    );
    Ok(())
}

pub(super) fn annotate_recovery(root: &Path, capture_id: &str, status: &mut HssRunSnapshot) {
    let Some(path) = journal_path(root, capture_id) else {
        return;
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            append_reason(
                status,
                "start journal unavailable; native outcome remains unknown",
            );
            return;
        }
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        append_reason(
            status,
            "start journal unreadable; native outcome remains unknown",
        );
        return;
    }
    let boundary = if bytes == PREFLIGHT.as_bytes() {
        Some("preflight_pending")
    } else if bytes == [PREFLIGHT.as_bytes(), FORMAL_START.as_bytes()].concat() {
        Some("formal_start_pending")
    } else {
        None
    };
    match boundary {
        Some(boundary) => append_reason(
            status,
            &format!(
                "last_recorded_start_boundary={boundary}; this does not prove the native call executed or completed"
            ),
        ),
        None => append_reason(
            status,
            "start journal invalid or incomplete; native outcome remains unknown",
        ),
    }
}

fn append_reason(status: &mut HssRunSnapshot, detail: &str) {
    let reason = status.reason.get_or_insert_with(String::new);
    if !reason.is_empty() {
        reason.push_str("; ");
    }
    reason.push_str(detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_is_bounded_and_never_overwrites_an_admission() {
        let directory = tempfile::tempdir().unwrap();
        record(directory.path(), "safe-id", StartBoundary::Preflight).unwrap();
        assert!(record(directory.path(), "safe-id", StartBoundary::Preflight).is_err());
        record(directory.path(), "safe-id", StartBoundary::FormalStart).unwrap();
        let path = journal_path(directory.path(), "safe-id").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{PREFLIGHT}{FORMAL_START}")
        );
        assert!(std::fs::metadata(path).unwrap().len() <= MAX_JOURNAL_BYTES);
        assert!(journal_path(directory.path(), "../outside").is_none());
    }
}
