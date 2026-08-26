# F0-B Worker 生存、租约与恢复证据

## 裁决

`PASS_WITH_LIMIT`

真实 Windows 命名管道已验证重新附着；控制父进程退出后，独立 Worker 按原截止时间完成采集、发布文件并释放探针租约。`capture_key` 等价请求返回同一采集 ID，非等价复用返回冲突，同探针第二 Worker 被拒绝，截断 `.partial` 能恢复到最后一个完整 CRC 块。

限制：实验使用 `create_new` 租约文件验证跨进程互斥。正常 shutdown 会释放租约，但强制终止 Worker 会遗留 stale 文件，因此该机制不冻结为生产实现。P1 必须采用 owner-death-safe Windows 内核租约，或实现带 PID/进程身份校验的 stale-owner 恢复。该实现约束不改变公共合同。

## 冻结身份

| 对象 | 身份 |
|---|---|
| 实验源码 | `experiments/p0/f0b-worker/src/main.rs`；SHA-256 `53B733EA5DE994562B468AFCFACC8EE59DDCF1B7B6F74FC15DDF0B3461BB328A` |
| Release 二进制 | `f0b-worker.exe`；633,344 bytes；SHA-256 `47100B748F51E6D86500917C6E41EA35CB254F0A9D2E0D19BDA21B7EB3040898` |
| OS 边界 | Windows named pipe `\\.\pipe\jlink-mcp-v2-f0b-*`；长度前缀 JSON 控制帧 |
| 模拟探针身份 | `260106173`，仅用于验证同身份跨进程租约；本实验不访问硬件 |
| 接受证据 | `validation/evidence/f0-b/suite-v3.json`；SHA-256 `5BA0712971E2D02DB74BDC8621BC0C648C6A179350AE0C4D434D500F718983D4` |

## 接受场景

1. 测试父进程启动独立 Worker，通过命名管道提交 `capture_key=f0b-parent-exit-capture`、`duration=3000 ms` 后同步写入回执并立即退出。
2. 父进程 PID `57588` 退出后，Worker PID `9196` 仍处于运行采集状态。
3. 新客户端重新连接同一命名管道，并以等价请求恢复；返回原采集 ID `2ee33b8fff4a19f0b7b2bd3e` 和 `idempotent=true`。
4. 使用相同 `capture_key` 但不同 duration 的请求返回 `CAPTURE_KEY_CONFLICT`。
5. 第二 Worker 对同探针身份取得租约失败并报告 `PROBE_BUSY`。
6. Worker 达到原截止时间后发布 60 个有效 CRC 块；完成文件 SHA-256 为 `065912649867BDF81CFEFC709CCB2D120DD29AF5465675AEECDF8B64A0E77D71`。
7. shutdown 后没有 `f0b-worker` 进程残留，正常路径租约文件已释放。

## 临时文件恢复

启动前构造的 `orphan.partial` 包含 3 个完整块和第 4 个截断块。Worker 启动扫描得到：

- 恢复前 75 bytes；恢复后 60 bytes；
- `validBlocks=3`；
- `truncatedTail=true`；
- `crcError=false`。

恢复后文件 SHA-256 为 `257ED55CE23FB21D4441222961C784D55F06C7ABF33EBAB5305DF9CA9D8E1FD7`。未验证尾部不会作为有效数据发布。

## 未接受尝试

- v1 没有把“父进程退出时采集仍在进行”作为强制断言，结果该字段为 false，因此不作为通过证据。
- v2 复用了 v1 的 `parent-receipt.json`，`create_new` 正确拒绝覆盖；已确认路径的隔离测试 Worker PID `34588` 被强制终止，失败证据保留。该操作同时暴露了文件租约的 stale-owner 限制。

## 复用与失效条件

证据只在 Windows named-pipe API、控制帧协议、Worker/父进程边界、采集键等价规则、租约身份规则和 CRC 临时文件语义不变时复用。IPC transport、进程模型、租约算法、`capture_key` 等价定义、截止时间所有权或临时文件格式变化时必须重测。
