# J-Link MCP 全阶段工具实测问题清单

## 状态与处理门禁

本清单记录 2026-08-28 起通过 Codex 插件 `jlink_mcp` 对 T26/S32K144 真机执行全阶段工具测试时发现的问题。全部 11 个步骤已完成；按照项目计划 P002 和本窗口确认的修复顺序直接处理，不创建独立 OpenSpec change。每一阶段只修改本阶段涉及的实现、Schema 或 Skill，并保留对应回归证据。

当前固定输入：Windows x64、J-Link DLL 6.98a、探针 S/N `260106173`、S32K144、SWD 4000 kHz、T26 `T26_DCU_APP_NXP.out`。实际调用只使用 `mcp__jlink_mcp__*`；旧用户级注册 `jlink-MCP-v2`（工具命名空间 `jlink_MCP_v2`）已移除。重启后的新窗口只发现插件 `jlink_mcp` 的六个工具，且 `jlink_target.config_get` 调用成功。

## 待处理问题

| ID | 状态 | 问题 | 后续验收方向 |
|---|---|---|---|
| FT-001 | 完成 | Codex 曾同时暴露用户级 `jlink-MCP-v2`（工具命名空间 `jlink_MCP_v2`）和插件级 `jlink_mcp`，两者各提供同一组六工具，且指向不同路径和不同二进制指纹。2026-08-28 已移除旧注册并重启 Codex；新窗口仅发现插件 `jlink_mcp` 的 `jlink_target`、`jlink_program`、`jlink_inspect`、`jlink_write`、`jlink_control`、`jlink_hss`，旧命名空间工具数为 0，`jlink_target.config_get` 成功返回当前配置。 | 已通过。后续重装回归继续确认单一服务发现。 |
| FT-002 | 已确认 | 六个工具描述重复公共合同、`structuredContent` 和副作用恢复说明，增加每个 `tools/list` 的固定上下文。 | 公共规则只保留一个权威入口；工具描述仅保留自身 action、关键约束和必要示例。 |
| FT-003 | 已确认 | `jlink_hss` 的联合输出在多个分支重复展开完整 quality 结构，Schema 体积明显高于其他工具。 | 在不改变严格输入输出、四种 query view 和客户端兼容性的前提下复用公共定义，并比较优化前后实际 `tools/list`。 |
| FT-004 | 已确认 | Skill 根规则、reference、MCP instructions 和工具描述存在内容重叠；每个新的 J-Link 用户回合还会重新加载完整根 Skill。 | 明确各层信息所有权，保持根 Skill 路由加按需 reference 的渐进披露；不得删除状态复用、副作用和错误恢复规则。 |
| FT-005 | 完成 | 根因是寄存器已在目录中命中后，DLL 单项读取状态非零仍被映射成 `REGISTER_NOT_FOUND`。修复后只有目录缺失返回该错误；running 返回 `TARGET_STATE_INVALID`，halted 仍失败返回 `TARGET_CONNECT_FAILED`。 | 单元、公共 Schema/错误 smoke、真机 running→halted→running 回归通过。 |
| FT-006 | 完成（预期生命周期） | 同一 MCP PID 内 Worker PID 稳定，连续寄存器/控制调用没有断连；MCP 结束后 Worker 随父进程退出，新 MCP 生命周期从 `disconnected` 和新 PID 对重新开始。 | 已证明阶段间断连与 MCP 父进程生命周期一致，不修改 Worker 生命周期代码。 |
| FT-007 | 兼容性复核 | 错误同时出现在 `content.text`、`structuredContent.error` 和 `isError`，存在信息重复，但可能服务于不同 MCP 客户端兼容层。 | 先验证目标客户端实际消费路径；不得仅为减小文本删除结构化错误或破坏兼容性。 |
| FT-008 | 已确认 | `jlink_write.variable` 的实时 Schema 将顶层数组限定为 `Array<string>`，但数组切片传入 `["17","34"]` 时运行时返回 `VALUE_INVALID`，并要求 JSON integer 或 `$int`；同一数组放入结构体对象并使用整数元素可以成功写入。 | 统一 Schema、反序列化和运行时整数模型；增加标量数组整写、切片写、结构体内嵌数组及超出安全整数范围的合同测试。 |
| FT-009 | 完成 | 根因是 Flash 主操作成功后，要到后置状态也成功才记录 Flash 已修改。修复后主操作成功立即使固件/验证证据失效；后置失败关闭目标并返回不可重放的 `EXECUTION_UNCERTAIN`，固定报告操作、阶段、`after`、Flash 修改事实和原始错误码。 | 单元测试、workspace 门禁和真机故障路径通过；擦除未重放，状态立即为 `faulted/unknown`，随后完成固件重烧、独立 verify 和 running 恢复。 |
| FT-010 | 完成 | 每项检查新增必填 `evidence`。running 每次实际执行 ICSR；halted/HardFault 只能复用同一连接中已成功的 running 证据，detail 明确当前状态和复用来源。 | 真机 halted validate 返回 `target_state=halted`、`background_access.evidence=reused`，不再伪称本次在 running 执行。 |
| FT-011 | 完成 | 当前 Worker/目标指纹内复用 DLL、导出、探针、目标身份、接口和 HSS 能力；目标状态始终重观测。Flash 清固件身份和动态后台证据，连接/Worker 变化全清。 | 首次、连续复用、halted、Flash 后失效、resume 重建和新 Worker 全执行矩阵通过。 |
| FT-012 | 已确认 | 1 kHz 结构体采样下，`around_event` 在 5 ms 前后窗口返回所有连续变化成员及逐项 relation；单个写事件产生数十项与写入无直接关系的变化，而当前 Schema 没有 `series` 过滤字段。 | 保留事件、时间不确定度和相关变化证据；评估增加可选 `series` 过滤或默认有界摘要，完整原始变化继续通过 `window/changes` 获取，并验证分页确定性。 |
| FT-013 | 待优化 | `status(completed)` 与紧随其后的 `query(overview)` 重复返回完整 quality、采样范围和捕获身份；异步流程又需要先等待终态再进入查询，形成正常调用链中的固定重复。 | 在保持 `status` 和离线 `overview` 可独立解释的前提下，比较终态摘要、客户端可信缓存或增量响应方案；不得删除质量证据或让客户端猜测捕获完整性。 |
| FT-014 | 待优化 | 并发矩阵首次使用 15 s 固定捕获，但 Agent 与工具多次往返消耗了捕获窗口；执行 `halt` 时捕获已自然结束，导致该结果不能证明运行期冲突，并额外重做一次 60 s 捕获。 | 在 Skill 和测试编排中根据操作数量、往返耗时上界和安全余量选择捕获时长，并在批量操作前确认捕获仍为 `running`；不得改变 HSS 固定时长或无手动停止的合同。 |
| FT-015 | 待优化 | 对会被 1 ms 任务消费并清零的控制变量使用同步 `verify=readback` 时，写入已生效仍会因最终值恢复为 0 而返回 `VERIFY_FAILED`，容易被 Agent 误判为未写入并危险重试。 | Skill 明确将命令、触发、握手和自清零变量默认写为 `verify=none`，随后按业务状态验证效果；说明 `VERIFY_FAILED` 只证明最终回读不匹配，不能单独证明写入从未发生，并增加不自动重试的流程验收。 |
| FT-016 | 待优化 | HSS Capture Store 当前从用户级探针租约根目录派生，实际位于 `%LOCALAPPDATA%\jlink-mcp\leases\captures\<probe_identity_hash>`；不同工程共用同一探针时，采集文件不能按工程自然隔离、定位和管理。 | 将 Capture Store 与用户级探针租约路径解耦，按工程确定稳定的存储根目录并保持同工程查询可定位；本轮修复中冻结具体目录、工程身份隔离和既有文件兼容策略，不静默移动或删除已有采集。 |
| FT-017 | 已确认 | 当前 Codex 通用资源读取链路对 201,208-byte 的完整 `.capture` 只交付 47,798 个 Base64 字符；完整编码应为 268,280 个字符，且回读长度不能被 4 整除，没有显式截断错误。磁盘文件和服务端完整编码路径均正常，因此该返回不能作为原始资源。 | Codex 客户端应无损接收资源或把二进制保存为不进入模型上下文的文件/句柄；以 201,208 bytes、`JMCPV101` 和 SHA-256 `A57C54A9E44FEC68E267FD9C010713BACA3F6B6AB8FD52D231307A9AB3CB8060` 做端到端验收。任何输出上限必须返回显式错误，不能把截断 Base64 当作成功资源。 |

