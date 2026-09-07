"""Apply only the reviewed source transformations on the dedicated branch."""
from pathlib import Path
import os
import runpy

BRANCH = "refs/heads/codex/hss-recovery-preflight-fixes"
if os.environ.get("GITHUB_REF") != BRANCH:
    raise SystemExit("This one-shot repair is restricted to its dedicated GitHub Actions branch")
root = Path.cwd()
changed = []
for name in ("repair-hss-source.py", "repair-mcp-source.py", "repair-workflow-source.py"):
    namespace = runpy.run_path(str(root / "scripts" / name))
    changed.extend(namespace["apply"](root))
if len(changed) != len(set(changed)):
    raise SystemExit("A repair path was modified by more than one transformation")
(root / "target").mkdir(exist_ok=True)
(root / "target" / "hss-repair-paths.txt").write_text("\n".join(changed) + "\n", encoding="utf-8")
print("Applied baseline-checked changes to:")
for path in changed:
    print(path)
