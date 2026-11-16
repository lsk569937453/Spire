#Requires -Version 7.0
<#
.SYNOPSIS
  Captures a CPU flamegraph of Spire under load with perf + flamegraph-rs.

.DESCRIPTION
  WSL2 exposes no hardware PMU, so perf is driven with the software event
  `cpu-clock` at 997Hz. perf must live in the same PID namespace as Spire, which
  is why the tool container joins it with --pid=container:<spire> instead of
  making the measured image carry profiling tools.

  The profiled binary is `spire-symbols` (release + debug info, no pprof
  feature) so the frame names resolve without changing the measured code path.

.EXAMPLE
  ./flamegraph.ps1 -Cpuset '4,6,8,10' -Concurrency 256 -Seconds 50 -Out spire-4core.svg
#>
param(
    [string]$Cpuset = '4,6,8,10',
    [int]$Concurrency = 256,
    [int]$Seconds = 50,
    [int]$Warmup = 6,
    [string]$Out = 'spire-4core.svg',
    [string]$Image = 'perf-spire:symbols',
    [switch]$KeepRunning
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$Network = 'perf-2026-11_bench'
$Results = Join-Path $Root 'results'
$Flamegraphs = Join-Path $Results 'flamegraphs'
New-Item -ItemType Directory -Force -Path $Flamegraphs | Out-Null

& docker rm -f spire-prof hey-prof 2>&1 | Out-Null

# The bridge network only exists while the compose project is up.
$env:GATEWAY_CPUSET = $Cpuset
& docker compose -f (Join-Path $Root 'compose/stack.yaml') up -d backend 2>&1 | Out-Null

& docker run -d --name spire-prof --network $Network --cpuset-cpus $Cpuset --memory 2g `
    --volume "$($Root -replace '\\', '/')/config/spire-bench.yaml:/etc/spire/config.yaml:ro" `
    $Image 2>&1 | Out-Null

for ($i = 0; $i -lt 40; $i++) {
    & docker run --rm --network $Network curlimages/curl:latest -sf -o NUL --max-time 2 'http://spire-prof:6667/api/1' 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { break }
    Start-Sleep -Milliseconds 500
}
Write-Host "spire-prof ready on cpuset=$Cpuset" -ForegroundColor DarkGray

& docker run -d --name hey-prof --network $Network --cpuset-cpus '0,2' --entrypoint hey `
    demisto/rakyll-hey:1.0.0.44140 -z "$($Seconds + $Warmup + 15)s" -c $Concurrency 'http://spire-prof:6667/api/1' 2>&1 | Out-Null
Start-Sleep -Seconds $Warmup

& docker run --rm --pid=container:spire-prof --privileged `
    --volume "$($Flamegraphs -replace '\\', '/'):/out" `
    perf-tool:6.8 -c "timeout -s INT $Seconds flamegraph --pid 1 --freq 997 -o /out/$Out" 2>&1 |
    Select-Object -Last 8

Get-ChildItem (Join-Path $Flamegraphs $Out) | Select-Object Name, Length
Write-Host "hey summary:" -ForegroundColor DarkGray
& docker logs hey-prof 2>&1 | Select-String 'Requests/sec|Average' | ForEach-Object { $_.Line }

& docker rm -f hey-prof 2>&1 | Out-Null
if (-not $KeepRunning) {
    & docker rm -f spire-prof 2>&1 | Out-Null
    & docker compose -f (Join-Path $Root 'compose/stack.yaml') down -v --remove-orphans 2>&1 | Out-Null
}