## 修复执行顺序

1. **安装与发现（FT-001，完成）**：已移除旧用户级 MCP 注册；重启后仅发现插件 `jlink_mcp` 的六个工具，旧工具数为 0，`config_get` 成功。
2. **高风险功能正确性（FT-009、FT-008、FT-005、FT-010、FT-006）**：先修复可能诱发破坏性重试的 Flash 分阶段结果，再统一数组值模型、寄存器错误分类和状态文案；最后复现并归因阶段间断连。
3. **验证状态复用（FT-011）**：拆分静态与动态检查的失效条件，明确返回 `executed/reused`，验证错误矛盾时会强制失效。
4. **Capture Store 与 HSS 合同（FT-016、FT-012、FT-013、FT-003）**：先固定工程级存储边界，再处理事件查询有界化、终态反馈去重和 Schema 公共定义复用。
5. **工具说明与 Skill 编排（FT-015、FT-014、FT-002、FT-004）**：在运行时合同稳定后补齐自清零变量、捕获时长预算和会话状态复用规则，并消除 MCP instructions、工具描述与 Skill 的无必要重复。
6. **客户端兼容与资源交付（FT-007、FT-017）**：验证错误的三层兼容消费路径和大资源无损交付；客户端能力不足时返回显式错误或文件句柄。
7. **最终回归**：重新安装当前插件，在新 Codex 窗口执行六工具发现、成功/故障调用、HSS 四种查询和上下文前后测量，再更新本清单状态。

