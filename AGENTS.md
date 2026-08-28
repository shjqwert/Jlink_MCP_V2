<!-- PROJECT_CONTEXT_START -->
# Project Agent Instructions

This managed section is the stable execution map for Codex in this project.
Use current code, configuration, specifications, and observed test output as evidence; never invent missing project facts.

## Project Overview

- jlink-mcp-V2 已实现为 Windows x64 Rust workspace,包含 jlink-domain、jlink-capture、jlink-worker、jlink-mcp 四个 crate,并以 MCP 主进程和独立 Worker 隔离 J-Link DLL 状态。
- V1 面向 SEGGER J-Link、ARM Cortex-M 和 SWD/JTAG,对外合同固定为 jlink_target、jlink_program、jlink_inspect、jlink_write、jlink_control、jlink_hss 六个工具,不兼容旧版且不包含 RTT。
- V1 已通过 PR #2 合入 main;FT-001~FT-016 的全阶段修复已通过 PR #11 合入,当前发布基线为 merge commit fc29dde。
- HSS 支持固定 1~300 秒、最多 10 个顶层 DWARF selector、最高请求 1 kHz、完整持久化值、结构化查询和显式质量证据;新 capture 按工程写入 .jlink-mcp/captures。
- Codex 插件 jlink-mcp 已提供 MCP 服务、渐进披露 Skill 和六工具严格 Schema;旧 MCP 注册已移除,插件重装与新窗口发现已验证。

## Build and Verification

- 使用 scripts/check-workspace.ps1 执行完整 workspace 门禁;日常修改先运行受影响 crate 测试和对应阶段 smoke,涉及硬件时只复用指纹未变化的真机证据。
- Rust 工具链由 rust-toolchain.toml 固定;提交前保持 cargo fmt、cargo clippy 和受影响测试通过,不提交 target 目录中的生成产物。
- 规格变化同步更新 openspec/specs、接口文档、测试与 Skill,并运行严格 OpenSpec 校验;真机、客户端和大型资源结论必须保留可复核证据。

## Code Analysis

- 仓库存在 .codegraph 时,先使用 CodeGraph 定位符号、调用路径和影响范围,再做有边界的源码读取。
- 需要精确符号与引用核查时使用 Serena;当前 Rust 源码和测试目录已经存在,不再按纯规格仓库处理。
- 实现修改保持四 crate 依赖方向和状态所有权:纯规则在 jlink-domain,capture 格式与查询在 jlink-capture,目标与 HSS 状态在 jlink-worker,配置与 MCP 合同在 jlink-mcp。

## Project References

- documentation: `architecture/data-contract/mcp-interface-v1.md` — Agent-facing V1 接口与六工具边界;公共合同判断的架构入口。
- documentation: `architecture/decision-review-q01-q67.md` — Q1~Q67 决策及冲突消解记录;变更产品边界前先核对。
- documentation: `architecture/data-contract/model.c4` — 当前架构模型;修改架构关系时与 changes 和 views 一并校验。
- test: `validation/full-stage-tool-test.md` — FT-001~FT-017、11 阶段真机回归和上下文优化的汇总证据。
- test: `validation/codex-plugin.md` — Codex 插件安装、六工具发现、严格 Schema 和客户端兼容证据。
- manual: `plugins/jlink-mcp/skills/jlink-mcp/SKILL.md` — J-Link MCP Agent 使用路由与跨工具不变量;具体参数按需读取 references。

- 当前代码、测试和 openspec/specs 是实现与合同事实来源;归档 change 与旧讨论只用于追溯,发生冲突时不得覆盖当前主规格和验证结果。
- HSS integrity/loss/overflow 没有 DLL 独立 overflow 或 sequence 证据时保持 unknown,不得由采样间隔正常推导绝对无丢样。
- MCP live Schema 是参数语法权威,structuredContent 是结果语义权威;Skill 负责状态复用、副作用与恢复编排,不机械复制完整 Schema。


## Project Context

- `.agent/context.json`: stable project metadata and context configuration.
- `.agent/planMsg.md`: confirmed project-level plans and key decisions, created only when needed.
- `.agent/handoff/`: cross-task handoff index and records.

## Sol Advisor Integration

- This project inherits global Sol Advisor eligibility, subject to the installed Skill's quality and benefit gates.
- Policy comes from schema-v1 `.agent/authorizations.json`: a missing file or key inherits the global default, `true` allows, and `false` disables implicit delegation.
- Invalid or unreadable policy fails closed to primary-only work; explicit current-user instructions override project defaults.
- Sol Advisor may read this policy but must not modify `AGENTS.md` or any `.agent` context, authorization, plan, or handoff file.
- If Sol Advisor or a required role is unavailable, continue in the primary session without substitution and without blocking ordinary project work.

## Handoff Context

- Create a handoff only when coherent work must continue in another task; skip routine questions and one-off small changes.
- 全阶段修复与插件验收续接时优先读取 W002,并以 main 的 fc29dde、PR #11、Issue #10 和 validation/full-stage-tool-test.md 核对当前状态。
- 进入新的实现目标时从当前 main 创建 codex/ 前缀分支,只携带仍然有效的代码、规格、硬件和客户端指纹,不重做已完成且指纹未变化的昂贵证据。
- If no reliable match exists, continue from the current project without forcing historical context or reading unrelated records.
- Use handoffs only to restore the objective, confirmed progress, verification, remaining work, and risks; current code, configuration, references, and test evidence remain authoritative.
<!-- PROJECT_CONTEXT_END -->
