# W001 Phase 0 测试环境

状态：基础信息、测试授权和 V1 客户端范围已确认；W001/P0 已开始，F0-A 真机与存储证据正在裁决，F0-B 至 F0-D 尚待执行。

本文件保存 W001 的本地测试对象与环境身份。公共行为、门禁和验收仍以 `openspec/changes/define-jlink-mcp-v1/` 为准；当前执行状态以 W001 handoff 为准。

## 实际目标工程

- SVN 工作副本：`D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP`
- MCU：`S32K144`
- 调试接口：`SWD`
- 接口速度：`4000 kHz`
- 工程文件：`Appl\T26_DCU_APP_NXP.ewp`、`Appl\T26_DCU_APP_NXP.eww`、`Appl\T26_DCU_APP_NXP.ewd`
- IAR 编译器：`IAR ANSI C/C++ Compiler V8.32.3.193/W32 for ARM`
- IAR 链接器：`IAR ELF Linker V8.32.3.193/W32 for ARM`
- IAR 命令行工具：`C:\Program Files (x86)\IAR Systems\Embedded Workbench 8.2\common\bin\IarBuild.exe`，文件版本 `8.2.4.5914`

现有输出于 2026-08-25 16:49:30（本地时间）生成：

| 产物 | 大小 | SHA-256 |
|---|---:|---|
| `Appl\Output\Exe\T26_DCU_APP_NXP.out` | 4,962,648 bytes | `3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF` |
| `Appl\Output\Exe\T26_DCU_APP_NXP.s19` | 593,962 bytes | `7FF2027EB6722889A80649274BEABE0E690B82C21230AE720453134861E183F7` |
| `Appl\Output\List\T26_DCU_APP_NXP.map` | 369,614 bytes | `3DE61C97875E7007BFE2D6F3A1F258BF849A3142381FAEBAE6904F79A103420F` |

`.out` 已只读确认为 ARM ELF32、小端、EABI5、入口 `0x0003BEAD`，包含 `.debug_info` 等 DWARF 段；首个 compilation unit 使用 DWARF v4，producer 为 IAR 8.32.3.193。现有产物可作为 F0-C 的实际 IAR 样本和 F0-A 的候选固件身份，但每次实验前必须重新计算内容哈希，防止 SVN 工程重新编译后继续复用旧证据。

## J-Link DLL 候选

F0-A 已按相同探针、固件、连接参数和证据格式比较三个版本。用户根据真机结果确认 6.98a 为 HSS 主线，使用毫秒源时间戳模式；8.38 和 9.70 为兼容性副线，不以其微秒模式或长时结果阻塞主线。公共 `timestamp_us/time_us` 仍按整数微秒表达，6.98a 源时间戳通过 `ms × 1000` 归一化并显式记录 1 ms 源分辨率。

| 线路 | 版本 | DLL | 大小 | SHA-256 | 角色 |
|---:|---|---|---:|---|---|
| 主线 | 6.98a | `C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll` | 19,117,344 bytes | `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` | HSS 生产候选基线；毫秒源时间戳 |
| 副线 | 8.38 | `C:\Users\usre\SEGGER\JLink_V838\JLink_x64.dll` | 24,381,392 bytes | `56C2C354475F48B9532D7EF03842D6D073E74D38FBAD212711EFAF773B1D5AD3` | 兼容性与回归证据 |
| 副线 | 9.70 | `C:\Program Files\SEGGER\JLink_V970\JLink_x64.dll` | 26,532,336 bytes | `1899A536ED388CF931F82E49C4F8DC25642F581AE290444DF1B192720A726D29` | 兼容性与回归证据 |

6.98a 已完成 `10 blocks × 1000 Hz × 300 s` 和三次采集中 RAM 写入，作为 V2 主线行为证据。8.38/9.70 在约 50 秒后出现普通 RAM 访问失败，且该故障在 `10 blocks × 500 Hz` 和 `1 block × 1000 Hz` 下都能复现，因此不是普通版探针的 1 kHz 上限或采样总吞吐直接导致。新版 DLL 弹出的探针识别警告和探针报告的未来固件日期作为兼容性异常保留，但不得据此断言硬件真伪或固件损坏。

## MCP 客户端与主机工具

- 用户确认主要使用 OpenAI Windows Codex。
- 当前安装包为 `OpenAI.Codex 26.818.5229.0`，package full name 为 `OpenAI.Codex_26.818.5229.0_x64__2p2nqsd0c76g0`；F0-D 必须同时保存应用包与 CLI 的实际版本。
- `codex-cli 0.149.1` 是 Windows Codex 当前调用的本地 MCP 客户端边界。
- V1 只要求验证 Windows Codex；ChatGPT Desktop、Claude 和其他客户端不属于 V1 验收范围。
- 旧 MCP 保持为全局服务 `jlink`，指向 `D:\Github\jlink-mcp\out\mcp\standalone.js`；P0 mock 使用独立名称 `jlink_p0_v2_probe`、独立 V2 路径和独立状态文件，测试不得混用两个命名空间。
- Rust MSVC 工具链已安装：`rustc 1.98.0 (88d9e12ae 2026-08-18)`、`cargo 1.98.0 (797e8a9bc 2026-08-05)`。
- `Arm GNU Toolchain arm-none-eabi 14.2 rel1` 已安装，可用于 ELF/DWARF 只读检查和独立 fixture；它不能替代实际 IAR 产物证据。

## 已确认测试授权

- 用户确认目标板已经连接，并授权 Phase 0 对该板执行烧录、reset/resume、RAM/MMIO/变量写入及最长 300 秒采集。
- 用户允许增加隔离的测试专用变量或模块，并生成测试固件；测试实现不得覆盖现有用户修改，构建产物必须采用隔离策略。
- Rust 工具链安装已获授权，但必须等 W001 被明确启动后再执行。

## 保护边界

- SVN 工作副本当前已有用户修改：`Appl\Source\Appl\AppPwrMode\AppPwrMode.h` 和 `Appl\T26_DCU_APP_NXP.dep`。不得恢复、覆盖或把这些变更当作 W001 修改。
- 在明确构建配置和隔离输出策略前，不在该 SVN 工作副本中执行 IAR build/clean；优先只读使用现有 `.out`、`.s19` 和 `.map`。
- `Appl\T26_DCU_APP_NXP.ewp` 是 IAR TSD 二进制工程文件，不按 XML 或普通文本改写。
- 本文件和 `architecture/` 目录被 V2 `.gitignore` 排除，不上传 Git。
- 探针已由工具枚举并连接：序列号 `260106173`、硬件 `V10.10`、目标 `S32K144 Cortex-M4 r0p1`、SWD-DP ID `0x2BA01477`；真机实验后测试 RAM 已恢复为零且 CPU 处于运行状态。

## 尚待自动登记或后续补齐

1. 若需要重新构建实际工程，仍需识别 IAR 的活动 configuration 名称，并先确定不覆盖当前输出和用户修改的隔离构建方式；可优先由工程或 IAR 工具自动发现。
2. 发布前仍需一套可用于 JTAG smoke 的硬件组合；当前板卡只确认 SWD，因此这不阻塞首轮 S32K144 Phase 0，但会阻塞包含 JTAG 的最终发布证据。
