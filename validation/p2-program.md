# P2 Flash 烧录、擦除与校验证据

## 结论

OpenSpec 任务 3.4 的主要测试 T-P2-PRG 通过。生产链路已接通 `jlink_program.flash/erase/verify`、严格 IPC、唯一 Worker gateway、动态器件 Flash 区域、默认/独立校验、显式 `after`、HSS 冲突和稳定错误。当前任务没有连接探针或修改目标 Flash；范围擦除、整片擦除、真实写入、读回及最终 CPU 状态仍必须由 3.7 的冻结硬件纵向测试证明，不得以本文件声明真机烧录已经通过。

## 已验证实现

- `jlink-mcp` 把请求或工程默认镜像解析为绝对路径；BIN 仍要求本次请求显式 `base_address`，其他自带地址格式拒绝该字段，`verify` 默认 `true`。
- Worker 在文件、DLL 或目标操作前首先检查活动 HSS 和连接身份；活动 HSS 立即返回 `OPERATION_CONFLICT`，不停止采集、不排队、不读取镜像。
- 冻结 DLL 的 `JLINKARM_DEVICE_GetIndex/GetInfo` 返回器件数据库区域；`jlink-domain` 对镜像所有段或擦除范围执行统一的非空、无溢出、单区域完整包含校验，失败为 `FLASH_RANGE_INVALID`，不会开始目标副作用。
- 烧录使用 `JLINKARM_BeginDownload(0) → JLINKARM_WriteMem → JLINKARM_EndDownload`；整片擦除使用 `JLINK_EraseChip`。范围擦除通过同一下载事务写入 `0xFF`，依赖器件 Flash 算法的 read-modify-write 保留区间外字节；该路线已实现但必须在 3.7 验证，不存在普通内存写入或 Commander fallback。
- 校验按段完整读回并流式统计连续不匹配区域。公共 `VERIFY_FAILED.details` 只包含 `first_address`、`first_length`、`total_regions`，不保留或返回目标内容。
- `flash/erase` 的 `after: none | reset_halt | reset_run` 由 Schema 强制提供。只有副作用、所请求校验和最终状态均成功才返回 `{}`；失败不伪装成功，也不隐式 reset/run。
- 副作用请求在命名管道已分派后失联返回 `EXECUTION_UNCERTAIN`；只读 verify 同场景保持可重试的 Worker 不可用语义。确定的 Flash 修改使验证缓存失效，但活动目标配置身份持续到 disconnect，允许同一连接后续独立 verify。
- 公共 Schema 和内部 `ProgramRequest` 都拒绝未声明字段；没有 authorization、confirmation token、权限对话或硬编码测试结果。

## 主要测试与提交门禁

| 检查 | 结果 | 覆盖 |
|---|---|---|
| `cargo test -p jlink-domain --test t_p2_program` | PASS，3/3 | 动态区域边界、跨区/零长/溢出拒绝、紧凑多区域 mismatch、显式 after、无授权字段、执行分类 |
| `cargo test -p jlink-worker --lib` | PASS，9/9；2 个环境测试按声明隔离 | HSS 首检、活动目标/验证缓存分离、Flash 不确定态、IPC command/payload 匹配、x64 ABI 结构 |
| `cargo test -p jlink-mcp --test t_p1_mcp` | PASS，8/8 | after 必填、整片/成对范围 Schema、默认六工具合同、`FLASH_RANGE_INVALID`/`VERIFY_FAILED` 公共映射 |
| 冻结 DLL 定向用例（显式 `--ignored --exact --nocapture`） | PASS，1/1 | 无探针加载 6.98a，并精确取得 S32K144 两个 Flash 区域 |
| `scripts/check-workspace.ps1` | PASS | 最终代码的格式、workspace `clippy -D warnings`、workspace 测试和四 crate 依赖方向 |
| `openspec validate define-jlink-mcp-v1 --strict` | PASS | 规格、任务与实现约束严格校验 |
| `git diff --check` | PASS | 最终代码状态无空白错误 |

提交门禁第一次只发现 Worker 路由函数长度、一个等价 `Option` 写法和错误参数借用三项静态质量问题；第二次只发现追加到旧 P1 测试的函数超长 1 行。修正分别为提取有独立职责的 program IPC 处理、机械等价表达式/借用，以及把 P2 Schema 断言独立成 T-P2-PRG 测试；第三次统一门禁通过。没有放宽断言、忽略失败、吞掉错误、硬编码成功或增加 fallback。

## 冻结输入与无探针 DLL 证据

| 输入 | 身份 / 结果 |
|---|---|
| J-Link DLL | `C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll`；6.98a；SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| x64 `JLINKARM_DEVICE_INFO` 前缀 | 568 bytes；`JLINKARM_DEVICE_GetInfo` 返回 0 |
| S32K144 Program Flash | `0x00000000`，长度 `0x00080000` |
| S32K144 Data Flash | `0x10000000`，长度 `0x00010000` |
| 实际 IAR OUT | SHA-256 `3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF`；三个归一化段均位于 Program Flash |
| T-P2-PRG 测试二进制 | `t_p2_program-b59088b89aeef8b5.exe`；SHA-256 `6AF68DFE70F1B32285C26EB51F4B6149A75E04ED3723D601FD6E430CE13B1E12` |
| Worker 测试二进制 | `jlink_worker-70029957f7b1d040.exe`；SHA-256 `4629127FE166DF549106C8B326C3798FEDBDDC282B593CE1ACDA3B9AC9E75FAC` |
| MCP 合同测试二进制 | `t_p1_mcp-0a53d5f5fa1f7864.exe`；SHA-256 `953A77143CDAEF8BD121CE9195BC09A52001B374F2AF4F5E4ABE8D3F907E61BE` |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`；`x86_64-pc-windows-msvc` |

实际 OUT 的段为 `0x00000000/784`、`0x00000400/16`、`0x00010010/205808`；最大结束地址小于 `0x00080000`。本轮原始命令输出保留在当前开发任务记录中。器件区域来源与范围擦除实现依据分别参考 SEGGER J-Link SDK、Device Support Kit 和 Read-Modify-Write Flash 文档；实际能力结论仍以冻结 DLL/目标的 3.7 测试为准。

## SVN 现场与证据边界

目标工作副本仍只有两处既有用户修改，未增加计划外差异，也未执行 `svn commit`：

| 受保护文件 | SHA-256 |
|---|---|
| `Appl/Source/Appl/AppPwrMode/AppPwrMode.h` | `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085` |
| `Appl/T26_DCU_APP_NXP.dep` | `B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620` |

PRG-001..PRG-006 的软件、合同、无探针 DLL 区域和状态机证据已完成。3.7 必须使用冻结的 DLL、探针、S32K144、SWD 4000 kHz 和当时 IAR OUT 重新验证：默认校验、独立 mismatch、整片/范围擦除、区间外字节保留、每种 `after`、HSS 活动冲突以及失败安全状态。任一 DLL、器件数据库、FFI ABI、Flash 区域、镜像解析、IPC、错误映射、范围擦除路线或状态机变化时，本证据失效。
