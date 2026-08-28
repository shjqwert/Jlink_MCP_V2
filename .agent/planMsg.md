# Project Plans

> Managed project-level plans only. Routine bugs, implementation tasks, and development journals do not belong here.

<!-- PROJECT_PLAN_DATA_START -->
```json
{
  "schemaVersion": 1,
  "plans": [
    {
      "id": "P001",
      "title": "交付 J-Link MCP V2 首版",
      "summary": "按照已确认的 Q1-Q67 决策和架构边界，完成 Windows x64 本机 stdio 的 Rust J-Link MCP V2，实现烧录、读写、目标控制和固定时长 HSS 采集，并以真机与现代 MCP 客户端证据完成验证。",
      "status": "completed",
      "successCriteria": [
        "公共 MCP 接口与已确认的 Q1-Q67 决策一致，并具有可验证的输入、输出和错误 Schema。",
        "Rust 实现保持主进程、独立 J-Link Worker 和 Capture Store 的单一状态所有权及明确依赖方向。",
        "首版在 Windows x64、SEGGER J-Link、ARM Cortex-M、SWD/JTAG 范围内实现烧录、校验、变量与内存读写、核心寄存器操作、目标控制和 HSS。",
        "HSS 支持结构体、数组、固定 1-300 秒采集、完整原始数据、确定性查询、质量证据和写入事件交错。",
        "冻结一组经过实机验证的 DLL 版本与哈希基线，并验证 HSS ABI、帧格式、时间戳、尾排空、丢样和溢出行为。",
        "使用目标现代 MCP 客户端验证 structuredContent、资源链接、分页游标、当前 MCP 所有的 Worker 正常关闭，以及意外中断 capture 的非完成恢复。"
      ],
      "specRefs": [
        "architecture/data-contract/changes/hss-agent-data-contract.c4",
        "openspec/changes/define-jlink-mcp-v1"
      ],
      "decisions": [
        "V2 不兼容旧版，使用 Rust，不包含 RTT。",
        "对外保持 6 个领域工具，通过少量 action 表达操作。",
        "所有 J-Link DLL 调用由独立 Worker 单一拥有并串行执行；HSS 期间只交错变量和 RAM/MMIO 写入。",
        "普通结果最小化；HSS 负责完整保存和确定性处理，Agent 负责解释与诊断。",
        "architecture/decision-review-q01-q67.md 是 Q1-Q67 冲突消解与最终产品决策记录。",
        "在 codex/v1-implementation 分支持续完成 define-jlink-mcp-v1 的 2.1-5.5；5.6、5.7、OpenSpec 归档、发布 PR 和 JTAG 真机验证保持待办。",
        "任务开发只运行主要测试和直接回归；原子提交执行受影响范围门禁，仅在 4.8 和 5.5 各执行一次完整 workspace 门禁与阶段纵向 smoke。",
        "SVN 检查采用事件触发并复用有效指纹；减少状态扫描不得减少 HSS 停止、尾排空、数据恢复、CPU 安全运行和硬件身份验证。",
        "复用未变化的代码、规格和证据上下文；优先查询具体符号与差异，仅在文件变化、任务切换、证据冲突或上下文失效时重新读取。",
        "普通机械问题在既有合同内自主修复；严重设计问题、合同或验证冲突及硬件安全异常立即停止并请求确认。",
        "P2 3.7、P3 4.8 与 P4 5.1-5.5 已完成；检查点 f124d2d 已推送，4.8 完成 capture 在输入指纹有效时继续复用；5.6 已按当前分支隔离 MCP 路线启动，5.7、归档和发布仍待办。",
        "Worker 由当前 MCP 创建并管理，生命周期绑定当前 Codex/MCP；正常关闭执行 HSS Stop、尾排空、非完成保存和目标断开，意外退出不续行，新 MCP 不接管旧 Worker、旧 HSS 或旧 capture_key。"
      ],
      "createdAt": "2026-08-25T03:13:38.576Z",
      "updatedAt": "2026-08-28T02:46:54.371Z",
      "transitions": [
        {
          "from": null,
          "to": "proposed",
          "reason": "Plan recorded.",
          "at": "2026-08-25T03:13:38.576Z"
        },
        {
          "from": "proposed",
          "to": "accepted",
          "reason": "用户已逐项确认 Q1-Q67，并确认最后三项接口偏差；产品范围、架构方向和公共接口边界已具备进入规格拆分的条件。",
          "at": "2026-08-25T03:13:43.366Z"
        },
        {
          "from": "accepted",
          "to": "in-progress",
          "reason": "用户已开始 OpenSpec 规格拆分讨论；P001 从已接受方向进入规格化执行阶段。",
          "at": "2026-08-25T03:17:29.726Z"
        },
        {
          "from": "in-progress",
          "to": "completed",
          "reason": "V1 已完成 5.7 SWD 发布门禁并通过 PR #2 合入 main；9 份主规格共 65 条需求严格校验通过，define-jlink-mcp-v1 已归档。JTAG 真机验证按用户决定不属于本次发布验收，保留为后续独立工作。",
          "at": "2026-08-28T02:46:54.371Z"
        }
      ],
      "dedupeKey": "sha256:7f7a4669f75c146ffe18b7abc4a3fa7bf857bbeca92d4d925ece334d6a0e2fb8"
    },
    {
      "id": "P002",
      "title": "全阶段测试后优化 J-Link MCP 上下文",
      "summary": "保持当前六工具实现和安装状态完成全阶段真机测试并保留证据；测试结束后统一治理旧新两套 MCP 重复注册、六工具描述中的公共重复、jlink_hss Schema 膨胀以及 Skill 内容重复，在不改变已确认公共语义的前提下降低 Codex 上下文占用。",
      "status": "completed",
      "successCriteria": [
        "在开始上下文优化前完成当前计划内的全阶段真机测试，并保留可复核的调用顺序、原始 structuredContent、状态变化和故障证据。",
        "完成迁移验证后仅保留预期的 jlink_mcp 插件服务，旧 jlink_MCP_v2 不再重复暴露同一组六个工具。",
        "公共服务说明只保留一份权威入口，每个工具描述聚焦自身 action、关键约束和必要示例，不再机械重复通用规则。",
        "在保持 jlink_hss 严格输入输出语义、四种查询视图和客户端兼容性的前提下，减少重复展开的质量与联合返回 Schema，并通过真实 tools/list 和调用验证。",
        "Skill 保持根文件路由加按需 reference 的渐进披露，消除与 MCP instructions、工具描述之间无必要的重复，同时保留会话状态复用、副作用和错误恢复规则。",
        "记录优化前后 tools/list、Skill 及典型调用链的可比文本或 token 估算，证明上下文下降且六工具行为没有回归。"
      ],
      "specRefs": [],
      "decisions": [
        "全阶段测试完成前不禁用任一现有 MCP、不改六工具合同、不压缩 Schema，以避免改变当前硬件验证基线。",
        "后续优化不删除 effective/sources、联合体原始值与 Bits、逐项 validate 检查或结构化错误等具有独立语义的信息。",
        "上下文优化实施前创建独立 OpenSpec change，并以新 Codex 窗口中的工具发现、严格 Schema、成功与故障调用作为验收证据。"
      ],
      "createdAt": "2026-08-28T06:04:51.375Z",
      "updatedAt": "2026-08-28T10:41:31.815Z",
      "transitions": [
        {
          "from": null,
          "to": "proposed",
          "reason": "Plan recorded.",
          "at": "2026-08-28T06:04:51.375Z"
        },
        {
          "from": "proposed",
          "to": "accepted",
          "reason": "用户明确要求先记录重复 MCP、公共工具描述、jlink_hss Schema 与 Skill 上下文问题，并在全阶段测试完成后统一修复。",
          "at": "2026-08-28T06:04:55.799Z"
        },
        {
          "from": "accepted",
          "to": "in-progress",
          "reason": "全阶段真机测试已完成；用户决定不创建独立 OpenSpec change，要求在当前窗口按修复顺序直接实施，并先清理需要重启 Codex 验收的旧 MCP 注册。后续以该最新决定为准。",
          "at": "2026-08-28T07:43:32.747Z"
        },
        {
          "from": "in-progress",
          "to": "completed",
          "reason": "FT-001～FT-016 已按顺序完成根因修复、规格同步、插件重装和最终 11 阶段真机回归；tools/list 与 jlink_hss 分别下降 28.5% 和 43.8%，六工具、严格 Schema、状态复用、副作用与错误兼容均无回归。FT-017 已证明服务端资源完整并保持 Codex 客户端 external-blocked，不阻止本地计划收口。",
          "at": "2026-08-28T10:41:31.815Z"
        }
      ],
      "dedupeKey": "sha256:77bb0250bacb6487f80462646ed90444e99ade75b2d7eb76a10c3cfea857fb31"
    }
  ]
}
```
<!-- PROJECT_PLAN_DATA_END -->

