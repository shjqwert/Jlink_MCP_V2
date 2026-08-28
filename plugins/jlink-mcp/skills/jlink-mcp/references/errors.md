# Errors and recovery

Load this reference only after a failed call, a lost response, or an uncertain side effect.

## Read the failure

1. If the tool returned `isError: true`, read `structuredContent.error.code`, `message`, `retryable`, and any `details`. These fields are authoritative; the text item is a compact human rendering.
2. JSON-RPC errors such as an unknown tool or invalid call structure mean the MCP request itself was not accepted. Refresh the current tool definition and correct the call instead of treating it as a device failure.
3. Do not use `retryable` as permission to repeat a hardware side effect. It describes the error, while replay safety depends on whether execution was dispatched and whether an idempotency contract exists.

## Stop conditions

| Error or class | Response |
|---|---|
| `EXECUTION_UNCERTAIN` | Never replay program, write, or control. Discard the old connection state, re-establish a trustworthy session, then use a safe readback or ask the user how to resolve the unknown side effect. |
| `OPERATION_CONFLICT` | Reconcile error details with the current session snapshot; query target/HSS status only if the conflict leaves state unknown. Wait or change the explicit plan, but do not queue an operation implicitly or interrupt HSS. |
| `CURSOR_INVALID`, `CURSOR_EXPIRED` | End pagination. A new query must be an explicit restart decision; never splice pages from different snapshots. |
| `CAPTURE_KEY_CONFLICT` | Do not alter and resend under the same key. Use the original equivalent request for recovery or choose a new key for a genuinely new capture. |
| `VERIFY_FAILED`, `FIRMWARE_ELF_MISMATCH`, `FIRMWARE_IDENTITY_UNKNOWN` | Report the observed identity/verification facts. Do not continue symbol-based interpretation as if firmware identity were proven. |
| `FRAME_INVALID` | Do not interpret affected raw bytes as samples. Preserve the error and any separately verified capture data. |

## Correct before retrying

- Configuration and environment: `CONFIG_INVALID`, `DLL_NOT_FOUND`, `DLL_VERSION_MISMATCH`, `DLL_HASH_MISMATCH`, `DLL_EXPORT_MISSING`, `TARGET_CONNECT_FAILED`, `TARGET_RECOVERY_FAILED`.
- Request and model: `VALUE_INVALID`, `SYMBOL_NOT_FOUND`, `SYMBOL_AMBIGUOUS`, `TYPE_UNSUPPORTED`, `DYNAMIC_LOCATION_UNSUPPORTED`, `SLICE_REQUIRED`, `ADDRESS_OUT_OF_RANGE`, `FLASH_RANGE_INVALID`, `REGISTER_NOT_FOUND`.
- HSS capability/start: `HSS_UNSUPPORTED`, `HSS_START_FAILED`.

Use error `details` and, when relevant, `config_get`, `status`, or `symbols` to make one concrete correction. Do not cycle through speculative values, silently reduce connection speed, choose an arbitrary probe, truncate ranges, or change the user's requested operation.

The only public side-effect recovery with an explicit idempotency contract is HSS `start`: within the same MCP/Worker lifecycle, the same `capture_key` and semantically equivalent request recover the existing capture. A new lifecycle or changed request requires a new key.
