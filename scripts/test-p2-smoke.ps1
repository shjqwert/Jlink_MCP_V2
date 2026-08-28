[CmdletBinding()]
param(
    [switch]$FlashDiagnosticOnly
)

$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$mcpPath = Join-Path $repositoryRoot 'target\debug\jlink-mcp.exe'
$workerPath = Join-Path $repositoryRoot 'target\debug\jlink-worker.exe'
$dllPath = 'C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll'
$commanderPath = 'C:\Program Files (x86)\SEGGER\JLink\JLink.exe'
$targetRoot = 'D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP'
$outPath = Join-Path $targetRoot 'Appl\Output\Exe\T26_DCU_APP_NXP.out'
$fixturePath = Join-Path $targetRoot 'Appl\Source\Appl\AppUserDesc\AppUserDesc.c'
$protectedHeader = Join-Path $targetRoot 'Appl\Source\Appl\AppPwrMode\AppPwrMode.h'
$protectedDependency = Join-Path $targetRoot 'Appl\T26_DCU_APP_NXP.dep'
$resumeScript = Join-Path $PSScriptRoot 't-p1-ses-resume.jlink'
$expectedDllHash = 'D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5'
$expectedCommanderHash = '0340130E7AD4F90EA8F8973C42A34A6508F0C5F6E988D532BB03DE9060FDFC04'
$expectedHeaderHash = 'E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085'
$probeSerial = 260106173
$writablePath = 'gulAppUserDescWritableTest'
$writableAddress = '0x1FFF8D68'
$originalValue = [uint64]0x12345678
$changedValue = [uint64]2309737967
$originalBytes = '78563412'
$changedBytes = 'efcdab89'

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
        [Parameter(Mandatory)][hashtable]$Request,
        [int]$TimeoutSeconds = 180
    )

    $payload = $Request | ConvertTo-Json -Compress -Depth 30
    $Process.StandardInput.WriteLine($payload)
    $Process.StandardInput.Flush()
    $readTask = $Process.StandardOutput.ReadLineAsync()
    if (-not $readTask.Wait([TimeSpan]::FromSeconds($TimeoutSeconds))) {
        throw "MCP 请求超时：$($Request.method)"
    }
    $line = $readTask.Result
    if ([string]::IsNullOrWhiteSpace($line)) {
        throw "MCP 进程未返回响应：$($Request.method)"
    }
    $response = $line | ConvertFrom-Json -Depth 30
    if ($response.error) {
        throw "MCP 请求失败：$($response.error.code) $($response.error.message)"
    }
    if ($response.result -and $response.result.isError -eq $true) {
        $toolError = $response.result.structuredContent.error
        $toolErrorJson = $toolError | ConvertTo-Json -Compress -Depth 30
        throw "MCP 工具错误：$toolErrorJson"
    }
    return $response
}

function Invoke-McpTool {
    param(
        [Parameter(Mandatory)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)][int]$Id,
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][hashtable]$Arguments
    )

    return Invoke-McpRequest -Process $Process -Request @{
        jsonrpc = '2.0'
        id = $Id
        method = 'tools/call'
        params = @{ name = $Name; arguments = $Arguments }
    }
}

function Assert-EmptyToolResult {
    param(
        [Parameter(Mandatory)]$Response,
        [Parameter(Mandatory)][string]$Description
    )

    $content = $Response.result.structuredContent | ConvertTo-Json -Compress -Depth 10
    if ($content -ne '{}') {
        throw "$Description 成功结果不是最小空对象：$content"
    }
}

function Invoke-JLinkResume {
    & $commanderPath -NoGui 1 -Device S32K144 -If SWD -Speed 4000 -SelectEmuBySN $probeSerial -CommanderScript $resumeScript
    if ($LASTEXITCODE -ne 0) {
        throw 'J-Link Commander 安全恢复失败'
    }
}

