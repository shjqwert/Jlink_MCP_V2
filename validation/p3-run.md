# P3-4.3 HSS 串行运行证据

## 结论

P3-4.3 为 `PASS`。生产 Worker 已由单一 DLL 线程持续排空 HSS，并将管道接受与 DLL 调度分离；固定时长到期后执行一次内部 Stop 和有界尾排空。变量及 RAM/MMIO 写入保留在唯一串行 gateway 的排空间隙内，记录排队、执行、结果及下一次排空的样本影响；失败写入不会把仍可继续的采集自动终止。

本任务没有执行新的真机采集。冻结 F0-A 的 J-Link 6.98a ABI、零返回哨兵读取、尾排空和写入交错时间线继续作为硬件语义输入；生产纵向真机验证仍由 4.8 负责。

## 实现边界

- 管道监听线程只接受/写回 IPC，并把请求、Worker 接受时刻和一次性响应通道交给主线程；它不持有 `DllGateway`。
- 主线程在活动采集期间以 1 ms 为最长等待，每次请求分派前排空一次；写入完成后的下一轮立即再次排空。
- 会话在活动 HSS 期间拒绝读取、寄存器、控制、烧录、校验和断开；仅 `WriteMemory` 与 `WriteVariable` 进入写入时间线。
- 截止时间到达后只执行一次 Stop，随后最多 500 ms 排空，连续 20 次空读取才形成调度完成；短尾或未收敛返回稳定错误。
- 读取失败时仅尝试一次 Stop；Stop 失败后不再调用目标 DLL。启动失败会撤销新建的 `capture_key` 预留，不污染幂等索引。
- 完整原始字节和调度证据当前由 Worker 内存保留；失败生命周期/质量属于 4.4，可恢复 Capture Store 属于 4.5。

## 主要测试

执行时间：2026-08-27T05:13:37.675Z。

```text
cargo test -p jlink-worker hss::tests --lib
2 passed; 0 failed

cargo test -p jlink-worker runtime::tests::hss_status_requires_only_one_non_empty_capture_identity --lib
1 passed; 0 failed

cargo test -p jlink-mcp mcp::tests::p3_start_errors_remain_structured_and_distinct --lib
1 passed; 0 failed
```

T-P3-RUN 的主断言顺序为：`Start -> drain -> write(failed) -> drain -> deadline drain -> Stop -> 20 empty tail drains -> completed`。最终证据确认：

- 写前完整样本数为 1，紧随写入的排空后为 2；
- 写入保留 `VERIFY_FAILED`，采集继续到正常自动 Stop；
- Stop 只发生一次，尾排空达到 20 次连续空读取；
- 独立回归证明活动排空失败只触发一次 Stop，且不得形成 completed。

## 复用与剩余验证

- 复用 `validation/f0-a.md` 的冻结 6.98a 真机 ABI 和时间线；DLL、探针、目标、OUT、SWD 4000 kHz 指纹均未在本任务改变。
- 生产 10×32-bit、1 kHz、300 秒纵向真机测试已在 `validation/p3-stage.md` 闭环，确认当前调度路径的交错写入、自动 Stop 和尾排空。
- 本证据不声明 Capture Store、质量分类、父进程恢复或查询接口完成；这些由 4.4–4.8 和 5.1–5.5 分别闭环。
