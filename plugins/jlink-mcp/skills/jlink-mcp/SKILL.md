---
name: jlink-mcp
description: Use the jlink_mcp MCP server to configure, connect, program, inspect, write, control, or capture data from one local SEGGER J-Link ARM Cortex-M target. Trigger for explicit J-Link MCP operations and requests that require its fixed-duration HSS capture data; do not trigger for general embedded advice, unrelated probes, GDB, RTT, or arbitrary J-Link Commander work.
---

# J-Link MCP

Operate the fixed six-tool V1 contract. Use the current `jlink_mcp` tool definitions as the sole syntax authority; this skill adds routing, state rules, result semantics, and failure handling.

## Route the request

| Intent | Tool and actions | Read first |
|---|---|---|
| Configure or manage the target session | `jlink_target`: `config_get`, `config_set`, `connect`, `disconnect`, `status`, `validate` | [target-session.md](references/target-session.md) |
| Program or verify Flash | `jlink_program`: `flash`, `erase`, `verify` | [programming.md](references/programming.md) |
| Read or discover values | `jlink_inspect`: `variable`, `memory`, `register`, `symbols` | [debug-access.md](references/debug-access.md) |
| Write values or control execution | `jlink_write`: `variable`, `memory`, `register`; `jlink_control`: `halt`, `resume`, `reset`, `step` | [debug-access.md](references/debug-access.md) |
| Capture or query high-speed data | `jlink_hss`: `start`, `status`, `query` | [hss.md](references/hss.md) |

Read only the relevant business reference. Read [errors.md](references/errors.md) after a non-success result, a lost response, or an uncertain side effect; do not load it for an ordinary successful call.

## Invariants for every call

1. Use only tools exposed by the `jlink_mcp` server. Do not substitute another installed server with similar tool names.
2. Inspect the current tool definition, choose one declared `action`, and send the smallest object that satisfies that action's strict Schema. Do not invent wrappers, aliases, defaults, or extra fields.
3. Within the current MCP/Worker lifecycle, reuse trustworthy session state established by successful calls and their declared transitions; do not mechanically call target `status` between consecutive operations. Query `status` only when state is unknown, invalidated, or contradicted by a result. A previous task, window, UI, or persistent configuration never proves a live connection.
4. Treat successful `structuredContent` as authoritative. An empty `{}` is a completed success with no payload; it is not missing data. Treat `structuredContent.error` as the authoritative tool failure.
5. A live-target read may establish a missing connection as a necessary prerequisite, but `connect` can resume or reset-plus-resume the CPU and its notices must be reported. If the user requires preserving the pre-connect CPU state, stop instead. Offline reads must not connect, and no read-only request authorizes `jlink_program`, `jlink_write`, or mutating `jlink_control` actions.
6. Never automatically repeat an uncertain side effect. Apply the recovery rules in `errors.md`.
7. During an active HSS capture, only target `status`, HSS `status/query`, and variable or RAM/MMIO writes are allowed. Other device operations conflict; allowed writes are serialized with capture drain rather than executed concurrently in the DLL.
8. Interpret measurements as evidence, not diagnosis. The MCP projects deterministic facts; causal analysis remains an Agent conclusion and must retain uncertainty.

Current release evidence covers Windows x64 and SWD. The Schema can represent JTAG, but JTAG has not passed the hardware release gate; disclose that limitation instead of claiming verified support.
