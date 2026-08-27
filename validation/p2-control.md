# P2 核心寄存器与目标控制证据

## 结论

OpenSpec 任务 3.6 的主要测试 T-P2-CTL 已通过软件合同、IPC、HSS 冲突和冻结真机链路验证。生产链路已接通精确规范名称的 Cortex-M 核心寄存器读写，以及 `halt`、`resume`、显式 `after` 的 `reset` 和 halted 前提下的 `step`。真机测试结束后已恢复被测寄存器原值并确认 CPU 运行；没有使用 JTAG、SWD 回退、Mock 硬件证据或失败重试。

## 已验证实现

- V1 规范寄存器集合固定为 `R0`–`R12`、`SP`、`LR`、`PC`、`XPSR`、`MSP`、`PSP`、`APSR`、`EPSR`、`IPSR`、`PRIMASK`、`BASEPRI`、`FAULTMASK`、`CONTROL`。名称大小写敏感，不接受 `R13/R14/R15` 别名或模糊匹配。
- Worker 使用 `JLINKARM_GetRegisterList/GetRegisterName` 读取活动目标目录，并与上述规范集合取交集；`SP/LR/PC` 分别映射冻结 DLL 的 `R13 (SP)`、`R14`、`R15 (PC)`。DLL 额外列出的 FPU、TrustZone 或其他扩展不会自动成为 V1 能力。
- 读取使用单项 `JLINKARM_ReadRegs` 并检查逐项状态；目录缺失或不支持时返回 `REGISTER_NOT_FOUND`。写入仅接受 32 位值；`XPSR/EPSR/IPSR` 在 `JLINKARM_WriteReg` 前作为只读视图返回 `VALUE_INVALID`，非零写入状态不得表示成功。
- `jlink_control` 四个 action 复用唯一 gateway 和 Worker 会话状态。`step` 先观察目标，运行态请求返回 `TARGET_STATE_INVALID` 且不隐式 halt；成功单步后确认仍为 halted。`reset` 必须显式选择 `run` 或 `halt`，控制后的实际状态写回会话。
- 任何已经进入控制 gateway、但无法确定最终状态的失败都会关闭目标连接并把会话置为 `faulted/unknown`，清除旧目标和验证身份；后续操作不得复用失败前缓存。运行态 `step` 属于设备动作前的确定拒绝，不错误地使会话失效。
- 活动 HSS 时，寄存器读写和所有公共目标控制均在第一次目标动作前返回 `OPERATION_CONFLICT`；只有既定的变量及 RAM/MMIO 写入保留交错资格。
- MCP 成功结果保持最小：寄存器读取返回固定宽度十六进制 `value`，完整写入与控制返回 `{}`；稳定错误经单一映射公开。

## DLL 目录与失败调查

冻结 6.98a DLL 的 x64 导出和反汇编确认了 `JLINKARM_GetRegisterList`、`JLINKARM_GetRegisterName`、`JLINKARM_ReadRegs`、`JLINKARM_WriteReg` 与 `JLINKARM_Step` 的实际调用边界。首次探索把目录返回的全部 92 项直接交给批量读取，调用超过 60 秒未返回；测试随即中止，并使用同接口、同速度的冻结 Commander 安全脚本恢复 CPU 运行。

根因证据是该目录同时包含当前 S32K144/Cortex-M4 主线不需要的 TrustZone、FPU 和其他扩展项，不能把“DLL 列出”解释为“V1 当前目标支持”。修正没有增加重试或 fallback，而是先冻结 V1 规范集合，再与活动目标目录精确相交并逐项检查状态。限定目录索引后的 26 项读取全部返回状态 0；随后完整 T-P2-CTL 真机测试通过。

## 最小相关测试

| 检查 | 当前结果 | 覆盖 |
|---|---|---|
| `cargo test -p jlink-domain --test t_p2_ctl` | PASS，3/3 | 精确规范名称、别名拒绝、只读写入前拒绝、控制值对象与执行分类 |
| `cargo test -p jlink-mcp --test t_p2_ctl` | PASS，2/2 | 六工具严格 Schema、reset.after、最小结果、稳定公共错误映射 |
| Worker IPC 定向用例 | PASS，1/1 | register/control command 与 payload 的精确组合 |
| Worker HSS 冲突定向用例 | PASS，1/1 | 活动 HSS 拒绝寄存器与控制，不削弱既定允许写入边界 |
| Worker 不确定控制定向用例 | PASS，1/1 | 不确定控制关闭并失效旧会话，确定的运行态 step 拒绝不误伤会话 |
| 冻结真机定向用例 | PASS，1/1 | 26 项目录、运行态 step 拒绝、R0 读写恢复、PC 单步变化、reset/halt/resume 状态收口 |
| `scripts/test-p2-ctl.ps1` | PASS | DLL/受保护文件前后哈希、完整 SVN 状态前后相同、真机链路与无条件安全恢复 |

