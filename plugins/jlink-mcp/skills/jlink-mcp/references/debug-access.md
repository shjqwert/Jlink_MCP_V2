# Debug access

Use this reference for `jlink_inspect`, `jlink_write`, and `jlink_control`.

## Symbols and variables

- `jlink_inspect.symbols` searches the configured local ELF and needs no target connection. Do not connect merely to discover a path.
- Memory, variable, and register access plus control require the current MCP/Worker lifecycle to own the connected, validated target session. Reuse trustworthy state from earlier successful calls; query target `status` only after the state becomes unknown, invalid, or contradictory.
- When an exact DWARF path is unknown, call `jlink_inspect.symbols`; use one returned path directly instead of inventing spelling or addresses.
- Variable paths cover static variables, members, fixed arrays, array elements, bitfields, and supported compound values.
- Flexible or dynamic-length arrays require an independent `slice {start,count}`. A path index does not replace the slice, including for one element.
- Pointers are unsigned address values. V1 never dereferences or follows them, and dynamic location expressions can be rejected.
- A compound write validates the complete selected object before its first target write. To update one member, select that member path. A union write identifies exactly one member.

## Typed values

- Ordinary booleans, safe integers, finite floats, structures, and fixed arrays use their direct JSON forms.
- Large integers use `$int` with decimal text plus `bits` and `signed`; non-finite floats use `$float` with `nan`, `inf`, or `-inf`; pointers use `$pointer` with a hexadecimal address.
- An unspecified union read uses `$union` to expose candidate members. A union write still selects exactly one member; do not treat tagged objects as arbitrary user-defined JSON.

## Raw memory

- A single read or write covers 1–4096 bytes. Do not silently split, truncate, or widen a request.
- Raw writes support RAM and MMIO. Flash addresses are rejected and must be routed to `jlink_program`.
- Treat returned memory `data` as exact hexadecimal bytes for the requested range. Do not reinterpret endianness or type unless the user supplies that model.

## Write verification

- Variable and memory writes default to no readback. Request `readback` only when the user needs verification and accepts the extra device access.
- Readback proves only that the same J-Link connection observed the requested value. It does not prove firmware behavior, consumption, persistence, or correctness.
- A successful write returns `{}`. An empty result is not permission to issue a second write.
- For an uncertain response, follow [errors.md](errors.md); never retry a register, memory, or variable write automatically.

## Registers and control

- Register names are case-sensitive canonical names: `R0`–`R12`, `SP`, `LR`, `PC`, `XPSR`, `MSP`, `PSP`, `APSR`, `EPSR`, `IPSR`, `PRIMASK`, `BASEPRI`, `FAULTMASK`, and `CONTROL`, intersected with the target's actual catalog.
- Do not substitute `R13`, `R14`, or `R15` for `SP`, `LR`, or `PC`.
- `XPSR`, `EPSR`, and `IPSR` are read-only views; never attempt to write them.
- `reset` always has explicit `after: run` or `after: halt`.
- `step` requires a trustworthy halted state and remains halted after one instruction. Do not implicitly halt a running target; call `halt` only when the user authorized that state change.
- After a successful `halt`, `resume`, `reset`, or `step`, update the trusted CPU snapshot to halted, running, the explicit reset `after` state, or halted respectively; no follow-up `status` is required unless another event invalidates it.

## Active HSS

During HSS, ordinary reads, register access, and target control conflict. Only variable writes and RAM/MMIO writes are accepted; the Worker serializes them between buffer drains and records their timing and quality impact.
