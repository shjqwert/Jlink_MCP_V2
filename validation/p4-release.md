# P4 5.7 V1 SWD 发布门禁证据

## 结论

`PASS`

`define-jlink-mcp-v1` 的 65 条 Requirement 均具有唯一主要测试和可定位证据。最终代码通过一次完整 workspace 软件门禁、S32K144/SWD 4000 kHz 静态调试纵向 smoke、10×32-bit/1 kHz/300 秒 HSS 纵向验收，以及当前 Windows Codex 的隔离客户端复核。

本结论只冻结实际通过的 SWD 支持。JTAG 未进行真机测试，不声明支持；本次验收未执行或允许 JTAG 失败后回退 SWD。OpenSpec 归档由独立后续操作决定。

## 软件发布门禁

| 项目 | 结果 |
|---|---|
| `scripts/check-workspace.ps1` | `PASS`；workspace fmt、Clippy `-D warnings`、全部非硬件测试和四 crate 依赖方向通过 |
| `openspec validate define-jlink-mcp-v1 --strict` | `PASS` |
| `cargo build --release --workspace` | `PASS` |
| `git diff --check` | `PASS`；仅有既有 LF/CRLF 工作副本提示，无空白错误 |
| Requirement 矩阵 | `65/65`；缺失 `0`、重复 `0`、无验证文档路线 `0` |

发布门禁首次发现一条旧 HSS 恢复测试仍要求新 Worker 通过旧 `capture_key` 查询中断 capture。生产恢复实现和 HSSA-009 均要求旧 key 随旧 MCP/Worker 生命周期退役；最短复现证明测试预期陈旧。修订仅更新该测试断言：中断文件仍可按 capture ID 查看为 `aborted + unknown`，旧 key 返回 `VALUE_INVALID`。定向回归和随后完整门禁均通过，未改变公共 Schema 或生产路径。

## SWD 静态调试纵向验收

| 项目 | 结果 |
|---|---|
| 时间窗口 | `2026-08-28T02:21:40Z` 至 `2026-08-28T02:21:57Z` |
| 命令 | `& .\scripts\test-p2-smoke.ps1` |
| 结果 | `PASS`；flash 默认校验、独立 verify、变量/原始内存交叉读写与恢复、R0 往返、单步、reset/run、disconnect |
| 连接 | S32K144；探针 `260106173`；SWD `4000 kHz`；最终 `connected/running` 后断开 |
| 变量 | `305419896 → 2309737967 → 305419896` |
| 原始内存 | `78563412 → efcdab89 → 78563412` |
| R0 | `0x000000FF → 0x000000FE → 0x000000FF` |
| PC 单步 | `0x00029F54 → 0x00029522` |

## 300 秒 HSS 纵向验收

| 项目 | 结果 |
|---|---|
| 时间窗口 | `2026-08-28T02:22:15.927Z` 至 `2026-08-28T02:27:15.972Z` |
| 命令 | `& .\scripts\test-p3-smoke.ps1 -CaptureKey 'p4-5.7-300s-1khz-20260828' -EvidenceDirectory 'p4-5.7'` |
| 结果 | `PASS`；自动停止、尾排空、两次交错写入、读回与原值恢复、不可变 Capture Store、CPU 运行态和断开 |
| Capture | `cap_0373e535731d6433da7d43bd`；300,004 条完整记录；13,200,176 payload bytes |
| 频率 | 请求 `1000 Hz`；实际 `1000000 millihz`；区间 min/max 均 `1000 us` |
| 原始摘要 | `ffbb0d1de778fff029736d3a781d98ba0276feb7818e2b8813702a3116c43f9a` |
| 完整文件 | `target/evidence/p4-5.7/capture-cap_0373e535731d6433da7d43bd.capture`；SHA-256 `4FD8F5BE662655E00CC9D1BA7A48733AAAEAFDB1269AF4DB9A973539E505C1FE` |
| 质量限制 | 6.98a 无独立 overflow/sequence counter；`loss=unknown`、`overflow=unknown`，未声明零丢样或无溢出 |

## 当前 Windows Codex 定向复核

