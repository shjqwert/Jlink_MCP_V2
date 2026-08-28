## Why

旧版基于外部开源项目继续扩展，语言、架构和冗余功能使长期维护困难。V2 需要以可验证的 Rust 架构重新建立最小而完整的 J-Link 调试能力，并在正式实现前验证 HSS ABI、进程模型、DWARF 和 MCP 客户端等高风险假设，避免实现中途推翻项目。

## What Changes

- **BREAKING**：V2 不兼容旧版 MCP 工具、配置文件或采集格式，不包含 RTT、任意 GDB/J-Link 命令、SVD、断点、图表、CSV 和历史数据库。
- 在 Windows x64 上提供本机 stdio MCP，面向 SEGGER J-Link、ARM Cortex-M、SWD/JTAG。
- 首版只面向内部个人使用，不包含公开发布、多人权限或商业交付能力。
- 对外提供 6 个领域工具：`jlink_target`、`jlink_program`、`jlink_inspect`、`jlink_write`、`jlink_control`、`jlink_hss`。
- 使用工程配置和用户配置管理目标、本机 J-Link 环境、DLL 路径、版本及哈希，并允许 Agent 查询和修正配置。
- 使用由当前 MCP 进程创建并管理的独立 Rust Worker 单一拥有探针会话和所有 J-Link DLL 调用；Worker 生命周期绑定当前 MCP/Codex，主 MCP 进程通过版本化 IPC 负责公共合同和查询，Capture Store 负责持久化采集。
- 支持烧录、擦除、校验、内存/变量/核心寄存器读写和目标运行控制。
- 使用 ELF/DWARF 解析静态变量、结构体、数组、位域、union 和柔性数组切片。
- 使用固定 1–300 秒 HSS 采集，最多 10 个顶层选择项、请求频率最高 1 kHz；完整保存有效样本并提供质量证据。
- HSS 期间只允许变量和 RAM/MMIO 写入在排空间隙串行交错，记录事件和采样影响，不依赖 DLL 并发调用安全。
- 提供 `overview`、`changes`、`window`、`around_event` 四种确定性查询以及完整原始资源链接。
- 在生产实现前执行强制 Phase 0 可行性门禁；门禁失败或限制改变公共能力时必须先修订并重新确认规格。
- 采用五阶段纵向交付、Requirement→测试→证据追踪和风险分级测试，复用未失效的昂贵验证证据。

## Capabilities

### New Capabilities

- `mcp-contract`: 六工具目录、action 路由、公共输入输出、最小返回、错误包装和 Windows Codex 客户端合同。
- `project-configuration`: 工程/用户配置、合并优先级、配置查询/修改及 J-Link DLL 身份基线。
- `jlink-runtime`: 独立 Worker、IPC、动态 FFI、探针租约、调用串行化和进程故障隔离。
- `target-session`: 单活动目标、连接生命周期、状态、显式验证、halted/HardFault 恢复和验证缓存。
- `artifact-symbol-model`: 固件格式、ELF 身份、DWARF 路径和复合类型的无损值模型。
- `firmware-programming`: Flash 烧录、擦除、镜像校验、边界检查和显式烧录后状态。
- `debug-access`: 普通变量、内存、核心寄存器读写及 halt/resume/reset/step 控制。
- `hss-acquisition`: 固定时长 HSS 生命周期、采样计划、存储、质量、恢复和采集中写入交错。
- `hss-query`: HSS 摘要、变化、窗口、事件、阈值、时间语义、分页和原始资源访问。

### Modified Capabilities

无。当前 OpenSpec 基线为空，本 change 创建 V1 初始能力规格。

## Impact

- 新建 Rust workspace、MCP 主进程、J-Link Worker、IPC 合同、Capture Store、ELF/DWARF 解析和测试基础设施。
- 动态依赖本机 `JLink_x64.dll`；真机证据已选择 J-Link 6.98a 作为 HSS 主线，8.38 和 9.70 作为兼容性副线，所有线路均按版本和 SHA-256 冻结身份。
- 需要 SEGGER J-Link 探针、ARM Cortex-M 目标和代表性 ELF/DWARF fixture 执行 Phase 0 与发布验收。
- 需要实测 Windows Codex 对 `structuredContent`、资源链接、错误、游标和长时采集恢复的行为；ChatGPT Desktop、Claude 和其他客户端不属于 V1 验收范围。
- 本 change 只产生规格和实施计划；生产实现必须由后续显式 apply 工作流启动。
