# F0-D MCP 客户端可行性证据

## 当前裁决

`PASS_WITH_LIMIT`

目标客户端已修正并冻结为 Windows Codex。协议 SDK 自测和真实 Windows Codex 子进程均完成；
限制仅是 Codex 的客户端审批策略，不改变六工具公共合同或本机 stdio 接入方式。

## 隔离与客户端身份

- Windows 应用：`OpenAI.Codex 26.818.5229.0`；package full name
  `OpenAI.Codex_26.818.5229.0_x64__2p2nqsd0c76g0`。
- CLI：`codex-cli 0.149.1`。
- P0 server 全局注册名：`jlink_p0_v2_probe`。
- 既有正式/旧版 server 注册名：`jlink`；配置和启用状态未修改。
- 客户端实测使用 `--ignore-user-config`，只注入 `jlink_p0_v2_probe`，因此运行时没有加载旧
  `jlink` 或其他 MCP server。
- P0 server 是无 J-Link DLL、无硬件访问的合成 mock，不是生产版 MCP。

## 目标客户端结果

| 能力 | Windows Codex 结果 | 证据 |
|---|---|---|
| 六工具发现、严格 Schema、annotations | PASS | run 4 精确返回六个工具；额外字段以 MCP `-32602` 拒绝 |
| `structuredContent` | PASS | run 1 的 `jlink_target status` 直接返回连接与运行状态 |
| `resource_link` | PASS | run 2 返回 `jlink-mcp://capture/cap_f0d_001/raw` 与固定 MIME |
| 结构化工具错误 | PASS | run 1 的 `motor.sped` 返回 `SYMBOL_NOT_FOUND` |
| 不透明分页游标 | PASS | run 2 使用 `f0d:v1:changes:page-2` 完成两页遍历，末页无后续游标 |
| 新进程同 key 恢复 | PASS | run 3 用 `codex-f0d-client-001` 恢复 `completed`，`elapsed_us=88102000` |

协议 SDK 自测还验证了 `resource_link` 后的 `resources/read`、closed 输出 Schema、`isError`、
分页和关闭第一组 client/server 后的同 key 恢复。SDK 1.30.0 会把 Schema 参数错误返回为
`isError` 工具结果；mock 与证据按该实际行为固定。

## 已审查限制

当 Codex 使用 `approval_policy=never` 时，带 `destructiveHint: true` 的 `jlink_hss` 会在客户端
审批层被阻止；在允许审批的客户端运行中，run 2 已完成 HSS 启动、资源链接和分页查询。该限制是
Windows Codex 的安全策略，不要求修改工具名称、Schema、annotations 或服务端权限模型，因此裁决为
`PASS_WITH_LIMIT`。

## 冻结身份与证据

| 对象 | 冻结值 |
|---|---|
| Node.js / pnpm | `v24.19.0` / `11.19.0` |
| MCP SDK / Schema | `@modelcontextprotocol/sdk 1.30.0` / `zod 3.25.76` |
| mock server | `jlink-mcp-f0d-probe 0.1.0`；`server.mjs` SHA-256 `789557DDEF99B62F0BDD770F4CD7950828FCD8BE461F1A2D84317D9D5CC1E048` |
| client self-test | `selftest.mjs` SHA-256 `BD0752C6C4FDFFB86ED3347AE3383A61280CA8A98CD2556C218B8B9177E16ED4` |
| lockfile | `pnpm-lock.yaml` SHA-256 `98EA364B12F10C198D8CCD8A4F3C2BCDC92E0D42F59E1222EBC77791D423D029` |
| 协议自测 | `protocol-selftest.json` SHA-256 `D25FB72A28F7C2B04398C5E7092035F79E6C8026EDC5FF3D59FD55BF1E446B49` |
| Codex run 1 | JSONL `70EF42A1DCD2FB57D20D22788271DFABD00417E07668747577E53A03B7B882AF`；final `BCE7B2E255FEB3529F54AD137BAEF98C4F4EB75D6D63194C6687A7872267DB42` |
| Codex run 2 | JSONL `8EB0400F41ED78EE912D0622E529BDE2140B8E490DA8AB9477481842B84C8DB0`；final `C684B10823B3320339A6E71F6850B5DD5D9D0BFEAC134B639514CBEAF4EA8E82` |
| Codex run 3 | JSONL `A26566C986A32FAFFE3BC60A8BE17F611BF44E96F0B4BB113B85045CCD94CF98`；final `9FED175E4F6D174C8D3C6B486BBFDA8B05047C703E008A4BB12DA70134A3ACDD` |
| Codex run 4 | JSONL `61D573089EDBE9D1ED13FEA11CB327747805AA2E2431713D1122F637D19D219A`；final `55E54B348BCDF49FC9699A3B8FCA19D289B9DB5EADE4FF2C30D8A1DF8F3ED631` |

## 验证与复用

- `pnpm install --frozen-lockfile`：锁定依赖安装成功。
- `pnpm test`：八项协议检查全部 `PASS`。
- 四次独立 `codex exec`：真实 Windows Codex 客户端矩阵全部通过。

协议证据仅在 mock 源、lockfile、Node、SDK、Zod、MCP 合同和 stdio transport 不变时复用。
目标客户端证据还要求 Windows Codex 应用包/CLI、六工具 metadata、审批策略和交互不变；相关指纹
变化时仅重跑受影响的客户端矩阵。
