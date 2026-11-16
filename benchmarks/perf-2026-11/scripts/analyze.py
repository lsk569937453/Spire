#!/usr/bin/env python3
"""Turn the raw perf-2026-11 results into tables, CSVs, charts and report snippets.

Inputs (written by scripts/run.ps1 and scripts/chaos.ps1):
    results/raw/meta.csv            one row per measured run
    results/raw/*.csv               hey `-o csv` per-request records
    results/chaos-*.json            k6 summary export (per-endpoint metrics)

Outputs:
    results/summary.csv             median-of-repeats per (gateway, tier, concurrency)
    results/chaos-summary.csv       per-endpoint latency per chaos phase
    results/charts/*.svg            self-contained SVG charts (no matplotlib here)
    results/tables.md               markdown tables pasted into the report
"""

from __future__ import annotations

import csv
import json
import re
import statistics
from collections import defaultdict
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
RAW = ROOT / "results" / "raw"
RESULTS = ROOT / "results"
CHARTS = RESULTS / "charts"

GATEWAY_ORDER = ["spire", "nginx"]
TIER_ORDER = ["1", "2", "4"]
COLORS = {
    ("spire", "1"): "#C2410C",
    ("spire", "2"): "#F59E0B",
    ("spire", "4"): "#B45309",
    ("nginx", "1"): "#1D4ED8",
    ("nginx", "2"): "#3B82F6",
    ("nginx", "4"): "#60A5FA",
}


def read_hey_csv(path: Path) -> tuple[np.ndarray, dict[str, int], float]:
    """Return latencies in milliseconds, a status-code histogram and the window.

    hey `-o csv` writes one row per request:
        response-time,DNS+dialup,DNS,Request-write,Response-delay,Response-read,status-code,offset

    The trailing `offset` column is seconds since the run started, i.e. the last
    value is the real measurement window. It must be used instead of the wall
    clock: this hey build stops after 1,000,000 requests, so fast cells finish
    their 60s budget early and dividing by the wall clock underestimates
    throughput (observed: 1M requests in 39.2s reported as 14k instead of 25k).
    """
    latencies: list[float] = []
    status: dict[str, int] = defaultdict(int)
    window = 0.0
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        row = next(csv.reader([line]), [])
        if len(row) < 7:
            continue
        try:
            seconds = float(row[0])
        except ValueError:
            continue
        code = row[6].strip()
        status[code or "error"] += 1
        try:
            window = max(window, float(row[7]))
        except (ValueError, IndexError):
            pass
        if code.isdigit() and int(code) >= 400:
            continue
        latencies.append(seconds * 1000.0)
    return np.array(latencies, dtype=float), dict(status), window


def pct(values: np.ndarray, q: float) -> float:
    if values.size == 0:
        return float("nan")
    return float(np.percentile(values, q))


def load_meta() -> list[dict]:
    meta_path = RAW / "meta.csv"
    if not meta_path.exists():
        return []
    with meta_path.open("r", encoding="utf-8") as handle:
        return list(csv.DictReader(handle))


