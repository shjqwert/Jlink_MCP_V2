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
| MCP-001 | T-P1-MCP | Local integration + target client | F0-D |
| MCP-002 | T-P1-MCP | Local integration + target client | F0-D |
| MCP-003 | T-P1-MCP | Local integration + target client | F0-D |
| MCP-004 | T-P1-DOM | Unit/contract fixture | None |
| MCP-005 | T-P1-MCP | Windows Codex | F0-D |
| CFG-001 | T-P1-CFG | Local integration | None |
| CFG-002 | T-P1-CFG | Local integration | None |
| CFG-003 | T-P1-CFG | Local integration | None |
| CFG-004 | T-P1-CFG | DLL identity fixture | F0-A identity |
| CFG-005 | T-P1-CFG | Repository/config fixture | None |
| RUN-001 | T-P1-IPC | Windows process integration | F0-B |
| RUN-002 | T-P3-RUN | J-Link hardware timeline | F0-A |
| RUN-003 | T-P1-IPC | Windows process integration | F0-B |
| RUN-004 | T-P3-RECOVER | Windows process integration | F0-B |
| RUN-005 | T-P1-DOM | Unit/contract fixture | F0-B fault model |
| RUN-006 | T-P3-ABI | J-Link hardware/DLL exports | F0-A |
| SES-001 | T-P1-SES | J-Link hardware integration | F0-A identity |
| SES-002 | T-P1-SES | J-Link hardware integration | F0-A |
| SES-003 | T-P1-SES | J-Link hardware state recovery | F0-A |
| SES-004 | T-P1-SES | J-Link hardware integration | F0-A |
| SES-005 | T-P1-SES | Hardware + identity fixture | F0-A |
| SES-006 | T-P1-SES | J-Link hardware diagnostics | F0-A |
| ART-001 | T-P2-IMG | Image-format fixture | F0-C ELF fixture |
| ART-002 | T-P2-IMG | ELF fixture + target Flash | F0-A + F0-C |
| ART-003 | T-P2-DWARF | IAR DWARF fixture | F0-C |
| ART-004 | T-P2-VALUE | IAR DWARF/value fixture | F0-C |
| ART-005 | T-P2-DWARF | DWARF rejection fixture | F0-C |
| ART-006 | T-P2-VALUE | Value round-trip fixture | F0-C |
| ART-007 | T-P2-DWARF | Compiler compatibility fixture | F0-C |
| PRG-001 | T-P2-PRG | J-Link hardware | F0-A |
| PRG-002 | T-P2-PRG | Contract + J-Link hardware | F0-A |
| PRG-003 | T-P2-PRG | J-Link hardware | F0-A |
| PRG-004 | T-P2-PRG | J-Link hardware | F0-A |
| PRG-005 | T-P2-PRG | Contract + J-Link hardware | F0-A |
| PRG-006 | T-P2-PRG | Active-capture hardware | F0-A |
| DBG-001 | T-P2-VALUE | IAR fixture + J-Link hardware | F0-A + F0-C |
| DBG-002 | T-P2-MEM | J-Link hardware | F0-A |
| DBG-003 | T-P2-MEM | J-Link hardware | F0-A |
| DBG-004 | T-P2-CTL | J-Link hardware | F0-A |
| DBG-005 | T-P2-CTL | J-Link hardware | F0-A |
| DBG-006 | T-P2-MEM | Contract + J-Link hardware | F0-A |
| DBG-007 | T-P3-RUN | Active-capture hardware timeline | F0-A |
| DBG-008 | T-P2-DWARF | IAR DWARF fixture | F0-C |
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