## 明确不作为缺陷的重复

- 本次控制验收在 `halt`、`resume` 和两种 `reset` 后读取 `status`，目的是独立验证实际状态转换；普通连续调试仍应复用成功 action 建立的可信状态，不机械查询。
- 控制成功返回 `{}` 后再读取状态，是测试证据而不是日常接口要求。
- `config_get` 的 `effective/sources`、联合体原始字段与 `Bits`、逐项 `validate.checks` 具有不同语义，不纳入去重范围。
- HSS 分页首屏返回 dictionary、续页不重复 dictionary，属于已验证的正确压缩行为；调用方应保留 `next_cursor`，不得为了恢复游标机械重放首屏。
- MCP 对自清零控制字返回 `VERIFY_FAILED` 本身不作为实现缺陷；FT-015 记录的是 Agent/Skill 缺少变量语义判断和安全验证流程的问题。

## CPU 控制阶段证据摘要

| 顺序 | 调用与反馈 |
|---|---|
| 会话恢复 | 初始 `PC` 读取返回 `TARGET_STATE_INVALID`；`status=disconnected`；`connect` 返回 `resumed_from_halt`；连接态 `validate` 七项全部通过。 |
| 运行态寄存器 | `jlink_inspect.register(PC)` 返回 `REGISTER_NOT_FOUND`，`dll_status=255`。 |
| Halt | `jlink_control.halt` 返回 `{}`；`status=halted`；`PC=0x0002951E`。 |
| Step | `jlink_control.step` 返回 `{}`；仍可在暂停态读取，`PC=0x00029F54`。 |
| Resume | `jlink_control.resume` 返回 `{}`；`status=running`。 |
| Reset halt | `jlink_control.reset(after=halt)` 返回 `{}`；`status=halted`；`PC=0x0003BEE0`。 |
| Reset run | `jlink_control.reset(after=run)` 返回 `{}`；`status=running`。 |
| 固件执行 | `gucCddAdcCount` 连续回读 `5715 → 5771`，证明最终固件继续执行。 |

CPU 控制 action 本身通过；FT-005 的公共错误分类已在后续修复回归中完成。最终目标保持 `connected/running`，本阶段没有变量写入、Flash 操作或 HSS。

