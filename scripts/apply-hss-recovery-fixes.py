"""Branch-only, exact-anchor follow-ups for the approved source repair."""
from pathlib import Path
import os
import subprocess

BRANCH = "refs/heads/codex/hss-recovery-preflight-fixes"
BASE = "dfbc9f719b85e1e07d5c28bbb6e7fb4ee2bdb1ab"
if os.environ.get("GITHUB_REF") != BRANCH:
    raise SystemExit("This repair is restricted to its dedicated branch")
root = Path.cwd()
subprocess.run(["git", "fetch", "--no-tags", "--depth=1", "origin", BASE], check=True)
if not (root / "crates/jlink-worker/src/hss_start_journal.rs").exists():
    raise SystemExit("The reviewed source repair must already be present")
changed = []

def write(path, text):
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)

def replace(path, before, after):
    text = (root / path).read_text(encoding="utf-8")
    if after and after in text:
        return
    if text.count(before) != 1:
        raise RuntimeError(f"Source anchor mismatch in {path}: {before[:120]!r}")
    write(path, text.replace(before, after, 1))

replace("crates/jlink-worker/src/hss.rs",
    "    fn recovery_preflight_has_durable_identity_before_any_native_work() {\n        use std::panic",
    "    fn recovery_preflight_has_durable_identity_before_any_native_work() {\n        use jlink_capture::{CaptureRecovery, CaptureStore};\n        use std::panic")
replace("crates/jlink-worker/src/hss.rs", 'status_by_key("never-admitted", Instant::now())', 'status_by_key("run-fixture", Instant::now())')

path = "scripts/invoke-verified-build.ps1"
text = (root / path).read_text(encoding="utf-8")
command = "$command = Get-Command -Name $FilePath -CommandType Application -ErrorAction Stop | Select-Object -First 1\n"
revoke = "if (Test-Path -LiteralPath $receipt) { Remove-Item -LiteralPath $receipt -Force }\n"
if text.count(command) not in (1, 2) or text.count(revoke) != 1:
    raise RuntimeError("Unexpected build gate command/receipt order")
corrected = text.replace(command, "").replace(revoke, revoke + command, 1)
if corrected != text:
    write(path, corrected)

replace("scripts/check-firmware-workflow.ps1", "    $noop = Join-Path $root 'noop.ps1'\n", """    [IO.File]::WriteAllText($receipt, '{"build_succeeded":true}')
    Assert-Throws {
        & $build -FilePath (Join-Path $root 'missing-compiler.exe') -ArtifactPath $artifact -OnSuccess $followup
    } 'An unresolved compiler did not stop dependent work'
    Assert-True (-not (Test-Path -LiteralPath $receipt)) 'Old success receipt survived compiler lookup failure'
    Assert-True (-not (Test-Path -LiteralPath $marker)) 'Dependent stage ran without a compiler'

    $noop = Join-Path $root 'noop.ps1'
""")

(root / "target").mkdir(exist_ok=True)
(root / "target" / "hss-repair-paths.txt").write_text(
    "\n".join(sorted(set(changed)) or ["crates/jlink-worker/src/hss.rs"]) + "\n", encoding="utf-8")
print("Follow-up paths:", changed)
