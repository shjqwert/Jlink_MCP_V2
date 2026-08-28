# Codex Plugin Integration Specification

## Purpose

定义 J-Link MCP 在 Codex 中的可安装插件边界，使新的本地环境能够发现服务器和使用指南，并以低上下文成本安全选择、调用和解释固定六工具 V1 合同。

## Requirements

### Requirement: CPI-001 仓库内插件可安装
系统 MUST 提供有效的 repo-local Codex marketplace 条目和 `jlink-mcp` 插件清单；插件 MUST 同时声明一个 MCP server 和一个可隐式发现的同域 Skill。安装配置 MUST NOT 固化开发电脑的绝对仓库路径，启动时 MUST 使用可移植的插件或安装位置，并确保 `jlink-mcp.exe` 与其同目录 `jlink-worker.exe` 成对可用。

#### Scenario: 新环境安装插件
- **WHEN** 用户在已构建或已安装产品二进制的新 Windows x64 环境中添加仓库 marketplace 并安装 `jlink-mcp` 插件
- **THEN** Codex 将插件列为已安装且启用，并能在新任务中启动 MCP、枚举固定六工具目录和发现 `jlink-mcp` Skill

#### Scenario: 产品二进制缺失
- **WHEN** Codex 尝试启动插件但找不到 MCP 主程序或同目录 Worker
- **THEN** 启动明确失败并指明缺失的产品文件或安装前置条件，不回退到其他全局 `jlink` 项目

### Requirement: CPI-002 单一 Skill 渐进式路由
插件 MUST 只提供一个 `jlink-mcp` Skill 作为六工具 V1 的 Agent 使用入口。入口 MUST 使用判别性触发描述，列出工具/action 路由、共享安全不变量和 reference 加载条件；详细指导 MUST 分离为目标会话、固件编程、调试访问、HSS 和错误恢复五个 reference，并且普通请求不要求一次加载全部 reference。

#### Scenario: 单域操作加载指导
- **WHEN** Agent 需要执行一次目标会话、编程、调试访问或 HSS 操作
- **THEN** Skill 指导 Agent 只读取对应业务 reference，并仅在调用失败或结果不确定时再读取错误恢复 reference

#### Scenario: 无关请求不触发
- **WHEN** 用户讨论非 SEGGER J-Link、非本 MCP 的一般嵌入式开发问题
- **THEN** Skill 的发现描述不把该请求路由到 `jlink-mcp` 工具

### Requirement: CPI-003 运行时 Schema 保持语法权威
Skill 和 server instructions MUST 将运行时 `tools/list` 的输入输出 Schema 作为字段名称、类型、枚举、必填性和额外字段拒绝规则的唯一语法权威。指导内容 MUST NOT 复制完整 Schema、发明包装对象、增加未声明字段，或形成独立的公共参数合同；仅可保留能澄清跨字段语义的最小调用骨架。

#### Scenario: Agent 准备工具调用
- **WHEN** Agent 根据 Skill 选择了工具和 action
- **THEN** Agent 先依据当前工具 Schema 构造最小参数对象，并且不会从 Skill 文档猜测 Schema 未声明的字段

#### Scenario: HSS 查询续页
- **WHEN** HSS 查询返回 `next_cursor`
- **THEN** 指导要求续页调用只携带 `action`、且仅一个 capture 身份和 `cursor`，省略 `view` 及所有视图字段

### Requirement: CPI-004 状态前置条件指导
Skill MUST 明确 Schema 无法单独表达的会话约束：`validate.after` 随连接状态变化，`config_set` 仅能在断开且无活动 HSS 时执行，`step` 要求目标已 halted，HSS 活动期间仅允许 target status、HSS status/query 以及 variable/memory write，其余需要 DLL 的操作按冲突处理。当前 MCP/Worker 生命周期内已有可信状态时 MUST 复用该状态，不得在连续调试调用之间机械重复 target status；仅当状态未知、失效或与返回结果矛盾时才查询。

#### Scenario: 状态不满足
- **WHEN** Agent 准备执行依赖当前会话状态的调用
- **THEN** Agent 先查询或利用可信状态，满足前置条件后调用，否则停止或选择合同允许的状态转换而不臆造隐式转换

### Requirement: CPI-005 副作用错误安全恢复
错误指导 MUST 以 `structuredContent.error` 为机器可读权威，并把 `retryable` 解释为错误证据而非副作用重放授权。MCP MUST 同时保留 `isError` 供通用客户端判定失败、`content.text` 供不读取结构化结果的客户端显示，并保证三层表达同一错误。对于 `EXECUTION_UNCERTAIN`，Agent MUST NOT 自动重复 program、write 或 control；后续处理 MUST 放弃旧连接状态，并通过无副作用状态重建、安全读回或用户决策收口。HSS start 仅在同一 MCP/Worker 生命周期内使用相同 `capture_key` 和语义等价请求时允许幂等恢复；新生命周期 MUST 使用新 key。

