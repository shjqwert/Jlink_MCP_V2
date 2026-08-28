## 1. 插件启动与清单

- [x] 1.1 在隔离临时目录用当前 Codex CLI 探测插件 MCP 的 `PLUGIN_ROOT`/命令路径行为，记录版本、最小配置和结果，并冻结无绝对仓库路径的启动方式
- [x] 1.2 使用 plugin-creator 脚手架创建 `.agents/plugins/marketplace.json` 与 `plugins/jlink-mcp` 的有效清单、`.mcp.json` 和单一 Skill 目录，不添加 hooks、apps 或图形资产
- [x] 1.3 实现最小 Windows 本地装配/安装入口，将 release `jlink-mcp.exe` 与 `jlink-worker.exe` 成对放入插件产品目录、注册 repo-local marketplace、应用 cachebuster 并安装插件

## 2. 渐进式 Skill 内容

- [x] 2.1 编写精简 `SKILL.md`，包含判别性触发描述、六工具/action 路由、运行时 Schema/`structuredContent` 权威规则和五个 reference 的按需加载条件
- [x] 2.2 编写 `target-session.md`、`programming.md` 和 `debug-access.md`，覆盖会话前置条件、编程字段组合、DWARF/内存/寄存器限制及副作用边界，不复制完整 Schema
- [x] 2.3 编写 `hss.md` 与 `errors.md`，覆盖 capture-key 生命周期、游标续页、质量/时间语义、`retryable` 和 `EXECUTION_UNCERTAIN` 的停止与恢复决策

## 3. 服务端 instructions 与合同校验

- [x] 3.1 新增短小 `server-instructions.md` 并由 `mcp.rs` 编译时加载，保持六工具公共 Schema 和 Worker 行为不变
- [x] 3.2 扩展现有 `t_p1_mcp.rs` 合同测试，验证 initialize instructions 的四项不变量和固定工具目录，不建立 Markdown 文本解析框架
- [x] 3.3 运行 Skill quick validation、插件 manifest validation、相关 Rust 测试、`git diff --check` 和 OpenSpec strict validation，并修复所有失败

## 4. 独立对抗审查

- [x] 4.1 使用 Sol Advisor 对完成实现做只读对抗审查，重点检查工具/action 遗漏、状态与副作用安全、HSS 解释、上下文浪费和过度设计
- [x] 4.2 定点核验 Sol 的决定性证据；若存在会改变方案的实质问题，向用户报告并等待决策，只有无实质问题或修订后复审通过才继续安装

## 5. Codex 安装与新任务验收

- [x] 5.1 构建 release 二进制，运行本地安装入口，并确认 `codex plugin list` 显示 `jlink-mcp` 已安装且启用、安装快照包含成对产品二进制
- [x] 5.2 在重新安装后的新 Codex 任务验证 Skill 正向/负向/间接发现、固定六工具枚举、只读代表调用、严格参数拒绝和结构化故障恢复
- [x] 5.3 记录 Codex、插件和二进制指纹、验收结果与 SWD/JTAG 证据边界，并重跑受安装影响的最小回归

## 6. Git 交付

- [x] 6.1 检查完整 diff、未跟踪文件和敏感信息，确认只包含本次变更且所有必需验证仍通过
- [x] 6.2 在当前 `codex/` 功能分支创建原子提交并推送到已配置远端，不改写历史或强制推送

## 7. 全阶段反馈收敛

- [x] 7.1 将 server instructions 收敛到 300 字符内的四项不变量，并补充严格长度/内容回归
- [x] 7.2 精简根 Skill；在 HSS reference 增加 60 秒或往返加 30 秒预算和 running 前置，在 debug reference 增加自清零 `verify=none`、业务验证及不自动重试规则
- [x] 7.3 补充 `isError`、`content.text`、`structuredContent.error` 三层一致性测试，并记录服务端完整资源与 Codex 截断的 FT-017 external-blocked 证据
- [x] 7.4 运行 Skill/plugin 校验、受影响 Rust 测试、workspace 门禁、OpenSpec strict validation 和重新安装后的新任务验收
