# Shared by the build, installer and launcher; Windows PowerShell 5.1 compatible.
Set-StrictMode -Version Latest

function Get-ReleasePayloadPaths {
    @(
        'bin/jlink-mcp.exe', 'bin/jlink-worker.exe',
        'scripts/install-codex-plugin.ps1', 'scripts/launch-jlink-mcp.ps1', 'scripts/release-common.ps1',
        '.agents/plugins/marketplace.json', 'plugins/jlink-mcp/.codex-plugin/plugin.json',
        'plugins/jlink-mcp/.mcp.json', 'plugins/jlink-mcp/skills/jlink-mcp/SKILL.md',
        'plugins/jlink-mcp/skills/jlink-mcp/agents/openai.yaml',
        'jlink-mcp.example.toml', 'INSTALL.md', 'LICENSE', 'THIRD-PARTY-NOTICES.txt'
    )
}

function Get-ContainedPath {
    param([string]$Root, [string]$RelativePath)
    if ($RelativePath -notmatch '^[A-Za-z0-9_.\-/]+$' -or
        $RelativePath.StartsWith('/') -or $RelativePath -match '(^|/)\.{1,2}(/|$)' -or
        $RelativePath -match '//|[. ](/|$)') {
        throw "Unsafe package path: $RelativePath"
    }
    $base = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    $result = [IO.Path]::GetFullPath((Join-Path $base $RelativePath))
    if (-not $result.StartsWith($base + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Path escapes its root: $RelativePath"
    }
    return $result
}

function Assert-NoReparsePoint {
    param([string]$Path)
    $cursor = [IO.Path]::GetFullPath($Path)
    while ($cursor) {
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "Reparse points are not supported in installation paths: $cursor"
            }
        }
        $cursor = Split-Path -Parent $cursor
    }
}

function Write-ReleaseJson {
    param([string]$Path, $Value)
    [IO.File]::WriteAllText($Path, (($Value | ConvertTo-Json -Depth 20) + [Environment]::NewLine), [Text.UTF8Encoding]::new($false))
}

function Set-ReleasePointer {
    param([string]$Path, $Value)
    Assert-NoReparsePoint $Path
    $temporary = $Path + '.' + [Guid]::NewGuid().ToString('N') + '.tmp'
    Write-ReleaseJson $temporary $Value
    if (Test-Path -LiteralPath $Path) {
        # PowerShell 5.1 converts ordinary $null to an empty string for this overload.
        [IO.File]::Replace($temporary, $Path, [NullString]::Value)
    }
    else {
        [IO.File]::Move($temporary, $Path)
    }
}

function Get-PeImports {
    param([string]$Path)
    # Parse the PE import tables without requiring dumpbin on the user's computer.
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 256 -or [BitConverter]::ToUInt16($bytes, 0) -ne 0x5A4D) { throw "Not a PE executable: $Path" }
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    if ($pe -lt 0 -or $pe + 264 -gt $bytes.Length -or [BitConverter]::ToUInt32($bytes, $pe) -ne 0x4550) { throw "Invalid PE header: $Path" }
    if ([BitConverter]::ToUInt16($bytes, $pe + 4) -ne 0x8664 -or [BitConverter]::ToUInt16($bytes, $pe + 24) -ne 0x20B) {
        throw "Expected an x64 PE32+ executable: $Path"
    }
    $sectionCount = [BitConverter]::ToUInt16($bytes, $pe + 6)
    $optionalLength = [BitConverter]::ToUInt16($bytes, $pe + 20)
    if ($optionalLength -lt 240) { throw 'Incomplete PE optional header' }
    $sections = @()
    for ($i = 0; $i -lt $sectionCount; $i++) {
        $offset = $pe + 24 + $optionalLength + 40 * $i
        if ($offset + 40 -gt $bytes.Length) { throw 'Incomplete PE section table' }
        $sections += [pscustomobject]@{
            Rva = [BitConverter]::ToUInt32($bytes, $offset + 12)
            Size = [Math]::Max([BitConverter]::ToUInt32($bytes, $offset + 8), [BitConverter]::ToUInt32($bytes, $offset + 16))
            Raw = [BitConverter]::ToUInt32($bytes, $offset + 20)
        }
    }
    $toOffset = {
        param([long]$Rva)
        foreach ($section in $sections) {
            if ($Rva -ge $section.Rva -and $Rva -lt ([long]$section.Rva + $section.Size)) {
                $position = [long]$section.Raw + $Rva - $section.Rva
                if ($position -lt 0 -or $position -ge $bytes.Length) { throw 'PE RVA outside file' }
                return [int]$position
            }
        }
        throw 'PE RVA outside sections'
    }
    # Normal imports and delay imports (RVA based PE32+ descriptors).
    foreach ($table in @(@(1, 20, 12), @(13, 32, 4))) {
        $entry = $pe + 24 + 112 + 8 * $table[0]
        $rva = [BitConverter]::ToUInt32($bytes, $entry)
        if ($rva -eq 0) { continue }
        $offset = & $toOffset $rva
        $terminated = $false
        for ($i = 0; $i -lt 4096; $i++) {
            $descriptor = $offset + $i * $table[1]
            if ($descriptor + $table[1] -gt $bytes.Length) { throw 'Incomplete PE import table' }
            $nameRva = [BitConverter]::ToUInt32($bytes, $descriptor + $table[2])
            if ($nameRva -eq 0) { $terminated = $true; break }
            if ($table[0] -eq 13 -and [BitConverter]::ToUInt32($bytes, $descriptor) -ne 1) { throw 'Unsupported delay import descriptor' }
            $nameOffset = & $toOffset $nameRva
            $end = $nameOffset
            while ($end -lt $bytes.Length -and $bytes[$end] -ne 0 -and $end - $nameOffset -lt 256) { $end++ }
            if ($end -ge $bytes.Length -or $end - $nameOffset -ge 256) { throw 'Invalid PE import name' }
            [Text.Encoding]::ASCII.GetString($bytes, $nameOffset, $end - $nameOffset)
        }
        if (-not $terminated) { throw 'Unterminated PE import table' }
    }
}

