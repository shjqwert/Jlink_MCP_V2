"""One-shot, branch-only source transformation; removed after validation."""
from pathlib import Path
import hashlib


def replace_once(text, before, after):
    if text.count(before) != 1:
        raise RuntimeError(f"Expected exactly one source anchor: {before[:100]!r}")
    return text.replace(before, after, 1)


def read_base(root, path, sha):
    data = (root / path).read_bytes()
    actual = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
    if actual != sha:
        raise RuntimeError(f"Refusing unexpected baseline for {path}: {actual}")
    return data.decode("utf-8")


JOURNAL = r'''//! Durable start boundaries; these are dispatch intentions, not hardware outcomes.
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
        || !capture_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
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
    let mut file = options.open(&path)
        .map_err(|error| failure(format!("Cannot open HSS start journal: {error}")))?;
    let length = file.metadata()
        .map_err(|error| failure(format!("Cannot inspect HSS start journal: {error}")))?.len();
    if length.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX)) > MAX_JOURNAL_BYTES {
        return Err(failure("HSS start journal exceeds its fixed bound".to_owned()));
    }
    file.write_all(bytes).and_then(|()| file.sync_all())
        .map_err(|error| failure(format!("Cannot persist HSS start boundary: {error}")))?;
    // Logging failure must not turn a diagnostic into a new Worker failure.
    let _ = writeln!(std::io::stderr().lock(), "hss capture_id={capture_id} boundary={}", boundary.label());
    Ok(())
}

pub(super) fn annotate_recovery(root: &Path, capture_id: &str, status: &mut HssRunSnapshot) {
    let Some(path) = journal_path(root, capture_id) else { return; };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            append_reason(status, "start journal unavailable; native outcome remains unknown");
            return;
        }
    };
    let mut bytes = Vec::new();
    if file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes).is_err() {
        append_reason(status, "start journal unreadable; native outcome remains unknown");
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
        Some(boundary) => append_reason(status, &format!(
            "last_recorded_start_boundary={boundary}; this does not prove the native call executed or completed"
        )),
        None => append_reason(status, "start journal invalid or incomplete; native outcome remains unknown"),
    }
}

fn append_reason(status: &mut HssRunSnapshot, detail: &str) {
    let reason = status.reason.get_or_insert_with(String::new);
    if !reason.is_empty() { reason.push_str("; "); }
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
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{PREFLIGHT}{FORMAL_START}"));
        assert!(std::fs::metadata(path).unwrap().len() <= MAX_JOURNAL_BYTES);
        assert!(journal_path(directory.path(), "../outside").is_none());
    }
}
'''

