# firmware-programming Specification

## Purpose

定义 Flash 镜像烧录、擦除和校验的可观察行为、边界及烧录后目标状态，使 Agent 不会把普通内存写入误当成可靠固件编程。

## Requirements

### Requirement: PRG-001 镜像烧录
`jlink_program.flash` MUST 使用设备 Flash 算法烧录支持的镜像格式，MUST 在执行前完成 ART-001 的 BIN 基地址校验并验证所有目标段位于已知 Flash 边界内，MUST 在首个 `BeginDownload` 前执行一次 `reset_halt` 并确认目标已暂停，并 MUST 默认执行镜像校验。默认校验启用时，系统 MUST 在成功的 `EndDownload` 后再次执行 `reset_halt` 并确认目标已暂停，再读回镜像并应用请求的 `after`。

#### Scenario: ELF 段超出 Flash
- **WHEN** 镜像包含已知 Flash 边界之外的可加载段
- **THEN** 系统返回 `FLASH_RANGE_INVALID` 且不开始烧录

#### Scenario: 烧录和默认校验成功
- **WHEN** 所有镜像段写入并与目标内容匹配
- **THEN** 系统执行请求的烧录后状态并返回空成功结果

#### Scenario: 下载前复位暂停失败
- **WHEN** 输入和 Flash 边界已验证，但下载前 `reset_halt` 无法确认目标已暂停
- **THEN** 系统返回 `TARGET_RECOVERY_FAILED`，且不调用 `BeginDownload`

#### Scenario: 下载后默认校验准备失败
- **WHEN** `EndDownload` 已成功，但默认校验前的 `reset_halt` 无法确认目标已暂停
- **THEN** 系统返回 `TARGET_RECOVERY_FAILED`，记录 Flash 已修改且不执行请求的 `after`

### Requirement: PRG-002 显式烧录后状态
flash 和 erase 请求 MUST 提供 `after: none | reset_halt | reset_run`；系统 MUST NOT 使用隐式默认值。`after` MUST 只控制成功完成 Flash 修改后的状态处理；下载前固定的 `reset_halt` 准备动作 MUST 同样适用于 `after: none`。

#### Scenario: 请求缺少 after
- **WHEN** Agent 提交 flash 或 erase 请求但未提供 `after`
- **THEN** Schema 拒绝请求且不访问目标

#### Scenario: after none 不执行烧录后转换
- **WHEN** flash 或 erase 使用 `after: none` 且 Flash 修改成功
- **THEN** 系统在下载前已执行必要的 `reset_halt`，但成功后不再复位、暂停或恢复运行

#### Scenario: Flash 已修改但后置状态失败
- **WHEN** flash 或 erase 的主操作已成功，但请求的烧录后状态无法完成或确认
- **THEN** 系统立即记录 Flash 已修改、关闭目标并清空可信会话状态，返回不可重放的 `EXECUTION_UNCERTAIN`
- **AND** `details` 包含 `operation`、`phase=post_action`、请求的 `after`、`flash_modified=true` 和原始 `cause_code`

### Requirement: PRG-003 整片和范围擦除
`jlink_program.erase` MUST 支持整片擦除或同时提供 address 和 length 的范围擦除。范围 MUST 完全位于已知 Flash 内；address 与 length 只提供一个时 MUST 拒绝请求。系统 MUST 在首个 `JLINK_EraseChip` 或 `BeginDownload` 前执行一次 `reset_halt` 并确认目标已暂停。

#### Scenario: 有效范围擦除
- **WHEN** address 和 length 同时存在且完整落在 Flash 边界内
- **THEN** 系统使用设备算法擦除该范围并执行显式 after 状态

### Requirement: PRG-004 独立镜像校验
`jlink_program.verify` MUST 在完成 ART-001 的 BIN 基地址校验后比较请求镜像与目标内容。完全匹配 MUST 返回空成功结果；不匹配 MUST 返回 `VERIFY_FAILED`，并只返回首个已确认不匹配区域和总不匹配数量。独立 verify MUST 保持只读，MUST NOT 隐式复位、暂停或恢复目标，也 MUST NOT 增加 `after` 字段。

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
