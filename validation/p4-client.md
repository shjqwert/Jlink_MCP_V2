# P4 5.6 Windows Codex 客户端验收

## 结论

`PASS`。目标 Windows Codex 仅加载当前分支的隔离 MCP `jlink_mcp_v2_acceptance`，完成六工具发现、严格输入、结构化错误、不可变查询、游标续页、原始资源读取和当前 MCP 所有的 Worker 生命周期验收。全局注册且指向旧项目的 `jlink` 未加载、未调用。

本验收保持六工具 action 与严格 Schema 形状不变；不测试或宣称跨 Codex 接管旧 HSS。5.7 发布门禁、OpenSpec 归档、发布 PR 和 JTAG 真机验证继续待办。

## 客户端与产品身份

| 项目 | 冻结值 |
|---|---|
| Windows Codex | `OpenAI.Codex 26.820.7780.0`；`OpenAI.Codex_26.820.7780.0_x64__2p2nqsd0c76g0` |
| Codex CLI | `codex-cli 0.150.0` |
| Windows | `Windows 10 Home China`；build `26200` |
| 隔离 MCP | `jlink_mcp_v2_acceptance`；`--ignore-user-config --ephemeral`；required |
| 模型 | `gpt-5.6-sol`；reasoning effort `max`；approval `never`；sandbox `danger-full-access` |
| 产品二进制 | `target/release/jlink-mcp.exe`；SHA-256 `DBE1DBA64444C15FC370DF175C02213E5EBED7C82344B49E78EE788DD42EE411` |
| 客户端运行目录 | `target/evidence/p4-5.6/client-runtime` |

启动前后均未发现残留 `jlink-mcp`、`jlink-worker` 或 `JLink` 进程。该客户端验收没有修改目标 SVN 工程，也没有重新建立或改变 DLL、探针、目标、接口、速度、OUT 和保护文件指纹。

## 查询合同修复

首次隔离查询最短复现发现两个相互独立、均在既有公共合同内的问题：

1. 早期工具说明没有帮助 Codex 识别查询视图的扁平参数，Codex 曾自行构造不存在的包装对象。修复阶段曾在 `jlink_hss` 说明中补齐骨架；全阶段 FT-003 后改由运行时严格 Schema 作为字段权威、按需 HSS reference 负责查询策略，避免在目录中复制完整骨架。
2. overview 按合同返回复合变量顶层短 ID `s0`，但查询解析器只接受叶 ID `s0.0...`。解析器现将合法顶层短 ID 确定性展开为其全部叶序列；精确叶 ID 和路径行为不变，并增加 T-P4-CHANGES 回归。

修复没有增加 action、字段、fallback 或宽松解析；客户端在修复后首次正确构造查询，不依赖重试、忽略错误或硬编码结果。

## 隔离客户端结果

### 工具、错误与 Worker 生命周期

- 预检只发现当前产品的六个闭集工具；`jlink_target status` 返回断开态结构化结果。
- 活动 HSS 占用期间的冲突路由返回 `OPERATION_CONFLICT`，没有自动回退或第二 DLL 入口。
- 当前 MCP 正常关闭复用 `validation/p3-recover.md` 的 T-P3-RECOVER 证据：内部 Shutdown 执行 Stop、有界尾排空、非完成结果保存、目标断开和 Worker 退出。
- 直接终止 Codex/MCP 的实测 capture `cap_4824676c95f80aed28298c60` 在下次启动只恢复为 `aborted + unknown`，包含 4057 条完整记录；旧 key 被拒绝，目标内存保持 `78563412`、CPU 为运行态且目标断开。中断文件没有标记为 `completed`，新 MCP 没有接管旧 HSS。

### 完成 capture、写入与查询

- 完成 capture：`cap_aab08eb4d3289ec2caa62277`，key `p4-5.6-completed-20260827-1`；10×32-bit、1 kHz、10 秒，共 10001 条记录，`elapsed_us=10038021`，实际速率 `1000000 millihz`。
- 活动采集中的内存写入 `efcdab89` 已读回确认，随后恢复原值 `78563412`；采集正常完成，目标安全状态不变。
- overview 返回顶层字典 `s0 -> gaulAppUserDescHssTest`、采集范围 `[0,10001000)`、一个写入事件和规范 raw resource link。
- changes 使用顶层 `s0` 查询成功；limit=1 时返回稳定字典和持久化写入事件，未产生错误或替代查询。

### Window 游标与原始资源

