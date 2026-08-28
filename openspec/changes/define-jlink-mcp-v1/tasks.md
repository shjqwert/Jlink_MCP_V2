## 1. P0 可行性门禁

- [x] 1.1 建立 change 内的 Requirement→主要测试→证据矩阵，登记证据指纹与复用/失效条件；每条 Requirement 只指定一个主要测试，避免跨层重复断言
- [x] 1.2 完成 F0-A HSS/写入/吞吐实验：以已连接的 S32K144、SWD、4000 kHz 和实际测试固件为主目标，以 J-Link 6.98a 毫秒时间戳模式为主线、8.38 和 9.70 为兼容性副线，验证最小 ABI、帧、源时间分辨率与 `timestamp_us` 归一化、持续和尾部排空、异常检测、串行调度影响及 300 秒存储上界，并冻结 DLL/探针/目标/固件身份和 Capture Store 编码候选
- [x] 1.3 完成 F0-B Worker 生存与租约实验：验证命名管道重新附着、父进程退出后固定时长续行、同探针互斥、`capture_key` 幂等和临时文件恢复
- [x] 1.4 完成 F0-C DWARF 复合类型实验：以 IAR 8.32 和隔离测试 fixture 为主组合，登记实际构建配置及产物指纹，并验证结构体、嵌套/多维数组、位域、union、柔性数组切片、64 位和非有限值访问计划
- [x] 1.5 完成 F0-D MCP 客户端实验：用最薄 mock server 验证目标 Windows Codex 的六工具发现、严格 Schema、`structuredContent`、资源链接、工具错误、分页游标和长任务恢复，并登记准确应用包与 CLI 版本
- [x] 1.6 对 F0-A 至 F0-D 分别裁决 `PASS`、`PASS_WITH_LIMIT` 或 `FAIL`；只有全部 `PASS`，或不改变公共能力且已记录/审查约束的 `PASS_WITH_LIMIT`，才开始 P1，任何改变公共能力的限制先修订 OpenSpec 并重新确认

## 2. P1 骨架与连接

- [x] 2.1 创建根 `Cargo.toml` 与 `crates/jlink-mcp`、`crates/jlink-worker`、`crates/jlink-domain`、`crates/jlink-capture` 四 crate workspace，冻结默认私有、禁止无职责通用层/纯转发 helper、依赖与抽象必要性门禁，加入依赖方向检查、统一格式/静态检查、英文 Rust 文档注释门禁，以及不进入普通 MCP 结果的本地阶段耗时观测；目标 IAR 测试 C/header 同时应用 embedded-code-style
- [x] 2.2 在 `jlink-domain` 实现版本化公共值对象、IPC 消息、稳定错误和状态转换，并以主要测试 T-P1-DOM 覆盖 MCP-004、RUN-005 的确定/不确定执行边界
- [x] 2.3 在 `jlink-mcp` 实现分层配置、逐字段来源、原子 `config_set`、具体器件标识、确定速度基线、DLL 身份和本机配置隔离，并以主要测试 T-P1-CFG 覆盖 CFG-001、CFG-002、CFG-003、CFG-004、CFG-005
- [x] 2.4 将 Windows 命名管道和探针租约修订为当前 MCP 创建并管理唯一 Worker 子进程；Worker 绑定父 MCP/Codex 生命周期，创建或附着失败时返回明确错误且不启用进程内 fallback；以主要测试 T-P1-IPC 覆盖 RUN-001、RUN-003，证据见 `validation/p3-recover.md`
- [x] 2.5 实现单活动目标的 connect/status/disconnect/validate、首次验证缓存和 halted/HardFault 恢复；断开态 validate 必须显式提供 `after: run | halt` 并复用唯一恢复流程后收口到请求状态，活动连接 validate 必须拒绝 `after` 且只观察当前会话；T-P1-SES 必须区分首次建连后的可观察状态与同一 Worker 会话内的真实 HardFault，并使用仅测试编译、无生产 IPC/MCP 入口且不修改 Flash/OUT 的注入器验证生产恢复状态机，覆盖 SES-001、SES-002、SES-003、SES-004、SES-005、SES-006
- [x] 2.6 实现六工具目录、严格 action Schema、最小成功结果、结构化错误与资源占位合同，并以主要测试 T-P1-MCP 覆盖 MCP-001、MCP-002、MCP-003、MCP-005；稳定错误合同复用 T-P1-DOM 的主要断言
- [x] 2.7 运行 P1 受影响测试和一个 connect→status→disconnect smoke，更新验收矩阵证据，不重复运行 F0 客户端或硬件实验