## P001 交付 J-Link MCP V2 首版

- Status: `completed`
- Updated: 2026-08-28T02:46:54.371Z
- Plan references: `architecture/data-contract/changes/hss-agent-data-contract.c4`, `openspec/changes/define-jlink-mcp-v1`

按照已确认的 Q1-Q67 决策和架构边界，完成 Windows x64 本机 stdio 的 Rust J-Link MCP V2，实现烧录、读写、目标控制和固定时长 HSS 采集，并以真机与现代 MCP 客户端证据完成验证。

### Success Criteria

- 公共 MCP 接口与已确认的 Q1-Q67 决策一致，并具有可验证的输入、输出和错误 Schema。
- Rust 实现保持主进程、独立 J-Link Worker 和 Capture Store 的单一状态所有权及明确依赖方向。
- 首版在 Windows x64、SEGGER J-Link、ARM Cortex-M、SWD/JTAG 范围内实现烧录、校验、变量与内存读写、核心寄存器操作、目标控制和 HSS。
- HSS 支持结构体、数组、固定 1-300 秒采集、完整原始数据、确定性查询、质量证据和写入事件交错。
- 冻结一组经过实机验证的 DLL 版本与哈希基线，并验证 HSS ABI、帧格式、时间戳、尾排空、丢样和溢出行为。
- 使用目标现代 MCP 客户端验证 structuredContent、资源链接、分页游标、当前 MCP 所有的 Worker 正常关闭，以及意外中断 capture 的非完成恢复。

