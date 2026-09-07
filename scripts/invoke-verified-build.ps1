#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$FilePath,
    [string[]]$ArgumentList = @(),
    [Parameter(Mandatory = $true)][string]$ArtifactPath,
    [string]$ReceiptPath = '',
    [scriptblock]$OnSuccess,
    [string[]]$InputPath = @(),
    [string]$BuildConfiguration = '',
    [switch]$AllowUnchangedArtifact
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$artifact = [IO.Path]::GetFullPath($ArtifactPath)
if (-not $ReceiptPath) { $ReceiptPath = $artifact + '.jlink-build.json' }
$receipt = [IO.Path]::GetFullPath($ReceiptPath)
if ($artifact.Equals($receipt, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'ReceiptPath must not equal ArtifactPath'
}
# Remove only this helper's prior receipt, never the user's prior firmware image.
# A failed build must not leave a stale success receipt eligible for a next stage.
$previous = $null
if (Test-Path -LiteralPath $receipt) {
    try { $previous = Get-Content -LiteralPath $receipt -Raw | ConvertFrom-Json } catch { $previous = $null }
    Remove-Item -LiteralPath $receipt -Force
}
$command = Get-Command -Name $FilePath -CommandType Application -ErrorAction Stop | Select-Object -First 1
if ($AllowUnchangedArtifact -and ($InputPath.Count -eq 0 -or -not $BuildConfiguration)) {
    throw 'Unchanged-artifact reuse requires an explicit complete InputPath list and BuildConfiguration'
}
function Get-InputEvidence {
    $files = @(foreach ($path in @($InputPath | Sort-Object -Unique)) {
        $full = [IO.Path]::GetFullPath($path)
        if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { throw "Missing declared build input: $full" }
        [ordered]@{ path = $full; sha256 = (Get-FileHash -LiteralPath $full).Hash }
    })
    [ordered]@{
        configuration = $BuildConfiguration
        command = $command.Source
        command_sha256 = (Get-FileHash -LiteralPath $command.Source).Hash
        arguments = @($ArgumentList)
        inputs = $files
    } | ConvertTo-Json -Depth 8 -Compress
}
$inputsBefore = if ($InputPath.Count -gt 0) { Get-InputEvidence } else { '' }
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
if ($inputsBefore -and (Get-InputEvidence) -ne $inputsBefore) { throw 'Declared build inputs changed during the build' }
$reuse = $false
if ($item.LastWriteTimeUtc -lt $started -or
    ($null -ne $before -and $before.modified -eq $item.LastWriteTimeUtc -and
     $before.length -eq $item.Length -and $before.sha256 -eq $sha256)) {
    $reuse = $AllowUnchangedArtifact -and $null -ne $previous -and
        $null -ne $previous.PSObject.Properties['input_evidence'] -and
        $previous.input_evidence -eq $inputsBefore -and
        $previous.build_succeeded -eq $true -and $previous.exit_code -eq 0 -and
        $previous.artifact_path -eq $artifact -and $previous.artifact_sha256 -eq $sha256.ToLowerInvariant()
    if (-not $reuse) { throw 'No fresh image or matching prior build/input evidence was proven' }
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
    reused_artifact = [bool]$reuse
    input_evidence = $inputsBefore
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
