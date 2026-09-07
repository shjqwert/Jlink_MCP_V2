# J-Link MCP

通过 Codex 使用 SEGGER J-Link 调试 ARM Cortex-M 目标。Windows x64 Rust 实现，以独立 Worker 隔离 J-Link DLL 和目标会话。

当前源码版本为 **1.1.3**。本版补齐 HSS 启动阶段持久化与历史 key 查询、逐操作 readiness 及其输出 Schema、Worker 失联诊断和未知器件的无弹窗拒绝；提供身份块生成与失败构建阻断脚本，并明确基础验收优先使用锁存结果、编程前显式连接的流程。

对应功能已完成 Windows 主机门禁、J-Link 9.64 与安装匹配智芯支持包后的 6.98a 采集/中断恢复、真实 IAR 构建到烧录流程，以及 Luna 新 Codex 任务的原生插件验收。新任务只验证当前变量一致性与强身份读取，不代表重新执行请求或多轮稳定性。依赖指定 DLL 或硬件的 ignored 测试不计作默认测试通过；正式发布状态以 Releases 为准。

此前 1.1.2 在 Windows x64、Codex、SWD、Z20K146MC、J-Link 9.64 和 1 MHz 环境实测：5 秒 100 Hz 返回 600 条，5 秒 50 Hz 返回 300 条，主机约 5.04 秒而源时间约 5.98 秒，两次均正确报告 degraded 和 source_host_clock_mismatch。本次两种 DLL 版本的补测仍报告该质量限制；源/主机时间比例差异尚未定位，不能声明物理采样计时已修复。历史 V1.1.0 及原 S32K118/J-Link 6.98a 证据保留；P1-18、P0-12 根因及 JTAG 真机发布门禁仍保留原有待办。

## 安装与使用

普通用户使用预编译 ZIP，无需 Rust、MSVC C++、Windows SDK 或源码。下载入口为本仓库 [Releases](https://github.com/shjqwert/Jlink_MCP_V2/releases)；开发版本请使用明确标注的非正式构建。安装、配置与排障见 [INSTALL.md](INSTALL.md)。

Codex、兼容的 SEGGER 软件和探针所需驱动由用户自行准备。本工具不捆绑或代装 SEGGER 组件，不在安装时连接或操作目标。

| 工具 | 用途 |
|---|---|
| `jlink_target` | 工程配置、目标连接和验证 |
| `jlink_program` | 固件烧录、校验和擦除 |
| `jlink_inspect` | 符号、变量、内存和寄存器读取 |
| `jlink_write` | 变量、内存和寄存器写入 |
| `jlink_control` | 暂停、运行、复位和单指令步进 |
| `jlink_hss` | 固定时长采集、数据查询和质量证据 |

连接可能恢复 CPU 运行，故障恢复可能复位目标。操作前确保测试板和控制输出安全；调试使用的 ELF/OUT 应与板内固件对应。

## 源码构建

开发者需要 Windows x64、[固定的 Rust 工具链](rust-toolchain.toml)、MSVC C++ 构建工具和 Windows SDK。用户安装预编译包不需要这些依赖。

在仓库根目录执行：

```powershell
./scripts/check-workspace.ps1
cargo build --locked --release --target x86_64-pc-windows-msvc -p jlink-mcp -p jlink-worker
```

生成静态 CRT 的完整分发包：

```powershell
./scripts/build-release.ps1
```

打包脚本要求已提交且干净的 Git 检出；开发中的构建可显式传入 `-AllowDirty`，清单会记录未提交状态。输出位于 `target/distribution/`；ZIP 和 SHA-256 作为 Release 附件交付，不提交构建产物。

## 仓库范围

- `crates/`：四个 crate 的生产源码、源码内单元测试和编译必需资源。
- `plugins/`、`.agents/plugins/`：Codex 插件、使用指引和市场入口。
- `scripts/`：源码检查、构建、打包、安装和启动脚本。
- `.github/workflows/`：公开源码检查与预编译包构建流程。

设计资料、实验、独立集成测试、硬件验证脚本和开发上下文保留在开发者本地，不再纳入后续 Git 提交；旧历史不会改写。这些本地文件需要单独备份。

`check-workspace.ps1` 检查当前检出中存在的全部目标和测试。新克隆及公开 CI 只包含源码内单元测试；开发者保留独立测试时会执行更多用例。公开 CI 不替代完整集成、安装器、客户端或硬件回归，正式发布需要对同一发布包取得相应证据。

## 许可证

项目使用 [MIT License](LICENSE)。发布包附带第三方依赖声明；SEGGER 组件不包含在本项目的分发包中。


### HSS clock evidence

`actual_rate_millihz` uses raw source timestamp intervals; it is not a host-clock
rate measurement. When a source timestamp exceeds its host read completion plus
the Start-call mapping bound, `source_host_clock_mismatch` marks degraded timing
and disables period/runtime estimates. All raw records remain available; unknown
loss evidence stays unknown. Do not truncate or rescale records to fit a request.
The host `duration_s` interval starts after DLL Start returns; completion elapsed
also includes Stop/tail drain. DLL/probe clock discrepancies require separate
hardware investigation and cannot be corrected by relabelling source time.

Only the named device operator may use J-Link, including reads; code delegation
does not transfer device ownership. For an authorized stimulus during HSS, use
`return_when: started`, then the operator performs the allowed serialized write.