#### Scenario: 写入结果未知
- **WHEN** program、write 或 control 返回 `EXECUTION_UNCERTAIN`
- **THEN** Agent 不重复原副作用调用，先重新建立可信会话并选择安全验证或请求用户决策

#### Scenario: HSS 启动响应丢失
- **WHEN** 当前 MCP/Worker 生命周期内 HSS start 的响应丢失且原请求可完整重建
- **THEN** Agent 仅以相同 `capture_key` 和语义等价参数重试；参数变化或生命周期变化时不复用该 key

#### Scenario: 通用客户端消费工具错误
- **WHEN** 工具返回一个公开领域错误
- **THEN** 响应设置 `isError=true`，`content.text` 提供可显示的错误文本，`structuredContent.error` 提供同一 code/message/retryable/details 的权威对象

### Requirement: CPI-006 HSS 结果保真解释
HSS 指导 MUST 区分采集生命周期和数据完整性，说明 `completed` 仍可能为 `degraded` 或 `unknown`，请求频率不等于实际频率，loss 或 overflow 的 `unknown` 不得解释为零。指导 MUST 将 `changes`、规则 `matches` 和跨时钟 `relations` 分别解释为观测事实、规则匹配和时间关系，不得把它们表述为精确变化时刻或因果结论；`CURSOR_INVALID` 和 `CURSOR_EXPIRED` MUST NOT 触发自动从头查询。包含多轮工具往返或并发矩阵时，指导 MUST 使用至少 60 秒捕获，或使用“预计总往返时间 + 30 秒安全余量”中更适合的固定时长，并在执行冲突矩阵前确认 capture 仍为 `running`。

#### Scenario: 已完成但降级的 capture
- **WHEN** Agent 查询到 lifecycle 为 `completed` 且 integrity 为 `degraded` 或 `unknown`
- **THEN** Agent 保留可解释数据并显式报告质量限制，不宣称采集完整或达到请求采样率

#### Scenario: 时间关系输出
- **WHEN** 查询返回 changes、matches 或 relations
- **THEN** Agent 分别陈述观测区间、规则匹配和时间关系，并且不据此自动生成因果诊断

#### Scenario: 多轮并发矩阵
- **WHEN** Agent 需要在活动 capture 中连续验证多项允许与冲突操作
- **THEN** Agent 选择足以覆盖往返和 30 秒余量且通常不少于 60 秒的固定时长，并在批量操作前确认 capture 为 `running`

### Requirement: CPI-007 服务端 instructions 最小自说明
MCP initialize response MUST 提供不超过 300 个 Unicode 字符的短小、自包含 instructions，只声明固定六工具边界、运行时 Schema 权威、成功结果以 `structuredContent` 为准，以及副作用结果未知时不得盲目重试。instructions MUST NOT 重复完整工具清单参数表、HSS 查询手册或 Skill 的五个工作流。

#### Scenario: 客户端只读取初始化信息
- **WHEN** MCP 客户端完成 initialize 但尚未加载 Skill reference
- **THEN** 客户端仍能识别公共边界、权威结果位置和最关键的副作用安全约束

### Requirement: CPI-008 证据边界和新任务验收
插件文档和验收 MUST 区分 Schema 接受能力与发布证据：当前只声明已验证的 Windows x64、SWD 路径，JTAG 必须标注未完成真机发布验证。发布前 MUST 在重新安装后的新 Codex 任务中覆盖插件发现、Skill 正向与负向触发、固定六工具枚举、至少一个只读代表调用、严格参数拒绝和一个结构化故障恢复场景。大 capture 资源验收 MUST 分别记录服务端规范资源的长度、头和 SHA-256 与 Codex 客户端实际交付；客户端截断 MUST 保持 external-blocked，不得通过删除 `resources/read`、加入本地路径字段或缩减规范资源规避。

#### Scenario: 新任务验收通过
- **WHEN** 插件静态校验、相关 Rust 测试和重新安装后的新任务验收全部通过
- **THEN** 证据记录插件与二进制指纹、Codex 版本、发现结果、调用结果及限制，并仅对实际覆盖的 SWD 范围作出发布声明

#### Scenario: Codex 截断完整 capture 资源
- **WHEN** 服务端文件与独立读取路径证明规范资源完整，但 Codex 交付的 Base64 长度或摘要不完整
- **THEN** 验收将问题标记为客户端 external-blocked，保留完整服务端资源合同且不把本地文件系统路径加入公共响应

### Requirement: CPI-009 固件消费型写入指导
调试 reference MUST 将命令、触发、握手和自清零控制变量识别为可能被固件异步消费的写入。此类写入 MUST 使用 `verify=none`，随后通过业务状态、回显变量或其他安全只读证据验证效果；`VERIFY_FAILED` 只表示最终回读不匹配，MUST NOT 被解释为写入从未发生，也 MUST NOT 触发自动重放副作用。

#### Scenario: 1 ms 任务消费控制字
- **WHEN** Agent 写入一个会被 1 ms 固件任务读取并清零的命令变量
- **THEN** Agent 使用 `verify=none`，随后读取业务结果；即使最终控制字为零也不自动重试原命令
