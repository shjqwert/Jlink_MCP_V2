"""Apply only the reviewed source transformations on the dedicated branch."""
from pathlib import Path
import hashlib
import os
import runpy
import subprocess

BRANCH = "refs/heads/codex/hss-recovery-preflight-fixes"
BASE = "dfbc9f719b85e1e07d5c28bbb6e7fb4ee2bdb1ab"
if os.environ.get("GITHUB_REF") != BRANCH:
    raise SystemExit("This one-shot repair is restricted to its dedicated GitHub Actions branch")
root = Path.cwd()
subprocess.run(["git", "fetch", "--no-tags", "--depth=1", "origin", BASE], check=True)

def read_reviewed_blob(root, path, expected_sha):
    data = subprocess.check_output(["git", "show", f"HEAD:{path}"], cwd=root)
    actual = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
    if actual != expected_sha:
        raise RuntimeError(f"Unexpected canonical baseline for {path}: {actual}")
    text = data.decode("utf-8")
    if (root / path).read_text(encoding="utf-8") != text.replace("\r\n", "\n"):
        raise RuntimeError(f"Unreviewed working-tree content in {path}")
    return text

changed = []
if not (root / "crates/jlink-worker/src/hss_start_journal.rs").exists():
    for name in ("repair-hss-source.py", "repair-mcp-source.py", "repair-workflow-source.py"):
        namespace = runpy.run_path(str(root / "scripts" / name))
        namespace["apply"].__globals__["read_base"] = read_reviewed_blob
        changed.extend(namespace["apply"](root))

def rewrite(path, before, after):
    text = (root / path).read_text(encoding="utf-8")
    if after in text:
        return
    if text.count(before) != 1:
        raise RuntimeError(f"Expected a unique follow-up anchor in {path}: {before[:100]!r}")
    (root / path).write_text(text.replace(before, after, 1), encoding="utf-8", newline="\n")
    changed.append(path)

# These transaction/readiness methods intentionally keep one ordered boundary.
rewrite("crates/jlink-worker/src/hss.rs", "    pub(crate) fn start<I, F>(", "    #[allow(clippy::too_many_lines)]\n    pub(crate) fn start<I, F>(")
rewrite("crates/jlink-mcp/src/config_readiness.rs", "    pub(crate) fn refresh_operation_readiness(&mut self)", "    #[allow(clippy::too_many_lines)]\n    pub(crate) fn refresh_operation_readiness(&mut self)")

# Keep independent local test scripts ignored. This public, hardware-free check
# is part of the shipped helper source gate, alongside check-workspace.ps1.
old_check = "scripts/test-firmware-workflow.ps1"
new_check = "scripts/check-firmware-workflow.ps1"
if (root / old_check).exists():
    (root / old_check).rename(root / new_check)
    changed = [new_check if path == old_check else path for path in changed]

# Exact legacy and helper-bearing payload sets remain supported. Every listed
# file still requires its size/hash check; a partial helper pair is rejected.
rewrite("scripts/release-common.ps1", "function Get-ReleasePayloadPaths {\n    @(", "function Get-ReleasePayloadPaths {\n    param([switch]$Legacy)\n    $paths = @(")
rewrite("scripts/release-common.ps1", """    )
}

function Get-ContainedPath""", """    )
    if ($Legacy) {
        $paths | Where-Object { $_ -notin @('scripts/new-firmware-identity.ps1', 'scripts/invoke-verified-build.ps1') }
    } else { $paths }
}

function Get-ContainedPath""")
rewrite("scripts/release-common.ps1", "    $expected = @(Get-ReleasePayloadPaths)\n", """    $helperPaths = @('scripts/new-firmware-identity.ps1', 'scripts/invoke-verified-build.ps1')
    $hasWorkflowHelpers = @($manifest.files | Where-Object { $helperPaths -contains $_.path }).Count -gt 0
    $expected = @(Get-ReleasePayloadPaths -Legacy:(-not $hasWorkflowHelpers))
""")
rewrite(new_check, "    Write-Host 'PASS: JLID generation", """    $legacy = @(Get-ReleasePayloadPaths -Legacy)
    Assert-True ($payload.Count -eq $legacy.Count + 2) 'Legacy package shape must remain an exact closed set'
    Assert-True ($legacy -notcontains 'scripts/new-firmware-identity.ps1') 'Legacy package incorrectly requires a new helper'
    Assert-True ($legacy -contains 'bin/jlink-worker.exe') 'Legacy compatibility removed a core payload requirement'
    Write-Host 'PASS: JLID generation""")

# Revoke the old receipt even when the requested compiler cannot be resolved.
rewrite("scripts/invoke-verified-build.ps1", "$command = Get-Command -Name $FilePath -CommandType Application -ErrorAction Stop | Select-Object -First 1\n", "")
rewrite("scripts/invoke-verified-build.ps1", "if (Test-Path -LiteralPath $receipt) { Remove-Item -LiteralPath $receipt -Force }\n", "if (Test-Path -LiteralPath $receipt) { Remove-Item -LiteralPath $receipt -Force }\n$command = Get-Command -Name $FilePath -CommandType Application -ErrorAction Stop | Select-Object -First 1\n")

path = "scripts/check-workspace.ps1"
text = (root / path).read_text(encoding="utf-8")
if "check-firmware-workflow.ps1" not in text:
    text += '\n# Pure helper checks use the minimum supported PowerShell runtime; no probe access.\n& powershell.exe -NoLogo -NoProfile -File "$PSScriptRoot\\check-firmware-workflow.ps1"\nif ($LASTEXITCODE -ne 0) { throw "Firmware workflow checks failed" }\n'
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)

changed = sorted(set(changed))
if not changed:
    # A validation-only retry may stage already tracked source paths harmlessly.
    changed = ["crates/jlink-worker/src/hss.rs"]
(root / "target").mkdir(exist_ok=True)
(root / "target" / "hss-repair-paths.txt").write_text("\n".join(changed) + "\n", encoding="utf-8")
print("Applied baseline-checked changes to:")
for path in changed:
    print(path)
