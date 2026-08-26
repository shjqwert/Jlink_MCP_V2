# J-Link MCP V2：MCP 接口合同 V1 草案

状态：`confirmed`

版本：`0.1`

范围：Windows x64、本机 stdio、SEGGER J-Link、ARM Cortex-M、SWD/JTAG。

本文件只定义 Agent 可见的公共合同，不定义 Rust crate、内部 IPC、存储实现或 J-Link DLL ABI。V1 不兼容旧版接口。

## 1. 设计结论

- 对外固定暴露 6 个工具：`jlink_target`、`jlink_program`、`jlink_inspect`、`jlink_write`、`jlink_control`、`jlink_hss`。
- 每个工具通过少量 `action` 合并同一领域操作，Agent 先按领域选工具，再选动作。
- 一个 MCP 进程只绑定一个工程配置和一个活动目标；请求不重复传入工程、探针、目标或会话字段。
- 所有输入使用 JSON Schema，禁止未声明字段。实现必须同时声明 `inputSchema` 和 `outputSchema`。
- 成功结果使用 `structuredContent`。普通操作不重复请求参数、不返回 `ok: true`、空数组或无意义的 `null`。
- 普通副作用操作完整成功时返回 `{}`。部分执行、执行状态未知或验证失败必须返回错误，不能返回 `{}`。
- HSS 结果可以包含采集状态、时间、质量、分页和资源链接；这些字段不扩散到普通读写接口。
- V1 只支持已验证可消费 `structuredContent` 和 `resource_link` 的现代 MCP 客户端，不为旧客户端复制完整 JSON 文本。

参考依据：

