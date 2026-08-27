# P3-4.8 阶段检查点证据

## 裁决

`PASS`

P3 的 4.1–4.8 已完成。冻结生产链路在 S32K144、SWD 4000 kHz 和 J-Link 6.98a 上完成一次连续 10×32-bit、1 kHz、300 秒 HSS：原 MCP 退出后同一 Worker 继续采集，新 MCP 按 `capture_key` 恢复同一 capture；活动窗口内只允许串行 RAM 写入并读回恢复，固定时长后自动 Stop、尾排空、原子发布完成文件，最终变量原值和 CPU running 状态均已确认。本记录只是 P3 实现检查点，不代表 P4、5.6、5.7、JTAG 真机或发布完成。

## 执行记录

| 字段 | 记录 |
|---|---|
| 硬件任务窗口 | `2026-08-27T07:26:39Z`–`2026-08-27T07:31:43Z`；包含 HSS Start、父进程退出/重新附着、300 秒采集、尾排空和后置安全检查 |
| 真机 smoke 命令 | `& .\scripts\test-p3-smoke.ps1` |
| 真机 smoke 结果 | `PASS`；退出码 `0` |
| capture | `cap_5b84206ec81ea3bd9df6952b`；key `p3-4.8-300s-1khz`；终态 `completed`；完整性 `unknown` |
| 固定计划 | 1 个顶层连续数组 slice，10×32-bit；帧载荷 40 bytes；记录 44 bytes；请求 1 kHz、300 秒 |
| 完整记录 | `300012`；原始载荷 `13200528` bytes；完成文件 `13847583` bytes |
| 时间 | Worker elapsed `300045168 us`；源 `0..300011000 us`；`300011` 个间隔均为 `1000 us`；实际频率 `1000000 mHz` |
| 排空 | `146385` 次；累计 `17754478 us`；最大单次 `17130 us` |
| 交错写入 | 2 次 RAM 写入均 `succeeded` 且显式读回；分别在下一次排空关联到记录 `1134` 和 `1359`；最终恢复 `78563412` |
| 父进程恢复 | 原 MCP 退出后 Worker PID 不变；新 MCP 按 key 返回同一 capture ID，活动读取返回 `OPERATION_CONFLICT`，未执行第二次 Start |
| Capture raw SHA-256 | `cc1ae1d4f5c23fdff59376381c1afc41efdc1b1342feceb3fe52569b6eeb374c` |
| Capture 文件 SHA-256 | `EC9CC9DE367F998577869A97D6E00B87324157F0A756991A5F0F6C543F4BD354` |
| 本地复用资源 | `target\evidence\p3-4.8\capture-cap_5b84206ec81ea3bd9df6952b.capture`；忽略目录，不进入 Git |

## 质量事实

- `requested_rate_hz=1000`、`expected_samples=300000`、`actual_samples=300012`。实际记录数和源首尾时间是观测事实，不把请求样本数当作精确完成计数。
- 源时间单位为 `milliseconds`，频率 `1000 Hz`，分辨率 `1000 us`；规范化单位为 `microseconds`，映射方法为 `capture_start_call_bound`，误差上界 `2690 us`。
- 相邻时间戳没有 collision、gap 或 regression，但冻结 6.98a ABI 没有独立 overflow/sequence counter。因此 `loss` 与 `overflow` 均为 `unknown/no_independent_overflow_or_sequence_counter`，没有输出 `lost_samples=0` 或 `events=0`，不得据此宣称零丢样或无溢出。
- 生命周期 `completed` 与完整性 `unknown` 保持独立；本次正常 Stop/尾排空不提升无法证明的数据完整性。

## 父进程、写入与恢复

- Start 返回 `running` 后只终止原 MCP 父进程，不终止其 Worker。脚本确认活动 Worker PID 未变化，再由使用相同工程/用户配置的新 MCP 通过 `capture_key` 取得同一 ID。
- 重新附着后，活动 HSS 的普通内存读取按合同返回 `OPERATION_CONFLICT`。两次 RAM 写入均经过唯一 Worker gateway 串行执行并显式读回；第二次立即恢复测试变量原值。
- 写入证据分别为：请求 `31812-4`，`requested/started/completed=1128463/1128488/1131616 us`，记录 `1122→1134`；请求 `31812-5`，`1364113/1364131/1366849 us`，记录 `1359→1359`，下一次排空仍已关联。
- 完成边界上原 Worker按父进程规则退出；状态只读重新附着/恢复完成索引后，新 MCP 重新连接同一冻结目标。最终内存读回为 `78563412`，目标 status 为 `connected/running`，随后 disconnect；结束时 J-Link 相关进程数为 0。

