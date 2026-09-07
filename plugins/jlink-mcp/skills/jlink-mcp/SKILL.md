---
name: jlink-mcp
description: Use jlink_mcp with one local SEGGER J-Link ARM Cortex-M target for configuration, programming, inspection, writes, control, and high-speed capture. Applies to J-Link MCP operations and debugging needing repeated/time-series data; not general embedded advice, GDB, RTT, unrelated probes, or J-Link Commander.
---

# J-Link MCP

## Device ownership and memory

Use one named logical device operator for the current target: the primary session
or one explicitly assigned child. All other agents refrain from J-Link calls,
including reads. Code implementation ownership does not grant device ownership.
Transfer ownership only after the previous operator has stopped issuing calls.
An active capture or an unfinished control operation remains owned by that operator
until it reaches a known terminal state or an explicit handoff identifies the next
operator and the exact live state. Existing worker serialization and probe
exclusivity remain in force.

Use live variables, registers and HSS samples only within the active debugging
task. Do not put these snapshots, samples or capture references into Handoff or
RAG. Firmware and code changes invalidate earlier runtime assumptions; re-observe
the current target when needed. Existing capture, persisted-sample queries and
export features remain available to the current operator.

Operate the fixed six-tool V1 contract. Live tool definitions are the sole syntax
authority; this self-contained Skill supplies routing, lifecycle state, result
semantics, and recovery. Do not load runtime references or recreate a Schema here.

## Preparation before test-firmware changes

Freeze the project's verified DLL path, exact file version and SHA-256 before
connecting. Use the existing project/session identity checks; do not silently
switch versions or hard-code one SEGGER version for every device. For a project
whose agreed baseline is 6.98a, retain that exact DLL throughout the test. Ordinary
reads succeeding does not attest to HSS compatibility.

Read `target.config_get` and its action-specific `readiness` first. A
`static_ready` value is configuration evidence, not successful live validation;
resolve `missing` fields and retain `pending_checks`. Flash/erase need loader RAM;
verify and ordinary reads do not acquire that requirement. Before HSS-dependent
firmware design, budget the combined payload and run offline `hss.plan` once the
ELF/selectors exist. The current payload limit is 40 bytes excluding the timestamp.
Do not start HSS just to discover its static limits.

Before programming, explicitly `target.connect` to establish the retained,
validated session. A successful standalone `target.validate` is not that session:
while disconnected it requires `after: run` or `halt`, performs a temporary
diagnostic connection, and disconnects afterward. During an existing connection,
omit `after` when validating; successful connect evidence can be reused.

For DWARF writes/HSS, the final ELF must retain `__jlink_mcp_identity` in loaded,
read-only Flash. Its bytes are `JLID`, format byte `1`, one-byte ASCII build-ID
length N, then N printable ASCII bytes; N is 1..58 and total size is at most 64.
Use `scripts/new-firmware-identity.ps1` from the complete package to generate the
array without a hand-maintained length. GCC/Clang section GC also needs the linker
KEEP rule stated by that generator. Change the build ID with firmware changes;
reusing a demonstration ID does not prove that two different builds match.
Raw-address HSS instead requires explicit type/range and declared readable RAM;
it must not be silently relabelled as DWARF evidence.

A failed compiler/linker result ends its dependency chain immediately: do not
prepare, copy, flash or test an old OUT as the new build. The packaged
`scripts/invoke-verified-build.ps1` can run the approved native build command,
require a fresh nonempty output, and invoke an explicitly supplied dependent step
only after success. Its receipt binds that output path and SHA-256, not arbitrary
future files. Recheck the hash before later use; independent MCP calls are not
magically gated by the receipt. Use an explicit rebuild/new output path when
incremental builds leave the previous artifact unchanged. Deliberate rollback to
an identified old image is a separate authorized task, not a failed-build fallback.

## Route precisely

| Intent | Tool and actions | Boundary |
|---|---|---|
| Configure/connect/target | `jlink_target`: `config_get`, `config_set`, `connect`, `disconnect`, `status`, `validate` | Config is offline; connect can change CPU state. |
| Flash/erase/compare | `jlink_program`: `flash`, `erase`, `verify` | Flash uses this path; verify does not reset. |
| Symbols/live reads | `jlink_inspect`: `symbols`, `variable`, `memory`, `register` | Symbols are offline; other reads need a live session. |
| Write a value | `jlink_write`: `variable`, `memory`, `register` | `variable`/`memory` may request `verify: readback`; never write Flash here. |
| CPU execution | `jlink_control`: `halt`, `resume`, `reset`, `step` | Use explicit transitions; no implicit halt/reset. |
| Plan/capture/query high-speed data | `jlink_hss`: `plan`, `start`, `status`, `query` | Plan is offline; start persists a fixed capture. |