## 3. P2 静态调试

- [x] 3.1 实现 ELF/AXF/OUT、HEX、SREC、BIN 输入，补齐 `flash/verify` 的 BIN `base_address` Schema、`VALUE_INVALID` 及固件身份稳定错误，并实现 ELF/目标固件身份计划；以主要测试 T-P2-IMG 覆盖 ART-001、ART-002，同时回归 T-P1-MCP/T-P1-DOM
- [x] 3.2 实现唯一 DWARF 路径解析、`symbols` 搜索、按 ELF SHA-256/选择器/解析器版本缓存的不可变 `AccessPlan`，覆盖静态成员、固定数组、显式柔性 `slice` 的路径/偏移/大小计划及动态位置拒绝；本任务只验证显式切片解析机制，不重复 ART-004/`SLICE_REQUIRED`，并以主要测试 T-P2-DWARF 覆盖 ART-003、ART-005、ART-007、DBG-008
- [x] 3.3 实现位域/union、柔性或动态长度数组的独立显式 `slice {start,count}` 校验与缺失/无效时的 `SLICE_REQUIRED`，以及无损 `TypedValue` 编解码和复合写入全量预校验；路径 `[i]` 不得替代 `slice`，单元素使用 `count:1`，并以主要测试 T-P2-VALUE 覆盖 ART-004、ART-006、DBG-001
- [x] 3.4 实现 Flash 烧录、整片/范围擦除、默认与独立校验、边界检查及显式 `after`，并以主要测试 T-P2-PRG 覆盖 PRG-001、PRG-002、PRG-003、PRG-004、PRG-005、PRG-006
- [x] 3.5 实现变量执行与 1–4096 字节原始内存读写、短写检测、RAM/MMIO/Flash 分类、可选读回，以及仅针对无副作用相邻 RAM/静态变量区间的安全读取合并；MMIO、`volatile`、跨区和未对齐访问不得自动合并，并以主要测试 T-P2-MEM 覆盖 DBG-002、DBG-003、DBG-006
- [x] 3.6 实现核心寄存器读写和 halt/resume/reset/step 控制，并以主要测试 T-P2-CTL 覆盖 DBG-004、DBG-005
- [x] 3.7 按冻结 J-Link 6.98a SDK 精确修正实际使用导出的 ABI 声明、器件命令返回与错误输出，并使用有界日志/错误回调保留失败诊断；通用 `JLINKARM_ReadMem` 按 `0` 成功状态解释，类型化读取按完成项目数解释。flash/erase 在首个 Flash 副作用前执行 `reset_halt`，flash 默认校验在 `EndDownload` 后再次 `reset_halt`，独立 verify 保持只读且无新公共字段。冻结生产 smoke 已通过 `connect → flash（默认校验）→ 独立 verify → resume 初始化 `.data` → 变量/原始内存交叉读写并恢复 → 核心寄存器/目标控制 → reset_run → disconnect`，并观察活动 HSS 占位下的冲突路由；全程没有 Commander 成功路径、降速、重试、固定延时或其他 fallback，证据见 `validation/p2-stage.md`

## 4. P3 HSS 采集

