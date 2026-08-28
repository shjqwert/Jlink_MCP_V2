# P4 5.1 Overview 验证

## 结论

PASS。T-P4-OVERVIEW 已覆盖 HSSQ-001、HSSQ-002、HSSQ-003 的只读完成快照路径；未连接探针，未修改目标 SVN 工程，也未复跑 P3 真机证据。

## 实现边界

- Capture Store 新增不创建目录的只读打开，以及按 `capture_id`/`capture_key` 查找不可变完成文件。
- overview 在重新校验文件头、块 CRC、终态清单和原始 SHA-256 后读取拼接 payload；启动扫描仍不保留 payload，避免常驻复制完整采集。
- 时间范围固定为半开区间 `from_us/to_us`，结束边界为最后一条完整记录的源时间加 6.98a 已声明的 1 ms 源分辨率。
- 顶层变量按请求顺序登记稳定 `s0..sN`；完整路径只在 `dictionary` 出现，变量项只含 `series/samples/changes`。
- `changes` 统计相邻完整样本的顶层选择字节是否变化；不解码成员，不生成成员预览。
- `events` 统计持久化写入、恢复和质量事件出现次数；6.98a 的 loss/overflow 为 `unknown` 时保留 `quality`，空 `quality.events` 省略。
- `query.overview` 只访问不可变完成文件，不加载 J-Link DLL、不连接 Worker、不创建未知 capture；结果附带由 5.5 接通的完整原始资源链接。
- 生命周期为 `failed/aborted` 的不可变部分文件只能通过 status 报告范围；overview 明确拒绝，不能伪装为完整结果。

## 主要证据

- `crates/jlink-capture/tests/t_p4_overview.rs`
  - 两个 CRC 块拼接后得到 3 条完整记录；两个顶层变量分别得到 3 个样本和 1 次变化。
  - 验证按 ID/key 得到同一快照、未知 key 不创建目录或 capture、路径只在字典登记、空质量事件省略，以及打开后文件被改动时查询重新校验并拒绝。
- `crates/jlink-mcp/tests/t_p4_overview.rs`
  - 通过 stdio MCP 分别按 ID/key 查询相同 overview，并由严格 action 结果 Schema 校验。
  - 验证 `from_us/to_us`、顶层导航计数和 `application/vnd.jlink-mcp.capture.v1+binary` 资源链接。
  - 未知 key 返回稳定 `VALUE_INVALID`；未知 view 在读取 Capture Store 前由输入 Schema 拒绝。
- `crates/jlink-mcp/src/runtime.rs` 的状态投影回归验证活动 capture 返回 `complete_records`、已持久化半开区间和独立完整性质量。

## 开发循环测试

- `cargo test -p jlink-capture --test t_p4_overview`：PASS。
- `cargo test -p jlink-mcp --test t_p4_overview`：PASS。
- `cargo test -p jlink-mcp --lib hss_state_tests`：PASS。
- `cargo test -p jlink-mcp --test t_p1_mcp t_p1_mcp_resource_link_template_and_read_share_one_contract`：旧占位夹具在严格 Schema 下失败；按正式 overview 结构修正后 PASS。
- `cargo test -p jlink-mcp --test t_p1_mcp t_p1_mcp_runtime_keeps_remaining_future_actions_unavailable`：PASS，剩余未实现查询视图仍返回不可用。

## 原子提交门禁

- `cargo fmt --all -- --check`：PASS。
- `cargo clippy -p jlink-capture -p jlink-mcp --all-targets -- -D warnings`：PASS。
- `cargo test -p jlink-capture`：PASS，3 个既有 Store 单元测试、T-P4-OVERVIEW 及文档测试通过。
- `cargo test -p jlink-mcp --test t_p4_overview`：PASS。
- `cargo test -p jlink-mcp --lib hss_state_tests`：PASS，3 个状态投影测试通过。
- `cargo test -p jlink-mcp --test t_p1_mcp`：PASS，8 个 MCP 目录、Schema、资源链接和错误回归通过。
- 本任务修改 OpenSpec 与公共接口说明，因此额外执行 OpenSpec strict；未修改 Cargo manifest、crate 数量、依赖方向或 LikeC4，不触发相应检查。

门禁首次发现旧 P1 资源夹具仍使用占位 overview 字段，以及联合输出 Schema 根节点未显式关闭额外属性；均取得最短复现后按正式 5.1 合同修正并由原测试回归。Clippy 仅发现函数长度和切片传参的机械问题，拆分后最终门禁一次通过；未放宽 lint 或 Schema。

## 证据失效条件

- Capture Store 文件格式、块校验、终态清单、HSS frame layout 或变量 sample offset 变化。
- overview 的时间边界、变化计数、事件计数、字典或质量省略规则变化。
- MCP HSS 输入/输出 Schema、资源链接合同、probe identity 到 Capture Store 路径映射变化。
