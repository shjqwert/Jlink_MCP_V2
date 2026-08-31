#requires -Version 5.1
[CmdletBinding()]
param([string]$OutputDirectory = '', [switch]$AllowDirty)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-common.ps1')
$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or -not [Environment]::Is64BitProcess) { throw 'Release builds require Windows x64' }
if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repositoryRoot 'target/distribution' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
Assert-NoReparsePoint $OutputDirectory
$target = 'x86_64-pc-windows-msvc'
$savedFlags = @{}
foreach ($name in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS')) {
    $savedFlags[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
Push-Location $repositoryRoot
try {
    $sourceCommit = (& git rev-parse HEAD) -join ''
    if ($LASTEXITCODE -ne 0) { throw 'A Git checkout is required to identify release sources' }
    $dirty = (@(& git status --porcelain --untracked-files=normal).Count -gt 0)
    if ($dirty -and -not $AllowDirty) { throw 'Commit the release sources first, or use -AllowDirty for a clearly marked local candidate' }
    $metadata = (& cargo metadata --format-version 1 --locked --filter-platform $target) -join "`n" | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Locked dependency resolution failed' }
    $version = @($metadata.packages | Where-Object { -not $_.source -and $_.name -eq 'jlink-mcp' })[0].version
    foreach ($package in @($metadata.packages | Where-Object { -not $_.source })) {
        if ($package.version -ne $version) { throw 'Workspace version mismatch' }
    }
    $plugin = Get-Content -LiteralPath 'plugins/jlink-mcp/.codex-plugin/plugin.json' -Raw | ConvertFrom-Json
    if ($plugin.version -ne $version) { throw 'Cargo/plugin version mismatch' }
    $sourceLines = @(& git ls-files --cached --others --exclude-standard | Sort-Object -Unique | ForEach-Object {
        if (Test-Path -LiteralPath $_ -PathType Leaf) { $_ + ' ' + (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash }
        else { $_ + ' DELETED' }
    })
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { $snapshotHash = [BitConverter]::ToString($hasher.ComputeHash([Text.Encoding]::UTF8.GetBytes(($sourceLines -join "`n")))).Replace('-', '').ToLowerInvariant() }
    finally { $hasher.Dispose() }
    [Environment]::SetEnvironmentVariable('RUSTFLAGS', '-C target-feature=+crt-static', 'Process')
    # Set the highest-precedence Cargo flag source explicitly. Some PowerShell/.NET
    # versions preserve an empty variable on null assignment, which masks RUSTFLAGS.
    $releaseFlags = @(& (Join-Path $PSScriptRoot 'release-rustflags.ps1'))
    [Environment]::SetEnvironmentVariable('CARGO_ENCODED_RUSTFLAGS', ($releaseFlags -join [char]31), 'Process')
    [Environment]::SetEnvironmentVariable('CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS', $null, 'Process')
    $buildDirectory = Join-Path $repositoryRoot 'target/distribution-build'
    & cargo build --locked --release --target $target --target-dir $buildDirectory -p jlink-mcp -p jlink-worker
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    foreach ($name in @('jlink-mcp', 'jlink-worker')) {
        $binaryText = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes((Join-Path $buildDirectory "$target/release/$name.exe")))
        foreach ($privatePath in @($repositoryRoot, $env:USERPROFILE)) {
            if ($privatePath -and ($binaryText.Contains($privatePath) -or $binaryText.Contains($privatePath.Replace('\', '/')))) { throw "Build path was not remapped in $name" }
        }
    }
    $runDirectory = Join-Path $OutputDirectory ([DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ') + '-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    $packageName = "jlink-mcp-V$version-windows-x64"
    $packageRoot = Join-Path $runDirectory $packageName
    $null = New-Item -ItemType Directory -Force -Path $packageRoot
    foreach ($relative in Get-ReleasePayloadPaths) {
        if ($relative -eq 'THIRD-PARTY-NOTICES.txt') { continue }
        $source = Get-ContainedPath $repositoryRoot $relative
        if ($relative.StartsWith('bin/')) { $source = Join-Path $buildDirectory "$target/release/$([IO.Path]::GetFileName($relative))" }
        $destination = Get-ContainedPath $packageRoot $relative
        $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination)
        Copy-Item -LiteralPath $source -Destination $destination
    }
    & (Join-Path $PSScriptRoot 'write-thirdparty-notices.ps1') -Metadata $metadata -Destination (Join-Path $packageRoot 'THIRD-PARTY-NOTICES.txt')
    $files = @(foreach ($relative in Get-ReleasePayloadPaths) {
        $path = Get-ContainedPath $packageRoot $relative
        [ordered]@{ path = $relative; bytes = (Get-Item -LiteralPath $path).Length; sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    $toolsVersion = 'unknown'
    if (Test-Path -LiteralPath $vswhere) {
        $vsRoot = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath) -join ''
        if ($vsRoot) { $toolsVersion = (Get-Content -LiteralPath (Join-Path $vsRoot 'VC/Auxiliary/Build/Microsoft.VCToolsVersion.default.txt') -Raw).Trim() }
    }
    $sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/Lib'
    $sdkVersions = @()
    if (Test-Path -LiteralPath $sdkRoot) { $sdkVersions = @(Get-ChildItem -LiteralPath $sdkRoot -Directory | Select-Object -ExpandProperty Name | Sort-Object) }
    $manifest = [ordered]@{
        schema_version = 1; version = $version; target = $target; crt_linkage = 'static'
        source_commit = $sourceCommit; source_dirty = $dirty; source_snapshot_sha256 = $snapshotHash
        build = [ordered]@{
            rustc = ((& rustc -Vv) -join "`n"); cargo = ((& cargo -V) -join '')
            rustflags = '-C target-feature=+crt-static --remap-path-prefix=<repository>=/jlink-mcp --remap-path-prefix=<user-profile>=/build-user'
            profile = 'release'; locked = $true
            host_os = [Environment]::OSVersion.Version.ToString()
            available_msvc_tools = $toolsVersion; available_windows_sdks = $sdkVersions
        }
        files = $files
    }
    Write-ReleaseJson (Join-Path $packageRoot 'release-manifest.json') $manifest
    $null = Read-ReleasePackage $packageRoot
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = Join-Path $runDirectory ($packageName + '.zip')
    [IO.Compression.ZipFile]::CreateFromDirectory($packageRoot, $archive, [IO.Compression.CompressionLevel]::Optimal, $true)
    $archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText(($archive + '.sha256'), "$archiveHash  $([IO.Path]::GetFileName($archive))`n", [Text.UTF8Encoding]::new($false))
    $result = [ordered]@{ version = $version; package_directory = $packageRoot; archive = $archive; sha256 = $archiveHash; source_dirty = $dirty }
    Write-ReleaseJson (Join-Path $runDirectory 'build-result.json') $result
    [pscustomobject]$result
}
finally {
    foreach ($name in $savedFlags.Keys) {
        if ($null -eq $savedFlags[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $savedFlags[$name], 'Process') }
    }
    Pop-Location
}
