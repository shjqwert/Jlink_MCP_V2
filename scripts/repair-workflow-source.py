"""One-shot source edits for firmware preparation, never target operations."""
from pathlib import Path
import hashlib


def replace_once(text, before, after):
    if text.count(before) != 1:
        raise RuntimeError(f"Expected one source anchor: {before[:100]!r}")
    return text.replace(before, after, 1)


def read_base(root, path, sha):
    data = (root / path).read_bytes()
    actual = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
    if actual != sha:
        raise RuntimeError(f"Unexpected baseline for {path}: {actual}")
    return data.decode("utf-8")


IDENTITY = r'''#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$BuildId,
    [string]$OutputPath = '',
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($BuildId.Length -gt 58) { throw 'BuildId must contain 1..58 printable ASCII characters' }
foreach ($character in $BuildId.ToCharArray()) {
    if ([int]$character -lt 32 -or [int]$character -gt 126) {
        throw 'BuildId must contain only printable ASCII characters (0x20..0x7E)'
    }
}
[byte[]]$idBytes = [Text.Encoding]::ASCII.GetBytes($BuildId)
[byte[]]$bytes = @([byte]0x4A, [byte]0x4C, [byte]0x49, [byte]0x44, [byte]1, [byte]$idBytes.Length) + $idBytes
$initializer = (@($bytes | ForEach-Object { '0x{0:X2}' -f $_ }) -join ', ')
# Do not insert unescaped user text into a C comment or literal.
$source = @"
/* Generated JLID v1. Use a fresh build ID when firmware changes.
 * Keep this symbol in a loaded, read-only Flash section of the final ELF.
 * GCC/Clang linker scripts using section GC must KEEP(.jlink_mcp_identity).
 */
#include <stdint.h>
#if defined(__ICCARM__)
#define JLINK_IDENTITY_KEEP __root
#elif defined(__GNUC__) || defined(__clang__)
#define JLINK_IDENTITY_KEEP __attribute__((used, section(".jlink_mcp_identity")))
#else
#error Define the compiler-specific retention rule for __jlink_mcp_identity
#endif
JLINK_IDENTITY_KEEP const uint8_t __jlink_mcp_identity[] = {
    $initializer
};
#undef JLINK_IDENTITY_KEEP
"@
if (-not $OutputPath) { $source; return }
$full = [IO.Path]::GetFullPath($OutputPath)
$mode = if ($Force) { [IO.FileMode]::Create } else { [IO.FileMode]::CreateNew }
$stream = [IO.File]::Open($full, $mode, [IO.FileAccess]::Write, [IO.FileShare]::None)
try {
    $encoded = [Text.Encoding]::ASCII.GetBytes($source + [Environment]::NewLine)
    $stream.Write($encoded, 0, $encoded.Length)
    $stream.Flush($true)
}
finally { $stream.Dispose() }
[pscustomobject]@{ path = $full; identity_bytes = $bytes.Length; build_id_length = $idBytes.Length }
'''

BUILD = r'''#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$FilePath,
    [string[]]$ArgumentList = @(),
    [Parameter(Mandatory = $true)][string]$ArtifactPath,
    [string]$ReceiptPath = '',
    [scriptblock]$OnSuccess
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$artifact = [IO.Path]::GetFullPath($ArtifactPath)
if (-not $ReceiptPath) { $ReceiptPath = $artifact + '.jlink-build.json' }
$receipt = [IO.Path]::GetFullPath($ReceiptPath)
if ($artifact.Equals($receipt, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'ReceiptPath must not equal ArtifactPath'
}
$command = Get-Command -Name $FilePath -CommandType Application -ErrorAction Stop | Select-Object -First 1
# Remove only this helper's prior receipt, never the user's prior firmware image.
# A failed build must not leave a stale success receipt eligible for a next stage.
if (Test-Path -LiteralPath $receipt) { Remove-Item -LiteralPath $receipt -Force }
$before = $null
if (Test-Path -LiteralPath $artifact -PathType Leaf) {
    $item = Get-Item -LiteralPath $artifact
    $before = [pscustomobject]@{
        modified = $item.LastWriteTimeUtc
        length = $item.Length
        sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
    }
}
$started = [DateTime]::UtcNow
& $command.Source @ArgumentList | Out-Host
$exitCode = $LASTEXITCODE
if ($exitCode -ne 0) { throw "Build failed with exit code $exitCode; dependent stages were not invoked" }
if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) { throw 'Build reported success but produced no firmware image' }
$item = Get-Item -LiteralPath $artifact
if ($item.Length -eq 0) { throw 'Build produced an empty firmware image' }
$sha256 = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash
if ($item.LastWriteTimeUtc -lt $started -or
    ($null -ne $before -and $before.modified -eq $item.LastWriteTimeUtc -and
     $before.length -eq $item.Length -and $before.sha256 -eq $sha256)) {
    throw 'No fresh image from this build was proven; use an explicit rebuild or a new output path'
}
$result = [pscustomobject][ordered]@{
    schema_version = 1
    build_succeeded = $true
    exit_code = $exitCode
    build_started_utc = $started.ToString('o')
    build_completed_utc = [DateTime]::UtcNow.ToString('o')
    artifact_path = $artifact
    artifact_bytes = $item.Length
    artifact_sha256 = $sha256.ToLowerInvariant()
}
$temporary = $receipt + '.' + [Guid]::NewGuid().ToString('N') + '.tmp'
try {
    [IO.File]::WriteAllText($temporary, ($result | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination $receipt -Force
}
finally {
    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
}
# Only explicitly supplied dependent work is invoked, and only after all gates.
# This helper does not flash, connect, or otherwise access a target on its own.
if ($null -ne $OnSuccess) { & $OnSuccess $result }
$result
'''

