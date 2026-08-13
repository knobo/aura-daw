# WebKitGTK gating measurements — results

Measured 2026-08-13 with `benches/ui-probe` (wry **0.55.1** + tao **0.35.3**,
pinned to the product's `src-tauri/Cargo.lock`, `build_gtk` path — i.e. the
exact WebKitGTK embedding Tauri v2 ships on this machine).
Raw data: `results/*.json`. These are the three §10.1 gate measurements
(round 1 §9.3); as far as we know these numbers have not been published
anywhere for WebKitGTK.

## Environment

- Ubuntu 24.04.4, kernel 6.8, X11 session, live desktop (not a clean lab —
  contamination controls described below)
- Intel i9-14900, 62.5 GiB RAM
- GPUs: **Intel UHD 770** (Raptor Lake-S GT1, primary X provider,
  `modesetting`, Mesa 25.2.8) + **NVIDIA RTX 4070 SUPER** (secondary
  provider `NVIDIA-G0`, reverse-PRIME driving the primary 4K monitor)
- Displays: 3840x2160@60 (primary, NVIDIA output) + 1680x1050@59.88;
  rAF vsync floor ≈ 16.7 ms
- WebKitGTK 2.52.3, GTK 3.24.41, Rust 1.94.1
- `glxinfo` renderer: `Mesa Intel(R) Graphics (RPL-S)` — hardware, not
  llvmpipe

## 1. setPointerCapture past the window edge — **PASS (3/3)**

600x400 window; 5 real XTEST drags from the window centre to 300 px beyond
the window's right edge, released **outside** the window. Trials 0-2 with
`setPointerCapture`, trials 3-4 without. (`results/pointer-capture.json`)

| trial | capture | moves | max clientX (innerWidth=600) | moves beyond width | pointerup delivered | capture lost early |
|---|---|---|---|---|---|---|
| 0 | yes | 37 | **1282.5** | 22 | **yes, outside (x=1282.5)** | no |
| 1 | yes | 51 | **1319.4** | 34 | **yes, outside (x=1319.4)** | no |
| 2 | yes | 38 | **1546.4** | 28 | **yes, outside (x=1546.4)** | no |
| 3 | no  | 272* | 598 (clamped) | 0 | stale up on re-entry (x=131) | n/a |
| 4 | no  | 107* | 598 (clamped) | 0 | **no — 3 s idle timeout** | n/a |

With capture: `pointermove` kept streaming while the button was held far
outside the window, with client coordinates **hundreds of px beyond the
window width** (never negative, no `pointercancel`, no early
`lostpointercapture`); inter-move gap never exceeded 16 ms (one frame);
`pointerup` was delivered even though the release happened on the desktop
outside the window. Without capture: coordinates clamp at the window edge
(max 598 of 600) and the outside release is not delivered — the drag ends
via timeout or a stale `pointerup` when the pointer re-enters.

*Live-desktop artifacts: trials 3-4 have inflated move counts because
without capture the missing `pointerup` makes consecutive physical drags
bleed into one logical trial; two stray real-user clicks were filtered by
the ≥250 px travel guard (`ignoredShortInteractions`). Neither affects the
verdict.*

**Verdict: window-edge drag-out is safe in WebKitGTK — arranger drags
(clip drag, marquee, fader sweeps) can rely on Pointer Events capture
semantics. `setPointerCapture` on `pointerdown` is mandatory, not
optional: without it, moves clamp at the edge and the release is lost.**

## 2. WebGL2 / Canvas2D throughput — hardware-accelerated, with one big caveat

1280x720 canvas. 120 rAF frames per config (10 warmup), plus a 30-frame
synced pass (draw + 1 px `readPixels` full flush) that measures true frame
work without the vsync floor. (`results/gfx*.json`)

- WebGL2 context: **created OK**, `MAX_TEXTURE_SIZE` = 16384 (32768 with
  dmabuf off)
- Renderer string: masked = `WebKit WebGL`; `WEBGL_debug_renderer_info`
  "unmasked" = **`Apple GPU` / `Apple Inc.`** — WebKitGTK 2.52
  fingerprint-resistance returns a hardcoded lie on Linux, so **renderer
  strings are useless for llvmpipe detection in WebKitGTK; only the timing
  tell works**. Real GPU per lspci/glxinfo: Intel UHD 770 (Mesa RPL-S).

### Bench A — WebGL2 instanced quads (default renderer path, run 1 / run 2)

| N instances | achieved FPS | rAF ms p50 / p95 | JS submit p50 | synced frame work ms p50 / p95 |
|---|---|---|---|---|
| 10 000 | 46.7 / 40.2 | 18/42 · 24/48 | 0 ms | **5 / 15** · **2 / 4** |
| 50 000 | 26.6 / 46.7 | 37/66 · 17/49 | 0 ms | 4 / 12 · 7 / 16 |
| 200 000 | 47.6 / 35.8 | 17/36 · 26/47 | 0 ms | **10 / 34** · **10 / 24** |

### Bench A with `WEBKIT_DISABLE_DMABUF_RENDERER=1`