### FT-005/006 修复回归

- 第一生命周期固定为 MCP PID `6984`、Worker PID `63616`。running 状态读取 `PC` 返回 `TARGET_STATE_INVALID`，详情为 `dll_status=255`、`target_state=running` 和显式 halt 建议；不再误报 `REGISTER_NOT_FOUND`。
- `halt` 返回 `{}`，连续两次读取 `PC` 均成功并返回 `0x00029566`；MCP/Worker PID 在连续调用前后未变化。`resume` 返回 `{}`，只读状态证据为 `connected/running`。
- MCP 进程结束后，原 MCP/Worker 均在 5 秒内退出。新生命周期先返回 `connection=disconnected`，随后 connect 建立新的 MCP PID `53396`、Worker PID `19748`；这证明 FT-006 是父进程边界，不是同一 MCP 内 Worker 丢失。最终断开时 CPU 保持 running。

### FT-010/011 修复回归

- 断开态 `validate(after=run)` 的七项检查全部为 `evidence=executed`。连接后连续两次 running validate 中，DLL、导出、探针、目标身份、接口和 HSS 六项均为 `reused`，`background_access` 每次为 `executed`，`validation_runs` 单调递增。
- 显式 halt 后 validate 返回 `target_state=halted`；后台检查为 `reused`，detail 明确“目标当前为 Halted；复用同一连接中已成功的运行态证据”，没有伪称本次在 running 执行。
- `flash(verify=true, after=none)` 成功后目标保持 halted；下一次 validate 仍复用六项静态证据，但后台检查为 `executed/failed` 并明确不存在同一连接的成功 running 证据，证明 Flash 已清除动态缓存。resume 后 running validate 重新执行 ICSR 并恢复 `valid=true`。
- 结束 MCP/Worker 后启动新生命周期，首次 `validate(after=run)` 的七项再次全部为 `executed`、`validation_runs=1`；旧 Worker 证据没有跨生命周期复用。最终 CPU 保持 running。

## 安全写入与 Flash 阶段证据摘要

### 构建与固件身份

- 使用 IAR ARM V8.32.3.193 构建 `S32K144` 配置，结果为 0 error、0 warning。
- 新 OUT 为 `T26_DCU_APP_NXP.out`，大小 4,964,348 bytes，SHA-256 `85D08B6E3AD9B1CEC350AA3782520B0D6DEDA392AA11CD2EE62BEEEDBCB8EA42`。
- Map 与 DWARF 均确认 `stTest=0x1FFFA620`、`ucTestflg=0x1FFFA640`；符号发现返回 `stTest.ucNum`、`stTest.uwData`、`stTest.uwData1` 和 `stTest.ucData2`。
- 初次烧录使用 `jlink_program.flash(after=reset_run, verify=true)`，返回 `{}`；独立 `verify` 和连接态七项 `validate` 均通过。

### 第 6 步：安全写入

| 顺序 | 调用与反馈 |
|---|---|
| 基线 | `ucTestflg=0`、`stTest` 全零；地址 `0x1FFFA640` 原始字节为 `00`。 |
| 标量变量 | 写 `ucTestflg=3` 并 readback 成功；变量回读为 `3`，原始内存为 `03`；随后写回 `0`。 |
| 结构体成员 | `ucNum=90`、`uwData=13398`、`uwData1=2309737967` 均写入并回读成功。 |
| 数组合同 | 对 `stTest.ucData2[0:2]` 传入 Schema 允许的 `["17","34"]`，返回 `VALUE_INVALID`；改为完整结构体对象内嵌整数数组后写入和回读成功，记录为 FT-008。 |
| 固件触发 | 写 `ucTestflg=1` 且不做同步 readback，固件将标志清零，并把结构体更新为 `1 / 0x1234 / 0x12345678 / [0x11,0x22,...]`；写 `2` 后标志和结构体全部清零。 |
| 原始 RAM | 向 `0x1FFFA640` 写 `03` 并 readback，DWARF 变量同步显示 `3`；写回 `00` 后变量为 `0`。 |
| 核心寄存器 | halt 后读取 `R0=0x000000DB`，写为 `0xA5A5A5A5` 并回读，再精确恢复 `0x000000DB`；resume 后 `status=connected/running`。 |

