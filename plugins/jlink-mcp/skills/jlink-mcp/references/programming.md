# Firmware programming

Use this reference for `jlink_program`. Programming and erase are hardware side effects; confirm that the user's request authorizes them before calling.

All three actions require the current MCP/Worker lifecycle to own the connected, validated target session. Reuse a trustworthy session established earlier in the same workflow; do not add a `status` call before each programming action. If state is unknown or invalidated, reconcile it before proceeding. Establishing a missing connection can itself resume or reset-plus-resume the CPU, as described in [target-session.md](target-session.md).

## Flash

- `after` is always explicit: `none`, `reset_halt`, or `reset_run`. Do not infer it from the user's usual workflow.
- `after: none` suppresses only the successful post-operation transition. The fixed pre-download `reset_halt` preparation still occurs.
- `image` may come from project configuration. If the resolved image is raw BIN, every request still needs an explicit hexadecimal `base_address`.
- ELF/AXF/OUT, Intel HEX, and S-record images carry their own addresses and reject `base_address`.
- Verification defaults to enabled only because the runtime Schema/implementation declares it; set `verify` explicitly when the user's request differs.
- A successful result is `{}`. Do not expect programmed byte counts or repeat the call because the payload is empty.

## Erase

- Choose whole-chip erase by omitting both range fields, or range erase by providing both address and length. Never provide just one.
- The range must remain inside known Flash. Do not split or clamp an invalid range on the user's behalf.
- `after` is required for whole-chip and range erase.

## Independent verify

- Verify is read-only with respect to Flash and has no `after` field. Do not reset, halt, or resume implicitly around it.
- Raw BIN uses the same explicit base-address rule as flash; self-describing images reject a supplied base address.
- `VERIFY_FAILED` means comparison completed and found a difference; report the first confirmed mismatch region and mismatch count from error details without dumping target contents.

## Boundaries

- Never use `jlink_write.memory` for Flash; use this tool so device Flash algorithms and boundaries are enforced.
- All programming actions conflict with active HSS and must fail immediately rather than queue for later.
- After any Flash modification, prior firmware/ELF validation evidence is stale.
- On `EXECUTION_UNCERTAIN`, do not repeat flash or erase. Read [errors.md](errors.md) and re-establish trustworthy state first.
