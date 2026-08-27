# V1 Requirement Verification Matrix

This matrix assigns exactly one primary test to every V1 requirement. Phase 0
packages are feasibility prerequisites, not duplicate primary tests. A later
stage may reuse Phase 0 evidence only while every listed fingerprint remains
unchanged.

## Evidence fingerprints

| Evidence class | Required fingerprint | Invalidate when |
|---|---|---|
| Software | Git commit, source-tree status, Rust toolchain and target triple, test binary SHA-256 | Source, compiler, target, feature set, or test binary changes |
| J-Link hardware | DLL path/version/SHA-256, probe model/serial/firmware, target device, interface/speed, firmware and ELF SHA-256, OS build | Any DLL, probe, target, connection, firmware, ELF, OS, or relevant requirement changes |
| DWARF fixture | Compiler/version, build configuration, fixture source SHA-256, ELF SHA-256, parser/test binary SHA-256 | Compiler, flags, source, ELF, parser, or relevant requirement changes |
| MCP client | Windows Codex application package/CLI version, OS build, mock/product server SHA-256, protocol and Schema SHA-256 | Client, OS, server, protocol, Schema, or relevant requirement changes |
| Local process/storage | OS build, executable SHA-256, IPC version, capture-format version, test parameters | OS, executable, IPC, format, parameters, or relevant requirement changes |

Evidence records MUST include start/end timestamps, exact command or interaction,
result, raw-output locator, and a final `PASS`, `PASS_WITH_LIMIT`, or `FAIL`
decision. A `PASS_WITH_LIMIT` is reusable only when the limit does not change the
public contract and is recorded in the implementation constraints.

## Requirement mapping

