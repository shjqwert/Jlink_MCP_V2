$ErrorActionPreference = 'Stop'

$dllPath = 'C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll'
$commanderPath = 'C:\Program Files (x86)\SEGGER\JLink\JLink.exe'
$outPath = 'D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP\Appl\Output\Exe\T26_DCU_APP_NXP.out'
$expectedDllHash = 'D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5'
$expectedOutHash = '3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF'
$haltScript = Join-Path $PSScriptRoot 't-p1-ses-halt.jlink'
$resumeScript = Join-Path $PSScriptRoot 't-p1-ses-resume.jlink'

if ((Get-FileHash -LiteralPath $dllPath -Algorithm SHA256).Hash -ne $expectedDllHash) {
    throw '冻结 J-Link 6.98a DLL 身份不匹配'
}
if ((Get-FileHash -LiteralPath $outPath -Algorithm SHA256).Hash -ne $expectedOutHash) {
    throw '冻结 IAR OUT 身份不匹配'
}
if (-not (Test-Path -LiteralPath $commanderPath)) {
    throw 'J-Link 6.98a Commander 不存在'
}
if (Get-Process -Name 'jlink-worker' -ErrorAction SilentlyContinue) {
    throw '检测到既有 jlink-worker 进程，拒绝接管探针'
}

cargo test -p jlink-domain --test t_p1_ses
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
cargo test -p jlink-worker session::tests
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
cargo build -p jlink-worker
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

function Invoke-JLinkCommander {
    param([Parameter(Mandatory)][string]$ScriptPath)

    & $commanderPath -NoGui 1 -Device S32K144 -If SWD -Speed 4000 -SelectEmuBySN 260106173 -CommanderScript $ScriptPath
    if ($LASTEXITCODE -ne 0) {
        throw "J-Link Commander 执行失败：$ScriptPath"
    }
}

$testError = $null
try {
    Invoke-JLinkCommander -ScriptPath $haltScript
    $workerPath = Join-Path $PSScriptRoot '..\target\debug\jlink-worker.exe'
    cargo run -p jlink-mcp --example t_p1_ses -- $workerPath halted
    if ($LASTEXITCODE -ne 0) {
        throw 'T-P1-SES halted 恢复测试失败'
    }

    $env:JLINK_MCP_T_P1_SES_DLL = $dllPath
    $env:JLINK_MCP_T_P1_SES_PROBE_SERIAL = '260106173'
    $env:JLINK_MCP_T_P1_SES_SPEED_KHZ = '4000'
    $env:JLINK_MCP_T_P1_SES_DEVICE = 'S32K144'
    $env:JLINK_MCP_T_P1_SES_ELF_SHA256 = $expectedOutHash
    cargo test -p jlink-worker session::tests::hardware_hardfault_recovery_uses_same_gateway_session -- --ignored --exact --nocapture
    if ($LASTEXITCODE -ne 0) {
        throw 'T-P1-SES HardFault 恢复测试失败'
    }

    cargo run -p jlink-mcp --example t_p1_ses -- $workerPath validate
    if ($LASTEXITCODE -ne 0) {
        throw 'T-P1-SES 显式验证测试失败'
    }
} catch {
    $testError = $_
}

$cleanupError = $null
try {
    Invoke-JLinkCommander -ScriptPath $resumeScript
} catch {
    $cleanupError = $_
}

if ($cleanupError) {
    throw "T-P1-SES 安全恢复失败：$cleanupError"
}
if ($testError) {
    throw $testError
}
if (Get-Process -Name 'jlink-worker' -ErrorAction SilentlyContinue) {
    throw 'T-P1-SES 结束后仍存在 jlink-worker 进程'
}
if ((Get-FileHash -LiteralPath $dllPath -Algorithm SHA256).Hash -ne $expectedDllHash) {
    throw 'T-P1-SES 结束后 J-Link DLL 身份发生变化'
}
if ((Get-FileHash -LiteralPath $outPath -Algorithm SHA256).Hash -ne $expectedOutHash) {
    throw 'T-P1-SES 结束后 IAR OUT 身份发生变化'
}

Write-Output 'T-P1-SES S32K144/SWD 4000 kHz 真机纵向测试：PASS'
