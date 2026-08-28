# P3-4.7 MCP 所有的 Worker 关闭与中断恢复证据

## 裁决

`PASS`

T-P3-RECOVER 已覆盖修订后的 RUN-004 与 HSSA-009：Worker 通过 Windows 父进程同步句柄绑定当前 MCP/Codex；正常关闭由内部 `Shutdown` 依次执行 HSS Stop、有界尾排空、非完成结果保存、目标断开和 Worker 退出；父进程意外退出时 Worker 不续采。新 Worker 只恢复或清理遗留 Capture Store 尾部，旧 `capture_key` 不跨生命周期续行，任何中断 capture 都不得成为 `completed`。

## 冻结实现

- MCP 创建 Worker 时传入当前 PID；只有同一父 MCP 可复用该 Worker，其他 MCP 得到 `PROBE_BUSY`，不得接管。
- Worker 每轮先检查父进程同步句柄；父进程退出后立即结束当前 Worker 生命周期，不因活动 HSS 而续行。
- 正常关闭使用内部 IPC `Shutdown`，不是新增公共 MCP action 或 Schema。活动 HSS 只调用一次 Stop，最多尾排空 500 ms，保存为 `failed` 非完成 capture 后断开目标并退出。
- 采集 ID 和同键冲突同时绑定探针身份、完整 `TargetConnectionSpec` 指纹、`capture_key` 与包含固件/访问计划的请求指纹。
- Capture Store 自描述头保存完整目标连接身份；启动扫描把可解释 `.partial` 截断到最后一个完整 CRC 块并恢复为 `aborted + unknown`。文件名、头、终态清单或确定性采集 ID 不一致时拒绝恢复，不覆盖证据。
- 不可变完成或非完成 capture 仍可按 `capture_id` 查询；启动扫描会退休上一 Worker 生命周期使用过的 key，按旧 key 查询失败，使用旧 key 发起新采集返回 `CAPTURE_KEY_CONFLICT` 且不触发硬件预检或 HSS Start。

## T-P3-RECOVER 与直接回归

```text
cargo test -p jlink-worker t_p3_recover --lib
4 passed; 0 failed

cargo test -p jlink-capture t_p3_store_recovers_verified_partial_blocks_as_aborted_unknown
1 passed; 0 failed

cargo test -p jlink-capture t_p3_store_crc_corruption_never_becomes_valid_partial_data
1 passed; 0 failed

cargo run -p jlink-mcp --example t_p1_ipc -- target\debug\jlink-worker.exe
PASS
```

主要断言确认：

- 正常关闭的活动 capture 只 Stop 一次，尾排空后持久化为 `failed`，`partial_available` 与实际完整记录一致，文件不会被标记为 `completed`。
- 尾排空读取失败时先持久化 `failed` capture，再把原始错误返回 MCP；关闭流程不会吞错、返回成功或继续执行目标操作。
- 已完成 capture 在新 Worker 中仍可按 ID 查询，但旧 key 已退休；用旧 key 发起新采集在硬件预检前返回 `CAPTURE_KEY_CONFLICT`，第二个 HSS I/O 调用记录为空。
- 未确认 Stop 或进程中断留下的 `.partial` 只恢复为同 ID 的 `aborted + unknown`；CRC 损坏的数据不会被当作有效部分证据。
- Windows 进程句柄能区分存活与已退出父进程；父进程退出会终止 Worker 生命周期，不再按活动 HSS 状态选择续行。
- 实际 `t_p1_ipc` 生产二进制复核了当前 MCP 单 Worker、跨 MCP 不接管、探针互斥、崩溃/正常释放，以及父进程退出后 Worker 在 5 秒界限内退出并释放租约。

提交前门禁通过：`cargo fmt --all -- --check`；workspace `cargo clippy --all-targets -- -D warnings`；T-P1-DOM、IPC frame、T-P1-IPC、T-P3-RECOVER 和两条 Capture Store 中断恢复回归；PowerShell stage smoke 语法解析；OpenSpec `define-jlink-mcp-v1` strict 校验；LikeC4 全项目语义校验（4 个源文件、0 错误）；`git diff --check`。Cargo manifest、crate 数量和依赖方向均未修改，因此不运行依赖门禁。

## 证据复用与阶段边界

- 复用 `validation/f0-b.md` 的真实 Windows 命名管道和探针租约证据；其中“父进程退出期间固定时长续行”和“新客户端重新附着”属于已废弃设计，不再作为生产验收事实。
- 本任务没有修改目标 SVN 工程、OUT、DLL、探针、接口或速度，也没有执行真机 HSS，因此无需重新建立 SVN 基线。
- `validation/p3-stage.md` 中既有 300 秒 HSS、交错写入、自动 Stop、尾排空、质量和目标安全恢复硬件事实继续保留；其中父进程续行与跨 MCP 同键恢复结论已明确作废。修订后的 stage smoke 只在单一 MCP 生命周期内验证 4.8。
- 父进程检测、Worker 启动参数、内部关闭 IPC、目标指纹规则、Capture Store 头或恢复扫描任一变化时，本证据对应部分失效。
