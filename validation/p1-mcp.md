# P1-2.6 MCP 合同验证证据

## 裁决

`PASS`

主要测试 T-P1-MCP 已覆盖 MCP-001、MCP-002、MCP-003 和 MCP-005。生产入口只枚举六个领域工具及封闭 action；每个工具声明顶层关闭且逐 action 关闭的输入 Schema 和关闭的输出 Schema。成功结果只使用 `structuredContent`，普通副作用成功结果为 `{}`，结构化错误统一经过内部到公共错误映射，原始采集资源固定使用占位 URI 模板和 MIME。

## 执行记录

| 字段 | 记录 |
|---|---|
| 主要测试开始 | `2026-08-26T07:17:46.1733743Z` |
| 主要测试结束 | `2026-08-26T07:17:46.9052247Z` |
| 主要测试命令 | `cargo test -p jlink-mcp --test t_p1_mcp` |
| 主要测试结果 | `PASS`；6/6 通过；退出码 `0` |
| 生产进程 smoke | `2026-08-26T07:18:06.1309449Z` 至 `2026-08-26T07:18:06.8377110Z`；真实 `jlink-mcp.exe` 完成 initialize、tools/list 和断开态 target status；六工具；退出码 `0` |
| 原始输出 | Codex 任务 `codex://threads/01a03bf8-d8c2-7e43-9626-06f420336dc2`；主要测试输出块 `b81bae`；生产进程 smoke 输出块 `6c6482` |
| 源码定位 | 父提交 `1212cb6c293444c05983f7741e897ae6de0b374b` 加本记录所在的 `[P1-2.6][开发]` 原子提交 |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`；`cargo 1.98.0 (797e8a9bc 2026-08-05)`；`x86_64-pc-windows-msvc` |
| 操作系统 | `Microsoft Windows NT 10.0.26200.0` |

## 冻结软件指纹

| 对象 | SHA-256 |
|---|---|
| MCP 合同模块 `mcp.rs` | `62FB723DC7ADCA92FE117821BB8BF33B11F099B9CA58D2B56AE570D83D7FB860` |
| MCP 运行时模块 `runtime.rs` | `623B1CF8E40DA0E73BEF0F9735DC6A57F9D3C3AAD9B7ADC8F23F975C3AABFA89` |
| T-P1-MCP 源码 | `7615698CC5ADE29D0FDBA2A862E6D7A973ABC30A7E2063D5B8D3660F16C2A661` |
| T-P1-MCP 测试二进制 | `2FF03C855303E4F19128683537567B76387872EC9D72955B61009E598FB59ECB` |
| 生产 `jlink-mcp.exe` | `8172AB22D1EE00D8F15A63021D2B7829C6566BF09A7D0FF16579008F81926BC5` |

## 主要结果

- 目录严格等于 `jlink_target`、`jlink_program`、`jlink_inspect`、`jlink_write`、`jlink_control`、`jlink_hss`，没有第七个工具或合同外 action。
- 六个输入/输出 Schema 均通过 JSON Schema 元 Schema 校验，顶层 `additionalProperties: false`；action 分支再次关闭未知字段并验证必要字段、枚举和范围。
- 未声明字段在进入 dispatcher 前以 MCP `-32602` 拒绝；测试确认无设备执行调用。
- `jlink_inspect` 的 variable、memory、register 和 symbols 成功结果分别按 action 校验；合法寄存器字符串可通过，空、混合或跨 action 结果以 `-32603` 拒绝。
- 普通 target status 成功结果只包含连接和目标状态，`content` 为空；没有 `ok`、请求回显、空数组或无意义 `null`。
- `WORKER_UNAVAILABLE` 等内部错误不会泄露到公共合同；该路径稳定映射为 `TARGET_CONNECT_FAILED`。IPC/响应格式故障保留为服务器级错误，不伪装成业务成功。
- overview 占位结果、资源模板和资源读取统一使用 `jlink-mcp://capture/{capture_id}/raw` 与 `application/vnd.jlink-mcp.capture.v1+binary`；实际不可变内容读取仍由任务 5.5 接通。
- 生产 stdio 二进制完成独立进程 smoke；本次不访问 J-Link DLL、探针或目标，不形成硬件能力证据。

## Windows Codex 基线复用

MCP-005 继续复用 `validation/f0-d.md` 的 `PASS_WITH_LIMIT` 客户端能力证据：`OpenAI.Codex 26.818.5229.0`、`codex-cli 0.149.1` 已消费六工具发现、关闭 Schema、`structuredContent`、资源链接和结构化工具错误。2.6 本地主要测试验证生产合同生成相同 MCP 结构；按任务约束不重复 F0-D 客户端实验。真实生产服务器的完整 Windows Codex 端到端验收仍保留给任务 5.6，本批次不得提前宣称发布完成。

## 门禁与失效条件

- `cargo fmt --all -- --check`：PASS。
- `cargo clippy --workspace --all-targets -- -D warnings`：PASS。
- `cargo test --workspace --all-targets`：PASS；现有真机专用测试保持由 T-P1-SES 硬件脚本显式执行，不作为本任务跳过项。
- `scripts/check-dependencies.ps1`：PASS，仍只有四个生产 crate 且依赖方向未改变。
- `openspec validate define-jlink-mcp-v1 --strict`：PASS。

本地证据在 MCP 合同/运行时/测试源码、Rust 工具链、目标 triple、测试或生产二进制变化时失效。Windows Codex 能力证据按 `validation/f0-d.md` 的客户端、协议能力和审批策略指纹管理；生产端到端声明必须等待 5.6。
