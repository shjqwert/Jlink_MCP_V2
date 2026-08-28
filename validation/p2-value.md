# P2 TypedValue 与复合写入预校验证据

## 结论

OpenSpec 任务 3.3 的主要测试 T-P2-VALUE 通过。实现已覆盖独立柔性 `slice {start,count}`、稳定 `SLICE_REQUIRED`、无损递归 `TypedValue`、位域与 union，以及在返回编码结果前完成的复合写入全量预校验。FT-008 进一步用公共 `$defs.typedValue` 对齐写入、读取和 HSS Schema，并已完成 T26 真机数组切片写入、读回和原值恢复。

## 已验证规则

- 柔性或动态长度数组没有独立 slice、使用路径 `[i]` 代替 slice、`count=0`、元素范围溢出或无界数组字节范围溢出时返回 `SLICE_REQUIRED`；单元素必须使用 `count:1`。
- `jlink_write.variable` 接受与读取相同的可选独立 slice，公共 MCP 错误保留稳定代码 `SLICE_REQUIRED`。
- 写入输入、变量读取输出、HSS 规则值和 HSS 采样值引用同一递归 `$defs.typedValue`；数值数组、嵌套结构体数组、`$int`、`$float`、`$pointer` 和 `$union` 可表达，普通字符串和 `null` 被 Schema 拒绝。
- 安全整数使用 JSON number；超出安全范围的整数保留十进制字符串、位宽和符号性；有限浮点、`NaN`、正负无穷、布尔、指针、结构体、固定/多维数组和 union 均有确定表示。
- 结构体写入要求完整且唯一的成员集合；数组写入要求精确元素数；union 写入必须且只能指定一个成员。任一嵌套值无效时，编码在调用者当前字节的副本上失败，不产生部分结果。
- union 读取保留所有可解释成员；仅跳过明确为 `TYPE_UNSUPPORTED` 的成员，其他错误继续返回。
- 位域写入只更新选中位。DWARF3/4 `DW_AT_bit_offset` 与 DWARF4 `DW_AT_data_bit_offset` 统一形成 `BitRange`；最短回归将 `0xA5` 的低 3 位读取为 `-3`，写入 `-2` 后得到 `0xA6`，其余位保持不变。

## 主要测试与门禁

| 检查 | 结果 | 覆盖 |
|---|---|---|
| `cargo test -p jlink-mcp --test t_p1_mcp --test t_p2_value -- --nocapture` | PASS，17/17 | 六工具输入/输出元 Schema、递归 TypedValue Schema、IAR 复合值、slice、64 位整数、非有限浮点、指针、union、位域和复合写入预校验 |
| `cargo test -p jlink-mcp t_p2_dwarf -- --nocapture` | PASS，10 passed / 1 ignored | 两种 DWARF 位域属性归一化、已验证版本边界和 IAR 访问计划回归；外部 T26 兼容测试按设计需要显式环境变量 |
| `scripts/check-workspace.ps1` | PASS | 最终代码状态的格式、workspace `clippy -D warnings`、workspace 测试和四 crate 依赖方向 |
| `openspec validate --specs --strict --no-interactive` | PASS，9/9 | 当前主规格严格校验 |

统一门禁首次发现 TypedValue 的函数长度和显式数值转换 lint，修正为职责明确的结构体编码函数、范围校验后的局部浮点窄化及位模式有符号转换；第二次发现两个等价 `Option` 表达式 lint，完成机械修正后门禁通过。没有放宽断言、吞掉错误、增加 fallback 或硬编码结果。

## 冻结输入与现场

| 输入 | SHA-256 / 身份 |
|---|---|
| IAR fixture 源 | `8C305EE9242DA9CECB5FA21863FB4755C2DE2068639774191033D3343749317A` |
| IAR fixture OUT | `0AD89204DE9382C06F2D1605A4CD53B284C62BC58E980C9BC61086EB43577909` |
| 解析器格式版本 | `1` |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`，`x86_64-pc-windows-msvc` |
| T26 OUT | `18F0D0BFA4B79CCDE12E2BB4B36B51F7E73ED1EFE8E6232B8EE6F138C8ABCA18` |

## FT-008 真机回归

- 固定目标为 S32K144、SWD 4000 kHz、J-Link DLL 6.98a、探针 S/N `260106173`。
- 首次读取 `stTest.ucData2` 的 `slice {start:0,count:2}` 得到 `[0,0]`。
- 使用实时 MCP Schema 提交数值数组 `[17,34]` 且 `verify=readback`，写入返回空对象；独立 `jlink_inspect.variable` 回读 `[17,34]`。
- 使用同一路径恢复 `[0,0]` 并独立回读确认；恢复后 `jlink_target.status` 为 `connected/running`，随后正常断开。

目标 SVN 工作副本仍只有两处既有用户修改：`AppPwrMode.h` 的 SHA-256 为 `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085`，`T26_DCU_APP_NXP.dep` 为 `B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620`。本任务未修改目标工程，也未执行 `svn commit`。

## 证据边界与失效条件

ART-004、ART-006 的主要 fixture 证据已完成。DBG-001 已补充 `stTest.ucData2` 数值数组切片的真机写入、独立读回和原值恢复；其他目标类型仍以对应阶段证据为准，不从本次数组回归外推。

IAR fixture/OUT 指纹、`gimli`/`object` 版本、解析器格式、TypedValue 表示、slice 规则、位域归一化、union 规则、复合预校验或公共错误 Schema 任一变化时，必须重跑 T-P2-VALUE；涉及 DWARF 解析时同时重跑 T-P2-DWARF。
