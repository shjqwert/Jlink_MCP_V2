#requires -Version 5.1
[CmdletBinding()]
param(
    [string]$PackageDirectory = '',
    [string]$BinaryDirectory = '',
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$OutputEncoding = [Text.UTF8Encoding]::new($false)
. (Join-Path $PSScriptRoot 'release-common.ps1')
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or -not [Environment]::Is64BitProcess) {
    throw 'Run this installer in 64-bit Windows PowerShell 5.1 or later'
}
if ($BinaryDirectory) {
    throw 'Bare binaries are no longer installed. Build a release package, then use -PackageDirectory. No compilation is performed here.'
}
if (-not $PackageDirectory) { $PackageDirectory = Split-Path -Parent $PSScriptRoot }
$PackageDirectory = [IO.Path]::GetFullPath($PackageDirectory)
$manifest = Read-ReleasePackage $PackageDirectory
$manifestHash = Get-Sha256Hex (Join-Path $PackageDirectory 'release-manifest.json')
$codexCommand = Get-Command codex -ErrorAction Stop
$productRoot = Get-ProductRoot

function Invoke-PluginCli {
    param([string[]]$Arguments)
    # The executable/arguments are separate values. Never build a shell command from a path.
    # In Windows PowerShell, native stderr becomes ErrorRecord objects. A warning
    # with exit code zero must neither abort installation nor contaminate JSON.
    $savedPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $result = @(& $codexCommand.Source @Arguments 2>&1)
        $commandExitCode = $LASTEXITCODE
    }
    finally { $ErrorActionPreference = $savedPreference }
    $stderr = @($result | Where-Object { $_ -is [Management.Automation.ErrorRecord] })
    $stdout = @($result | Where-Object { $_ -isnot [Management.Automation.ErrorRecord] })
    if ($commandExitCode -ne 0) { throw "Codex $($Arguments -join ' ') failed (exit $commandExitCode): $($result -join [Environment]::NewLine)" }
    foreach ($line in $stderr) { [Console]::Error.WriteLine([string]$line) }
    return ($stdout -join [Environment]::NewLine)
}

function Get-PluginState {
    $markets = Invoke-PluginCli @('plugin', 'marketplace', 'list', '--json') | ConvertFrom-Json
    $market = @($markets.marketplaces | Where-Object { $_.name -eq 'jlink-mcp-v2' })
    if ($market.Count -gt 1) { throw 'Ambiguous jlink-mcp-v2 marketplace registration' }
    $installed = @()
    $available = @()
    if ($market.Count -eq 1) {
        $listing = Invoke-PluginCli @('plugin', 'list', '--marketplace', 'jlink-mcp-v2', '--available', '--json') | ConvertFrom-Json
        $installed = @($listing.installed | Where-Object { $_.pluginId -eq 'jlink-mcp@jlink-mcp-v2' })
        $available = @($listing.available | Where-Object { $_.pluginId -eq 'jlink-mcp@jlink-mcp-v2' })
    }
    return [pscustomobject]@{ market = $market; installed = $installed; available = $available }
}

function Remove-ProductRegistration {
    $state = Get-PluginState
    if ($state.installed.Count -gt 0) { $null = Invoke-PluginCli @('plugin', 'remove', 'jlink-mcp@jlink-mcp-v2', '--json') }
    if ($state.market.Count -gt 0) { $null = Invoke-PluginCli @('plugin', 'marketplace', 'remove', 'jlink-mcp-v2') }
}

