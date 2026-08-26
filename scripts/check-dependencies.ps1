$ErrorActionPreference = 'Stop'

$allowedDependencies = @{
    'jlink-domain'  = @()
    'jlink-capture' = @('jlink-domain')
    'jlink-worker'  = @('jlink-capture', 'jlink-domain')
    'jlink-mcp'     = @('jlink-capture', 'jlink-domain')
}

$metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) {
    throw 'cargo metadata failed'
}

$workspacePackages = @($metadata.packages | Where-Object { $allowedDependencies.ContainsKey($_.name) })
if ($workspacePackages.Count -ne $allowedDependencies.Count) {
    throw "Expected exactly four production crates, found $($workspacePackages.Count)"
}

foreach ($package in $workspacePackages) {
    $actual = @(
        $package.dependencies |
            Where-Object { $allowedDependencies.ContainsKey($_.name) } |
            ForEach-Object { $_.name } |
            Sort-Object -Unique
    )
    $expected = @($allowedDependencies[$package.name] | Sort-Object)
    if (Compare-Object -ReferenceObject $expected -DifferenceObject $actual) {
        throw "Invalid workspace dependency direction for $($package.name): expected [$($expected -join ', ')], actual [$($actual -join ', ')]"
    }
}

Write-Output 'Workspace dependency direction: PASS'
