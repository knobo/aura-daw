#!/bin/bash
# Pointer-capture probe driver. Synthesizes real X11 pointer input via xdotool
# (XTEST), so it briefly takes over the pointer. Saves and restores the pointer
# position. Total synthesized-input time: ~10 s (5 drags of ~1.5-2 s each).
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=target/release/ui-probe
OUT=results/pointer-capture.json
rm -f "$OUT"

# This is a LIVE desktop: wait until the real pointer has been still for 5 s
# so the user's own mouse activity does not interleave with synthetic drags.
echo "waiting for pointer to be idle for 5 s (max 120 s)…"
IDLE=0; PREV=""
for i in $(seq 1 120); do
  CUR=$(xdotool getmouselocation | cut -d' ' -f1-2)
  if [ "$CUR" = "$PREV" ]; then IDLE=$((IDLE + 1)); else IDLE=0; fi
  PREV=$CUR
  [ "$IDLE" -ge 5 ] && break
  sleep 1
done
echo "pointer idle for ${IDLE}s, proceeding"

# Save current pointer position so we can put it back.
eval "$(xdotool getmouselocation --shell)"   # sets X Y SCREEN WINDOW
SAVED_X=$X; SAVED_Y=$Y

cleanup() {
  xdotool mouseup 1 2>/dev/null || true
  xdotool mousemove "$SAVED_X" "$SAVED_Y" 2>/dev/null || true
  kill "$APP" 2>/dev/null || true
}
trap cleanup EXIT

"$BIN" pointer pointer-capture &
APP=$!
sleep 2

# GTK creates a phantom X window with the same title; activate every candidate
# and trust getactivewindow (verified by name) to identify the real toplevel.
WID=""
for attempt in $(seq 1 20); do
  for cand in $(xdotool search --onlyvisible --name '^ui-probe-pointer$' 2>/dev/null); do
    xdotool windowactivate --sync "$cand" 2>/dev/null || true
  done
  sleep 0.5
  ACT=$(xdotool getactivewindow 2>/dev/null || echo "")
  if [ -n "$ACT" ] && [ "$(xdotool getwindowname "$ACT" 2>/dev/null)" = "ui-probe-pointer" ]; then
    WID=$ACT; break
  fi
done
[ -n "$WID" ] || { echo "FAIL: probe window not found/focused"; exit 1; }
sleep 0.3

eval "$(xdotool getwindowgeometry --shell "$WID")"   # sets X Y WIDTH HEIGHT SCREEN
WX=$X; WY=$Y
echo "window: pos=$WX,$WY size=${WIDTH}x${HEIGHT} (physical px)"

SX=$((WX + WIDTH / 2)); SY=$((WY + HEIGHT / 2))
EX=$((WX + WIDTH + 300))   # 300 px beyond the right window edge
echo "drag: $SX,$SY -> $EX,$SY (edge at $((WX + WIDTH)))"

for trial in 1 2 3 4 5; do
  echo "trial $trial"
  xdotool mousemove "$SX" "$SY"; sleep 0.15
  xdotool mousedown 1;            sleep 0.15
  x=$SX
  while [ "$x" -lt "$EX" ]; do
    x=$((x + 60)); [ "$x" -gt "$EX" ] && x=$EX
    xdotool mousemove "$x" "$SY"
    sleep 0.02
  done
  sleep 0.4                       # hold beyond the edge
  xdotool mouseup 1               # release OUTSIDE the window
  sleep 0.7
done

# Wait for the page to post its JSON (no-capture trials may need idle timeouts).
for i in $(seq 1 60); do
  [ -f "$OUT" ] && break
  kill -0 "$APP" 2>/dev/null || break
  sleep 1
done

if [ -f "$OUT" ]; then echo "OK: $OUT"; else echo "FAIL: no result"; exit 1; fi
