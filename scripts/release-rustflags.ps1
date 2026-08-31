# Emit an argument array so paths containing spaces survive CARGO_ENCODED_RUSTFLAGS.
$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
'-C'
'target-feature=+crt-static'
'--remap-path-prefix'
$repositoryRoot + '=/jlink-mcp'
if ($env:USERPROFILE) {
    '--remap-path-prefix'
    [IO.Path]::GetFullPath($env:USERPROFILE) + '=/build-user'
}