TESTS = r'''#requires -Version 5.1
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Assert-Throws([scriptblock]$Action, [string]$Message) {
    $failed = $false
    try { & $Action | Out-Null } catch { $failed = $true }
    Assert-True $failed $Message
}
$identity = Join-Path $PSScriptRoot 'new-firmware-identity.ps1'
$build = Join-Path $PSScriptRoot 'invoke-verified-build.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) ('jlink workflow ' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $root
try {
    $source = (& $identity -BuildId 'TEST') -join "`n"
    $matches = [regex]::Matches($source, '0x[0-9A-F]{2}')
    Assert-True ($matches.Count -eq 10) 'Identity byte count must be 6 + ASCII length'
    Assert-True (($matches | ForEach-Object { $_.Value }) -join ',' -eq '0x4A,0x4C,0x49,0x44,0x01,0x04,0x54,0x45,0x53,0x54') 'JLID layout mismatch'
    $long = (& $identity -BuildId ('A' * 58)) -join "`n"
    Assert-True ([regex]::Matches($long, '0x[0-9A-F]{2}').Count -eq 64) 'Maximum identity must fit 64 bytes'
    Assert-Throws { & $identity -BuildId ('A' * 59) } 'Oversized identity was accepted'
    Assert-Throws { & $identity -BuildId ([string][char]0x4E2D) } 'Non-ASCII identity was accepted'
    $identityPath = Join-Path $root 'identity.c'
    & $identity -BuildId 'TEST' -OutputPath $identityPath | Out-Null
    Assert-Throws { & $identity -BuildId 'TEST2' -OutputPath $identityPath } 'Generator overwrote a file without Force'

    $artifact = Join-Path $root 'firmware.out'
    $receipt = $artifact + '.jlink-build.json'
    $marker = Join-Path $root 'dependent-stage.txt'
    $followup = { param($proof) [IO.File]::WriteAllText($marker, $proof.artifact_sha256) }.GetNewClosure()
    [IO.File]::WriteAllText($artifact, 'old image')
    [IO.File]::SetLastWriteTimeUtc($artifact, [DateTime]::UtcNow.AddDays(-1))
    [IO.File]::WriteAllText($receipt, '{"build_succeeded":true}')
    $failure = Join-Path $root 'fail.ps1'
    [IO.File]::WriteAllText($failure, 'exit 7')
    Assert-Throws {
        & $build -FilePath 'powershell.exe' -ArgumentList @('-NoProfile', '-File', $failure) -ArtifactPath $artifact -OnSuccess $followup
    } 'Failed compiler did not stop dependent work'
    Assert-True (-not (Test-Path -LiteralPath $marker)) 'Dependent stage ran after failure'
    Assert-True (-not (Test-Path -LiteralPath $receipt)) 'Old success receipt survived a failed build'
    Assert-True ([IO.File]::ReadAllText($artifact) -eq 'old image') 'Old firmware was deleted or changed'

    $noop = Join-Path $root 'noop.ps1'
    [IO.File]::WriteAllText($noop, 'exit 0')
    Assert-Throws {
        & $build -FilePath 'powershell.exe' -ArgumentList @('-NoProfile', '-File', $noop) -ArtifactPath $artifact -OnSuccess $followup
    } 'A stale image was accepted after a no-op build'
    Assert-True (-not (Test-Path -LiteralPath $marker)) 'Dependent stage ran for stale image'
    Assert-Throws {
        & $build -FilePath 'powershell.exe' -ArgumentList @('-NoProfile', '-File', $noop) -ArtifactPath $artifact -ReceiptPath $artifact
    } 'Receipt was allowed to replace firmware'
    Assert-True ([IO.File]::ReadAllText($artifact) -eq 'old image') 'Receipt path validation damaged firmware'

    $success = Join-Path $root 'success.ps1'
    [IO.File]::WriteAllText($success, 'param([string]$Output); [IO.File]::WriteAllText($Output, "fresh image"); exit 0')
    $proof = & $build -FilePath 'powershell.exe' -ArgumentList @('-NoProfile', '-File', $success, '-Output', $artifact) -ArtifactPath $artifact -OnSuccess $followup
    Assert-True (Test-Path -LiteralPath $marker) 'Successful gated build did not invoke dependent work'
    Assert-True (Test-Path -LiteralPath $receipt) 'Successful build did not produce receipt'
    Assert-True ($proof.build_succeeded -eq $true) 'Build proof missing success'
    Assert-True ($proof.artifact_sha256 -eq (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLowerInvariant()) 'Receipt digest does not identify the output'
    . (Join-Path $PSScriptRoot 'release-common.ps1')
    $payload = @(Get-ReleasePayloadPaths)
    Assert-True ($payload -contains 'scripts/new-firmware-identity.ps1') 'Identity helper is absent from release payload'
    Assert-True ($payload -contains 'scripts/invoke-verified-build.ps1') 'Build gate is absent from release payload'
    Write-Host 'PASS: JLID generation, size/ASCII bounds, overwrite guard, failed/stale build blocking, receipt protection, fresh output and payload inclusion.'
}
finally { Remove-Item -LiteralPath $root -Recurse -Force }
'''

