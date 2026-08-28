$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$mcpPath = Join-Path $repositoryRoot 'target\debug\jlink-mcp.exe'
$workerPath = Join-Path $repositoryRoot 'target\debug\jlink-worker.exe'
$dllPath = 'C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll'
$commanderPath = 'C:\Program Files (x86)\SEGGER\JLink\JLink.exe'
$targetRoot = 'D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP'
$outPath = Join-Path $targetRoot 'Appl\Output\Exe\T26_DCU_APP_NXP.out'
$protectedHeader = Join-Path $targetRoot 'Appl\Source\Appl\AppPwrMode\AppPwrMode.h'
$protectedDep = Join-Path $targetRoot 'Appl\T26_DCU_APP_NXP.dep'
$resumeScript = Join-Path $PSScriptRoot 't-p1-ses-resume.jlink'
$expectedDllHash = 'D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5'
$expectedCommanderHash = '0340130E7AD4F90EA8F8973C42A34A6508F0C5F6E988D532BB03DE9060FDFC04'
$expectedOutHash = '3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF'
$expectedHeaderHash = 'E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085'
$expectedDepHash = 'B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620'
$probeSerial = 260106173

function Assert-FileHash {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Expected,
        [Parameter(Mandatory)][string]$Description
    )

    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($actual -ne $Expected) {
        throw "$Description SHA-256 不匹配：$actual"
    }
}

function Invoke-McpRequest {
    param(
        [Parameter(Mandatory)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)][hashtable]$Request
    )

    $payload = $Request | ConvertTo-Json -Compress -Depth 20
    $Process.StandardInput.WriteLine($payload)
    $Process.StandardInput.Flush()
    $readTask = $Process.StandardOutput.ReadLineAsync()
    if (-not $readTask.Wait([TimeSpan]::FromSeconds(30))) {
        throw "MCP 请求超时：$($Request.method)"
    }
    $line = $readTask.Result
    if ([string]::IsNullOrWhiteSpace($line)) {
        throw "MCP 进程未返回响应：$($Request.method)"
    }
    $response = $line | ConvertFrom-Json -Depth 20
    if ($response.error) {
        throw "MCP 请求失败：$($response.error.code) $($response.error.message)"
    }
    if ($response.result -and $response.result.isError -eq $true) {
        $toolError = $response.result.structuredContent.error
        throw "MCP 工具错误：$($toolError.code) $($toolError.message)"
    }
    return $response
}

function Invoke-JLinkResume {
    & $commanderPath -NoGui 1 -Device S32K144 -If SWD -Speed 4000 -SelectEmuBySN $probeSerial -CommanderScript $resumeScript
    if ($LASTEXITCODE -ne 0) {
        throw 'J-Link Commander 安全恢复失败'
    }
}

Assert-FileHash -Path $dllPath -Expected $expectedDllHash -Description '冻结 J-Link 6.98a DLL'
Assert-FileHash -Path $commanderPath -Expected $expectedCommanderHash -Description '冻结 J-Link 6.98a Commander'
Assert-FileHash -Path $outPath -Expected $expectedOutHash -Description '冻结 IAR OUT'
Assert-FileHash -Path $protectedHeader -Expected $expectedHeaderHash -Description '受保护 AppPwrMode.h'
Assert-FileHash -Path $protectedDep -Expected $expectedDepHash -Description '受保护 T26_DCU_APP_NXP.dep'
if (Get-Process -Name 'jlink-worker', 'JLink' -ErrorAction SilentlyContinue) {
    throw '检测到既有 jlink-worker 或 JLink 进程，拒绝接管探针'
}

$svnStatusBefore = (& svn status -q $targetRoot) -join "`n"
if ($LASTEXITCODE -ne 0) {
    throw '无法读取测试工程 SVN 状态'
}

cargo build -p jlink-mcp -p jlink-worker
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
if (-not (Test-Path -LiteralPath $mcpPath) -or -not (Test-Path -LiteralPath $workerPath)) {
    throw '生产 MCP 或 Worker 二进制不存在'
}

$temporaryBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$temporaryRoot = [System.IO.Path]::GetFullPath(
    (Join-Path $temporaryBase ("jlink-mcp-p1-smoke-{0}" -f [guid]::NewGuid().ToString('N')))
)
if (-not $temporaryRoot.StartsWith($temporaryBase, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw '临时目录超出系统临时根目录'
}
$localAppData = Join-Path $temporaryRoot 'localappdata'
$null = New-Item -ItemType Directory -Path $localAppData

$process = $null
$testError = $null
$cleanupError = $null
$connectedState = $null
$targetRequiresRecovery = $false
try {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $mcpPath
    $startInfo.WorkingDirectory = $temporaryRoot
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardInput = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $true
    $startInfo.Environment['LOCALAPPDATA'] = $localAppData
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        throw '无法启动生产 jlink-mcp.exe'
    }

    $null = Invoke-McpRequest -Process $process -Request @{
        jsonrpc = '2.0'
        id = 1
        method = 'initialize'
        params = @{ protocolVersion = '2025-11-25' }
    }
    $null = Invoke-McpRequest -Process $process -Request @{
        jsonrpc = '2.0'
        id = 2
        method = 'tools/call'
        params = @{
            name = 'jlink_target'
            arguments = @{
                action = 'config_set'
                scope = 'project'
                values = @{
                    'target.device' = 'S32K144'
                    'target.interface' = 'swd'
                    'target.speed_khz' = 4000
                    'symbols.elf' = $outPath
                    'jlink.dll_path' = $dllPath
                    'jlink.dll_version' = '6.98a'
                    'jlink.dll_sha256' = $expectedDllHash
                    'capture.max_bytes' = 536870912
                }
            }
        }
    }
    $null = Invoke-McpRequest -Process $process -Request @{
        jsonrpc = '2.0'
        id = 3
        method = 'tools/call'
        params = @{
            name = 'jlink_target'
            arguments = @{
                action = 'config_set'
                scope = 'user'
                values = @{ 'probe.serial' = $probeSerial }
            }
        }
    }
    $targetRequiresRecovery = $true
    $connect = Invoke-McpRequest -Process $process -Request @{
        jsonrpc = '2.0'
        id = 4
        method = 'tools/call'
        params = @{ name = 'jlink_target'; arguments = @{ action = 'connect' } }
    }
    $status = Invoke-McpRequest -Process $process -Request @{
        jsonrpc = '2.0'
        id = 5
        method = 'tools/call'
        params = @{ name = 'jlink_target'; arguments = @{ action = 'status' } }
    }
    $connectedState = $status.result.structuredContent
    if ($connectedState.connection -ne 'connected' -or $connectedState.state -ne 'running') {
        throw "MCP 状态不符合预期：$($connectedState | ConvertTo-Json -Compress)"
    }
    $disconnect = Invoke-McpRequest -Process $process -Request @{
        jsonrpc = '2.0'
        id = 6
        method = 'tools/call'
        params = @{ name = 'jlink_target'; arguments = @{ action = 'disconnect' } }
    }
    $targetRequiresRecovery = $false
    if (($disconnect.result.structuredContent | ConvertTo-Json -Compress) -ne '{}') {
        throw 'disconnect 成功结果不是最小空对象'
    }
    $process.StandardInput.Close()
    if (-not $process.WaitForExit(5000) -or $process.ExitCode -ne 0) {
        throw '生产 jlink-mcp.exe 未正常退出'
    }
    Write-Output ("MCP_CONNECT={0}" -f ($connect.result.structuredContent | ConvertTo-Json -Compress))
    Write-Output ("MCP_STATUS={0}" -f ($connectedState | ConvertTo-Json -Compress))
    Write-Output ("MCP_DISCONNECT={0}" -f ($disconnect.result.structuredContent | ConvertTo-Json -Compress))
} catch {
    $testError = $_
} finally {
    if ($process -and -not $process.HasExited) {
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(1000)) {
            $process.Kill($true)
            $process.WaitForExit()
        }
    }
    Get-Process -Name 'jlink-worker' -ErrorAction SilentlyContinue | Stop-Process -Force
    if ($targetRequiresRecovery) {
        try {
            Invoke-JLinkResume
        } catch {
            $cleanupError = $_
        }
    }
    if (Test-Path -LiteralPath $temporaryRoot) {
        $resolvedTemporaryRoot = [System.IO.Path]::GetFullPath($temporaryRoot)
        if (-not $resolvedTemporaryRoot.StartsWith($temporaryBase, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw '拒绝删除未验证的临时目录'
        }
        Remove-Item -LiteralPath $resolvedTemporaryRoot -Recurse -Force
    }
}

if ($cleanupError) {
    throw "P1 smoke 安全恢复失败：$cleanupError"
}
if ($testError) {
    throw $testError
}
if (Get-Process -Name 'jlink-worker', 'JLink' -ErrorAction SilentlyContinue) {
    throw 'P1 smoke 结束后仍存在 jlink-worker 或 JLink 进程'
}

Assert-FileHash -Path $dllPath -Expected $expectedDllHash -Description 'P1 smoke 结束后 J-Link DLL'
Assert-FileHash -Path $commanderPath -Expected $expectedCommanderHash -Description 'P1 smoke 结束后 J-Link Commander'
Assert-FileHash -Path $outPath -Expected $expectedOutHash -Description 'P1 smoke 结束后 IAR OUT'
Assert-FileHash -Path $protectedHeader -Expected $expectedHeaderHash -Description 'P1 smoke 结束后 AppPwrMode.h'
Assert-FileHash -Path $protectedDep -Expected $expectedDepHash -Description 'P1 smoke 结束后 T26_DCU_APP_NXP.dep'
$svnStatusAfter = (& svn status -q $targetRoot) -join "`n"
if ($LASTEXITCODE -ne 0 -or $svnStatusAfter -ne $svnStatusBefore) {
    throw 'P1 smoke 前后 SVN 状态不一致'
}

Write-Output 'P1 connect→status→disconnect S32K144/SWD 4000 kHz 真机 smoke：PASS'
