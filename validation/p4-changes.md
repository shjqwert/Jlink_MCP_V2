# P4 5.2 Changes 验证

## 结论

PASS。T-P4-CHANGES 覆盖 HSSQ-004 的不可变完成快照查询；未连接探针、未修改目标 SVN 工程，也未使 4.8 真机证据失效。

## 实现边界

- `changes` 重新校验并读取不可变完成文件，按相邻完整源记录生成精确变化；只声明 `after_us/observed_by_us` 观测区间，不伪造变化时刻。
- 顶层标量使用 `sN`，结构和定长数组叶使用 `sN.ordinal`；查询接受稳定短 ID 或精确叶路径，结果路径只通过首次所需 `dictionary` 登记。
- 固定规则为 `abs_delta_gte`、`outside`、`equals`、`crosses(up/down/either)`；规则 ID 归一排序，重复 ID、未知运算符、脚本式路径、递归通配符和未匹配路径均拒绝。
- 规则路径只允许精确成员、非负数组索引和 `[*]`。定长数组及已收敛 slice 按 DWARF 顺序展开；不跟随指针，不推断 union 活动成员。
- 省略查询规则时复用启动时规则；显式规则集合替换启动规则，显式空集合用于仅查询精确变化。二者使用同一归一化和求值路径，查询不修改采集文件。
- 时间过滤按 `observed_by_us` 使用半开区间；源时间戳回退、非完整尾、样本计数不一致和不安全数值比较返回稳定错误。
- `limit` 对精确变化和阈值匹配的确定性合并顺序统一限界；5.4 接通稳定游标前仅报告 `truncated`，不生成临时游标。

## 主要证据

- `crates/jlink-capture/tests/t_p4_changes.rs`
  - 两元素结构数组的三条记录产生两个精确变化，并验证启动上穿规则与同一查询规则得到相同结果。
  - 覆盖四种固定规则、`channels[*].temperature` 的稳定路径顺序、精确叶选择、统一限界和脚本式路径拒绝。
- `crates/jlink-mcp/tests/t_p4_changes.rs`
  - 通过 stdio MCP 按 key/ID 查询不可变 capture，严格结果 Schema 分别返回 `changes` 与 `matches`。
  - 验证查询规则替换启动规则；未知规则 kind 在读取 Store 前由输入 Schema 拒绝，未匹配路径返回 `VALUE_INVALID`。
- `crates/jlink-domain/tests/t_p3_start.rs`
  - 既有启动规划回归继续验证规则进入请求指纹并按稳定 ID 归一化。

## 开发循环测试

- `cargo test -p jlink-domain --test t_p3_start`：PASS。
- `cargo test -p jlink-capture --test t_p4_changes`：PASS，3 个测试。
- `cargo test -p jlink-mcp --test t_p4_changes`：PASS，2 个测试。
- `cargo test -p jlink-mcp --test t_p4_overview`：PASS，changes 路由接入后 overview 路由回归通过。

## 原子提交门禁

- `cargo fmt --all -- --check`：PASS。
- `cargo clippy -p jlink-domain -p jlink-capture -p jlink-mcp --all-targets -- -D warnings`：PASS。
- `cargo test -p jlink-domain --test t_p3_start`：PASS，5 个启动规划和规则指纹回归。
- `cargo test -p jlink-capture --test t_p4_changes`：PASS，3 个 Capture 查询主要测试。
- `cargo test -p jlink-mcp --test t_p4_changes`：PASS，2 个 stdio MCP 主要测试。
- `cargo test -p jlink-mcp --test t_p1_mcp`：PASS，8 个目录、Schema、资源和未来动作回归。
- `openspec validate define-jlink-mcp-v1 --strict`：PASS；`git diff --check`：PASS。
- 本任务未修改 Cargo manifest、依赖方向或 LikeC4，不触发相应检查；未运行 workspace 全量门禁。

门禁首次发现旧 P1 回归仍把 `changes` 作为未来未实现动作；最短复现确认正式 5.2 路由已替代该占位后，将回归目标改为仍待 5.3 实现的 `window`，单独重跑后通过。Clippy 仅发现文档反引号和冗余闭包的机械问题，修正后未放宽 lint。

## 证据失效条件

- Capture Store 格式、HSS frame layout、DWARF `AccessLayout`、TypedValue 解码或时间单位变化。
- changes 的叶 ID、字典、相邻观测区间、规则语义、路径语法、排序、限界或规则覆盖顺序变化。
- MCP HSS changes 输入/输出 Schema、不可变 capture 查找或错误映射变化。
