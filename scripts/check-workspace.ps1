$ErrorActionPreference = 'Stop'

cargo fmt --all -- --check
if ($LASTEXITCODE -ne 0) {
    throw 'cargo fmt check failed'
}

cargo clippy --workspace --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) {
    throw 'cargo clippy failed'
}

cargo test --workspace
if ($LASTEXITCODE -ne 0) {
    throw 'cargo test failed'
}

& "$PSScriptRoot\check-dependencies.ps1"
