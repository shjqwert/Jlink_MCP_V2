# P4 5.5 原始 Capture Resource 证据

## 结论

- T-P4-RESOURCE 覆盖 HSSQ-010、HSSQ-011：`resources/read` 在无活动 Worker/目标连接时读取不可变完成文件，固定返回 `application/vnd.jlink-mcp.capture.v1+binary` 和完整文件的标准 Base64。
- 返回内容是 Capture Store 的原始 `.capture` 数据，不是图片、文本、抽取后的 payload 或服务端生成曲线；数值曲线继续由 window 原始/显式聚合视图提供。
- 当前工程 Store 是新采集和离线读取的首选位置；本文件的旧用户 Store 夹具保留用于证明显式 capture ID 的只读兼容回退，不触发迁移、覆盖或删除。
- 读取前通过同一文件句柄重新验证 V1 magic/版本、自描述头 CRC、每块 CRC、终态 CRC、原始 payload SHA-256、采集身份和终态清单；失败返回资源错误，不返回部分数据或 fallback。

## 格式与依赖边界

- 完整资源保留固定二进制版本头、目标身份、HSS 启动计划、ELF/Flash 段身份、变量访问计划、源时间与主机时间证据、质量/写入/恢复终态以及全部校验字段。
- V1 的 `s0..sN` 由资源内 `plan.variables` 的稳定顶层顺序定义；路径、布局、数组、位域、union 和编码均由内嵌 `AccessPlan` 独立恢复，不依赖当前 ELF 或活动会话。
- `data-encoding` 只在 `jlink-mcp` 协议边界将已验证字节编码为 MCP `blob`；未增加 crate、状态所有者或反向依赖。

## 主要证据

- `crates/jlink-mcp/tests/t_p4_resource.rs`
  - 两次读取同一规范 URI 得到相同 blob，解码字节与原子发布的 `.capture` 文件逐字节一致，且可由 Capture Store 重新验证。
  - Runtime 从未连接 Worker 或目标，仍可读取并恢复 capture ID、S32K144/SWD/4000 kHz 目标身份、ELF SHA-256、完整样本数和原始 SHA-256。
  - 非规范 URI 与不存在 capture 均返回 `VALUE_INVALID`，不产生内容或替代路径。
  - 在隔离临时文件中翻转 payload 字节后，块 CRC 失败并返回 `FRAME_INVALID`，不返回损坏资源。
  - 内容项只有 `uri/mimeType/blob`，没有 text 或 image MIME。
- T-P1-MCP 继续复核模板、链接和读取三处 URI/MIME 一致性，不承担 HSSQ-010、HSSQ-011 的主要断言。

## FT-017 客户端交付边界

- 服务端不可变 `.capture` 文件为 201,208 bytes，头为 `JMCPV101`，SHA-256 为 `A57C54A9E44FEC68E267FD9C010713BACA3F6B6AB8FD52D231307A9AB3CB8060`；独立资源路径得到完整文件，标准 Base64 长度应为 268,280 字符。
- 当前 Codex 通用资源链路只向 Agent 交付 47,798 个 Base64 字符，且长度不能被 4 整除；这不是可解析的完整标准 Base64，也没有显式截断错误。
- 因此 FT-017 标记为 Codex 客户端 external-blocked。服务端继续保留标准完整 `resources/read`、既有 URI 与二进制 MIME；不新增本地路径字段、不删除资源接口、不迁移或缩减规范文件。

## P4 阶段纵向 Smoke

- 同一不可变 capture 依次贯通 overview 资源链接、changes 首/续页游标、window raw、around_event 和 `resources/read`。
- Smoke 只确认跨组件路由和同一快照绑定，不复制 5.1-5.4 的主要语义断言。
- `cargo test -p jlink-mcp --test t_p4_resource`：PASS，4 个资源主要测试/阶段 smoke。

## 开发循环与直接回归

- `cargo test -p jlink-capture store::tests`：PASS，3 个原子发布、部分恢复和 CRC 损坏回归。
- `cargo test -p jlink-mcp --test t_p1_mcp`：PASS，8 个 MCP 目录、严格 Schema、资源合同和错误回归。
- 新 smoke 首次最短复现发现测试请求漏填既有必填字段 `action: query`；只修正测试夹具构造，完整 T-P4-RESOURCE 随后通过，生产合同未变化。

## 5.5 阶段检查点门禁

- `cargo fmt --all -- --check`：PASS。
- `cargo clippy --workspace --all-targets -- -D warnings`：PASS。首次门禁只发现新 smoke helper 不必要的按值后克隆；改为直接消费 JSON 对象并重跑 T-P4-RESOURCE 后，完整 Clippy 通过，未放宽 lint。
- `cargo test --workspace --all-targets`：PASS；全部非硬件目标通过。5 个明确依赖冻结 DLL/探针/目标的真机测试保持 `ignored`，继续复用 `validation/p3-stage.md` 的 4.8 真机证据，没有用 Mock 代替硬件结论。
- `scripts/check-dependencies.ps1`：PASS；仍为四个 crate，依赖方向不变。
- `openspec validate define-jlink-mcp-v1 --strict`：PASS。
- `git diff --check`：PASS；仅报告 Git 的既有 LF/CRLF 工作副本提示，无空白错误。
- P4 纵向 smoke 已包含在 `cargo test -p jlink-mcp --test t_p4_resource` 和 workspace 全量测试中并通过，不额外重复执行。
- 任务/矩阵范围检查未发现 2.1-5.5 未勾选项、TBD、TODO 或 HSSQ-010/011 占位证据；该检查点当时保留 5.6、5.7 待办，5.6 后续结果见 `validation/p4-client.md`。

阶段末只执行一次完整 SVN 状态检查。状态仍由已登记的 IAR 输出/中间文件、计划内 `AppUserDesc.c` 和既有用户修改组成，没有 P4 新增目标工程差异；未执行 `svn commit`。冻结身份全部一致：

- `AppPwrMode.h`：`E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085`。
- `T26_DCU_APP_NXP.dep`：`4FDA4431B3502EBDB1B0313BF58B21995A2B962C9C0BA853DF42F3988B4A6F85`。
- `AppUserDesc.c` fixture：`1133B85709AB5ED3509ED58433ED4132E4D0869724140F8D3F560F7BA3B709E4`。
- IAR OUT：`F8ADB9A2B9BBFD26B469C66F2478EE6E22735302706B83509B2D4F2AE7F7738D`。

本节只补录已经完成的结果，不改变代码、构建、测试输入或验证合同，因此不重复运行已通过且指纹有效的测试。

## 证据失效条件

- Capture Store magic/版本、头/块/终态编码、CRC、原始 SHA-256 或原子发布规则变化。
- `HssStartPlan`/`AccessPlan`/目标/质量终态序列化、`sN` 顺序规则或资源 URI/MIME 变化。
- MCP `resources/read` 内容形状、Base64 实现、Capture Store 定位或断开态读取路径变化。
- 5.6 Windows Codex 端到端验收见 `validation/p4-client.md`；5.7 发布门禁仍为待办。
