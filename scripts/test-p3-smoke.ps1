[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$mcpPath = Join-Path $repositoryRoot 'target\debug\jlink-mcp.exe'
$workerPath = Join-Path $repositoryRoot 'target\debug\jlink-worker.exe'
$captureInspectorPath = Join-Path $repositoryRoot 'target\debug\examples\t_p3_capture.exe'
$evidenceRoot = Join-Path $repositoryRoot 'target\evidence\p3-4.8'
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
$expectedOutHash = 'F8ADB9A2B9BBFD26B469C66F2478EE6E22735302706B83509B2D4F2AE7F7738D'
$expectedFixtureHash = '1133B85709AB5ED3509ED58433ED4132E4D0869724140F8D3F560F7BA3B709E4'
$expectedHeaderHash = 'E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085'
$expectedDependencyHash = '4FDA4431B3502EBDB1B0313BF58B21995A2B962C9C0BA853DF42F3988B4A6F85'
$expectedSvnStatusHash = '6827FD361AB388ABB26A6648158B0417CDDB76FAC515F91472C06B5715794685'

$probeSerial = 260106173
$captureKey = 'p3-4.8-300s-1khz'
$writableAddress = '0x1FFF8D68'
$originalBytes = '78563412'
$changedBytes = 'efcdab89'
$activeStates = @('starting', 'running', 'stopping')

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

function Get-SvnStatusHash {
    param([Parameter(Mandatory)][string]$Path)

    $lines = @(& svn status $Path)
    if ($LASTEXITCODE -ne 0) {
        throw '无法读取测试工程 SVN 状态'
    }
    $text = $lines -join "`n"
    $digest = [System.Security.Cryptography.SHA256]::HashData(
        [System.Text.Encoding]::UTF8.GetBytes($text)
    )
    return [PSCustomObject]@{
        Hash = [Convert]::ToHexString($digest)
        Lines = $lines.Count
        Untracked = @($lines | Where-Object { $_.StartsWith('?') }).Count
        Modified = @($lines | Where-Object { $_.StartsWith('M') }).Count
    }
}

function Start-McpProcess {
    param(
        [Parameter(Mandatory)][string]$WorkingDirectory,
        [Parameter(Mandatory)][string]$LocalAppData
    )

    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $mcpPath
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardInput = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $true
    $startInfo.Environment['LOCALAPPDATA'] = $LocalAppData
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        throw '无法启动生产 jlink-mcp.exe'
    }
    return $process
}

function Invoke-McpRequest {
    param(
        [Parameter(Mandatory)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)][hashtable]$Request,
        [int]$TimeoutSeconds = 30
    )

    if ($Process.HasExited) {
        throw "MCP 进程已退出：$($Process.ExitCode)"
    }
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
    if ($response.PSObject.Properties['error']) {
        throw "MCP 请求失败：$($response.error.code) $($response.error.message)"
    }
    return $response
}

function Initialize-Mcp {
    param(
        [Parameter(Mandatory)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)][int]$Id
    )

    $null = Invoke-McpRequest -Process $Process -Request @{
        jsonrpc = '2.0'
        id = $Id
        method = 'initialize'
        params = @{ protocolVersion = '2025-11-25' }
    }
}

function Invoke-McpToolRaw {
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

function Invoke-McpTool {
    param(
        [Parameter(Mandatory)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)][int]$Id,
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][hashtable]$Arguments
    )

    $response = Invoke-McpToolRaw -Process $Process -Id $Id -Name $Name -Arguments $Arguments
    $isError = $response.result.PSObject.Properties['isError']
    if ($isError -and $isError.Value -eq $true) {
        $toolError = $response.result.structuredContent.error | ConvertTo-Json -Compress -Depth 20
        throw "MCP 工具错误：$toolError"
    }
    return $response
}

