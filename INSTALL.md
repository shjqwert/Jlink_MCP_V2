# J-Link MCP V1.1.1 预编译包

本包部署 Windows x64 MCP 主程序、同版本 Worker 和 Codex 插件。**用户电脑不需要 Rust、Cargo、MSVC、Windows SDK、Git 或源代码。** 安装脚本仅需 Windows 自带的 64 位 PowerShell 5.1；不会安装其他脚本运行时。

V1.1.0 已验证 Windows x64 / Codex / SWD / Z20K146MC / J-Link 9.64 / 1 MHz。P1-18 已实现但未完成专项验证；原 S32K118/J-Link 6.98a 的 P0-12 根因仍未由本版关闭。JTAG、其他客户端和 ARM64 未通过本次发布门禁；正式状态以对应版本的 Release 说明为准。

## 用户自行准备

- Codex，以及可运行 `codex plugin` 的 Codex CLI。CLI 具体安装方式由用户选择；本工具不自动下载或安装 Codex，也不要求必须使用 npm 版本。
- 需要操作硬件时，自行安装兼容的 SEGGER J-Link 软件及探针所需驱动，并准备目标工程、ELF 和实际器件标识。本版本已验证的 J-Link 基线是 9.64；不能直接假定任意其他版本兼容。
- SEGGER DLL 自身需要的运行库由用户按 SEGGER 的要求准备。我们的静态 CRT 构建不改变第三方 DLL 的依赖。

本包**不包含 SEGGER DLL、驱动或安装程序**；安装器不下载、安装、升级或卸载这些组件，不连接探针、不操作目标。未准备 SEGGER 仍可安装插件、初始化 MCP、列出六工具和查询断开状态。

## 下载和安装

1. 从可信的仓库 Release 取得 `jlink-mcp-V1.1.1-windows-x64.zip` 与 `.zip.sha256`。使用 `Get-FileHash <ZIP路径> -Algorithm SHA256` 与可信发布页的值核对；同来源的可修改摘要只能帮助检错，不能独立认证发布者。本版未进行代码签名。
2. 解压完整目录，保留 `.agents`、`.codex-plugin` 等目录；不要仅复制 EXE。
3. 关闭本工具正在运行的 Codex 任务/HSS 会话。在普通权限的 64 位 PowerShell 中进入解压根目录，执行：

   ```powershell
   powershell.exe -NoLogo -NoProfile -ExecutionPolicy RemoteSigned -File .\scripts\install-codex-plugin.ps1
   ```

   `RemoteSigned` 只作用于本次子进程，不修改全局策略，也不能覆盖组织组策略。若 Windows 将从网络下载的脚本标记为阻止，先核对来源和摘要，再由用户对该包的三个 `.ps1` 文件使用文件属性“解除锁定”或 `Unblock-File`；不要关闭安全软件或绕过组织策略。
4. 安装结果应包含版本 `1.1.1`、deployment 路径和 `segger_managed: false`。打开新的 Codex 任务，确认只出现本插件固定的六工具。

安装包位于其他目录时，可以给安装器传 `-PackageDirectory <完整发布包目录>`。从源码树直接运行安装器不会编译，必须先在开发机生成包。`-SkipBuild` 为旧调用保留，但不再改变任何行为；裸 `-BinaryDirectory` 不作为发布包接受。

安装完成后不再依赖解压目录，可由用户自行移走。不要移动安装器输出的 deployment 目录。

## 首次硬件配置

安装不会创建或覆盖工程 `jlink-mcp.toml`。参照 `jlink-mcp.example.toml` 填入实际器件、ELF，以及本机 DLL 路径、文件版本和 SHA-256。示例中的占位符不能直接用于连接。

DLL 身份属于工程基线，用户级配置不能覆盖 DLL 路径/版本/哈希。复制工程到新电脑时应显式更新适用工程配置并验证，不要关闭身份检查。配置有效后，由用户显式请求验证和连接；路径缺失、版本或摘要不符按已有错误合同报告，不自动修复 SEGGER 安装。

## 安装位置、升级和回退

产品根：`%LOCALAPPDATA%\Programs\jlink-mcp`。

```text
jlink-mcp/
  current.json                        当前完整 deployment 的原子指针
  install.lock                        安装/启动互斥锁文件
  deployments/1.1.1-<16位摘要前缀>/     成套 EXE、插件、marketplace 和安装资料
  transactions/                       本机安装及恢复记录
```

每次升级使用新包执行同一安装器；运行中拒绝升级且不结束进程。新版本验证后切换，失败恢复原本的版本指针和本插件 marketplace 来源。旧版本和旧固定路径二进制不会自动删除。

目录名使用摘要前缀缩短路径，复用目录时仍核对完整 SHA-256。若已有目录损坏或上次安装中断，新部署增加随机后缀而不覆盖原目录；新包在独立目录中完成校验后才发布指针。超过 Windows PowerShell 5.1 路径限制的用户目录会明确拒绝，不能通过忽略包校验继续安装。

安装器只自动替换可识别的本地 `jlink-mcp-v2` marketplace；遇到已禁用插件、Git marketplace 或无法确认的既有注册会停止，避免改变用户偏好或破坏来源。用户需显式处理该状态后重试。

安装进程若被强行终止或系统断电，Codex 注册可能尚未完成；已发布的指针仍只指向完整目录。重新执行安装器完成恢复。普通失败若显示 `Recovery also failed`，保留输出和 `transactions` 记录，先修复 Codex CLI/来源目录问题再重试；不能据此认为自动回退已成功。不要手工编辑或同时替换两份 EXE。

需要回退时，关闭本工具会话，然后用保留的上一份完整发布包运行同一安装器。升级或回退都不会修改工程配置、`.jlink-mcp/captures`、其他插件或 SEGGER 文件。安装成功后会删除已确认属于本产品且不再被当前指针引用的旧 deployment；无法识别或无法访问的目录会保留并在结果中报告。路径不存在与权限不足会分别报告。

## 卸载和排障

卸载是用户显式操作：关闭相关会话，使用 `codex plugin remove jlink-mcp@jlink-mcp-v2` 移除本插件，必要时使用 `codex plugin marketplace remove jlink-mcp-v2` 移除其 marketplace。产品文件与事务记录由用户确认后自行清理；工程配置、采集数据和 SEGGER 软件不属于本工具的自动清理范围。

- `codex` 不存在：先准备支持插件命令的 CLI；安装器不会代装。
- 清单或文件摘要不符：停止，重新取得可信完整包；不要忽略错误。
- 文件锁/进程占用：关闭本工具的任务和会话后重试；遗留的空 `install.lock` 文件本身不代表仍被占用。
- 缺少 DLL、哈希不符或目标无法连接：由用户检查 SEGGER 环境及工程配置。
- 不支持的 CLI 输出/策略限制：保持原安装，提供错误与 CLI 版本，不通过修改全局配置绕过。
