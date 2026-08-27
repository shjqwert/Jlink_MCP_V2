# P3-4.7 父进程续行与同键恢复证据

## 裁决

`PASS`

T-P3-RECOVER 已覆盖 RUN-004 与 HSSA-009 的生产恢复机制：Worker 通过 Windows 父进程同步句柄识别 MCP 退出；有活动 HSS 时不因父进程退出而提前结束，没有活动 HSS 时在有界轮询内退出并释放租约。完成文件和可解释的部分文件会在新 Worker 启动时重建 `capture_key` 索引，同键恢复不执行目标预检、重新连接或第二次 HSS Start。

## 冻结实现

- MCP 仅在创建新 Worker 时传入当前 PID；附着已有 Worker 不改写其原始固定时长所有权。
- Worker 活动采集期间维持 1 ms 调度等待；空闲时最多 100 ms 检查一次父进程句柄。
- 父进程退出且仍有活动 HSS 时，Worker 继续使用原 deadline、内部 Stop、尾排空和 Capture Store 发布路径；采集收口后退出。父进程退出且没有活动 HSS 时直接安全收口，不等待新命令。
- 采集 ID 和同键冲突同时绑定探针身份、完整 `TargetConnectionSpec` 指纹、`capture_key` 与包含固件/访问计划的请求指纹。
- Capture Store 自描述头保存完整目标连接身份；启动扫描同时恢复不可变 `.capture` 和可解释 `.partial`。文件名、头、终态清单或确定性采集 ID 不一致时拒绝恢复，不覆盖证据。
- 公共 `jlink_hss.status` 已接通 `capture_id` 与 `capture_key` 两条既有 Schema 路线。完成边界与旧 Worker 退出竞态只允许只读状态进行一次重新附着；不得重复提交 HSS Start。

## T-P3-RECOVER 与直接回归

```text
cargo test -p jlink-worker t_p3_recover --lib
2 passed; 0 failed

cargo test -p jlink-domain --test t_p3_start
5 passed; 0 failed

cargo test -p jlink-capture --lib
3 passed; 0 failed

cargo test -p jlink-domain --test t_p1_ipc_frame
3 passed; 0 failed

cargo test -p jlink-mcp hss --lib
3 passed; 0 failed
```

主要断言确认：

- 完成 capture 在原 Worker 状态丢失后仍按原 key 返回同一 ID 和终态；恢复调用在硬件预检前返回，第二个 HSS I/O 的调用记录为空。
- 相同 key 与相同 HSS 请求在目标速度变化时返回 `CAPTURE_KEY_CONFLICT`，并保留原目标与请求目标两个不同指纹。
- 未确认 Stop 留下的 `.partial` 在新 Worker 中恢复为同 ID 的 `aborted + unknown`，并可按原 key 查询。
- Windows 进程句柄能区分存活与已退出父进程；退出判定只关闭无活动 HSS 的 Worker，不中断活动采集。
- 实际 `t_p1_ipc` 生产二进制复核了命名管道附着优先、唯一 Worker、探针互斥、崩溃/正常释放，以及父进程退出后空闲 Worker 在 5 秒界限内退出并释放租约。

原子提交门禁同时通过：`cargo fmt --all -- --check`；四个受影响 crate 的 `cargo clippy --all-targets -- -D warnings`；OpenSpec `define-jlink-mcp-v1` strict 校验；`git diff --check`。Cargo manifest、crate 数量、依赖方向和 LikeC4 均未修改，因此没有运行无关门禁。

## 证据复用与阶段边界

- 复用 `validation/f0-b.md` 的真实 Windows 命名管道、父进程退出期间固定时长续行和新客户端重新附着模式。F0-B 的旧 capture-key 等价算法不再作为生产身份实现证据；完整目标身份和当前 Store 索引由本任务生产测试替代。
- 本任务没有修改目标 SVN 工程、OUT、DLL、探针、接口或速度，也没有执行真机 HSS，因此无需重新建立 SVN 基线。
- 生产 Worker 在真实 HSS 活动期间遭遇 MCP 退出后继续 Stop、尾排空、持久化和目标安全恢复的组合证据已在 `validation/p3-stage.md` 闭环；这不是用 Mock 替代真机声明。
- 父进程检测、Worker 启动参数、IPC 状态身份、目标指纹规则、Capture Store 头或恢复扫描任一变化时，本证据对应部分失效。
