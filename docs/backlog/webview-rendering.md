# Webview rendering — where AURA's UI CPU actually goes

Opened 2026-08-30 from the WebKitGTK 2.53.91 experiment (#133). This file
holds what was measured and what is still open. It is not a plan.

## The finding that started it

`docs/LANDED.md` records #132 closing the full-viewport repaint
investigation with an **accepted platform limit**: paint duration scaled
with window pixel area rather than damaged area, which was read as
WebKitGTK doing a full-surface raster on any invalidation.

That reading does not survive contact with a webview that has hardware
rendering. With the DMA-BUF renderer enabled, WebKit paints a 370x804
region, not the full 2864x1518 viewport, and each paint costs 0.4-0.7 ms
instead of ~455 ms. **The full-viewport quad was the software
rasteriser**, not a limit of the engine.

## Why AURA was on the software rasteriser

`src-tauri/src/main.rs` sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` at
startup unless the user has already chosen a value. That is the accepted
Tauri workaround ([tauri#9304]) for the DMA-BUF renderer white-flashing
or killing the web process when the GPU is saturated — in AURA'''s case by
ACE-Step inference.

The workaround costs the web process its GL context: with it set, only
the `libEGL`/`libGLX`/`libgbm` stubs are mapped and no driver
(`libEGL_nvidia`, `iris_dri`, …) is ever loaded, so Skia falls back to
its CPU raster pipeline. `perf` on the web process shows exactly that —
`ml3::lowp::bilerp_clamp_8888`, `gather_8888` and `gradient`, all
`SkRasterPipeline` stages — dominating the profile.

So the cost is paid on every frame of every session, while the bug it
avoids only appears under heavy GPU load.

[tauri#9304]: https://github.com/tauri-apps/tauri/issues/9304

## Measurements

WebKitGTK 2.53.91 built locally into `/opt/webkitgtk-2.53.91`; app idle,
transport stopped, 22 tracks; `WebKitWebProcess` CPU sampled from
`/proc/<pid>/stat` over 20-25 s.

| Configuration | web process CPU |
|---|---|
| `WEBKIT_DISABLE_DMABUF_RENDERER=1` (today'''s default) | 101% |
| `=0` | 36.4%, 39.8% |
| `=0` plus the meter redraw guard (#134) | 9.7%, 8.5%, 8.7% |

Two independent causes, roughly 11x together.

For reference, WebKitGTK 2.53.91 is also cheaper than the distro'''s
2.52.3 on the same idle page in MiniBrowser: 36.6% against 82.4%.

## Open

1. **Does the white-flash bug still exist on 2.53.91?** The whole
   question of whether the workaround can be narrowed or dropped rests on
   this, and it can only be answered by a human watching the window under
   a heavy AI job. Until then `main.rs` stays as it is.
2. **Playback still sits near 100% CPU.** The meter guard (#134)
   correctly does nothing there — the meters genuinely change every
   frame. The idle profile with DMA-BUF enabled points at
   `SkiaCompositingLayer::computeTransformsAndAnimations` and
   `TextBoxPainter`/`RenderBlock::paint`, i.e. text repainted and layers
   recomposed every frame. **No profile has been taken under playback
   yet** — that is the next measurement, not a conclusion.
3. **Nothing about 2.53.91 is proposed for the project.** It is a
   development release, absent from Ubuntu 24.04 and from every PPA, and
   shipping a non-distro webview is its own problem. It was built to
   answer a question, and it answered one.

## Reproducing

`scripts/run-with-webkit.sh` points a single run at a prefix that already
holds a locally built WebKitGTK; it does not build one. The build was:

```sh
cmake -S . -B build -GNinja -DPORT=GTK -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX=/opt/webkitgtk-2.53.91 \
  -DUSE_GTK4=OFF -DENABLE_DOCUMENTATION=OFF -DENABLE_INTROSPECTION=OFF \
  -DUSE_LIBBACKTRACE=OFF -DENABLE_MINIBROWSER=ON -DENABLE_SPEECH_SYNTHESIS=OFF
```

`USE_GTK4=OFF` is what makes it usable: it emits `webkit2gtk-4.1.pc`,
the same pkg-config name and API wry links against, so the result is a
drop-in at runtime. Every dependency minimum is met by Ubuntu 24.04;
`ENABLE_SPEECH_SYNTHESIS=OFF` only because noble ships no Flite dev
package.

## The matrix that answers it (2026-08-30, second pass)

The first pass compared 2.53.91-with-DMA-BUF against stock 2.52.3-without,
which moves two variables at once. Filling in the missing cells changes the
conclusion completely.

Method: web-process CPU from `/proc/<pid>/stat`, 25 s window, app restarted
per configuration, window maximised via `xdotool`, surface panel pinned open,
transport seeked to 0 and played over MCP so every run traverses the same
material. `cap=1` means only the first per-frame canvas widget (`Meter` or
`Gauge`) is allowed to draw; everything else about the page is unchanged.

| Webview | DMA-BUF | Idle | Playback | Playback, cap=1 |
|---|---|---|---|---|
| 2.52.3 (what ships today) | off | 101% | 103% | 102.8% |
| 2.52.3 | on | **4.8%** | 109% | **19.7%** |
| 2.53.91 | on | 8.7% | 116% | 20.2% |

Three things follow.

**1. AURA does not need WebKitGTK 2.53.** On every measure 2.53.91 is level
with or slightly worse than the 2.52.3 Ubuntu already ships. The webview
build was worth it as an instrument — it is what made the software
rasteriser visible — but there is nothing here to adopt.

**2. The idle win is the DMA-BUF workaround, not the webview.** Turning the
renderer on takes idle from 101% to 4.8% on the stock webview. That is the
largest single number in this investigation and it costs three lines in
`main.rs`. It is still gated on the white-flash question below.

**3. Canvas count drives playback, but only on the hardware path.** With the
renderer disabled, capping the number of animating canvases changes nothing
(102.8% vs 103%) — the whole surface is rastered on any invalidation
regardless, which is exactly the behaviour #132 measured and read as a
platform limit. With the renderer enabled, the same cap is the difference
between 19.7% and 109%. So consolidating the per-track meter canvases into
one compact canvas is worth building — *after* the renderer is on, not
before, and it would have measured as worthless if tried today.

The mechanism is inferred rather than measured: WebKit rasters in tiles, a
93x76 meter dirties a whole tile, and 22 track headers plus the surface
strips scatter dirty tiles across the viewport every frame. That also
explains three experiments that did nothing — drawing at 30 Hz instead of
60 (59.2% vs 62.0%), meter canvases at `dpr=1` (55.3%), and
`WEBKIT_SKIA_ENABLE_DDL=0` (63.3% against a +/-11 noise floor). None of them
change which tiles are dirty.

Two more things measured along the way, both negative and both worth not
repeating: the redundant-frame rate under playback is only ~25% (600
draw attempts/sec, ~150 skipped by #134's guard), so there is no second
idle-style win hiding in playback; and `surfaceHidesTrackMeters`, the
shipped preference for exactly this problem, did not move the number
(53.6% against a 56.7% baseline).

### Still open

- **The white-flash question, now with a price on it.** The workaround costs
  101% against 4.8% idle on the webview users already have. Even if the bug
  is real, disabling hardware rendering permanently is a steep price for
  something that only appears while the GPU is saturated by an AI job —
  narrowing it to the duration of such a job looks like the right shape.
  Still needs a human watching the window under load.
- **Meter canvas consolidation.** One compact canvas per region (track
  column, surface strips) rather than one per widget. Hit-testing for the
  clip-reset click and scroll sync are the real work.
- **A caveat on every playback number here.** The baseline moved from
  54-62% to 99-116% when this branch was rebased onto #121 (Plan V
  players). Two measurements, one merge — not evidence, but the doubling
  coincides with it and nothing else changed.
