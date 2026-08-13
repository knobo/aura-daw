#!/bin/bash
# Keydown-to-paint latency driver. Usage: run-latency.sh [simple|heavy]
# Sends 100 real XTEST keypresses at 5/s (~20 s) to the focused probe window.
# Do not type during the run; keystrokes go to the probe window only.
set -euo pipefail
cd "$(dirname "$0")/.."
MODE=${1:-simple}
BIN=target/release/ui-probe
case "$MODE" in
  simple) TEST=latency;       OUT=results/latency-simple.json; STEM=latency-simple ;;
  heavy)  TEST=latency-heavy; OUT=results/latency-heavy.json;  STEM=latency-heavy ;;
  *) echo "mode must be simple|heavy"; exit 2 ;;
esac
rm -f "$OUT"

eval "$(xdotool getmouselocation --shell)"
SAVED_X=$X; SAVED_Y=$Y
cleanup() {
  xdotool mousemove "$SAVED_X" "$SAVED_Y" 2>/dev/null || true
  kill "$APP" 2>/dev/null || true
}
trap cleanup EXIT

"$BIN" "$TEST" "$STEM" &
APP=$!
sleep 2

# GTK creates a phantom X window with the same title; activate every candidate
# and trust getactivewindow (verified by name) to identify the real toplevel.
WID=""
for attempt in $(seq 1 20); do
  for cand in $(xdotool search --onlyvisible --name '^ui-probe-latency$' 2>/dev/null); do
    xdotool windowactivate --sync "$cand" 2>/dev/null || true
  done
  sleep 0.5
  ACT=$(xdotool getactivewindow 2>/dev/null || echo "")
  if [ -n "$ACT" ] && [ "$(xdotool getwindowname "$ACT" 2>/dev/null)" = "ui-probe-latency" ]; then
    WID=$ACT; break
  fi
done
[ -n "$WID" ] || { echo "FAIL: probe window not found/focused"; exit 1; }
echo "probe toplevel WID=$WID (focused)"
sleep 0.3

# 100 discrete presses, 200 ms apart (5/s), via XTEST to the focused window.
# Sent in chunks of 10; before each chunk verify the probe window still has
# focus (live desktop: if the user clicks elsewhere, abort rather than type
# into their window).
for chunk in $(seq 1 10); do
  ACTIVE=$(xdotool getactivewindow 2>/dev/null || echo 0)
  if [ "$ACTIVE" != "$WID" ]; then
    echo "ABORT key send: probe window lost focus (chunk $chunk)"; break
  fi
  xdotool key --repeat 10 --repeat-delay 200 a
done

for i in $(seq 1 40); do
  [ -f "$OUT" ] && break
  kill -0 "$APP" 2>/dev/null || break
  sleep 1
done

if [ -f "$OUT" ]; then echo "OK: $OUT"; else echo "FAIL: no result"; exit 1; fi
