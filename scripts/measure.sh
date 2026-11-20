#!/bin/bash
# Measures rewindd's own CPU and energy impact while capturing.
# Run from a terminal that has Screen Recording permission.
set -u

cd "$(dirname "$0")/.." || exit 1
cd daemon
BIN=./.build/debug/rewindd
SECONDS_PER_CASE=${SECONDS_PER_CASE:-40}

sample() {
  top -l 2 -s 1 -stats pid,command,cpu,power 2>/dev/null \
    | awk '$2 == "rewindd" { cpu=$3; power=$4 } END { print cpu, power }'
}

run_case() {
  local name=$1; shift
  local log="/tmp/rewind-measure-$name.log"
  $BIN run --seconds "$SECONDS_PER_CASE" --stats 10 "$@" > "$log" 2>&1 &
  local pid=$!
  sleep $((SECONDS_PER_CASE - 12))
  local reading
  reading=$(sample)
  wait "$pid" 2>/dev/null
  printf '%-14s cpu/power = %-12s  %s\n' "$name" "$reading" "$(grep -o 'in=.*' "$log" | tail -1)"
}

echo "baseline (nothing capturing): $(sample)"

run_case idle-1080p30   --display 0
run_case idle-720p15    --display 0 --height 720 --fps 15

ffplay -loglevel quiet -noborder -autoexit -t $((SECONDS_PER_CASE * 3)) \
  -f lavfi -i testsrc2=size=1400x900:rate=30 &
FFPLAY=$!
sleep 3

run_case active-1080p30 --display 0
run_case active-720p15  --display 0 --height 720 --fps 15

kill "$FFPLAY" 2>/dev/null
wait "$FFPLAY" 2>/dev/null
