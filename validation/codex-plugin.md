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

## 全阶段反馈收敛

- initialize instructions 资源正文从 452 缩至 293 个 Unicode 字符（-35.2%），线上值含结尾换行共 294 字符；仅保留六工具边界、运行时 Schema 权威、`structuredContent` 权威和不重放不确定副作用。
- 根 Skill 从 3,711 缩至 2,994 个 Unicode 字符（-19.3%），只保留 action 路由和跨工具不变量；目标会话、编程、调试、HSS 与错误恢复细节按需加载。
- HSS reference 对多轮/并发矩阵规定至少 60 秒，或“预计总往返时间 + 30 秒安全余量”，并要求执行矩阵前确认 capture 仍为 `running`。
- debug/error reference 对命令、触发、握手和自清零变量规定 `verify=none`、业务状态验证及不得因 `VERIFY_FAILED` 自动重放。
- `isError`、`content.text`、`structuredContent.error` 分别承担通用失败标志、可显示文本和机器可读权威错误；三层一致性由 MCP 回归锁定，不作为可删除重复。
- FT-017 保持 external-blocked：服务端资源完整，Codex 客户端交付发生静默截断；没有删除 `resources/read`、加入本地路径字段或缩减规范资源。

### 全阶段修订后重装验收

| 项目 | 结果 |
|---|---|
| 插件版本 | `0.1.0+codex.20260828102524`；`jlink-mcp@jlink-mcp-v2` 为 `installed, enabled` |
| `jlink-mcp.exe` | 7,559,680 bytes；SHA-256 `B2ED72DED15C3638110F532E628A0F67961C51A41AEA8777E7492EB138F59DCD` |
| `jlink-worker.exe` | 2,081,280 bytes；SHA-256 `F2903443929559F5DB10FAF72B6D2DD1F7A4D5BCEB6F09C86CD958694407C302` |
| 原始目录合同 | initialize instructions 294 字符；`tools/list` 32,555 bytes；`jlink_hss` 18,875 bytes；固定六工具 |
| 新任务 | 临时任务 `01a047e7-9292-7192-a37f-c9b00055f0f3`；加载 `jlink-mcp:jlink-mcp`，只发现插件 `jlink_mcp` 的六工具 |
| 代表调用 | `jlink_target.status` 返回 `connection=disconnected`；未 connect、未执行硬件副作用 |
| 严格拒绝 | `status` 增加 `unexpected=true` 返回 JSON-RPC `-32602`，请求未进入工具实现 |

重装前精确确认产品目录只有空闲 MCP 子进程，没有 `jlink-worker` 或 `JLink`；按重启验收边界停止这些子进程后安装。隔离 release 与产品目录二进制哈希一致。新任务总 token 计数为 26,321，其中还包含项目上下文、交接检索和 reference；它不是六工具目录的独立 token 计量，因此上下文压缩结论以可重复的 raw 字节数和 Skill 字符数为准。