def build_matrix_summary() -> list[dict]:
    rows = load_meta()
    grouped: dict[tuple[str, str, int], list[dict]] = defaultdict(list)
    for row in rows:
        if row.get("tag") != "matrix":
            continue
        key = (row["gateway"], row["tier"], int(row["concurrency"]))
        grouped[key].append(row)

    summary: list[dict] = []
    for (gateway, tier, concurrency), runs in sorted(
        grouped.items(), key=lambda kv: (GATEWAY_ORDER.index(kv[0][0]), int(kv[0][1]), kv[0][2])
    ):
        per_run = []
        for run in runs:
            csv_path = RAW / run["csv_file"]
            if not csv_path.exists():
                continue
            lat, status, window = read_hey_csv(csv_path)
            if lat.size == 0:
                continue
            per_run.append(
                {
                    "rps": lat.size / window if window > 0 else 0.0,
                    "window": window,
                    "requests": int(lat.size),
                    "p50": pct(lat, 50),
                    "p90": pct(lat, 90),
                    "p99": pct(lat, 99),
                    "p999": pct(lat, 99.9),
                    "errors": sum(v for k, v in status.items() if k.isdigit() and int(k) >= 400),
                    "cores": float(run.get("cpu_cores_used") or 0.0),
                    "rss": float(run.get("gateway_peak_rss_mb") or 0.0),
                }
            )
        if not per_run:
            continue
        summary.append(
            {
                "gateway": gateway,
                "tier": tier,
                "concurrency": concurrency,
                "repeats": len(per_run),
                "rps": statistics.median(r["rps"] for r in per_run),
                "window_s": statistics.median(r["window"] for r in per_run),
                "requests": statistics.median(r["requests"] for r in per_run),
                "p50_ms": statistics.median(r["p50"] for r in per_run),
                "p90_ms": statistics.median(r["p90"] for r in per_run),
                "p99_ms": statistics.median(r["p99"] for r in per_run),
                "p999_ms": statistics.median(r["p999"] for r in per_run),
                "errors": statistics.median(r["errors"] for r in per_run),
                "cpu_cores": statistics.median(r["cores"] for r in per_run),
                "rss_mb": statistics.median(r["rss"] for r in per_run),
            }
        )
    return summary


def write_summary_csv(summary: list[dict]) -> Path:
    path = RESULTS / "summary.csv"
    fields = [
        "gateway",
        "tier",
        "concurrency",
        "repeats",
        "rps",
        "window_s",
        "requests",
        "p50_ms",
        "p90_ms",
        "p99_ms",
        "p999_ms",
        "errors",
        "cpu_cores",
        "rss_mb",
    ]
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        for row in summary:
            writer.writerow({key: round(row[key], 3) if isinstance(row[key], float) else row[key] for key in fields})
    return path


# --------------------------------------------------------------------------- #
# Minimal SVG charting (no matplotlib in this environment)
# --------------------------------------------------------------------------- #


def _scale(value: float, lo: float, hi: float, out_lo: float, out_hi: float) -> float:
    if hi <= lo:
        return (out_lo + out_hi) / 2
    return out_lo + (value - lo) / (hi - lo) * (out_hi - out_lo)


