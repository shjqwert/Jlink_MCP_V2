#requires -Version 5.1
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

    [IO.File]::WriteAllText($receipt, '{"build_succeeded":true}')
    Assert-Throws {
        & $build -FilePath (Join-Path $root 'missing-compiler.exe') -ArtifactPath $artifact -OnSuccess $followup
    } 'An unresolved compiler did not stop dependent work'
    Assert-True (-not (Test-Path -LiteralPath $receipt)) 'Old success receipt survived compiler lookup failure'
    Assert-True (-not (Test-Path -LiteralPath $marker)) 'Dependent stage ran without a compiler'

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
    $legacy = @(Get-ReleasePayloadPaths -Legacy)
    Assert-True ($payload.Count -eq $legacy.Count + 2) 'Legacy package shape must remain an exact closed set'
    Assert-True ($legacy -notcontains 'scripts/new-firmware-identity.ps1') 'Legacy package incorrectly requires a new helper'
    Assert-True ($legacy -contains 'bin/jlink-worker.exe') 'Legacy compatibility removed a core payload requirement'
    $incremental = Join-Path $root 'incremental.ps1'
    $inputFile = Join-Path $root 'source.c'
    $freshArtifact = Join-Path $root 'incremental.out'
    [IO.File]::WriteAllText($inputFile, 'source version 1')
    [IO.File]::WriteAllText($incremental, 'param([string]$Output); if (-not (Test-Path -LiteralPath $Output)) { [IO.File]::WriteAllText($Output, "built image") }; exit 0')
    $options = @{ FilePath='powershell.exe'; ArgumentList=@('-NoProfile', '-File', $incremental, '-Output', $freshArtifact); ArtifactPath=$freshArtifact; InputPath=@($inputFile, $incremental); BuildConfiguration='fixture-debug' }
    $first = & $build @options
    $second = & $build @options -AllowUnchangedArtifact
    Assert-True (-not $first.reused_artifact -and $second.reused_artifact) 'Proven unchanged incremental result was not reused'
    [IO.File]::WriteAllText($inputFile, 'source version 2')
    Assert-Throws { & $build @options -AllowUnchangedArtifact } 'Changed input reused stale firmware'
    Assert-True (-not (Test-Path -LiteralPath ($freshArtifact + '.jlink-build.json'))) 'Rejected reuse left an applicable receipt'
    Write-Host 'PASS: JLID generation, size/ASCII bounds, overwrite guard, failed/stale build blocking, receipt protection, fresh output and payload inclusion.'
}
finally { Remove-Item -LiteralPath $root -Recurse -Force }
