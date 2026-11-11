#!/usr/bin/env bash
# Benchmark: the same three HTTP routes on six servers, one core each.
#
#   bench/run.sh [rounds] [seconds-per-measurement]
#
#   SERVERS="giong monoio glommio tokio-raw" OUT=results/io-uring-runtimes bench/run.sh
#       measures a subset and writes somewhere else.
#
# Method
#   * The server is pinned to SERVER_CPU, the load generator (wrk) to LOAD_CPU,
#     so every server gets exactly one core and never competes with wrk.
#   * Servers are measured in interleaved rounds (A B C ... A B C ...) so that
#     drift in the machine's speed affects all of them equally.
#   * Each measurement is preceded by a warm-up; the report shows the median
#     of all rounds plus the min/max, so you can see the noise.
#
# Requires: wrk, cargo, python3, and node / bun for those two servers.
# Results: bench/results/ (or $OUT).
set -euo pipefail
cd "$(dirname "$0")"

ROUNDS=${1:-5}
SECS=${2:-5}
SERVER_CPU=${SERVER_CPU:-1}
LOAD_CPU=${LOAD_CPU:-0}
CONNS=${CONNS:-50}
OUT=${OUT:-results}
mkdir -p "$OUT"
CSV="$OUT/raw.csv"

echo "building..." >&2
(cd .. && cargo build --release --example http -q)
for d in axum tokio-raw native-epoll monoio glommio; do (cd "$d" && cargo build --release -q); done

read -r -a NAMES <<<"${SERVERS:-node bun axum tokio-raw native-epoll monoio glommio giong}"
declare -A CMD=(
  [node]="node node/server.js"
  [bun]="bun bun/server.js"
  [axum]="axum/target/release/bench-axum"
  [tokio-raw]="tokio-raw/target/release/bench-tokio-raw"
  [native-epoll]="native-epoll/target/release/bench-native-epoll"
  [monoio]="monoio/target/release/bench-monoio"
  [glommio]="glommio/target/release/bench-glommio"
  [giong]="../target/release/examples/http 127.0.0.1:3006"
)
declare -A PORT=([node]=3001 [bun]=3002 [axum]=3003 [native-epoll]=3004 [tokio-raw]=3005 [giong]=3006 [monoio]=3007 [glommio]=3008)

echo "server,endpoint,round,rps,p50,p99,rss_kb" > "$CSV"
for round in $(seq 1 "$ROUNDS"); do
  for s in "${NAMES[@]}"; do
    taskset -c "$SERVER_CPU" ${CMD[$s]} >/dev/null 2>&1 &
    pid=$!
    sleep 1.2
    base="http://127.0.0.1:${PORT[$s]}"
    for ep in json users "compute?n=25"; do
      taskset -c "$LOAD_CPU" wrk -t1 -c"$CONNS" -d2s "$base/$ep" >/dev/null 2>&1
      out=$(taskset -c "$LOAD_CPU" wrk -t1 -c"$CONNS" -d"${SECS}s" --latency "$base/$ep")
      rps=$(awk '/Requests\/sec/{print $2}' <<<"$out")
      p50=$(awk '$1=="50%"{print $2}' <<<"$out")
      p99=$(awk '$1=="99%"{print $2}' <<<"$out")
      echo "$s,$ep,$round,$rps,$p50,$p99,$(ps -o rss= -p $pid | tr -d ' ')" >> "$CSV"
    done
    out=$(taskset -c "$LOAD_CPU" wrk -t1 -c"$CONNS" -d"${SECS}s" -s pipeline.lua "$base")
    echo "$s,pipelined-json,$round,$(awk '/Requests\/sec/{print $2}' <<<"$out"),,,$(ps -o rss= -p $pid | tr -d ' ')" >> "$CSV"
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    sleep 0.5
    echo "round $round/$ROUNDS: $s done" >&2
  done
done

python3 - "$CSV" <<'PY' | tee "$OUT/summary.txt"
import csv, statistics, sys

def to_ms(v):
    if not v: return None
    for unit, k in (("us", 1e-3), ("ms", 1.0), ("s", 1e3)):
        if v.endswith(unit): return float(v[:-len(unit)]) * k
    return float(v)

rows = list(csv.DictReader(open(sys.argv[1])))
servers, endpoints = [], []
for r in rows:
    if r["server"] not in servers: servers.append(r["server"])
    if r["endpoint"] not in endpoints: endpoints.append(r["endpoint"])

print(f"{'endpoint':<16}{'server':<14}{'req/s median':>13}{'min':>10}{'max':>10}{'p50 ms':>9}{'p99 ms':>9}")
for ep in endpoints:
    for s in servers:
        sel = [r for r in rows if r["server"] == s and r["endpoint"] == ep]
        rps = [float(r["rps"]) for r in sel]
        p50 = [to_ms(r["p50"]) for r in sel if r["p50"]]
        p99 = [to_ms(r["p99"]) for r in sel if r["p99"]]
        fmt = lambda xs: f"{statistics.median(xs):9.2f}" if xs else f"{'-':>9}"
        print(f"{ep:<16}{s:<14}{statistics.median(rps):13.0f}{min(rps):10.0f}{max(rps):10.0f}{fmt(p50)}{fmt(p99)}")
    print()
print("memory after load (RSS, MB, median):")
for s in servers:
    rss = [int(r["rss_kb"]) for r in rows if r["server"] == s and r["endpoint"] == "pipelined-json" and r["rss_kb"]]
    print(f"  {s:<14}{statistics.median(rss)/1024:6.1f}")
PY
