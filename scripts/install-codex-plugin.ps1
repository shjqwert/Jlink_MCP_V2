[CmdletBinding()]
param(
    [string]$BinaryDirectory = '',
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$sourcePlugin = Join-Path $repositoryRoot 'plugins\jlink-mcp'
$sourceMarketplace = Join-Path $repositoryRoot '.agents\plugins\marketplace.json'
$marketplaceRoot = Join-Path $repositoryRoot '.local-marketplace'
$stagedPlugin = Join-Path $marketplaceRoot 'plugins\jlink-mcp'
$stagedMarketplace = Join-Path $marketplaceRoot '.agents\plugins\marketplace.json'

if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
    throw 'LOCALAPPDATA is unavailable; cannot select the per-user product directory'
}
$productDirectory = Join-Path $env:LOCALAPPDATA 'Programs\jlink-mcp'
if (-not (Test-Path -LiteralPath $sourcePlugin -PathType Container)) {
    throw "Plugin source is missing: $sourcePlugin"
}
if (-not (Test-Path -LiteralPath $sourceMarketplace -PathType Leaf)) {
    throw "Marketplace source is missing: $sourceMarketplace"
}
$productExecutables = @(
    [System.IO.Path]::GetFullPath((Join-Path $productDirectory 'jlink-mcp.exe'))
    [System.IO.Path]::GetFullPath((Join-Path $productDirectory 'jlink-worker.exe'))
)
$activeProductProcesses = Get-Process -Name 'jlink-mcp', 'jlink-worker' -ErrorAction SilentlyContinue |
    Where-Object {
        try {
            $productExecutables -contains [System.IO.Path]::GetFullPath($_.Path)
        }
        catch {
            $false
        }
    }
if ($activeProductProcesses) {
    throw 'Stop active jlink-mcp and jlink-worker processes from the product directory before updating the installed binaries'
}

if ([string]::IsNullOrWhiteSpace($BinaryDirectory)) {
    $BinaryDirectory = Join-Path $repositoryRoot 'target\release'
}
elseif (-not [System.IO.Path]::IsPathRooted($BinaryDirectory)) {
    $BinaryDirectory = Join-Path $repositoryRoot $BinaryDirectory
}
$BinaryDirectory = [System.IO.Path]::GetFullPath($BinaryDirectory)

if (-not $SkipBuild) {
    cargo build --release -p jlink-mcp -p jlink-worker
    if ($LASTEXITCODE -ne 0) {
        throw 'Release build failed'
    }
}

$sourceMcp = Join-Path $BinaryDirectory 'jlink-mcp.exe'
$sourceWorker = Join-Path $BinaryDirectory 'jlink-worker.exe'
if (-not (Test-Path -LiteralPath $sourceMcp -PathType Leaf)) {
    throw "MCP binary is missing: $sourceMcp"
}
if (-not (Test-Path -LiteralPath $sourceWorker -PathType Leaf)) {
    throw "Worker binary is missing: $sourceWorker"
}

$null = New-Item -ItemType Directory -Force -Path $productDirectory
Copy-Item -LiteralPath $sourceMcp -Destination (Join-Path $productDirectory 'jlink-mcp.exe') -Force
Copy-Item -LiteralPath $sourceWorker -Destination (Join-Path $productDirectory 'jlink-worker.exe') -Force

$null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $stagedMarketplace)
$null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $stagedPlugin)
Copy-Item -LiteralPath $sourceMarketplace -Destination $stagedMarketplace -Force
$null = New-Item -ItemType Directory -Force -Path $stagedPlugin
Get-ChildItem -LiteralPath $sourcePlugin -Force |
    Copy-Item -Destination $stagedPlugin -Recurse -Force

$stagedManifestPath = Join-Path $stagedPlugin '.codex-plugin\plugin.json'
$stagedManifest = Get-Content -Raw -LiteralPath $stagedManifestPath | ConvertFrom-Json -Depth 20
$baseVersion = ($stagedManifest.version -split '\+', 2)[0]
$cachebuster = [DateTime]::UtcNow.ToString('yyyyMMddHHmmss')
$stagedManifest.version = "$baseVersion+codex.$cachebuster"
$stagedManifestJson = $stagedManifest | ConvertTo-Json -Depth 20
[System.IO.File]::WriteAllText(
    $stagedManifestPath,
    $stagedManifestJson + [Environment]::NewLine,
    [System.Text.UTF8Encoding]::new($false)
)

$codexCommand = Get-Command codex -ErrorAction Stop
$pluginListing = (& $codexCommand.Source plugin list 2>&1) -join [Environment]::NewLine
if ($pluginListing -match [regex]::Escape('jlink-mcp@jlink-mcp-v2')) {
    & $codexCommand.Source plugin remove 'jlink-mcp@jlink-mcp-v2'
    if ($LASTEXITCODE -ne 0) {
        throw 'Failed to remove the previous jlink-mcp plugin installation'
    }
}
$marketplaceListing = (& $codexCommand.Source plugin marketplace list 2>&1) -join [Environment]::NewLine
if ($marketplaceListing -match '(?m)^jlink-mcp-v2\s') {
    & $codexCommand.Source plugin marketplace remove 'jlink-mcp-v2'
    if ($LASTEXITCODE -ne 0) {
        throw 'Failed to remove the previous jlink-mcp-v2 marketplace registration'
    }
}

& $codexCommand.Source plugin marketplace add $marketplaceRoot
if ($LASTEXITCODE -ne 0) {
    throw 'Failed to register the staged jlink-mcp-v2 marketplace'
}
& $codexCommand.Source plugin add 'jlink-mcp@jlink-mcp-v2' --json
if ($LASTEXITCODE -ne 0) {
    throw 'Failed to install the jlink-mcp plugin'
}

[pscustomobject]@{
    plugin = 'jlink-mcp@jlink-mcp-v2'
    version = $stagedManifest.version
    product_directory = $productDirectory
    marketplace_root = $marketplaceRoot
    jlink_mcp_sha256 = (Get-FileHash -LiteralPath (Join-Path $productDirectory 'jlink-mcp.exe') -Algorithm SHA256).Hash
    jlink_worker_sha256 = (Get-FileHash -LiteralPath (Join-Path $productDirectory 'jlink-worker.exe') -Algorithm SHA256).Hash
} | ConvertTo-Json -Depth 4
