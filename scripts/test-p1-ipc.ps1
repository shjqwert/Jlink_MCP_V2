$ErrorActionPreference = 'Stop'

cargo test -p jlink-domain --test t_p1_ipc_frame
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

cargo build -p jlink-worker
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

$workerPath = Join-Path $PSScriptRoot '..\target\debug\jlink-worker.exe'
cargo run -p jlink-mcp --example t_p1_ipc -- $workerPath
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Output 'T-P1-IPC Windows 进程集成：PASS'
