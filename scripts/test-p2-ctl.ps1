$ErrorActionPreference = 'Stop'

$dllPath = 'C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll'
$commanderPath = 'C:\Program Files (x86)\SEGGER\JLink\JLink.exe'
$expectedDllHash = 'D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5'
$targetRoot = 'D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP'
$protectedHeader = Join-Path $targetRoot 'Appl\Source\Appl\AppPwrMode\AppPwrMode.h'
$protectedDependency = Join-Path $targetRoot 'Appl\T26_DCU_APP_NXP.dep'
$expectedHeaderHash = 'E67117E5E240E21EAE55F11E943D95ECE50528ECB5C04B65E9FFF89CE99F9085'
$expectedDependencyHash = 'B73FCCA00DADB12D639B65B60FD6B44F60295D43301536333373448D5C00D620'
$resumeScript = Join-Path $PSScriptRoot 't-p1-ses-resume.jlink'

if ((Get-FileHash -LiteralPath $dllPath -Algorithm SHA256).Hash -ne $expectedDllHash) {
    throw '冻结 J-Link 6.98a DLL 身份不匹配'
}
if ((Get-FileHash -LiteralPath $protectedHeader -Algorithm SHA256).Hash -ne $expectedHeaderHash) {
    throw '受保护 AppPwrMode.h 哈希与前置条件不一致'
}
if ((Get-FileHash -LiteralPath $protectedDependency -Algorithm SHA256).Hash -ne $expectedDependencyHash) {
    throw '受保护 T26_DCU_APP_NXP.dep 哈希与前置条件不一致'
}
if (-not (Test-Path -LiteralPath $commanderPath)) {
    throw 'J-Link 6.98a Commander 不存在，无法执行失败安全恢复'
}
if (Get-Process -Name 'jlink-worker' -ErrorAction SilentlyContinue) {
    throw '检测到既有 jlink-worker 进程，拒绝接管探针'
}

$svnStatusBefore = svn status $targetRoot
$testError = $null
try {
    cargo test -p jlink-domain --test t_p2_ctl
    if ($LASTEXITCODE -ne 0) {
        throw 'T-P2-CTL 领域测试失败'
    }
    cargo test -p jlink-mcp --test t_p2_ctl
    if ($LASTEXITCODE -ne 0) {
        throw 'T-P2-CTL MCP 合同测试失败'
    }
    cargo test -p jlink-worker --lib runtime::tests::register_and_control_commands_require_exact_payloads -- --exact
    if ($LASTEXITCODE -ne 0) {
        throw 'T-P2-CTL IPC 负载测试失败'
    }
    cargo test -p jlink-worker --lib session::tests::active_hss_rejects_reads_but_accepts_validated_memory_writes -- --exact
    if ($LASTEXITCODE -ne 0) {
        throw 'T-P2-CTL HSS 冲突测试失败'
    }

    $env:JLINK_MCP_T_P2_CTL_DLL = $dllPath
    $env:JLINK_MCP_T_P2_CTL_DEVICE = 'S32K144'
    $env:JLINK_MCP_T_P2_CTL_PROBE = '260106173'
    cargo test -p jlink-worker gateway::tests::hardware_core_register_and_control_round_trip -- --ignored --exact --nocapture
    if ($LASTEXITCODE -ne 0) {
        throw 'T-P2-CTL 真机寄存器与控制测试失败'
    }
} catch {
    $testError = $_
}

$cleanupError = $null
try {
    & $commanderPath -NoGui 1 -Device S32K144 -If SWD -Speed 4000 -SelectEmuBySN 260106173 -CommanderScript $resumeScript
    if ($LASTEXITCODE -ne 0) {
        throw 'J-Link Commander 安全恢复失败'
    }
} catch {
    $cleanupError = $_
}

$svnStatusAfter = svn status $targetRoot
if (($svnStatusBefore -join "`n") -ne ($svnStatusAfter -join "`n")) {
    throw 'T-P2-CTL 执行期间 SVN 工作副本状态发生变化'
}
if ((Get-FileHash -LiteralPath $protectedHeader -Algorithm SHA256).Hash -ne $expectedHeaderHash) {
    throw 'T-P2-CTL 执行后受保护 AppPwrMode.h 哈希发生变化'
}
if ((Get-FileHash -LiteralPath $protectedDependency -Algorithm SHA256).Hash -ne $expectedDependencyHash) {
    throw 'T-P2-CTL 执行后受保护 T26_DCU_APP_NXP.dep 哈希发生变化'
}
if ($cleanupError) {
    throw "T-P2-CTL 安全恢复失败：$cleanupError"
}
if ($testError) {
    throw $testError
}

Write-Output 'T-P2-CTL S32K144/SWD 4000 kHz 寄存器与控制测试：PASS'
