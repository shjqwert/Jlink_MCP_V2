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

1. Use only the current `jlink_mcp` server. Choose one declared `action` and send the smallest object accepted by its live strict Schema; do not invent wrappers, aliases, defaults, or extra fields.
2. Reuse trustworthy state established in the current MCP/Worker lifecycle. Do not insert target `status` between consecutive operations; query only when state is unknown, invalidated, or contradicted. Earlier tasks, UI state, and configuration do not prove a live connection.
3. Successful `structuredContent` is authoritative and `{}` means completed success. On failure, load `errors.md` and use `structuredContent.error`; never replay an uncertain side effect automatically.
4. Offline operations must not connect. A necessary live connection can resume or reset-plus-resume the CPU, so follow `target-session.md` and report notices; a read request never authorizes programming, writes, or mutating control.
5. During active HSS, only target `status`, HSS `status/query`, and variable or RAM/MMIO writes are allowed. Other device operations conflict; accepted writes are serialized with capture drain.

Current release evidence covers Windows x64 and SWD. The Schema can represent JTAG, but JTAG has not passed the hardware release gate; disclose that limitation instead of claiming verified support.
