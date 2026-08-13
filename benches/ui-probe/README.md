# ui-probe — WebKitGTK measurement harness

Produces the three measurements that gate AURA's render architecture
(`docs/CORE-REDESIGN-ROUND-2.md` §10.1, originally round 1 §9.3):

1. **pointer** — does `setPointerCapture` keep delivering `pointermove`
   (with out-of-bounds coordinates) and the final `pointerup` when the
   pointer is dragged past the *window* edge onto the desktop?
2. **gfx** — WebGL2 instanced-quad and Canvas2D `fillRect` throughput,
   plus a software-rasterizer (llvmpipe) tell, with and without
   `WEBKIT_DISABLE_DMABUF_RENDERER=1`.
3. **latency / latency-heavy** — keydown-to-paint-commit latency on a
   canvas-only page (double-rAF technique).

Results and interpretation: see `RESULTS.md`. Raw data: `results/*.json`.

## Why wry

The harness embeds WebKitGTK through **wry 0.55.1 + tao 0.35.3 — the exact
versions resolved by the product's `src-tauri/Cargo.lock`** (Tauri v2), so
the numbers are representative of what the shipped app would see. The
webview is built with `WebViewBuilderExtUnix::build_gtk` (the same path
Tauri uses on Linux). No fallback to raw webkit2gtk-rs was needed.

## Layout

- `src/main.rs` — one binary; test selected by CLI arg
  (`pointer | gfx | latency | latency-heavy`), optional second arg = output
  file stem. Page JS posts its JSON result via `window.ipc.postMessage`;
  the IPC handler writes `results/<stem>.json` and exits (180 s safety
  timeout if the page wedges).
- `html/*.html` — the three probe pages, embedded via `include_str!`.
- `scripts/run-pointer.sh` — drives 5 real XTEST drags with xdotool
  (3 with capture, 2 without), 300 px past the window's right edge.
- `scripts/run-latency.sh [simple|heavy]` — focuses the probe window and
  sends 100 XTEST keypresses at 5/s.
- `scripts/run-gfx.sh` — self-driving; runs the gfx page twice (default
  renderer path, then `WEBKIT_DISABLE_DMABUF_RENDERER=1`).
- `scripts/capture-env.py` — snapshots hardware/software to
  `results/env.json`.

## Rerunning

Requirements: X11 session with `DISPLAY` set, `xdotool`,
webkit2gtk-4.1 dev libraries, Rust.

```sh
cd benches/ui-probe
cargo build --release
./scripts/run-gfx.sh            # no input synthesis, ~1-2 min
./scripts/run-pointer.sh        # moves the REAL pointer for ~10 s
./scripts/run-latency.sh simple # types into the probe window for ~20 s
./scripts/run-latency.sh heavy
python3 scripts/capture-env.py
```

### Live-desktop safety

The pointer and latency scripts synthesize real X input:

- both save and restore the pointer position, and `run-pointer.sh` first
  waits for the pointer to be idle for 5 s;
- the pointer page discards interactions travelling < 250 px horizontally,
  so stray real clicks don't count as trials (they are logged under
  `ignoredShortInteractions`);
- the latency script sends keys in chunks of 10 and aborts if the probe
  window loses focus, so keystrokes cannot land in another application;
- GTK creates a phantom X window with the same title as the toplevel —
  the scripts resolve the real window by activating candidates and
  checking `xdotool getactivewindow` by name.

Do not run the input-synthesis scripts while a build is running, and keep
the probe window unoccluded during `run-gfx.sh` (hidden pages get rAF
throttled).
