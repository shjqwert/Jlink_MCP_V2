# P2 变量与原始内存访问证据

## 结论

OpenSpec 任务 3.5 的主要测试 T-P2-MEM 已完成软件、合同、冻结 DLL 器件区域和实际 IAR 夹具验证。生产链路已接通变量读写、1–4096 字节原始内存读写、Flash/RAM/MMIO 分类、短写不确定态、显式读回、固件身份验证和安全读取合并规则。本任务没有连接探针或访问目标内存；真实 RAM/MMIO/变量写入、读回、对齐能力和目标最终状态仍必须由 3.7 的冻结硬件纵向测试证明。

## 已验证实现

- `jlink_inspect.memory/variable` 与 `jlink_write.memory/variable` 经严格 MCP Schema、版本化 IPC 和唯一 Worker gateway 执行；普通读取只返回 `data` 或 `value`，完整成功写入只返回 `{}`。
- 原始内存请求统一校验 32 位 Cortex-M 地址空间和 1–4096 字节上限。冻结 DLL 的器件数据库提供 Flash/RAM 区域；命中已知 Flash 的普通写入在副作用前返回 `ADDRESS_OUT_OF_RANGE` 并指向 `jlink_program`，跨已知区域边界同样拒绝。
- 已知区域外的显式地址作为 MMIO 候选，不增加确认、白名单、授权字段或未获批准的 fallback。实际可访问性及目标传输对齐由冻结 J-Link 通用内存调用的精确返回确认；短写或负返回统一为不可重试的 `EXECUTION_UNCERTAIN`，并报告本次请求/实际总长度。
- `verify` 默认为 `none`，不执行写后读回；显式 `readback` 才读取相同范围并返回 `VERIFY_FAILED`、首个差异地址和期望/实际字节。
- 变量操作由同一 ELF 生成 `AccessPlan` 和固件身份计划。Worker 在连接会话首次需要时读取完整 Flash 身份段，匹配后缓存；断连、Worker 退出或 Flash 修改时失效。计划、类型和值在写入前重新校验，复合值先在当前字节副本中完整编码，失败不产生部分写入。
- 活动 HSS 时普通读取被拒绝；RAM/MMIO 或变量写入只进入既定串行调度边界。若变量写入尚需首次固件身份读取，则因 HSS 期间禁止普通读取而拒绝，不通过停止 HSS 或绕过身份校验继续。
- 安全合并规划器只合并原顺序中相邻、4 字节对齐、无副作用且同区域的 RAM/静态变量读取。MMIO、`volatile`、跨区、未对齐、不相邻或降序访问保持独立；写入从不因合并扩大范围。该规划器将在 P3 多选择项读取中复用，3.5 已冻结并验证纯规则。

## 目标 IAR 测试夹具

仅在 `Appl/Source/Appl/AppUserDesc/AppUserDesc.c` 增加三个 `__root` 测试对象：一个 8 字节复合变量、一个连续 `UINT32[10]` HSS 数组和一个 32 位可写标量。没有新增函数、任务、业务调用链、头文件或运行时逻辑；相邻注释均为英文。

IAR Embedded Workbench 8.2.4.5914 使用目标工程 `S32K144` 配置在隔离副本中完成构建：0 errors、26 warnings。构建没有接触真实 SVN 工作副本的 `.dep`，随后只把验证过的 OUT/S19/MAP 回写到既有未版本化输出位置。显式证据测试从新 OUT 解析到全部三个对象：复合对象 8 字节、成员 4 字节、数组 40 字节、首尾元素地址相差 36 字节、标量 4 字节，地址均位于 S32K144 SRAM；同一 OUT 的固件身份段全部位于冻结器件 Flash 区域。

