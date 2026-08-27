# P3-4.6 HSS 质量证据

## 结论

P3-4.6 为 `PASS`。生产 Worker 已在唯一排空路径中解析冻结 6.98a 帧时间戳，保存请求/实际频率、样本与间隔统计、短帧/格式/间隔/回退/溢出事件、loss/overflow 证据等级和跨时钟映射。生命周期与质量继续独立：稳定低于请求频率的采集可以正常 `completed`，但质量明确呈现实际频率和异常间隔。

冻结主线仍没有独立 overflow/sequence counter。没有直接信号时，生产结果固定输出 `loss.evidence=unknown`、`overflow.evidence=unknown`，省略 `lost_samples` 和 overflow 数量；不得据此声明零丢样或无溢出。若底层得到可识别的直接溢出信号，唯一质量管道会记录 `confirmed` 事件、Worker 时间和受影响记录范围，并将数据完整性降级。

## 主要测试 T-P3-QUALITY

`cargo test -p jlink-worker t_p3_quality --lib` 覆盖：

- 请求 1 kHz、源间隔 1 ms 时报告 `actual_rate_millihz=1000000`，但 loss/overflow 保持 `unknown` 且没有数值零。
- 稳定 2 ms 源间隔报告 500 Hz 实际频率、gap 统计和 `suspected` loss，采集仍为 `completed` 而不是工具失败。
- 可识别溢出信号生成 `confirmed` 事件、范围和降级完整性。
- 一个记录跨两次短读取时字节完整保留，短帧事件聚合，完成记录不丢弃。
- `12,345 ms` 精确归一化为 `12,345,000 us`；源单位、1000 Hz、1000 us 分辨率、Worker 单调域、Start 调用映射方法和误差上界同时保留。

## 直接下游回归

- `cargo test -p jlink-worker hss --lib`：4.3 调度、4.4 状态边界和 4.5 持久化接线继续通过。
- `cargo test -p jlink-capture --lib`：增加终态质量元数据后，T-P3-STORE 的 CRC/SHA、原子发布和恢复继续通过。
- `cargo test -p jlink-mcp hss_state_tests --lib`：完成结果的 `quality.integrity` 与失败/降级边界继续通过。

## 复用与剩余验证

- 本任务没有执行真机或修改目标 SVN 工程；DLL、探针、S32K144、SWD 4000 kHz、OUT 与保护文件指纹未变化。
- 复用 F0-A 已确认的 6.98a 帧布局、毫秒时间戳和“无 overflow/sequence counter”限制；脚本 I/O 只验证确定分类规则，不替代 4.8 真机质量证据。
- 4.8 仍需在生产 10×32-bit、1 kHz、300 秒链路确认源首尾时间、实际频率、碰撞/gap、交错写入关联和 unknown loss/overflow 结论。
- 帧布局、时间单位/分辨率、质量算法、直接溢出信号映射、Capture Store 终态、Worker Start 时刻或公共质量字段变化时，本证据对应部分失效。
