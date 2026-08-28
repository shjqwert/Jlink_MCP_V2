# debug-access Specification

## Purpose

定义普通变量、原始内存、核心寄存器和目标运行控制的 V1 行为边界，使 Agent 能以最小结果安全、确定地完成日常嵌入式调试操作。

## Requirements

### Requirement: DBG-001 变量读写
系统 MUST 通过当前目标绑定的 ELF/DWARF 路径读取和写入静态变量、结构体成员、数组元素、位域及受支持的复合值；写入复合值前 MUST 完整验证路径、类型、形状和值范围，不得产生部分写入。

#### Scenario: 读取单个变量
- **WHEN** Agent 提交一个有效的静态变量路径
- **THEN** 系统返回该路径的无损 `value`，且不重复请求中的路径或目标信息

#### Scenario: 复合值中存在无效成员
- **WHEN** Agent 写入结构体或数组且任一成员名称、索引、类型或数值范围无效
- **THEN** 系统在第一次目标写入前拒绝整个请求并指出无效位置

### Requirement: DBG-002 原始内存访问
系统 MUST 支持按显式地址读取和写入 1–4096 字节的普通内存。读取结果 MUST 返回不带歧义的十六进制 `data`；系统 MUST 检查地址加长度溢出、可访问范围和目标要求的对齐。原始写入 MUST 支持 RAM 与 MMIO，但 MUST 拒绝直接写 Flash 并引导使用烧录能力；写入后 MUST 核对 DLL 报告的实际写入长度，短写不得作为成功返回。

#### Scenario: 读取有效内存范围
- **WHEN** Agent 请求一个有效地址和 1–4096 字节长度
- **THEN** 系统返回精确覆盖该范围的十六进制 `data`

#### Scenario: 原始地址写入 Flash
- **WHEN** Agent 请求对识别为 Flash 的地址范围执行普通内存写入
- **THEN** 系统在写入前拒绝请求并指明应使用 `jlink_program`

#### Scenario: DLL 返回短写
- **WHEN** Agent 请求写入有效 RAM/MMIO 范围但 DLL 报告的实际写入长度小于请求长度
- **THEN** 系统返回确定的短写错误及请求/实际长度，不把操作表示为完整成功

### Requirement: DBG-003 可选读回校验
变量和原始内存写入 MUST 默认不执行读回校验；Agent 显式请求 `readback` 时，系统 MUST 读回并比较最终字节或值，不一致时 MUST 返回最小差异信息。

#### Scenario: 默认写入
- **WHEN** Agent 写入有效变量且未指定校验方式
- **THEN** 系统执行一次写入且不产生额外读回操作

#### Scenario: 读回不一致
- **WHEN** Agent 请求 `readback` 且目标读回值与请求值不一致
- **THEN** 系统返回稳定校验错误及首个可定位差异

### Requirement: DBG-004 核心寄存器访问
系统 MUST 支持读取和写入当前 Cortex-M 核心的受支持寄存器，并 MUST 使用规范寄存器名称报告未知或不可写寄存器。

#### Scenario: 读取核心寄存器
- **WHEN** Agent 请求一个受支持的核心寄存器
- **THEN** 系统返回该寄存器的无损数值

#### Scenario: 运行态寄存器暂不可读
- **WHEN** 规范寄存器已在目标目录中找到，但 DLL 在目标 running 时报告单项读取失败
- **THEN** 系统返回 `TARGET_STATE_INVALID`、`target_state=running` 和显式 halt 建议，不得返回 `REGISTER_NOT_FOUND`

#### Scenario: 暂停态寄存器读取失败
- **WHEN** 规范寄存器已在目标目录中找到，但目标 halted 时 DLL 仍报告单项读取失败
- **THEN** 系统返回 `TARGET_CONNECT_FAILED` 和实际 `target_state`，不得返回 `REGISTER_NOT_FOUND`

#### Scenario: 写入只读寄存器
- **WHEN** Agent 请求写入当前目标声明为只读的核心寄存器
- **THEN** 系统在设备调用前拒绝请求并报告寄存器不可写

### Requirement: DBG-005 目标运行控制
系统 MUST 支持 `halt`、`resume`、`reset` 和单步操作。`reset` MUST 要求显式 `after` 值 `run` 或 `halt`；单步 MUST 要求目标已经 halted，执行一条指令后仍保持 halted，不得隐式暂停运行中的目标。

#### Scenario: 复位后运行
- **WHEN** Agent 请求 `reset` 且 `after` 为 `run`
- **THEN** 系统完成复位并使目标进入运行状态，结果只报告与请求预期不同的事实或恢复通知

#### Scenario: 运行中请求单步
- **WHEN** Agent 在目标运行时请求单步
- **THEN** 系统拒绝请求并指出必须先显式 `halt`，不得产生隐式目标状态变更

### Requirement: DBG-006 不增加写授权策略
系统 MUST NOT 对变量、RAM、MMIO 或核心寄存器写入增加确认、白名单或模型之外的授权阻塞；系统仍 MUST 执行类型、地址、能力和边界校验。

#### Scenario: 有效 MMIO 写入
- **WHEN** Agent 请求一个通过地址和能力校验的 MMIO 写入
- **THEN** 系统直接执行该写入，不要求额外确认

### Requirement: DBG-007 HSS 期间的操作边界
HSS 活动期间，系统 MUST 只接受 `hss-acquisition` HSSA-008 定义的变量写入和 RAM/MMIO 写入；普通读取、寄存器访问、目标控制以及其他设备操作 MUST 返回稳定的采集冲突错误。具体串行调度只由 `jlink-runtime` RUN-002 定义。

#### Scenario: HSS 期间请求变量读取
- **WHEN** 一个目标存在活动 HSS 采集且 Agent 请求普通变量读取
- **THEN** 系统拒绝该操作并指出可使用 HSS 查询获取采集数据

#### Scenario: HSS 期间请求允许的写入
- **WHEN** 一个目标存在活动 HSS 采集且 Agent 请求符合交错规则的变量或 RAM/MMIO 写入
- **THEN** 系统接受该操作并交由 HSSA-008 的写入事件与质量规则处理

### Requirement: DBG-008 符号路径发现
`jlink_inspect.symbols` MUST 接受非空 `query` 和可选 `limit`，`limit` 范围 MUST 为 1–50、默认 20。结果 MUST 按稳定顺序只返回可直接用于变量操作的精确 DWARF 路径，不返回地址、类型、单位或重复请求信息。

#### Scenario: 搜索可用变量路径
- **WHEN** Agent 以 `query` 搜索当前 ELF 中的符号且限制为 10
- **THEN** 系统最多返回 10 个稳定排序且可直接用于变量读写或 HSS 选择的路径
