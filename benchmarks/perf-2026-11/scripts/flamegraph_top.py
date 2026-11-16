#!/usr/bin/env python3
"""Summarise a flamegraph-rs / inferno SVG by *self* time.

Every frame in the SVG carries its own absolute sample count:

    <rect ... fg:x="12036108" fg:w="53159477" .../><title>... (53,159,477 samples, 0.03%)</title>

`fg:w` includes all descendants, so ranking by it double-counts. This script
rebuilds the frame tree from (x, w, depth) and reports self time = w minus the
sum of the direct children's widths, which is what "where does the CPU go"
should mean.

Usage:
    python flamegraph_top.py results/flamegraphs/spire-4core-perf.svg [top_n]
"""

from __future__ import annotations

import html
import re
import sys
from collections import defaultdict
from pathlib import Path

FRAME = re.compile(
    r'<g><title>(?P<title>.*?)</title><rect[^>]*y="(?P<y>\d+)"[^>]*fg:x="(?P<x>\d+)" fg:w="(?P<w>\d+)"',
    re.S,
)

BUCKETS = [
    ("spire-proxy (this crate)", ("spire_proxy",)),
    ("hyper (HTTP codec)", ("hyper::", "hyper_util")),
    ("tokio (runtime/io)", ("tokio::", "mio::", "tokio_util")),
    ("mimalloc (allocator)", ("mimalloc", "_mi_")),
    ("tracing (logging)", ("tracing::", "tracing_subscriber", "tracing_core", "logroller")),
    ("rustls/ring (TLS)", ("rustls", "ring::")),
    ("serde (config parse)", ("serde_",)),
    ("kernel write path (send)", ("writev", "sendmsg", "sendto", "tcp_send", "sock_write_iter")),
    ("kernel read path (recv)", ("readv", "recvmsg", "recvfrom", "tcp_recv", "sock_read_iter")),
    ("kernel accept/connect", ("accept", "connect", "inet_csk")),
    ("kernel epoll/wakeup", ("epoll", "schedule", "futex")),
    ("libc mem/string", ("memcpy", "memset", "bcmp", "memcmp", "memmove")),
]


def bucket(symbol: str) -> str:
    for name, needles in BUCKETS:
        if any(needle in symbol for needle in needles):
            return name
    return "other"


def main() -> None:
    if len(sys.argv) < 2:
        print(__doc__)
        raise SystemExit(2)
    path = Path(sys.argv[1])
    top_n = int(sys.argv[2]) if len(sys.argv) > 2 else 30
    text = path.read_text(encoding="utf-8", errors="replace")

    frames = []
    for match in FRAME.finditer(text):
        scales_marker = " ("
        title = html.unescape(match.group("title"))
        symbol = title.split(scales_marker)[0].strip() or title
        frames.append(
            {
                "symbol": symbol,
                "depth": -int(match.group("y")),  # SVG y grows downwards: root is at the largest y
                "x": int(match.group("x")),
                "w": int(match.group("w")),
                "children": [],
            }
        )
    if not frames:
        raise SystemExit("no frames parsed")

    # inferno emits frames leaf-first, so parent/child cannot be inferred from
    # document order. Rebuild the tree by containment: the parent of a frame is
    # the deepest other frame whose range strictly encloses it.
    n = len(frames)
    parents = [-1] * n
    for i in range(n):
        fi = frames[i]
        best = -1
        best_depth = -(10**9)
        for j in range(n):
            if i == j:
                continue
            fj = frames[j]
            if fj["depth"] >= fi["depth"]:
                continue
            if fj["x"] <= fi["x"] and fi["x"] + fi["w"] <= fj["x"] + fj["w"]:
                if fj["depth"] > best_depth:
                    best = j
                    best_depth = fj["depth"]
        parents[i] = best

    child_widths: dict[int, int] = defaultdict(int)
    for i, parent in enumerate(parents):
        if parent >= 0:
            child_widths[parent] += frames[i]["w"]

    total = max((f["w"] for f in frames if parents[frames.index(f)] < 0), default=max(f["w"] for f in frames))
    self_samples: dict[str, int] = defaultdict(int)
    for i, frame in enumerate(frames):
        own = frame["w"] - child_widths[i]
        if own > 0:
            self_samples[frame["symbol"]] += own

    overall = sum(self_samples.values()) or 1

    print(f"# {path.name}")
    print(f"frames: {len(frames)}, root samples: {total:,}, summed self: {overall:,}\n")
    print("## top symbols by SELF time")
    print(f"{'share':>8}  {'samples':>14}  symbol")
    for symbol, samples in sorted(self_samples.items(), key=lambda kv: kv[1], reverse=True)[:top_n]:
        print(f"{samples / overall * 100:7.2f}%  {samples:14,}  {symbol[:100]}")

    print("\n## by component (self time)")
    grouped: dict[str, int] = defaultdict(int)
    for symbol, samples in self_samples.items():
        grouped[bucket(symbol)] += samples
    for name, samples in sorted(grouped.items(), key=lambda kv: kv[1], reverse=True):
        print(f"{samples / overall * 100:7.2f}%  {name}")


if __name__ == "__main__":
    main()
