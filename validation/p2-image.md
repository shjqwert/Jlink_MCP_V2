# P2 镜像与固件身份验证

## 结论

OpenSpec 任务 3.1 的主要测试 T-P2-IMG 通过。生产领域层按内容识别 ELF，并支持 Intel HEX、Motorola S-record 和显式基地址 BIN；`.axf/.out` 只要内容为 ELF 即按 ELF 处理，MAP 不进入符号路线。`jlink_program.flash/verify` 的公共 Schema 已增加可选十六进制 `base_address`，领域校验只允许 BIN 使用该字段，并在任何探针访问前拒绝缺失或多余字段。

固件身份计划记录完整 ELF SHA-256 与归一化加载段指纹。目标读取缺失、重复、畸形或计划本身无效时返回 `FIRMWARE_IDENTITY_UNKNOWN`；完整读取已确认摘要不同才返回 `FIRMWARE_ELF_MISMATCH`，不存在零区间真空成功。

## 主要测试与回归

| 检查 | 结果 | 覆盖 |
|---|---|---|
| `cargo test -p jlink-domain --test t_p2_img` | PASS，3/3 | ELF/AXF/OUT、HEX、SREC、BIN、MAP 拒绝、校验和、S5/S6 计数与终点、身份 match/unknown/mismatch |
| `cargo test -p jlink-domain --test t_p1_dom` | PASS，6/6 | `VALUE_INVALID`、`FIRMWARE_IDENTITY_UNKNOWN`、`FIRMWARE_ELF_MISMATCH` 稳定拼写 |
| `cargo test -p jlink-mcp --test t_p1_mcp` | PASS，6/6 | 六工具闭集、`flash/verify.base_address` 严格 Schema、错误映射 |
| `scripts/check-workspace.ps1` | PASS | 格式、workspace `clippy -D warnings`、workspace 测试、四 crate 依赖方向 |
| `openspec validate define-jlink-mcp-v1 --strict` | PASS | 规划与规格严格校验 |

Sol Advisor 只读对抗审查先后发现空身份计划的真空成功、S5/S6 声明计数未核对、计数记录后仍可追加数据三条边界。主窗口分别增加精确回归并把根因收敛为身份计划不变量验证及“S5/S6 关闭数据记录序列”；上述测试和门禁均在最终修正后重新通过。

## 真实 IAR OUT 证据

| 项目 | 指纹 |
|---|---|
| 输入 | `D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP\Appl\Output\Exe\T26_DCU_APP_NXP.out` |
| ELF SHA-256 | `3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF` |
| T-P2-IMG 示例二进制 | SHA-256 `8B51D18C13AB74AD123DCE6D9DD36CFD01411D606AC2E5815109727C567593B8` |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`，`x86_64-pc-windows-msvc` |

真实 OUT 由最终实现解析为三个归一化加载区间：

| 地址 | 长度 | 段 SHA-256 |
|---|---:|---|
| `0x00000000` | 784 | `710887A076F668B032841B4385C9BA2DD90FCE823A2F811F64D366C66077CC51` |
| `0x00000400` | 16 | `2D8DDDA6A9B0E016063FC4E8447CDB7E183DB72878EE08F7F22C5B61D77150EE` |
| `0x00010010` | 205808 | `833D233203737BDC37D6C9A8241699A0835F61886D4942205DC4126B771B9DF5` |

当前工程直接使用该 OUT 时不需要 `base_address`。只有从 OUT 导出裸 BIN 时才使用导出选择的起始地址；完整导出并保留最低加载区间时为 `0x00000000`，局部导出必须使用该局部导出范围的实际起点。

## SVN 现场

本任务只读访问目标工程，未构建、烧录、连接探针或修改 SVN 文件。任务前后 `svn status -q` 均只有两处既有用户修改：

| 受保护文件 | SHA-256 |
|---|---|
| `AppPwrMode.h` | `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085` |
| `T26_DCU_APP_NXP.dep` | `B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620` |

本证据只覆盖 ART-001、ART-002 的纯解析、公共合同和真实 OUT 身份计划。目标 Flash 的实际只读身份比较将在 3.4–3.7 接通 Worker 设备执行后形成硬件证据；本任务不得据此声明已完成烧录或真机固件匹配。
