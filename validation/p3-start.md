# P3 HSS 启动计划与预检证据

## 裁决

P3-4.2 为 `PASS`。生产实现已形成无副作用的规范化 `HssStartPlan`，覆盖固定时长、目标频率、1–10 个顶层 DWARF 选择项、40 字节展开载荷上限、固件身份绑定、启动规则的类型化保留与确定排序、稳定请求指纹和 `capture_key` 幂等/冲突。冻结 J-Link 6.98a 真机只读预检确认目标连接、运行态后台访问和 HSS 能力。公共 `jlink_hss start` 尚未接入；实际 Start、持续排空、内部 Stop 和尾排空属于 4.3，不在本任务中声明通过。

## 冻结合同

| 对象 | 冻结值 |
|---|---|
| 请求 | `capture_key`；`duration_s=1..300`；`rate_hz=1..1000`；`return_when=started|completed`；1–10 个顶层选择项 |
| 符号计划 | 当前 ELF/DWARF 生成的不可变 `AccessPlan`；动态位置、未定长选择或固件身份不一致在 DLL/目标动作前拒绝 |
| 帧上限 | 最多 10 个连续块；样本载荷最多 40 字节；加 4 字节毫秒源时间戳后记录最多 44 字节 |
| 6.98a 能力 | `max_blocks=10`；`max_frequency_hz=1000`；`flags=2`；保留字段全零；源时间 1000 Hz/1000 us、单调；不声明微秒分辨率 |
| 采集身份 | 探针身份、`capture_key` 和规范化请求指纹确定稳定 `cap_<24 hex>`；规则按唯一 ID 排序并进入指纹，顺序变化等价、内容变化冲突；非等价复用返回 `CAPTURE_KEY_CONFLICT` |
| 恢复 | 复用 SES-003；halted 先 resume，若进入 HardFault 再 reset_run，并同时保留 `resumed_from_halt` 与 `reset_after_fault` 通知 |

## 自动验证

| 命令 | 结果 | 覆盖 |
|---|---|---|
| `cargo test -p jlink-domain --test t_p3_start` | `PASS`；5/5 | 10×32-bit/44 字节布局、边界与能力拒绝、规则排序和指纹绑定、键幂等/冲突、IPC 后指纹复核 |
| `cargo test -p jlink-mcp --test t_p3_start` | `PASS`；3/3 | 冻结 IAR F0-C 复合成员和显式 slice 的真实 AccessPlan、未定长选择前置拒绝、闭合 Start Schema |
| `cargo test -p jlink-worker --lib t_p3_start_reuses_session_resume_then_reset_and_preserves_both_notices` | `PASS`；1/1 | HSS 预检复用 SES-003 且保留两阶段恢复通知 |
| `cargo test -p jlink-mcp --lib p3_start_errors_remain_structured_and_distinct` | `PASS`；1/1 | `HSS_UNSUPPORTED` 与 `CAPTURE_KEY_CONFLICT` 维持结构化且互不混淆 |

## 真机只读预检

| 字段 | 记录 |
|---|---|
| 时间 | `2026-08-27T04:27:29.8012718Z`–`2026-08-27T04:27:31.5814658Z` |
| 命令 | `cargo run -p jlink-mcp --example t_p3_start -- .\target\debug\jlink-worker.exe` |
| 结果 | `PASS`；退出码 `0`；`attach → connect → active validate → disconnect` |
| 目标 | S32K144；Cortex-M4；SWD 4000 kHz；探针序列号 `260106173` |
| HSS 能力 | `max_blocks=10`；`max_frequency_hz=1000`；源时间戳 `1000 Hz/1000 us`；`monotonic=true` |
| 恢复与最终状态 | connect 从 halted 收口为 running，通知 `resumed_from_halt`；预检后正常 disconnect，Worker 正常退出，相关进程数为 0 |
| 原始输出 | 本记录下方的结构化 JSON；Codex 任务 `codex://threads/01a0412c-18fb-7571-8ff3-b4e34a162f5a` 同一时间窗口的完整终端输出 |

```json
{"device":"S32K144","dll_sha256":"D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5","dll_version":"6.98a","elf_sha256":"F8ADB9A2B9BBFD26B469C66F2478EE6E22735302706B83509B2D4F2AE7F7738D","hss_capability":"max_blocks=10，max_frequency_hz=1000，source_timestamp=1000 Hz/1000 us，monotonic=true","interface":"SWD","probe_serial":260106173,"recovery_notifications":["resumed_from_halt"],"speed_khz":4000,"status":"PASS"}
```

## 身份与 SVN 安全边界

| 对象 | SHA-256 |
|---|---|
| J-Link 6.98a DLL | `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| J-Link Commander | `0340130E7AD4F90EA8F8973C42A34A6508F0C5F6E988D532BB03DE9060FDFC04` |
| IAR OUT | `F8ADB9A2B9BBFD26B469C66F2478EE6E22735302706B83509B2D4F2AE7F7738D` |
| `AppUserDesc.c` 夹具 | `1133B85709AB5ED3509ED58433ED4132E4D0869724140F8D3F560F7BA3B709E4` |
| 受保护 `AppPwrMode.h` | `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085` |
| 重新冻结 DEP | `4FDA4431B3502EBDB1B0313BF58B21995A2B962C9C0BA853DF42F3988B4A6F85` |
| 真机预检示例 | `5B133507810B26EFB2EA2CE00F0AB7A8B11C94151DC1C1289ABD4453EE94BF52` |
| 生产 Worker | `2EBB57B888FEACDF516D1ABE00425D0C58DA2AE9A673F7D6EFD4EEA810EB900A` |

- 硬件任务前后均读取完整 `svn status --depth infinity`；两次均为 612 行，内容 SHA-256 均为 `6827FD361AB388ABB26A6648158B0417CDDB76FAC515F91472C06B5715794685`。
- 三个受控 `M` 仍为既有 `AppPwrMode.h`、计划内 `AppUserDesc.c` 和重新冻结的 DEP；没有执行 `svn commit`，也没有烧录、擦除、HSS Start 或目标内存写入。
- Rust 为 `rustc 1.98.0 (88d9e12ae 2026-08-18)`、`cargo 1.98.0 (797e8a9bc 2026-08-05)`、`x86_64-pc-windows-msvc`；系统为 `Microsoft Windows NT 10.0.26200.0`。
- 源码基线为 `[P3-4.2][开发]` 提交 `38823aefd86c1e5e933e974303f7fe73cd809ec4` 加后续规则指纹修复提交。

## 边界与失效条件

- F0-A 的既有长时结果只作为 6.98a ABI、帧和毫秒时间戳参考；本次使用当前冻结 OUT 重新确认连接、后台访问和 HSS 能力，但没有复用旧固件行为来声明实际采集通过。
- 规则修复只改变无副作用的请求元数据、规范化和幂等判定；冻结 DLL、Worker gateway、目标连接、OUT 和真机预检路径均未变化，因此既有真机只读预检证据保持有效，无需重复连接。
- 本证据不证明 HSS Start、自动停止、持续/尾部排空、写入交错、质量事件、Capture Store 或父进程退出恢复；这些分别由 4.3–4.8 的主要测试和阶段纵向验收形成证据。
- DLL、探针、目标、接口/速度、OUT、DWARF 夹具、AccessPlan 编码、请求规范化、能力字段、恢复状态机、IPC 或预检示例任一变化时，相应证据失效。
- JTAG、Windows Codex 客户端验收、5.6、5.7、OpenSpec 归档和发布仍待办。
