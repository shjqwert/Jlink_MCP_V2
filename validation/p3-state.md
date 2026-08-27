# P3-4.4 HSS 状态与失败收口证据

## 结论

P3-4.4 为 `PASS`。生产领域模型已将 `starting/running/stopping/completed/failed/aborted` 生命周期与 `complete/degraded/unknown` 数据完整性分离，并禁止终态重写。Worker 在能够确认内部 Stop 时将采集故障受控收口为 `failed`，保留原始数据、失败码、部分数据事实与恢复通知；无法确认 Stop 时继续按硬件停机规则退出，不把未知硬件状态伪装为 `failed`。

本任务不需要新的真机操作。F0-A/F0-B 只作为冻结故障与恢复语义输入；生产持久化扫描和真机阶段结论分别由 4.5、4.8 形成。

## 确定边界

- 正常转换只允许 `starting -> running -> stopping -> completed`；`completed` 的完整性可以独立为 `degraded`。
- DLL Start 明确失败保留同一 `capture_id` 的 `failed + unknown + partial_available=false`，工具仍返回可重试 `HSS_START_FAILED`；等价同键请求恢复该身份而不重复 Start。
- 活动排空失败在一次 Stop 成功后转换为 `failed`；已经保留的完整记录和尾字节不会因失败丢弃。
- Stop 失败属于无法完成安全收口的硬件异常，直接终止 Worker 批次，不创建虚假的受控终态。
- 尾排空收敛但存在不完整尾字节时形成 `completed + degraded`，保留所有原始字节；具体短帧质量事件由 4.6 增加。
- `aborted + unknown` 只描述强制终止、存储中断或后续启动扫描发现的残留事实；4.4 冻结状态和通知，4.5 接入真实临时文件恢复。

## 主要测试

执行时间：2026-08-27T05:28:59.854Z。

```text
cargo test -p jlink-domain --test t_p3_state
3 passed; 0 failed

cargo test -p jlink-worker hss::tests --lib
5 passed; 0 failed

cargo test -p jlink-mcp hss_state_tests --lib
2 passed; 0 failed
```

T-P3-STATE 及直接回归证明：

- `completed + degraded` 与流程失败相互独立；
- `failed` 保留 `FRAME_INVALID`、部分记录和 `StopCompletedAfterFailure/PartialDataRetained` 通知；
- `aborted` 固定为 `unknown`，携带 reason、recoverable 和恢复通知；
- Start 失败可按同一采集身份恢复且不会再次调用 Start；
- Stop 无法确认时保持致命错误，不被重标为 `failed`；
- MCP 终态输出对 failed 返回 `failure_code/partial_available`，对 completed degraded 返回 `quality.integrity=degraded`。

## 剩余验证

- `complete` 完整性结论、溢出/间隔/丢样事件和实际频率仍属于 4.6；4.4 对无充分质量证据的正常采集保持 `unknown`。
- `.partial` 扫描、`aborted` 实际恢复、CRC 和原子完成属于 4.5。
- 生产 10×32-bit、1 kHz、300 秒硬件生命周期及恢复安全已由 `validation/p3-stage.md` 阶段纵向测试证明。
