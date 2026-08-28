# artifact-symbol-model Specification

## Purpose

定义固件镜像、ELF 身份、DWARF 变量路径和复合值的共同模型，使普通变量操作与 HSS 使用完全相同的地址、类型和编码规则。

## Requirements

### Requirement: ART-001 固件与符号输入格式
系统 MUST 接受带 DWARF 的 ELF 作为变量和 HSS 符号来源；扩展名为 `.axf` 或 `.out` 但内容为 ELF 时 MUST 同等处理。烧录输入 MUST 支持 ELF、Intel HEX、Motorola S-record 和 BIN。`jlink_program.flash/verify` 的 `base_address` MUST 是可选十六进制地址；解析后的镜像为 BIN 时，即使 `image` 来自工程默认值，每次请求仍 MUST 显式提供该字段；解析后的镜像为其他格式时 MUST 拒绝该字段。系统 MUST NOT 根据芯片、文件名或相邻文件猜测 BIN 基地址，也 MUST NOT 从 MAP 文件解析变量。

#### Scenario: AXF 实际为 ELF
- **WHEN** 输入文件扩展名为 `.axf` 且魔数和结构为有效 ELF/DWARF
- **THEN** 系统按 ELF/DWARF 解析，不因扩展名拒绝

#### Scenario: BIN 缺少基地址
- **WHEN** Agent 请求烧录 BIN 且未提供 base address
- **THEN** 系统在访问探针前返回 `VALUE_INVALID`

#### Scenario: 自描述镜像携带基地址
- **WHEN** Agent 为 ELF、HEX 或 SREC 请求提供 `base_address`
- **THEN** 系统在访问探针前返回 `VALUE_INVALID`，要求使用镜像自带地址

### Requirement: ART-002 ELF 与目标固件身份
系统 MUST 记录 ELF SHA-256 和可加载 Flash 段指纹，并在同一目标连接会话首次执行变量或 HSS 符号操作前，以只读 Flash 读取验证目标固件与 ELF 匹配；验证成功后 MAY 在相关指纹不变时按连接会话缓存。无法证明匹配时 MUST 返回 `FIRMWARE_IDENTITY_UNKNOWN`，已证明不匹配时 MUST 返回 `FIRMWARE_ELF_MISMATCH`。连接丢失、Worker 退出、Flash 修改或 ELF/目标/接口/DLL 身份变化 MUST 使缓存失效。

#### Scenario: ELF 与目标 Flash 不匹配
- **WHEN** 目标可读取 Flash 段指纹与 ELF 记录不同
- **THEN** 系统拒绝变量和 HSS 操作，不得使用可能过期的地址

### Requirement: ART-003 静态 DWARF 路径
系统 MUST 支持全局或静态可定位变量、结构体成员、嵌套结构体、固定数组和多维数组的确定路径，并 MUST 使用 DWARF 地址、成员偏移和数组维度形成不可变访问计划。

#### Scenario: 选择嵌套数组成员
- **WHEN** Agent 请求有效路径 `controller.channels[3].state`
- **THEN** 系统解析唯一静态地址、大小和类型，普通读取与 HSS 使用相同计划

### Requirement: ART-004 位域、union 与柔性数组
系统 MUST 按 DWARF 位范围解码和编码位域；读取 union MUST 在未指定成员时提供所有可解释成员但不得推断 active member；写 union MUST 指定唯一成员。柔性或动态长度数组 MUST 独立提供 `slice {start,count}`；路径中的 `[i]` 不得替代 `slice`，只选择一个元素时也 MUST 使用 `count:1`。

#### Scenario: 柔性数组没有 slice
- **WHEN** Agent 读取或采集柔性数组且没有提供有效元素范围
- **THEN** 系统返回 `SLICE_REQUIRED`，不推测长度

#### Scenario: 写入 union 多个成员
- **WHEN** Agent 在一次 union 写入中提供多个成员
- **THEN** 系统返回 `VALUE_INVALID` 且不执行部分写入

### Requirement: ART-005 不跟随动态指针
指针 MUST 作为无符号地址值返回；V1 MUST NOT 自动解引用或逐样本跟随指针。需要动态 DWARF location expression 且不能固定为静态地址的变量 MUST 返回 `DYNAMIC_LOCATION_UNSUPPORTED`。

#### Scenario: HSS 请求指针成员内容
- **WHEN** 选择器需要逐样本计算 `ptr->member`
- **THEN** 系统拒绝该选择器，但仍可采集指针本身的地址值

### Requirement: ART-006 无损 TypedValue
系统 MUST 递归编码布尔、安全整数、有限浮点、超出 JSON 安全范围的整数、非有限浮点、指针、结构体、数组、位域和 union，且 MUST 保持原始位宽、符号性、成员名称和数组维度所需的信息。

#### Scenario: 读取最大无符号 64 位整数
- **WHEN** 变量值超出 IEEE-754 安全整数范围
- **THEN** 系统使用十进制字符串并同时返回位宽和符号性，不以 JSON number 丢失精度

#### Scenario: Agent 通过公共 Schema 提交递归值
- **WHEN** Agent 提交数值数组、嵌套结构体数组或 `$int` 标签值
- **THEN** 写入、读取和 HSS Schema 通过同一递归 TypedValue 定义解释该值，并拒绝普通字符串或 `null`

### Requirement: ART-007 精确解析与兼容声明
变量路径 MUST 精确且唯一解析，系统 MUST NOT 静默模糊匹配。支持声明 MUST 依据实际通过的 ELF/DWARF 特性和 fixture；未验证编译器版本只能作为兼容候选。

#### Scenario: 同名符号存在歧义
- **WHEN** 提交路径匹配多个符号且无法唯一确定
- **THEN** 系统返回 `SYMBOL_AMBIGUOUS` 并要求更具体路径
