#Requires -Version 7.0
<#
.SYNOPSIS
  Interleaved Spire-vs-nginx comparison on a stable metric: CPU per request.

.DESCRIPTION
  Wall-clock RPS on this machine drifts by tens of percent between minutes
  (host browsers, WSL2 scheduling), which makes a throughput target impossible
  to hit reliably. CPU time per request is intrinsic to the code and barely
  moves, so it is the metric to optimise against:

      cpu_us_per_request = cgroup usage_usec delta / requests handled

  Both gateways are exercised back to back in one session, and RPS is recorded
  alongside as a sanity check.

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
    $rpsLine = ($out | Select-String 'Requests/sec').Line
    $rps = if ($rpsLine) { [double](($rpsLine -replace '.*Requests/sec:\s*', '') -replace '\s.*', '') } else { 0.0 }
    $total = 0.0
    $doneLine = ($out | Select-String 'Total:').Line
    if ($doneLine -match 'Total:\s*([\d.]+) secs') { $total = [double]$Matches[1] }
    return @{ rps = $rps; seconds = $total }
}

function Get-CpuUsec([string]$Container) {
    $raw = & docker exec $Container cat /sys/fs/cgroup/cpu.stat 2>&1
    foreach ($line in $raw) {
        if ($line -match '^usage_usec\s+(\d+)') { return [int64]$Matches[1] }
    }
    return [int64]0
}

function Measure-Gateway([string]$Container, [string]$Url) {
    $before = Get-CpuUsec $Container
    $res = Get-Hey $Url
    $after = Get-CpuUsec $Container
    $requests = [math]::Round($res.rps * $res.seconds)
    $usPerReq = 0.0
    if ($requests -gt 0) { $usPerReq = (($after - $before) / $requests) }
    return @{ rps = $res.rps; requests = $requests; us_per_req = $usPerReq }
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

$spireId = (& docker compose -f $Compose ps -q spire).Trim()
$nginxId = (& docker compose -f $Compose ps -q nginx).Trim()

$s1 = Measure-Gateway $spireId 'http://spire:6667/api/1'
$n1 = Measure-Gateway $nginxId 'http://nginx:80/api/1'
$s2 = Measure-Gateway $spireId 'http://spire:6667/api/1'
$n2 = Measure-Gateway $nginxId 'http://nginx:80/api/1'

$sUs = ($s1.us_per_req + $s2.us_per_req) / 2
$nUs = ($n1.us_per_req + $n2.us_per_req) / 2
$sRps = [math]::Max($s1.rps, $s2.rps)
$nRps = [math]::Max($n1.rps, $n2.rps)

Write-Host ("cpuset={0} c={1} s={2}s" -f $Cpuset, $Concurrency, $Seconds) -ForegroundColor DarkGray
Write-Host ("spire {0:N1} us/req  {1:N0} rps   |   nginx {2:N1} us/req  {3:N0} rps   |   cpu ratio {4:N3}" -f `
        $sUs, $sRps, $nUs, $nRps, ($(if ($nUs -gt 0) { $sUs / $nUs } else { 0 }))) -ForegroundColor Green

$csv = Join-Path $Results 'ab.csv'
# Export-Csv rather than string concatenation: the cpuset field contains commas
# ("4,6") and this machine's locale would otherwise render 62529.6 as "62,529.6",
# both of which silently shift every column for any standard CSV reader.
$row = [pscustomobject]@{
    tag              = $Tag
    cpuset           = $Cpuset
    concurrency      = $Concurrency
    seconds          = $Seconds
    spire_us_per_req = ('{0:F2}' -f $sUs)
    nginx_us_per_req = ('{0:F2}' -f $nUs)
    cpu_ratio        = ('{0:F3}' -f ($(if ($nUs -gt 0) { $sUs / $nUs } else { 0 })))
    spire_rps        = ('{0:F1}' -f $sRps)
    nginx_rps        = ('{0:F1}' -f $nRps)
    throughput_ratio = ('{0:F3}' -f ($(if ($nRps -gt 0) { $sRps / $nRps } else { 0 })))
    ts               = (Get-Date -Format 'HH:mm:ss')
}
if (Test-Path $csv) {
    $row | Export-Csv -Path $csv -Append -NoTypeInformation -Encoding utf8
} else {
    $row | Export-Csv -Path $csv -NoTypeInformation -Encoding utf8
}

& docker compose -f $Compose --profile spire --profile nginx down -v --remove-orphans 2>&1 | Out-Null
