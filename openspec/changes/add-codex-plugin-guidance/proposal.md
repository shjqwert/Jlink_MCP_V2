## Why

当前 MCP 已实现并验证六工具 V1 合同，但新的 Codex 环境只能看到运行时 Schema，缺少对工具选择、跨字段状态约束、HSS 质量语义和不确定执行恢复的渐进式指导。仓库需要提供可安装的 Codex 插件，使新电脑、新项目和新任务能够发现 MCP，并在不复制公共 Schema 的前提下可靠调用它。

## What Changes

- 新增仓库内 Codex 插件和 repo-local marketplace 条目，随插件提供本地 stdio MCP 配置与一个可隐式发现的 `jlink-mcp` Skill。
- 新增一个精简 `SKILL.md` 和五个按需 reference，覆盖目标会话、固件编程、调试访问、HSS 和错误恢复。
- 将 MCP initialize instructions 提取为短小的编译时资源，声明六工具边界、`structuredContent` 权威性和副作用调用不得盲目重试，不复制完整参数 Schema 或 HSS 查询手册。
- 扩展现有合同测试和 Skill/插件校验，验证清单、严格路由、关键安全语义与文档示例不会偏离运行时工具目录。
- 收敛重复指导：server instructions 只保留四项协议不变量且不超过 300 字符，根 Skill 只保留路由和跨工具不变量；HSS 多轮编排、固件消费型写入和错误恢复继续按需放在业务 reference。
- 保留 MCP 错误的 `isError`、`content.text`、`structuredContent.error` 三层兼容输出并增加用途回归；记录 Codex 大资源截断的客户端证据，不改变服务端规范资源或暴露本地路径。
- 增加 repo-local marketplace 安装、Codex 新任务发现和代表性调用验收；公开 marketplace、npm 分发、图标资产、hooks、通用 DSL 和 JTAG 发布声明不在本次范围内。

## Capabilities

### New Capabilities

- `codex-plugin-integration`: 定义仓库内 Codex 插件的发现、渐进式工具指导、服务端 instructions、防错语义、安装和新任务验收要求。

### Modified Capabilities

无。

## Impact

- 新增 `.agents/plugins/marketplace.json`、`plugins/jlink-mcp/` 插件清单、MCP 配置和 Skill 文件。
- 修改 `crates/jlink-mcp/src/mcp.rs` 并新增服务端 instructions 资源；不改变六工具/action、输入输出 Schema 或 Worker/DLL 边界。
- 扩展 `crates/jlink-mcp/tests/t_p1_mcp.rs` 及插件/Skill 静态校验和安装验收证据。
- 更新全阶段和 P4 验证证据，覆盖至少 60 秒的多轮 HSS 编排、自清零变量的 `verify=none` 流程、错误三层兼容与外部资源阻塞。
- 本地安装依赖现有 Windows x64 `jlink-mcp.exe` 与同目录 `jlink-worker.exe`；当前发布能力仍仅声明已验证的 SWD 路径。
