#Requires -Version 7.0
<#
.SYNOPSIS
  Splits a gateway's CPU per request into user and system time.

.DESCRIPTION
  cgroup cpu.stat gives one number; the interesting question for "why is Spire
  slower per request than nginx" is whether the extra time is userspace work or
  kernel work. /proc/<pid>/stat fields 14/15 (utime/stime, in clock ticks) are
  exact and cheap, so this sums them over every process in the container
  (nginx runs a master plus workers) before and after a fixed load.

  A tick is 10ms, so with a 20s saturated run (~2000 ticks) the resolution is
  around 0.5% - enough to make the call.

.EXAMPLE
  ./cpu-split.ps1 -Gateway spire -Cpuset '4' -Concurrency 64 -Seconds 20
#>
param(
    [ValidateSet('spire', 'nginx')]
    [string]$Gateway = 'spire',
    [string]$Cpuset = '4',
    [int]$Concurrency = 64,
    [int]$Seconds = 20
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Compose = Join-Path $Root 'compose/stack.yaml'
$Network = 'perf-2026-11_bench'
$HeyImage = 'demisto/rakyll-hey:1.0.0.44140'

& docker compose -f $Compose down -v --remove-orphans 2>&1 | Out-Null
$env:GATEWAY_CPUSET = $Cpuset
$env:SPIRE_CONFIG = 'spire-bench.yaml'
& docker compose -f $Compose --profile $Gateway up -d --remove-orphans backend $Gateway 2>&1 | Out-Null

$url = if ($Gateway -eq 'spire') { 'http://spire:6667/api/1' } else { 'http://nginx:80/api/1' }
for ($i = 0; $i -lt 60; $i++) {
    & docker run --rm --network $Network curlimages/curl:latest -sf -o NUL --max-time 2 $url 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { break }
    Start-Sleep -Milliseconds 500
}
$id = (& docker compose -f $Compose ps -q $Gateway).Trim()

function Get-Ticks([string]$Container) {
    $out = & docker exec $Container sh -c 'cat /proc/[0-9]*/stat 2>/dev/null | awk "{u+=\$14; s+=\$15} END {print u+0, s+0}"' 2>&1
    $parts = ("$out".Trim() -split '\s+')
    if ($parts.Count -lt 2) { return @{ u = 0; s = 0 } }
    return @{ u = [double]$parts[0]; s = [double]$parts[1] }
}

& docker run --rm --network $Network --cpuset-cpus '0,2' --entrypoint hey $HeyImage -z 5s -c $Concurrency 'http://backend:8080/api/1' 2>&1 | Out-Null

$before = Get-Ticks $id
$hey = & docker run --rm --network $Network --cpuset-cpus '0,2' --entrypoint hey $HeyImage -z "${Seconds}s" -c $Concurrency $url 2>&1
$after = Get-Ticks $id

$rpsLine = ($hey | Select-String 'Requests/sec').Line
$rps = if ($rpsLine) { [double](($rpsLine -replace '.*Requests/sec:\s*', '') -replace '\s.*', '') } else { 0.0 }
$totalLine = ($hey | Select-String 'Total:').Line
$wall = 0.0
if ($totalLine -match 'Total:\s*([\d.]+) secs') { $wall = [double]$Matches[1] }
$requests = [math]::Round($rps * $wall)

# clock ticks per second on Linux is USER_HZ = 100
$usPerTick = 10000.0
$du = ($after.u - $before.u) * $usPerTick
$ds = ($after.s - $before.s) * $usPerTick

$u = if ($requests -gt 0) { $du / $requests } else { 0 }
$s = if ($requests -gt 0) { $ds / $requests } else { 0 }
Write-Host ("{0,-6} cpuset={1,-12} c={2,-4} requests={3,9:N0}  user={4,7:N1} us  sys={5,7:N1} us  total={6,7:N1} us/req" -f `
        $Gateway, $Cpuset, $Concurrency, $requests, $u, $s, ($u + $s)) -ForegroundColor Green

$csv = Join-Path $Root 'results/cpu-split.csv'
if (-not (Test-Path $csv)) {
    'gateway,cpuset,concurrency,requests,user_us,sys_us,total_us,ts' | Set-Content $csv -Encoding utf8
}
'{0},{1},{2},{3},{4:N2},{5:N2},{6:N2},{7}' -f $Gateway, $Cpuset, $Concurrency, $requests, $u, $s, ($u + $s), (Get-Date -Format 'HH:mm:ss') |
    Add-Content $csv -Encoding utf8

& docker compose -f $Compose --profile spire --profile nginx down -v --remove-orphans 2>&1 | Out-Null
