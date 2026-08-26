# Phase 0 Experiments

This workspace contains disposable feasibility probes for the active
`define-jlink-mcp-v1` OpenSpec change. It is not the production workspace and
must not be used as implementation evidence for P1 or later stages.

Each experiment emits machine-readable evidence under the ignored
`validation/evidence/` tree. Hardware evidence is valid only for the complete
fingerprint recorded by `validation/requirement-matrix.md`.

- `f0a-hss` validates the J-Link HSS ABI, target behavior, timestamp semantics,
  write interleaving, and Capture Store candidate.
- `f0b-worker` validates Windows named-pipe reattachment, parent-exit survival,
  probe leases, capture-key idempotency, and partial-file recovery.
- `f0c-dwarf` validates IAR 8.32 DWARF composite types and fixed access plans
  against an isolated linked fixture.
- `f0d-mcp` validates the six-tool stdio MCP contract with a mock server and
  client self-test.