$null = New-Item -ItemType Directory -Force -Path $productRoot
Assert-NoReparsePoint $productRoot
$lock = $null
$transactionPath = $null
$registrationTouched = $false
$pointerTouched = $false
$oldPointerText = $null
$oldState = $null
try {
    try {
        $lockPath = Get-ContainedPath $productRoot 'install.lock'
        Assert-NoReparsePoint $lockPath
        $lock = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    }
    catch { throw 'J-Link MCP is running or another installation is in progress. Close its Codex tasks and retry.' }
    # Also detect legacy installations, which do not participate in the launcher lock.
    foreach ($process in @(Get-Process -Name 'jlink-mcp', 'jlink-worker' -ErrorAction SilentlyContinue)) {
        try { $processPath = $process.Path } catch { throw 'Cannot establish whether a running J-Link MCP process belongs to this installation' }
        if (-not $processPath) { throw 'Cannot identify a running J-Link MCP process; close it before installation' }
        if ([IO.Path]::GetFullPath($processPath).StartsWith($productRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Stop the active product MCP/Worker before installing. No process was terminated.'
        }
    }
    $oldState = Get-PluginState
    if ($oldState.installed.Count -gt 1) { throw 'Ambiguous installed plugin state' }
    if ($oldState.installed.Count -eq 1 -and -not $oldState.installed[0].enabled) {
        throw 'The existing plugin is disabled. Enable it or remove it explicitly before replacing it; the installer will not change that preference.'
    }
    if ($oldState.market.Count -gt 0) {
        $sourceEntries = @($oldState.installed) + @($oldState.available)
        if ($sourceEntries.Count -eq 0) { throw 'Existing marketplace cannot be identified as this product; leaving it unchanged' }
        foreach ($entry in $sourceEntries) {
            if ($entry.marketplaceSource.sourceType -ne 'local') { throw 'Only local product marketplaces can be replaced and restored automatically' }
        }
        if (-not (Test-Path -LiteralPath $oldState.market[0].root -PathType Container)) { throw 'Previous marketplace root is unavailable; cannot guarantee restoration' }
    }
    $pointerPath = Get-ContainedPath $productRoot 'current.json'
    Assert-NoReparsePoint $pointerPath
    if (Test-Path -LiteralPath $pointerPath) { $oldPointerText = [IO.File]::ReadAllText($pointerPath) }
    $transactionId = [Guid]::NewGuid().ToString('N')
    $transaction = [ordered]@{
        schema_version = 1; state = 'staging'; version = $manifest.version
        previous_pointer = $oldPointerText; previous_registration = $oldState
    }
    $transactionPath = Get-ContainedPath $productRoot "transactions/$transactionId.json"
    Assert-NoReparsePoint $transactionPath
    $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $transactionPath)
    Write-ReleaseJson $transactionPath $transaction

    # A short directory identity avoids the legacy MAX_PATH limit in PowerShell 5.1.
    # The full manifest hash is still compared before any deployment can be reused.
    $deploymentRelative = "deployments/$($manifest.version)-$($manifestHash.Substring(0, 16))"
    $deployment = Get-ContainedPath $productRoot $deploymentRelative
    Assert-NoReparsePoint $deployment
    $reuse = $false
    if (Test-Path -LiteralPath $deployment) {
        try {
            $null = Read-ReleasePackage $deployment
            $reuse = (Get-Sha256Hex (Join-Path $deployment 'release-manifest.json')) -eq $manifestHash
        }
        catch { $reuse = $false }
        if (-not $reuse) {
            # An interrupted/corrupt deployment is retained. Never repair it in place.
            $deploymentRelative += '-' + $transactionId.Substring(0, 8)
            $deployment = Get-ContainedPath $productRoot $deploymentRelative
            Assert-NoReparsePoint $deployment
        }
    }
    foreach ($relative in Get-ReleasePayloadPaths) {
        if ((Get-ContainedPath $deployment $relative).Length -ge 260) { throw 'Installation path is too long for Windows PowerShell 5.1; use a shorter Windows user profile path' }
    }
    if (-not $reuse) {
        # Populate an unpublished directory. Only the later atomic pointer publishes it.
        # No directory rename is required, including on systems that lock copied trees.
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $deployment)
        $null = New-Item -ItemType Directory -Path $deployment
        foreach ($relative in @('release-manifest.json') + @(Get-ReleasePayloadPaths)) {
            $destination = Get-ContainedPath $deployment $relative
            $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination)
            Copy-Item -LiteralPath (Get-ContainedPath $PackageDirectory $relative) -Destination $destination
        }
        $null = Read-ReleasePackage $deployment
    }
    $transaction.state = 'registering'
    Write-ReleaseJson $transactionPath $transaction
    $registrationTouched = $true
    Remove-ProductRegistration
    $null = Invoke-PluginCli @('plugin', 'marketplace', 'add', $deployment)
    $null = Invoke-PluginCli @('plugin', 'add', 'jlink-mcp@jlink-mcp-v2', '--json')
    $newState = Get-PluginState
    $transaction['observed_registration'] = $newState
    Write-ReleaseJson $transactionPath $transaction
    # Codex running as an MSIX app can return a physical LocalCache path for the
    # same logical LOCALAPPDATA directory. Verify the registered payload, not the
    # spelling of that path. The CLI add operation was given only our deployment.
    $registeredPackageMatches = $false
    if ($newState.market.Count -eq 1) {
        $null = Read-ReleasePackage $newState.market[0].root
        $registeredPackageMatches = (Get-Sha256Hex (Join-Path $newState.market[0].root 'release-manifest.json')) -eq $manifestHash
    }
    if ($newState.installed.Count -ne 1 -or -not $newState.installed[0].enabled -or
        $newState.installed[0].version -ne $manifest.version -or $newState.market.Count -ne 1 -or
        -not $registeredPackageMatches) {
        throw 'Codex did not confirm the expected plugin version and marketplace root'
    }
    $pointerTouched = $true
    Set-ReleasePointer $pointerPath ([ordered]@{ schema_version = 1; version = $manifest.version; deployment = $deploymentRelative })
    if ((Get-CurrentDeployment $productRoot) -ne $deployment) { throw 'Deployment pointer verification failed' }
    $transaction.state = 'installed'
    Write-ReleaseJson $transactionPath $transaction
    [pscustomobject]@{
        plugin = 'jlink-mcp@jlink-mcp-v2'; version = $manifest.version
        product_directory = $productRoot; deployment = $deployment
        manifest_sha256 = $manifestHash; segger_managed = $false
        next_step = 'Open a new Codex task. Prepare SEGGER and project configuration yourself before connecting hardware.'
    } | ConvertTo-Json -Depth 4
}
catch {
    $originalError = $_.Exception.Message
    $rollbackErrors = @()
    if ($pointerTouched) {
        try {
            if ($null -ne $oldPointerText) {
                Set-ReleasePointer $pointerPath ($oldPointerText | ConvertFrom-Json)
            }
            elseif (Test-Path -LiteralPath $pointerPath) {
                $failedPointer = Get-ContainedPath $productRoot "transactions/$transactionId.failed-pointer.json"
                Assert-NoReparsePoint $failedPointer
                [IO.File]::Move($pointerPath, $failedPointer)
            }
        }
        catch { $rollbackErrors += $_.Exception.Message }
    }
    if ($registrationTouched) {
        try {
            Remove-ProductRegistration
            if ($oldState.market.Count -gt 0) {
                $null = Invoke-PluginCli @('plugin', 'marketplace', 'add', $oldState.market[0].root)
                if ($oldState.installed.Count -gt 0) { $null = Invoke-PluginCli @('plugin', 'add', 'jlink-mcp@jlink-mcp-v2', '--json') }
            }
            $restored = Get-PluginState
            if ($restored.installed.Count -ne $oldState.installed.Count -or $restored.market.Count -ne $oldState.market.Count) { throw 'Restored registration has an unexpected shape' }
            if ($oldState.installed.Count -eq 1 -and $restored.installed[0].version -ne $oldState.installed[0].version) { throw 'Previous plugin version was not restored' }
            if ($oldState.market.Count -eq 1 -and $restored.market[0].root -ne $oldState.market[0].root) { throw 'Previous marketplace root was not restored' }
        }
        catch { $rollbackErrors += $_.Exception.Message }
    }
    if ($null -ne $transactionPath) {
        $transaction.state = 'failed'
        $transaction['error'] = $originalError
        $transaction['rollback_errors'] = $rollbackErrors
        try { Write-ReleaseJson $transactionPath $transaction } catch { $rollbackErrors += $_.Exception.Message }
    }
    if ($rollbackErrors.Count -gt 0) { throw "Installation failed: $originalError. Recovery also failed: $($rollbackErrors -join '; '). Retained transaction: $transactionPath" }
    throw "Installation failed; previous installation preserved/restored: $originalError"
}
finally {
    if ($null -ne $lock) { $lock.Dispose() }
}
