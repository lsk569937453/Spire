#!/usr/bin/env python3
"""Re-derives every headline number in report.md from the raw result files.

The report quotes medians and ratios; this script recomputes them from
results/ab.csv, results/bin-ab.csv, results/chaos-summary.csv and
results/logging-ab.csv and compares against the values printed in the report.
Run it after regenerating any result file:

    python scripts/verify_report.py

Exit code is non-zero when a quoted number no longer matches the data.
"""

from __future__ import annotations

import csv
import statistics
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RESULTS = ROOT / "results"

failures: list[str] = []


def load(name: str) -> list[dict]:
    path = RESULTS / name
    with path.open("r", encoding="utf-8", newline="") as handle:
        return list(csv.DictReader(handle))


def close(actual: float, expected: float, tolerance: float) -> bool:
    if expected == 0:
        return abs(actual) <= tolerance
    return abs(actual - expected) / abs(expected) <= tolerance


def check(label: str, actual: float, expected: float, tolerance: float = 0.02) -> None:
    ok = close(actual, expected, tolerance)
    mark = "PASS" if ok else "FAIL"
    print(f"  [{mark}] {label}: data={actual:,.3f} report={expected:,.3f}")
    if not ok:
        failures.append(label)


def section_1_core_table() -> None:
    """§1 headline table: medians of three interleaved runs per tier."""
    print("§1 1/2/4-core table (median of 3 runs, exclusive of cpuset repeats)")
    rows = load("ab.csv")
    groups = {
        "1 core": ["final-1c", "conf-1c-a", "conf-1c-b"],
        "2 cores": ["final-2c", "conf-2c-a", "conf-2c-b"],
        "4 cores": ["final-4c", "conf-4c-a", "conf-4c-b"],
    }
    for label, tags in groups.items():
        sub = [row for tag in tags for row in rows if row["tag"] == tag]
        if len(sub) != 3:
            failures.append(f"{label}: expected 3 rows, found {len(sub)}")
            print(f"  [FAIL] {label}: expected 3 rows, found {len(sub)}")
            continue
        spire_us = statistics.median(float(r["spire_us_per_req"]) for r in sub)
        nginx_us = statistics.median(float(r["nginx_us_per_req"]) for r in sub)
        cpu_ratio = statistics.median(float(r["cpu_ratio"]) for r in sub)
        spire_rps = statistics.median(float(r["spire_rps"]) for r in sub)
        nginx_rps = statistics.median(float(r["nginx_rps"]) for r in sub)
        check(f"{label} spire us/req", spire_us, {"1 core": 30.7, "2 cores": 30.0, "4 cores": 37.8}[label], 0.03)
        check(f"{label} nginx us/req", nginx_us, {"1 core": 25.6, "2 cores": 27.0, "4 cores": 31.4}[label], 0.03)
        check(f"{label} CPU ratio", cpu_ratio, {"1 core": 1.199, "2 cores": 1.111, "4 cores": 1.199}[label], 0.02)
        check(f"{label} spire rps", spire_rps, {"1 core": 33554, "2 cores": 62886, "4 cores": 88400}[label], 0.02)
        check(f"{label} nginx rps", nginx_rps, {"1 core": 39297, "2 cores": 55290, "4 cores": 74909}[label], 0.02)
        check(f"{label} throughput ratio", spire_rps / nginx_rps,
              {"1 core": 0.85, "2 cores": 1.14, "4 cores": 1.18}[label], 0.03)


def section_5_logging() -> None:
    """§5 log-level A/B."""
    print("§5 logging A/B")
    rows = load("logging-ab.csv")
    by_key = {(r["phase"], r["concurrency"]): float(r["rps"]) for r in rows}
    check("warn c=64 rps", by_key[("warn", "64")], 48974, 0.02)
    check("warn c=256 rps", by_key[("warn", "256")], 43751, 0.02)
    check("debug c=64 rps", by_key[("debug", "64")], 37012, 0.02)
    check("debug c=256 rps", by_key[("debug", "256")], 22331, 0.02)


def section_7_chaos() -> None:
    """§7 chaos table: api_7 vs the median of the other nine."""
    print("§7 chaos experiment")
    rows = load("chaos-summary.csv")
    expectations = {
        "chaos-baseline": {"api7_p95": 13.2, "others_p95": 13.8, "error": 0.0, "rps": 1000},
        "chaos-injected": {"api7_p95": 2027.6, "others_p95": 23.5, "error": 0.0, "rps": 965},
        "chaos-timeout-300ms": {"api7_p95": 316.0, "others_p95": 29.4, "error": 0.101, "rps": 995},
    }
    for phase, expect in expectations.items():
        sub = [r for r in rows if r["phase"] == phase and r["endpoint"].startswith("api_")]
        if not sub:
            failures.append(f"{phase}: no per-endpoint rows")
            print(f"  [FAIL] {phase}: no per-endpoint rows")
            continue
        slow = next(r for r in sub if r["endpoint"] == "api_7")
        fast = [r for r in sub if r["endpoint"] != "api_7"]
        total = next(r for r in rows if r["phase"] == phase and r["endpoint"] == "__throughput__")
        others = statistics.median(float(r["p95_ms"]) for r in fast)
        check(f"{phase} api_7 p95 ms", float(slow["p95_ms"]), expect["api7_p95"], 0.05)
        check(f"{phase} others p95 ms", others, expect["others_p95"], 0.05)
        check(f"{phase} error rate", float(total["error_rate"] or 0), expect["error"], 0.05)
        check(f"{phase} req/s", float(total["avg_ms"]) * 1000, expect["rps"], 0.02)


def section_8_build_experiments() -> None:
    """§8.2/§8.4 build-flag A/B results."""
    print("§8 build experiments")
    rows = [r for r in load("bin-ab.csv") if r["cpuset"] == "4"]
    by_pair = {(r["image_a"], r["image_b"]): r for r in rows}
    pgo = by_pair.get(("perf-spire:gnu-base", "perf-spire:pgo"))
    if pgo:
        check("PGO vs gnu-base user ratio", float(pgo["b_over_a"]), 0.649, 0.03)
    tlc = by_pair.get(("perf-spire:gnu-base", "perf-spire:gnu-tlcache"))
    if tlc:
        check("thread-local cache user ratio", float(tlc["b_over_a"]), 0.938, 0.03)
    mixed = by_pair.get(("perf-spire:pgo", "perf-spire:pgo-mixed"))
    if mixed:
        check("mixed PGO training set ratio", float(mixed["b_over_a"]), 1.055, 0.03)


def main() -> int:
    print(f"verifying {ROOT.name} report against results/\n")
    section_1_core_table()
    print()
    section_5_logging()
    print()
    section_7_chaos()
    print()
    section_8_build_experiments()
    print()
    if failures:
        print(f"FAILED: {len(failures)} quoted number(s) no longer match the data:")
        for item in failures:
            print(f"  - {item}")
        return 1
    print("OK: every quoted number still matches the raw data.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