Assert-FileHash -Path $dllPath -Expected $expectedDllHash -Description '冻结 J-Link 6.98a DLL'
Assert-FileHash -Path $commanderPath -Expected $expectedCommanderHash -Description '冻结 J-Link 6.98a Commander'
Assert-FileHash -Path $protectedHeader -Expected $expectedHeaderHash -Description '受保护 AppPwrMode.h'
$outHashBefore = (Get-FileHash -LiteralPath $outPath -Algorithm SHA256).Hash
$fixtureHashBefore = (Get-FileHash -LiteralPath $fixturePath -Algorithm SHA256).Hash
$dependencyHashBefore = (Get-FileHash -LiteralPath $protectedDependency -Algorithm SHA256).Hash
if (Get-Process -Name 'jlink-mcp', 'jlink-worker', 'JLink' -ErrorAction SilentlyContinue) {
    throw '检测到既有 jlink-mcp、jlink-worker 或 JLink 进程，拒绝接管探针'
}

$svnStatusBefore = (& svn status $targetRoot) -join "`n"
if ($LASTEXITCODE -ne 0) {
    throw '无法读取测试工程 SVN 状态'
}

cargo test -p jlink-worker --lib session::tests::active_hss_rejects_programming_before_other_session_checks -- --exact
if ($LASTEXITCODE -ne 0) {
    throw 'P2 smoke 的 HSS/program 冲突观察失败'
}
cargo test -p jlink-worker --lib session::tests::active_hss_rejects_reads_but_accepts_validated_memory_writes -- --exact
if ($LASTEXITCODE -ne 0) {
    throw 'P2 smoke 的 HSS/debug/control 冲突观察失败'
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
    (Join-Path $temporaryBase ("jlink-mcp-p2-smoke-{0}" -f [guid]::NewGuid().ToString('N')))
)
if (-not $temporaryRoot.StartsWith($temporaryBase, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw '临时目录超出系统临时根目录'
}
$localAppData = Join-Path $temporaryRoot 'localappdata'
$null = New-Item -ItemType Directory -Path $localAppData

$process = $null
$testError = $null
$cleanupError = $null
$safetyNote = $null
$connected = $false
$targetRequiresRecovery = $false
$flashAttempted = $false
$flashCompleted = $false
$variableDirty = $false
$registerDirty = $false
$originalRegister = $null
$pcBefore = $null
$pcAfter = $null
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
    $projectConfig = Invoke-McpTool -Process $process -Id 2 -Name 'jlink_target' -Arguments @{
        action = 'config_set'
        scope = 'project'
        values = @{
            'target.device' = 'S32K144'
            'target.interface' = 'swd'
            'target.speed_khz' = 4000
            'symbols.elf' = $outPath
            'firmware.image' = $outPath
            'jlink.dll_path' = $dllPath
            'jlink.dll_version' = '6.98a'
            'jlink.dll_sha256' = $expectedDllHash
            'capture.max_bytes' = 536870912
        }
    }
    Assert-EmptyToolResult -Response $projectConfig -Description 'project config_set'
    $userConfig = Invoke-McpTool -Process $process -Id 3 -Name 'jlink_target' -Arguments @{
        action = 'config_set'
        scope = 'user'
        values = @{ 'probe.serial' = $probeSerial }
    }
    Assert-EmptyToolResult -Response $userConfig -Description 'user config_set'

    $targetRequiresRecovery = $true
    $connect = Invoke-McpTool -Process $process -Id 4 -Name 'jlink_target' -Arguments @{ action = 'connect' }
    $connected = $true

    $flashAttempted = $true
    $flash = Invoke-McpTool -Process $process -Id 5 -Name 'jlink_program' -Arguments @{
        action = 'flash'
        image = $outPath
        after = 'reset_halt'
    }
    $flashCompleted = $true
    Assert-EmptyToolResult -Response $flash -Description 'flash'
    if ($FlashDiagnosticOnly) {
        $disconnect = Invoke-McpTool -Process $process -Id 6 -Name 'jlink_target' -Arguments @{ action = 'disconnect' }
        Assert-EmptyToolResult -Response $disconnect -Description 'diagnostic disconnect'
        $connected = $false
        $targetRequiresRecovery = $false

        $process.StandardInput.Close()
        if (-not $process.WaitForExit(5000) -or $process.ExitCode -ne 0) {
            throw '生产 jlink-mcp.exe 未正常退出'
        }
        Write-Output ("MCP_CONNECT={0}" -f ($connect.result.structuredContent | ConvertTo-Json -Compress))
    } else {
        $verify = Invoke-McpTool -Process $process -Id 6 -Name 'jlink_program' -Arguments @{
        action = 'verify'
        image = $outPath
        }
        Assert-EmptyToolResult -Response $verify -Description 'verify'

    # flash 的 reset_halt 使手工 .data 初始化尚未执行；独立 verify 保持只读后再显式恢复运行。
    $resumeForDataInit = Invoke-McpTool -Process $process -Id 25 -Name 'jlink_control' -Arguments @{
        action = 'resume'
    }
    Assert-EmptyToolResult -Response $resumeForDataInit -Description 'resume for runtime data initialization'

    $variable = Invoke-McpTool -Process $process -Id 7 -Name 'jlink_inspect' -Arguments @{
        action = 'variable'
        path = $writablePath
    }
    if ([uint64]$variable.result.structuredContent.value -ne $originalValue) {
        throw "烧录后变量初值不符：$($variable.result.structuredContent.value)"
    }
    $memory = Invoke-McpTool -Process $process -Id 8 -Name 'jlink_inspect' -Arguments @{
        action = 'memory'
        address = $writableAddress
        length = 4
    }
    if ($memory.result.structuredContent.data -ne $originalBytes) {
        throw "变量地址的原始内存初值不符：$($memory.result.structuredContent.data)"
    }

    $variableDirty = $true
    $writeVariable = Invoke-McpTool -Process $process -Id 9 -Name 'jlink_write' -Arguments @{
        action = 'variable'
        path = $writablePath
        value = $changedValue
        verify = 'readback'
    }
    Assert-EmptyToolResult -Response $writeVariable -Description 'variable write'
    $changedMemory = Invoke-McpTool -Process $process -Id 10 -Name 'jlink_inspect' -Arguments @{
        action = 'memory'
        address = $writableAddress
        length = 4
    }
    if ($changedMemory.result.structuredContent.data -ne $changedBytes) {
        throw "变量写入后的原始内存不符：$($changedMemory.result.structuredContent.data)"
    }
    $restoreMemory = Invoke-McpTool -Process $process -Id 11 -Name 'jlink_write' -Arguments @{
        action = 'memory'
        address = $writableAddress
        data = $originalBytes
        verify = 'readback'
    }
    Assert-EmptyToolResult -Response $restoreMemory -Description 'memory restore'
    $restoredVariable = Invoke-McpTool -Process $process -Id 12 -Name 'jlink_inspect' -Arguments @{
        action = 'variable'
        path = $writablePath
    }
    if ([uint64]$restoredVariable.result.structuredContent.value -ne $originalValue) {
        throw "原始内存恢复后的变量值不符：$($restoredVariable.result.structuredContent.value)"
    }
    $variableDirty = $false

    $halt = Invoke-McpTool -Process $process -Id 13 -Name 'jlink_control' -Arguments @{ action = 'halt' }
    Assert-EmptyToolResult -Response $halt -Description 'halt'
    $pcBeforeResponse = Invoke-McpTool -Process $process -Id 14 -Name 'jlink_inspect' -Arguments @{
        action = 'register'
        name = 'PC'
    }
    $pcBefore = $pcBeforeResponse.result.structuredContent.value
    $registerResponse = Invoke-McpTool -Process $process -Id 15 -Name 'jlink_inspect' -Arguments @{
        action = 'register'
        name = 'R0'
    }
    $originalRegister = $registerResponse.result.structuredContent.value
    $originalRegisterValue = [Convert]::ToUInt32($originalRegister.Substring(2), 16)
    $changedRegisterValue = $originalRegisterValue -bxor 1
    $changedRegister = '0x{0:X8}' -f $changedRegisterValue

    $registerDirty = $true
    $writeRegister = Invoke-McpTool -Process $process -Id 16 -Name 'jlink_write' -Arguments @{
        action = 'register'
        name = 'R0'
        value = $changedRegister
    }
    Assert-EmptyToolResult -Response $writeRegister -Description 'register write'
    $changedRegisterResponse = Invoke-McpTool -Process $process -Id 17 -Name 'jlink_inspect' -Arguments @{
        action = 'register'
        name = 'R0'
    }
    if ($changedRegisterResponse.result.structuredContent.value -ne $changedRegister) {
        throw "R0 写入读回不符：$($changedRegisterResponse.result.structuredContent.value)"
    }
    $restoreRegister = Invoke-McpTool -Process $process -Id 18 -Name 'jlink_write' -Arguments @{
        action = 'register'
        name = 'R0'
        value = $originalRegister
    }
    Assert-EmptyToolResult -Response $restoreRegister -Description 'register restore'
    $restoredRegisterResponse = Invoke-McpTool -Process $process -Id 19 -Name 'jlink_inspect' -Arguments @{
        action = 'register'
        name = 'R0'
    }
    if ($restoredRegisterResponse.result.structuredContent.value -ne $originalRegister) {
        throw "R0 恢复读回不符：$($restoredRegisterResponse.result.structuredContent.value)"
    }
    $registerDirty = $false

    $step = Invoke-McpTool -Process $process -Id 20 -Name 'jlink_control' -Arguments @{ action = 'step' }
    Assert-EmptyToolResult -Response $step -Description 'step'
    $pcAfterResponse = Invoke-McpTool -Process $process -Id 21 -Name 'jlink_inspect' -Arguments @{
        action = 'register'
        name = 'PC'
    }
    $pcAfter = $pcAfterResponse.result.structuredContent.value
    if ($pcAfter -eq $pcBefore) {
        throw "单步后 PC 未变化：$pcBefore"
    }
    $resetRun = Invoke-McpTool -Process $process -Id 22 -Name 'jlink_control' -Arguments @{
        action = 'reset'
        after = 'run'
    }
    Assert-EmptyToolResult -Response $resetRun -Description 'reset_run'
    $status = Invoke-McpTool -Process $process -Id 23 -Name 'jlink_target' -Arguments @{ action = 'status' }
    if ($status.result.structuredContent.connection -ne 'connected' -or $status.result.structuredContent.state -ne 'running') {
        throw "P2 smoke 最终状态不符合预期：$($status.result.structuredContent | ConvertTo-Json -Compress)"
    }
    $disconnect = Invoke-McpTool -Process $process -Id 24 -Name 'jlink_target' -Arguments @{ action = 'disconnect' }
    Assert-EmptyToolResult -Response $disconnect -Description 'disconnect'
    $connected = $false
    $targetRequiresRecovery = $false

    $process.StandardInput.Close()
    if (-not $process.WaitForExit(5000) -or $process.ExitCode -ne 0) {
        throw '生产 jlink-mcp.exe 未正常退出'
    }
    Write-Output ("MCP_CONNECT={0}" -f ($connect.result.structuredContent | ConvertTo-Json -Compress))
    Write-Output ("VARIABLE_ROUND_TRIP={0}->{1}->{0}" -f $originalValue, $changedValue)
    Write-Output ("MEMORY_ROUND_TRIP={0}->{1}->{0}" -f $originalBytes, $changedBytes)
    Write-Output ("REGISTER_R0_ROUND_TRIP={0}->{1}->{0}" -f $originalRegister, $changedRegister)
    Write-Output ("PC_STEP={0}->{1}" -f $pcBefore, $pcAfter)
    Write-Output ("MCP_STATUS={0}" -f ($status.result.structuredContent | ConvertTo-Json -Compress))
    }
} catch {
    $testError = $_
} finally {
    $unsafeFlashFailure = $flashAttempted -and -not $flashCompleted
    if ($unsafeFlashFailure) {
        $safetyNote = 'Flash 结果不确定，按停机规则未继续执行 RAM 或寄存器操作；随后仅执行冻结 Commander CPU 安全恢复'
    } elseif ($process -and -not $process.HasExited -and $connected) {
        try {
            if ($variableDirty) {
                $restoreMemory = Invoke-McpTool -Process $process -Id 90 -Name 'jlink_write' -Arguments @{
                    action = 'memory'
                    address = $writableAddress
                    data = $originalBytes
                    verify = 'readback'
                }
                Assert-EmptyToolResult -Response $restoreMemory -Description 'failure cleanup memory restore'
                $variableDirty = $false
            }
            if ($registerDirty) {
                $restoreRegister = Invoke-McpTool -Process $process -Id 91 -Name 'jlink_write' -Arguments @{
                    action = 'register'
                    name = 'R0'
                    value = $originalRegister
                }
                Assert-EmptyToolResult -Response $restoreRegister -Description 'failure cleanup register restore'
                $registerDirty = $false
            }
            $resetRun = Invoke-McpTool -Process $process -Id 92 -Name 'jlink_control' -Arguments @{
                action = 'reset'
                after = 'run'
            }
            Assert-EmptyToolResult -Response $resetRun -Description 'failure cleanup reset_run'
            $disconnect = Invoke-McpTool -Process $process -Id 93 -Name 'jlink_target' -Arguments @{ action = 'disconnect' }
            Assert-EmptyToolResult -Response $disconnect -Description 'failure cleanup disconnect'
            $connected = $false
            $targetRequiresRecovery = $false
        } catch {
            $cleanupError = $_
        }
    }

    if ($process -and -not $process.HasExited) {
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(1000)) {
            $process.Kill($true)
            $process.WaitForExit()
        }
    }
    Get-Process -Name 'jlink-worker' -ErrorAction SilentlyContinue | Stop-Process -Force

    if ($targetRequiresRecovery -and -not $cleanupError -and -not $variableDirty -and -not $registerDirty) {
        try {
            Invoke-JLinkResume
            $targetRequiresRecovery = $false
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
    throw "P2 smoke 安全恢复失败：$cleanupError；原始错误：$testError"
}
if ($testError) {
    if ($safetyNote) {
        throw "$safetyNote；原始错误：$testError"
    }
    throw $testError
}
if (Get-Process -Name 'jlink-mcp', 'jlink-worker', 'JLink' -ErrorAction SilentlyContinue) {
    throw 'P2 smoke 结束后仍存在 jlink-mcp、jlink-worker 或 JLink 进程'
}

Assert-FileHash -Path $dllPath -Expected $expectedDllHash -Description 'P2 smoke 结束后 J-Link DLL'
Assert-FileHash -Path $commanderPath -Expected $expectedCommanderHash -Description 'P2 smoke 结束后 J-Link Commander'
Assert-FileHash -Path $outPath -Expected $outHashBefore -Description 'P2 smoke 当次 IAR OUT'
Assert-FileHash -Path $fixturePath -Expected $fixtureHashBefore -Description 'P2 smoke 当次 AppUserDesc.c'
Assert-FileHash -Path $protectedHeader -Expected $expectedHeaderHash -Description 'P2 smoke 结束后 AppPwrMode.h'
Assert-FileHash -Path $protectedDependency -Expected $dependencyHashBefore -Description 'P2 smoke 当次 T26_DCU_APP_NXP.dep'
$svnStatusAfter = (& svn status $targetRoot) -join "`n"
if ($LASTEXITCODE -ne 0 -or $svnStatusAfter -ne $svnStatusBefore) {
    throw 'P2 smoke 前后 SVN 状态不一致'
}

Write-Output ("P2_OUT_SHA256={0}" -f $outHashBefore)
Write-Output ("P2_FIXTURE_SHA256={0}" -f $fixtureHashBefore)
Write-Output ("P2_DEP_SHA256={0}" -f $dependencyHashBefore)

if ($FlashDiagnosticOnly) {
    Write-Output 'P2 connect→flash S32K144/SWD 4000 kHz 仅诊断运行：PASS'
} else {
    Write-Output 'P2 flash→verify→variable/memory→register/control S32K144/SWD 4000 kHz 真机 smoke：PASS'
}
