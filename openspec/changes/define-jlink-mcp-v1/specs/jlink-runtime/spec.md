## Purpose

定义 J-Link DLL、探针租约和跨进程执行的唯一所有权边界，使 DLL 崩溃、长时采集和并发请求不会破坏 MCP 主进程或产生多写者状态。

## ADDED Requirements

### Requirement: RUN-001 独立 Worker 单一拥有设备状态
每个活动探针 MUST 最多对应一个独立 Worker。只有该 Worker 可以加载 J-Link DLL、持有探针连接、修改目标会话状态或拥有活动 HSS 状态；MCP 主进程 MUST NOT 直接调用 DLL。

#### Scenario: 主进程提交目标请求
- **WHEN** MCP 主进程接收需要访问探针的有效操作
- **THEN** 请求通过受版本控制的 IPC 合同发送给唯一 Worker 执行

### Requirement: RUN-002 DLL 调用串行化
同一 Worker 内所有 J-Link FFI 调用 MUST 通过唯一 gateway 串行进入 DLL。系统 MUST NOT 假设同一连接支持并发 DLL 调用；允许并发提交的请求只能被排队和安全交错。

#### Scenario: HSS 期间提交变量写入
- **WHEN** HSS 正在排空且 Agent 提交允许的变量写入
- **THEN** Worker 在一次排空结束后串行执行写入，再立即继续排空，不发生并发 FFI 进入

### Requirement: RUN-003 跨进程探针租约
Worker MUST 在连接开始时取得与探针身份绑定的跨进程租约，并在连接、采集、Stop、尾排空和落盘全部结束前保持租约。其他进程无法取得租约时 MUST 返回明确忙状态。

#### Scenario: 第二个 MCP 访问同一探针
- **WHEN** 活动 Worker 仍持有该探针租约
- **THEN** 第二个 MCP 不得建立并行会话，并收到可重试的探针忙诊断

### Requirement: RUN-004 主进程退出后的有限续行
MCP 主进程意外退出时，持有活动 HSS 的 Worker MUST 继续运行到原请求截止时间，执行 Stop、尾排空和落盘后释放租约；没有活动 HSS 的 Worker MUST 安全关闭并释放资源。

#### Scenario: 采集中主进程被终止
- **WHEN** Worker 检测到主进程退出但固定采集截止时间尚未到达
- **THEN** Worker 完成剩余采集和收尾，并保留可由 `capture_key` 恢复的结果

### Requirement: RUN-005 崩溃隔离和不确定执行
Worker 崩溃 MUST NOT 终止 MCP 主进程。主进程 MUST 将会话标记为 faulted，释放或等待租约恢复，并区分未执行、已失败和执行结果未知。

#### Scenario: 写入期间 Worker 崩溃
- **WHEN** 主进程无法证明写入是否到达目标
- **THEN** 结果为 `EXECUTION_UNCERTAIN`，后续请求不得复用旧连接状态

### Requirement: RUN-006 最小动态 FFI
Worker MUST 仅动态解析 V1 实际使用的 J-Link 导出，且每个实际调用的函数指针 MUST 逐项匹配冻结 J-Link 6.98a SDK 的参数、返回类型和 Windows x64 调用约定；启动时 MUST 报告缺失的必要导出，系统 MUST NOT 复制或绑定整套 J-Link SDK。DLL 打开入口 MUST 提供有界日志与错误回调作为本地失败诊断，回调 MUST NOT 重入 DLL、拥有业务状态或改变普通 MCP 成功结果。

#### Scenario: ABI 声明与冻结 SDK 不一致
- **WHEN** 任一已解析导出的函数指针声明不能证明与冻结 J-Link 6.98a SDK ABI 精确一致
- **THEN** 该导出不得进入生产调用路径，验证必须失败而不是依赖简化的 `i32` 或 `void` 声明解释结果

#### Scenario: HSS 导出缺失
- **WHEN** 普通调试导出完整但候选 DLL 缺少必要 HSS 导出
- **THEN** 普通能力可以按验证矩阵继续使用，但 HSS 启动返回 `DLL_EXPORT_MISSING`
