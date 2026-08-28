# hss-query Specification

## Purpose

定义 Agent 如何以低上下文成本查询 HSS 的状态、变化、完整窗口和事件关系，同时保证原始样本、分页、阈值和时间语义确定且不会被静默压缩。

## Requirements

### Requirement: HSSQ-001 采集状态查询
系统 MUST 支持通过采集 ID 或 `capture_key` 查询生命周期、进度和可用资源；状态和四种视图请求 MUST 且只能提供这两个标识之一。活动或异常终止的采集 MUST 以 `complete_records` 及可用时的半开区间 `from_us/to_us` 明确报告已持久化范围，不得把部分数据表示为完整结果。`completed` 状态 MUST 只返回进入查询所需的 `capture_id`、生命周期、耗时和完整记录数；完整范围与质量证据 MUST 由 `overview` 返回。`failed/aborted` 状态 MUST 继续保留故障、恢复和必要质量证据。

#### Scenario: 查询运行中采集
- **WHEN** Agent 使用有效采集 ID 查询状态
- **THEN** 系统返回当前生命周期、已采集时间或样本进度、已知质量异常和可用的部分范围

#### Scenario: 查询未知采集键
- **WHEN** Agent 使用不存在的 `capture_key` 查询
- **THEN** 系统返回稳定的未找到错误，不创建采集

#### Scenario: 完成态进入概览查询
- **WHEN** Agent 查询已完成采集的状态
- **THEN** 系统返回不含 `quality/from_us/to_us` 的终态摘要，Agent 可使用同一 `capture_id` 请求 `overview` 获取完整质量与范围

### Requirement: HSSQ-002 四种确定性视图
系统 MUST 提供 `overview`、`changes`、`window` 和 `around_event` 四种查询视图。每种视图 MUST 具有独立严格 Schema，并 MUST 从已持久化采集的不可变快照计算结果。

#### Scenario: 请求未知视图
- **WHEN** Agent 请求四种视图之外的查询类型
- **THEN** 系统在读取采集数据前拒绝请求并列出允许视图

### Requirement: HSSQ-003 低冗余概览
`overview` MUST 以半开区间 `from_us/to_us` 返回采集时间范围、事件数量、完整原始资源链接，并为每个顶层变量只返回短 `series` ID 及 `samples` 和 `changes` 导航计数；完整路径只通过首次所需 `dictionary` 登记。覆盖与质量字段只在异常时出现，正常且为空的质量类别 MUST 省略。具体变化路径和值只能通过 `changes` 或 `window` 获取，不得在 overview 增加成员预览。

#### Scenario: 正常采集包含变量变化
- **WHEN** Agent 请求完整性为 `complete` 的采集概览
- **THEN** 系统按顶层变量返回 `samples` 和 `changes`、事件数量及原始资源链接，不返回成员预览或空质量数组

### Requirement: HSSQ-004 变化与阈值规则
`changes` MUST 支持精确变化和固定声明式阈值规则：`abs_delta_gte(value)`、`outside(min,max)`、`equals(value)` 和 `crosses(value,direction)`，其中 `direction` 只能为 `up`、`down` 或 `either`。阈值 MUST 能在采集启动时提交或查询时提交，并对同一数据产生相同结果；规则 MUST 支持精确叶路径和数组通配符 `[*]`，不得支持任意脚本、递归通配符或指针解引用。查询规则 MUST NOT 改写原始采集数据。

#### Scenario: 查询数组成员阈值穿越
- **WHEN** Agent 对 `channels[*].temperature` 请求声明式上穿阈值规则
- **THEN** 系统按稳定路径顺序返回每个匹配元素的阈值事件及时间，不修改原始样本

#### Scenario: 提交任意表达式脚本
- **WHEN** Agent 在阈值规则中提交脚本或未声明运算符
- **THEN** 系统拒绝规则并指出允许的声明式形式

### Requirement: HSSQ-005 完整窗口与显式聚合
`window` MUST 能按时间范围和变量路径分页返回全部原始值。系统 MAY 提供显式的 `min_max`、`first_last` 或 `transitions` 聚合，但 MUST 仅在 Agent 选择该模式时使用；系统不得将原始窗口静默降采样、去重或替换为曲线摘要。

#### Scenario: 请求原始时间窗口
- **WHEN** Agent 对一个变量请求 `raw` 窗口
- **THEN** 系统按确定顺序返回该范围内全部可用样本或分页游标，并保留重复值

