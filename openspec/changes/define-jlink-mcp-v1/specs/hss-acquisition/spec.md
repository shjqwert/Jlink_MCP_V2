## Purpose

定义固定时长 HSS 采集从预检、启动、持续排空、写入交错到自动停止、持久化和恢复的完整合同，并确保任何数据质量问题都可见且可追溯。

## ADDED Requirements

### Requirement: HSSA-001 固定时长采集请求
每次 HSS 启动 MUST 要求 `capture_key`、`duration_s`、`rate_hz`、1–10 个顶层变量选择项和 `return_when`。`return_when` MUST 且只能为 `started` 或 `completed`；`duration_s` MUST 为 1–300，`rate_hz` MUST 为 1–1000；请求频率是目标值而非无条件保证值。相同 `capture_key` 和等价请求 MUST 幂等恢复，非等价复用 MUST 返回冲突。

#### Scenario: 首次启动有效采集
- **WHEN** Agent 提交有效参数和未使用的 `capture_key`
- **THEN** 系统创建唯一采集并按照 `return_when` 约定返回活动状态或最终结果

#### Scenario: 重试相同启动请求
- **WHEN** Agent 使用同一 `capture_key` 重试语义等价的启动请求
- **THEN** 系统返回原采集的当前状态，不创建第二个采集

#### Scenario: 复用键但参数不同
- **WHEN** Agent 使用已有 `capture_key` 提交非等价采集参数
- **THEN** 系统拒绝请求并返回键冲突及原请求指纹

### Requirement: HSSA-002 符号化采样计划
HSS 选择项 MUST 由 ELF/DWARF 静态变量路径解析，不得接受原始地址。结构体或数组选择项 MUST 在启动前展开为固定偏移的采样帧；最多 10 个限制 MUST 按 Agent 提交的顶层选择项计算。展开后的帧大小 MUST 通过探针能力和实现上限校验。

#### Scenario: 选择结构体和数组切片
- **WHEN** Agent 提交可静态解析的结构体路径和定长数组或显式数组切片
- **THEN** 系统生成确定的固定偏移采样计划并保留成员路径映射

#### Scenario: 选择动态位置变量
- **WHEN** Agent 提交需要运行时指针跟随或动态位置表达式的变量
- **THEN** 系统在启动前拒绝该选择项并指出不受支持的定位方式

### Requirement: HSSA-003 启动预检与目标恢复
系统 MUST 在启动前检查必要 HSS 导出函数、探针能力、目标连接、目标运行状态、后台内存访问能力，以及源时间戳的单位、分辨率和单调性能力。V1 MUST 支持 J-Link 6.98a 的毫秒源时间戳模式，不得要求 DLL 提供微秒模式。HSS 启动检查 MUST 在 Worker 已持有的同一 J-Link 目标会话内观察目标状态；目标 halted 或 HardFault 时 MUST 复用 `target-session` SES-003 的唯一恢复流程。HSS 只消费恢复结果并保存其通知，不得实现第二套恢复规则。

#### Scenario: halted 目标恢复成功
- **WHEN** HSS 启动预检发现目标 halted 且 `resume` 后能够稳定运行
- **THEN** 系统继续采集、保持运行状态并在结果中通知发生过自动 `resume`

#### Scenario: resume 后进入 HardFault
- **WHEN** 自动 `resume` 后目标进入 HardFault 且复位后运行恢复正常
- **THEN** 系统继续采集并通知发生过 `resume` 和 `reset`

#### Scenario: 必要导出缺失
- **WHEN** 当前 DLL 缺少任一必要 HSS 导出函数
- **THEN** 系统不启动采集，并列出缺失能力、DLL 身份和修正方向

### Requirement: HSSA-004 内部自动停止与尾部排空
系统 MUST 按固定时长在内部自动停止采集，不得依赖 Agent 的第二次停止调用，也不得在 V1 暴露公共 cancel/stop action。采集期间 MUST 持续排空 DLL 缓冲区；到期后 MUST 调用停止并继续排空所有可读取尾部数据，再完成持久化。

#### Scenario: 固定时长到期
- **WHEN** 采集达到请求时长
- **THEN** Worker 自动停止 HSS、完成尾部排空并将采集转换为终态

#### Scenario: Agent 未保持连接
- **WHEN** 启动采集的 MCP 客户端在采集期间断开
- **THEN** Worker 仍按原时长完成停止、尾部排空和持久化

### Requirement: HSSA-005 生命周期与数据完整性分离
采集生命周期 MUST 使用 `starting`、`running`、`stopping`、`completed`、`failed` 或 `aborted`；数据完整性 MUST 独立使用 `complete`、`degraded` 或 `unknown`。检测到质量问题时，系统 MUST 保留仍可解释的数据，不得仅因数据降级将其静默丢弃。能够执行受控故障收口但未形成正常完成 capture 时 MUST 使用 `failed`；进程被强制终止、存储中断或启动扫描发现遗留临时采集时 MUST 使用 `aborted`。

#### Scenario: 采集完成但存在可识别丢样
- **WHEN** 自动停止和持久化完成且检测到部分丢样
- **THEN** 生命周期为 `completed`、完整性为 `degraded`，并保留有效样本和丢样证据

