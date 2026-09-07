$ErrorActionPreference = 'Stop'

cargo fmt --all -- --check
if ($LASTEXITCODE -ne 0) {
    throw 'cargo fmt check failed'
}

cargo clippy --locked --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) {
    throw 'cargo clippy failed'
}

cargo test --locked --workspace
if ($LASTEXITCODE -ne 0) {
    throw 'cargo test failed'
}

& "$PSScriptRoot\check-dependencies.ps1"

# Pure helper checks use the minimum supported PowerShell runtime; no probe access.
& powershell.exe -NoLogo -NoProfile -File "$PSScriptRoot\check-firmware-workflow.ps1"
if ($LASTEXITCODE -ne 0) { throw "Firmware workflow checks failed" }
