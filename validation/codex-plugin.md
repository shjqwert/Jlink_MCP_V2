# Codex 插件集成证据

## MCP 启动路径探测

| 项目 | 结果 |
|---|---|
| 时间 | 2026-08-28（Asia/Shanghai） |
| Codex | `codex-cli 0.150.1` |
| 隔离插件 | 临时 repo-local marketplace `jlink-mcp-probe-marketplace`，安装后已从 Codex 配置移除 |
| 产品二进制 | 现有 `target/release/jlink-mcp.exe` 与同目录 `jlink-worker.exe` 的临时副本 |
| `PLUGIN_ROOT` | MCP 子进程中为空 |
| MCP 子进程工作目录 | 发起 Codex 任务的项目目录 `D:\Github\jlink-mcp-V2`，不是插件源或缓存目录 |
| `${PLUGIN_ROOT}/bin/jlink-mcp.exe` | 未展开，启动返回 Windows `os error 3` |
| `./bin/jlink-mcp.exe` | 相对当前项目解析，启动返回 Windows `os error 3` |

因此插件 MCP 配置不能依赖 `PLUGIN_ROOT`、插件相对路径或开发机仓库绝对路径。本变更冻结的 Windows 本地入口为 `%LOCALAPPDATA%\Programs\jlink-mcp\jlink-mcp.exe`；安装步骤将 `jlink-mcp.exe` 与 `jlink-worker.exe` 成对装配到该目录，`.mcp.json` 由非交互 PowerShell 通过当前用户的 `LOCALAPPDATA` 解析主程序。该入口不修改 `PATH`，也不会命中既有的其他 J-Link MCP 配置。

## 静态与新任务验收

### 安装快照

| 项目 | 结果 |
|---|---|
| 时间 | 2026-08-28（Asia/Shanghai） |
| Codex | `codex-cli 0.150.1` |
| 插件 | `jlink-mcp@jlink-mcp-v2`，`installed, enabled` |
| 插件版本 | `0.1.0+codex.20260828052838` |
| 产品目录 | `C:\Users\usre\AppData\Local\Programs\jlink-mcp` |
| `jlink-mcp.exe` | 7,514,624 bytes；SHA-256 `655848F6457C21938FE587C3EC69784F95063FCAD94BBBE77E33B2E91F8E1961` |
| `jlink-worker.exe` | 2,062,336 bytes；SHA-256 `D02400C3F07068D2824F336D690957D77414EF543E2BE8E9FD9E91265956895A` |

release 构建使用被 Git 忽略的 `target/plugin-build`，避免覆盖其他活动 Codex 任务正在使用的仓库 `target/release` 二进制。安装器只阻止产品目录中的活动进程，不会终止其他任务的开发构建。

### 校验结果

| 检查 | 结果 |
|---|---|
| Skill `quick_validate.py` | PASS |
| 插件 `validate_plugin.py` | PASS |
| PowerShell 安装器语法解析 | PASS |
| `cargo fmt --all -- --check` | PASS |
| `cargo test -p jlink-mcp` | PASS；全部软件测试通过，1 个明确要求 IAR 目标固件的硬件 fixture 保持 ignored |
| `openspec validate add-codex-plugin-guidance --strict` | PASS |
| `git diff --check` | PASS；仅显示工作区 LF/CRLF 转换提示 |
| Sol Advisor 修订后复审 | `NO_MATERIAL_GAP_FOUND`；未发现工具语义缺失或过度设计 |

### 新 Codex 任务

| 用例 | 任务 | 观察结果 |
|---|---|---|
| 间接正向发现与只读调用 | `01a046d8-41bc-7d40-bdab-2751846a7fec` | 未点名 Skill 仍发现 `jlink-mcp:jlink-mcp` 和 `jlink_mcp`；枚举六工具；`jlink_target.status` 返回 `connection: disconnected`，没有 connect 或硬件副作用 |
| 结构化故障恢复 | 同上 | `config_get` 返回 `structuredContent.error.code: CONFIG_INVALID`、`retryable: false` 和 `target.device is required`；Agent 读取错误指导后停止，没有猜测或修改配置 |
| 严格参数拒绝 | 同上后续轮次 | `jlink_target.status` 携带 `unexpected: true` 被 MCP 参数层以 JSON-RPC `-32602` 拒绝；未进入硬件调用且未修正重试 |
| 负向触发 | `01a046da-6b40-7740-b489-bb26f878d265` | 一般嵌入式 C `volatile` 问题直接回答，任务记录没有工具调用 |
| 显式 Skill 与语义解释 | `01a046da-dafe-7d00-9026-6a8066b76e7b` | `$jlink-mcp` 正确说明同生命周期状态复用、未知副作用停止、HSS failed/aborted 部分数据和游标边界；没有调用 J-Link 工具或修改文件 |

首个验收任务为了只读定位安装后的 reference 使用 Serena，Serena 在插件版本缓存内生成了 `.serena/project.yml`。该副产物不在仓库或 `.local-marketplace`，没有改变 Skill/MCP 内容或硬件状态，也不作为插件验证证据。

### 证据边界

本次验收证明插件在当前 Windows x64 Codex 环境中可发现、可启动、可枚举固定六工具，并能完成不连接硬件的代表性调用、严格参数拒绝和结构化错误处理。由于本机配置缺少 `target.device`，本次没有重复既有的硬件纵向验收；硬件发布证据仍复用正式 V1 的 SWD 门禁。JTAG 只由 Schema 表达，未完成真机发布验证。