HSS_TESTS = r'''

    #[test]
    fn recovery_preflight_has_durable_identity_before_any_native_work() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let (root, mut coordinator) = open_coordinator();
        let plan = start_plan();
        let mut io = ScriptedHss::healthy([]);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = coordinator.start(
                "260106173", &target(), plan.clone(), TEST_CAPTURE_MAX_BYTES, &mut io,
                |io| {
                    // Inspect before unwinding: BufWriter::drop must not be what makes
                    // the identity readable. No hardware is used by this fault injection.
                    let store = CaptureStore::open(root.path()).unwrap();
                    let recoveries = store.recover_partials().unwrap();
                    assert_eq!(recoveries.len(), 1);
                    match &recoveries[0] {
                        CaptureRecovery::Aborted { capture_key, plan: stored_plan, .. } => {
                            assert_eq!(capture_key.as_deref(), Some(plan.capture_key()));
                            assert_eq!(stored_plan.as_ref().unwrap().request_fingerprint(), plan.request_fingerprint());
                        }
                        CaptureRecovery::Published(_) => panic!("admission is not completed data"),
                    }
                    io.start_hss(&plan)?;
                    panic!("injected Worker loss during temporary HSS preflight");
                },
            );
        }));
        assert!(result.is_err());
        drop(coordinator);
        let recovered = HssCoordinator::open(root.path(), "260106173").unwrap();
        let snapshot = recovered.status_by_key(plan.capture_key(), Instant::now()).unwrap();
        assert_eq!(snapshot.state, HssRunState::Aborted);
        assert_eq!(snapshot.integrity, HssDataIntegrity::Unknown);
        assert_eq!(snapshot.complete_records, 0);
        assert!(!snapshot.partial_available);
        assert!(snapshot.reason.as_deref().unwrap().contains("preflight_pending"));
        assert_eq!(recovered.status(&snapshot.capture_id, Instant::now()).unwrap(), snapshot);
    }

    #[test]
    fn recovery_formal_start_loss_keeps_key_without_claiming_hardware_success() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        struct LostAtStart;
        impl HssIo for LostAtStart {
            fn start_hss(&mut self, _: &HssStartPlan) -> Result<(), JlinkError> {
                panic!("injected loss at formal Start");
            }
            fn read_hss(&mut self, _: &mut [u8], _: usize) -> Result<HssReadOutcome, JlinkError> {
                panic!("no drain should occur");
            }
            fn stop_hss(&mut self) -> Result<(), JlinkError> {
                panic!("unconfirmed Start must not be blindly replayed or stopped");
            }
        }
        let (root, mut coordinator) = open_coordinator();
        let plan = start_plan();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = coordinator.start(
                "260106173", &target(), plan.clone(), TEST_CAPTURE_MAX_BYTES,
                &mut LostAtStart, |_| Ok(rate_assessment()),
            );
        }));
        assert!(result.is_err());
        drop(coordinator);
        let mut recovered = HssCoordinator::open(root.path(), "260106173").unwrap();
        let snapshot = recovered.status_by_key(plan.capture_key(), Instant::now()).unwrap();
        assert_eq!(snapshot.state, HssRunState::Aborted);
        assert!(snapshot.reason.as_deref().unwrap().contains("formal_start_pending"));
        let mut io = ScriptedHss::healthy([]);
        let error = recovered.start(
            "260106173", &target(), plan, TEST_CAPTURE_MAX_BYTES, &mut io,
            |_| panic!("historical key cannot restart a stream"),
        ).unwrap_err();
        assert_eq!(error.code, ErrorCode::CaptureKeyConflict);
        assert!(io.calls.is_empty());
    }

    #[test]
    fn recovery_preflight_rejection_is_recorded_and_does_not_leave_a_dangling_key() {
        let (root, mut coordinator) = open_coordinator();
        let plan = start_plan();
        let mut io = ScriptedHss::healthy([]);
        let error = coordinator.start(
            "260106173", &target(), plan.clone(), TEST_CAPTURE_MAX_BYTES, &mut io,
            |_| Err(JlinkError::new(ErrorCode::HssUnsupported, "rate rejected", false)),
        ).unwrap_err();
        assert_eq!(error.code, ErrorCode::HssUnsupported);
        let failed = coordinator.status_by_key(plan.capture_key(), Instant::now()).unwrap();
        assert_eq!(failed.state, HssRunState::Failed);
        assert_eq!(failed.failure_code, Some(ErrorCode::HssUnsupported));
        assert!(io.calls.is_empty());
        let again = coordinator.start(
            "260106173", &target(), plan.clone(), TEST_CAPTURE_MAX_BYTES, &mut io,
            |_| panic!("same request recovers the failed attempt, not a second preflight"),
        ).unwrap();
        assert!(!again.started_new);
        assert_eq!(again.snapshot, failed);
        drop(coordinator);
        let recovered = HssCoordinator::open(root.path(), "260106173").unwrap();
        assert_eq!(recovered.status_by_key(plan.capture_key(), Instant::now()).unwrap(), failed);
    }

    #[test]
    fn recovery_capacity_failure_never_enters_live_preflight() {
        let (_root, mut coordinator) = open_coordinator();
        let mut io = ScriptedHss::healthy([]);
        let error = coordinator.start(
            "260106173", &target(), start_plan(), 1, &mut io,
            |_| panic!("storage admission must finish before temporary HSS"),
        ).unwrap_err();
        assert_eq!(error.code, ErrorCode::HssUnsupported);
        assert!(io.calls.is_empty());
        assert!(coordinator.status_by_key("never-admitted", Instant::now()).is_err());
    }
'''


