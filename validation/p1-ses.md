# P1-2.5 目标会话验证证据

## 裁决

`PASS`

主要测试 T-P1-SES 已覆盖 SES-001 至 SES-006。当前实现完成单活动目标、首次验证缓存、连接状态、显式诊断、halted 恢复以及同一 Worker/DLL 会话内真实 HardFault 恢复。断开态 `validate` 必须显式提供 `after: run | halt`，活动连接拒绝 `after`。测试结束后目标稳定运行，未修改 Flash、冻结 OUT 或受保护的 SVN 文件。

## 执行记录

| 字段 | 记录 |
|---|---|
| 开始时间 | `2026-08-26T06:21:51.6331150Z` |
| 结束时间 | `2026-08-26T06:22:04.3176243Z` |
| 命令 | `& .\scripts\test-p1-ses.ps1`，外层记录 UTC 起止时间并原样返回退出码 |
| 结果 | `PASS`；退出码 `0` |
| 原始输出 | Codex 任务 `codex://threads/01a03bf8-d8c2-7e43-9626-06f420336dc2`，命令输出块 `f13266` |
| 源码定位 | 父提交 `b1ab9d21b065b18c4918e7e269410dcaf54fbe44` 加本记录所在的 `[P1-2.5][开发]` 原子提交 |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`；`cargo 1.98.0 (797e8a9bc 2026-08-05)`；`x86_64-pc-windows-msvc` |
| 操作系统 | `Microsoft Windows NT 10.0.26200.0` |

## 冻结身份

| 对象 | 冻结值 |
|---|---|
| J-Link DLL | `C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll`；6.98a；SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| 探针 | 序列号 `260106173`；硬件 `V10.10`；固件报告 `J-Link V10 compiled Jun 27 2028 10:57:29` |
| 目标 | S32K144；Cortex-M4 r0p1；SW-DP ID `0x2BA01477`；SWD 4000 kHz |
| IAR OUT | `T26_DCU_APP_NXP.out`；SHA-256 `3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF` |
| T-P1-SES 合同测试 | SHA-256 `2113E185EB1E9B174A8DD76765EFB161F70D02D0DDF96E5FA26588199C1E445E` |
| Worker HardFault 测试 | SHA-256 `2F3A3827AE4B78ACBB3500FC701AC881A034532AC89E72F20E3B61F65CEBF7C4` |
| T-P1-SES 硬件驱动 | SHA-256 `866DC0FA8AD8964D33AE2DCD523F1DA9A4CD5926FC84D159AEFFE3C39F56D1C9` |
| jlink-worker | SHA-256 `9C6D69F1161C72DBCF7FC416C3ADCAB2F16AF69CC65C7676398940E3D07CA2B0` |

## 主要结果

- 严格合同测试 4/4 通过：唯一探针与具体器件、严格 connect 请求、HSS 断开冲突、显式 `validate.after`。
- halted 真机路径通过：首次可观察状态为 halted，唯一恢复流程执行 resume，通知 `resumed_from_halt`，最终 running。
- HardFault 真机路径通过：测试专用 `JLINKARM_WriteReg` 在同一 gateway 会话内注入真实异常，生产恢复流程观察 HardFault 后执行 reset/run，通知 `reset_after_fault`。
- 连接态 validate 只观察并拒绝 `after`；变化后的 ELF 身份不复用缓存；第二活动目标被拒绝。
- 断开态缺少 `after` 被拒绝；`after=halt` 返回 halted；后续 `after=run` 返回 running；两次均完成七项确定顺序检查。
- 公开稳定错误码保持 `TARGET_CONNECT_FAILED`；不存在合同外的 `TARGET_CONNECTION_FAILED` 或 `TARGET_VALIDATION_FAILED`。

## 安全恢复与受保护文件

- Commander 清理使用 S32K144、SWD 4000 kHz、探针 `260106173`，关闭 vector catch 并执行 go；最终 CPU running。
- 测试前后均无 `jlink-worker` 或 `JLink` 残留进程。
- `AppPwrMode.h` 前后 SHA-256 均为 `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085`。
- `T26_DCU_APP_NXP.dep` 前后 SHA-256 均为 `B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620`。
- 测试前后 `svn status -q` 只显示上述两处既有用户修改；未执行 `svn commit`。

## 复用与失效条件

本证据仅在 2.5 会话与恢复实现、测试二进制、DLL、探针、目标、接口/速度、固件/OUT 身份及 `validate.after` 合同均未变化时复用。任一相关源码、二进制、硬件身份、固件身份或合同变化均使对应证据失效并要求重新验证。
