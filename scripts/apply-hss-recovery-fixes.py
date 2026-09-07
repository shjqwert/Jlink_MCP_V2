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
# The checkout is shallow; retain the reviewed base for the final source diff.
subprocess.run(["git", "fetch", "--no-tags", "--depth=1", "origin", BASE], check=True)

def read_reviewed_blob(root, path, expected_sha):
    # Git for Windows can check LF blobs out as CRLF. Verify the canonical blob,
    # and independently reject working-tree content changes beyond line endings.
    data = subprocess.check_output(["git", "show", f"HEAD:{path}"], cwd=root)
    actual = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
    if actual != expected_sha:
        raise RuntimeError(f"Unexpected canonical baseline for {path}: {actual}")
    text = data.decode("utf-8")
    if (root / path).read_text(encoding="utf-8") != text.replace("\r\n", "\n"):
        raise RuntimeError(f"Unreviewed working-tree content in {path}")
    return text

changed = []
for name in ("repair-hss-source.py", "repair-mcp-source.py", "repair-workflow-source.py"):
    namespace = runpy.run_path(str(root / "scripts" / name))
    namespace["apply"].__globals__["read_base"] = read_reviewed_blob
    changed.extend(namespace["apply"](root))
if len(changed) != len(set(changed)):
    raise SystemExit("A repair path was modified by more than one transformation")
(root / "target").mkdir(exist_ok=True)
(root / "target" / "hss-repair-paths.txt").write_text("\n".join(changed) + "\n", encoding="utf-8")
print("Applied baseline-checked changes to:")
for path in changed:
    print(path)
