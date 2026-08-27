# P3-4.5 Capture Store 证据

## 结论

P3-4.5 为 `PASS`。生产 `jlink-capture` 已实现默认 512 MiB、工程可调整的单次采集上限，启动前最坏情况大小与磁盘可用空间预检，追加 CRC 块、周期检查点、终态 SHA-256 清单、发布前校验和同目录原子发布。完成资源只读打开且拒绝覆盖；启动扫描不删除临时文件或完成文件，并将未完成的已校验块标记为 `aborted + unknown`。

Worker 在唯一 DLL 调度线程内直接写 Capture Store，不经 IPC 回传高频原始数据。MCP 解析后的 `capture.max_bytes` 通过版本化内部 IPC 传给 Worker；活动采集只在内存中保留跨调用不完整尾字节、生命周期和调度证据。Stop 无法确认时保留 `.partial`，新 Worker 启动扫描后可按同一 `capture_id` 查询 `aborted` 状态。

## 主要测试 T-P3-STORE

`cargo test -p jlink-capture --lib` 覆盖：

- 估算超过配置上限时在创建临时文件和 DLL Start 前拒绝，并返回估算值与调整建议。
- 有效块独立 CRC，终态清单保存块计数、payload 字节数和原始 SHA-256；完成前校验临时文件，再原子发布唯一 `.capture`。
- 完成资源可重新校验打开，既有 `.capture` 和 `.partial` 均不得覆盖。
- 无终态临时文件恢复为 `aborted + unknown`，保留已验证完整记录和尾部事实。
- CRC 损坏块不计入有效部分数据，损坏文件保持可检查且不自动删除。

## 直接下游回归

- `cargo test -p jlink-worker hss --lib`：既有调度、失败状态、一次 Stop 和不完整尾字节测试继续通过；新增验证正常/Start 失败原子发布，以及 Stop 无法确认后由新 Worker 扫描为 `aborted + unknown`。
- `cargo test -p jlink-domain --test t_p1_ipc_frame`：新增可选内部字段保持帧版本、未知字段和截断防护。
- `cargo test -p jlink-mcp --test t_p3_start`：公共 HSS Schema、DWARF 展开和启动计划保持不变；`capture.max_bytes` 只沿既有内部 IPC 下传，没有扩大公共 Schema。

## 边界与复用

- 本任务不执行真机或修改目标 SVN 工程；冻结 DLL、探针、目标、SWD 速度、OUT 和保护文件基线均未变化。
- F0-A 的吞吐结果只作为块格式和 300 秒容量输入；生产 10×32-bit、1 kHz、300 秒持久化行为仍由 4.8 真机纵向验收闭环。
- Capture Store 版本、块头、CRC/SHA、终态清单、路径、大小估算、IPC 上限字段或 Worker 恢复路径变化时，本证据对应部分失效。
- 质量事件、父进程有限续行和查询/资源接口分别由 4.6、4.7、5.1–5.5 完成；本任务不声明这些能力已完成。
