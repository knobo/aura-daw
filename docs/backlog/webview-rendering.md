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