## 提交前统一门禁

| 检查 | 当前结果 | 覆盖 |
|---|---|---|
| `scripts/check-workspace.ps1` | PASS | 格式、workspace `clippy -D warnings`、workspace 测试、四 crate 依赖方向 |
| `openspec validate define-jlink-mcp-v1 --strict` | PASS | 规格、设计、任务和证据路线严格校验 |
| `git diff --check` | PASS | 最终代码状态空白检查 |

统一门禁最终通过时间为 `2026-08-26T20:21:58+08:00`。首次门禁只发现两个机械 clippy 问题：相同 match 分支和增长到 128 行的 IPC 合同校验；修正为合并等价分支并提取具有独立语义的 control payload 校验，没有添加 lint 豁免。第二次门禁只发现旧 P1 测试仍把已经接通的 `jlink_control` 当作未来 action；修正后该测试继续验证真正待实现的 HSS 路由，寄存器和控制行为仍由 T-P2-CTL 断言。对应最短复现均先通过，第三次统一门禁完整通过。

门禁前差异复核还发现控制调用若执行结果不确定，旧会话状态可能被继续复用。修正复用既有不确定执行状态机：关闭目标、置 `faulted/unknown`、清除旧身份并拒绝后续复用；回归测试通过。由于该修正改变 Worker 指纹，随后只重跑任务级 `scripts/test-p2-ctl.ps1`，当前二进制的真机测试和安全恢复再次 PASS；没有重复执行 workspace 全量门禁。

## 冻结身份与真机结果

| 输入 / 产物 | SHA-256 / 身份 |
|---|---|
| J-Link DLL | `C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll`；6.98a；SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| 探针 | S/N `260106173`；J-Link V10；硬件 V10.10；VTref 约 4.677 V |
| 目标配置 | S32K144；Cortex-M4 r0p1；SWD；4000 kHz |
| IAR OUT | `Appl/Output/Exe/T26_DCU_APP_NXP.out`；SHA-256 `9CA4B80CE028F03BDE56082C20BFEDFA65FAE6264B33FB3190FD87FC7DA5CCE2` |
| T-P2-CTL domain 测试二进制 | `t_p2_ctl-78e34c86e00a1e3d.exe`；SHA-256 `431C174F61B187626A323EE54FF3C7B813F75EE66D5D3B3DC2651E072278D6D4` |
| T-P2-CTL MCP 测试二进制 | `t_p2_ctl-d1d8a6603dc6cdd0.exe`；SHA-256 `3E4F73EFAFC9FF4860967668E9FE885ECF325931EDBCD0944694DAA9488545C3` |
| Worker 真机测试二进制 | `jlink_worker-70029957f7b1d040.exe`；SHA-256 `C84B653F9C153899239640E5161F1B884EFBEF70A081D495F34E37F72141E71E` |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`；`x86_64-pc-windows-msvc` |

真机纵向用例首先确认 CPU 运行，验证运行态 `step` 无隐式状态变化，再执行 `halt → R0 读取/改写/读回/恢复 → PC 单步变化 → reset_halt → reset_run → halt/resume`。清理路径在测试成功或失败时都恢复 R0（若已改写）并调用冻结 Commander 脚本确认 CPU 最终运行。

## SVN 现场与证据边界

未执行 `svn commit`。`scripts/test-p2-ctl.ps1` 对测试前后的完整 `svn status` 做逐行比较，结果完全一致；3.6 没有修改目标工程。两个受保护文件哈希保持不变：

| 受保护文件 | SHA-256 |
|---|---|
| `Appl/Source/Appl/AppPwrMode/AppPwrMode.h` | `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085` |
| `Appl/T26_DCU_APP_NXP.dep` | `B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620` |

DBG-004、DBG-005 的主要证据已经形成。3.7 已在同一冻结链路完成烧录、校验、变量/内存、R0 往返恢复、PC 单步、reset_run 和 disconnect 组合 smoke，并观察活动 HSS 占位状态下的冲突路由，证据见 `validation/p2-stage.md`。任一 DLL、探针/目标、OUT、寄存器规范集合、FFI ABI、IPC、错误映射、HSS 边界或会话状态机变化时，本证据失效。
