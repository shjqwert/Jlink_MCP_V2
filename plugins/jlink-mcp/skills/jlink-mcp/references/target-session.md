# Target session

Use this reference for `jlink_target`.

## Select the action

| Need | Action | Semantic constraint |
|---|---|---|
| Inspect effective settings and their source | `config_get` | Read-only and independent of a live connection. Use it before guessing why connection inputs differ. |
| Persist a partial setting update | `config_set` | Allowed only while disconnected and with no active HSS. Project and user scopes accept different fields; follow the action Schema. |
| Establish the single target session | `connect` | Requires a concrete device and an unambiguous probe selection. It can change CPU state as described below. |
| Observe state | `status` | During HSS this is cached/observed state and must not be treated as a fresh DLL read. |
| Verify the environment | `validate` | The `after` rule depends on connection state; see below. |
| Release the session | `disconnect` | Rejected during active HSS; never stop a capture by disconnecting. |

## Validate correctly

- When disconnected, include `after: run` or `after: halt`. Validation may create a temporary target session and must close it in the explicitly requested state.
- When connected, omit `after`. Validation only observes the active session and must not change its state.
- If the current connection state is not trustworthy, call `status` first rather than guessing which validate shape applies.
- A validation result reports checks actually performed. Do not convert a partial or failed check list into a general statement that the target is ready.

## Configuration and connection

- Use `config_get` to inspect `effective` values and `sources`; ordinary calls intentionally omit this diagnostic map.
- `config_set` is an atomic partial update, not a per-call override. Do not retry it after an uncertain filesystem or process failure without reading the effective configuration again.
- Use a concrete J-Link device name. A generic Cortex-M core name is not evidence that the device-specific command was accepted.
- `connect` owns at most one probe and target. Disconnect before intentionally switching target identity.
- A successful `connect` leaves the target running. If the target was halted, it resumes it and returns `notices: ["resumed_from_halt"]`; if recovery found HardFault, it performs reset plus resume and returns `notices: ["reset_after_fault"]`. Preserve and report these notices.
- Configuration, local-symbol, and persisted-capture reads are offline and must not connect. A request to read live target memory, variables, registers, or Flash verification may establish a missing connection as its necessary prerequisite, but report any recovery notice. If the user requires preserving the pre-connect CPU state, stop and explain that this contract cannot guarantee a non-disturbing new connection.
- UI state, an earlier Codex task, or configuration files do not prove that the current Worker still owns a connection.

## Reuse current-session state

- Reuse trustworthy connection, CPU, validation, and HSS state established by successful calls in the current MCP/Worker lifecycle. Successful control actions update that state according to their declared transition.
- Do not insert `status` between consecutive calls merely to reconfirm state. Query it when the state is unknown, a Worker/lifecycle changed, an uncertain result invalidated the snapshot, or a returned conflict contradicts it.
- `config_get` and target `status` need no live target. Other references identify additional offline operations.

## State and release evidence

An active HSS capture rejects disconnect and all target actions except `status`. Current release evidence covers SWD only. If a request selects JTAG, state that it is Schema-supported but not hardware-release-verified in this version.
