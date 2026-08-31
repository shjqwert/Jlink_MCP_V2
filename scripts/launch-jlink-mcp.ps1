#requires -Version 5.1
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$OutputEncoding = [Text.UTF8Encoding]::new($false)
. (Join-Path $PSScriptRoot 'release-common.ps1')
$lease = $null
try {
    $productRoot = Get-ProductRoot
    $lockPath = Get-ContainedPath $productRoot 'install.lock'
    Assert-NoReparsePoint $lockPath
    # Shared for the entire MCP lifetime, exclusive for installations.
    $lease = [IO.File]::Open($lockPath, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    $deployment = Get-CurrentDeployment $productRoot
    $ownRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
    if ($deployment -ne $ownRoot) { throw 'Deployment changed during startup; retry in a new Codex task' }
    $null = Read-ReleasePackage $deployment
    # No PowerShell pipeline: native stdin/stdout stay connected to the MCP client.
    & (Get-ContainedPath $deployment 'bin/jlink-mcp.exe')
    $processExitCode = $LASTEXITCODE
}
catch {
    [Console]::Error.WriteLine('J-Link MCP startup failed: ' + $_.Exception.Message)
    $processExitCode = 127
}
finally {
    if ($null -ne $lease) { $lease.Dispose() }
}
exit $processExitCode