- 第一页首次调用即使用扁平请求：`action=query`、`view=window`、`series=[s0]`、范围 `[0,10001000)`、`mode=raw`、`limit=2`。
- 第一页时间为 `[0,1000]`，一次性字典包含 `s0.0..s0.9`；10 个序列分别保持值 `0..9`，`truncated=true` 并返回不透明游标。
- 续页请求只含 `action`、`capture_id` 和 `cursor`，没有 `view` 或视图字段。第二页字典为空，时间为 `[2000,3000]`，序列顺序和值保持稳定；两页严格递增，无重叠、漏项或重复。
- Codex 通过 `jlink-mcp://capture/cap_aab08eb4d3289ec2caa62277/raw` 调用 `resources/read`，观察到 MIME `application/vnd.jlink-mcp.capture.v1+binary` 和 `JMCPV101` 头。源文件为 465461 bytes，SHA-256 `8CA2427AC63578CF874993A1B47C8C68C93E216D5D0C464F5300FDD585806B9C`；完整字节一致性继续由 T-P4-RESOURCE 的主要测试负责。

隔离 Codex thread：`01a04351-2c51-7a53-ae31-0d7661dff1b3`。客户端在总结资源内容时重复调用读取接口，但每次均成功且没有 Schema、业务、资源或状态错误；该冗余不触发服务端 fallback，也不改变验收结果。

后续 FT-017 复核使用更大的不可变文件区分服务端与客户端边界：磁盘及服务端规范资源为 201,208 bytes、头 `JMCPV101`、SHA-256 `A57C54A9E44FEC68E267FD9C010713BACA3F6B6AB8FD52D231307A9AB3CB8060`，完整 Base64 应为 268,280 字符；Codex 通用资源链路只交付 47,798 字符且长度不能被 4 整除。该项标记为客户端 external-blocked，服务端继续返回完整标准资源，不增加本地路径字段或删除 `resources/read`。

## 最小回归与证据复用

- `cargo test -p jlink-capture --test t_p4_changes --test t_p4_window`：PASS，7 个 changes/window 回归。
- `cargo test -p jlink-mcp --test t_p4_changes --test t_p4_window`：PASS，4 个 MCP 查询回归。
- `cargo test -p jlink-mcp --test t_p1_mcp t_p1_mcp_catalog_is_closed_and_action_strict -- --exact`：PASS，扁平查询说明与闭集目录合同。
- `cargo fmt --all -- --check`、`cargo clippy -p jlink-mcp --all-targets -- -D warnings`、`git diff --check`：PASS。
- 当前 MCP Worker 关闭和中断恢复实现及主要测试复用 `validation/p3-recover.md`；10×32-bit、1 kHz、写入交错和目标恢复硬件事实复用 `validation/p3-stage.md`。相关代码路径、DLL、OUT、探针、目标、接口、速度和测试输入指纹均未变化，因此不重复真机 smoke。

## 5.7 客户端指纹变更后定向复核

5.7 发布门禁发现 Windows Codex 和最终产品二进制指纹已变化，因此只重做客户端敏感路线。`OpenAI.Codex 26.820.10647.0`、`codex-cli 0.150.1` 使用 `--ignore-user-config --ephemeral`，仅加载 required server `jlink_mcp_v2_acceptance`；产品 `jlink-mcp.exe` SHA-256 为 `E4BBAF457E7D9355179D497A4D1411F051B44EE1FF4E03D2FA47CB494F909F9B`。

定向复核在不可变完成 capture `cap_0373e535731d6433da7d43bd` 上再次通过六工具闭集、严格 Schema `-32602`、结构化断开态、overview、window 首/续页和 raw resource。分页时间为 `[0,1000]` 与 `[2000,3000]`，续页只携带 `action/capture_id/cursor`；资源 MIME 和 `JMCPV101` 头保持正确。全局旧 `jlink` 未加载或调用，结束后无相关残留进程。完整发布证据与当前支持矩阵见 `validation/p4-release.md`。

## 证据失效条件

- Windows Codex 应用包、CLI、MCP 产品二进制、六工具 metadata、严格 Schema、资源处理或审批行为变化。
- HSS 查询说明、顶层/叶 series ID 规则、游标绑定、分页排序、Capture Store 格式、资源 URI/MIME 或 Worker 生命周期变化。
- DLL、探针、目标、OUT、接口、速度、测试 fixture 或安全恢复合同发生相关变化时，仅对应的硬件复用证据失效。
