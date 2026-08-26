# P2 DWARF 与访问计划证据

## 结论

任务 3.2 的 T-P2-DWARF 通过。生产解析器可以从 IAR 8.32 ARM little-endian ELF 的 DWARF 3/4、`.debug_types` 和 `DW_FORM_ref_sig8` 建立索引，按精确且唯一的路径生成不可变 `AccessPlan`，并接通 `jlink_inspect.symbols`。本任务没有执行目标写入、烧录或硬件连接。

## 已验证规则

- `AccessPlan` 固定记录完整 ELF SHA-256、解析器格式版本、规范化选择器、静态地址、字节大小、位域范围、volatile 属性和递归类型布局。
- 缓存键严格为 `ELF SHA-256 + 规范化选择器（含独立 slice）+ 解析器格式版本`；不缓存目标内存值或 Worker 状态。
- 全局或静态变量只按精确名称解析；多个非 declaration 定义返回 `SYMBOL_AMBIGUOUS`，不存在返回 `SYMBOL_NOT_FOUND`。
- 变量位置只接受单一 `DW_OP_addr` 或等价静态地址；动态表达式返回 `DYNAMIC_LOCATION_UNSUPPORTED`。
- 指针仅形成地址值计划，不跟随成员。
- 固定数组允许路径 `[i]`；柔性或动态长度数组必须独立提供 `slice {start,count}`，`[i]` 不得替代 slice，单元素使用 `count:1`。3.2 只验证有效切片机制，ART-004 的 `SLICE_REQUIRED` 由 3.3 主要测试负责。
- `symbols` 使用 ASCII 不区分大小写的子串查询，保留 DWARF 精确拼写，按路径稳定升序返回；`limit` 为 1–50，默认 20。

## 冻结输入

| 输入 | SHA-256 / 身份 |
|---|---|
| IAR fixture 源 | `8C305EE9242DA9CECB5FA21863FB4755C2DE2068639774191033D3343749317A` |
| IAR fixture OUT | `0AD89204DE9382C06F2D1605A4CD53B284C62BC58E980C9BC61086EB43577909` |
| 实际 T26 OUT | `3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF` |
| 实际 producer | `IAR ANSI C/C++ Compiler V8.32.3.193/W32 for ARM`；`IAR Assembler V8.32.3.193/W32 for ARM` |
| 解析器格式版本 | `1` |

## 实际 T26 OUT 结果

- DWARF 版本：3、4。
- `.debug_info` compile units：2960。
- `.debug_types` type units：35。
- 已解析 `DW_FORM_ref_sig8` 引用：93。
- 类型定义：7174。
- 非 declaration 变量定义：3535。
- 可供 `symbols` 返回的稳定精确路径：3556。
- 实际 OUT 只读解析成功；没有修改或重新构建 SVN 工程。

## 自动验证

```text
cargo test -p jlink-mcp t_p2_dwarf -- --nocapture
结果：9 passed，0 failed

cargo clippy -p jlink-domain -p jlink-mcp --all-targets -- -D warnings
结果：通过

cargo run -p jlink-mcp --example t_p2_dwarf -- <T26_DCU_APP_NXP.out>
结果：DWARF 3/4、35 个 type unit、7174 个类型均完成索引
```

T-P2-DWARF 覆盖二维固定数组地址/步长、有效显式柔性 slice、含无界成员的外层 aggregate 拒绝、volatile 传播、规范化选择器缓存、ELF 内容变化失效、稳定 symbols 搜索、实际运行时路由和公共错误码；同一主要测试筛选还覆盖同名歧义、declaration 去重、动态位置拒绝、指针不跟随，以及实际 T26 OUT 的 DWARF 3/4、35 个 type unit 和 93 个 `DW_FORM_ref_sig8` 引用。缺失或无效 slice 及 `SLICE_REQUIRED` 不在本任务重复断言。

该主要测试显式依赖本机冻结的 F0-C fixture 和用户指定的 `D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP` 实际 OUT；路径或指纹缺失时测试明确失败，不静默跳过。

## SVN 安全基线

- `AppPwrMode.h` 仍为用户已有 `M`，SHA-256：`E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085`。
- `T26_DCU_APP_NXP.dep` 仍为用户已有 `M`，SHA-256：`B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620`。
- 本任务没有写入上述文件，也没有执行 `svn commit`。

## 证据失效条件

IAR 编译器版本、fixture/实际 OUT SHA-256、`gimli`/`object` 版本、解析器格式版本、选择器规范化、静态位置规则、柔性 slice 规则或访问计划字段任一变化时，必须重跑 T-P2-DWARF 和实际 T26 OUT 解析。