除 FT-008 外，第 6 步写入、交叉回读和原值恢复均通过。自动消费的控制标志使用 `verify=none`，随后验证固件状态变化；这避免把业务代码立即清零误判为写入失败。

### 第 7 步：Flash 校验、擦除与恢复

| 顺序 | 调用与反馈 |
|---|---|
| 擦除前校验 | 独立 `jlink_program.verify` 返回 `{}`。 |
| 整片擦除 | `erase(after=reset_halt)` 返回 `TARGET_CONNECT_FAILED`，失败点为读取 `0xE000ED04`；未重放擦除。 |
| 状态核对 | 首次 `status` 为 `connected/running`，但 verify 读取 `0x00000000` 失败，halt 也失败并使状态转为 `faulted/unknown`。 |
| 空片恢复到可验证状态 | 重新 connect 后，首次 validate 的 `background_access` 未通过；执行 `reset(after=halt)` 后 validate 七项通过，`target_state=halted`。 |
| 擦除效果 | 独立 verify 返回 `VERIFY_FAILED`：`first_address=0x0`、`first_length=784`、`total_regions=1822`，证明擦除已生效。 |
| 重烧恢复 | `flash(after=reset_run, verify=true)` 返回 `{}`；独立 verify 返回 `{}`；连接态 validate 七项通过且 `target_state=running`。 |
| 固件运行 | `ucTestflg=0`、`stTest` 全零；`gucCddAdcCount` 连续回读 `1511 → 1518`。 |

第 7 步最终通过，目标已经恢复为新固件并保持 `connected/running`。FT-009 是本阶段的主要可靠性问题：擦除已执行却返回普通可重试连接错误；正确处理必须禁止直接重放，并通过状态重建和只读证据确认实际 Flash 状态。

### FT-009 修复回归

- 使用修复后的本地 release 二进制再次执行一次整片擦除。主操作成功、后置 ICSR 读取失败时返回 `EXECUTION_UNCERTAIN`、`retryable=false`；`details` 为 `operation=erase`、`phase=post_action`、`after=reset_halt`、`flash_modified=true`、`cause_code=TARGET_CONNECT_FAILED`。
- 紧随其后的只读 `status` 为 `faulted/unknown`，不再保留错误的可信连接状态；未重放 erase。原第 7 步已保留同一 DLL/探针路径下独立 verify 的 `VERIFY_FAILED` 空片证据。本次空片运行态 Flash 读取和 halt 均不可用，因此没有把访问失败冒充内容证据。
- 断开并重建会话后，`flash(verify=true, after=reset_run)` 和独立 `verify` 均返回 `{}`，`status` 为 `connected/running`；最终安全断开且测试固件继续处于运行态。

## HSS 1 kHz 与并发阶段证据摘要

### 测试固件与烧录

- 在 `OsUserConfig` 中增加独立 `TASK_ID_HSS_TEST`，由现有 `TASK_1MS` 调度；原任务 ID、`ucTestflg` 和 `stTest` 均未改动。
- 采样状态包含 32 位序号、变量/内存写入回显、常量哨兵、0..999 ramp、1 ms toggle、四相 phase 和四元素 pattern；控制字支持冻结、运行和复位。
- IAR `S32K144` 配置最终全量构建结果为 0 error、27 warning；warning 均为工程已有的 `LOCAL_INLINE` 重定义。OUT 大小 4,967,220 bytes，SHA-256 `18F0D0BFA4B79CCDE12E2BB4B36B51F7E73ED1EFE8E6232B8EE6F138C8ABCA18`；独立 `program.verify` 证明当前板上 Flash 与该 OUT 的可加载内容一致，无需因调试元数据变化重复烧录。
- Map/DWARF 确认 `gstHssTestInfo=0x1FFFBA84`、`gucHssTestCtrl=0x1FFFBA9C`、`gulHssVariableCmd=0x1FFFBAA0`、`gulHssMemoryCmd=0x1FFFBAA4`。烧录使用 `flash(after=reset_run, verify=true)`，前后两次必要的 `validate` 均为七项通过。