- [x] 4.1 将 F0-A 冻结的 `GetCaps/Start/Read/Stop` 结构体、标志位和帧解析收敛为最小动态 FFI，并以主要测试 T-P3-ABI 覆盖 RUN-006；HSS 启动能力诊断只作为集成观察，证据见 `validation/p3-abi.md`
- [x] 4.2 实现带 `capture_key` 的固定时长请求、1–10 个顶层 DWARF 选择项、1–1000 Hz 目标频率、展开帧上限和启动预检，并以主要测试 T-P3-START 覆盖 HSSA-001、HSSA-002、HSSA-003；公共 Start 留待 4.3 调度器接入，证据见 `validation/p3-start.md`
- [x] 4.3 实现 HSS 优先的串行调度、持续排空、内部 Stop、尾部排空及变量/RAM/MMIO 写入交错，并记录排空、写入排队和执行阶段耗时；以主要测试 T-P3-RUN 覆盖 RUN-002、HSSA-004、HSSA-008、DBG-007，证据见 `validation/p3-run.md`
- [x] 4.4 实现生命周期与完整性双状态机、确定的 failed/aborted 边界、失败数据保留和恢复通知，并以主要测试 T-P3-STATE 覆盖 HSSA-005，证据见 `validation/p3-state.md`
- [x] 4.5 实现默认 512 MiB 且可由工程配置调整的单次采集上限、磁盘预检、追加块、校验提交、原子发布、临时文件恢复及不可变完成资源，并以主要测试 T-P3-STORE 覆盖 HSSA-006，证据见 `validation/p3-store.md`
- [x] 4.6 实现溢出、短帧、格式、间隔和丢样质量事件，以及请求/实际频率、源时间单位/分辨率和跨时钟域 `timestamp_us` 证据；毫秒源时间戳必须按 `ms × 1000` 精确归一化且不得宣称微秒分辨率，并以主要测试 T-P3-QUALITY 覆盖 HSSA-007、HSSA-010、HSSA-011，证据见 `validation/p3-quality.md`
- [x] 4.7 实现 MCP 正常关闭时的 HSS Stop、尾排空、非完成结果保存、目标断开和 Worker 退出；意外退出不续行，下次 Worker 只把遗留 Capture Store 恢复为 `aborted + unknown` 或安全清理，新采集必须使用新 `capture_key`；以主要测试 T-P3-RECOVER 覆盖 RUN-004、HSSA-009，证据见 `validation/p3-recover.md`
- [x] 4.8 运行一次 P3 真机纵向验收：固定时长采集期间交错写入、自动 Stop、尾排空、异常质量与恢复；复用未失效的 F0-A/F0-B 证据，不做重复压力测试；完成证据见 `validation/p3-stage.md`

## 5. P4 查询与发布

- [x] 5.1 实现按采集 ID/`capture_key` 的状态、部分范围和只含顶层变量导航计数的 `overview`，并以主要测试 T-P4-OVERVIEW 覆盖 HSSQ-001、HSSQ-002、HSSQ-003
- [x] 5.2 实现精确变化、启动时/查询时同义阈值和 `[*]` 路径规则，并以主要测试 T-P4-CHANGES 覆盖 HSSQ-004
- [x] 5.3 实现不丢重复值的 `window raw`、显式 `min_max`/`first_last`/`transitions` 与复用窗口边界的 `around_event`，并以主要测试 T-P4-WINDOW 覆盖 HSSQ-005、HSSQ-006
- [x] 5.4 实现变化区间、设备调用区间、跨时钟关系、一次性短 ID 字典和不可变快照游标，并以主要测试 T-P4-TIMELINE 覆盖 HSSQ-007、HSSQ-008、HSSQ-009
- [x] 5.5 实现自描述不可变原始资源、固定 MIME、版本/校验和 MCP 资源读取，确保只返回数据而不生成图片，并以主要测试 T-P4-RESOURCE 覆盖 HSSQ-010、HSSQ-011；MCP 资源合同只作为 T-P1-MCP 的端到端复核
- [x] 5.6 在目标 Windows Codex 以当前分支的隔离 MCP 配置运行一次端到端验收，验证低冗余结果、错误、资源、分页、当前 MCP 所有的 Worker 生命周期、正常关闭清理和写入后状态变化；不得调用已安装但指向旧项目的 `jlink` MCP，不测试或宣称跨 Codex 接管旧 HSS，只在客户端或相关指纹变化时重做 F0-D 能力验证；证据见 `validation/p4-client.md`
- [x] 5.7 完成 Requirement→测试→证据矩阵审计，运行一次 V1 全量发布门禁和 SWD 真机纵向验收，冻结实际通过的 DLL、探针、目标、编译器、客户端和限制支持矩阵；目标 IAR 测试夹具可按主要测试需要修改、编译、生成 OUT/DEP、烧录或擦除，并以当次实际指纹作为证据输入；JTAG 不作为本次测试项，在取得独立真机证据前记录为未验证且不声明支持；证据见 `validation/p4-release.md`
