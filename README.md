# J-Link MCP

通过 Codex 使用 SEGGER J-Link 调试 ARM Cortex-M 目标。Windows x64 Rust 实现，以独立 Worker 隔离 J-Link DLL 和目标会话。

当前版本为 **V1.0.0 候选**，发布验收范围为 Windows 11 x64、Codex 和 SWD。干净 Windows、新 Codex 桌面任务及新二进制真机验收尚未完成；不将构建成功等同于正式验收通过。

## 安装与使用

普通用户使用预编译 ZIP，无需 Rust、MSVC C++、Windows SDK 或源码。下载入口为本仓库 [Releases](https://github.com/shjqwert/Jlink_MCP_V2/releases)；未发布版本请使用明确标注的候选交付包。安装、配置与排障见 [INSTALL.md](INSTALL.md)。

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

打包脚本要求已提交且干净的 Git 检出；开发中的候选可显式传入 `-AllowDirty`，清单会记录未提交状态。输出位于 `target/distribution/`；ZIP 和 SHA-256 作为 Release 附件交付，不提交构建产物。

## 仓库范围

- `crates/`：四个 crate 的生产源码、源码内单元测试和编译必需资源。
- `plugins/`、`.agents/plugins/`：Codex 插件、使用指引和市场入口。
- `scripts/`：源码检查、构建、打包、安装和启动脚本。
- `.github/workflows/`：公开源码检查与候选包构建流程。

设计资料、实验、独立集成测试、硬件验证脚本和开发上下文保留在开发者本地，不再纳入后续 Git 提交；旧历史不会改写。这些本地文件需要单独备份。

`check-workspace.ps1` 检查当前检出中存在的全部目标和测试。新克隆及公开 CI 只包含源码内单元测试；开发者保留独立测试时会执行更多用例。公开 CI 不替代完整集成、安装器、客户端或硬件回归，正式发布需要对同一候选包取得相应证据。

## 许可证

项目使用 [MIT License](LICENSE)。发布包附带第三方依赖声明；SEGGER 组件不包含在本项目的分发包中。
