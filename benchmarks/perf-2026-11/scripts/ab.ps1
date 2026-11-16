#Requires -Version 7.0
<#
.SYNOPSIS
  Fair, interleaved comparison of Spire and nginx in one session.

.DESCRIPTION
  Host noise on this machine drifts by tens of percent between minutes, so a
  gateway measured ten minutes after its competitor is not comparable. This
  script brings both gateways up on the same cpuset and interleaves the runs:

      ceiling -> spire -> nginx -> ceiling

  and reports each gateway normalised by the mean of the two ceiling runs. A
  ratio above 1.0 means the gateway beat the bare backend measurement, which is
  impossible in practice, so it also serves as a sanity check on noise.

.EXAMPLE
  ./ab.ps1 -Cpuset '4' -Concurrency 64 -Seconds 20
#>
param(
    [string]$Cpuset = '4',
    [int]$Concurrency = 64,
    [int]$Seconds = 20,
    [string]$Tag = 'ab'
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$Compose = Join-Path $Root 'compose/stack.yaml'
$Results = Join-Path $Root 'results'
$Network = 'perf-2026-11_bench'
$HeyImage = 'demisto/rakyll-hey:1.0.0.44140'
$ClientCpuset = '0,2'
New-Item -ItemType Directory -Force -Path $Results | Out-Null

function Get-Hey([string]$Url) {
    $out = & docker run --rm --network $Network --cpuset-cpus $ClientCpuset --entrypoint hey $HeyImage `
        -z "${Seconds}s" -c $Concurrency $Url 2>&1
    $line = ($out | Select-String 'Requests/sec').Line
    if (-not $line) { return 0.0 }
    return [double](($line -replace '.*Requests/sec:\s*', '') -replace '\s.*', '')
}

& docker compose -f $Compose down -v --remove-orphans 2>&1 | Out-Null
$env:GATEWAY_CPUSET = $Cpuset
$env:SPIRE_CONFIG = 'spire-bench.yaml'
& docker compose -f $Compose --profile spire --profile nginx up -d --remove-orphans backend spire nginx 2>&1 | Out-Null

foreach ($url in @('http://spire:6667/api/1', 'http://nginx:80/api/1')) {
    for ($i = 0; $i -lt 60; $i++) {
        & docker run --rm --network $Network curlimages/curl:latest -sf -o NUL --max-time 2 $url 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) { break }
        Start-Sleep -Milliseconds 500
    }
}

& docker run --rm --network $Network --cpuset-cpus $ClientCpuset --entrypoint hey $HeyImage -z 5s -c $Concurrency 'http://backend:8080/api/1' 2>&1 | Out-Null

$ceil1 = Get-Hey 'http://backend:8080/api/1'
$spire = Get-Hey 'http://spire:6667/api/1'
$nginx = Get-Hey 'http://nginx:80/api/1'
$ceil2 = Get-Hey 'http://backend:8080/api/1'
$ceiling = ($ceil1 + $ceil2) / 2

$sRatio = if ($ceiling -gt 0) { $spire / $ceiling } else { 0 }
$nRatio = if ($ceiling -gt 0) { $nginx / $ceiling } else { 0 }
$head = if ($nginx -gt 0) { $spire / $nginx } else { 0 }

Write-Host ("cpuset={0} c={1} s={2}s | ceiling {3:N0} / {4:N0}" -f $Cpuset, $Concurrency, $Seconds, $ceil1, $ceil2) -ForegroundColor DarkGray
Write-Host ("spire {0:N0} rps (ratio {1:N3})   nginx {2:N0} rps (ratio {3:N3})   spire/nginx {4:N3}" -f `
        $spire, $sRatio, $nginx, $nRatio, $head) -ForegroundColor Green

$csv = Join-Path $Results 'ab.csv'
if (-not (Test-Path $csv)) {
    'tag,cpuset,concurrency,seconds,spire_rps,nginx_rps,ceiling_rps,spire_ratio,nginx_ratio,spire_over_nginx,ts' |
        Set-Content $csv -Encoding utf8
}
'{0},{1},{2},{3},{4:N1},{5:N1},{6:N1},{7:N3},{8:N3},{9:N3},{10}' -f `
    $Tag, $Cpuset, $Concurrency, $Seconds, $spire, $nginx, $ceiling, $sRatio, $nRatio, $head, (Get-Date -Format 'HH:mm:ss') |
    Add-Content $csv -Encoding utf8

& docker compose -f $Compose --profile spire --profile nginx down -v --remove-orphans 2>&1 | Out-Null
