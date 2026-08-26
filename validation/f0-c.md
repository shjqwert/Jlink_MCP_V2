# F0-C DWARF 复合类型可行性证据

## 裁决

`PASS`

IAR 8.32.3 隔离 fixture 已无警告编译、链接；Rust 实验解析器从链接后的 DWARF 4 生成 15 个固定访问计划，并从 ELF 初始化数据验证地址、成员偏移、数组步长、位域范围、数值解码和编码往返。覆盖结构体、嵌套/二维数组、位域、union 显式成员、柔性数组显式 slice、无符号/有符号 64 位、`float`/`double` 的 NaN 与正无穷。

实际测试工程的 `T26_DCU_APP_NXP.out` 也完成全量索引，包含 DWARF 3/4 和 IAR `.debug_types` 签名引用；没有修改或重新构建 SVN 工程，也没有把 fixture 烧录到板卡。

## 冻结构建与产物身份

| 对象 | 冻结值 |
|---|---|
| 实际工程 | `D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP`；配置名 `S32K144`（来自 `T26_DCU_APP_NXP.dep`） |
| 实际编译器 | IAR ANSI C/C++ Compiler `V8.32.3.193/W32 for ARM`；实际产物 DWARF producer 同值 |
| 实际 ELF | `Appl\Output\Exe\T26_DCU_APP_NXP.out`；4,962,648 bytes；SHA-256 `3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF` |
| 实际工程文件 | `T26_DCU_APP_NXP.ewp`；98,304 bytes；SHA-256 `654A0D5F4A21A6AD3D51E24AE7D057FF315D4CECB0E4662B3652703EA69C06BE` |
| 实际依赖清单 | `T26_DCU_APP_NXP.dep`；502,323 bytes；SHA-256 `B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620` |
| Fixture 编译 | `iccarm --debug --cpu Cortex-M4 --endian little -On -e`；源 SHA-256 `8C305EE9242DA9CECB5FA21863FB4755C2DE2068639774191033D3343749317A` |
| Fixture 链接 | IAR `ilinkarm V8.32.3.193`；实际工程 `S32K144_64_ram.icf`；`--no_entry --no_library_search` |
| Fixture object | `F0cDwarfFixture.o`；8,000 bytes；SHA-256 `E2D6B964199CA46DFAA7CBECB203FFF394DE63F2F06072DD99B3C8288EC79ABD` |
| Fixture ELF | `F0cDwarfFixture.out`；6,252 bytes；SHA-256 `0AD89204DE9382C06F2D1605A4CD53B284C62BC58E980C9BC61086EB43577909` |
| 实验二进制 | `f0c-dwarf.exe`；657,920 bytes；SHA-256 `D01BCEB5BFFEEB1C3930A50A8271FD57BFD58C75F7469CBA1D6DEFC4F935C15F` |
| 接受证据 | `validation/evidence/f0-c/access-plans.json`；SHA-256 `70D645813C19CB54F45615CFB564D04372C3FCF98769CF96C47927D4888430F2` |

完整可复现命令记录在 `experiments/p0/f0c-dwarf/README.md`。

## 访问计划结果

| 能力 | 代表选择器 | 解析结果 |
|---|---|---|
| 嵌套结构体 | `gstF0cRoot.stNested.ulSequence` | `0x20000000`，4 bytes，值 `7` |
| 二维数组 | `gstF0cRoot.stNested.awMatrix[1][2]` | `0x2000000E`，2 bytes，值 `3` |
| 位域 | `uiReadyFlg` / `iDelta` / `uiMode` | 同一 32-bit storage；LSB/宽度分别为 `0/1`、`1/5`、`6/3`；值 `1/-7/5` |
| union | `gstF0cRoot.unPayload.fPhysicalValue` | 显式成员视图，值 `1.0`；未推断 active member |
| 柔性数组 | `gstF0cFlex.aucPayload slice(start=1,count=3)` | `0x20001003`，值 `[22,33,44]`；无 slice 时不生成计划 |
| 64 位 | `ullCounter` / `llOffset` | 8-byte little-endian；值 `18364758544493064720` / `-5124095576030430` |
| 非有限值 | `gaunF0cFloatSpecial[*].fPhysicalValue`、`gaunF0cDoubleSpecial[*].dPhysicalValue` | `float`/`double` 的 `NaN` 和 `Infinity` 均保留位模式完成往返 |

15 个计划全部满足期望值且 `encodeDecodeRoundTrip=true`。所有变量位置均为单一 `DW_OP_addr`；其他动态 location 不生成固定计划，符合当前 OpenSpec 的拒绝边界。

## 实际产物兼容性

实际 `T26_DCU_APP_NXP.out` 索引结果：

- ARM little-endian；DWARF 3 和 4；producer 为 IAR compiler/assembler 8.32.3.193。
- 2,960 个 `.debug_info` 编译单元、35 个 `.debug_types` type unit。
- 7,174 个已索引类型、1,983 个变量，其中 1,054 个变量具有固定位置。
- 解析器支持 IAR 实际产物出现的 `DW_FORM_ref_sig8`；fixture 本身只使用普通 `.debug_info`，两条路径都已验证。

## 验证

- IAR fixture compile/link：errors `0`，warnings `0`。
- `cargo fmt --check`：通过。
- `cargo test -p f0c-dwarf`：2 passed，0 failed。
- `cargo clippy -p f0c-dwarf --all-targets -- -D warnings`：通过。
- `cargo build -p f0c-dwarf --release`：通过。
- Release 端到端：`F0-C PASS: 15 access plans`。

## 复用与失效条件

本证据仅在 IAR 编译器版本、实际 `.out` SHA-256、fixture 源/构建命令、DWARF 解析器版本、位域 offset 规则、选择器规范化、数值编码和固定 location 策略不变时复用。上述任一项变化，或 P2 增加新的 DWARF expression、endianness、ABI 或动态数组规则时，必须重跑对应访问计划。
