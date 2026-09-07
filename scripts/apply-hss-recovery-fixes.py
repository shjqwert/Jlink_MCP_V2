"""Final branch-scoped correction; no target access or release action."""
from pathlib import Path
import os
import subprocess

if os.environ.get("GITHUB_REF") != "refs/heads/codex/hss-recovery-preflight-fixes":
    raise SystemExit("Dedicated repair branch required")
root = Path.cwd()
subprocess.run(["git", "fetch", "--no-tags", "--depth=1", "origin", "dfbc9f719b85e1e07d5c28bbb6e7fb4ee2bdb1ab"], check=True)
changed = []
path = "crates/jlink-worker/src/hss.rs"
text = (root / path).read_text(encoding="utf-8")
original = text
lookup = '''        let Some(root) = std::env::var_os("JLINK_TEST_HSS_HARD_EXIT_ROOT") else {
            return;
        };
        let phase = std::env::var("JLINK_TEST_HSS_HARD_EXIT_PHASE").unwrap();
'''
start = text.index("    fn recovery_child_exits_without_dropping_admission() {")
end = text.index("    #[test]", start)
block = text[start:end]
if block.count(lookup) != 1:
    raise RuntimeError("Expected the exact child environment guard")
block = block.replace(lookup, "", 1)
anchor = "        let mut coordinator =\n            HssCoordinator::open(std::path::PathBuf::from(root), \"260106173\").unwrap();"
if block.count(anchor) != 1:
    raise RuntimeError("Expected the exact child coordinator setup")
block = block.replace(anchor, lookup + anchor, 1)
text = text[:start] + block + text[end:]

# This is a live admission failure, not evidence that a restart scan occurred.
start = text.index("    fn record_start_failure(")
end = text.index("    /// Drains once", start)
block = text[start:end]
old = "            recovery_notifications: status.recovery_notifications().to_vec(),"
new = """            // No restart scan occurred when this live admission was rejected.
            recovery_notifications: Vec::new(),"""
if old in block:
    if block.count(old) != 1:
        raise RuntimeError("Ambiguous start-failure notification field")
    block = block.replace(old, new, 1)
elif new not in block:
    raise RuntimeError("Unexpected start-failure notification field")
text = text[:start] + block + text[end:]

start = text.index("    fn recovery_uncertain_preflight_cleanup_remains_aborted_unknown() {")
block = text[start:]
anchor = "        assert_eq!(snapshot.failure_code, Some(ErrorCode::TargetRecoveryFailed));"
assertion = anchor + "\n        assert!(snapshot.recovery_notifications.is_empty());"
if assertion not in block:
    if block.count(anchor) != 1:
        raise RuntimeError("Expected the uncertain-start fixture assertion")
    block = block.replace(anchor, assertion, 1)
text = text[:start] + block
text = text.replace('status_by_key("never-admitted", Instant::now())', 'status_by_key("run-fixture", Instant::now())')
if text != original:
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)

# Clarify existing wire-state documentation; the serialized state enum is unchanged.
path = "crates/jlink-domain/src/hss.rs"
text = (root / path).read_text(encoding="utf-8")
old = "    /// A prior process ended without completing the capture.\n    Aborted,"
new = "    /// Acquisition ended without a confirmed normal hardware completion.\n    Aborted,"
if old in text:
    if text.count(old) != 1:
        raise RuntimeError("Ambiguous aborted-state documentation")
    (root / path).write_text(text.replace(old, new, 1), encoding="utf-8", newline="\n")
    changed.append(path)
elif new not in text:
    raise RuntimeError("Unexpected aborted-state documentation")

(root / "target").mkdir(exist_ok=True)
(root / "target" / "hss-repair-paths.txt").write_text(
    "\n".join(changed or ["crates/jlink-worker/src/hss.rs"]) + "\n", encoding="utf-8")
print("Corrected paths:", changed)
