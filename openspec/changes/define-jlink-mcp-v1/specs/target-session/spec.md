## Purpose

定义一个 MCP 进程与一个活动目标之间的连接、状态、验证和自动恢复合同，使 Agent 在不重复探测的前提下获得可信且可操作的目标状态。

## ADDED Requirements

### Requirement: SES-001 单活动目标
一个 MCP 进程 MUST 最多维护一个活动探针和目标。系统 MAY 枚举多个探针，但连接多个候选时 MUST 通过配置确定唯一探针；切换目标前 MUST 断开当前会话。

#### Scenario: 多个探针且没有唯一选择
- **WHEN** 自动发现得到多个探针且配置没有序列号
- **THEN** 连接返回 `CONFIG_INVALID` 或明确的探针选择诊断，不得任意选择

### Requirement: SES-002 连接与首次验证
`jlink_target.connect` MUST 使用有效配置建立连接，并在当前目标连接会话第一次需要设备能力时验证 DLL 身份、必要导出、探针、目标、接口及后台访问能力。验证成功 MUST 在同一连接会话内复用。

#### Scenario: 同一连接内连续读取
- **WHEN** 首次连接验证已成功且配置及设备身份未变化
- **THEN** 后续普通读取和 HSS 启动不得重复执行完整环境验证

### Requirement: SES-003 halted 与 HardFault 自动恢复
连接或 HSS 启动发现目标 halted 时，系统 MUST 先尝试 resume；resume 失败或目标进入 HardFault 时 MUST 执行 reset 后再次尝试运行。恢复成功后 MUST 保持实际最终状态并通知恢复动作，不得恢复最初 halted 状态。

#### Scenario: resume 后正常运行
- **WHEN** 目标最初 halted 且 resume 成功
- **THEN** 操作继续并返回 `resumed_from_halt` 通知

#### Scenario: reset 后仍不能运行
- **WHEN** resume 和 reset 恢复均失败
- **THEN** 系统返回 `TARGET_RECOVERY_FAILED`，包含已完成步骤和可读取的 PC、IPSR、CFSR、HFSR、DFSR 诊断

### Requirement: SES-004 状态与断开
`jlink_target.status` MUST 返回缓存或已观察的连接状态及目标运行状态。`disconnect` MUST 释放会话和租约；活动 HSS 期间 MUST 拒绝断开。

#### Scenario: HSS 期间读取状态
- **WHEN** Agent 在活动 HSS 期间调用 target status
- **THEN** 系统只返回 Worker 已观察状态，不新增 DLL 调用

#### Scenario: HSS 期间断开
- **WHEN** Agent 在活动 HSS 期间调用 disconnect
- **THEN** 系统返回 `OPERATION_CONFLICT` 且采集继续

### Requirement: SES-005 验证缓存失效
连接丢失、Worker 异常退出、烧录/擦除/其他 Flash 修改，或 DLL、ELF、目标、接口、核心配置变化时，系统 MUST 使相关验证缓存失效。UI 窗口和永久配置 MUST NOT 被视为连接仍有效的证据。

#### Scenario: 烧录后启动符号读取
- **WHEN** 当前连接执行了 Flash 修改
- **THEN** 下一次变量或 HSS 操作重新验证固件与 ELF 身份

### Requirement: SES-006 显式诊断
`jlink_target.validate` MUST 在不产生业务副作用的前提下返回实际完成的 DLL、导出、探针、HSS 和目标检查结果；失败 MUST 给出可操作修正建议。

#### Scenario: Agent 修正配置后复检
- **WHEN** Agent 修改 DLL 路径并调用 validate
- **THEN** 系统重新计算文件身份和能力，不复用修改前的验证结果