def line_chart(
    series: dict[str, list[tuple[float, float]]],
    path: Path,
    title: str,
    y_label: str,
    x_label: str = "concurrency",
    log_x: bool = True,
    log_y: bool = False,
) -> None:
    width, height = 860, 460
    pad_l, pad_r, pad_t, pad_b = 90, 190, 56, 62
    xs = [x for points in series.values() for x, _ in points]
    ys = [y for points in series.values() for _, y in points if y == y]
    if not xs or not ys:
        return
    x_lo, x_hi = min(xs), max(xs)
    y_lo, y_hi = min(ys), max(ys)
    if log_x:
        x_lo, x_hi = np.log2(max(x_lo, 1)), np.log2(max(x_hi, 2))
    if log_y:
        y_lo, y_hi = np.log10(max(y_lo, 0.01)), np.log10(max(y_hi, 0.02))
    y_pad = (y_hi - y_lo) * 0.08 or 1.0
    y_lo, y_hi = y_lo - y_pad, y_hi + y_pad

    def px(x: float) -> float:
        return _scale(np.log2(max(x, 1)) if log_x else x, x_lo, x_hi, pad_l, width - pad_r)

    def py(y: float) -> float:
        return height - pad_b - _scale(np.log10(max(y, 0.01)) if log_y else y, y_lo, y_hi, 0, height - pad_t - pad_b)

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" font-family="Inter, system-ui, sans-serif">',
        f'<rect width="{width}" height="{height}" fill="#FFFFFF"/>',
        f'<text x="{pad_l}" y="30" font-size="17" font-weight="700" fill="#1C1917">{title}</text>',
        f'<line x1="{pad_l}" y1="{height - pad_b}" x2="{width - pad_r}" y2="{height - pad_b}" stroke="#A8A29E"/>',
        f'<line x1="{pad_l}" y1="{pad_t}" x2="{pad_l}" y2="{height - pad_b}" stroke="#A8A29E"/>',
    ]

    for tick in sorted({x for x in xs}):
        x = px(tick)
        parts.append(f'<line x1="{x:.1f}" y1="{height - pad_b}" x2="{x:.1f}" y2="{height - pad_b + 5}" stroke="#A8A29E"/>')
        parts.append(
            f'<text x="{x:.1f}" y="{height - pad_b + 20}" font-size="11" fill="#57534E" '
            f'text-anchor="middle">{int(tick)}</text>'
        )

    for i in range(6):
        value = y_lo + (y_hi - y_lo) * i / 5
        raw = (10**value) if log_y else value
        y = height - pad_b - (height - pad_t - pad_b) * i / 5
        parts.append(f'<line x1="{pad_l}" y1="{y:.1f}" x2="{width - pad_r}" y2="{y:.1f}" stroke="#F5F5F4"/>')
        label = f"{raw:,.1f}" if raw < 100 else f"{raw:,.0f}"
        parts.append(
            f'<text x="{pad_l - 12}" y="{y + 4:.1f}" font-size="11" fill="#57534E" text-anchor="end">{label}</text>'
        )

    for name, points in series.items():
        ordered = sorted(points)
        d = " ".join(f"{'M' if i == 0 else 'L'}{px(x):.1f},{py(y):.1f}" for i, (x, y) in enumerate(ordered) if y == y)
        color = COLORS.get(tuple(name.split("/")), "#78716C")
        parts.append(f'<path d="{d}" fill="none" stroke="{color}" stroke-width="2.2"/>')
        for x, y in ordered:
            if y == y:
                parts.append(f'<circle cx="{px(x):.1f}" cy="{py(y):.1f}" r="3.4" fill="{color}"/>')

    for i, name in enumerate(series):
        color = COLORS.get(tuple(name.split("/")), "#78716C")
        y = pad_t + 10 + i * 22
        parts.append(f'<rect x="{width - pad_r + 18}" y="{y - 9}" width="12" height="12" fill="{color}"/>')
        parts.append(
            f'<text x="{width - pad_r + 36}" y="{y + 2}" font-size="12" fill="#44403C">{name}</text>'
        )

    parts.append(
        f'<text x="{width - pad_r + 18}" y="{height - pad_b - 4}" font-size="11" fill="#78716C">'
        f"{x_label} → / {y_label} ↑</text>"
    )
    parts.append("</svg>")
    path.write_text("\n".join(parts), encoding="utf-8")


def matrix_charts(summary: list[dict]) -> None:
    CHARTS.mkdir(parents=True, exist_ok=True)
    rps_series: dict[str, list[tuple[float, float]]] = defaultdict(list)
    p99_series: dict[str, list[tuple[float, float]]] = defaultdict(list)
    cpu_series: dict[str, list[tuple[float, float]]] = defaultdict(list)
    for row in summary:
        name = f"{row['gateway']}/{row['tier']}"
        rps_series[name].append((row["concurrency"], row["rps"]))
        p99_series[name].append((row["concurrency"], row["p99_ms"]))
        cpu_series[name].append((row["concurrency"], row["cpu_cores"]))
    line_chart(rps_series, CHARTS / "throughput.svg", "Throughput vs concurrency", "req/s")
    line_chart(p99_series, CHARTS / "p99.svg", "p99 latency vs concurrency", "milliseconds", log_y=True)
    line_chart(cpu_series, CHARTS / "cpu.svg", "Gateway CPU used vs concurrency", "cores")


# --------------------------------------------------------------------------- #
# Chaos results
# --------------------------------------------------------------------------- #


