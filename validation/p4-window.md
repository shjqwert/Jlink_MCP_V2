# P4 5.3 Window 验证

## 结论

PASS。T-P4-WINDOW 覆盖 HSSQ-005、HSSQ-006 的不可变完成快照查询；未连接探针、未修改目标 SVN 工程，也未改变 J-Link ABI 或原始 HSS payload。稳定续页游标仍由 5.4 完成。

## 实现边界

- `window raw` 按持久化记录顺序返回 sample 时钟、严格半开范围、矩形 `time_us/values` 和完整 TypedValue；重复值不去重、不降采样。
- `transitions` 仅在至少一个所选叶值变化时返回该完整观测行；范围内首行与其前一条源记录比较，避免边界变化漏报。
- `min_max`、`first_last` 只有显式选择时启用；请求范围等分为 1..1000 个固定桶，只返回非空桶。`min_max` 拒绝非数值叶，并使用无静默精度损失的 TypedValue 数值比较。
- `series` 必须是 1..N 个唯一稳定短 ID 或精确叶路径；`limit` 限制行或非空桶并输出 `truncated`。5.4 接入与不可变快照绑定的 `next_cursor`，本任务不生成临时游标。
- `around_event` 返回事件、sample 时钟复用边界、附近精确变化与重叠质量事实，不复制原始波形；可选 `series` 与 `changes/window` 共用短 ID/精确路径解析，省略时保持全序列，完整原始邻域必须再次显式请求 `window raw`。
- 写入事件按持久化 host 单调区间排序并分配稳定 `eN`；新 capture 区分 `memory_write/variable_write`，旧清单缺失新增字段时向后兼容为 `target_write`，不猜测具体写入类型。
- host→sample 边界只使用已持久化的 start-call 映射误差并裁剪到采集范围；缺少映射误差、sample 范围或事件 ID 时返回稳定错误。

## 主要证据

- `crates/jlink-capture/tests/t_p4_window.rs`
  - 四条样本 `[8,8,12,10]` 验证 raw 保留重复值、transitions 只返回两条变化观测、限界不改写已有行。
  - 两个固定桶分别验证 `min_max` 的 `[8,8]/[10,12]` 与 `first_last` 的 `[8,8]/[12,10]`，证明聚合语义只由显式 mode 决定。
  - 写入事件邻域返回可复用 sample 边界、两条精确变化和重叠质量证据；`series=[s0]` 只保留 `s0` 字典、变化和关系；同一边界再次执行 raw 得到完整原始行。
  - 删除序列化写入类别后仍能读取旧清单并只分类为 `target_write`。
- `crates/jlink-mcp/tests/t_p4_window.rs`
  - 通过 stdio MCP 按 key/ID 查询 raw、min_max 和 around_event，并由各 action 的独立严格结果 Schema 校验；目录回归同时验证 around_event 接受 `series`。
  - 缺少聚合 `points` 在读取 Store 前由输入 Schema 拒绝；空时间范围返回 `VALUE_INVALID`。
- `crates/jlink-worker/src/hss.rs` 回归验证写入类别与原有排空、写入结果、下一次 drain 证据共同持久化。

## 开发循环测试

- `cargo test -p jlink-capture --test t_p4_window`：PASS，3 个测试。
- `cargo test -p jlink-mcp --test t_p4_window`：PASS，2 个测试。
- `cargo test -p jlink-worker hss::tests`：PASS，7 个 Worker HSS 回归。
- `cargo test -p jlink-mcp --test t_p1_mcp t_p1_mcp_runtime_keeps_raw_resource_unavailable_until_5_5`：PASS，5.4 接通 cursor 后只保留 5.5 raw resource 占位。

## 原子提交门禁

- `cargo fmt --all -- --check`：PASS。
- `cargo clippy -p jlink-domain -p jlink-capture -p jlink-worker -p jlink-mcp --all-targets -- -D warnings`：PASS。
- `cargo test -p jlink-capture --test t_p4_window`：PASS，3 个 window/around_event 主要测试。
- `cargo test -p jlink-capture --test t_p4_changes`：PASS，3 个共享 leaf 投影回归。
- `cargo test -p jlink-worker hss::tests`：PASS，7 个 HSS 排空、恢复、质量和写入回归；新增写入类别断言单独重跑后 PASS。
- `cargo test -p jlink-mcp --test t_p4_window`：PASS，2 个 stdio MCP 主要测试。
- `cargo test -p jlink-mcp --test t_p1_mcp`：PASS，8 个目录、Schema、资源和未来动作回归。
- `openspec validate define-jlink-mcp-v1 --strict`：PASS；`git diff --check`：PASS。
- 本任务未修改 Cargo manifest、依赖方向或 LikeC4，不触发相应检查；未运行 workspace 全量门禁。

实现审查发现既有写入清单无法区分公共事件示例中的内存与变量写入。修订未改变公共 Schema、J-Link ABI 或原始 payload：新清单增加向后兼容的写入类别，Worker 在既有串行入口按原始 DebugRequest 记录类别，旧清单缺失字段时默认 `target_write`；对应 Capture 和 Worker 回归通过。

## 证据失效条件

- Capture Store 清单、HSS frame layout、TypedValue 解码、leaf series 展开、sample 时间单位或 host→sample 映射变化。
- window 范围、raw 重复值、transition、桶边界、数值比较、限界或 around_event 边界规则变化。
- MCP window/around_event 输入输出 Schema、事件类别或不可变 capture 查找变化。