def apply(root):
    root = Path(root)
    changed = []
    path = "crates/jlink-capture/src/store.rs"
    text = read_base(root, path, "1459313e8cdf021414dc9adef40ad501f9927b7c")
    text = replace_once(text, "        let bytes_written = FILE_HEADER_BYTES\n", """        // Admission must survive a hard Worker exit, not only BufWriter::drop.
        // This sync is before any temporary or formal native HSS Start.
        writer
            .flush()
            .and_then(|()| writer.get_ref().sync_all())
            .map_err(|error| storage_error(format!("Cannot persist Capture Store admission: {error}")))?;
        let bytes_written = FILE_HEADER_BYTES
""")
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)

    path = "crates/jlink-worker/src/hss.rs"
    text = read_base(root, path, "31240eb716ce7991d761326ac1910d33bd54f231")
    text = replace_once(text, "use crate::gateway::DllGateway;", """use crate::gateway::DllGateway;

#[path = "hss_start_journal.rs"]
mod start_journal;
use start_journal::StartBoundary;""")
    text = replace_once(text, """                    status,
                    ..
                } => {
                    match (&target, &plan) {""", """                    mut status,
                    ..
                } => {
                    start_journal::annotate_recovery(store.root(), &capture_id, &mut status);
                    match (&target, &plan) {""")
    old_preflight = """        let rate_assessment = match preflight(io) {
            Ok(assessment) => assessment.ensure_accepted()?,
            Err(error) => {
                self.registry.rollback_created(&plan, &reservation);
                return Err(error);
            }
        };
"""
    text = replace_once(text, old_preflight, "")
    text = replace_once(text, """        let start_called = Instant::now();
        let start_result = io.start_hss(&plan);""", """        // The self-describing header is durable before preflight, which can
        // itself run a temporary native stream. Never lose an admitted key.
        if let Err(error) = start_journal::record(self.store.root(), &capture_id, StartBoundary::Preflight) {
            return Err(self.record_start_failure(&capture_id, plan, writer, error));
        }
        let rate_assessment = match preflight(io).and_then(HssRateAssessment::ensure_accepted) {
            Ok(assessment) => assessment,
            Err(error) => {
                let error = error.with_detail("start_boundary", json!(StartBoundary::Preflight.label()));
                return Err(self.record_start_failure(&capture_id, plan, writer, error));
            }
        };
        if let Err(error) = start_journal::record(self.store.root(), &capture_id, StartBoundary::FormalStart) {
            return Err(self.record_start_failure(&capture_id, plan, writer, error));
        }
        let start_called = Instant::now();
        let start_result = io.start_hss(&plan);""")
    text = replace_once(text, """        if let Err(error) = start_result {
            return Err(self.record_start_failure(&capture_id, plan, writer, error));
        }""", """        if let Err(error) = start_result {
            let error = error.with_detail("start_boundary", json!(StartBoundary::FormalStart.label()));
            return Err(self.record_start_failure(&capture_id, plan, writer, error));
        }""")
    text = replace_once(text, """        status
            .mark_failed(error.code, false, Vec::new())
            .expect("a controlled Start failure can terminate starting");""", """        if matches!(error.code, ErrorCode::ExecutionUncertain | ErrorCode::TargetRecoveryFailed) {
            status.mark_aborted(
                "HSS admission ended with unconfirmed native execution or cleanup",
                false, false, Vec::new(),
            ).expect("an uncertain start remains aborted/unknown");
        } else {
            status
                .mark_failed(error.code, false, Vec::new())
                .expect("a controlled Start failure can terminate starting");
        }""")
    text = replace_once(text, """            failure_code: status.failure_code(),
            partial_available: false,
            reason: None,
            recoverable: None,
            recovery_notifications: Vec::new(),""", """            failure_code: Some(error.code),
            partial_available: false,
            reason: status.reason().map(str::to_owned),
            recoverable: status.recoverable(),
            recovery_notifications: status.recovery_notifications().to_vec(),""")
    text = replace_once(text, '.with_detail("state", json!(HssRunState::Failed))', '.with_detail("state", json!(status.lifecycle()))')
    text = replace_once(text, """            .capture_id_for_key(capture_key)
            .ok_or_else(|| {""", """            .capture_id_for_key(capture_key)
            // Retirement prevents a new Start, not a historical status lookup.
            .or_else(|| self.retired_keys.get(capture_key).map(String::as_str))
            .ok_or_else(|| {""")
    text = replace_once(text, """        assert_eq!(
            recovered
                .status_by_key(plan.capture_key(), Instant::now())
                .expect_err("capture key does not cross Worker lifecycles")
                .code,
            ErrorCode::ValueInvalid
        );""", """        assert_eq!(
            recovered.status_by_key(plan.capture_key(), Instant::now())
                .expect("historical status remains queryable by key"),
            by_id
        );""")
    text = replace_once(text, """        let retired_key = recovered
            .status_by_key("run-fixture", Instant::now())
            .expect_err("aborted capture key is retired after restart");
        assert_eq!(retired_key.code, ErrorCode::ValueInvalid);""", """        let by_key = recovered
            .status_by_key("run-fixture", Instant::now())
            .expect("retired key can query an aborted capture without restarting it");
        assert_eq!(by_key, snapshot);""")
    assert text.endswith("}\n") and "#[cfg(test)]\nmod tests" in text
    text = text[:-2] + HSS_TESTS + "}\n"
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)
    path = "crates/jlink-worker/src/hss_start_journal.rs"
    if (root / path).exists():
        raise RuntimeError("Refusing to overwrite start-journal module")
    (root / path).write_text(JOURNAL, encoding="utf-8", newline="\n")
    changed.append(path)

    path = "crates/jlink-worker/src/gateway.rs"
    text = read_base(root, path, "abf2443ba7328bfb66b62d2bb481b22c7ceb9a7c")
    text = replace_once(text, "const RECOVERY_TIMEOUT: Duration", """fn log_hss_boundary(message: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "hss {message}");
}

const RECOVERY_TIMEOUT: Duration""")
    text = replace_once(text, """        // SAFETY: `caps` is writable and the frozen 6.98a ABI was verified in F0-A.
        let result = unsafe { get_caps(&raw mut caps) };""", """        log_hss_boundary("native_call=GetCaps boundary=enter");
        // SAFETY: `caps` is writable and the frozen 6.98a ABI was verified in F0-A.
        let result = unsafe { get_caps(&raw mut caps) };
        log_hss_boundary(&format!("native_call=GetCaps boundary=return result={result}"));""")
    text = replace_once(text, """        // SAFETY: block layout and call signature are frozen by F0-A; the unique""", """        log_hss_boundary("native_call=Start boundary=enter");
        // SAFETY: block layout and call signature are frozen by F0-A; the unique""")
    text = replace_once(text, """        if result < 0 {
            return Err(hss_start_error(result));
        }""", """        log_hss_boundary(&format!("native_call=Start boundary=return result={result}"));
        if result < 0 {
            return Err(hss_start_error(result));
        }""")
    text = replace_once(text, """        // SAFETY: the unique gateway owns the matching successful Start call.
        let result = unsafe { stop() };""", """        log_hss_boundary("native_call=Stop boundary=enter");
        // SAFETY: the unique gateway owns the matching successful Start call.
        let result = unsafe { stop() };
        log_hss_boundary(&format!("native_call=Stop boundary=return result={result}"));""")
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)
    return changed
