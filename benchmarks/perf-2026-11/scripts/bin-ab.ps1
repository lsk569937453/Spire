#Requires -Version 7.0
<#
.SYNOPSIS
  Compares two Spire builds in one session, interleaved, on CPU per request.

.DESCRIPTION
  Build-flag experiments (thin vs fat LTO, native vs generic) differ by single
  digit percentages, which host noise easily swallows when the two builds are
  measured minutes apart. This script alternates A,B,A,B within one session and
  reports the median CPU per request for each, using /proc utime+stime so the
  number does not depend on the wall-clock throughput of that moment.

.EXAMPLE
  ./bin-ab.ps1 -ImageA perf-spire:fat -ImageB perf-spire:native -Cpuset '4' -Rounds 3
#>
param(
    [string]$ImageA = 'perf-spire:fat',
    [string]$ImageB = 'perf-spire:native',
    [string]$Cpuset = '4',
    [int]$Concurrency = 64,
    [int]$Seconds = 15,
    [int]$Rounds = 3,
    [string]$Config = 'spire-bench.yaml'
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$Compose = Join-Path $Root 'compose/stack.yaml'
$Network = 'perf-2026-11_bench'
$HeyImage = 'demisto/rakyll-hey:1.0.0.44140'

function Get-Ticks([string]$Container) {
    $out = & docker exec $Container sh -c 'cat /proc/[0-9]*/stat 2>/dev/null | awk "{u+=\$14; s+=\$15} END {print u+0, s+0}"' 2>&1
    $parts = ("$out".Trim() -split '\s+')
    if ($parts.Count -lt 2) { return @{ u = 0.0; s = 0.0 } }
    return @{ u = [double]$parts[0]; s = [double]$parts[1] }
}

function Measure-Image([string]$Image, [string]$Name) {
    & docker rm -f $Name 2>&1 | Out-Null
    & docker run -d --name $Name --network $Network --cpuset-cpus $Cpuset --memory 2g `
        --volume "$($Root -replace '\\', '/')/config/${Config}:/etc/spire/config.yaml:ro" $Image 2>&1 | Out-Null
    for ($i = 0; $i -lt 60; $i++) {
        & docker run --rm --network $Network curlimages/curl:latest -sf -o NUL --max-time 2 "http://${Name}:6667/api/1" 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) { break }
        Start-Sleep -Milliseconds 300
    }
    & docker run --rm --network $Network --cpuset-cpus '0,2' --entrypoint hey $HeyImage -z 3s -c $Concurrency "http://${Name}:6667/api/1" 2>&1 | Out-Null

    $before = Get-Ticks $Name
    $hey = & docker run --rm --network $Network --cpuset-cpus '0,2' --entrypoint hey $HeyImage -z "${Seconds}s" -c $Concurrency "http://${Name}:6667/api/1" 2>&1
    $after = Get-Ticks $Name
    & docker rm -f $Name 2>&1 | Out-Null

    $rps = 0.0
    $hop = ($hey | Select-String 'Requests/sec').Line
    if ($hop -match 'Requests/sec:\s*([\d.]+)') { $rps = [double]$Matches[1] }
    $wall = 0.0
    $total = ($hey | Select-String 'Total:').Line
    if ($total -match 'Total:\s*([\d.]+) secs') { $wall = [double]$Matches[1] }
    $requests = [math]::Round($rps * $wall)
    $usPerTick = 10000.0
    $u = ($after.u - $before.u) * $usPerTick
    $s = ($after.s - $before.s) * $usPerTick
    if ($requests -le 0) { return @{ user = 0.0; sys = 0.0; rps = 0.0 } }
    return @{ user = $u / $requests; sys = $s / $requests; rps = $rps }
}

& docker compose -f $Compose down -v --remove-orphans 2>&1 | Out-Null
& docker compose -f $Compose up -d backend 2>&1 | Out-Null

$a = @()
$b = @()
for ($round = 1; $round -le $Rounds; $round++) {
    $ra = Measure-Image $ImageA 'spire-a'
    $rb = Measure-Image $ImageB 'spire-b'
    $a += $ra
    $b += $rb
    Write-Host ("round {0}: A user={1,6:N1} sys={2,6:N1} | B user={3,6:N1} sys={4,6:N1}" -f `
            $round, $ra.user, $ra.sys, $rb.user, $rb.sys) -ForegroundColor DarkGray
}

function Median($values) {
    $sorted = $values | Sort-Object
    $mid = [int][math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2 -eq 1) { return $sorted[$mid] }
    return ($sorted[$mid - 1] + $sorted[$mid]) / 2
}

$aUser = Median ($a | ForEach-Object { $_.user })
$bUser = Median ($b | ForEach-Object { $_.user })
$aSys = Median ($a | ForEach-Object { $_.sys })
$bSys = Median ($b | ForEach-Object { $_.sys })

Write-Host ""
Write-Host ("A={0}: user {1:N1} us  sys {2:N1} us" -f $ImageA, $aUser, $aSys) -ForegroundColor Green
Write-Host ("B={0}: user {1:N1} us  sys {2:N1} us" -f $ImageB, $bUser, $bSys) -ForegroundColor Green
Write-Host ("B/A user ratio {0:N3}" -f ($(if ($aUser -gt 0) { $bUser / $aUser } else { 0 })))

$csv = Join-Path $Root 'results/bin-ab.csv'
# Same CSV-hygiene reason as ab.ps1: quote the cpuset and avoid locale grouping.
$row = [pscustomobject]@{
    image_a    = $ImageA
    image_b    = $ImageB
    cpuset     = $Cpuset
    concurrency = $Concurrency
    a_user_us  = ('{0:F2}' -f $aUser)
    a_sys_us   = ('{0:F2}' -f $aSys)
    b_user_us  = ('{0:F2}' -f $bUser)
    b_sys_us   = ('{0:F2}' -f $bSys)
    b_over_a   = ('{0:F3}' -f ($(if ($aUser -gt 0) { $bUser / $aUser } else { 0 })))
    ts         = (Get-Date -Format 'HH:mm:ss')
}
if (Test-Path $csv) {
    $row | Export-Csv -Path $csv -Append -NoTypeInformation -Encoding utf8
} else {
    $row | Export-Csv -Path $csv -NoTypeInformation -Encoding utf8
}

& docker compose -f $Compose down -v --remove-orphans 2>&1 | Out-Null