### Decisions

- V2 不兼容旧版，使用 Rust，不包含 RTT。
- 对外保持 6 个领域工具，通过少量 action 表达操作。
- 所有 J-Link DLL 调用由独立 Worker 单一拥有并串行执行；HSS 期间只交错变量和 RAM/MMIO 写入。
- 普通结果最小化；HSS 负责完整保存和确定性处理，Agent 负责解释与诊断。
- architecture/decision-review-q01-q67.md 是 Q1-Q67 冲突消解与最终产品决策记录。
- 在 codex/v1-implementation 分支持续完成 define-jlink-mcp-v1 的 2.1-5.5；5.6、5.7、OpenSpec 归档、发布 PR 和 JTAG 真机验证保持待办。
- 任务开发只运行主要测试和直接回归；原子提交执行受影响范围门禁，仅在 4.8 和 5.5 各执行一次完整 workspace 门禁与阶段纵向 smoke。
- SVN 检查采用事件触发并复用有效指纹；减少状态扫描不得减少 HSS 停止、尾排空、数据恢复、CPU 安全运行和硬件身份验证。
- 复用未变化的代码、规格和证据上下文；优先查询具体符号与差异，仅在文件变化、任务切换、证据冲突或上下文失效时重新读取。
- 普通机械问题在既有合同内自主修复；严重设计问题、合同或验证冲突及硬件安全异常立即停止并请求确认。
- P2 3.7、P3 4.8 与 P4 5.1-5.5 已完成；检查点 f124d2d 已推送，4.8 完成 capture 在输入指纹有效时继续复用；5.6 已按当前分支隔离 MCP 路线启动，5.7、归档和发布仍待办。
- Worker 由当前 MCP 创建并管理，生命周期绑定当前 Codex/MCP；正常关闭执行 HSS Stop、尾排空、非完成保存和目标断开，意外退出不续行，新 MCP 不接管旧 Worker、旧 HSS 或旧 capture_key。