## 阶段门禁与根因修正

- 4.8 最终代码执行 `scripts/check-workspace.ps1`：workspace 格式、workspace Clippy `-D warnings`、workspace tests、四 crate 数量和依赖方向全部 `PASS`；随后 OpenSpec strict 和 `git diff --check` 均 `PASS`。
- 首次 workspace test 发现 P1 旧测试仍把 4.7 已接通的 `jlink_hss.status` 当作未来 action。最短复现为 `t_p1_mcp_runtime_keeps_remaining_future_actions_unavailable`；仅将该断言改为仍待 P4 的 `query.overview`，定向测试和完整阶段门禁复跑均通过，生产代码未因门禁失败修改。
- 300 秒 HSS 只执行一次。此前验收脚本的两次机械校正分别发生在本地 `config_set` 成功响应解析和一次 connect 返回形状检查；第一次没有创建 Worker，第二次只连接后安全释放，均未调用 HSS Start。修正统一遵循当前生产输出 Schema，没有重试采集、放宽断言或修改生产结果。

## 冻结身份与 SVN 现场

| 对象 | 冻结值 |
|---|---|
| J-Link DLL | `C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll`；6.98a；SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| J-Link Commander | 6.98a；SHA-256 `0340130E7AD4F90EA8F8973C42A34A6508F0C5F6E988D532BB03DE9060FDFC04`；成功路径未使用 |
| 探针/目标 | 序列号 `260106173`；S32K144/Cortex-M4；SWD 4000 kHz |
| IAR OUT | SHA-256 `F8ADB9A2B9BBFD26B469C66F2478EE6E22735302706B83509B2D4F2AE7F7738D` |
| 测试夹具 | `AppUserDesc.c`；SHA-256 `1133B85709AB5ED3509ED58433ED4132E4D0869724140F8D3F560F7BA3B709E4` |
| smoke 驱动 | `scripts/test-p3-smoke.ps1`；SHA-256 `BBD502D92BD0CC17883F5A655D8EE03D9C4E5B7172EDADA357BE2E35387A743B` |
| Capture 检查器 | `t_p3_capture.exe`；SHA-256 `F51980555EEFFB2B83E25A5DDF56D2272E759042F2A69AF8896F647C868CE659` |
| 生产 MCP/Worker | MCP `ED0337FC8EBC9279E4C5A74CFD81EF4E41F3157A67FA2583A5D9B70B21A3D8FE`；Worker `33BDC1A032C5E535000CF5E1FF07ED507F0645140F8C9406F6E183314D6D3BEE` |

- P3 首次硬件任务前和阶段结束时的完整 SVN 状态均为 612 行：609 个既有未跟踪构建/工具文件、3 个已知 `M`；内容 SHA-256 均为 `6827FD361AB388ABB26A6648158B0417CDDB76FAC515F91472C06B5715794685`。
- `AppPwrMode.h` 保持 `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085`；`T26_DCU_APP_NXP.dep` 保持 `4FDA4431B3502EBDB1B0313BF58B21995A2B962C9C0BA853DF42F3988B4A6F85`；没有执行 `svn commit`。
- 本阶段没有修改目标工程或重新构建 IAR，复用用户授权的 OUT/DEP 基线；测试结束后夹具、OUT 和保护文件哈希均未变化。

## 范围和失效条件

4.8 是跨组件阶段 smoke，不替代 4.1–4.7 的唯一主要测试。它证明当前冻结输入下的实际 HSS Start、父进程续行/按 key 恢复、写入交错、300 秒固定时长、自动 Stop、尾排空、质量事实和完成文件组合链路。它不证明 JTAG 真机、所有异常注入、查询视图、Codex 客户端或发布门禁。DLL、探针、目标、接口/速度、OUT、夹具、HSS ABI、调度/恢复逻辑、Capture Store 格式、质量算法、IPC/Schema 或 smoke 驱动任一相关变化时，本阶段对应证据失效。