def load_k6_summary(path: Path) -> dict | None:
    if not path.exists():
        return None
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def chaos_rows() -> list[dict]:
    """Per-endpoint latency for every chaos phase.

    k6 keeps no tag sub-metrics in the JSON export, so k6/mixed.js declares one
    explicit Trend per endpoint (`api_7_ms` etc.); tagged duration metrics are
    still picked up when a future k6 version exports them.
    """
    per_endpoint = re.compile(r"^api_(\d+)_ms$")
    rows: list[dict] = []
    for path in sorted(RESULTS.glob("chaos-*.json")):
        data = load_k6_summary(path)
        if not data:
            continue
        metrics = data.get("metrics", {})
        phase = path.stem
        seen: set[str] = set()
        for key, value in metrics.items():
            match = per_endpoint.match(key)
            if match:
                endpoint = "api_" + match.group(1)
            elif key.startswith("http_req_duration{") and "endpoint:" in key:
                endpoint = key.split("endpoint:")[1].rstrip("}")
            else:
                continue
            values = value.get("values", {})
            seen.add(endpoint)
            rows.append(
                {
                    "phase": phase,
                    "endpoint": endpoint,
                    "avg_ms": values.get("avg"),
                    "p95_ms": values.get("p(95)"),
                    "p99_ms": values.get("p(99)"),
                    "max_ms": values.get("max"),
                }
            )
        reqs = metrics.get("http_reqs", {}).get("values", {})
        fails = metrics.get("http_req_failed", {}).get("values", {})
        rows.append(
            {
                "phase": phase,
                "endpoint": "__throughput__",
                "avg_ms": (reqs.get("rate") or 0) / 1000.0 if reqs.get("rate") else None,
                "p95_ms": None,
                "p99_ms": None,
                "max_ms": None,
                "error_rate": fails.get("rate"),
            }
        )
    return rows


def write_chaos_csv(rows: list[dict]) -> Path:
    path = RESULTS / "chaos-summary.csv"
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(
            handle, fieldnames=["phase", "endpoint", "avg_ms", "p95_ms", "p99_ms", "max_ms", "error_rate"]
        )
        writer.writeheader()
        for row in rows:
            writer.writerow(row)
    return path


def chaos_chart() -> None:
    CHARTS.mkdir(parents=True, exist_ok=True)
    rows = [r for r in chaos_rows() if r["endpoint"] != "__throughput__" and r.get("p99_ms")]
    if not rows:
        return
    phases = sorted({r["phase"] for r in rows})
    endpoints = sorted({r["endpoint"] for r in rows}, key=lambda e: int(e.split("_")[1]))
    series: dict[str, list[tuple[float, float]]] = {}
    for phase in phases:
        points = []
        for endpoint in endpoints:
            match = next((r for r in rows if r["phase"] == phase and r["endpoint"] == endpoint), None)
            if match:
                points.append((float(endpoint.split("_")[1]), float(match["p99_ms"])))
        series[phase] = points
    line_chart(series, CHARTS / "chaos-p99.svg", "Chaos: p99 per endpoint", "milliseconds", log_y=True)


def markdown_tables(summary: list[dict]) -> str:
    lines: list[str] = []
    for gateway in GATEWAY_ORDER:
        rows = [r for r in summary if r["gateway"] == gateway]
        if not rows:
            continue
        lines.append(f"### {gateway}")
        lines.append("")
        lines.append("| cores | concurrency | RPS | window s | p50 ms | p99 ms | p99.9 ms | errors | RSS MB |")
        lines.append("|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
        for row in sorted(rows, key=lambda r: (int(r["tier"]), r["concurrency"])):
            lines.append(
                f"| {row['tier']} | {row['concurrency']} | {row['rps']:,.0f} | {row['window_s']:.0f} | "
                f"{row['p50_ms']:.2f} | "
                f"{row['p99_ms']:.2f} | {row['p999_ms']:.2f} | {int(row['errors'])} | "
                f"{row['rss_mb']:.0f} |"
            )
        lines.append("")
    return "\n".join(lines)


def main() -> None:
    RESULTS.mkdir(parents=True, exist_ok=True)
    summary = build_matrix_summary()
    if summary:
        print(f"summary csv -> {write_summary_csv(summary)}")
        matrix_charts(summary)
        (RESULTS / "tables.md").write_text(markdown_tables(summary), encoding="utf-8")
        print(markdown_tables(summary))
    else:
        print("no matrix data yet")
    chaos = chaos_rows()
    if chaos:
        print(f"chaos csv -> {write_chaos_csv(chaos)}")
        chaos_chart()
    else:
        print("no chaos data yet")


if __name__ == "__main__":
    main()
