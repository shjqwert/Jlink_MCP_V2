# mcp-contract Specification

## Purpose

定义 Agent 可见的 V1 MCP 工具目录、公共输入输出语义、错误结构和客户端兼容边界，使所有领域能力共享一套无歧义且低上下文占用的合同。

## Requirements

### Requirement: MCP-001 固定六工具目录
系统 MUST 只暴露以下封闭工具/action 集合，不得增加未列出的工具或 action：

| 工具 | 允许的 action |
|---|---|
| `jlink_target` | `connect`、`disconnect`、`status`、`validate`、`config_get`、`config_set` |
| `jlink_program` | `flash`、`erase`、`verify` |
| `jlink_inspect` | `variable`、`memory`、`register`、`symbols` |
| `jlink_write` | `variable`、`memory`、`register` |
| `jlink_control` | `halt`、`resume`、`reset`、`step` |
| `jlink_hss` | `start`、`status`、`query` |

#### Scenario: Agent 枚举工具
- **WHEN** MCP 客户端请求工具目录
- **THEN** 系统返回且仅返回表中六个领域工具、封闭 action、输入和输出 Schema

### Requirement: MCP-002 严格结构化合同
每个工具 MUST 声明 `inputSchema` 和 `outputSchema`，输入对象 MUST 拒绝未声明字段；成功的权威结果 MUST 使用 `structuredContent`，完整采集文件 MUST 使用 MCP 资源链接按需暴露。

#### Scenario: 请求包含未知字段
- **WHEN** Agent 提交不属于目标 action Schema 的字段
- **THEN** 系统在执行任何设备操作前拒绝该请求并指出无效字段

#### Scenario: Windows Codex 读取结构化结果
- **WHEN** 经过验证的 Windows Codex 调用成功
- **THEN** 客户端能够从 `structuredContent` 获取权威结果，并在需要时读取返回的资源链接

### Requirement: MCP-003 最小成功结果
普通操作 MUST 只返回 Agent 下一步无法从请求中得知的信息。系统 MUST NOT 重复工具名、action、请求参数、目标信息、`ok: true`、空数组或无意义的 `null`；完整成功且无新增事实的副作用操作 MUST 返回空对象。

#### Scenario: 普通变量读取成功
- **WHEN** Agent 读取一个变量且没有异常或恢复通知
- **THEN** 结果只包含该变量的 `value`

#### Scenario: 副作用操作完整成功
- **WHEN** 写入或控制操作已完整执行且没有需要报告的新事实
- **THEN** 结构化结果为空对象

### Requirement: MCP-004 稳定错误合同
业务校验或设备执行错误 MUST 返回稳定 `code`、可操作 `message` 和 `retryable`；仅当信息能帮助修正请求时才返回 `details`。已经产生副作用但无法确定结果时 MUST 返回 `EXECUTION_UNCERTAIN`，不得伪装为成功。

#### Scenario: 符号名称错误
- **WHEN** Agent 请求的变量路径不存在且存在可信候选
- **THEN** 系统返回 `SYMBOL_NOT_FOUND` 并在 `details` 中给出有限候选

#### Scenario: DLL 调用中断且副作用未知
- **WHEN** Worker 在副作用操作返回确定结果前异常退出
- **THEN** 系统返回 `EXECUTION_UNCERTAIN` 并说明已知的执行边界

### Requirement: MCP-005 Windows Codex 客户端基线
V1 MUST 只声明支持实际验证能够消费 `structuredContent`、资源链接和工具错误的 Windows Codex 版本，MUST NOT 把 ChatGPT Desktop、Claude 或其他客户端作为 V1 验收目标，也 MUST NOT 为未知客户端在文本中复制完整 JSON。

#### Scenario: Windows Codex 不支持资源链接
- **WHEN** F0-D 表明目标 Windows Codex 无法读取资源链接
- **THEN** F0-D 必须判定为 `FAIL` 并阻止生产实现，直到公共合同完成修订和重新确认；系统不得静默复制完整采集内容作为替代
