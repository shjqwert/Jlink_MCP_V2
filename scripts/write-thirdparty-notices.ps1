param([Parameter(Mandatory = $true)]$Metadata, [Parameter(Mandatory = $true)][string]$Destination)
$ErrorActionPreference = 'Stop'
$packages = @{}
$nodes = @{}
foreach ($package in $Metadata.packages) { $packages[$package.id] = $package }
foreach ($node in $Metadata.resolve.nodes) { $nodes[$node.id] = $node }
$pending = [Collections.Generic.Queue[string]]::new()
foreach ($package in $Metadata.packages) {
    if (-not $package.source -and $package.name -in @('jlink-mcp', 'jlink-worker')) { $pending.Enqueue($package.id) }
}
$visited = @{}
while ($pending.Count -gt 0) {
    $id = $pending.Dequeue()
    if ($visited.ContainsKey($id)) { continue }
    $visited[$id] = $true
    foreach ($dep in $nodes[$id].deps) {
        # Include normal and build dependencies, never follow dev-only edges.
        if (@($dep.dep_kinds | Where-Object { $_.kind -ne 'dev' }).Count -gt 0) { $pending.Enqueue($dep.pkg) }
    }
}
$text = [Text.StringBuilder]::new()
[void]$text.AppendLine('J-Link MCP third-party notices')
[void]$text.AppendLine('Includes the resolved Windows production/build dependency closure and Rust library notices. Inclusion of build-tool notices does not mean every listed component is linked into the executable. SEGGER software is not distributed.')
foreach ($package in @($visited.Keys | ForEach-Object { $packages[$_] } | Where-Object { $_.source } | Sort-Object name, version)) {
    $directory = Split-Path -Parent $package.manifest_path
    $licenses = @(Get-ChildItem -LiteralPath $directory -File -Force | Where-Object { $_.Name -match '^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)' } | Sort-Object Name)
    [void]$text.AppendLine("`n===== $($package.name) $($package.version) | $($package.license) | $($package.repository) =====")
    if ($licenses.Count -eq 0) {
        # These upstream workspaces omit the root MIT license from their crates.io subcrates.
        $vcs = Get-Content -LiteralPath (Join-Path $directory '.cargo_vcs_info.json') -Raw | ConvertFrom-Json
        if ($package.name -in @('jsonschema-regex', 'jsonschema-value') -and $package.version -eq '0.51.0' -and $vcs.git.sha1 -eq 'b7fd606646d1e7faefa9c0411569ee2f6b6cb161') {
            $licenses = @(Get-Item -LiteralPath (Join-Path $PSScriptRoot 'license-overrides/jsonschema-MIT.txt'))
            [void]$text.AppendLine('License source: https://github.com/Stranger6667/jsonschema/blob/b7fd606646d1e7faefa9c0411569ee2f6b6cb161/LICENSE')
        }
        elseif ($package.name -in @('uuid-simd', 'vsimd') -and $package.version -eq '0.8.0' -and $vcs.git.sha1 -eq 'd74c030d9dc4f3cae02146d1f497ff62726ef09a') {
            $licenses = @(Get-Item -LiteralPath (Join-Path $PSScriptRoot 'license-overrides/simd-MIT.txt'))
            [void]$text.AppendLine('License source: https://github.com/Nugine/simd/blob/d74c030d9dc4f3cae02146d1f497ff62726ef09a/LICENSE')
        }
        else { throw "License text needs review: $($package.name) $($package.version)" }
    }
    foreach ($license in $licenses) {
        [void]$text.AppendLine("--- $($license.Name) ---")
        [void]$text.AppendLine([IO.File]::ReadAllText($license.FullName))
    }
}
$sysroot = (& rustc --print sysroot) -join ''
if ($LASTEXITCODE -ne 0) { throw 'Cannot locate Rust library notices' }
$rustNotices = Join-Path $sysroot 'share/doc/rust'
[void]$text.AppendLine("`n===== Rust standard library copyright inventory (upstream HTML, preserved verbatim) =====")
[void]$text.AppendLine([IO.File]::ReadAllText((Join-Path $rustNotices 'COPYRIGHT-library.html')))
foreach ($license in Get-ChildItem -LiteralPath (Join-Path $rustNotices 'licenses') -File | Sort-Object Name) {
    [void]$text.AppendLine("`n--- Rust license text: $($license.Name) ---")
    [void]$text.AppendLine([IO.File]::ReadAllText($license.FullName))
}
[IO.File]::WriteAllText($Destination, $text.ToString(), [Text.UTF8Encoding]::new($false))