#### Scenario: 请求最小最大聚合
- **WHEN** Agent 显式请求 `min_max` 窗口模式
- **THEN** 系统返回明确标记的分桶范围及每桶最小值和最大值，不将结果表示为原始样本

### Requirement: HSSQ-006 事件邻域查询
`around_event` MUST 默认返回目标事件及其附近的变化摘要，并 MAY 通过可选 `series` 只投影指定叶序列。`series` MUST 与 `changes/window` 使用相同的稳定短 ID 或精确叶路径解析；省略时 MUST 保持全序列行为，分页游标 MUST 绑定规范化后的选择。Agent 请求完整原始邻域时 MUST 通过 `window` 的原始模式获取，避免在两个视图中重复定义原始样本协议。

#### Scenario: 查看写入后的状态变化
- **WHEN** Agent 对一个采集中写入事件请求 `around_event`
- **THEN** 系统返回该事件、所选前后时间范围内的变量变化和质量影响，并提供可用于原始窗口查询的边界

#### Scenario: 限定事件邻域序列
- **WHEN** Agent 使用 `series` 请求事件附近的一个叶序列并继续分页
- **THEN** 系统只返回该序列的字典、变化和关系，续页保持同一序列选择

### Requirement: HSSQ-007 时间线与关系语义
系统 MUST 在统一时间线上表达样本变化、写入、自动恢复和质量事件。样本变化 MUST 区分最迟未变化时间 `after_us` 与首次观察时间 `observed_by_us`；持续设备调用 MUST 使用开始和结束时间。跨时钟域关系 MUST 依据已知误差输出 `before`、`after`、`overlaps` 或 `indeterminate`，不得声称因果关系。

#### Scenario: 写入与首次观察变化可确定先后
- **WHEN** 写入结束区间早于变量变化的 `after_us` 且误差区间不重叠
- **THEN** 系统可报告写入事件 `before` 该变化，但不得将其表述为已证明的原因

#### Scenario: 时钟误差导致区间重叠
- **WHEN** 两个事件的映射误差使其先后无法确定
- **THEN** 系统报告 `indeterminate` 并保留原始区间信息

### Requirement: HSSQ-008 一次性变量字典
HSS 结果 MUST 使用稳定短序列 ID 引用变量，并在查询快照首次需要时提供 ID 到完整 DWARF 路径的字典。单位和缩放系统不进入 V1，后续页面 MUST NOT 重复未改变的完整字典。

#### Scenario: 首次分页查询
- **WHEN** Agent 首次请求包含短序列 ID 的查询页
- **THEN** 系统返回所需字典和数据页

#### Scenario: 获取后续页
- **WHEN** Agent 使用同一快照游标获取下一页
- **THEN** 系统返回数据和必要增量，不重复完整未变化字典

### Requirement: HSSQ-009 不可变快照分页
分页游标 MUST 绑定采集 ID、查询参数、排序、Schema 版本和不可变数据快照，并具有明确有效期。每页 MUST 指明是否截断和下一游标；页级质量信息 MUST 只描述该页覆盖范围。游标失效时 MUST 返回稳定错误，不得自动从新快照继续。

#### Scenario: 完成后新增查询不影响旧游标
- **WHEN** Agent 在同一采集上使用已有游标读取下一页
- **THEN** 系统从原不可变快照按原顺序继续，结果不因其他查询而变化

#### Scenario: 游标已经失效
- **WHEN** Agent 提交超过有效期或无法验证的游标
- **THEN** 系统返回游标失效错误并要求显式重新开始查询

### Requirement: HSSQ-010 自描述原始资源
完整原始资源 MUST 使用 `application/vnd.jlink-mcp.capture.v1+binary`，并包含解析所需的格式版本、采集身份、目标与 ELF 身份、变量字典、时间语义、质量事件和数据校验信息。资源链接 MUST 指向不可变内容，读取方无需依赖当前活动会话即可解释数据。

#### Scenario: 会话断开后读取原始资源
- **WHEN** 目标已经断开且 Agent 读取已完成采集的资源链接
- **THEN** 系统仍能提供可校验、自描述且与采集终态一致的完整内容

### Requirement: HSSQ-011 数据而非服务端图像
V1 MUST 返回数值、时间、事件和质量数据，不得由 MCP 服务端生成曲线图片。Agent 或客户端 MAY 基于完整数据自行绘图和解释。

#### Scenario: Agent 需要查看变化曲线
- **WHEN** Agent 请求一个变量在完整采集窗口内的曲线数据
- **THEN** 系统通过 `window` 原始或显式聚合模式返回数值序列，而不是图片
