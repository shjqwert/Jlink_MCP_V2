# P0 可行性总门禁

## 裁决

`PASS_WITH_LIMIT`

F0-A 至 F0-D 均已完成。三个 `PASS_WITH_LIMIT` 的约束均已记录，且不删除或改变已确认的六工具
公共能力，因此允许进入 P1；本裁决不代表 P1 已开始，也不代表 P0 mock 是生产实现。

## 分项结果

| 包 | 裁决 | 关键证据 | 已审查限制 |
|---|---|---|---|
| F0-A HSS/写入/吞吐 | `PASS_WITH_LIMIT` | J-Link 6.98a、S32K144、SWD 4000 kHz；10 blocks × 1 kHz × 300 秒；300004 samples；3 次交错写入 | 源时间戳分辨率为 1 ms；无独立 overflow/sequence counter |
| F0-B Worker 生存与租约 | `PASS_WITH_LIMIT` | Windows named pipe 重新附着、父进程退出续行、同键幂等、租约、partial 恢复 | 强制终止可遗留文件租约；生产实现需 kernel lease 或 PID validation |
| F0-C DWARF 复合类型 | `PASS` | IAR 8.32.3、DWARF 3/4、15/15 AccessPlan | 无改变公共能力的限制 |
| F0-D Windows Codex | `PASS_WITH_LIMIT` | 六工具、严格 Schema、`structuredContent`、资源链接、错误、分页和跨进程恢复 | `approval_policy=never` 阻止 destructiveHint 工具；允许审批时通过 |

## 门禁影响

- P1 可以按 `openspec/changes/define-jlink-mcp-v1/tasks.md` 开始，但需由后续执行明确启动。
- HSS 实现必须保留源时间单位/分辨率和质量限制，不得把毫秒源宣称为微秒分辨率。
- Worker 生产租约不得直接复制 F0-B 的文件租约限制。
- Windows Codex 是 V1 唯一目标客户端；ChatGPT Desktop、Secure MCP Tunnel 和其他客户端不属于
  当前验收基线。
- 全局 P0 server 固定使用 `jlink_p0_v2_probe`，既有 `jlink` 配置保持不变。

## 证据入口

- `validation/f0-a.md`
- `validation/f0-b.md`
- `validation/f0-c.md`
- `validation/f0-d.md`
- `validation/requirement-matrix.md`
