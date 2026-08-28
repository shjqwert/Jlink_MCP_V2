# P4 5.4 Timeline 验证

## 结论

PASS。T-P4-TIMELINE 覆盖 HSSQ-007、HSSQ-008、HSSQ-009 的统一事件关系、增量字典和不可变快照游标；未连接探针、未修改目标 SVN 工程，也未改变 J-Link ABI、HSS payload 或既有 capture 文件格式。

## 实现边界

- changes 页将精确变化区间、持久化设备调用、质量事件和恢复事实放入同一稳定事件语义；设备调用保留 host `start/end`，变化保留 sample `after_us/observed_by_us`。
- 跨时钟关系使用持久化 mapping uncertainty 的保守包络：完整包络早于/晚于变化区间时才输出 `before/after`；中心区间实际重叠时输出 `overlaps`；仅误差导致顺序不能确定或缺少误差时输出 `indeterminate`。所有关系明确为非因果事实。
- 事件按 host 起点、终点、类别和持久化顺序稳定分配 `eN`；每页只返回映射包络与本页 sample 范围相交的设备、质量和恢复事件，window 的 `quality` 同样按页过滤。
- 不透明游标绑定 cursor Schema v1、实际 `capture_id`、raw SHA-256、规范化原查询、视图排序、下一确定性位置和已登记 series ID。
- 首次页返回当页所需字典；续页仅返回新出现的 series 增量。changes、window raw/aggregates 和 around_event 共用同一续页入口；around_event 的可选 `series` 进入规范查询绑定，调用方不能通过游标请求修改选择、范围、规则、mode、points、limit 或排序。
- 游标有效期固定为绑定的不可变 capture 内容仍存在。格式、摘要或身份校验失败为 `CURSOR_INVALID`；资源不存在或内容身份改变为 `CURSOR_EXPIRED`，不重试、不回退、不从新查询位置开始。
- `truncated=true` 的 MCP 页始终生成 `next_cursor`；末页 `truncated=false` 并省略游标。Capture 内部投影仍只返回确定性截断事实，游标由 MCP 查询边界统一封装。

## 主要证据

- `crates/jlink-mcp/tests/t_p4_timeline.rs`
  - 两个变量在相邻记录依次变化，limit=1 的 changes 首、次页分别返回 `s0` 完整字典和 `s1` 增量字典，不重复既有路径。
  - 第一页写入事件与变化中心区间 `overlaps`；around_event 续页证明写入误差包络完整早于下一变化时关系为 `before`。
  - 独立 Capture fixture 的较大误差回归证明仅因误差包络无法判断时输出 `indeterminate`。
  - raw window limit=2 精确续接第三条重复保留行，后页字典为空；around_event limit=1 续接第二条附近变化，并验证 `series=[s1]` 只返回 `s1` 字典、变化和关系。
  - 篡改摘要、错配 capture 身份分别返回 `CURSOR_INVALID`；删除测试临时目录中的绑定 capture 后返回 `CURSOR_EXPIRED`，没有重启查询 fallback。
  - 第二 changes 页只返回落入其 sample 范围的质量事件；window 首、次页分别返回相交质量和空质量，证明页级过滤。
- `crates/jlink-capture/tests/t_p4_window.rs`
  - around_event 对同一事件的两条附近变化分别输出 `overlaps` 与 `indeterminate`，并保留原始区间及映射误差。
- `crates/jlink-capture/src/cursor.rs`
  - 严格 payload、规范化字段、版本前缀、小写十六进制和 SHA-256 domain separation 共同检测损坏；snapshot 校验独立检查 capture ID 与 raw SHA。

## 开发循环测试

- `cargo test -p jlink-mcp --test t_p4_timeline`：PASS，4 个主要测试。
- `cargo test -p jlink-capture --test t_p4_changes --test t_p4_window`：PASS，6 个变化、窗口和关系回归。
- `cargo test -p jlink-mcp --test t_p4_changes --test t_p4_window --test t_p1_mcp`：PASS，12 个 changes/window/around_event、严格 Schema、资源和 5.5 占位回归。

## 原子提交门禁

- `cargo fmt --all -- --check`：PASS。
- `cargo clippy -p jlink-domain -p jlink-capture -p jlink-mcp --all-targets -- -D warnings`：PASS；只覆盖受影响 crate 及直接下游，未运行 workspace 全量门禁。
- `openspec validate define-jlink-mcp-v1 --strict`：PASS。
- `git diff --check`：PASS；仅报告 Git 的既有 LF/CRLF 工作副本提示，无空白错误。
- 本任务修改 OpenSpec 与公共接口说明，因此执行 OpenSpec strict；未修改 Cargo manifest、依赖方向或 LikeC4，不触发相应检查。
- Clippy 仅触发窗口查询解析拆分、采样范围 `reduce` 和测试夹具配置拆分三项机械修正；修正后上述受影响测试全部通过。
- 本节只补录已完成的结果，不改变代码、测试输入或验证合同，因此不重复运行已通过且指纹有效的测试。

## 证据失效条件

- Cursor payload、摘要、版本、snapshot 身份、规范查询字段、排序、位置或有效期策略变化。
- 事件短 ID、页范围、关系包络、映射误差、增量字典或页级质量过滤变化。
- MCP changes/window/around_event 输出 Schema、cursor 输入或 `CURSOR_INVALID/CURSOR_EXPIRED` 映射变化。
