#Requires -Version 7.0
<#
.SYNOPSIS
  Chaos experiment: one of ten downstream endpoints is slowed by chaosblade.

.DESCRIPTION
  Phase flow (run them in order, or use -Phase all):

    setup     start backend + spire on the 4-core tier and copy chaosblade 1.7.3
              into the backend container
    baseline  k6 mixed traffic, no fault injected
    inject    `blade create jvm delay --time 2000 --classname Backend$E7` and
              re-run the same mixed traffic while the fault is active
    timeout   restart spire with the 300ms route timeout and repeat the
              injected run, to observe 504 behaviour instead of slow responses
    destroy   remove any experiment that is still active
    down      tear the stack down

  The traffic mix is fixed by k6/mixed.js: ~10% of requests hit /api/7, the
  other 90% are spread over /api/1..10 (minus 7). k6 reports per-endpoint
  latency, so the headline metric is how much the nine healthy endpoints
  degrade while the seventh is slow.
#>
param(
    [ValidateSet('setup', 'baseline', 'inject', 'timeout', 'destroy', 'down', 'all')]
    [string]$Phase = 'all',

    [int]$Rate = 1000,
    [string]$Duration = '60s',
    [int]$DelayMs = 2000,
    [string]$TargetClass = 'Backend$E7',
    [string]$TargetMethod = 'serve',
    [string]$GatewayCpuset = '4,6,8,10'
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$Compose = Join-Path $Root 'compose/stack.yaml'
$Results = Join-Path $Root 'results'
$K6Dir = Join-Path $Root 'k6'
$ChaosDir = Join-Path $Root 'chaos'
$Network = 'perf-2026-11_bench'
$ClientCpuset = '0,2'
$Blade = '/chaos/chaosblade-1.7.3/blade'
$BaseUrl = 'http://spire:6667'

New-Item -ItemType Directory -Force -Path $Results | Out-Null

function Invoke-Compose([string[]]$Arguments) {
    & docker compose -f $Compose @Arguments
    if ($LASTEXITCODE -ne 0) { throw "docker compose $($Arguments -join ' ') failed" }
}

function Get-BackendId {
    $id = (& docker compose -f $Compose ps -q backend 2>&1).Trim()
    if (-not $id) { throw 'backend container is not running' }
    return $id
}

function Start-Stack([string]$Config) {
    $env:GATEWAY_CPUSET = $GatewayCpuset
    $env:SPIRE_CONFIG = $Config
    Invoke-Compose @('up', '-d', '--build', 'backend', 'spire')
    for ($i = 0; $i -lt 60; $i++) {
        & docker run --rm --network $Network curlimages/curl:latest -sf -o NUL --max-time 2 "$BaseUrl/api/1" 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) { return }
        Start-Sleep -Milliseconds 500
    }
    throw 'spire did not become ready'
}

function Install-Blade {
    $id = Get-BackendId
    & docker exec $id sh -c 'mkdir -p /chaos' | Out-Null
    & docker cp (Join-Path $ChaosDir 'chaosblade-1.7.3') "${id}:/chaos/" | Out-Null
    & docker exec $id chmod +x $Blade | Out-Null
    $version = & docker exec $id $Blade version 2>&1
    Write-Host ($version | Select-Object -First 3 | Out-String).Trim() -ForegroundColor DarkGray
}

function Invoke-K6([string]$Tag) {
    $json = Join-Path $Results "$Tag.json"
    & docker run --rm --network $Network --cpuset-cpus $ClientCpuset `
        -v "${K6Dir}:/scripts:ro" -v "${Results}:/out" `
        -e "BASE=$BaseUrl" -e "RATE=$Rate" -e "DURATION=$Duration" -e "TAG=$Tag" -e 'TARGET=7' `
        grafana/k6:latest run /scripts/mixed.js
    if ($LASTEXITCODE -ne 0) { Write-Warning "k6 exited with $LASTEXITCODE for $Tag" }
    return $json
}

function New-DelayExperiment {
    $id = Get-BackendId
    $out = & docker exec $id $Blade create jvm delay --time $DelayMs --classname "$TargetClass" --methodname $TargetMethod --pid 1 2>&1
    Write-Host ($out | Out-String).Trim() -ForegroundColor Yellow
    if (($out | Out-String) -match '"code":(\d+)') {
        $code = [int]$Matches[1]
        if ($code -ne 200) { Write-Warning "blade returned code $code; the experiment may not be active" }
    }
}

function Remove-DelayExperiment {
    $id = Get-BackendId
    & docker exec $id $Blade destroy jvm delay --time $DelayMs --classname "$TargetClass" --methodname $TargetMethod --pid 1 2>&1 |
        ForEach-Object { Write-Host $_ -ForegroundColor DarkGray }
}

if ($Phase -in @('setup', 'all')) {
    Start-Stack 'spire-bench.yaml'
    Install-Blade
}

if ($Phase -in @('baseline', 'all')) {
    Write-Host '== baseline (no fault) ==' -ForegroundColor Cyan
    Invoke-K6 'chaos-baseline'
}

if ($Phase -in @('inject', 'all')) {
    Write-Host '== injected (2s on /api/7, default 5s upstream timeout) ==' -ForegroundColor Cyan
    New-DelayExperiment
    Invoke-K6 'chaos-injected'
    Remove-DelayExperiment
}

if ($Phase -in @('timeout', 'all')) {
    Write-Host '== injected (2s on /api/7, gateway timeout 300ms) ==' -ForegroundColor Cyan
    & docker compose -f $Compose --profile spire down -v --remove-orphans 2>&1 | Out-Null
    Start-Stack 'spire-chaos-timeout.yaml'
    Install-Blade
    New-DelayExperiment
    Invoke-K6 'chaos-timeout-300ms'
    Remove-DelayExperiment
}

if ($Phase -in @('destroy', 'all')) {
    try { Remove-DelayExperiment } catch { Write-Warning "destroy skipped: $_" }
}

if ($Phase -in @('down', 'all')) {
    & docker compose -f $Compose --profile spire --profile nginx down -v --remove-orphans 2>&1 | Out-Null
}