| Requirement | Primary test | Evidence route | Phase 0 prerequisite |
|---|---|---|---|
| MCP-001 | T-P1-MCP | `validation/p1-mcp.md` local integration + target client | F0-D |
| MCP-002 | T-P1-MCP | `validation/p1-mcp.md` local integration + target client | F0-D |
| MCP-003 | T-P1-MCP | `validation/p1-mcp.md` local integration + target client | F0-D |
| MCP-004 | T-P1-DOM | `validation/p1-stage.md` unit/contract fixture | None |
| MCP-005 | T-P1-MCP | `validation/p1-mcp.md` + Windows Codex | F0-D |
| CFG-001 | T-P1-CFG | `validation/p1-stage.md` local integration | None |
| CFG-002 | T-P1-CFG | `validation/p1-stage.md` local integration | None |
| CFG-003 | T-P1-CFG | `validation/p1-stage.md` local integration | None |
| CFG-004 | T-P1-CFG | `validation/p1-stage.md` DLL identity fixture | F0-A identity |
| CFG-005 | T-P1-CFG | `validation/p1-stage.md` repository/config fixture | None |
| RUN-001 | T-P1-IPC | `validation/p1-stage.md` Windows process integration | F0-B |
| RUN-002 | T-P3-RUN | J-Link hardware timeline | F0-A |
| RUN-003 | T-P1-IPC | `validation/p1-stage.md` Windows process integration | F0-B |
| RUN-004 | T-P3-RECOVER | Windows process integration | F0-B |
| RUN-005 | T-P1-DOM | `validation/p1-stage.md` unit/contract fixture | F0-B fault model |
| RUN-006 | T-P3-ABI | J-Link hardware/DLL exports | F0-A |
| SES-001 | T-P1-SES | `validation/p1-ses.md` + `validation/p1-stage.md` hardware integration | F0-A identity |
| SES-002 | T-P1-SES | `validation/p1-ses.md` + `validation/p1-stage.md` hardware integration | F0-A |
| SES-003 | T-P1-SES | `validation/p1-ses.md` hardware state recovery | F0-A |
| SES-004 | T-P1-SES | `validation/p1-ses.md` hardware integration | F0-A |
| SES-005 | T-P1-SES | `validation/p1-ses.md` hardware + identity fixture | F0-A |
| SES-006 | T-P1-SES | `validation/p1-ses.md` hardware diagnostics | F0-A |
| ART-001 | T-P2-IMG | Image-format fixture | F0-C ELF fixture |
| ART-002 | T-P2-IMG | ELF fixture + `validation/p2-stage.md` target Flash | F0-A + F0-C |
| ART-003 | T-P2-DWARF | `validation/p2-dwarf.md` IAR AccessPlan fixture | F0-C |
| ART-004 | T-P2-VALUE | `validation/p2-value.md` IAR DWARF/value 与 slice 契约 | F0-C |
| ART-005 | T-P2-DWARF | `validation/p2-dwarf.md` dynamic/pointer rejection | F0-C |
| ART-006 | T-P2-VALUE | `validation/p2-value.md` 无损 Value round-trip fixture | F0-C |
| ART-007 | T-P2-DWARF | `validation/p2-dwarf.md` IAR compatibility evidence | F0-C |
| PRG-001 | T-P2-PRG | `validation/p2-program.md` image/region unit + frozen DLL；`validation/p2-stage.md` actual Flash | F0-A |
| PRG-002 | T-P2-PRG | `validation/p2-program.md` strict Schema/state fixture；`validation/p2-stage.md` state smoke | F0-A |
| PRG-003 | T-P2-PRG | `validation/p2-program.md` range/device-algorithm primary evidence；3.7 does not add erase hardware evidence | F0-A |
| PRG-004 | T-P2-PRG | `validation/p2-program.md` compact mismatch fixture；`validation/p2-stage.md` matching readback | F0-A |
| PRG-005 | T-P2-PRG | `validation/p2-program.md` closed Schema/no-token contract；3.7 hardware | F0-A |
| PRG-006 | T-P2-PRG | `validation/p2-program.md` HSS-first state fixture；`validation/p2-stage.md` conflict route | F0-A |
| DBG-001 | T-P2-VALUE | `validation/p2-value.md` 预校验 + `validation/p2-memory.md` 执行链路；`validation/p2-stage.md` scalar hardware | F0-A + F0-C |
| DBG-002 | T-P2-MEM | `validation/p2-memory.md` unit/contract/IAR fixture；`validation/p2-stage.md` RAM hardware | F0-A |
| DBG-003 | T-P2-MEM | `validation/p2-memory.md` readback fixture；`validation/p2-stage.md` readback hardware | F0-A |
| DBG-004 | T-P2-CTL | `validation/p2-control.md` domain/MCP/IPC + J-Link hardware；`validation/p2-stage.md` | F0-A |
| DBG-005 | T-P2-CTL | `validation/p2-control.md` state rules + J-Link hardware；`validation/p2-stage.md` | F0-A |
| DBG-006 | T-P2-MEM | `validation/p2-memory.md` closed contract + frozen DLL；`validation/p2-stage.md` | F0-A |
| DBG-007 | T-P3-RUN | Active-capture hardware timeline | F0-A |
| DBG-008 | T-P2-DWARF | `validation/p2-dwarf.md` symbols route evidence | F0-C |
| HSSA-001 | T-P3-START | Contract + J-Link hardware | F0-A |
| HSSA-002 | T-P3-START | IAR fixture + J-Link hardware | F0-A + F0-C |
| HSSA-003 | T-P3-START | J-Link hardware diagnostics | F0-A |
| HSSA-004 | T-P3-RUN | Timed J-Link hardware capture | F0-A |
| HSSA-005 | T-P3-STATE | State-machine/fault fixture | F0-A + F0-B |
| HSSA-006 | T-P3-STORE | Local storage integration | F0-A throughput |
| HSSA-007 | T-P3-QUALITY | J-Link raw-frame fixture/hardware | F0-A |
| HSSA-008 | T-P3-RUN | J-Link write-interleaving timeline | F0-A |
| HSSA-009 | T-P3-RECOVER | Windows process + hardware integration | F0-B |
| HSSA-010 | T-P3-QUALITY | J-Link timing evidence | F0-A |
| HSSA-011 | T-P3-QUALITY | J-Link/host clock evidence | F0-A |
| HSSQ-001 | T-P4-OVERVIEW | Capture fixture integration | None |
| HSSQ-002 | T-P4-OVERVIEW | Capture fixture integration | None |
| HSSQ-003 | T-P4-OVERVIEW | Capture fixture integration | F0-D resource observation |
| HSSQ-004 | T-P4-CHANGES | Deterministic query fixture | None |
| HSSQ-005 | T-P4-WINDOW | Deterministic query fixture | None |
| HSSQ-006 | T-P4-WINDOW | Deterministic query fixture | None |
| HSSQ-007 | T-P4-TIMELINE | Deterministic timeline fixture | F0-A clock evidence |
| HSSQ-008 | T-P4-TIMELINE | Deterministic pagination fixture | None |
| HSSQ-009 | T-P4-TIMELINE | Immutable snapshot fixture | None |
| HSSQ-010 | T-P4-RESOURCE | Capture resource integration | F0-A format + F0-D link |
| HSSQ-011 | T-P4-RESOURCE | Contract/resource integration | F0-D |

## Stage smoke and evidence reuse

- Stage smoke tests observe cross-component wiring and MUST NOT become a second
  primary assertion for a requirement already assigned above.
- F0-A evidence may be reused by P2/P3 only while the complete J-Link hardware
  fingerprint is unchanged.
- F0-B evidence may be reused by P1/P3 only while the executable, IPC, lease,
  capture-key, and capture-format fingerprints are unchanged.
- F0-C evidence may be reused by P2/P3 only while the compiler, fixture, ELF,
  parser, and access-plan format fingerprints are unchanged.
- F0-D evidence may be reused by P1/P4 only while Windows Codex, the MCP
  protocol/Schema, server binary, and resource behavior fingerprints are unchanged.
