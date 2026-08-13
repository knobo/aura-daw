#!/bin/bash
# Graphics throughput driver. Self-driving page, no input synthesis.
# Runs twice: default renderer path, then with WEBKIT_DISABLE_DMABUF_RENDERER=1.
# Keep the probe window unoccluded while it runs (rAF throttles when hidden).
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=target/release/ui-probe

echo "== run 1: default WebKitGTK renderer path =="
rm -f results/gfx.json
"$BIN" gfx gfx
echo
echo "== run 2: WEBKIT_DISABLE_DMABUF_RENDERER=1 =="
rm -f results/gfx-dmabuf-off.json
WEBKIT_DISABLE_DMABUF_RENDERER=1 "$BIN" gfx gfx-dmabuf-off
