## Purpose

定义 J-Link DLL、探针租约和跨进程执行的唯一所有权边界，使 DLL 崩溃、长时采集和并发请求不会破坏 MCP 主进程或产生多写者状态。

## ADDED Requirements

### Requirement: RUN-001 独立 Worker 单一拥有设备状态
每个 MCP 进程 MUST 最多为一个活动探针创建一个独立 Worker 子进程。只有该 Worker 可以加载 J-Link DLL、持有探针连接、修改目标会话状态或拥有活动 HSS 状态；MCP 主进程 MUST NOT 直接调用 DLL 或启用进程内 fallback。Worker 生命周期 MUST 绑定创建它的当前 MCP/Codex。

#### Scenario: 主进程提交目标请求
- **WHEN** MCP 主进程接收需要访问探针的有效操作
- **THEN** MCP 通过受版本控制的 IPC 合同创建或复用当前 MCP 已拥有的唯一 Worker 并提交请求

#### Scenario: Worker 创建或启动失败
- **WHEN** MCP 无法创建、附着或验证当前请求对应的 Worker 子进程
- **THEN** 系统返回 `WORKER_UNAVAILABLE` 或更具体的稳定错误，且不得继续设备操作

### Requirement: RUN-002 DLL 调用串行化
同一 Worker 内所有 J-Link FFI 调用 MUST 通过唯一 gateway 串行进入 DLL。系统 MUST NOT 假设同一连接支持并发 DLL 调用；允许并发提交的请求只能被排队和安全交错。

#### Scenario: HSS 期间提交变量写入
- **WHEN** HSS 正在排空且 Agent 提交允许的变量写入
- **THEN** Worker 在一次排空结束后串行执行写入，再立即继续排空，不发生并发 FFI 进入

### Requirement: RUN-003 跨进程探针租约
Worker MUST 在启动时取得与探针身份绑定的跨进程租约，并在连接、采集、Stop、尾排空、落盘和关闭清理全部结束前保持租约。其他进程无法取得租约时 MUST 返回明确忙状态。

#### Scenario: 第二个 MCP 访问同一探针
- **WHEN** 当前 MCP 的 Worker 仍持有该探针租约
- **THEN** 第二个 MCP 不得建立并行会话，并收到可重试的探针忙诊断

### Requirement: RUN-004 MCP 所有的 Worker 关闭边界
MCP 正常关闭时 MUST 请求当前 Worker 执行有界清理：活动 HSS 执行 Stop、尾排空并保存为非 `completed` 结果，随后断开目标、释放 DLL 和探针租约并退出。MCP 或 Worker 意外退出时 MUST NOT 承诺继续采集或由新 MCP 接管；下次启动只能恢复或清理遗留 Capture Store 尾部，并 MUST NOT 将其标记为 `completed`。

#### Scenario: 采集中 MCP 正常关闭
- **WHEN** MCP 在固定采集截止时间前进入正常关闭流程
- **THEN** Worker 停止 HSS、完成有界尾排空、保存非完成结果、断开目标并退出

#### Scenario: 采集中 MCP 或 Worker 意外退出
- **WHEN** 进程被强制终止并留下未完成 Capture Store 文件
- **THEN** 下次启动把该 capture 恢复为 `aborted + unknown` 或安全清理，且新的采集重新连接目标并使用新的 `capture_key`

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
