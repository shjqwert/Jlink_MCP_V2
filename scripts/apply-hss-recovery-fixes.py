"""Exact, branch-only final checks and diagnostics corrections."""
from pathlib import Path
import os
import subprocess

if os.environ.get("GITHUB_REF") != "refs/heads/codex/hss-recovery-preflight-fixes":
    raise SystemExit("Dedicated repair branch required")
root = Path.cwd()
subprocess.run(["git", "fetch", "--no-tags", "--depth=1", "origin", "dfbc9f719b85e1e07d5c28bbb6e7fb4ee2bdb1ab"], check=True)
changed = []

def replace(path, before, after, count=1):
    text = (root / path).read_text(encoding="utf-8")
    if before not in text and after in text:
        return
    if text.count(before) != count:
        raise RuntimeError(f"Source anchor mismatch in {path}: {before[:120]!r}")
    (root / path).write_text(text.replace(before, after), encoding="utf-8", newline="\n")
    changed.append(path)

replace("crates/jlink-mcp/src/worker_diagnostics.rs", "fn diagnostic_error(error: io::Error)", "fn diagnostic_error(error: &io::Error)")
replace("crates/jlink-mcp/src/worker_diagnostics.rs", ".map_err(diagnostic_error)?", ".map_err(|error| diagnostic_error(&error))?", 2)
replace("crates/jlink-mcp/src/worker_client.rs", '''    let stderr = child
        .stderr
        .take()
        .expect("Worker was spawned with piped stderr");''', '''    let Some(stderr) = child.stderr.take() else {
        // A newly spawned process has not received a target request yet.
        let _ = child.kill();
        let _ = child.wait();
        return Err(JlinkError::new(
            ErrorCode::WorkerUnavailable,
            "New Worker did not expose the requested diagnostic pipe",
            false,
        ));
    };''')

HARD_EXIT_TESTS = r'''

    #[test]
    fn recovery_child_exits_without_dropping_admission() {
        let Some(root) = std::env::var_os("JLINK_TEST_HSS_HARD_EXIT_ROOT") else {
            return;
        };
        let phase = std::env::var("JLINK_TEST_HSS_HARD_EXIT_PHASE").unwrap();
        struct ExitAtFormalStart;
        impl HssIo for ExitAtFormalStart {
            fn start_hss(&mut self, _: &HssStartPlan) -> Result<(), JlinkError> {
                std::process::exit(71);
            }
            fn read_hss(&mut self, _: &mut [u8], _: usize) -> Result<HssReadOutcome, JlinkError> {
                panic!("hard-exit fixture must never drain");
            }
            fn stop_hss(&mut self) -> Result<(), JlinkError> {
                panic!("hard-exit fixture must never stop");
            }
        }
        let mut coordinator = HssCoordinator::open(std::path::PathBuf::from(root), "260106173").unwrap();
        let _ = coordinator.start(
            "260106173", &target(), start_plan(), TEST_CAPTURE_MAX_BYTES,
            &mut ExitAtFormalStart,
            |_| {
                if phase == "preflight" {
                    // process::exit does not unwind or flush the Rust BufWriter.
                    std::process::exit(71);
                }
                assert_eq!(phase, "formal_start");
                Ok(rate_assessment())
            },
        );
        panic!("hard-exit fixture unexpectedly returned");
    }

    #[test]
    fn recovery_survives_process_exit_without_destructors_in_both_start_phases() {
        for phase in ["preflight", "formal_start"] {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "hss::tests::recovery_child_exits_without_dropping_admission", "--nocapture"])
                .env("JLINK_TEST_HSS_HARD_EXIT_ROOT", root.path())
                .env("JLINK_TEST_HSS_HARD_EXIT_PHASE", phase)
                .output().unwrap();
            assert_eq!(output.status.code(), Some(71), "{}", String::from_utf8_lossy(&output.stderr));
            let mut recovered = HssCoordinator::open(root.path(), "260106173").unwrap();
            let plan = start_plan();
            let snapshot = recovered.status_by_key(plan.capture_key(), Instant::now()).unwrap();
            assert_eq!(snapshot.state, HssRunState::Aborted);
            assert_eq!(snapshot.integrity, HssDataIntegrity::Unknown);
            assert_eq!(snapshot.complete_records, 0);
            assert!(!snapshot.partial_available);
            assert!(snapshot.reason.as_deref().unwrap().contains(&format!("{phase}_pending")));
            assert_eq!(recovered.status(&snapshot.capture_id, Instant::now()).unwrap(), snapshot);
            let mut io = ScriptedHss::healthy([]);
            let error = recovered.start(
                "260106173", &target(), plan, TEST_CAPTURE_MAX_BYTES, &mut io,
                |_| panic!("historical lookup must not permit a new native stream"),
            ).unwrap_err();
            assert_eq!(error.code, ErrorCode::CaptureKeyConflict);
            assert!(io.calls.is_empty());
        }
    }

    #[test]
    fn recovery_uncertain_preflight_cleanup_remains_aborted_unknown() {
        let (root, mut coordinator) = open_coordinator();
        let plan = start_plan();
        let mut io = ScriptedHss::healthy([]);
        let error = coordinator.start(
            "260106173", &target(), plan.clone(), TEST_CAPTURE_MAX_BYTES, &mut io,
            |_| Err(JlinkError::new(ErrorCode::TargetRecoveryFailed, "temporary Stop unconfirmed", false)),
        ).unwrap_err();
        assert_eq!(error.code, ErrorCode::TargetRecoveryFailed);
        assert!(!error.retryable);
        let snapshot = coordinator.status_by_key(plan.capture_key(), Instant::now()).unwrap();
        assert_eq!(snapshot.state, HssRunState::Aborted);
        assert_eq!(snapshot.integrity, HssDataIntegrity::Unknown);
        assert_eq!(snapshot.failure_code, Some(ErrorCode::TargetRecoveryFailed));
        assert!(!snapshot.partial_available);
        drop(coordinator);
        let recovered = HssCoordinator::open(root.path(), "260106173").unwrap();
        assert_eq!(recovered.status_by_key(plan.capture_key(), Instant::now()).unwrap(), snapshot);
    }
'''
path = "crates/jlink-worker/src/hss.rs"
text = (root / path).read_text(encoding="utf-8")
if "fn recovery_survives_process_exit_without_destructors_in_both_start_phases()" not in text:
    if not text.rstrip().endswith("}") or "mod tests {" not in text:
        raise RuntimeError("Expected inline HSS test module")
    text = text.rstrip()[:-1] + HARD_EXIT_TESTS + "}\n"
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)

(root / "target").mkdir(exist_ok=True)
(root / "target" / "hss-repair-paths.txt").write_text(
    "\n".join(sorted(set(changed)) or [path]) + "\n", encoding="utf-8")
print("Final correction paths:", changed)
