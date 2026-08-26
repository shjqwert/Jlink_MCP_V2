# P1-2.7 阶段检查点证据

## 裁决

`PASS`

P1 的 2.1–2.7 已完成。四 crate 依赖边界、领域合同、分层配置、Windows Worker/IPC、目标会话和六工具 MCP 边界的主要测试及工作区门禁全部通过。生产 `jlink-mcp.exe` 通过本机 stdio 调用真实 Worker 完成 S32K144/SWD 4000 kHz 的 `connect → status → disconnect`。本记录只是 P1 实现检查点，不代表 P2–P4、5.6、5.7 或发布完成。

## 执行记录

| 字段 | 记录 |
|---|---|
| 工作区门禁 | `& .\scripts\check-workspace.ps1`；格式、Clippy `-D warnings`、workspace 测试和依赖方向全部 `PASS` |
| P1 本地主要测试 | T-P1-DOM 6/6、T-P1-IPC frame 3/3、T-P1-SES 合同 4/4、T-P1-CFG 7/7、T-P1-MCP 6/6，其余受影响单元测试全部通过 |
| OpenSpec | `openspec validate define-jlink-mcp-v1 --strict`；`PASS` |
| 真机 smoke 开始 | `2026-08-26T07:34:29.0745899Z` |
| 真机 smoke 结束 | `2026-08-26T07:34:33.1589660Z` |
| 真机 smoke 命令 | `& .\scripts\test-p1-smoke.ps1`，外层记录 UTC 起止时间并原样返回退出码 |
| 真机 smoke 结果 | `PASS`；退出码 `0`；connect 返回 `resumed_from_halt`；status 为 `connected/running`；disconnect 为 `{}` |
| 原始输出 | Codex 任务 `codex://threads/01a03bf8-d8c2-7e43-9626-06f420336dc2`；最终工作区/OpenSpec 门禁输出块 `8e80ab`；真机 smoke 输出块 `d8e764` |
| 源码定位 | 父提交 `ff006a20cdb8e1f42d046d2ad7598c493ea6ca12` 加本记录所在的 `[P1-2.7][验证]` 原子提交 |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`；`cargo 1.98.0 (797e8a9bc 2026-08-05)`；`x86_64-pc-windows-msvc` |
| 操作系统 | `Microsoft Windows NT 10.0.26200.0` |

## 冻结身份

| 对象 | 冻结值 |
|---|---|
| J-Link DLL | `C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll`；6.98a；SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| J-Link Commander | `C:\Program Files (x86)\SEGGER\JLink\JLink.exe`；6.98a；SHA-256 `0340130E7AD4F90EA8F8973C42A34A6508F0C5F6E988D532BB03DE9060FDFC04` |
| 探针 | 序列号 `260106173`；硬件 `V10.10`；固件报告 `J-Link V10 compiled Jun 27 2028 10:57:29` |
| 目标 | S32K144；Cortex-M4 r0p1；SW-DP ID `0x2BA01477`；SWD 4000 kHz |
| IAR OUT | `T26_DCU_APP_NXP.out`；SHA-256 `3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF` |
| P1 smoke 驱动 | `scripts/test-p1-smoke.ps1`；SHA-256 `09B5BBFC11808369264B40E0F279F231DF1F9B1D5A4AC3C58BB1ABCE2148DB7F` |
| 生产 MCP | `jlink-mcp.exe`；SHA-256 `38DCE490521E64065D813997BF1AF418BCA8DBD3A07B1F9CDEC6C70396921195` |
| 生产 Worker | `jlink-worker.exe`；SHA-256 `9E2A85314AD6B9ECE6766B577578B50A2B73EE370B40899C10E0695CBFC8A9C1` |

## 真机结果与安全恢复

- 生产 MCP 从临时 project/user 配置层读取冻结身份，没有在仓库或 SVN 工程写入运行配置。
- workspace 中的 HardFault 真机用例仍只由显式 T-P1-SES 硬件驱动以 `--ignored --exact` 执行，其已通过的身份与证据保留在 `validation/p1-ses.md`；2.7 未用 workspace smoke 替代该需求测试。
- connect 观察到 halted 后复用生产恢复状态机执行 resume，返回 `resumed_from_halt`；紧接的 status 为 `connected/running`。
- disconnect 通过唯一 Worker 管道完成并返回最小 `{}`；MCP 正常退出。
- 成功路径不额外运行 Commander；最后一次目标观察为 running，随后 Worker 安全 disconnect 并退出，结束后无 `jlink-worker` 或 `JLink` 残留进程。只有 connect 已开始但未安全 disconnect 时，失败清理才使用冻结 Commander 关闭 vector catch 并执行 `go`；该路径的器件初始化可能复位目标，仅作为失败后恢复 CPU 的安全手段。
- `AppPwrMode.h` 和 `T26_DCU_APP_NXP.dep` 前后 SHA-256 分别保持 `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085` 和 `B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620`。
- 前后 `svn status -q` 一致，只有上述两处既有用户修改；没有执行 `svn commit`。DLL 和 IAR OUT 指纹未变。

## 范围和失效条件

P1 硬件检查点仅适用于本记录的 DLL、探针、目标、SWD 速度、OUT、MCP/Worker 源码和二进制。任一身份或 connect/status/disconnect 路径变化时，真机 smoke 证据失效。JTAG 真机、静态调试、烧录、HSS/Capture Store、查询资源、Windows Codex 生产验收和发布门禁均未由本检查点完成。
