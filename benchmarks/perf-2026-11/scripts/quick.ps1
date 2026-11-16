#Requires -Version 7.0
<#
.SYNOPSIS
  Fast, noise-resistant A/B measurement: gateway vs bare backend, in one session.

.DESCRIPTION
  Absolute RPS on this machine drifts (host browsers, WSL2 scheduling, the
  shared bridge), so optimisation decisions are made on the ratio

      gateway RPS / bare-backend RPS

  measured back to back in the same container session. The bare-backend number
  is the client+backend ceiling for that moment, so a ratio improvement means
  the gateway really got faster rather than the host getting quieter.

.EXAMPLE
  ./quick.ps1 -Gateway spire -Cpuset '4' -Concurrency 64 -Seconds 20
  ./quick.ps1 -Gateway nginx -Cpuset '4' -Concurrency 64 -Seconds 20
#>
param(
    [ValidateSet('spire', 'nginx')]
    [string]$Gateway = 'spire',
    [string]$Cpuset = '4',
    [int]$Concurrency = 64,
    [int]$Seconds = 20,
    [string]$Tag = 'quick'
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$Compose = Join-Path $Root 'compose/stack.yaml'
$Results = Join-Path $Root 'results'
$Network = 'perf-2026-11_bench'
$HeyImage = 'demisto/rakyll-hey:1.0.0.44140'
$ClientCpuset = '0,2'

New-Item -ItemType Directory -Force -Path $Results | Out-Null

function Get-HeyRps([string]$Url) {
    $out = & docker run --rm --network $Network --cpuset-cpus $ClientCpuset --entrypoint hey $HeyImage `
        -z "${Seconds}s" -c $Concurrency $Url 2>&1
    $rps = ($out | Select-String 'Requests/sec').Line
    $avg = ($out | Select-String 'Average:').Line
    if (-not $rps) { return @{ rps = 0.0; line = 'FAILED' } }
    return @{
        rps  = [double](($rps -replace '.*Requests/sec:\s*', '') -replace '\s.*', '')
        line = (($rps + ' ' + ($avg -replace '\s+', ' ')).Trim())
    }
}

& docker compose -f $Compose down -v --remove-orphans 2>&1 | Out-Null
$env:GATEWAY_CPUSET = $Cpuset
& docker compose -f $Compose --profile $Gateway up -d --remove-orphans backend $Gateway 2>&1 | Out-Null

$port = if ($Gateway -eq 'spire') { 'http://spire:6667' } else { 'http://nginx:80' }
for ($i = 0; $i -lt 60; $i++) {
    & docker run --rm --network $Network curlimages/curl:latest -sf -o NUL --max-time 2 "$port/api/1" 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { break }
    Start-Sleep -Milliseconds 500
}

# Warm both paths, then measure the ceiling and the gateway back to back.
& docker run --rm --network $Network --cpuset-cpus $ClientCpuset --entrypoint hey $HeyImage `
    -z 5s -c $Concurrency 'http://backend:8080/api/1' 2>&1 | Out-Null

$ceiling = Get-HeyRps 'http://backend:8080/api/1'
$gate = Get-HeyRps "$port/api/1"
$ratio = if ($ceiling.rps -gt 0) { $gate.rps / $ceiling.rps } else { 0 }

$line = '{0},{1},{2},{3},{4:N1},{5:N1},{6:N3},{7}' -f `
    $Tag, $Gateway, $Cpuset, $Concurrency, $gate.rps, $ceiling.rps, $ratio, (Get-Date -Format 'HH:mm:ss')
$csv = Join-Path $Results 'quick.csv'
if (-not (Test-Path $csv)) {
    'tag,gateway,cpuset,concurrency,gateway_rps,ceiling_rps,ratio,ts' | Set-Content $csv -Encoding utf8
}
Add-Content $csv $line -Encoding utf8
Write-Host $line -ForegroundColor Green
Write-Host "  gateway: $($gate.line)" -ForegroundColor DarkGray
Write-Host "  ceiling: $($ceiling.line)" -ForegroundColor DarkGray

& docker compose -f $Compose --profile spire --profile nginx down -v --remove-orphans 2>&1 | Out-Null
