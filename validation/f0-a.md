# F0-A HSS / 写入 / 吞吐可行性证据

## 裁决

`PASS_WITH_LIMIT`

J-Link 6.98a 在实际 S32K144、SWD 4000 kHz 上完成 10 个 32-bit block、1 kHz、300 秒 HSS，并在采集期间完成三次串行 RAM 写入及读回；Stop、文件校验、RAM 恢复和目标继续运行均成功。限制为源时间戳分辨率 1 ms，且 DLL 没有提供独立 overflow/sequence counter，因此数据质量只能按 `confirmed / suspected / unknown` 证据分级，不能宣称“零丢样”或“无溢出”。这些限制已写入公共规格，不缩减 1–300 秒、1–10 个顶层选择项或 1–1000 Hz 请求范围。

J-Link 8.38 和 9.70 仅作为兼容性副线。它们的微秒模式或长时 HSS 失败不阻塞 6.98a 主线，也不得被声明为已支持的长时 HSS 基线。

## 冻结身份

| 对象 | 冻结值 |
|---|---|
| 主线 DLL | J-Link 6.98a；`C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll`；SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| 副线 DLL | J-Link 8.38；SHA-256 `56C2C354475F48B9532D7EF03842D6D073E74D38FBAD212711EFAF773B1D5AD3` |
| 副线 DLL | J-Link 9.70；SHA-256 `1899A536ED388CF931F82E49C4F8DC25642F581AE290444DF1B192720A726D29` |
| 探针 | 序列号 `260106173`；硬件 `V10.10`；普通版、非 PRO；报告固件 `J-Link V10 compiled Jun 27 2028 10:57:29` |
| 目标 | S32K144；Cortex-M4 r0p1；SWD-DP ID `0x2BA01477`；SWD 4000 kHz |
| IAR ELF | `T26_DCU_APP_NXP.out`；4,962,648 bytes；SHA-256 `3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF` |
| 上板前完整 Flash 备份 | 524,288 bytes；SHA-256 `33F0CB7F9FFCC42187145542E470E4457466D7F690D6ED0E7EED361D0B4EF194` |
| 实验二进制 | `f0a-hss.exe`；448,000 bytes；SHA-256 `DEA2BD6F114AE791617ADCE919032929F737DA5DB79FF6F4F46A8583D283F1D7` |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`；`cargo 1.98.0 (797e8a9bc 2026-08-05)` |

探针报告的固件编译日期晚于测试日期，且 8.38/9.70 弹出探针识别警告；这两项作为兼容性异常保留，不据此判定探针真伪或断言固件损坏。官方安装包 release notes 也存在新版误报修复记录，因此身份警告与 HSS 行为分别记录。

## 冻结的最小 ABI 候选

- `JLINK_HSS_GetCaps(*mut HssCaps) -> i32`
- `JLINK_HSS_Start(*mut HssBlock, i32 block_count, i32 period_us, i32 flags) -> i32`
- `JLINK_HSS_Read(*mut void, u32 buffer_bytes) -> i32`
- `JLINK_HSS_Stop() -> i32`
- `HssBlock` 为 4 个 little-endian `u32`：`address`、`byte_count`、`flags`、`reserved`，共 16 bytes。
- `HssCaps` 为 `max_blocks`、`max_frequency_hz`、`flags` 和 5 个 reserved `u32`，共 32 bytes。
- 三版实测能力均为 `max_blocks=10`、`max_frequency_hz=1000`、`flags=2`、reserved 全零。
- 6.98a 主线使用 `flags=0`。每帧记录为一个 32-bit 源时间戳，随后按 block 顺序拼接原始字节；10 个 32-bit block 的记录步长为 44 bytes。
- 主线源时间频率为 1000 Hz、分辨率 1000 us。公共时间使用 `timestamp_us = timestamp_ms × 1000`；单位归一化不提升源分辨率。

预检证据为 `validation/evidence/f0-a/rust-preflight-v698a-mainline.json` 和 `validation/evidence/f0-a/v698a-mainline-getcaps.json`。19 个实验所需导出均存在。

## 主线真机结果

### 300 秒上界

证据：

- `validation/evidence/f0-a/v698a-10block-1khz-300s.json`
- `validation/evidence/f0-a/v698a-10block-1khz-300s.jmcf`
- `validation/evidence/f0-a/v698a-10block-1khz-300s-verify.json`

结果：

| 指标 | 结果 |
|---|---:|
| 请求 | 10 blocks、1000 Hz、300 s |
| 实际采集时长 | 300.000429 s |
| 样本记录 | 300,004；期望 300,000 |
| 实际频率 | 1000.0 Hz |
| short / malformed / regression | 0 / 0 / 0 |
| timestamp collision / gap slots | 15 / 15 |
| 交错写入 | 75 s、150 s、225 s 共 3 次，全部写入和读回成功 |
| Stop / 尾排空 | Stop 返回 0；尾排空 61.502 ms |
| 采集文件 | 13,839,928 bytes；26,655 个 CRC 块；SHA-256 `86323CFD51D8468EF561A43F703AD3512025189256A6C7F8294B2FFE677DD9B5` |
| 最终状态 | 原值恢复；CPU running |

1 ms 时间戳在 1 kHz 边界会产生碰撞和对应间隔。碰撞数与 gap slots 相等且样本计数达到门槛，但由于没有独立 overflow/sequence counter，丢样证据仍记为 `unknown`，而不是“确认无丢样”。

### 时间单位归一化 smoke

证据：`validation/evidence/f0-a/v698a-ms-normalization-10block-1khz-5s-v2.json`。

- 10 blocks、1000 Hz、5 s，得到 5,001 条记录和 3 次成功写入。
- `timestampUnit=ms`、`timestampFrequencyHz=1000`、`sourceTimestampResolutionUs=1000`、`normalizedTimestampUnit=us`。
- 原始首尾时间戳 `0..5000 ms` 精确归一化为 `0..5000000 us`。
- Capture Store CRC 正常，原值恢复，CPU running。
- 纯规则测试同时覆盖 `12,345 ms -> 12,345,000 us` 和微秒源单位恒等转换。

## Capture Store 候选

候选格式版本标识为 `JMCF0A01`：固定文件头保存记录步长、首地址、请求频率、HSS flags 和源时间频率；每个 `BLK1` 追加块保存 payload 长度、CRC32、主机单调时钟 `host_elapsed_us` 和 phase。完成文件使用 SHA-256 身份；截断尾部只恢复完整 CRC 块。

300 秒等价基准 `10 × 32-bit + timestamp`：

| 指标 | 结果 |
|---|---:|
| 样本数 | 300,000 |
| 文件大小 | 13,204,880 bytes |
| 写入耗时 | 58 ms |
| 吞吐 | 216.36 MiB/s |
| 完整文件 | 202 个有效块；CRC 正常；SHA-256 `9C3373563EC3390297254DF42064BD5B39227E4DA8E1672A28A04894E3FDDD0B` |
| 截断恢复 | `truncatedTail=true`；恢复前 201 个有效块；CRC 正常 |

证据为 `validation/evidence/f0-a/store-mainline-300s-10x32-v2.json`、对应 `.jmcf` 和 `.partial`。该结果远低于默认 512 MiB 单次采集上限；它只证明当前未压缩候选编码的空间和吞吐，不替代运行时磁盘预检。

## 副线结果

| DLL | 测试 | 结果 |
|---|---|---|
| 8.38 | 10 blocks、1 kHz、100 s、微秒模式 | 25 s 首次写入成功；约 50 s 第二次写入后的普通 RAM 读回失败；HSS 文件保留 |
| 9.70 | 10 blocks、500 Hz、100 s、微秒模式 | 相同地在约 50 s 失去普通 RAM 访问；排除 10×1 kHz 满负载解释 |
| 9.70 | 1 block、1 kHz、100 s、微秒模式 | 相同地在约 50 s 失去普通 RAM 访问；排除 block 数量和总吞吐解释 |
| 9.70 | 10 blocks、1 kHz、80 s、无写入 | HSS 持续到 Stop，Stop 后首次普通内存读取失败 |
| 6.98a | 微秒 flag smoke | 进程访问异常；该模式不进入主线 |

8.38/9.70 的失败集中在新版 DLL 与当前探针固件组合的长 HSS 会话后普通内存访问路径。没有证据证明普通版探针的 1 kHz 能力上限是原因；也没有证据足以把问题唯一归因于固件损坏。

## 复用与失效条件

本证据仅在以下身份和语义不变时可由 P2/P3 复用：主线 DLL 路径/版本/SHA-256、探针序列号/硬件/固件字符串、S32K144 目标、SWD 4000 kHz、目标固件/ELF SHA-256、HSS ABI/帧假设、时间归一化规则和 Capture Store 版本。

DLL 替换、探针固件升级或降级、目标固件变化、连接接口/速度变化、ABI/解析器变化、源时间语义变化或 Capture Store 版本变化都会使对应证据失效。8.38/9.70 的副线结果不能替代 6.98a 主线证据。