#### Scenario: Worker 退出且尾部状态不可知
- **WHEN** Worker 在确定停止结果前被强制终止并由后续启动扫描发现残留采集
- **THEN** 系统将采集标记为 `aborted`、完整性为 `unknown`，并保留已经落盘的可验证数据

#### Scenario: 受控启动失败
- **WHEN** Worker 能够完成故障收口但 HSS 未形成正常完成的 capture
- **THEN** 系统将采集标记为 `failed` 并返回 failure code 和是否存在部分数据

### Requirement: HSSA-006 有界且可恢复的本地存储
系统 MUST 在启动前估算空间并执行工程配置的 `capture.max_bytes` 单次采集上限；未配置时默认 512 MiB。有效配置可以调低或调高该上限，但不得绕过可用磁盘空间预检。活动采集 MUST 写入可恢复的临时文件；只有完成终态元数据和校验信息后才能原子发布最终资源。启动时 MUST 检测并恢复或明确标记残留临时文件，且 MUST NOT 自动删除已完成采集。

#### Scenario: 预计超过单次上限
- **WHEN** 根据采样计划、频率和时长估算的采集大小超过有效 `capture.max_bytes`
- **THEN** 系统在启动前拒绝请求并给出估算大小和降低请求的方法

#### Scenario: 进程退出留下临时文件
- **WHEN** Worker 重启时发现未完成采集文件
- **THEN** 系统依据可验证元数据恢复可用部分或标记为不可恢复，且不把它误报为完整采集

### Requirement: HSSA-007 数据质量检测
系统 MUST 检测并报告 DLL 缓冲溢出、短帧、帧格式异常、实际样本间隔异常和能够识别的丢样。丢样证据 MUST 区分 `confirmed`、`suspected` 和 `unknown`；缺少可靠计数依据时 MUST NOT 报告零丢样。

#### Scenario: DLL 报告缓冲溢出
- **WHEN** 采集期间 DLL 返回可识别的缓冲溢出信号
- **THEN** 系统记录发生时间、受影响范围和 `confirmed` 质量事件，并将完整性降级

#### Scenario: 无法证明是否丢样
- **WHEN** 帧格式没有序号且时钟证据不足以确认丢样数量
- **THEN** 系统将相关质量状态标记为 `unknown` 或 `suspected`，不得输出 `lost_samples: 0`

### Requirement: HSSA-008 采集中写入交错
HSS 活动期间，只允许变量写入或 RAM/MMIO 写入进入 Worker；它们 MUST 按 RUN-002 的唯一串行 gateway 规则在缓冲排空间隙执行。每次写入 MUST 记录请求时间、执行开始与结束时间、结果以及可观测的采样影响；失败写入 MUST 报告错误，但除非采集本身无法继续，不得自动终止采集。

#### Scenario: 采集中变量写入成功
- **WHEN** Agent 在 HSS 活动期间提交有效变量写入
- **THEN** Worker 在一次排空后的安全间隙执行写入，并将该写入作为时间线事件保存

#### Scenario: 写入导致排空间隔异常
- **WHEN** 交错写入使相邻样本或排空时间超过预期范围
- **THEN** 系统同时记录写入事件和对应质量影响，不把变化错误归因为采样值本身

### Requirement: HSSA-009 采集恢复与探针租约
采集身份 MUST 同时绑定稳定的采集 ID、`capture_key`、目标身份和探针租约。主 MCP 进程重启后 MUST 能查询仍在运行或已完成的采集；租约或 Worker 状态不确定时 MUST 先恢复事实，不得重复启动同一采集。

#### Scenario: MCP 主进程重启后查询
- **WHEN** Worker 在主进程退出期间继续采集且新的主进程使用原 `capture_key` 查询
- **THEN** 系统返回同一采集的状态和 ID，不重新连接探针或启动第二次采集

### Requirement: HSSA-010 请求频率透明度
系统 MUST 保存请求频率、实际样本数和可观测时间间隔统计。实际频率低于请求频率本身 MUST NOT 自动使采集失败，但任何超出已验证能力或出现异常间隔的情况 MUST 在质量信息中明确呈现。

#### Scenario: 实际频率稳定但低于请求值
- **WHEN** 请求 1 kHz 而硬件稳定提供较低实际频率且没有其他错误
- **THEN** 系统完成采集并报告请求频率、实际样本计数和间隔统计，不宣称达到 1 kHz

### Requirement: HSSA-011 统一时间表达与源分辨率
每个样本批次和时间线事件 MUST 能映射到单调递增的 `timestamp_us`。J-Link 6.98a 的毫秒源时间戳 MUST 通过 `milliseconds × 1000` 精确归一化，系统 MUST 同时保存源单位和 1 ms 分辨率，不得因公共字段使用微秒单位而宣称微秒分辨率。若 DLL 与主机事件使用不同时间域，系统 MUST 保存各自时间域及已知映射误差，不得伪造绝对同步。

#### Scenario: DLL 与主机时钟映射存在误差
- **WHEN** 采样时间来自 DLL 而写入事件时间来自主机单调时钟
- **THEN** 系统保存两种时间域、映射方法和误差界限，以支持确定性的先后关系判断

#### Scenario: 毫秒源时间戳归一化
- **WHEN** J-Link 6.98a 返回毫秒源时间戳
- **THEN** 系统将其精确转换为 `timestamp_us`、记录 1 ms 源分辨率，并且不报告源时间具有微秒精度