### 第 8 步：固定 1 kHz 捕获与查询

| 项目 | 结果 |
|---|---|
| 捕获 | `cap_07163a022577bc6606a5e111`，5 s、1000 Hz、4 个顶层 selector；状态经历 running → completed。 |
| 采样 | expected 5000、actual 4999，实际速率 1000 Hz；相邻时间戳全部为 1000 us，collision/gap/regression 均为 0。 |
| 固件波形 | sequence 每 1 ms 递增，ramp 在 0..999 周期运行，toggle 每 1 ms 翻转，phase 按 0..3 循环，常量和两个未写命令保持不变。 |
| 规则 | ramp 上穿 500 的规则命中：`after_us=906000`、`observed_by_us=907000`，与 1 ms 采样边界一致。 |
| 查询视图 | `overview`、`changes`、`window(raw/transitions/min_max/first_last)` 均返回可解释结果。 |
| 分页 | 首屏 20 项并返回 8 项 dictionary；续页 20 项，dictionary 为 0 项且 `next_cursor` 继续有效，符合续页不重复字典的合同。 |
| 质量边界 | DLL 无独立 overflow/sequence 信号，因此 integrity、loss、overflow 均为 unknown；不能仅凭无 gap 宣称完整或无丢样。 |

第 8 步功能通过。4999/5000 的边界差异没有被错误报告为丢样；当前证据只能确认实际记录的节拍连续，不能证明捕获完整性。

### 第 9 步：捕获期间写入、事件关联与冲突矩阵

| 项目 | 结果 |
|---|---|
| 事件证据捕获 | `cap_3aae28dac5b33064c0e0797b`，15 s、1000 Hz；typed variable 写 `0x11223344`、raw memory 写 `0x55667788` 均成功并产生 `e0/e1`。两个回显分别在写事件后约 1 ms 出现在采样中。 |
| 事件语义 | mapping uncertainty 为 2235 us；命令变化可判为 overlaps，1 ms 后回显为 indeterminate。工具没有把时间邻近错误提升为确定因果。 |
| 并发判定捕获 | `cap_65e326e673ad67c11253547c`，60 s、1000 Hz；expected 60000、actual 59996，间隔仍全部为 1000 us，无 collision/gap/regression。 |
| 允许操作 | 捕获 running 时 `target.status` 可用；typed variable 写 `0xA1B2C3D4` 和 raw memory 写 `0xCAFEBABE` 均成功，并被 1 ms 任务回显。 |
| 拒绝操作 | 捕获 running 时普通 `inspect.variable`、`program.verify`、`control.halt`、`target.disconnect` 均返回 `OPERATION_CONFLICT`。 |
| 事件查询 | `changes` 返回两个成功写事件、对应规则命中和有界 relation；`around_event` 能定位事件，但暴露 FT-012 的高上下文问题。 |

第 9 步功能通过。最初 15 s 捕获在测试 halt 前自然结束，因此该次 halt 成功不能作为并发证据；随后用 60 s 捕获在明确 running 状态下重测完整冲突矩阵，排除了调用延迟造成的误判。

### 第 10 步：完成态 Capture Store 离线读取与原始资源

本步骤不测试已经移除的“离线持续 HSS 采集”。只复用第 8 步已完成的不可变 capture，验证目标断开后的状态、查询和原始资源读取。