### Status History

- 2026-08-25T03:13:38.576Z: created -> proposed — Plan recorded.
- 2026-08-25T03:13:43.366Z: proposed -> accepted — 用户已逐项确认 Q1-Q67，并确认最后三项接口偏差；产品范围、架构方向和公共接口边界已具备进入规格拆分的条件。
- 2026-08-25T03:17:29.726Z: accepted -> in-progress — 用户已开始 OpenSpec 规格拆分讨论；P001 从已接受方向进入规格化执行阶段。
- 2026-08-28T02:46:54.371Z: in-progress -> completed — V1 已完成 5.7 SWD 发布门禁并通过 PR #2 合入 main；9 份主规格共 65 条需求严格校验通过，define-jlink-mcp-v1 已归档。JTAG 真机验证按用户决定不属于本次发布验收，保留为后续独立工作。

## P002 全阶段测试后优化 J-Link MCP 上下文

- Status: `completed`
- Updated: 2026-08-28T10:41:31.815Z
- Plan references: none

保持当前六工具实现和安装状态完成全阶段真机测试并保留证据；测试结束后统一治理旧新两套 MCP 重复注册、六工具描述中的公共重复、jlink_hss Schema 膨胀以及 Skill 内容重复，在不改变已确认公共语义的前提下降低 Codex 上下文占用。

### Success Criteria

- 在开始上下文优化前完成当前计划内的全阶段真机测试，并保留可复核的调用顺序、原始 structuredContent、状态变化和故障证据。
- 完成迁移验证后仅保留预期的 jlink_mcp 插件服务，旧 jlink_MCP_v2 不再重复暴露同一组六个工具。
- 公共服务说明只保留一份权威入口，每个工具描述聚焦自身 action、关键约束和必要示例，不再机械重复通用规则。
- 在保持 jlink_hss 严格输入输出语义、四种查询视图和客户端兼容性的前提下，减少重复展开的质量与联合返回 Schema，并通过真实 tools/list 和调用验证。
- Skill 保持根文件路由加按需 reference 的渐进披露，消除与 MCP instructions、工具描述之间无必要的重复，同时保留会话状态复用、副作用和错误恢复规则。
- 记录优化前后 tools/list、Skill 及典型调用链的可比文本或 token 估算，证明上下文下降且六工具行为没有回归。

### Decisions

- 全阶段测试完成前不禁用任一现有 MCP、不改六工具合同、不压缩 Schema，以避免改变当前硬件验证基线。
- 后续优化不删除 effective/sources、联合体原始值与 Bits、逐项 validate 检查或结构化错误等具有独立语义的信息。
- 上下文优化实施前创建独立 OpenSpec change，并以新 Codex 窗口中的工具发现、严格 Schema、成功与故障调用作为验收证据。

### Status History

- 2026-08-28T06:04:51.375Z: created -> proposed — Plan recorded.
- 2026-08-28T06:04:55.799Z: proposed -> accepted — 用户明确要求先记录重复 MCP、公共工具描述、jlink_hss Schema 与 Skill 上下文问题，并在全阶段测试完成后统一修复。
- 2026-08-28T07:43:32.747Z: accepted -> in-progress — 全阶段真机测试已完成；用户决定不创建独立 OpenSpec change，要求在当前窗口按修复顺序直接实施，并先清理需要重启 Codex 验收的旧 MCP 注册。后续以该最新决定为准。
- 2026-08-28T10:41:31.815Z: in-progress -> completed — FT-001～FT-016 已按顺序完成根因修复、规格同步、插件重装和最终 11 阶段真机回归；tools/list 与 jlink_hss 分别下降 28.5% 和 43.8%，六工具、严格 Schema、状态复用、副作用与错误兼容均无回归。FT-017 已证明服务端资源完整并保持 Codex 客户端 external-blocked，不阻止本地计划收口。