PREPARATION = '''## Preparation before test-firmware changes

Freeze the project's verified DLL path, exact file version and SHA-256 before
connecting. Use the existing project/session identity checks; do not silently
switch versions or hard-code one SEGGER version for every device. For a project
whose agreed baseline is 6.98a, retain that exact DLL throughout the test. Ordinary
reads succeeding does not attest to HSS compatibility.

Read `target.config_get` and its action-specific `readiness` first. A
`static_ready` value is configuration evidence, not successful live validation;
resolve `missing` fields and retain `pending_checks`. Flash/erase need loader RAM;
verify and ordinary reads do not acquire that requirement. Before HSS-dependent
firmware design, budget the combined payload and run offline `hss.plan` once the
ELF/selectors exist. The current payload limit is 40 bytes excluding the timestamp.
Do not start HSS just to discover its static limits.

For DWARF writes/HSS, the final ELF must retain `__jlink_mcp_identity` in loaded,
read-only Flash. Its bytes are `JLID`, format byte `1`, one-byte ASCII build-ID
length N, then N printable ASCII bytes; N is 1..58 and total size is at most 64.
Use `scripts/new-firmware-identity.ps1` from the complete package to generate the
array without a hand-maintained length. GCC/Clang section GC also needs the linker
KEEP rule stated by that generator. Change the build ID with firmware changes;
reusing a demonstration ID does not prove that two different builds match.
Raw-address HSS instead requires explicit type/range and declared readable RAM;
it must not be silently relabelled as DWARF evidence.

A failed compiler/linker result ends its dependency chain immediately: do not
prepare, copy, flash or test an old OUT as the new build. The packaged
`scripts/invoke-verified-build.ps1` can run the approved native build command,
require a fresh nonempty output, and invoke an explicitly supplied dependent step
only after success. Its receipt binds that output path and SHA-256, not arbitrary
future files. Recheck the hash before later use; independent MCP calls are not
magically gated by the receipt. Use an explicit rebuild/new output path when
incremental builds leave the previous artifact unchanged. Deliberate rollback to
an identified old image is a separate authorized task, not a failed-build fallback.

'''