| N instances | achieved FPS | rAF ms p50 / p95 | synced frame work ms p50 / p95 |
|---|---|---|---|
| 10 000 | **62.2** | 16 / 17 | 0 / 1 |
| 50 000 | **62.2** | 16 / 17 | 1 / 2 |
| 200 000 | **62.2** | 16 / 17 | 0 / 1 |

### Bench B — Canvas2D fillRect

| N rects | default path FPS (rAF p50/p95) | dmabuf-off FPS (rAF p50/p95) | JS draw ms p50 |
|---|---|---|---|
| 5 000 | 35.5 (26/48) | **62.2** (16/17) | 2 (6 dmabuf-off) |
| 20 000 | 45.8 (17/49) | 42.0 (24/25) | 9 (20 dmabuf-off) |

### Bench C — llvmpipe tell: **LIKELY-HARDWARE**

Synced frame work for 10k quads: p50 = 5 ms (run 1) / 2 ms (run 2), well
under the 8 ms PROBABLE-SOFTWARE threshold; even 200k quads complete in
~10 ms p50. llvmpipe would be far above this. Consistent with the real
Intel UHD 770 doing the rendering.

### Interpretation

- **Throughput is not the constraint.** 200k instanced quads/frame — far
  beyond any realistic arranger scene — cost ~10 ms of GPU work on the
  *iGPU* (0-1 ms on the dmabuf-off path), with JS submit ~0 ms. Canvas2D
  at 20k rects costs ~9 ms of main-thread JS per frame, so Canvas2D is
  usable for moderate scenes but WebGL2 is the right choice for dense ones.
- **Frame pacing on the default dmabuf path is the real finding.** The
  default WebKitGTK DMA-BUF renderer delivered erratic 27-48 FPS with rAF
  p95 of 36-66 ms *independent of scene complexity* (non-monotonic in N —
  pacing noise, not GPU load; reproduced across two runs). With
  `WEBKIT_DISABLE_DMABUF_RENDERER=1` the same workloads lock to a flat
  62 FPS / 16-17 ms every frame. Plausible cause on this box: dmabuf
  buffer sharing across the Intel-render → NVIDIA-scanout (reverse PRIME)
  split. Treat the env var as a known, shippable mitigation knob and
  re-test pacing on single-GPU machines.

## 3. keydown → paint-commit latency — p50 ≈ 22 ms (~1.3 frames)

1600x900 canvas-only page, 100 discrete XTEST keypresses at 5/s per mode;
keydown handler repaints synchronously, then double-rAF: the timestamp is
taken in the *second* rAF callback, minus `event.timeStamp`.
(`results/latency-simple.json`, `results/latency-heavy.json`)

| mode | n | min | p50 | p95 | max | sync draw call p50 |
|---|---|---|---|---|---|---|
| simple (region + bar) | 100 | 14 | **22** | **30** | 33 | 0 ms |
| heavy (3-layer full-canvas gradient) | 100 | 16 | **25** | **32** | 34 | 0 ms |

Event queueing delay (`performance.now()` at handler entry minus
`event.timeStamp`) was ≈ 0 (−2 to +1 ms; slightly negative because
WebKitGTK derives `event.timeStamp` from the X server clock, which runs
~2 ms ahead of the page's `performance.now()` — so the distributions carry
roughly ±3 ms clock skew). Scaling from simple to a full-canvas 3-gradient
repaint costs only ~3 ms at p50 — commit latency is frame-scheduling-bound,
not raster-bound, at this canvas size.

**Stated limitation:** this measures event-delivery-to-paint-commit
*inside the page*, not photon latency. OS/X11/WebKitGTK input latency
before `event.timeStamp` is not captured (xdotool injects at the X server,
so the synthesis path is realistic from the server onward, but
device→server time is absent), and the double-rAF technique can
overestimate by up to one frame (it timestamps the start of the frame
*after* the commit). True figures are therefore bounded: commit happens
between ~1 and ~2 frames after keydown, plus display scanout.

## Does anything here invalidate a WebView-hosted arranger?

**No.** All three gates pass on the real Tauri v2 stack (wry 0.55.1 /
WebKitGTK 2.52.3): pointer capture behaves to spec across the window edge
(3/3, with out-of-bounds coordinates and reliable `pointerup`), so
drag-based editing has no input-model blocker; WebGL2 is genuinely
hardware-accelerated on this machine and instanced drawing absorbs 200k
quads/frame in ~10 ms of iGPU work — an order of magnitude beyond arranger
needs; and keypress-to-paint-commit is ~22 ms p50 / 30 ms p95 (~1.3-2
frames), which is competitive with native toolkit UIs that also commit on
the next vsync. The one genuine risk found is not throughput but **frame
pacing on WebKitGTK's default DMA-BUF renderer** (erratic 27-48 FPS on
this dual-GPU reverse-PRIME setup vs a flat 60 FPS with
`WEBKIT_DISABLE_DMABUF_RENDERER=1`); that argues for shipping a
pacing self-check plus the documented env-var fallback, and for re-running
this probe on single-GPU and Wayland machines — it does not argue against
the WebView architecture. Two things remain unknown: photon (glass-to-eye)
latency, which this harness cannot see, and whether the renderer string
masking ("Apple GPU") ever hides a real llvmpipe fallback on other
machines — the shipped self-check should use the timing tell from Bench C,
never the string.
