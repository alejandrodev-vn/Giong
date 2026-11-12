#!/usr/bin/env bash
# How do the runtimes behave from one connection to thousands?
#
#   bench/scale.sh [rounds] [seconds-per-measurement]
#
#   SERVERS="giong tokio-raw" CONNS="1 50 1000 5000" ENDPOINT=json bench/scale.sh
#
# For every server and connection count this records throughput, latency and
# the CPU time the server spent per request (user + kernel, from /proc). CPU
# per request is the number to trust when the load generator is the
# bottleneck, which it is for the small response on a two-core machine: it
# says how much work one request costs, whatever the offered load.
#
# Same method as run.sh: server pinned to SERVER_CPU, wrk to LOAD_CPU,
# interleaved rounds, medians with min/max.
set -euo pipefail
cd "$(dirname "$0")"

ROUNDS=${1:-5}
SECS=${2:-6}
SERVER_CPU=${SERVER_CPU:-1}
LOAD_CPU=${LOAD_CPU:-0}
ENDPOINT=${ENDPOINT:-json}
OUT=${OUT:-results/scale}
read -r -a NAMES <<<"${SERVERS:-giong giong-spin giong-http monoio-tuned tokio-raw native-epoll glommio}"
read -r -a CONNS <<<"${CONNS:-1 50 1000 5000}"
TICKS=$(getconf CLK_TCK)
mkdir -p "$OUT"
CSV="$OUT/raw.csv"
ulimit -n "$(ulimit -Hn)"

echo "building..." >&2
for d in giong giong-http tokio-raw native-epoll monoio glommio; do (cd "$d" && cargo build --release -q); done

declare -A CMD=(
  [giong]="giong/target/release/bench-giong"
  [giong-spin]="env GIONG_SPIN_US=200 giong/target/release/bench-giong"
  [giong-http]="giong-http/target/release/bench-giong-http"
  [monoio]="monoio/target/release/bench-monoio"
  [monoio-tuned]="env MONOIO_RING=defer monoio/target/release/bench-monoio"
  [tokio-raw]="tokio-raw/target/release/bench-tokio-raw"
  [native-epoll]="native-epoll/target/release/bench-native-epoll"
  [glommio]="glommio/target/release/bench-glommio"
  [glommio-spin]="env GLOMMIO_SPIN_US=200 glommio/target/release/bench-glommio"
)
declare -A PORT=([giong]=3006 [giong-spin]=3006 [giong-http]=3009 [monoio]=3007 [monoio-tuned]=3007
  [tokio-raw]=3005 [native-epoll]=3004 [glommio]=3008 [glommio-spin]=3008)

cpu_ticks() { cat /proc/"$1"/task/*/stat 2>/dev/null | awk '{u+=$14; s+=$15} END{print u+0, s+0}'; }

echo "server,conns,round,rps,p50,p99,user_us,kernel_us" > "$CSV"
for round in $(seq 1 "$ROUNDS"); do
  for s in "${NAMES[@]}"; do
    taskset -c "$SERVER_CPU" ${CMD[$s]} >/dev/null 2>&1 &
    pid=$!
    sleep 1.2
    # `env` execs the server, so $pid is the server either way.
    url="http://127.0.0.1:${PORT[$s]}/$ENDPOINT"
    for c in "${CONNS[@]}"; do
      taskset -c "$LOAD_CPU" wrk -t1 -c"$c" -d2s --timeout 5s "$url" >/dev/null 2>&1
      read -r u0 k0 <<<"$(cpu_ticks "$pid")"
      out=$(taskset -c "$LOAD_CPU" wrk -t1 -c"$c" -d"${SECS}s" --timeout 5s --latency "$url")
      read -r u1 k1 <<<"$(cpu_ticks "$pid")"
      reqs=$(awk '/requests in/{print $1}' <<<"$out")
      rps=$(awk '/Requests\/sec/{print $2}' <<<"$out")
      p50=$(awk '$1=="50%"{print $2}' <<<"$out")
      p99=$(awk '$1=="99%"{print $2}' <<<"$out")
      read -r uus kus <<<"$(awk -v r="$reqs" -v u=$((u1-u0)) -v k=$((k1-k0)) -v t="$TICKS" \
        'BEGIN{printf "%.3f %.3f", u/t/r*1e6, k/t/r*1e6}')"
      echo "$s,$c,$round,$rps,$p50,$p99,$uus,$kus" >> "$CSV"
      sleep 0.5
    done
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    sleep 0.5
    echo "round $round/$ROUNDS: $s done" >&2
  done
done

python3 - "$CSV" <<'PY' | tee "$OUT/summary.txt"
import csv, statistics, sys

def to_ms(v):
    for unit, k in (("us", 1e-3), ("ms", 1.0), ("s", 1e3)):
        if v.endswith(unit): return float(v[:-len(unit)]) * k
    return float(v)

rows = list(csv.DictReader(open(sys.argv[1])))
servers, conns = [], []
for r in rows:
    if r["server"] not in servers: servers.append(r["server"])
    if r["conns"] not in conns: conns.append(r["conns"])
med = statistics.median
print(f"{'conns':<7}{'server':<14}{'req/s median':>13}{'min':>9}{'max':>9}{'cpu us/req':>12}{'(user':>7}{'+kernel)':>9}{'p50 ms':>9}{'p99 ms':>9}")
for c in conns:
    for s in servers:
        sel = [r for r in rows if r["server"] == s and r["conns"] == c]
        if not sel: continue
        rps = [float(r["rps"]) for r in sel]
        u = med(float(r["user_us"]) for r in sel); k = med(float(r["kernel_us"]) for r in sel)
        cpu = med(float(r["user_us"]) + float(r["kernel_us"]) for r in sel)
        p50 = med(to_ms(r["p50"]) for r in sel); p99 = med(to_ms(r["p99"]) for r in sel)
        print(f"{c:<7}{s:<14}{med(rps):13.0f}{min(rps):9.0f}{max(rps):9.0f}{cpu:12.2f}{u:7.2f}{k:9.2f}{p50:9.3f}{p99:9.2f}")
    print()
PY
