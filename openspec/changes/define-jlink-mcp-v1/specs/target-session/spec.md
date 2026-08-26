## Purpose

定义一个 MCP 进程与一个活动目标之间的连接、状态、验证和自动恢复合同，使 Agent 在不重复探测的前提下获得可信且可操作的目标状态。

## ADDED Requirements

### Requirement: SES-001 单活动目标
一个 MCP 进程 MUST 最多维护一个活动探针和目标。系统 MAY 枚举多个探针，但连接多个候选时 MUST 通过配置确定唯一探针；切换目标前 MUST 断开当前会话。

#### Scenario: 多个探针且没有唯一选择
- **WHEN** 自动发现得到多个探针且配置没有序列号
- **THEN** 连接返回 `CONFIG_INVALID` 或明确的探针选择诊断，不得任意选择

### Requirement: SES-002 连接与首次验证
`jlink_target.connect` MUST 使用有效配置建立连接，并在当前目标连接会话第一次需要设备能力时验证 DLL 身份、必要导出、探针、目标、接口及后台访问能力。具体器件选择命令 MUST 在同一 DLL 会话内得到确定接受，命令返回码及冻结 API 定义的错误输出都 MUST 被检查；通用 Cortex-M 连接或静态器件数据库查询 MUST NOT 单独证明具体器件已选择。验证成功 MUST 在同一连接会话内复用。

#### Scenario: 具体器件命令被拒绝
- **WHEN** `device = <configured-device>` 返回失败或产生冻结 API 定义的错误输出
- **THEN** 连接返回 `TARGET_CONNECT_FAILED`，不得建立活动会话或继续到任何 Flash 副作用

#### Scenario: 同一连接内连续读取
- **WHEN** 首次连接验证已成功且配置及设备身份未变化
- **THEN** 后续普通读取和 HSS 启动不得重复执行完整环境验证

### Requirement: SES-003 halted 与 HardFault 自动恢复
Worker 建立并持有 J-Link 目标会话后，连接完成检查或 HSS 启动检查发现目标 halted 时，系统 MUST 先尝试 resume；resume 失败或同一活动会话观察到目标进入 HardFault 时 MUST 执行 reset 后再次尝试运行。恢复成功后 MUST 保持实际最终状态并通知恢复动作，不得恢复最初 halted 状态。若器件专用的 J-Link 首次建连初始化已经复位、暂停或以其他方式归一化连接前状态，系统 MUST 只依据建连后的首个可观察状态执行和报告恢复，不得声称观察到已被厂商初始化清除的连接前 HardFault。

#### Scenario: resume 后正常运行
- **WHEN** 目标最初 halted 且 resume 成功
- **THEN** 操作继续并返回 `resumed_from_halt` 通知

#### Scenario: 活动会话观察到 HardFault
- **WHEN** Worker 已持有目标会话并真实观察到目标进入 HardFault，且复位后能够稳定运行
- **THEN** 系统执行 reset 后运行并返回 `reset_after_fault` 通知

#### Scenario: 首次建连归一化连接前状态
- **WHEN** 器件专用 J-Link 建连初始化使连接前 HardFault 不再可观察，建连后的首个可观察状态为 halted
- **THEN** 系统按 halted 路径恢复并只报告实际执行的 `resume`，不得伪造 HardFault 或 reset 通知

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
`jlink_target.validate` MUST 返回实际完成的 DLL、导出、探针、HSS 和目标检查结果；失败 MUST 给出可操作修正建议。活动目标已经连接时，validate MUST 只观察当前会话，MUST NOT 接受 `after` 或改变目标状态。目标未连接且目标级检查需要建立临时会话时，请求 MUST 显式提供 `after: run | halt`；Worker MUST 先复用 SES-003 的唯一恢复流程得到可控运行状态，再收口到请求的最终状态，并返回实际最终状态和全部恢复通知。系统 MUST NOT 根据临时建连后的状态推断建连前状态。除显式目标状态收口外，validate MUST NOT 修改 Flash、RAM、MMIO、业务变量或用户请求的核心寄存器。

#### Scenario: Agent 修正配置后复检
- **WHEN** Agent 修改 DLL 路径并在断开状态调用带 `after: run` 的 validate
- **THEN** 系统重新计算文件身份和能力、不复用修改前的验证结果，并在临时会话结束前确认目标稳定运行

#### Scenario: 断开状态缺少最终状态
- **WHEN** 目标未连接且 Agent 调用未提供 `after` 的 validate
- **THEN** 系统在建立临时目标会话前拒绝请求，不猜测验证后的目标状态

#### Scenario: 断开状态要求最终暂停
- **WHEN** 目标未连接且 Agent 调用带 `after: halt` 的 validate
- **THEN** 系统完成实际检查和必要恢复后显式暂停目标，返回 `target_state: halted` 及实际恢复通知

#### Scenario: 活动连接携带 after
- **WHEN** 目标已经连接且 Agent 调用携带 `after` 的 validate
- **THEN** 系统拒绝请求且不改变当前目标状态；不携带 `after` 的 validate 只观察活动会话