| 输入 / 产物 | SHA-256 / 身份 |
|---|---|
| `AppUserDesc.c` | `1133B85709AB5ED3509ED58433ED4132E4D0869724140F8D3F560F7BA3B709E4` |
| IAR OUT | `9CA4B80CE028F03BDE56082C20BFEDFA65FAE6264B33FB3190FD87FC7DA5CCE2` |
| IAR S19 | `0948AD69AF91437E2906A8371B6C18BE38C00DC60B23C9739773FB92CB82686A` |
| IAR MAP | `D5400CF903966A66EC06C12050A9CED6AD661ED6760D7C2BB656D27E7D75FFB7` |
| J-Link DLL | `C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll`；6.98a；SHA-256 `D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5` |
| 目标配置 | S32K144；SWD；4000 kHz |
| T-P2-MEM domain 测试二进制 | `t_p2_mem-3b829331c238ff12.exe`；SHA-256 `A3263CC1D6BFA42F986AEAB1BF4AE93DCE5CCB10DA4ADED5C26172345C800A56` |
| T-P2-MEM MCP 测试二进制 | `t_p2_mem-cf894bc7e0f8885f.exe`；SHA-256 `F8BB61A0E1F5710D67BB475A9482AD11D8A9F980F74ED63858C3EF377B3186EE` |
| Worker 测试二进制 | `jlink_worker-5c030b770711a58a.exe`；SHA-256 `77F287B9E492095735283797406739CEBF8C995C726CFAA440EC3E92A8D80BA2` |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)`；`x86_64-pc-windows-msvc` |

## 最小相关测试与提交门禁

| 检查 | 当前结果 | 覆盖 |
|---|---|---|
| `cargo test -p jlink-domain --test t_p2_mem` | PASS，3/3 | 长度/地址/区域、Flash 拒绝、短写、读回差异、安全合并 |
| `cargo test -p jlink-mcp --test t_p2_mem` | PASS，3/3；目标 OUT 证据用例另以 `--ignored --exact` 显式 PASS 1/1 | 严格 Schema、同 ELF 执行载荷、公共成功/错误、实际 IAR 变量布局与固件段 |
| Worker 定向用例 | PASS | command/payload 匹配、HSS 读写边界、Flash 修改后的固件身份缓存失效 |
| 冻结 DLL 定向用例 | PASS，1/1 | 6.98a 无探针加载；S32K144 Flash/RAM 区域及 MMIO 候选分类 |
| `scripts/check-workspace.ps1` | PASS | 格式、workspace `clippy -D warnings`、workspace 测试、依赖方向 |
| `openspec validate define-jlink-mcp-v1 --strict` | PASS | 规格、任务与证据路线严格校验 |
| `git diff --check` | PASS | 最终代码状态空白检查 |

原始命令输出保留在当前开发任务记录中，最终门禁完成时间为 `2026-08-26T19:37:33+08:00`。开发和局部修复只运行上述最小相关测试；全量门禁只在任务 3.5 准备原子提交时统一执行。

提交门禁先后发现两组机械 clippy 问题：手写倍数判断/潜在 panic，以及只读参数所有权/固定分块 API；均以等价标准库写法和消除 panic 路径修正。随后 T-P2-DWARF 明确报告实际 SVN OUT 已从 3.2 的旧指纹变化为本任务新指纹，按证据失效规则重新解析，确认 DWARF 3/4、35 个 type unit、7177 个类型和 93 个 `DW_FORM_ref_sig8` 引用，并只更新当前实际 OUT 的测试与刷新记录；旧 F0-C/P2-3.2 历史证据未被改写。最终统一门禁通过，没有忽略测试、放宽断言、吞掉错误、硬编码成功或增加 fallback。

## SVN 现场与证据边界

未执行 `svn commit`。当前受控差异为计划内 `AppUserDesc.c` 夹具、既有用户修改及未版本化构建输出；两个受保护文件在隔离构建和产物回写后哈希保持不变：

| 受保护文件 | SHA-256 |
|---|---|
| `Appl/Source/Appl/AppPwrMode/AppPwrMode.h` | `E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085` |
| `Appl/T26_DCU_APP_NXP.dep` | `B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620` |

DBG-002、DBG-003、DBG-006 的软件与合同证据已经形成；DBG-001 增补了实际目标工程的执行计划和夹具证据。3.7 必须使用上述 DLL、S32K144、SWD 4000 kHz、新 OUT 和当时实际探针身份重新验证：原始 RAM/MMIO 读写、变量/复合值读写、默认无读回、显式读回、短写/对齐失败、HSS 冲突及写后原值恢复。任一 DLL、器件数据库、OUT/ELF、AccessPlan 格式、TypedValue、IPC、范围/错误规则或状态机变化时，本证据失效。