function Assert-ToolError {
    param(
        [Parameter(Mandatory)]$Response,
        [Parameter(Mandatory)][string]$ExpectedCode,
        [Parameter(Mandatory)][string]$Description
    )

    $isError = $Response.result.PSObject.Properties['isError']
    if (-not $isError -or $isError.Value -ne $true) {
        throw "$Description 未返回预期错误"
    }
    $actualCode = $Response.result.structuredContent.error.code
    if ($actualCode -ne $ExpectedCode) {
        throw "$Description 错误码不符：$actualCode"
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

function Wait-HssTerminal {
    param(
        [Parameter(Mandatory)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory)][string]$CaptureKey,
        [Parameter(Mandatory)][int]$FirstId,
        [int]$TimeoutSeconds = 330
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $nextProgress = [DateTime]::UtcNow.AddSeconds(30)
    $requestId = $FirstId
    do {
        $response = Invoke-McpTool -Process $Process -Id $requestId -Name 'jlink_hss' -Arguments @{
            action = 'status'
            capture_key = $CaptureKey
        }
        $status = $response.result.structuredContent
        if ($activeStates -notcontains $status.state) {
            return $status
        }
        if ([DateTime]::UtcNow -ge $deadline) {
            throw "HSS 未在 $TimeoutSeconds 秒内进入终态：$($status.state)"
        }
        if ([DateTime]::UtcNow -ge $nextProgress) {
            Write-Host ("HSS_PROGRESS state={0} elapsed_us={1}" -f $status.state, $status.elapsed_us)
            $nextProgress = [DateTime]::UtcNow.AddSeconds(30)
        }
        $requestId++
        Start-Sleep -Seconds 2
    } while ($true)
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
Assert-FileHash -Path $fixturePath -Expected $expectedFixtureHash -Description '计划内 AppUserDesc.c 测试夹具'
Assert-FileHash -Path $protectedHeader -Expected $expectedHeaderHash -Description '受保护 AppPwrMode.h'
Assert-FileHash -Path $protectedDependency -Expected $expectedDependencyHash -Description '受保护 T26_DCU_APP_NXP.dep'

foreach ($requiredPath in @($mcpPath, $workerPath, $captureInspectorPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
        throw "阶段验收二进制不存在：$requiredPath"
    }
}
if (Get-Process -Name 'jlink-mcp', 'jlink-worker', 'JLink' -ErrorAction SilentlyContinue) {
    throw '检测到既有 jlink-mcp、jlink-worker 或 JLink 进程，拒绝接管探针'
}

$temporaryBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$temporaryRoot = [System.IO.Path]::GetFullPath(
    (Join-Path $temporaryBase ("jlink-mcp-p3-smoke-{0}" -f [guid]::NewGuid().ToString('N')))
)
if (-not $temporaryRoot.StartsWith($temporaryBase, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw '临时目录超出系统临时根目录'
}
$localAppData = Join-Path $temporaryRoot 'localappdata'
$null = New-Item -ItemType Directory -Path $localAppData

$ownerProcess = $null
$testError = $null
$cleanupError = $null
$captureId = $null
$captureStarted = $false
$captureTerminal = $false
$variableDirty = $false
$safeStateConfirmed = $false

try {
    $ownerProcess = Start-McpProcess -WorkingDirectory $temporaryRoot -LocalAppData $localAppData
    Initialize-Mcp -Process $ownerProcess -Id 1

    $projectConfig = Invoke-McpTool -Process $ownerProcess -Id 2 -Name 'jlink_target' -Arguments @{
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
    $userConfig = Invoke-McpTool -Process $ownerProcess -Id 3 -Name 'jlink_target' -Arguments @{
        action = 'config_set'
        scope = 'user'
        values = @{ 'probe.serial' = $probeSerial }
    }
    Assert-EmptyToolResult -Response $userConfig -Description 'user config_set'

    $null = Invoke-McpTool -Process $ownerProcess -Id 4 -Name 'jlink_target' -Arguments @{ action = 'connect' }
    $connectStatus = Invoke-McpTool -Process $ownerProcess -Id 5 -Name 'jlink_target' -Arguments @{ action = 'status' }
    if ($connectStatus.result.structuredContent.connection -ne 'connected' -or
        $connectStatus.result.structuredContent.state -ne 'running') {
        throw "HSS 前连接状态不是 connected/running：$($connectStatus.result.structuredContent | ConvertTo-Json -Compress)"
    }
    $initialMemory = Invoke-McpTool -Process $ownerProcess -Id 6 -Name 'jlink_inspect' -Arguments @{
        action = 'memory'
        address = $writableAddress
        length = 4
    }
    if ($initialMemory.result.structuredContent.data -ne $originalBytes) {
        throw "HSS 前测试变量原值不符：$($initialMemory.result.structuredContent.data)"
    }

    $start = Invoke-McpTool -Process $ownerProcess -Id 7 -Name 'jlink_hss' -Arguments @{
        action = 'start'
        capture_key = $captureKey
        duration_s = 300
        rate_hz = 1000
        variables = @(
            @{
                path = 'gaulAppUserDescHssTest'
                slice = @{ start = 0; count = 10 }
            }
        )
        return_when = 'started'
        rules = @()
    }
    if ($start.result.structuredContent.state -ne 'running') {
        throw "HSS Start 未返回 running：$($start.result.structuredContent | ConvertTo-Json -Compress)"
    }
    $captureId = [string]$start.result.structuredContent.capture_id
    $captureStarted = $true

    $workers = @(Get-Process -Name 'jlink-worker' -ErrorAction Stop)
    if ($workers.Count -ne 1) {
        throw "HSS Start 后 Worker 数量不是 1：$($workers.Count)"
    }

    $conflict = Invoke-McpToolRaw -Process $ownerProcess -Id 102 -Name 'jlink_inspect' -Arguments @{
        action = 'memory'
        address = $writableAddress
        length = 4
    }
    Assert-ToolError -Response $conflict -ExpectedCode 'OPERATION_CONFLICT' -Description '活动 HSS 读取冲突路由'

    $variableDirty = $true
    $changedWrite = Invoke-McpTool -Process $ownerProcess -Id 103 -Name 'jlink_write' -Arguments @{
        action = 'memory'
        address = $writableAddress
        data = $changedBytes
        verify = 'readback'
    }
    Assert-EmptyToolResult -Response $changedWrite -Description 'HSS 交错写入'
    $restoredWrite = Invoke-McpTool -Process $ownerProcess -Id 104 -Name 'jlink_write' -Arguments @{
        action = 'memory'
        address = $writableAddress
        data = $originalBytes
        verify = 'readback'
    }
    Assert-EmptyToolResult -Response $restoredWrite -Description 'HSS 交错写入恢复'
    $variableDirty = $false

    $terminal = Wait-HssTerminal -Process $ownerProcess -CaptureKey $captureKey -FirstId 200
    $captureTerminal = $true
    if ($terminal.state -ne 'completed') {
        throw "HSS 未正常完成：$($terminal | ConvertTo-Json -Compress -Depth 20)"
    }
    if ([uint64]$terminal.elapsed_us -lt 300000000) {
        throw "HSS 提前结束：$($terminal.elapsed_us) us"
    }
    if ([uint64]$terminal.quality.expected_samples -ne 300000 -or [uint64]$terminal.quality.actual_samples -eq 0) {
        throw "HSS 样本质量事实无效：$($terminal.quality | ConvertTo-Json -Compress -Depth 20)"
    }
    if ($terminal.quality.loss.evidence -notin @('confirmed', 'suspected', 'unknown') -or
        $terminal.quality.overflow.evidence -notin @('confirmed', 'suspected', 'unknown')) {
        throw 'HSS loss/overflow 未使用冻结证据等级'
    }
    if ($terminal.quality.clock.source_unit -ne 'milliseconds' -or
        [uint32]$terminal.quality.clock.source_frequency_hz -ne 1000 -or
        [uint32]$terminal.quality.clock.source_resolution_us -ne 1000 -or
        $terminal.quality.clock.normalized_unit -ne 'microseconds') {
        throw "HSS 6.98a 时间单位证据不符：$($terminal.quality.clock | ConvertTo-Json -Compress)"
    }

    $finalConnectStatus = Invoke-McpTool -Process $ownerProcess -Id 601 -Name 'jlink_target' -Arguments @{ action = 'status' }
    if ($finalConnectStatus.result.structuredContent.connection -ne 'connected' -or
        $finalConnectStatus.result.structuredContent.state -ne 'running') {
        throw "HSS 后目标未恢复 connected/running：$($finalConnectStatus.result.structuredContent | ConvertTo-Json -Compress)"
    }
    $restoredMemory = Invoke-McpTool -Process $ownerProcess -Id 602 -Name 'jlink_inspect' -Arguments @{
        action = 'memory'
        address = $writableAddress
        length = 4
    }
    if ($restoredMemory.result.structuredContent.data -ne $originalBytes) {
        throw "HSS 后测试变量未恢复：$($restoredMemory.result.structuredContent.data)"
    }
    $finalStatus = Invoke-McpTool -Process $ownerProcess -Id 603 -Name 'jlink_target' -Arguments @{ action = 'status' }
    if ($finalStatus.result.structuredContent.connection -ne 'connected' -or
        $finalStatus.result.structuredContent.state -ne 'running') {
        throw "HSS 后 CPU 状态不安全：$($finalStatus.result.structuredContent | ConvertTo-Json -Compress)"
    }
    $disconnect = Invoke-McpTool -Process $ownerProcess -Id 604 -Name 'jlink_target' -Arguments @{ action = 'disconnect' }
    Assert-EmptyToolResult -Response $disconnect -Description 'P3 disconnect'
    $safeStateConfirmed = $true

    $ownerProcess.StandardInput.Close()
    if (-not $ownerProcess.WaitForExit(5000) -or $ownerProcess.ExitCode -ne 0) {
        throw '当前 MCP 未在 disconnect 后正常退出'
    }
    Start-Sleep -Milliseconds 250
    if (Get-Process -Name 'jlink-mcp', 'jlink-worker', 'JLink' -ErrorAction SilentlyContinue) {
        throw 'P3 disconnect 后仍存在 J-Link 相关进程'
    }

    $captureRoot = Join-Path $localAppData 'jlink-mcp\leases\captures'
    $captureFiles = @(Get-ChildItem -LiteralPath $captureRoot -Recurse -File -Filter "capture-$captureId.capture")
    if ($captureFiles.Count -ne 1) {
        throw "完成 Capture Store 文件数量不为 1：$($captureFiles.Count)"
    }
    $captureJson = & $captureInspectorPath $captureFiles[0].Directory.FullName $captureId
    if ($LASTEXITCODE -ne 0) {
        throw 'Capture Store 只读校验器失败'
    }
    $capture = $captureJson | ConvertFrom-Json -Depth 40
    if ($capture.capture_key -ne $captureKey -or $capture.capture_id -ne $captureId) {
        throw 'Capture Store 的 key/id 身份不一致'
    }
    if ($capture.target.device -ne 'S32K144' -or $capture.target.interface -ne 'swd' -or
        [uint32]$capture.target.speed_khz -ne 4000 -or [uint32]$capture.target.probe_serial -ne $probeSerial) {
        throw "Capture Store 的目标连接身份不符：$($capture.target | ConvertTo-Json -Compress)"
    }
    if ([uint32]$capture.duration_s -ne 300 -or [uint32]$capture.rate_hz -ne 1000 -or
        [uint32]$capture.selector_count -ne 1 -or [uint32]$capture.sample_bytes -ne 40 -or
        [uint32]$capture.record_bytes -ne 44) {
        throw "Capture Store 的 10x32-bit/1kHz/300s 计划不符：$($capture | ConvertTo-Json -Compress -Depth 10)"
    }
    if ($capture.status.state -ne 'completed' -or [uint64]$capture.status.complete_records -eq 0 -or
        [uint64]$capture.status.drain.calls -eq 0) {
        throw "Capture Store 终态或排空证据无效：$($capture.status | ConvertTo-Json -Compress -Depth 20)"
    }
    if ([uint64]$capture.payload_bytes -ne ([uint64]$capture.status.complete_records * 44)) {
        throw 'Capture Store payload_bytes 与完整记录计数不一致'
    }
    if ($capture.status.writes.Count -ne 2) {
        throw "Capture Store 未保留两次交错写入：$($capture.status.writes.Count)"
    }
    foreach ($write in $capture.status.writes) {
        $nextDrain = $write.PSObject.Properties['samples_after_next_drain']
        if ($write.result.state -ne 'succeeded' -or -not $nextDrain) {
            throw "交错写入结果或下一次排空证据无效：$($write | ConvertTo-Json -Compress)"
        }
    }
    if ([uint64]$capture.status.quality.actual_samples -ne [uint64]$capture.status.complete_records) {
        throw 'Capture Store 质量样本计数与完整记录计数不一致'
    }

    $null = New-Item -ItemType Directory -Path $evidenceRoot -Force
    $evidencePath = Join-Path $evidenceRoot $captureFiles[0].Name
    if (Test-Path -LiteralPath $evidencePath) {
        throw "P3 阶段证据已存在，拒绝覆盖：$evidencePath"
    }
    Copy-Item -LiteralPath $captureFiles[0].FullName -Destination $evidencePath
    $evidenceFileHash = (Get-FileHash -LiteralPath $evidencePath -Algorithm SHA256).Hash
    $actualRateProperty = $capture.status.quality.PSObject.Properties['actual_rate_millihz']
    $actualRateMillihz = if ($actualRateProperty) { $actualRateProperty.Value } else { 'omitted' }

    $svn = Get-SvnStatusHash -Path $targetRoot
    if ($svn.Hash -ne $expectedSvnStatusHash -or $svn.Lines -ne 612 -or
        $svn.Untracked -ne 609 -or $svn.Modified -ne 3) {
        throw "P3 阶段结束 SVN 状态偏离基线：$($svn | ConvertTo-Json -Compress)"
    }
    Assert-FileHash -Path $outPath -Expected $expectedOutHash -Description 'P3 结束后 IAR OUT'
    Assert-FileHash -Path $fixturePath -Expected $expectedFixtureHash -Description 'P3 结束后 AppUserDesc.c'
    Assert-FileHash -Path $protectedHeader -Expected $expectedHeaderHash -Description 'P3 结束后 AppPwrMode.h'
    Assert-FileHash -Path $protectedDependency -Expected $expectedDependencyHash -Description 'P3 结束后 T26_DCU_APP_NXP.dep'

    Write-Output ("P3_CAPTURE_ID={0}" -f $captureId)
    Write-Output ("P3_COMPLETE_RECORDS={0}" -f $capture.status.complete_records)
    Write-Output ("P3_PAYLOAD_BYTES={0}" -f $capture.payload_bytes)
    Write-Output ("P3_RAW_SHA256={0}" -f $capture.raw_sha256)
    Write-Output ("P3_CAPTURE_RESOURCE={0}" -f $evidencePath)
    Write-Output ("P3_CAPTURE_FILE_SHA256={0}" -f $evidenceFileHash)
    Write-Output ("P3_LOSS={0}/{1}" -f $capture.status.quality.loss.evidence, $capture.status.quality.loss.basis)
    Write-Output ("P3_OVERFLOW={0}/{1}" -f $capture.status.quality.overflow.evidence, $capture.status.quality.overflow.basis)
    Write-Output ("P3_ACTUAL_RATE_MILLIHZ={0}" -f $actualRateMillihz)
    Write-Output ("P3_SVN_STATUS={0}/{1}/{2}/{3}" -f $svn.Hash, $svn.Lines, $svn.Untracked, $svn.Modified)
} catch {
    $testError = $_
} finally {
    if ($testError -and $ownerProcess -and -not $ownerProcess.HasExited) {
        try {
            if ($variableDirty) {
                $restore = Invoke-McpTool -Process $ownerProcess -Id 900 -Name 'jlink_write' -Arguments @{
                    action = 'memory'
                    address = $writableAddress
                    data = $originalBytes
                    verify = 'readback'
                }
                Assert-EmptyToolResult -Response $restore -Description '失败清理测试变量恢复'
                $variableDirty = $false
            }
            if ($captureStarted -and -not $captureTerminal) {
                $cleanupTerminal = Wait-HssTerminal -Process $ownerProcess -CaptureKey $captureKey -FirstId 910
                if ($cleanupTerminal.state -notin @('completed', 'failed', 'aborted')) {
                    throw "失败清理未取得 HSS 终态：$($cleanupTerminal.state)"
                }
                $captureTerminal = $true
            }
            if ($captureTerminal) {
                $resetRun = Invoke-McpTool -Process $ownerProcess -Id 1201 -Name 'jlink_control' -Arguments @{
                    action = 'reset'
                    after = 'run'
                }
                Assert-EmptyToolResult -Response $resetRun -Description '失败清理 reset_run'
                $disconnect = Invoke-McpTool -Process $ownerProcess -Id 1202 -Name 'jlink_target' -Arguments @{ action = 'disconnect' }
                Assert-EmptyToolResult -Response $disconnect -Description '失败清理 disconnect'
                $safeStateConfirmed = $true
            }
        } catch {
            $cleanupError = $_
        }
    }

    foreach ($process in @($ownerProcess)) {
        if ($process -and -not $process.HasExited) {
            try {
                $process.StandardInput.Close()
                if (-not $process.WaitForExit(2000)) {
                    $process.Kill()
                    $process.WaitForExit()
                }
            } catch {
                if (-not $cleanupError) {
                    $cleanupError = $_
                }
            }
        }
    }

    if ($testError -and $captureStarted -and $captureTerminal -and -not $safeStateConfirmed -and -not $cleanupError) {
        try {
            $workerExitDeadline = [DateTime]::UtcNow.AddSeconds(5)
            while ((Get-Process -Name 'jlink-worker' -ErrorAction SilentlyContinue) -and
                [DateTime]::UtcNow -lt $workerExitDeadline) {
                Start-Sleep -Milliseconds 100
            }
            if (Get-Process -Name 'jlink-worker' -ErrorAction SilentlyContinue) {
                throw 'HSS 已终止但 Worker 未释放探针，拒绝执行 Commander 恢复'
            }
            Invoke-JLinkResume
            $safeStateConfirmed = $true
        } catch {
            $cleanupError = $_
        }
    }

    if ((-not $captureStarted -or $captureTerminal) -and (Test-Path -LiteralPath $temporaryRoot)) {
        $resolvedTemporaryRoot = [System.IO.Path]::GetFullPath($temporaryRoot)
        if (-not $resolvedTemporaryRoot.StartsWith($temporaryBase, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw '拒绝删除未验证的临时目录'
        }
        Remove-Item -LiteralPath $resolvedTemporaryRoot -Recurse -Force
    }
}

if ($cleanupError) {
    throw "P3 smoke 安全恢复失败：$cleanupError；原始错误：$testError"
}
if ($testError) {
    throw $testError
}
if (-not $safeStateConfirmed) {
    throw 'P3 smoke 未确认 CPU 安全运行状态'
}

Write-Output 'P3 10x32-bit/1kHz/300s HSS、单一 MCP 生命周期、交错写入、尾排空和安全恢复：PASS'
