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
persisted data. A single live value routes to `inspect`; repeated samples,
transitions, duration, or high-rate observation routes to `hss.plan` then `start`.
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
  needs a new key.

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