function Read-ReleasePackage {
    param([string]$Root)
    Assert-NoReparsePoint $Root
    $manifestPath = Get-ContainedPath $Root 'release-manifest.json'
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($manifest.schema_version -ne 1 -or $manifest.version -notmatch '^\d+\.\d+\.\d+$' -or
        $manifest.target -ne 'x86_64-pc-windows-msvc' -or $manifest.crt_linkage -ne 'static') {
        throw 'Unsupported release manifest, version, architecture or CRT mode'
    }
    $expected = @(Get-ReleasePayloadPaths)
    $seen = @{}
    foreach ($entry in $manifest.files) {
        if ($expected -cnotcontains $entry.path -or $seen.ContainsKey($entry.path) -or $entry.sha256 -notmatch '^[A-Fa-f0-9]{64}$') {
            throw "Unexpected or duplicate release entry: $($entry.path)"
        }
        $seen[$entry.path] = $true
        $filePath = Get-ContainedPath $Root $entry.path
        Assert-NoReparsePoint $filePath
        if (-not (Test-Path -LiteralPath $filePath -PathType Leaf)) { throw "Missing release file: $($entry.path)" }
        if ((Get-Item -LiteralPath $filePath).Length -ne $entry.bytes -or
            (Get-FileHash -LiteralPath $filePath -Algorithm SHA256).Hash -ne $entry.sha256) {
            throw "Release file integrity mismatch: $($entry.path)"
        }
    }
    if ($seen.Count -ne $expected.Count) { throw 'Release manifest omits required payload files' }
    foreach ($file in Get-ChildItem -LiteralPath $Root -Recurse -Force) {
        if (($file.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Release contains a reparse point' }
        if ($file.PSIsContainer) { continue }
        $relative = $file.FullName.Substring([IO.Path]::GetFullPath($Root).TrimEnd('\', '/').Length + 1).Replace('\', '/')
        if ($relative -ne 'release-manifest.json' -and -not $seen.ContainsKey($relative)) { throw "Unexpected release file: $relative" }
    }
    $plugin = Get-Content -LiteralPath (Get-ContainedPath $Root 'plugins/jlink-mcp/.codex-plugin/plugin.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($plugin.name -ne 'jlink-mcp' -or $plugin.version -ne $manifest.version) { throw 'Plugin/release version mismatch' }
    foreach ($name in @('jlink-mcp', 'jlink-worker')) {
        $imports = @(Get-PeImports (Get-ContainedPath $Root "bin/$name.exe"))
        if ($imports.Count -eq 0) { throw "Missing system imports in $name" }
        foreach ($import in $imports) {
            if ($import -match '^(vcruntime|msvcp|msvcr|ucrtbase|api-ms-win-crt-|libgcc|libstdc\+\+)' -or $import -match 'jlink') {
                throw "Unexpected runtime import in ${name}: $import"
            }
        }
    }
    return $manifest
}

function Get-ProductRoot {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { throw 'LOCALAPPDATA is required' }
    if (-not [IO.Path]::IsPathRooted($env:LOCALAPPDATA)) { throw 'LOCALAPPDATA must be an absolute path' }
    $root = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'Programs/jlink-mcp'))
    Assert-NoReparsePoint $root
    return $root
}

function Get-CurrentDeployment {
    param([string]$ProductRoot)
    $pointerPath = Get-ContainedPath $ProductRoot 'current.json'
    Assert-NoReparsePoint $pointerPath
    $pointer = Get-Content -LiteralPath $pointerPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($pointer.schema_version -ne 1 -or $pointer.deployment -notmatch '^deployments/\d+\.\d+\.\d+-[a-f0-9]{16}(-[a-f0-9]{8})?$') { throw 'Invalid current deployment pointer; rerun the installer' }
    return (Get-ContainedPath $ProductRoot $pointer.deployment)
}
