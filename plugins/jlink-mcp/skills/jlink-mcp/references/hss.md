# Fixed-duration HSS

Use this reference for `jlink_hss`. The tool exposes `start`, `status`, and `query`; V1 has no stop or cancel action.

## Start

- Start requires a connected, validated session whose trusted CPU state is running. Reuse current-lifecycle state; do not query target `status` again when it is already known. Do not resume a halted CPU unless the user authorized that state change.
- Provide a new `capture_key`, a duration from 1 to 300 seconds, a requested rate from 1 to 1000 Hz, 1–10 top-level DWARF selectors, and explicit `return_when`.
- Selectors use static DWARF paths, never raw addresses. Structures and fixed arrays expand before capture; flexible arrays need a slice. Pointers are captured only as address values.
- The 10-item limit counts submitted top-level selectors, not expanded leaves.
- A requested rate is a target, not a guarantee of the actual rate or losslessness.
- The Worker stops automatically at the fixed duration, drains the tail, and persists the terminal capture. Do not simulate cancel with disconnect or process termination.

## Capture-key recovery

- Within the same MCP/Worker lifecycle, repeat `start` only when the original response was lost and the same key plus semantically equivalent normalized request can be reproduced. This recovers the same capture.
- Reusing a key with changed parameters is a conflict, not a new capture request.
- After MCP/Worker restart, use a new key. An unfinished old capture can only become `aborted + unknown` or be safely cleaned; a new process cannot take it over.

## Status and query identity

- Status and query for an existing persisted capture do not require an active target connection. Do not connect merely to read capture state or data.
- Every status or query supplies exactly one of `capture_id` and `capture_key`.
- Use the four fixed query views: `overview`, `changes`, `window`, and `around_event`. The live tool Schema and description own their exact flat fields; never create `query` or `resolution` wrapper objects.
- When a page returns `next_cursor`, continuation contains only `action: query`, the same one capture identity, and `cursor`. Omit `view` and every view-specific field.
- `CURSOR_INVALID` or `CURSOR_EXPIRED` ends that pagination chain. Do not silently restart from page one; ask for or make an explicit new-query decision.

## Interpret results without inventing certainty

- Lifecycle (`starting`, `running`, `stopping`, `completed`, `failed`, `aborted`) and integrity (`complete`, `degraded`, `unknown`) are independent. `completed` can still be degraded or unknown.
- Unknown loss or overflow evidence is not zero. Compare requested rate with actual sample counts and interval statistics before describing achieved performance.
- `changes` are observed value changes. A change occurred after `after_us` and by `observed_by_us`; neither bound is an exact change instant.
- `matches` are declared-rule matches, not additional raw changes. `relations` are only `before`, `after`, `overlaps`, or `indeterminate` across stated clock uncertainty; none proves causality.
- A raw window retains all available ordered values, including repeats, across pages. `min_max`, `first_last`, and `transitions` are explicit modes and must not be described as raw samples.
- Quality problems remain data facts when readable data exists; do not discard the capture merely because it is degraded.
- A `failed` terminal state reports `failure_code` and `partial_available`; an `aborted` state reports `reason`, `recoverable`, and `partial_available`. These fields describe capture recovery evidence, not permission to restart it.
- When `partial_available` is true, query may still return the already validated portion with abnormal quality made explicit. When false, do not invent samples or treat the failed capture as completed.

Completed raw resources are immutable, self-describing binary resources and remain readable without a live target session. The server returns data, not generated charts; visualization and diagnosis belong to the client or Agent.