Use `inspect.symbols` when an ELF or DWARF path is unknown, and `hss.query` for
persisted data. Choose the least invasive evidence that meets acceptance: a
latched result, counter, completion flag or bounded target-run result routes to
`inspect`, even when the test performed many internal transitions. Use `hss.plan`
then `start` only when continuous samples, ordering or transient timing evidence
is actually needed. Software latches do not by themselves prove pin waveforms.
HSS has no stop action. Prefer `return_when: started` for a capture expected to
outlast a normal tool turn, then use `status` and `query`; reserve
`return_when: completed` for a short capture that fits the current tool wait. The
same named operator owns the capture through its terminal state or explicit handoff.
When an authorized stimulus must occur during capture, use `return_when: started`
and have that operator issue the allowed serialized write after start succeeds.

`target.status` reports connection/CPU; `hss.status` reports capture lifecycle and
quality. Neither replaces the other. `program.verify` compares image to Flash;
`readback` is a `jlink_write.variable`/`memory` verify mode, not an action or a
follow-up inspect call. Inspect is current state; HSS query is historical data.

## Call and state invariants

1. Before every call, use the current live Schema and send the smallest accepted
   object. Fields, types, defaults, enums, and ranges never come from this Skill.
   On a parameter error, stop a batch immediately; do not guess, truncate,
   substitute, or continue. Preserve returned field path, rule/range, and value.
2. Reuse trustworthy target, CPU, validation, and HSS state from successful calls
   in the current MCP/Worker lifecycle. Do not add `target.status` between
   consecutive operations merely to reconfirm state. Query it after reconnect,
   an uncertain result, invalidation, or contradiction. UI state, another task,
   and configuration files do not prove a live connection.
3. Offline configuration, symbol lookup, HSS planning, and persisted-capture
   queries must not connect. A live read may establish its required session and
   must report any resume/reset notice; a read never authorizes a write, program,
   erase, or control action. Keep implicit Skill invocation enabled for routine
   debugging and apply this routing automatically.
4. During active HSS, only target status, HSS status/query, and serialized variable
   or RAM/MMIO writes are allowed. Programming, erase, ordinary reads, register
   access, control, and disconnect conflict; never queue them or use disconnect
   to cancel capture.

## Side effects and recovery

- Treat `{}` as successful completion, not missing output and not permission to
  repeat. Flash, erase, writes, control, and a new connection can change hardware.
- On failure or a lost response, inspect `structuredContent.error` (code, message,
  retryability, and details). If execution or side effects are uncertain, never
  replay program/write/control; reconcile state and obtain safe read-only evidence
  or ask how to proceed. An HSS `start` may recover only with the same key and an
  equivalent request in the same lifecycle. A new lifecycle or changed request
  needs a new key. Historical `status` can still use the original key after a
  Worker restart; this lookup never restarts acquisition. Preserve aborted/unknown
  outcomes and last recorded boundaries without inferring unobserved hardware results.

## HSS evidence and pagination

`hss.plan` expands selectors without occupying the probe and returns size/reduction
guidance; `start` uses the same planner. Raw-address selectors are explicit
address/type/length/endianness evidence restricted to declared readable RAM. They
are not DWARF variables and must not receive symbol semantics. DWARF selectors
require strong firmware identity. Requested rate is not achieved rate: report
actual samples and quality fields. Without independent overflow/sequence evidence,
never claim that no samples were lost.
`actual_rate_millihz` is derived from source timestamps, not an independent host
rate measurement. `source_host_clock_mismatch` invalidates period/runtime use:
preserve the raw records and both clocks; do not truncate or rescale to match the
requested duration.

For HSS `status`/`query`, provide exactly one capture identity. A continuation uses
the same identity and cursor with `action: query`, omitting prior view-specific
fields. `CURSOR_INVALID` and `CURSOR_EXPIRED` end that chain; never silently restart
page one. Lifecycle and integrity/quality are independent facts, so preserve
degraded or unknown evidence.

Release evidence covers Windows x64/SWD; JTAG is Schema-supported, not
hardware-release-verified.
