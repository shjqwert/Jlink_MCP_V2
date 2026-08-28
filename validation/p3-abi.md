# P3 HSS 最小动态 ABI 证据

## 裁决

P3-4.1 为 `PASS`。生产 gateway 只声明 F0-A 冻结的 `JLINK_HSS_GetCaps/Start/Read/Stop`，精确固定 Windows x64 函数签名、结构体布局、标志位和 little-endian 原始帧解释。四个导出作为一个可选 HSS 能力组；缺失项不会阻断普通调试 DLL 加载，但必须在 HSS 能力诊断中逐项报告，后续 HSS 启动在首次目标动作前返回 `DLL_EXPORT_MISSING`。

## 冻结输入与合同

| 对象 | 冻结值 |
|---|---|
| J-Link DLL | `C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll`；6.98a；SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| GetCaps | `JLINK_HSS_GetCaps(*mut HssCaps) -> i32` |
| Start | `JLINK_HSS_Start(*mut HssBlock, i32 block_count, i32 period_us, i32 flags) -> i32` |
| Read | `JLINK_HSS_Read(*mut void, u32 buffer_bytes) -> i32` |
| Stop | `JLINK_HSS_Stop() -> i32` |
| `HssBlock` | `address/byte_count/flags/reserved` 四个连续 `u32`；16 字节；4 字节对齐 |
| `HssCaps` | `max_blocks/max_frequency_hz/flags/reserved[5]`；32 字节；4 字节对齐 |
| 6.98a 主线标志 | block flags `0`；Start flags `0`；实验性微秒标志 `1` 不受 V1 支持 |
| 原始帧 | little-endian `u32` 源时间戳加声明顺序的块载荷；10×32-bit 布局为 44 字节一帧 |
| 依据 | `validation/f0-a.md`、F0-A 原始 ABI/解析证据与 RUN-006 |

## 自动验证

| 命令 | 结果 | 覆盖 |
|---|---|---|
| `cargo test -p jlink-domain --test t_p3_abi` | `PASS`；2/2 | 6.98a 标志、10×32-bit 帧步长、little-endian 双帧解析、不完整尾部保留、无效/溢出布局拒绝 |
| `cargo test -p jlink-worker --lib t_p3_abi` | `PASS`；3/3，另有 1 个显式冻结 DLL 测试默认忽略 | 函数指针类型、结构体大小/对齐/字段偏移、完整和缺失导出集合；缺失 HSS 只使完整环境报告降级，不阻断普通调试连接，其他失败检查仍阻断 |
| 设置 `JLINK_MCP_T_P3_ABI_DLL` 后运行精确 ignored 测试 | `PASS`；1/1 | 冻结 6.98a DLL 可加载且四个 HSS 导出全部存在；不连接探针、不启动 HSS |

## 边界与失效条件

- 本证据只证明 ABI、导出存在性和纯帧解析，不声明已完成 HSS 启动、采集质量、停止、尾排空或真机支持；HSS `GetCaps` 目标能力仍是集成观察。
- F0-A 的 `PASS_WITH_LIMIT` 保持有效：主线只使用毫秒源时间戳，没有独立溢出或序列计数器，不得据此声明零丢样或无溢出。
- 更换 DLL 内容/版本、Windows 调用约定、冻结结构体或标志位、帧布局、导出集合或解析规则时，本证据失效并必须重跑 T-P3-ABI。
- 本任务没有连接探针、修改目标 SVN 工程或执行硬件副作用。源码基线为 `b2fe88275c80559143ec453285353e2f69dd49a3` 加本记录所在的 `[P3-4.1][开发]` 原子提交。
