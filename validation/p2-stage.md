# P2-3.7 阶段检查点证据

## 裁决

`PASS`

P2 的 3.1–3.7 已完成。冻结生产链路通过 S32K144、SWD 4000 kHz 的 `connect → flash（默认校验）→ 独立 verify → resume → 变量/原始内存交叉读写并恢复 → 核心寄存器/目标控制 → reset_run → disconnect`。活动 HSS 占位下的 programming 和 debug/control 冲突路由由同一 smoke 的两项定向测试观察通过。本记录只是 P2 实现检查点，不代表 P3、P4、5.6、5.7、JTAG 真机或发布完成。

## 执行记录

| 字段 | 记录 |
|---|---|
| 硬件任务窗口 | `2026-08-27T03:36:59Z`–`2026-08-27T03:39:04Z`；包含冻结前置检查、完整 smoke 和后置安全检查 |
| 真机 smoke 命令 | `pwsh -NoLogo -NoProfile -File .\scripts\test-p2-smoke.ps1` |
| 真机 smoke 结果 | `PASS`；退出码 `0` |
| 烧录与校验 | flash 默认校验和随后独立只读 verify 均通过 |
| 变量往返 | `305419896 → 2309737967 → 305419896` |
| 原始内存往返 | `78563412 → efcdab89 → 78563412` |
| R0 往返 | `0x00000000 → 0x00000001 → 0x00000000` |
| PC 单步 | `0x00029E9A → 0x00029E9C` |
| 最终目标状态 | MCP 观察为 `connected/running`，随后成功 disconnect |
| HSS 冲突观察 | programming 首检拒绝、read/control 拒绝且已验证 RAM 写入边界的两项定向测试均 `PASS` |
| 原始输出 | Codex 任务 `codex://threads/01a0412c-18fb-7571-8ff3-b4e34a162f5a`；真机 smoke 输出块 `f7d0d3` |
| 源码定位 | 修复提交 `c0c4d7f1a87d5c0d16c410720dc2684695be8c37` 加本记录所在的 `[P2-3.7][验证]` 原子提交 |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`；`cargo 1.98.0 (797e8a9bc 2026-08-05)`；`x86_64-pc-windows-msvc` |
| 操作系统 | `Microsoft Windows NT 10.0.26200.0` |

## 冻结身份

| 对象 | 冻结值 |
|---|---|
| J-Link DLL | `C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll`；6.98a；SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| J-Link Commander | `C:\Program Files (x86)\SEGGER\JLink\JLink.exe`；6.98a；SHA-256 `0340130E7AD4F90EA8F8973C42A34A6508F0C5F6E988D532BB03DE9060FDFC04` |
| 探针 | 序列号 `260106173`；J-Link V10；硬件 V10.10 |
| 目标 | S32K144；Cortex-M4；SWD 4000 kHz |
| IAR OUT | `Appl\Output\Exe\T26_DCU_APP_NXP.out`；SHA-256 `F8ADB9A2B9BBFD26B469C66F2478EE6E22735302706B83509B2D4F2AE7F7738D` |
| IAR S19 | SHA-256 `0948AD69AF91437E2906A8371B6C18BE38C00DC60B23C9739773FB92CB82686A` |
| IAR MAP | SHA-256 `91E3EE2D4D7C77BAAF05AC30CEAA8A8EC1835668FDBB1E188CA0DCC342A40AFC` |
| 测试夹具 | `AppUserDesc.c`；SHA-256 `1133B85709AB5ED3509ED58433ED4132E4D0869724140F8D3F560F7BA3B709E4` |
| smoke 驱动 | `scripts/test-p2-smoke.ps1`；SHA-256 `3DA45CE3D192F1B259F277B2CB77D821A3E0AE0F6F861C4D8247114C3E840236` |
| 生产 MCP | `target\debug\jlink-mcp.exe`；SHA-256 `6C9CFFA14F48D2139998671A6C61746ECF75921C3ECDA12D7A82B4B76335DF56` |
| 生产 Worker | `target\debug\jlink-worker.exe`；SHA-256 `5E3BFA13837EAA480020E4896F8A00D1867CF4F736ED3F1B4C169495EF61EF52` |

## 根因修正与纵向结果

- 冻结 DLL 的静态反汇编证明通用 `JLINKARM_ReadMem` 返回状态：内部完整读取时为 `0`，非零为失败；类型化 `JLINKARM_ReadMemU32` 等接口才返回完成项目数。生产 gateway 已分别解释，回归测试覆盖 `0` 成功和非零拒绝。
- flash 在首个 `BeginDownload` 前执行 `reset_halt`；默认校验在 `EndDownload` 成功后再次 `reset_halt`，完成读回后才应用请求的 `after`。完整 smoke 的默认校验与独立 verify 均通过，没有 Commander、降速、重试或 fallback。
- 独立 verify 保持只读。由于 flash 的 `after: reset_halt` 使目标停在启动初始化前，OUT/Map 与启动汇编证明可写测试量位于由 `init_data_bss` 手工初始化的 `.data`；smoke 因此在 verify 后显式 `resume`，等待生产状态机确认稳定运行后一次读取初值，没有重试读取或固定延时兜底。
- 变量和同地址原始内存交叉验证了同一 32 位值，并由相反入口恢复原值；随后 R0 写入、读回和恢复通过，PC 单步发生变化。
- 最终 `reset_run` 后 status 为 `connected/running`，disconnect 成功，进程正常退出。

## SVN 现场与安全恢复

- 硬件任务前后均读取完整 `svn status --depth infinity`；两次均为 612 行，内容 SHA-256 均为 `6827FD361AB388ABB26A6648158B0417CDDB76FAC515F91472C06B5715794685`。
- 受控文件仍只有三处 `M`：既有 `AppPwrMode.h`、计划内 `AppUserDesc.c` 测试夹具和用户重新冻结的 `T26_DCU_APP_NXP.dep`；没有执行 `svn commit`。
- `AppPwrMode.h` 前后 SHA-256 保持 `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085`；`T26_DCU_APP_NXP.dep` 保持 `4FDA4431B3502EBDB1B0313BF58B21995A2B962C9C0BA853DF42F3988B4A6F85`。
- 测试变量和 R0 已恢复原值，最后一次目标观察为 running，随后安全断开；结束后 `jlink-mcp`、`jlink-worker`、`JLink` 进程数为 0。

## 范围和失效条件

3.7 是跨组件 smoke，不替代 3.1–3.6 已分配的主要需求测试。本轮实际证明了当前冻结镜像的 Flash 写入、默认校验、独立只读校验、RAM 变量/原始内存、核心寄存器与控制组合链路；没有重复整片或范围擦除、校验 mismatch、所有 `after` 分支、MMIO/复合变量主要断言，也不据此宣称这些额外硬件场景已验证。实际 HSS 采集、写入交错和恢复属于 P3；JTAG、Windows Codex 客户端验收和发布门禁继续待办。DLL、探针、目标、接口/速度、OUT、夹具、gateway ABI、Flash 状态机、AccessPlan、IPC 或 smoke 驱动任一变化时，本阶段真机证据失效。
