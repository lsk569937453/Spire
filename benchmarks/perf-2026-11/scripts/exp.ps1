#Requires -Version 7.0
<#
.SYNOPSIS
  One-shot optimisation experiment: run Spire with extra env/args, report the
  gateway/ceiling ratio so results survive host noise.

.EXAMPLE
  ./exp.ps1 -Tag mimi-purge -Env 'MIMALLOC_PURGE_DELAY=-1'
  ./exp.ps1 -Tag nodelay -Image perf-spire:exp
#>
param(
    [string]$Tag = 'exp',
    [string]$Cpuset = '4',
    [int]$Concurrency = 64,
    [int]$Seconds = 20,
    [string]$Env = '',
    [string]$Image = 'perf-spire:bench',
    [string]$Config = 'spire-bench.yaml',
    [string]$Binary = ''
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
$Compose = Join-Path $Root 'compose/stack.yaml'
$Results = Join-Path $Root 'results'
$Network = 'perf-2026-11_bench'
$HeyImage = 'demisto/rakyll-hey:1.0.0.44140'
$ClientCpuset = '0,2'

New-Item -ItemType Directory -Force -Path $Results | Out-Null

# The bridge network only exists while a compose service is up.
& docker rm -f spire-exp 2>&1 | Out-Null
& docker compose -f $Compose down -v --remove-orphans 2>&1 | Out-Null
& docker compose -f $Compose up -d backend 2>&1 | Out-Null

$envPairs = @()
if ($Env) { $envPairs = $Env.Split(';') | Where-Object { $_ } }
$args = @('run', '-d', '--name', 'spire-exp', '--network', $Network, '--cpuset-cpus', $Cpuset, '--memory', '2g',
    '--volume', "$($Root -replace '\\', '/')/config/${Config}:/etc/spire/config.yaml:ro")
foreach ($pair in $envPairs) { $args += @('-e', $pair) }
if ($Binary) { $args += @('--entrypoint', "/bin/sh") }
$args += $Image
if ($Binary) { $args += @('-c', "cp /bin/true /dev/null 2>/dev/null; exec /usr/local/bin/spire -f /etc/spire/config.yaml") }
& docker @args 2>&1 | Out-Null
Start-Sleep -Seconds 3

for ($i = 0; $i -lt 60; $i++) {
    & docker run --rm --network $Network curlimages/curl:latest -sf -o NUL --max-time 2 'http://spire-exp:6667/api/1' 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { break }
    Start-Sleep -Milliseconds 500
}

function Get-Hey([string]$Url) {
    $out = & docker run --rm --network $Network --cpuset-cpus $ClientCpuset --entrypoint hey $HeyImage `
        -z "${Seconds}s" -c $Concurrency $Url 2>&1
    $rpsLine = ($out | Select-String 'Requests/sec').Line
    if (-not $rpsLine) { return 0.0 }
    return [double](($rpsLine -replace '.*Requests/sec:\s*', '') -replace '\s.*', '')
}

& docker run --rm --network $Network --cpuset-cpus $ClientCpuset --entrypoint hey $HeyImage -z 5s -c $Concurrency 'http://backend:8080/api/1' 2>&1 | Out-Null
$ceiling = Get-Hey 'http://backend:8080/api/1'
$gate = Get-Hey 'http://spire-exp:6667/api/1'
$ratio = if ($ceiling -gt 0) { $gate / $ceiling } else { 0 }

$line = '{0},{1},{2},{3},{4:N1},{5:N1},{6:N3},{7}' -f $Tag, 'spire', $Cpuset, $Concurrency, $gate, $ceiling, $ratio, (Get-Date -Format 'HH:mm:ss')
$csv = Join-Path $Results 'experiments.csv'
if (-not (Test-Path $csv)) {
    'tag,gateway,cpuset,concurrency,gateway_rps,ceiling_rps,ratio,ts' | Set-Content $csv -Encoding utf8
}
Add-Content $csv $line -Encoding utf8
Write-Host $line -ForegroundColor Green

& docker rm -f spire-exp 2>&1 | Out-Null
& docker compose -f $Compose down -v --remove-orphans 2>&1 | Out-Null