| 项目 | 结果 |
|---|---|
| 时间窗口 | `2026-08-28T02:29:06.529Z` 至 `2026-08-28T02:31:18.993Z` |
| 客户端 | `OpenAI.Codex_26.820.10647.0_x64__2p2nqsd0c76g0`；`codex-cli 0.150.1` |
| 隔离方式 | `--ignore-user-config --ephemeral`；仅配置 required server `jlink_mcp_v2_acceptance` |
| 产品二进制 | `jlink-mcp.exe` SHA-256 `E4BBAF457E7D9355179D497A4D1411F051B44EE1FF4E03D2FA47CB494F909F9B` |
| 结果 | `PASS`；六工具闭集、额外字段 `-32602`、结构化断开态、overview、window 两页游标、raw resource |
| 分页 | 第一页 `[0,1000]`；第二页 `[2000,3000]`；续页只携带 `action/capture_id/cursor` |
| 资源 | `jlink-mcp://capture/cap_0373e535731d6433da7d43bd/raw`；`application/vnd.jlink-mcp.capture.v1+binary`；头 `JMCPV101` |
| 最终摘要 | `target/evidence/p4-5.7/client-final.txt`；SHA-256 `39A62B5B691EBADEC40518D2413F2B29287A4616BAB6CA4D210C2D5749E4BF1C` |

客户端只读取已完成的不可变 capture，没有连接或控制硬件。全局旧项目的 `jlink` 未加载、未调用。验收结束后 `jlink-mcp`、`jlink-worker` 和 `JLink` 进程数均为 0。

## 冻结支持矩阵

| 维度 | V1 发布结论 |
|---|---|
| 主机/传输 | Windows x64；本机 stdio MCP |
| 操作系统 | Windows 11 家庭版中文版；build `26200` |
| J-Link | 6.98a；DLL SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| 探针/目标 | 探针 `260106173`；S32K144/Cortex-M4 |
| 接口 | SWD `4000 kHz`：`PASS`；JTAG：未验证、不声明支持 |
| IAR | ARM 8.32.3；OUT SHA-256 `F8ADB9A2B9BBFD26B469C66F2478EE6E22735302706B83509B2D4F2AE7F7738D` |
| 测试夹具 | `AppUserDesc.c` SHA-256 `1133B85709AB5ED3509ED58433ED4132E4D0869724140F8D3F560F7BA3B709E4` |
| IAR DEP | 当次 SHA-256 `7AE5506F02D23CAC1E95E5C52F1B60282E151001FA47D0237F55EE40EFFA73A5`；允许计划内构建更新并使相关旧证据失效 |
| Rust | `rustc 1.98.0`、`cargo 1.98.0`；`x86_64-pc-windows-msvc` |
| 产品二进制 | MCP `E4BBAF457E7D9355179D497A4D1411F051B44EE1FF4E03D2FA47CB494F909F9B`；Worker `F5D89591E4D038991DF70303A136A6DFC90F82D75BDF50B88CF35B99A113C72B` |
| HSS | 1–300 秒、1–10 个顶层 selector、请求最高 1 kHz；6.98a 毫秒源归一化为微秒，源分辨率 1 ms |
| 客户端 | Windows Codex `26.820.10647.0`；CLI `0.150.1` |

J-Link 8.38/9.70 只保留 P0 兼容性参考，不属于本 V1 生产支持矩阵。测试工程允许按后续测试场景修改变量、编译、生成 OUT/DEP、烧录或擦除；任何新指纹只使相关硬件证据失效，不改变公共合同。

## 目标工程与安全恢复

- 本轮没有执行 `svn commit`。SVN 状态在 HSS 前后保持 `6827FD361AB388ABB26A6648158B0417CDDB76FAC515F91472C06B5715794685 / 612 / 609 / 3`。
- 与本轮测试无关的 `AppPwrMode.h` 保持 SHA-256 `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085`。
- P2/P3 结束时测试变量和寄存器均恢复原值；HSS 已停止并尾排空；CPU 恢复运行；目标断开；无残留进程。

## 证据失效条件

- 公共 MCP Schema、IPC、核心状态机、Capture Store 格式、资源 URI/MIME、查询游标或 Worker 生命周期变化。
- DLL、探针、目标、接口、速度、OUT、测试夹具、IAR 配置或安全恢复合同发生相关变化。
- Windows Codex 应用包/CLI、产品二进制或客户端资源处理变化。

只补录说明、提交信息、任务状态或证据索引且未改变上述输入时，本门禁证据继续有效。
