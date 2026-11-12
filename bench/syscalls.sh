#!/usr/bin/env bash
# Where does the time go? For each Rust server, under the same /json load:
#   * system calls per request (strace -c), and
#   * CPU time per request, split into user space and kernel.
#
#   bench/syscalls.sh                      (SERVERS="..." and ENDPOINT=users to vary)
#
# Requires: wrk, strace. Build first with bench/run.sh (or cargo build --release).
set -euo pipefail
cd "$(dirname "$0")"
SERVER_CPU=${SERVER_CPU:-1}
LOAD_CPU=${LOAD_CPU:-0}
ENDPOINT=${ENDPOINT:-json}
TICKS=$(getconf CLK_TCK)

declare -A CMD=(
  [tokio-raw]="tokio-raw/target/release/bench-tokio-raw"
  [native-epoll]="native-epoll/target/release/bench-native-epoll"
  [monoio]="monoio/target/release/bench-monoio"
  [glommio]="glommio/target/release/bench-glommio"
  [monoio-tuned]="env MONOIO_RING=defer monoio/target/release/bench-monoio"
  [giong]="giong/target/release/bench-giong"
  [giong-http]="giong-http/target/release/bench-giong-http"
)
declare -A PORT=([native-epoll]=3004 [tokio-raw]=3005 [giong]=3006 [monoio]=3007 [monoio-tuned]=3007 [glommio]=3008 [giong-http]=3009)

cpu_ticks() { cat /proc/"$1"/task/*/stat | awk '{u+=$14; s+=$15} END{print u, s}'; }

for s in ${SERVERS:-tokio-raw native-epoll monoio monoio-tuned glommio giong giong-http}; do
  taskset -c "$SERVER_CPU" ${CMD[$s]} >/dev/null 2>&1 &
  pid=$!
  sleep 1
  url="http://127.0.0.1:${PORT[$s]}/$ENDPOINT"
  taskset -c "$LOAD_CPU" wrk -t1 -c50 -d2s "$url" >/dev/null

  # CPU split, without strace attached.
  read -r u0 k0 <<<"$(cpu_ticks "$pid")"
  reqs=$(taskset -c "$LOAD_CPU" wrk -t1 -c50 -d8s "$url" | awk '/requests in/{print $1}')
  read -r u1 k1 <<<"$(cpu_ticks "$pid")"
  awk -v s="$s" -v r="$reqs" -v u=$((u1-u0)) -v k=$((k1-k0)) -v t="$TICKS" 'BEGIN{
    printf "%-13s %8.0f req/s | per request: %.2f us user + %.2f us kernel = %.2f us | kernel share %.0f%%\n",
      s, r/8, u/t/r*1e6, k/t/r*1e6, (u+k)/t/r*1e6, 100*k/(u+k) }'

  # Syscalls per request (strace slows the server down; only the ratio matters).
  tids=$(ls /proc/"$pid"/task | sed 's/^/-p /' | tr '\n' ' ')
  timeout 5 strace -c -o /tmp/strace.$s $tids 2>/dev/null &
  sp=$!
  sleep 0.5
  reqs=$(taskset -c "$LOAD_CPU" wrk -t1 -c50 -d3s "$url" | awk '/requests in/{print $1}')
  wait "$sp" 2>/dev/null || true
  calls=$(awk '$NF=="total"{print $4}' /tmp/strace.$s)
  awk -v c="$calls" -v r="$reqs" 'BEGIN{printf "              %.3f syscalls per request (%d calls / %d requests)\n", c/r, c, r}'
  awk 'NR>2 && $NF!="total" && $1+0>0 {printf "                %-16s %s calls\n", $NF, $4}' /tmp/strace.$s | head -4

  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  sleep 0.5
done