INSTALL_ADDITION = '''
## 测试固件准备与失败阻断（修复分支，尚未发布）

先固定当前工程经过确认的 DLL 路径、精确版本和 SHA-256，再检查
`config_get.readiness`。`static_ready` 只代表静态配置，不代表目标连接、
固件强身份或 HSS 能力已经验证。`flash/erase` 缺少 `loader_ram` 时先补齐；
`verify` 和普通读取不应因此受阻。HSS 先规划再启动，所有选择项合计载荷
不得超过 40 字节（另加时间戳），不要通过真实启动来发现静态限制。

完整发布包增加两个离线工作流辅助脚本，不自动调用、不自动连接或烧录：

```powershell
# BuildId 应随固件改变；以下字符串只是格式示例，不应重复用于不同固件。
./scripts/new-firmware-identity.ps1 -BuildId 'project-build-unique-id' -OutputPath './identity.c'

# 替换为已经获准执行的实际原生构建程序、参数和输出文件。
./scripts/invoke-verified-build.ps1 -FilePath '<compiler.exe>' -ArgumentList @('<project>', '<rebuild-options>') -ArtifactPath '<firmware.out>'
```

身份块必须在最终 ELF 中保留并位于加载到 Flash 的只读区。IAR 使用
`__root`，GCC/Clang 的 section GC 需要链接脚本 `KEEP` 对应段；生成源码
不替代最终 ELF 检查。构建辅助脚本失败时不生成成功凭据，也不会调用
`-OnSuccess` 提供的依赖步骤；旧固件文件不会被删除。成功凭据记录输出路径
和 SHA-256，后续独立操作仍需核对文件没有改变。该脚本不是所有外部构建
命令的全局拦截器，调用者不能忽略失败后继续烧录。

Worker 失联诊断日志按探针/PID 保存于租约目录下 `diagnostics/`，每个文件
最多 32 KiB。它是有界、尽力保留的日志尾部，不证明最终原生调用或 DLL 崩溃。
HSS 的 `.start-journal` 仅记录已刷盘的启动意图边界；`aborted/unknown` 不能
解释为采样从未执行。历史 key 可查询终态，但不能隐式重新启动旧采集。
'''


def apply(root):
    root = Path(root)
    changed = []
    path = "plugins/jlink-mcp/skills/jlink-mcp/SKILL.md"
    text = read_base(root, path, "9116c75cc6d234a7866245608e828c3db4b542fd")
    text = replace_once(text, "## Route precisely\n", PREPARATION + "## Route precisely\n")
    text = replace_once(text, """persisted data. A single live value routes to `inspect`; repeated samples,
transitions, duration, or high-rate observation routes to `hss.plan` then `start`.
""", """persisted data. Choose the least invasive evidence that meets acceptance: a
latched result, counter, completion flag or bounded target-run result routes to
`inspect`, even when the test performed many internal transitions. Use `hss.plan`
then `start` only when continuous samples, ordering or transient timing evidence
is actually needed. Software latches do not by themselves prove pin waveforms.
""")
    text = replace_once(text, """  needs a new key.
""", """  needs a new key. Historical `status` can still use the original key after a
  Worker restart; this lookup never restarts acquisition. Preserve aborted/unknown
  outcomes and last recorded boundaries without inferring unobserved hardware results.
""")
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)

    path = "scripts/release-common.ps1"
    text = read_base(root, path, "f7acaf58a57c109de3f87c334486af501f68830d")
    text = replace_once(text, """        'scripts/install-codex-plugin.ps1', 'scripts/launch-jlink-mcp.ps1', 'scripts/release-common.ps1',
""", """        'scripts/install-codex-plugin.ps1', 'scripts/launch-jlink-mcp.ps1', 'scripts/release-common.ps1',
        'scripts/new-firmware-identity.ps1', 'scripts/invoke-verified-build.ps1',
""")
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)
    for path, content in [
        ("scripts/new-firmware-identity.ps1", IDENTITY),
        ("scripts/invoke-verified-build.ps1", BUILD),
        ("scripts/test-firmware-workflow.ps1", TESTS),
    ]:
        if (root / path).exists():
            raise RuntimeError(f"Refusing to overwrite {path}")
        (root / path).write_text(content, encoding="utf-8", newline="\n")
        changed.append(path)

    path = "INSTALL.md"
    text = read_base(root, path, "72daca63828d3c65b414860ee376c7c67c3a7169")
    text += INSTALL_ADDITION
    (root / path).write_text(text, encoding="utf-8", newline="\n")
    changed.append(path)
    return changed