- [OpenAI model guidance](https://developers.openai.com/api/docs/guides/latest-model)：工具面保持精简，工具描述明确输出形状与错误行为，大结果先做确定性收缩。
- [Anthropic tool definition guidance](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools)：相关操作合并为少量工具，只返回 Agent 下一步需要的高信号信息。
- [MCP Tools specification 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)：使用 JSON Schema、`structuredContent`、`resource_link` 和 `isError`。

## 2. 通用合同

### 2.1 请求

- 每个请求必须包含 `action`。
- 地址使用十六进制字符串，例如 `"0x20001000"`，避免 JSON 数值精度和进制歧义。
- 时间输入和公共时间字段使用整数微秒；这只统一表示单位，不承诺底层采样源具有微秒分辨率。持续时间的公开入口使用整数秒。
- ELF/DWARF 变量路径是变量操作的唯一定位依据。直接地址只用于普通内存读写。
- 输入对象统一设置 `additionalProperties: false`。

### 2.2 成功结果

MCP 包装层：

```json
{
  "content": [],
  "structuredContent": {}
}
```

规则：

- `{}` 表示请求已完整、原子地完成；不表示目标固件已经消费写入值。
- 普通变量读取只返回 `{ "value": ... }`。
- 普通地址读取只返回 `{ "data": "..." }`。
- `resource_link` 放在 `content` 中，不在 `structuredContent` 中重复 URI。
- 可选字段仅在有信息时出现；HSS 的 `truncated` 例外，分页结果始终明确返回它。

### 2.3 工具执行错误

参数能通过 MCP 请求结构解析、但业务校验或设备执行失败时，返回：

```json
{
  "isError": true,
  "content": [
    {
      "type": "text",
      "text": "SYMBOL_NOT_FOUND: variable 'motor.sped' was not found"
    }
  ],
  "structuredContent": {
    "error": {
      "code": "SYMBOL_NOT_FOUND",
      "message": "Variable 'motor.sped' was not found",
      "retryable": false,
      "details": {
        "suggestions": ["motor.speed"]
      }
    }
  }
}
```

- `code`、`message`、`retryable` 必须存在。
- `details` 仅在能帮助 Agent 修正请求时出现。
- JSON-RPC/MCP 协议错误只用于未知工具、无法解析的调用结构或服务器级异常。
- 已执行但结果未知时使用专门错误码 `EXECUTION_UNCERTAIN`，并在 `details` 中说明已知副作用边界。

### 2.4 TypedValue

变量值递归映射为 JSON：

| DWARF 类型 | JSON 表示 |
|---|---|
| 布尔、可安全表示的整数、有限浮点数 | JSON `boolean` / `number` |
| 超出 IEEE-754 安全整数范围 | `{ "$int": "18446744073709551615", "bits": 64, "signed": false }` |
| `NaN`、正负无穷 | `{ "$float": "nan" }`、`"inf"`、`"-inf"` |
| 指针 | `{ "$pointer": "0x20001000" }`；V1 不自动解引用 |
| 结构体 | 以成员名为键的 JSON object |
| 固定数组/多维数组 | 保持维度的 JSON array |
| 位域 | 解码后的 `boolean` 或 `number` |
| union | 未指定成员时返回 `{ "$union": { "memberA": ..., "memberB": ... } }` |

写入结构体或数组时必须提供完整选中对象；局部写入通过成员或元素路径完成。写 union 时必须定位具体成员，或在 `$union` 中只提供一个成员。柔性数组必须提供 `slice`。

## 3. 工具总表

| 工具 | actions | 用途 | MCP annotations（保守值） |
|---|---|---|---|
| `jlink_target` | `connect`、`disconnect`、`status`、`validate`、`config_get`、`config_set` | 目标连接、配置和显式诊断 | `readOnly=false`、`destructive=false`、`idempotent=false`、`openWorld=false` |
| `jlink_program` | `flash`、`erase`、`verify` | Flash 编程、擦除和镜像校验 | `readOnly=false`、`destructive=true`、`idempotent=false`、`openWorld=false` |
| `jlink_inspect` | `variable`、`memory`、`register`、`symbols` | 单次只读检查 | `readOnly=true`、`destructive=false`、`idempotent=true`、`openWorld=false` |
| `jlink_write` | `variable`、`memory`、`register` | 类型化或原始写入 | `readOnly=false`、`destructive=true`、`idempotent=false`、`openWorld=false` |
| `jlink_control` | `halt`、`resume`、`reset`、`step` | CPU 运行状态控制 | `readOnly=false`、`destructive=true`、`idempotent=false`、`openWorld=false` |
| `jlink_hss` | `start`、`status`、`query` | 固定时长 HSS 采集和结果查询 | `readOnly=false`、`destructive=true`、`idempotent=true`、`openWorld=false` |

annotations 只是客户端提示，不构成 MCP 写入授权或阻塞逻辑。

## 4. `jlink_target`

### 4.1 `connect`

```json
{ "action": "connect" }
```

首次连接会验证配置中的 DLL 路径、版本、SHA-256、必要导出、探针和目标连接。当前 MCP 进程内验证成功后复用结果，配置或 DLL 文件变化会使缓存失效。

正常成功：

```json
{}
```

若目标最初 halted，MCP 自动 resume；失败或进入 HardFault 时执行 reset+resume。恢复后保持实际运行状态，不恢复最初 halted，并返回通知：

```json
{ "notices": ["resumed_from_halt"] }
```

或：

```json
{ "notices": ["reset_after_resume_failure"] }
```

### 4.2 `disconnect`

```json
{ "action": "disconnect" }
```

成功返回 `{}`。若仍有活动 HSS 采集则返回 `OPERATION_CONFLICT`。

### 4.3 `status`

```json
{ "action": "status" }
```

示例：

```json
{ "connection": "connected", "state": "running" }
```

`connection` 为 `disconnected | connecting | connected | faulted`；连接后 `state` 为 `running | halted | hardfault | unknown`。活动 HSS 期间该动作只读取 Worker 已观察状态，不额外调用 DLL。

### 4.4 `validate`

断开状态必须显式声明验证后的目标状态：

```json
{ "action": "validate", "after": "run" }
```

`after` 只能是 `run | halt`。临时会话先复用唯一恢复流程收口到可控运行态，完成验证后再进入请求状态；接口不推断建连前状态。连接状态下 `validate` 只观察当前会话，必须省略 `after`：

```json
{ "action": "validate" }
```

用于配置修正后的显式复检。它返回确定顺序的 `checks`、实际最终 `target_state`、`target_id`、`validation_runs`，以及本次发生时才出现的 `recovery_notifications`。失败项包含明确修正建议；执行失败通过稳定错误返回。断开状态缺少 `after`，或连接状态携带 `after`，均拒绝执行。

### 4.5 `config_get`

```json
{ "action": "config_get" }
```

返回当前有效配置及每个值的来源。它是 Agent 主动请求的诊断结果，因此允许返回完整配置，但不附加到普通操作结果：

```json
{
  "effective": {
    "target.device": "Z20K146M",
    "target.interface": "swd",
    "target.speed_khz": 4000,
    "symbols.elf": "build/firmware.elf",
    "jlink.dll_path": "C:\\Program Files\\SEGGER\\JLink_V970\\JLink_x64.dll",
    "jlink.dll_version": "9.70",
    "jlink.dll_sha256": "1899a536...a726d29"
  },
  "sources": {
    "target.device": "project",
    "target.interface": "project",
    "target.speed_khz": "project",
    "symbols.elf": "project",
    "jlink.dll_path": "project",
    "jlink.dll_version": "project",
    "jlink.dll_sha256": "project"
  }
}
```

来源为 `request | user | project | discovered | default`。工程配置是长期基线，并共同保存 DLL 路径、版本和 SHA-256；用户配置不允许覆盖 DLL 身份字段。

`target.device` 使用当前 J-Link 基线可识别的具体器件标识；已有具体器件支持时，不接受只描述内核的通用 Cortex-M 名称。`target.speed_khz` 是确定的工程连接基线，同一有效配置下不得在普通调用中重复探测。配置速度连接失败时返回实际失败速度和降速建议，不静默改变或持久化有效速度。

### 4.6 `config_set`

```json
{
  "action": "config_set",
  "scope": "project",
  "values": {
    "jlink.dll_path": "C:\\Program Files\\SEGGER\\JLink_V970\\JLink_x64.dll",
    "jlink.dll_version": "9.70",
    "jlink.dll_sha256": "1899a536...a726d29"
  }
}
```

- `scope` 为 `project | user`；只更新 `values` 中显式提供的字段。
- 工程配置允许目标、接口、速度、ELF/固件路径、DLL 路径、版本、哈希和正整数 `capture.max_bytes`；该单次采集上限默认 512 MiB，并可由工程配置调低或调高。
- 用户配置只允许默认探针序列号等明确声明的本机选择覆盖。
- 配置必须先完整校验，再原子写入；成功返回 `{}`。
- `config_set` 只允许在目标已断开且没有活动 HSS 时执行，否则返回 `OPERATION_CONFLICT`。
- 修改 DLL、目标、接口、ELF 或固件身份字段会使现有验证缓存失效；下次连接重新验证。

## 5. `jlink_program`

### 5.1 `flash`

```json
{
  "action": "flash",
  "image": "build/firmware.elf",
  "verify": true,
  "after": "reset_run"
}
```

- `image` 可省略，省略时使用工程配置中的默认镜像。
- `verify` 默认 `true`。
- `after` 必填：`none | reset_halt | reset_run`。显式字段避免 Agent 误解烧录后的目标状态。
- 已知 Flash 边界之外的镜像段返回 `FLASH_RANGE_INVALID`。
- 完整成功返回 `{}`。

### 5.2 `erase`

整片擦除：

```json
{ "action": "erase", "after": "none" }
```

范围擦除：

```json
{
  "action": "erase",
  "address": "0x08004000",
  "length": 16384,
  "after": "reset_halt"
}
```

`address` 和 `length` 必须同时存在或同时省略。范围必须落在已知 Flash 边界内。完整成功返回 `{}`。

### 5.3 `verify`

```json
{ "action": "verify", "image": "build/firmware.elf" }
```

`image` 可省略。匹配返回 `{}`；不匹配返回 `VERIFY_FAILED`，`details` 只给出首个已确认不匹配区域和总不匹配计数。

## 6. `jlink_inspect`

### 6.1 `variable`

```json
{ "action": "variable", "path": "motor.state" }
```

柔性数组或显式子区间：

```json
{
  "action": "variable",
  "path": "rx_buffer",
  "slice": { "start": 0, "count": 64 }
}
```

结果严格为：

```json
{ "value": 3 }
```

结构体和数组直接放在 `value` 中，不附带类型、地址、单位或请求回显。

### 6.2 `memory`

```json
{
  "action": "memory",
  "address": "0x20001000",
  "length": 16
}
```

V1 单次长度范围为 `1..4096` 字节。结果仅包含按内存地址顺序排列的小写十六进制字节，不带 `0x` 或分隔符：

```json
{ "data": "7856341200000000aabbccddeeff0011" }
```

### 6.3 `register`

```json
{ "action": "register", "name": "PC" }
```

结果：

```json
{ "value": "0x08001234" }
```

支持 Cortex-M 通用寄存器和已确认存在的特殊寄存器；名称不准确时返回 `REGISTER_NOT_FOUND`。

### 6.4 `symbols`

```json
{ "action": "symbols", "query": "motor", "limit": 10 }
```

`limit` 范围 `1..50`，默认 `20`。结果只返回可供后续变量操作使用的路径：

```json
{ "symbols": ["motor", "motor.state", "motor.speed"] }
```

## 7. `jlink_write`

### 7.1 `variable`

```json
{
  "action": "variable",
  "path": "motor.command",
  "value": 2,
  "verify": "none"
}
```

- `verify` 为 `none | readback`，默认 `none`。默认不回读，避免固件立即消费控制变量时产生假失败。
- `readback` 只证明同一 J-Link 连接观察到相同值，不证明固件行为。
- 完整结构/数组值先全部编码和校验，再提交一次写入；任何成员无效时不执行写入。

### 7.2 `memory`

```json
{
  "action": "memory",
  "address": "0x40001000",
  "data": "01000000",
  "verify": "none"
}
```

- `data` 是按地址顺序排列的偶数长度十六进制字符串，最多 4096 字节。
- 可写 RAM 或 MMIO。已知 Flash 地址必须改用 `jlink_program`。
- `verify` 为 `none | readback`，默认 `none`。

### 7.3 `register`

```json
{
  "action": "register",
  "name": "R0",
  "value": "0x00000001"
}
```

完整成功均返回 `{}`。不设置授权字段，不弹出确认；只做类型、范围、连接状态和后端能力判断。

### 7.4 HSS 期间

- `variable` 和 `memory` 被 Worker 排队，在两次 HSS 缓冲排空之间串行执行。
- 工具调用只有在实际写入完成后才返回 `{}`。
- 写入的开始、结束和失败事件写入 HSS 时间线。
- `register` 在 HSS 期间返回 `OPERATION_CONFLICT`，因为 V1 不保证运行中核心寄存器写入语义。

## 8. `jlink_control`

```json
{ "action": "halt" }
```

```json
{ "action": "resume" }
```

```json
{ "action": "reset", "after": "run" }
```

```json
{ "action": "step" }
```

- `reset.after` 必填：`run | halt`。
- `step` 要求目标已 halted，执行一条指令后仍保持 halted。
- 完整成功返回 `{}`。
- 活动 HSS 期间 Agent 发起的 `halt`、`resume`、`reset`、`step` 均返回 `OPERATION_CONFLICT`。HSS 内部自动恢复不受此限制，并必须记录事件和通知。

## 9. `jlink_hss`

### 9.1 VariableSelector

```json
{ "path": "motor" }
```

```json
{ "path": "samples", "slice": { "start": 0, "count": 128 } }
```

- 每次采集最多 10 个顶层 selector。
- `path` 可以选择标量、结构体成员、完整结构体、固定数组或多维数组。
- 固定数组维度来自 DWARF；柔性数组必须提供 `slice`。
- 不自动跟随指针。

### 9.2 ThresholdRule

`start` 和采集后的 `query` 使用同一规则结构。规则只标记显著事件，不影响原始采集，也不替代精确变化事实。

```json
{ "id": "r0", "path": "motor.speed", "kind": "abs_delta_gte", "value": 100 }
```

```json
{ "id": "r1", "path": "cells[*].voltage", "kind": "outside", "min": 3.0, "max": 4.25 }
```

```json
{ "id": "r2", "path": "motor.state", "kind": "equals", "value": 4 }
```

```json
{ "id": "r3", "path": "temperature", "kind": "crosses", "value": 80, "direction": "up" }
```

固定规则集：

| `kind` | 必要字段 | 含义 |
|---|---|---|
| `abs_delta_gte` | `value` | 相邻观测值绝对差不小于阈值 |
| `outside` | `min`、`max` | 值进入或处于闭区间外 |
| `equals` | `value` | 值变为指定值 |
| `crosses` | `value`、`direction` | 穿越阈值；方向为 `up | down | either` |

路径只允许精确叶路径或数组 `[*]`，不支持 `**`、脚本或任意表达式。

### 9.3 `start`

```json
{
  "action": "start",
  "capture_key": "boot-check-001",
  "duration_s": 30,
  "rate_hz": 1000,
  "variables": [
    { "path": "motor" },
    { "path": "temperature" }
  ],
  "return_when": "started",
  "rules": [
    { "id": "r0", "path": "motor.speed", "kind": "abs_delta_gte", "value": 100 }
  ]
}
```

- `capture_key` 必填，由 Agent 提供；同一规范化请求和 key 幂等映射到同一 capture。
- 相同 key 配置不同请求返回 `CAPTURE_KEY_CONFLICT`。
- `duration_s` 为 `1..300`；`rate_hz` 为 `1..1000`，属于请求值而不是无损保证。
- `return_when` 必填：`started | completed`。
- MCP 内部按固定时长自动停止、尾部排空和完成校验，不提供 Agent `stop` action。

`return_when = started`：

```json
{ "capture_id": "cap_01J...", "state": "running" }
```

`return_when = completed`：返回 `capture_id`、`state` 和最小 overview：

```json
{
  "capture_id": "cap_01J...",
  "state": "completed",
  "elapsed_us": 30001234,
  "variables": [
    {
      "path": "motor",
      "samples": 30000,
      "changes": 83
    },
    {
      "path": "temperature",
      "samples": 30000,
      "changes": 19
    }
  ],
  "events": 2
}
```

正常时不返回 `quality`；出现降频、间隙、溢出、短包、格式异常、时钟不确定或可识别丢样时才返回。

### 9.4 `status`

通过 ID：

```json
{ "action": "status", "capture_id": "cap_01J..." }
```

通过恢复 key：

```json
{ "action": "status", "capture_key": "boot-check-001" }
```

两者必须且只能提供一个。运行中示例：

```json
{ "state": "running", "elapsed_us": 12400000 }
```

`state` 为 `starting | running | stopping | completed | failed | aborted`。

- `failed`：采集流程能够完成故障收口，但没有形成正常完成的 capture；返回 `failure_code` 和 `partial_available`。
- `aborted`：进程被强制终止、存储中断或遗留 `.partial` 被启动扫描发现；返回 `reason`、`recoverable` 和 `partial_available`。
- `aborted` 只描述事实，不恢复公共 `stop/cancel` action。

状态查询本身仍是成功调用。

### 9.5 `query`

公共字段：

| 字段 | 规则 |
|---|---|
| `capture_id` / `capture_key` | 必须且只能提供一个 |
| `view` | `overview | changes | window | around_event` |
| `cursor` | 不透明游标；提供后不得再改变其他查询字段 |
| `limit` | `1..1000`，默认 `200` |

时间窗口统一为半开区间 `[from_us, to_us)`。

#### `overview`

```json
{
  "action": "query",
  "capture_id": "cap_01J...",
  "view": "overview"
}
```

返回采集边界、每个顶层变量的 `samples`/`changes`、事件数量和异常质量，不返回成员预览。`content` 同时附带：

```json
{
  "type": "resource_link",
  "uri": "jlink-mcp://capture/cap_01J.../raw",
  "name": "cap_01J...-raw",
  "description": "Complete self-describing HSS capture",
  "mimeType": "application/vnd.jlink-mcp.capture.v1+binary"
}
```

原始资源包含完整 path→`series_id` 字典、DWARF 路径规则、数组维度、位域范围、union 歧义、编码、格式版本、采集边界、完整性信息和校验和。

#### `changes`

```json
{
  "action": "query",
  "capture_id": "cap_01J...",
  "view": "changes",
  "series": ["motor.state", "motor.speed"],
  "from_us": 0,
  "to_us": 30000000,
  "rules": [
    { "id": "r0", "path": "motor.speed", "kind": "abs_delta_gte", "value": 100 }
  ],
  "limit": 200
}
```

示例结果：

```json
{
  "dictionary": {
    "s0": "motor.state",
    "s1": "motor.speed"
  },
  "changes": [
    {
      "series": "s0",
      "after_us": 100000,
      "observed_by_us": 101000,
      "from": 1,
      "to": 2
    }
  ],
  "matches": [
    {
      "rule": "r0",
      "series": "s1",
      "after_us": 150000,
      "observed_by_us": 151000
    }
  ],
  "events": [],
  "truncated": false
}
```

`changes` 是精确观测事实；`matches` 是阈值规则匹配，二者不得混为一个含义。采样变化只声明发生在 `after_us` 与 `observed_by_us` 之间，不伪造精确变化时刻。

#### `window`

完整原始样本：

```json
{
  "action": "query",
  "capture_id": "cap_01J...",
  "view": "window",
  "series": ["motor.speed", "temperature"],
  "from_us": 1000000,
  "to_us": 2000000,
  "mode": "raw",
  "limit": 1000
}
```

```json
{
  "clock": "sample",
  "dictionary": {
    "s0": "motor.speed",
    "s1": "temperature"
  },
  "time_us": [1000000, 1001000, 1002000],
  "values": {
    "s0": [1200, 1210, 1210],
    "s1": [25.1, 25.1, 25.2]
  },
  "truncated": false
}
```

`mode`：

- `raw`：所有原始样本，绝不静默降采样。
- `min_max`：每个桶返回 `[min,max]`，必须提供 `points`。
- `first_last`：每个桶返回 `[first,last]`，必须提供 `points`。
- `transitions`：只返回发生值变化的观测行。

非 `raw` 模式必须由 Agent 显式选择。MCP 不自动替 Agent 决定曲线简化方式。

#### `around_event`

```json
{
  "action": "query",
  "capture_id": "cap_01J...",
  "view": "around_event",
  "event_id": "e17",
  "before_us": 100000,
  "after_us": 200000,
  "limit": 200
}
```

默认返回事件和附近变化，不返回原始波形；原始波形通过 `window` 获取。

事件时间使用显式时钟域：

```json
{
  "id": "e17",
  "kind": "memory_write",
  "start": { "clock": "host", "us": 5012300 },
  "end": { "clock": "host", "us": 5012480 },
  "sample_relation": "overlaps",
  "mapping_uncertainty_us": 800
}
```

跨时钟关系只允许 `before | after | overlaps | indeterminate`，表示时间关系而非因果关系。

### 9.6 分页与质量

- 游标绑定固定 capture 快照、查询字段、排序、schema 版本和页位置。
- 同一游标序列不受后续追加数据影响；已完成 capture 的游标在资源存在期间有效。
- `truncated: true` 时必须返回 `next_cursor`；否则省略 `next_cursor`。
- 每页只报告落在该页范围内的 gap、overflow、rate、frame 和 clock 问题。
- capture 失败后，只要有已校验部分数据，`query` 仍允许读取，并明确返回异常质量。

## 10. HSS 并发边界

HSS 活动期间：

| 操作 | V1 行为 |
|---|---|
| `jlink_hss status/query` | 允许；读取 Worker 状态或已持久化快照 |
| `jlink_target status` | 允许；只读缓存状态，不新增 DLL 调用 |
| `jlink_write variable/memory` | 允许；在 HSS 排空间隙串行执行并记录事件 |
| `jlink_inspect` | 拒绝，返回 `OPERATION_CONFLICT`；采样数据通过 HSS query 获取 |
| `jlink_program` | 拒绝，返回 `OPERATION_CONFLICT` |
| `jlink_control` | 拒绝，返回 `OPERATION_CONFLICT` |
| `jlink_target disconnect/validate` | 拒绝，返回 `OPERATION_CONFLICT` |

V1 不依赖 `JLink_x64.dll` 对同一连接的并发安全。所有 DLL 调用由 Worker 内唯一 gateway 串行执行；这里允许的是操作交错，不是 DLL 并发调用。

## 11. 稳定错误码 V1

| 错误码 | 含义 | 默认可重试 |
|---|---|---|
| `CONFIG_INVALID` | 工程配置缺失或字段无效 | false |
| `DLL_NOT_FOUND` | 配置的 DLL 不存在 | false |
| `DLL_VERSION_MISMATCH` | DLL 版本不符合支持基线 | false |
| `DLL_HASH_MISMATCH` | DLL 文件哈希变化 | false |
| `DLL_EXPORT_MISSING` | 必要导出不存在 | false |
| `PROBE_NOT_FOUND` | 未发现指定探针 | true |
| `TARGET_CONNECT_FAILED` | 目标连接失败 | true |
| `TARGET_RECOVERY_FAILED` | resume/reset 后仍不能正常运行 | true |
| `TARGET_STATE_INVALID` | 当前状态不能执行该动作 | true |
| `SYMBOL_NOT_FOUND` | DWARF 路径不存在 | false |
| `SYMBOL_AMBIGUOUS` | 路径不能唯一解析 | false |
| `TYPE_UNSUPPORTED` | 类型无法按 V1 规则编码 | false |
| `SLICE_REQUIRED` | 柔性数组缺少有效 slice | false |
| `VALUE_INVALID` | 写入值不符合类型或范围 | false |
| `ADDRESS_OUT_OF_RANGE` | 地址或区间不属于允许的目标地址空间 | false |
| `FLASH_RANGE_INVALID` | Flash 操作超出已知 Flash 边界 | false |
| `REGISTER_NOT_FOUND` | 核心寄存器名称无效或目标不支持 | false |
| `VERIFY_FAILED` | Flash 或显式 readback 校验失败 | false |
| `HSS_UNSUPPORTED` | DLL、探针或目标能力不满足 HSS | false |
| `HSS_START_FAILED` | HSS 启动失败 | true |
| `OPERATION_CONFLICT` | 当前活动操作与请求不能交错 | true |
| `CAPTURE_NOT_FOUND` | capture ID/key 不存在 | false |
| `CAPTURE_KEY_CONFLICT` | 同一 key 对应不同采集请求 | false |
| `CURSOR_INVALID` | 游标损坏或与查询不匹配 | false |
| `CURSOR_EXPIRED` | capture 资源已经不存在 | false |
| `FRAME_INVALID` | HSS 数据帧无法可靠解析 | false |
| `EXECUTION_UNCERTAIN` | 设备调用中断，无法证明是否已产生副作用 | false |

`RATE_DEGRADED`、`BUFFER_OVERFLOW`、`SAMPLE_GAP`、`SHORT_FRAME` 和 `CLOCK_UNCERTAIN` 首先是 capture `quality` 事实，不自动把可读取的 capture 变成工具错误。

## 12. 已确认的接口边界（Q60–Q67）

1. `jlink_program.flash/erase` 的 `after` 保持必填，不设置隐式默认状态。
2. 普通变量和原始内存写入的 `verify` 默认值为 `none`；只有请求显式指定时才回读校验。
3. 普通原始内存单次读取和写入上限均为 4096 字节；超限明确拒绝，不静默拆分或截断。
4. HSS 不提供手动 `stop/cancel`；Worker 严格按照请求的 1–300 秒固定时长自动结束、停止并执行尾部排空。
5. HSS 活动期间只开放变量和原始内存写入；其他需要调用 DLL 的操作按第 10 节拒绝。允许的是 Worker 串行交错执行，不承诺同一 J-Link 会话的 DLL 并发调用安全。
6. 所有 HSS `start` 都要求 Agent 提供 `capture_key`，不只限于 `return_when=completed`；相同规范化请求与 key 幂等映射到同一 capture。
7. `jlink_target` 提供 `config_get/config_set`，允许 Agent 显式查询配置来源，并在断开状态下原子修正工程或用户配置。
8. HSS 保留 `aborted` 终态来表示异常中止和可恢复部分采集，但不提供公共 `cancelled/stop` 操作。
