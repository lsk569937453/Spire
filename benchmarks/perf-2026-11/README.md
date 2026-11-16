# perf-2026-11 — Spire vs nginx benchmark, flamegraphs, chaos experiment

Reproducible rig for the report in [`report.md`](report.md).

## Layout

```
backend/        zero-dependency Java downstream (10 endpoints, 96-byte bodies,
                one class per endpoint so chaosblade can slow exactly one)
config/         spire-bench.yaml, spire-chaos-timeout.yaml, nginx.conf.tpl,
                spire-bench-debug.yaml (the logging A/B variant)
compose/        stack.yaml: backend + spire + nginx + hey + k6 on one bridge
k6/             mixed.js (10 endpoints, ~10% to the chaos target), ready.js
perf-tool/      image with perf 6.8 + flamegraph-rs 0.6.15 + inferno
spire/          Dockerfile taking ARG BINARY (release | symbols | pprof)
scripts/        run.ps1 (matrix), chaos.ps1 (chaos phases), analyze.py,
                flamegraph.ps1, flamegraph_top.py
bin/            cross-built Linux binaries (spire-release/symbols/pprof)
results/        raw hey CSVs, meta.csv, summary.csv, charts, flamegraphs
```

## Quick start

```powershell
# images
docker build -f backend/Dockerfile -t perf-backend:21 ./backend
docker build -f spire/Dockerfile -t perf-spire:bench .
docker build -f perf-tool/Dockerfile -t perf-tool:6.8 `
  --build-arg http_proxy=http://host.docker.internal:7890 `
  --build-arg https_proxy=http://host.docker.internal:7890 .

# benchmark matrix (tiers 1/2/4 cores, concurrency 1/16/64/256/1024, 3 repeats)
pwsh -File scripts/run.ps1 -SkipBuild

# flamegraph of the 4-core tier under load
pwsh -File scripts/flamegraph.ps1 -Cpuset '4,6,8,10' -Concurrency 256 -Seconds 50

# chaos: baseline -> injected -> timeout variant
pwsh -File scripts/chaos.ps1 -Phase all

# summary tables, per-endpoint chaos CSV and SVG charts
python scripts/analyze.py
```

## Core pinning

| Role | cpuset | Why |
|---|---|---|
| measured gateway | `4` / `4,6` / `4,6,8,10` | distinct P-cores, thread 0 only, so "2 cores" is not one physical core with HT |
| downstream backend | `12-19` | all E-cores, never the measured cores |
| load generator | `0,2` | two otherwise idle P-cores |

Everything runs on one user-defined bridge: routing client traffic through the
Docker Desktop → WSL2 port forward would cap throughput and flatten the
1/2/4-core difference.

## Notes that cost time to discover

- `forward_to` must not end with `/`: `http://backend:8080/` plus a `/` prefix
  matcher produces `//api/1` upstream and a `400 Bad request URI`.
- `log_level: debug` enables the access log and halves throughput (see report §5).
- `com.sun.net.httpserver` costs ~40ms per request on loopback (Nagle + delayed
  ACK); the backend here writes the whole response in one syscall with
  `TCP_NODELAY`.
- The repo's `benchmarks/spires/docker-compose.yaml` sets `CONFIG_FILE_PATH`,
  which the binary never reads, and mounts the config to `/tmp` while the image
  reads `/etc/spire/config.yaml`.
