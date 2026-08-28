# project-configuration Specification

## Purpose

定义工程配置与本机用户配置的持久化、合并、诊断和修改行为，使 Agent 能自动连接正确目标并可靠识别经过验证的 J-Link DLL 基线。

## Requirements

### Requirement: CFG-001 分层配置与确定优先级
系统 MUST 按 `本次请求 > 用户配置 > 工程配置 > 自动发现 > 安全默认值` 合并允许覆盖的配置，并 MUST 能指出每个有效值的来源。工程配置 MUST 保存长期目标基线以及 J-Link DLL 路径、版本和 SHA-256；用户配置 MUST 只保存允许的本机探针选择覆盖，不得覆盖 DLL 身份字段。

#### Scenario: 工程配置提供 DLL 身份
- **WHEN** 工程配置定义 DLL 路径、版本和 SHA-256
- **THEN** 系统使用且共同校验这三个工程配置值，不从用户配置替换 DLL 路径

### Requirement: CFG-002 配置查询
`jlink_target.config_get` MUST 返回当前有效配置和逐字段来源，且 MUST NOT 把这些诊断字段附加到普通连接、读取或写入结果。

#### Scenario: Agent 调查连接失败
- **WHEN** Agent 显式调用 `config_get`
- **THEN** 系统返回有效目标、接口、速度、ELF、DLL 身份和对应来源

### Requirement: CFG-003 原子配置修改
`jlink_target.config_set` MUST 接受 `project` 或 `user` scope 的部分更新，在写入前完整校验，并原子替换目标配置。系统 MUST 只允许工程配置保存目标、接口、速度、ELF/固件、DLL 路径/版本/SHA-256 基线和正整数 `capture.max_bytes`；该字段的默认值 MUST 为 512 MiB，并可由工程配置调低或调高。用户配置 MUST 只允许默认探针序列号等明确声明的本机选择覆盖。`target.device` MUST 使用当前 J-Link 基线可识别的具体器件标识，不得在已有具体器件支持时仅配置通用 Cortex-M 内核名；`target.speed_khz` MUST 作为确定的工程连接基线复用，不得在普通调用中重复探测或静默改写。

#### Scenario: 配置更新有效
- **WHEN** 目标已断开、没有活动 HSS 且 Agent 提交有效的部分更新
- **THEN** 系统原子写入指定 scope 并返回空成功结果

#### Scenario: 活动会话期间修改配置
- **WHEN** 目标已连接或存在活动 HSS 时 Agent 调用 `config_set`
- **THEN** 系统返回 `OPERATION_CONFLICT` 且不修改任何配置文件

#### Scenario: 配置使用通用内核名
- **WHEN** 当前 J-Link 基线能够识别具体目标器件，但工程配置只提供通用 Cortex-M 内核名
- **THEN** 系统在连接前返回可操作的配置诊断，要求选择具体器件标识

#### Scenario: 配置速度无法稳定连接
- **WHEN** 使用工程配置的 `target.speed_khz` 无法稳定连接目标
- **THEN** 系统返回实际失败速度和可操作的降速建议，不得静默采用或持久化另一个速度

### Requirement: CFG-004 DLL 身份基线
工程配置 MUST 记录候选或已验证 `JLink_x64.dll` 的路径、版本和 SHA-256。连接前系统 MUST 验证文件存在、架构正确、版本和哈希匹配，并 MUST 在文件身份变化时使验证缓存失效。

#### Scenario: DLL 文件被升级
- **WHEN** 配置路径保持不变但文件 SHA-256 与工程基线不同
- **THEN** 系统返回 `DLL_HASH_MISMATCH`，不得加载该 DLL 执行目标操作

### Requirement: CFG-005 本机路径不得进入可移植示例
真实工程配置 MAY 包含本机绝对 DLL 路径，但可上传的示例配置 MUST NOT 包含用户机器路径、探针序列号或其他本机身份。

#### Scenario: 生成可共享配置示例
- **WHEN** 项目提供用于版本控制的配置示例
- **THEN** 示例只包含占位符和非敏感基线字段，真实本机配置保持在版本控制之外
