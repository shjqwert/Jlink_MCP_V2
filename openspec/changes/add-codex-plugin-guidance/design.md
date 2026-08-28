## Context

正式 V1 已在 Windows x64/SWD 范围完成实现和发布验证，运行时 `tool_catalog()` 是六工具输入输出语法的权威来源，当前 initialize instructions 只有一句概述。插件需要跨越仓库 marketplace、Codex 插件清单、MCP 启动配置、Skill 文档、Rust 初始化响应和新任务验收，但不得改变现有公共工具合同或把 Agent 解释职责下沉到 MCP。

Codex 官方网页当前未提供足以确定本地插件 MCP 路径展开方式的公开页面；实现阶段应以已安装 Codex CLI、插件校验器和一次最小本地安装探测为准，不在配置中猜测未验证的占位符。

## Goals / Non-Goals

**Goals:**

- 让 repo-local marketplace 安装得到可发现的 MCP 与单一 Skill。
- 以渐进式披露保留状态、安全、HSS 和错误恢复语义，同时避免复制 Schema。
- 使用现有 Rust 合同测试、官方 Skill/插件校验器和真实 Codex 新任务形成可复核证据。
- 保持插件启动配置不依赖开发电脑的绝对仓库路径。

**Non-Goals:**

- 不新增或修改六工具/action、输入输出 Schema、Worker/DLL 所有权或 HSS 查询模型。
- 不实现公开 marketplace、npm 安装器、跨平台运行、JTAG 发布验证、图标/截图、hooks 或通用查询 DSL。
- 不让 Skill 自动批准硬件副作用、自动诊断因果或替代 MCP 的确定性校验。

## Decisions

### 1. 使用一个插件和一个隐式 Skill

插件名和 Skill 名均使用 `jlink-mcp`。六个工具共享同一目标会话和错误模型，拆成多个 Skill 会增加触发竞争与跨 Skill 状态丢失；一个入口按任务路由到五个 reference 能维持单一发现面。

备选方案是按工具或 HSS/调试拆分多个 Skill。该方案会重复共享规则，并让同一用户请求同时触发多个入口，因此不采用。

### 2. 五个 reference 是语义分区，不是参数手册

`SKILL.md` 只保留触发、工具/action 路由、共享不变量和 reference 选择。`target-session.md`、`programming.md`、`debug-access.md`、`hss.md`、`errors.md` 分别承载独有的状态与解释规则；默认读取一个业务 reference，只有失败或未知执行才读取 `errors.md`。

reference 使用决策表和少量跨字段骨架，不复制运行时 JSON Schema。HSS 已在 `hss_tool()` 描述中提供被测试锁定的查询骨架，`hss.md` 只解释游标、生命周期、质量和时间语义。

状态指导采用当前 MCP/Worker 生命周期内的会话快照复用：Agent 从成功调用及其声明的状态转换持续更新可信状态，连续调试时不机械插入 target status；只在新生命周期、未知或失效状态、未知执行、外部变化或返回冲突使快照不再可信时查询。Worker 仍是最终状态校验权威，Skill 不接管运行时安全职责。

### 3. server instructions 单独编译且保持短小

新增 `crates/jlink-mcp/resources/server-instructions.md`，由 `mcp.rs` 通过编译时包含返回。内容只覆盖在 Skill 尚未加载时也必须可见的四项不变量：固定六工具、Schema 权威、`structuredContent` 权威和未知副作用不重试。

备选方案是把完整 Skill 摘要放入 initialize response。它会让每次连接都付出上下文成本，并与工具描述及 Skill 形成第三份合同，因此不采用。

### 4. 插件启动采用经过实测的可移植产品入口

`.mcp.json` 最终只启动正式 `jlink-mcp.exe`，Worker 继续由主程序按同目录规则发现。最小临时插件探针确认 Codex 0.150.1 的 MCP 子进程没有 `PLUGIN_ROOT`，工作目录也是调用任务目录，因此插件入口不能依赖插件相对路径。最终使用 Windows 非交互 PowerShell 从当前用户的 `%LOCALAPPDATA%\Programs\jlink-mcp` 解析入口；安装步骤把两份 release 二进制放入该产品目录，不修改 `PATH`，也不写入开发机仓库绝对路径。

探测只决定路径表达方式，不改变可安装、无绝对路径和主/Worker 同目录的规格。不得回退到 PATH 中来源不明的旧 `jlink` 项目。

### 5. 校验复用现有体系

静态层使用 `quick_validate.py` 和 `validate_plugin.py`；Rust 层扩展 `t_p1_mcp.rs` 验证 initialize instructions 与固定 catalog，不创建 Markdown 解析框架。行为层使用有限的新任务用例覆盖发现、误触发、参数拒绝、只读调用、未知执行/游标/质量解释。

备选方案是建立通用文档示例生成器和独立 eval 框架。首版维护成本高于收益，因此推迟到真实漂移问题出现后再评估。

### 6. repo-local marketplace 只承担开发和验收分发

仓库提供 `.agents/plugins/marketplace.json`。安装脚本先把 marketplace 与插件源复制到被 Git 忽略的 `.local-marketplace`，只在该暂存副本写入 cachebuster，再通过 `codex plugin marketplace add` 注册并安装插件。受控清单保持可复现，修改后的插件仍能通过重新安装进入新任务；公开分发机制留给独立变更。

## Risks / Trade-offs

- [Skill 内容仍可能与运行时语义漂移] → 每条非显然规则映射到当前规格或测试；不复制完整字段表，并让 catalog/schema 继续作为语法权威。
- [插件根路径行为缺少公开网页契约] → 在实现第一项任务中用当前 Codex CLI 和最小插件实测，记录版本与结果；仅采用被实际验证的入口。
- [安装包缺少任一二进制会在运行时失败] → 安装验收同时检查 `jlink-mcp.exe`、`jlink-worker.exe` 和主程序的同目录发现行为。
- [隐式 Skill 误触发或漏触发] → 使用判别性 description，并在新任务分别运行正向、负向和间接请求。
- [硬件调用带来副作用] → 插件识别验收默认使用 `tools/list`、target status/config_get 和故意的严格参数拒绝；硬件纵向调用只复用已有授权与现有发布门禁。
- [Schema 支持被误认为 JTAG 已验证] → Skill 和证据均明确当前发布只覆盖 SWD。

## Migration Plan

1. 生成并静态校验 repo-local marketplace、插件清单、MCP 配置和 Skill。
2. 构建 release 二进制并装配本地插件产品目录，验证主程序能发现同目录 Worker。
3. 扩展 Rust 合同测试并运行受影响测试、Skill/插件校验和 OpenSpec 严格校验。
4. 注册 repo-local marketplace，安装带 cachebuster 的插件，并在新 Codex 任务运行发现与代表性调用验收。
5. Sol Advisor 对完成实现做只读对抗审查；若发现会改变已确认方案的实质问题，先报告并取得用户决策，再修订和重验。
6. 全部证据通过后提交并推送当前非受保护功能分支；安装失败时卸载插件或恢复上一 cachebuster，不改写 Git 历史。

## Open Questions

- 公开分发最终采用 npm、Git marketplace 还是独立安装器；本次 repo-local 验收不会锁定该选择。
