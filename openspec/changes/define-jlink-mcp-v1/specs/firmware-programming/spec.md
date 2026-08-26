## Purpose

定义 Flash 镜像烧录、擦除和校验的可观察行为、边界及烧录后目标状态，使 Agent 不会把普通内存写入误当成可靠固件编程。

## ADDED Requirements

### Requirement: PRG-001 镜像烧录
`jlink_program.flash` MUST 使用设备 Flash 算法烧录支持的镜像格式，MUST 在执行前完成 ART-001 的 BIN 基地址校验并验证所有目标段位于已知 Flash 边界内，并 MUST 默认执行镜像校验。

#### Scenario: ELF 段超出 Flash
- **WHEN** 镜像包含已知 Flash 边界之外的可加载段
- **THEN** 系统返回 `FLASH_RANGE_INVALID` 且不开始烧录

#### Scenario: 烧录和默认校验成功
- **WHEN** 所有镜像段写入并与目标内容匹配
- **THEN** 系统执行请求的烧录后状态并返回空成功结果

### Requirement: PRG-002 显式烧录后状态
flash 和 erase 请求 MUST 提供 `after: none | reset_halt | reset_run`；系统 MUST NOT 使用隐式默认值。

#### Scenario: 请求缺少 after
- **WHEN** Agent 提交 flash 或 erase 请求但未提供 `after`
- **THEN** Schema 拒绝请求且不访问目标

### Requirement: PRG-003 整片和范围擦除
`jlink_program.erase` MUST 支持整片擦除或同时提供 address 和 length 的范围擦除。范围 MUST 完全位于已知 Flash 内；address 与 length 只提供一个时 MUST 拒绝请求。

#### Scenario: 有效范围擦除
- **WHEN** address 和 length 同时存在且完整落在 Flash 边界内
- **THEN** 系统使用设备算法擦除该范围并执行显式 after 状态

### Requirement: PRG-004 独立镜像校验
`jlink_program.verify` MUST 在完成 ART-001 的 BIN 基地址校验后比较请求镜像与目标内容。完全匹配 MUST 返回空成功结果；不匹配 MUST 返回 `VERIFY_FAILED`，并只返回首个已确认不匹配区域和总不匹配数量。

#### Scenario: 镜像存在多个不匹配区域
- **WHEN** 目标内容与镜像在多个区域不一致
- **THEN** 系统返回首个区域和计数，不把完整目标内容放入 Agent 上下文

### Requirement: PRG-005 不实施授权阻塞
MCP MUST NOT 为烧录或擦除增加确认令牌、权限对话或模型外授权。系统仍 MUST 执行确定性格式、地址、长度、边界和执行结果检查。

#### Scenario: 有效擦除请求
- **WHEN** 请求满足 Schema 和设备边界条件
- **THEN** MCP 直接执行，不要求二次确认令牌

### Requirement: PRG-006 HSS 冲突
活动 HSS 期间 flash、erase 和 verify MUST 返回 `OPERATION_CONFLICT`，不得中断采集或排队到采集完成后隐式执行。

#### Scenario: 采集中请求烧录
- **WHEN** Agent 在 HSS 活动期间调用 flash
- **THEN** 请求立即失败且 HSS 生命周期不受影响
