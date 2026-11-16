#Requires -Version 7.0
<#
.SYNOPSIS
  Runs the Spire vs nginx benchmark matrix for the perf-2026-11 report.

.DESCRIPTION
  For every (gateway, CPU tier, concurrency, repeat) combination it starts the
  gateway plus backend, warms up, runs `hey` for N seconds, snapshots the
  gateway's cgroup CPU accounting and (for Spire) /metrics, then tears the stack
  down. Every run is written to results/raw so the report can be regenerated
  without re-running the benchmark.

  Concurrency is the only knob varied inside a tier: each level is a separate
  CSV, and the saturation point is where p99 breaks away from p50.

.EXAMPLE
  ./run.ps1 -Smoke                      # 1 tier, 1 gateway, 1 level, 10s
  ./run.ps1                             # the full matrix
  ./run.ps1 -Gateways nginx -Tiers 4    # one cell only
#>
param(
    [ValidateSet('1', '2', '4')]
    [string[]]$Tiers = @('1', '2', '4'),

    [ValidateSet('spire', 'nginx')]
    [string[]]$Gateways = @('spire', 'nginx'),

    [int[]]$Concurrency = @(1, 16, 64, 256, 1024),

    [int]$Repeats = 3,
    [int]$Seconds = 60,
    [int]$Warmup = 10,
    [string]$Tag = 'matrix',
    [switch]$SkipBuild,
    [switch]$Smoke
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$Compose = Join-Path $Root 'compose/stack.yaml'
$Results = Join-Path $Root 'results'
$Raw = Join-Path $Results 'raw'
$Network = 'perf-2026-11_bench'
$ClientCpuset = '0,2'
$HeyImage = 'demisto/rakyll-hey:1.0.0.44140'
$CurlImage = 'curlimages/curl:latest'

if ($Smoke) {
    $Tiers = @('1')
    $Gateways = @('spire')
    $Concurrency = @(16)
    $Repeats = 1
    $Seconds = 10
    $Warmup = 3
    $Tag = 'smoke'
}

New-Item -ItemType Directory -Force -Path $Raw | Out-Null

function Get-Cpuset([string]$Tier) {
    switch ($Tier) {
        '1' { return '4' }
        '2' { return '4,6' }
        '4' { return '4,6,8,10' }
    }
    throw "unsupported tier $Tier"
}

function Get-BaseUrl([string]$Gateway) {
    if ($Gateway -eq 'spire') { return 'http://spire:6667' }
    return 'http://nginx:80'
}

function Invoke-Compose([string[]]$Arguments) {
    & docker compose -f $Compose @Arguments
    if ($LASTEXITCODE -ne 0) { throw "docker compose $($Arguments -join ' ') failed with $LASTEXITCODE" }
}

function Start-Stack([string]$Gateway, [string]$Cpuset) {
    $env:GATEWAY_CPUSET = $Cpuset
    $upArgs = @('up', '-d', '--remove-orphans')
    if (-not $SkipBuild) { $upArgs += '--build' }
    $upArgs += @('backend', $Gateway)
    Invoke-Compose $upArgs
}

function Stop-Stack {
    & docker compose -f $Compose --profile spire --profile nginx --profile client --profile chaos down -v --remove-orphans 2>&1 | Out-Null
}

function Wait-Ready([string]$Gateway) {
    $url = "$(Get-BaseUrl $Gateway)/api/1"
    for ($i = 0; $i -lt 60; $i++) {
        & docker run --rm --network $Network $CurlImage -sf -o NUL --max-time 2 $url 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) { return $true }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

function Get-CpuUsageUsec([string]$Service) {
    $raw = & docker compose -f $Compose exec -T $Service cat /sys/fs/cgroup/cpu.stat 2>&1
    foreach ($line in $raw) {
        if ($line -match '^usage_usec\s+(\d+)') { return [int64]$Matches[1] }
    }
    return [int64]0
}

$metaPath = Join-Path $Raw 'meta.csv'
if (-not (Test-Path $metaPath)) {
    'tag,gateway,tier,concurrency,repeat,seconds,requests,throughput_rps,cpu_cores_used,gateway_peak_rss_mb,exit_code,csv_file' |
        Set-Content -Path $metaPath -Encoding utf8
}

$total = $Gateways.Count * $Tiers.Count * $Concurrency.Count * $Repeats
$done = 0

foreach ($gateway in $Gateways) {
    foreach ($tier in $Tiers) {
        foreach ($conc in $Concurrency) {
            for ($run = 1; $run -le $Repeats; $run++) {
                $done++
                $stamp = Get-Date -Format 'HH:mm:ss'
                Write-Host "[$stamp] ($done/$total) $gateway tier=$tier c=$conc run=$run" -ForegroundColor Cyan

                $cpuset = Get-Cpuset $tier
                Stop-Stack
                Start-Stack $gateway $cpuset

                if (-not (Wait-Ready $gateway)) {
                    Write-Warning "  gateway $gateway did not become ready; recording a failed run"
                    Stop-Stack
                    continue
                }

                $baseUrl = Get-BaseUrl $gateway
                $target = "$baseUrl/api/1"

                # Warm-up: JIT the backend, fill the connection pools, settle the CPU governor.
                & docker run --rm --network $Network --cpuset-cpus $ClientCpuset --entrypoint hey $HeyImage `
                    -z "${Warmup}s" -c $conc $target 2>&1 | Out-Null

                $cpuBefore = Get-CpuUsageUsec $gateway
                $wall = [System.Diagnostics.Stopwatch]::StartNew()
                $csvName = '{0}-{1}tier-c{2}-r{3}-{4}.csv' -f $gateway, $tier, $conc, $run, $Tag
                $csvPath = Join-Path $Raw $csvName
                $heyOut = & docker run --rm --network $Network --cpuset-cpus $ClientCpuset --entrypoint hey $HeyImage `
                    -z "${Seconds}s" -c $conc -o csv $target 2>&1
                $heyExit = $LASTEXITCODE
                $wall.Stop()
                $heyOut | Set-Content -Path $csvPath -Encoding utf8
                $cpuAfter = Get-CpuUsageUsec $gateway

                $coresUsed = 0.0
                if ($wall.Elapsed.TotalSeconds -gt 0) {
                    $coresUsed = [math]::Round((($cpuAfter - $cpuBefore) / 1000000.0) / $wall.Elapsed.TotalSeconds, 3)
                }

                $requests = 0
                $rps = 0.0
                $dataRows = @($heyOut | Where-Object { $_ -match '^\d' })
                if ($dataRows.Count -gt 0) {
                    $requests = $dataRows.Count
                    $rps = [math]::Round($requests / $wall.Elapsed.TotalSeconds, 1)
                }

                $rssMb = 0.0
                $statsLine = & docker stats --no-stream --format '{{.MemUsage}}' (docker compose -f $Compose ps -q $gateway) 2>&1
                if ($statsLine -match '([\d.]+)(MiB|GiB)') {
                    $rssMb = if ($Matches[2] -eq 'GiB') { [double]$Matches[1] * 1024 } else { [double]$Matches[1] }
                }

                if ($gateway -eq 'spire') {
                    $metricsPath = Join-Path $Raw ('{0}-{1}tier-c{2}-r{3}-{4}.metrics.txt' -f $gateway, $tier, $conc, $run, $Tag)
                    & docker run --rm --network $Network $CurlImage -s --max-time 5 'http://spire:8870/metrics' 2>&1 |
                        Set-Content -Path $metricsPath -Encoding utf8
                }

                '{0},{1},{2},{3},{4},{5},{6},{7},{8},{9},{10},{11}' -f `
                    $Tag, $gateway, $tier, $conc, $run, $Seconds, $requests, $rps, $coresUsed, [math]::Round($rssMb, 1), $heyExit, $csvName |
                    Add-Content -Path $metaPath -Encoding utf8

                Write-Host ("  requests={0} rps={1} cores_used={2} rss={3}MB" -f $requests, $rps, $coresUsed, [math]::Round($rssMb, 1)) -ForegroundColor DarkGray

                Stop-Stack
            }
        }
    }
}

Write-Host "matrix complete: $metaPath" -ForegroundColor Green
