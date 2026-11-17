#Requires -Version 7.0
<#
.SYNOPSIS
  Builds Spire with profile-guided optimisation (PGO) and reports the result.

.DESCRIPTION
  PGO is three passes:

    1. build an instrumented binary      (-Cprofile-generate)
    2. run it under the real workload    (profile lands on SIGTERM, which Spire
                                          already handles as a graceful shutdown)
    3. collect + rebuild                 (llvm-profdata merge, -Cprofile-use)

  The profile is collected on the same topology the benchmark uses: the
  instrumented gateway pinned to the measured cores, the Java backend on the E
  cores, and hey as the client. The final binary is written to
  bin/spire-pgo and baked into perf-spire:pgo so bin-ab.ps1 can compare it
  against any other build in one session.

.EXAMPLE
  ./pgo.ps1 -LoadSeconds 30
#>
param(
    [string]$Target = 'x86_64-unknown-linux-gnu',
    [string]$Proxy = 'http://host.docker.internal:7890',
    [string]$Toolchain = 'rust:1.100',
    [string]$Cpuset = '4,6,8,10',
    [int]$Concurrency = 64,
    [int]$LoadSeconds = 30,
    [string]$Volume = 'pgo-data'
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$Repo = Split-Path -Parent (Split-Path -Parent $Root)
$Bin = Join-Path $Root 'bin'
$Compose = Join-Path $Root 'compose/stack.yaml'
$Network = 'perf-2026-11_bench'
$HeyImage = 'demisto/rakyll-hey:1.0.0.44140'

New-Item -ItemType Directory -Force -Path $Bin | Out-Null
$repoPosix = $Repo -replace '\\', '/'
$binPosix = $Bin -replace '\\', '/'

Write-Host '== 1/4 build instrumented binary ==' -ForegroundColor Cyan
& docker run --rm --volume "${repoPosix}:/src" --volume "${binPosix}:/out" -w /src/rust-proxy `
    -e "HTTP_PROXY=$Proxy" -e "HTTPS_PROXY=$Proxy" `
    $Toolchain bash -c "set -e
export PATH=/usr/local/cargo/bin:`$PATH
rustup component add llvm-tools >/dev/null 2>&1 || true
export CARGO_PROFILE_RELEASE_LTO=fat CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
RUSTFLAGS='-Cprofile-generate=/work/pgo' cargo build --release --target $Target 2>&1 | tail -2
cp target/$Target/release/spire /out/spire-pgogen"
if ($LASTEXITCODE -ne 0) { throw 'instrumented build failed' }

Write-Host '== 2/4 collect a profile under load ==' -ForegroundColor Cyan
& docker rm -f pgo-gw 2>&1 | Out-Null
& docker volume rm -f $Volume 2>&1 | Out-Null
& docker volume create $Volume 2>&1 | Out-Null
& docker build -f (Join-Path $Root 'spire/Dockerfile') --build-arg BINARY=spire-pgogen -t perf-spire:pgogen $Root 2>&1 | Out-Null
& docker compose -f $Compose down -v --remove-orphans 2>&1 | Out-Null
& docker compose -f $Compose up -d backend 2>&1 | Out-Null
& docker run -d --name pgo-gw --network $Network --cpuset-cpus $Cpuset --memory 2g `
    --volume "${Volume}:/work/pgo" `
    --volume "${binPosix}:/out" `
    -e 'LLVM_PROFILE_FILE=/work/pgo/spire-%p.profraw' `
    --entrypoint /usr/local/bin/spire `
    perf-spire:pgogen -f /etc/spire/config.yaml 2>&1 | Out-Null
# The instrumented image has no config baked in; mount it after creation.
& docker rm -f pgo-gw 2>&1 | Out-Null
& docker run -d --name pgo-gw --network $Network --cpuset-cpus $Cpuset --memory 2g `
    --volume "${Volume}:/work/pgo" `
    --volume "$($Root -replace '\\', '/')/config/spire-bench.yaml:/etc/spire/config.yaml:ro" `
    -e 'LLVM_PROFILE_FILE=/work/pgo/spire-%p.profraw' `
    perf-spire:pgogen 2>&1 | Out-Null
for ($i = 0; $i -lt 60; $i++) {
    & docker run --rm --network $Network curlimages/curl:latest -sf -o NUL --max-time 2 'http://pgo-gw:6667/api/1' 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { break }
    Start-Sleep -Milliseconds 500
}
& docker run --rm --network $Network --cpuset-cpus '0,2' --entrypoint hey $HeyImage `
    -z "${LoadSeconds}s" -c $Concurrency 'http://pgo-gw:6667/api/1' 2>&1 | Select-String 'Requests/sec' | ForEach-Object { $_.Line }
# SIGTERM -> Spire's graceful shutdown -> LLVM's atexit hook flushes the profile.
& docker stop -t 20 pgo-gw 2>&1 | Out-Null
& docker rm -f pgo-gw 2>&1 | Out-Null

Write-Host '== 3/4 merge and rebuild with the profile ==' -ForegroundColor Cyan
& docker run --rm --volume "${repoPosix}:/src" --volume "${Volume}:/work/pgo" --volume "${binPosix}:/out" -w /src/rust-proxy `
    -e "HTTP_PROXY=$Proxy" -e "HTTPS_PROXY=$Proxy" `
    $Toolchain bash -c "set -e
export PATH=/usr/local/cargo/bin:`$PATH
rustup component add llvm-tools >/dev/null 2>&1 || true
LLVM_PROFDATA=`$(find /usr/local/rustup -name llvm-profdata | head -1)
ls -l /work/pgo
`$LLVM_PROFDATA merge -o /work/pgo/default.profdata /work/pgo/*.profraw
export CARGO_PROFILE_RELEASE_LTO=fat CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
RUSTFLAGS='-Cprofile-use=/work/pgo/default.profdata -Cllvm-args=-pgo-warn-missing-function' \
  cargo build --release --target $Target 2>&1 | tail -2
cp target/$Target/release/spire /out/spire-pgo"
if ($LASTEXITCODE -ne 0) { throw 'profile-use build failed' }

Write-Host '== 4/4 bake the image ==' -ForegroundColor Cyan
& docker build -f (Join-Path $Root 'spire/Dockerfile') --build-arg BINARY=spire-pgo -t perf-spire:pgo $Root 2>&1 | Out-Null
& docker compose -f $Compose down -v --remove-orphans 2>&1 | Out-Null

Get-ChildItem (Join-Path $Bin 'spire-pgo') | Select-Object Name, Length
Write-Host 'PGO build ready: perf-spire:pgo' -ForegroundColor Green