| 项目 | 结果 |
|---|---|
| 断开目标 | `jlink_target.disconnect` 返回 `{}`；没有停止或新建 HSS。 |
| 离线状态 | 断开后读取 `cap_07163a022577bc6606a5e111`，`status=completed`、complete records 4999、expected 5000、actual 4999，quality 仍为 `unknown`。 |
| 离线查询 | 同一 capture 的 `overview` 成功返回 4 个顶层 dictionary、4999 samples 和 0 events；随后 `target.status=disconnected`，证明查询没有隐式连接目标。 |
| 资源发现 | 静态 resources 列表为空；参数化模板 `jlink-mcp://capture/{capture_id}/raw` 存在，MIME 为 `application/vnd.jlink-mcp.capture.v1+binary`，符合动态资源模型。 |
| 磁盘文件 | 当前文件位于用户级 Capture Store 的探针身份哈希目录，长度 201,208 bytes，文件头 `JMCPV101`，SHA-256 为 `A57C54A9E44FEC68E267FD9C010713BACA3F6B6AB8FD52D231307A9AB3CB8060`；该路径事实同时支撑 FT-016。 |
| 原始资源 | `read_mcp_resource` 返回正确 URI、MIME 和文件头前缀，但当前 Codex 链路仅交付 47,798 个 Base64 字符，而完整文件应为 268,280 个字符；结果不是完整标准 Base64，记录为 FT-017。 |
| 状态恢复 | `connect` 返回 `resumed_from_halt`；最终 `target.status=connected/running`。该通知符合成功 connect 会将暂停目标恢复运行的合同。 |

第 10 步部分通过：完成态持久化、断开态 status/query、动态资源模板和无隐式连接均通过；当前 Codex 客户端的完整原始资源交付未通过。测试结束后目标已恢复为 `connected/running`。

### 第 11 步：错误合同与无意外副作用

| 项目 | 调用与结果 |
|---|---|
| 非法 action | `jlink_inspect({"action":"invalid"})` 在协议入口返回 JSON-RPC `-32602`，说明不符合任一 `oneOf`；请求未进入工具实现。 |
| 额外字段 | `jlink_target({"action":"status","unexpected":true})` 在协议入口返回 JSON-RPC `-32602`，明确拒绝额外字段；请求未执行目标状态读取。 |
| 无效符号 | 读取 `__jlink_mcp_step11_missing_symbol__` 返回 `isError=true` 和权威 `structuredContent.error`：`SYMBOL_NOT_FOUND`、`retryable=false`。 |
| Flash 地址写入 | `jlink_write.memory(address=0x00000000,data=00,verify=none)` 返回 `ADDRESS_OUT_OF_RANGE`、`retryable=false`，details 为 `region=flash`、`use_tool=jlink_program`；普通内存接口未修改 Flash。 |
| 副作用核对 | 紧随其后的独立 `jlink_program.verify` 返回 `{}`，证明当前 Flash 仍与配置镜像一致。 |
| 最终状态 | `jlink_target.status` 返回 `connected/running`。 |

第 11 步通过。严格 Schema、协议错误、领域结构化错误和 Flash 写入边界均符合合同，没有产生意外副作用。领域错误同时出现在 `content`、`structuredContent.error` 和 `isError` 的现象仍由 FT-007 统一复核，不新增重复问题。

### 流程与反馈优化结论

- 正确流程可以复用当前 MCP 生命周期内的可信连接/验证状态；本阶段只在新固件烧录前后执行必要 `validate`，没有在每个查询前机械调用 `status`。
- 并发矩阵应在一次足够长的捕获中批量完成，避免 15 s 捕获因 Agent/tool 往返耗时而失效；本轮因此额外执行一次 60 s 捕获。
- 查询分页必须在同一调试上下文保留 cursor；本轮最终核查确认续页本身没有 dictionary 重复。
- MCP 侧需要统一评估 FT-011、FT-012、FT-013、FT-016；Codex 资源集成侧处理 FT-017；Skill 与测试编排侧处理 FT-014、FT-015。全阶段测试已完成，后续按本清单的修复执行顺序逐项修改和回归。

## 后续统一修复门禁

按照上述顺序在当前修复流程中实施，以本清单和 P002 为输入，不另建 OpenSpec change。最终至少验证：单一插件服务发现、六工具严格 Schema、数组值模型、Flash 分阶段副作用语义、空片恢复、`validate` 静态检查复用及失效规则、运行态/暂停态寄存器错误分类、成功与故障 `structuredContent`、HSS 四种查询、分页字典复用、`around_event` 有界输出、终态 quality 反馈去重、Capture Store 工程级路径与跨工程隔离、原始资源无损交付且不把大段 Base64 注入模型上下文、并发测试捕获时长预算与 `running` 前置证据、自清零变量的 `verify=none` 及业务状态验证、Skill 触发与状态复用，以及优化前后上下文的可比测量。
