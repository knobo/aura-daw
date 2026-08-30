# Landed — do not restart

Everything in this table is merged to `main`. If a job you are about to
start appears here, you are about to redo it. Open the pointer instead.

Newest first.

## The DMA-BUF workaround is gone, and hardware rendering is the default

`main.rs` set `WEBKIT_DISABLE_DMABUF_RENDERER=1` at startup for everyone —
the accepted Tauri workaround ([tauri#9304]) for the renderer white-flashing
or killing the web process when the GPU is saturated by ACE-Step inference.

#136 measured what that cost. Disabling the renderer denies the web process
a GL context, so Skia falls back to its CPU raster pipeline and every frame
of every session is rastered in software. Idle, transport stopped, on the
WebKitGTK 2.52.3 Ubuntu 24.04 ships: **101% of a core with the workaround,
4.8% without**. Paid continuously, against a bug that only appears under
heavy GPU load.

The block is deleted rather than inverted, so WebKit reads the environment
itself and the escape hatch is the plain one: a user who does hit the flash
sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` and has the old behaviour back.
README's Linux/GPU troubleshooting section now leads with that instead of
explaining how to opt out of the workaround.

**What was and was not verified.** The app was run in exactly this
configuration (stock 2.52.3, renderer enabled, hardware GL and
`SkiaGPUWorker` threads both confirmed in the running process) through
several minutes of ordinary use with no flash and no web-process respawn.
The heavy case was **not** reached: no ACE-Step job ran, so the load the
workaround exists for is still untested. That is the known gap, and the
escape hatch is what covers it.

PR: `fix/dmabuf-renderer-default` → #137.
→ [`docs/backlog/webview-rendering.md`](backlog/webview-rendering.md)

[tauri#9304]: https://github.com/tauri-apps/tauri/issues/9304

## AURA does not need WebKitGTK 2.53 — it needs its own DMA-BUF workaround gone

#133 built WebKitGTK 2.53.91 to re-measure #132's accepted platform limit,
and found AURA running on Skia's software rasteriser because `main.rs`
disables the DMA-BUF renderer. #134 fixed the idle meter redraw on top of
that. Playback moved in neither, and this closes out why.

The first pass compared 2.53.91-with-DMA-BUF against stock 2.52.3-without,
which moves two variables at once. Filling in the missing cells reverses the
reading:

| Webview | DMA-BUF | Idle | Playback | Playback, cap=1 |
|---|---|---|---|---|
| 2.52.3 (ships today) | off | 101% | 103% | 102.8% |
| 2.52.3 | on | 4.8% | 109% | 19.7% |
| 2.53.91 | on | 8.7% | 116% | 20.2% |

**2.53.91 is level with or slightly worse than the webview Ubuntu already
ships**, on every measure. The build was worth it as an instrument — it is
what made the software rasteriser visible — but there is nothing to adopt,
and shipping a non-distro webview is a problem the project does not have to
take on after all.

**The idle win belongs to the workaround, not the webview**: 101% to 4.8% on
stock 2.52.3, three lines in `main.rs`. Deliberately not made — it is gated
on whether the white-flash bug the workaround exists for is still real, which
only a human watching the window under a heavy AI job can answer. What
changed is that the question now has a price on it.

**Canvas count drives playback, but only on the hardware path.** With the
renderer off, capping how many meter canvases animate changes nothing
(102.8% vs 103%) — the whole surface is rastered on any invalidation, which
is exactly what #132 measured. With it on, the same cap is 19.7% against
109%. So consolidating the per-track meter canvases into one compact canvas
per region is worth building, but only after the renderer is on; tried
today it would have measured as worthless and been abandoned.

Four negatives are recorded in the backlog file so nobody repeats them: 30 Hz
drawing, `dpr=1` meter canvases, `WEBKIT_SKIA_ENABLE_DDL=0`, and the shipped
`surfaceHidesTrackMeters` preference all sat inside a +/-11 noise floor. The
redundant-frame rate under playback is only ~25%, so there is no second
idle-style win hiding there either.

PRs: `exp/webkitgtk-2.53` → #133, `fix/meter-idle-redraw` → #134,
`perf/playback-cpu` → #136.
→ [`docs/backlog/webview-rendering.md`](backlog/webview-rendering.md)

## The meter canvas redrew every frame whether or not anything moved

Found by profiling the webview, not from the backlog. `Meter.svelte`
cleared and refilled its canvas on every animation frame regardless of
whether the picture changed. Stopped and silent, the lane ballistics
(`disp`, `rms`, `holdDb`) decay to `DB_MIN` in about two seconds and stay
pinned there, so every frame after that drew a pixel-identical picture,
dirtied the canvas anyway, and made the compositor re-upload its tile —
22 tracks on screen means 22 canvases at 60/sec of work that changes
nothing.

The draw now advances both lanes' ballistics first, then compares a
signature of what the drawing depends on (peak, rms and hold positions
quantised to the half-pixel that survives to the canvas, plus the clip
latch) and returns early when it matches the last frame drawn.
Quantising rather than comparing raw dB is the point: the decay never
settles exactly, so raw values would differ forever by amounts too small
to see.

`ResizeObserver` resets the signature, and that reset carries the
correctness: writing `canvas.width` clears the canvas, so the picture the
signature stands for is gone while the levels behind it have not moved.
Without it the meter goes blank on resize and — stopped and silent, where
no level ever changes again — stays blank.

Idle web-process CPU over 25s, A/B/A against the running dev server:
36.4%/39.8% without, 9.7%/8.5%/8.7% with.

**This does not touch playback.** The meters genuinely change every frame
there, the signature always differs, and the app still sits near 100% CPU
with something playing. That is open, and the profile taken with the
DMA-BUF renderer enabled points at `SkiaCompositingLayer::
computeTransformsAndAnimations` and `TextBoxPainter`/`RenderBlock::paint`
— text being repainted and layers recomposed every frame — as where to
look next. No playback profile has been taken yet.

PR: `fix/meter-idle-redraw` → #134. The webview experiment it was found
in is #133.

## The WebKitGTK full-viewport repaint, closed out: one real bug, one accepted platform limit

Direct follow-up to #131's "duplicate meter rendering" entry below — that
PR's `contain`/canvas work cut paint duration ~3.8× (611ms → 161ms avg)
but every paint still covered the exact full viewport, and the question
left open was why.

**The real bug, found and fixed:** `TransportBar.svelte`'s clock/bars and
`MasterBar.svelte`'s dB readout wrote `textContent` unconditionally on
every rAF tick, unlike `Meter.svelte`/`Gauge.svelte` which already guard
against writing an unchanged value. While transport is `"stopped"`,
`positionAt()` returns a fixed value, so this wrote the identical string
60×/sec forever, even fully idle. A WebKit Timelines capture with the app
sitting still (no playback, no interaction) showed a full-viewport
invalidate-styles → recalculate-styles → layout → paint cycle repeating
at ~160ms regardless — this was the entire cause. Fixed with the same
`if (text !== shown)` guard the other two already use; idle re-capture
afterward showed zero non-paint layout events.

**The platform limit, measured and accepted:** full-viewport paint under
real interaction (resizing the Surface panel, with and without playback)
persists, and its duration scales with window pixel area almost exactly
(2× the area measured as 1.94× the duration across two window sizes) —
not with how much content actually changed. That's WebKitGTK doing a
full-surface raster on any invalidation rather than an incremental/
tile-based repaint of the damaged region, and it isn't reachable from
application code. WebAssembly/SIMD was raised and ruled out in the same
sitting: script execution was already under 5% of total time in every
capture across this whole investigation (idle, resize, resize+play) —
the cost is entirely in rasterization, which happens identically no
matter what issued the draw calls. A `Forced Layout` also turned up
(`TransportBar.svelte`'s `recompute()` reading `offsetWidth` right after
the clock tick left layout dirty) — sub-millisecond, 17 times over
14.6s, not worth chasing next to the paint cost. All three findings are
in `STANDING-CONSTRAINTS.md`'s "No continuously-animated SVG" section,
which is now the first place to look if a WebKit Timelines capture ever
shows a full-page quad again.

Reaching further than this needs WebKitGTK's own compositing/GPU-
acceleration configuration — an environment-level investigation, not an
application-code one.

PR: `investigate/surface-repaint` → #132.

## Pointer/zoom drift and duplicate meter rendering, found by profiling the real app under load

Not from the backlog — surfaced by asking "why does the native build feel
laggy compared to the browser demo" and following the evidence. Two
unrelated problems, fixed in the same branch because the second was found
while chasing the first.

**Pointer math under interface zoom.** `clientX`/`getBoundingClientRect()`
report VISUAL px (already multiplied by `prefs.uiZoom`), but drag deltas
and hit-zone constants across the codebase assumed CSS `zoom` keeps
everything in one coordinate space — it doesn't; `clientWidth`/layout
constants stay in LAYOUT px. At 150% zoom this meant clips, automation
clips, panel resizes, faders, knobs, the H-scrollbar thumb, and the
floating launch-map window all moved 1.5× the pointer distance; portalled
popovers (`+` menu, bind picker) landed off their anchor because their
`left`/`top` got multiplied by the zoom a second time inside the zoomed
`document.body`. Fixed with a new `uiZoomFactor()` (`utils/ui-zoom.ts`)
divided in at every mixed site. Verified empirically: unpatched clip drag
measured exactly 1.5000 in a Playwright ratio test at 150% zoom; patched,
1.0000.

**Duplicate/expensive meter rendering.** A WebKit Timelines capture on the
native (Tauri/WebKitGTK) build — not visible in browser demo mode, which
uses Chromium/Gecko — showed per-frame DOM/SVG mutation (the control
surface's `Gauge.svelte`, up to 44 SVG segments or 26 `<i>` rungs toggled
every frame) folding into full-viewport repaints, ~600ms each. Rewrote
`Gauge.svelte` to canvas (matching `Meter.svelte`'s existing architecture,
now the standing pattern — see `STANDING-CONSTRAINTS.md` "No
continuously-animated SVG"), added `contain`/`will-change` isolation
across every other continuous rAF site, added `runWhileVisible()`
(`utils/visible-raf.ts`, IntersectionObserver-gated) so an off-screen
meter/gauge stops scheduling entirely, and fixed the resulting duplicate
render when the control surface and a track header show the same track's
level at once (opt-in `surfaceHidesTrackMeters` pref, off by default).
Paint duration dropped ~3.8× on re-measurement (611ms → 161ms avg) but the
full-viewport-quad pattern itself persisted at reduced cost — logged as
still-open in `STANDING-CONSTRAINTS.md` rather than claimed fixed.

An Opus review of the branch before merge caught a real regression in the
visibility-gating fix (clip detection was gated along with the redraw, so
a clipped track scrolled out of view never latched its LED) and a
containment-scope bug (TransportBar's always-on clock lost its shared
baseline with the bar counter and had its playing-state glow clipped) —
both fixed in the same branch; see the PR for the full list.

PR: `fix/pointer-zoom-scaling` → #131.

## Startup progress: the boot has two clocks, and the slower one is invisible to the caller

Opening the last project on launch took seconds and looked identical to a
hang: the window painted white in dev, then an empty shell, with nothing to
tell "loading" from "wedged". Two events now carry real progress to the UI.
`project://open-progress` fires before each of the nine stages of
`ControlPlane::open_project_epoch`, with a per-instance detail while
plugins instantiate. `project://media-progress` fires per file from the
engine control thread's `ensure_loaded` — which is the SECOND clock: it
runs after `open_project` has already returned, which is exactly why the
old "await the open command" model could never have reported it.
`index.html` now paints a dark branded first frame before Svelte mounts (in
dev, `app.css` is injected by JS, so the flash was genuinely white), and
`main.ts` clears its mount target first, since Svelte 5's `mount()` appends
rather than replaces.

Measured on this machine restoring `~/Music/AURA/TestProsjekt.aura` (6
restored plugin instances, ~2 GB project dir, 7.6 MB journal):

    open: project loaded in 0 ms
    open: midi adopted in 0 ms
    open: journal checked in 21 ms
    open: plugins adopted in 2221 ms
    open: automation adopted in 0 ms
    open: midi outputs adopted in 1 ms
    open: modulation adopted in 1 ms
    open: rebuild dispatched in 0 ms
    open: project opened in 2247 ms total
    media: prepared 6 file(s)/clip(s) in 2121 ms

Plugin instantiation is essentially the whole first clock and media decode
is a second, near-equal one hiding after it — both are why, not just that;
see [`TRAPS.md`](TRAPS.md) §Backend. The `open:`/`media:` `log::info!`
lines are permanent, so the next slow start is readable from a log without
reproducing it live.

→ PR #130. No backlog file: this was reported live, not planned.

## Brushed Steel, a 3D frame, and four meter faces

A sixth material token, `brush`: the anisotropic streak a wire wheel leaves,
as distinct from `grain`'s direction-less speckle. Its own token because a
real panel carries both at different strengths — bead-blasted metal is
speckled and not brushed, brushed steel is streaked and only faintly
speckled, moulded plastic is neither. It is layered in through
`.grain::before`, so every surface that already declares what it is made of
picks up the finish with no markup change and no visual difference at
`brush: 0`, which is where all eleven pre-existing built-ins are left.

The texture needs a filter chain `--grain-tex` does not, and the first cut
proved it: raw feTurbulence is colour noise with a noisy alpha, and blended
`overlay` at 0.85 it painted rainbow streaks across every panel. It is now
desaturated, its alpha flattened, and its range squeezed into 0.35–0.65 —
a narrow band around the value `overlay` leaves the backdrop alone at.

`--bevel-frame` is a four-sided moulded rim for the things that are BOXES —
instrument racks, module blocks, channel strips, pad decks, the clip list.
At that size two horizontal lips read as a card with a highlight; four read
as a bezel with thickness. Small controls keep the two lips.

The knob is redrawn as a ring of LEDs around a collared cap with a raised
inner dome, the indicator a single lit lamp rather than a painted line. The
leading lamp lights fractionally, so a slow drag reads as continuous rather
than stepping between 31 positions.

Two new built-ins the token exists for: **Brushed Steel Dark** and **Brushed
Steel Light**. The light half is not the dark half inverted — its ramp runs
upward and its `sheen` is held *below* the dark half's, because a white dome
highlight on a near-white knob cap reads as plastic rather than as metal.

And the round gauge went from one face to four (`meterFace` preference):
LED ARC, DIAL, LADDER, ANALOG VU. Three of them gained a peak hold; the VU
cannot have one, because a needle *is* the peak.

The channel strip's mute/solo/arm row was **overflowing its own strip**:
3 × 28px keys plus 2 × 4px gaps is 92px of content inside a 76px box, so the
outer keys sat on the strip's bevel and the row began at x=0 of a strip with
8px padding — measured in the browser, not eyeballed. It is now a `1fr` grid
with `min-width: 0` (a grid item's default `min-width: auto` is what would
otherwise let it grow straight back), inset by its own margin so it is not
the one thing on the strip touching the walls, and in two shapes behind a
`stripKeys` preference: moulded KEYS or one SEGMENTED switch. The compact key
says **A**, matching the track header — `R` was already spent on automation
Read in the same header row.

→ [`themes.md`](themes.md) §6 (material), §8 (meter faces), §9 (strip keys).
## The layout ran out of room in five places, and the track rail can be dragged

Started from a headless overflow scan (Chrome, browser demo mode) at
1280x800, 1440x900 and 1920x1080 with every dock panel open, comparing
each element's scroll size against its client box and its rect against
the viewport; continued from two owner screenshots of a real project
that the scan's demo data could not produce.

- **The empty piano roll held 340px to say one sentence** — 42% of an
  800px window, at every size. Collapsed to its tab strip; the height is
  remembered, not spent. The track rail went from 3 of 7 lanes visible
  at 1280x800 to all 7 at 1920x1080. This was the single biggest reason
  everything else felt cramped.
- **The master strip pushed `jobs`/`idle` to x=1293 in a 1280px window**,
  unreachable. The gap gives first now, the meter shrinks to a floor,
  and the sections carry `min-width: 0`.
- **The lane routing row wrapped inside a box that cannot grow.**
  `.header` is a fixed `--track-height` and must match the lane column
  row for row, so a wrapped line rendered on top of the row beneath it
  (measured: row 17px tall, scrollHeight 48, chips at y=52 over a
  status row at y=45). `nowrap`, with a stated shrink order and a floor
  under every chip — a chip allotted 0px is invisible and unclickable.
- **Three chips stopped saying the default out loud**, following the
  owner's own 2026-08-24 pattern: `Instrument · ` off the instrument
  chip, `MASTER` off an output chip that is routed to the master, and
  the `A` kind chip off plain audio lanes. That is the space the names
  now use.
- **The track rail is draggable** (`ui.railWidth`, 312px…45vw). 312 is
  derived from the routing row's own floors, not chosen; below it the
  chips would have to overlap again.

Five frontend traps came out of it and are in
[`TRAPS.md`](TRAPS.md) §Frontend — the sharpest being that a CSS
selector written for a child component's element matches nothing at all.

→ PR #128. No backlog file: this was reported live, not planned.

## A MIDI port id must not encode its place in a list

The `midi_out::tests::*` "thread race" that had kept `--test-threads=1` in
CI was not a thread race and was not confined to tests. A port id is minted
as `"<name>#<index>"` and was resolved by exact string match; the index is a
position in `midir`'s ALSA-seq enumeration, which renumbers every port after
one that CLOSES. Measured with no test parallelism at all: a port at
`"…129:0#3"` becomes `"…#2"` when an unrelated earlier port goes away, and
the id handed out a moment earlier stops resolving — for a port with the
same client, name and ALSA address, still plainly there.

`resolve_port_id` makes the index a tiebreaker rather than an identity, on
both the input and output side. Persisted routing was never exposed
(`midi_out::persist` keys by name); the hazard is holding an id across
anything that closes a port. Fixing it exposed three timing assumptions the
port failures had been masking — loopback tests whose position thread
outran the port being opened, and a clap_host test guessing at the depth of
the process-wide `plugin_main()` queue.

Default parallelism, one machine, one sitting: `midi_out::` went 0 of 10
fully green to 25 of 25, the full `--lib` suite 0 of 8 to 45 of 46. **CI
keeps `--test-threads=1`**: the one remaining failure is PR #124's
PDEATHSIG test, whose child survived a SIGKILLed parent for over 30 s once.
It is **on hold by the owner's decision**, with the two candidate causes and
the conditions for reopening written down — it is not an unclaimed job.

→ [`backlog/ci-hardening.md`](backlog/ci-hardening.md) item 6,
[`TRAPS.md`](TRAPS.md) §Backend and §Tests.

## A plugin window closing must not kill AURA

Three process-death bugs on the plugin path, all of them shaped the same
way: something outside our control ends, and AURA (or its test binary)
disappears without a panic, a signal or a log line.

- **`wm_stack` restacked window ids `xdotool` found a round trip earlier.**
  When the window had closed in between, **Xlib's default error handler
  called `exit(1)`** — in the app, the session's unsaved work. X errors
  are asynchronous, so the guard has to sync BEFORE it restores the
  previous handler; the other order protects nothing, measured both ways.
  Ours answers only for our own display connection, so GTK's is untouched.
- **`tests/plugin_load_profile.rs` re-executed itself as its scan worker.**
  An integration binary has no `cfg(test)`, so it took the production
  branch; the child ran its own tests, returned in 2.9 ms with no protocol
  lines, and `scan_all()` lost its entire CLAP half in silence — 326
  plugins where 363 were installed. On a box where the profiled plugins
  are CLAP-only that is `PERF-VERDICT: SKIP`, i.e. exit 125, so PR #120's
  gate and every bisect on it would call each commit unjudgeable.
  `scan_worker::set_worker_command` is the runtime override.
- **`zynaddsubfx-ext-gui` outlived a crashed AURA**, leaving a window the
  user could not close. `PR_SET_PDEATHSIG` in `pre_exec` fixes it, and the
  frozen `Cargo.toml` was never the blocker the backlog recorded: `prctl`
  is reachable with a bare `extern "C"` block, no `libc` crate.

**Checked in the running app, so nobody owes this one an ear-check.** Two
sessions on 2026-08-28: three real `zynaddsubfx-ext-gui` GUIs spawned and
closed (so `die_with_parent`'s `pre_exec` does not break the spawn), and
the changed X path ran `pin` on a real plugin window three times and
`unpin` five times, with no `X Error` and no exit. What that does NOT show
is the fix working — the stale-id race cannot be hit by hand. It shows the
absence of a regression; the fix itself is proved by the deterministic
test, which sidesteps the race with an id that cannot exist.

Evidence in [`backlog/ci-hardening.md`](backlog/ci-hardening.md) item 5;
the two general lessons in [`TRAPS.md`](TRAPS.md) §Backend.

| PR |
|---|
| PR #124 |

## The engine performance gate

`scripts/perf-check.sh` — the measurement from §9 turned into something
that answers yes or no. Exit **0** under budget, **1** over, **125**
unjudgeable (did not build, harness absent at that commit, plugins
missing), which is the protocol `git bisect run` needs. Bisect SKIPs on
125 instead of blaming a commit that merely failed to compile.

Defaults to the `bare` column, which needs **no plugins installed** and
runs anywhere; `--run full` covers the plugin path. `--harness-from
<ref>` injects the harness at commits that predate it, so a bisect can
cross the commit that introduced it — verified end to end across
b7fb583, which has no harness of its own.

The budget is passed in rather than committed: an absolute µs baseline in
a file is a property of one CPU. Best-of-N, not median — benchmark noise
is one-sided. A reference-workload ratio was built and then removed: the
thermal drift it was meant to cancel did not survive a cold-vs-hot
measurement, and the ratio was noisier than the absolute it replaced.

Rules in [`STANDING-CONSTRAINTS.md`](STANDING-CONSTRAINTS.md)
§Performance and [`../CLAUDE.md`](../CLAUDE.md); caveats in
[`GAP_ANALYSIS.md`](GAP_ANALYSIS.md) §9.6.

| PR |
|---|
| PR #120 |

## Where a block's time actually goes, under real plugin load

`GAP_ANALYSIS.md` §8.4 asked for one measurement before any further
engine performance work. It exists now:
[`GAP_ANALYSIS.md`](GAP_ANALYSIS.md) §9, reproduced by
`src-tauri/tests/plugin_load_profile.rs`.

The premise held in direction and not in emphasis. At 32 tracks, 8 hosted
instruments and 66 insert slots: plugin DSP **28%** of the block, AURA's
own code **52%** — of which the per-insert host path is **25%** on its
own, **~3 µs per insert slot**. The whole session uses **8.6% of the
10.67 ms deadline**, so none of it is audible today (§9.3); the point is
that the one large addressable line is ours, not the plugins'.

Method: no instrumentation on the RT path. The same session renders four
times with different plugins attached and the costs come out by
subtraction — including a `cheap_fx` pass (same chain lengths, one
multiply per sample) that separates host overhead from DSP without a
profiler, which this machine cannot run (`kernel.perf_event_paranoid=4`).
Figures are the median of three runs: §9.2 shows why the plugin-DSP
column must never be quoted from one.

| PR |
|---|
| PR #119 |

## Plan V — players (a pad that is an instrument)

Two cuts landed, six staged behind them: [`backlog/plan-v-players.md`](backlog/plan-v-players.md).

| What | Where |
|---|---|
| **V2 — one player, real.** `session.players[]`, ops, a graph slot, an audio-clip source (`raw` bit-exact at unity, ruling V-16) and a MIDI-clip source into a player-owned plugin instance no track owns; per-node clocks (V-13/V-14) replace the launch overlay entirely, and V-15 keeps players out of the offline bounce by construction. `player_fire`/`player_stop` fire a pad while the arrangement keeps running, at unity, with the source track's chain bypassed — the design §2.2 defect this cut exists to kill. Every launch binding a project had migrates to a player naming the same instrument, with the minted id derived from the clip so an unsaved reopen lands on the same player. Two lessons outlived it: six defects reached the owner or a reviewer on the last day and every one was a **reachability** defect in the surface UI, not the engine (see `STANDING-CONSTRAINTS.md`'s testing rule this earned); and `--test-threads=1` is still required, for a reason unrelated to the crash it was introduced for (`TRAPS.md`). | PR #121. Backlog: [`backlog/plan-v-players.md`](backlog/plan-v-players.md). Design: [`superpowers/specs/2026-08-26-plan-v-players-design.md`](superpowers/specs/2026-08-26-plan-v-players-design.md) §10. |
| **V1 — `MixNode` as the graph compiler's input.** `compile_inserts` and `compile_routing` stop reading `TrackState` and take `&[MixNode]` instead; tracks and buses become two producers of one node type (`audio/node.rs`, `From<&TrackState>` + `mix_nodes()`). Behaviour-neutral by construction: two characterization gates were written and hashed BEFORE the refactor and are still green, unedited, after it — `audio::offline::tests::bounce_of_a_full_strip_is_byte_stable` (a rendered bounce exercising clip + bus + send + output + gain/pan, FNV-1a-64 over the samples) and `audio::bus::tests::routing_plan_of_a_full_strip_is_stable` (the exact routing plan — `track_pdc`, `out_delay`, `output`, `bus_ids`, send delays — with a bypassed insert live so declared PDC ≠ applied PDC). `bus::would_cycle` deliberately keeps `&[TrackState]`: it is a control-plane guard answering "would this edge close a loop" about document rows before the edge is written, not a graph-compiler input; see its doc comment. No new IPC, no document change, no schema bump. | PR #118. Backlog: [`backlog/plan-v-players.md`](backlog/plan-v-players.md) |

## Plugin manager and native floating GUI

Two PRs, one track. Details, scope calls and what is left:
[`backlog/plugin-manager.md`](backlog/plugin-manager.md).

| What | Where |
|---|---|
| **LV2 port properties — stepped, expensive, non-automatable** | PR #114. ZamVerb declares "Room" as `lv2:integer` 0..6 **plus** `pprops:expensive` and `kx:NonAutomatable` (the value picks the convolution impulse response); AURA reported it as a continuous 0..6 param, drew a smooth knob, and a drag streamed ~60 fractional values a second at a port that reloads an IR per write — the crackling the owner heard. `livi` 0.7 exposes no port properties, so `lv2_host::control_port_params` now reads them from raw lilv: `lv2:toggled`/`lv2:enumeration`/`lv2:integer` → `ParamInfo.steps`, `pprops:expensive`/`kx:NonAutomatable` → the new additive `ParamInfo.non_automatable`. `modulation.pickTarget` refuses to mint a lane on a flagged param (an existing one still opens); the panel's `A` button goes dashed with the reason. CLAP reports `false` — see the backlog for why `!IS_AUTOMATABLE` was NOT wired. **Owner ear-check owed.** | PR #114. Detail: [`backlog/plugin-manager.md`](backlog/plugin-manager.md) |
| **Step 7 — unified browser audition** | PR #106. Double-click any browser row to hear it (Shift+Enter for keyboard parity), behind a new `browserAudition` pref defaulting off. `utils/audition-target.ts` resolves a row to an `AuditionTarget`; `state/audition.svelte.ts` plays it. No new IPC, no ops. Detail and rulings R-1…R-4: [`backlog/plugin-manager.md`](backlog/plugin-manager.md). |
| **Step 6 — automation as an inventory** | PR #98, amended by PR #105. Param cache + shared lane reveal (`plugins.svelte.ts` `paramCache` over the frozen `plugin_get_params`; `utils/lane-reveal.ts`'s `revealParamLane`), `AutomationMatrix.svelte` + `utils/automation-matrix.ts` as a 4th `ManagerMode`, pinned params in `PluginParamPanel.svelte`, and `LanePluginStrip.svelte` + `utils/lane-strip.ts` in `TrackHeader.svelte`'s `.metadata-row`. #98 was squashed four minutes before its own fix wave was pushed, so its final review's merge blocker landed separately in #105: `revealParamLane` now refuses an empty `trackId` instead of minting a curve and a binding no lane can show. |
| **Step 5 — plugin manager + native floating GUI** — catalog, browse/split/rack, Ctrl+P frecency, CLAP/LV2/Zyn editors, live on-top pref | PR #93 `e1ec61f`. Winner spec: [`2026-08-20-plugin-admin-winner-design.md`](superpowers/specs/2026-08-20-plugin-admin-winner-design.md). Plan: [`2026-08-20-plugin-manager.md`](superpowers/plans/2026-08-20-plugin-manager.md). |
| Timeline follow-on-seek | `view.revealSamples` (in PR #93) |
| Shared `browser/` layer | instruments / samples / presets migrated (in PR #93) |
| Lane multi-select + bulk M/S/A | `lane-bulk.ts`, ARIA grid (in PR #93) |

## Mixer graph — insert FX, sends, returns (Plan G)

Product cut, what shipped and what did not:
[`backlog/insert-fx-sends-sidechain.md`](backlog/insert-fx-sends-sidechain.md).

| What | Where |
|---|---|
| **G2 — bus tracks, sends AND output routing.** Two primitives, kept apart on purpose: a send is a COPY (shared reverb, everyone still heard dry), an output is a MOVE (a submix — the track stops reaching the master). Buses route and send too, so a drum bus into a mix bus works; the compiler is a real DAG with a topological order and cycle rejection (`bus::would_cycle`), and PDC is per EDGE so a dry path can wait for a slow return while its own send into that return leaves immediately. `kind: "bus"` returns, `TrackState.sends` + `TrackState.output` edges, `audio::bus::compile_routing` shared by the engine and the bounce, two compensating delays (source alignment before the taps, dry-path alignment after them), a windowed render so the bus pass can run after every tap, the balance pan law on returns, send amount as a `ParamTable` lane (no rebuild per knob frame), a SENDS rack on the track header and `+ BUS`. Also closes G1 Task 8's offline half: the bounce now walks insert chains. **Owner ear-check owed** — one convolution reverb, several sources, one room, then export. | PR #109 |
| G1 Task 7 — inserts wired into `engine::rebuild` (effects audible on audio and MIDI tracks) | PR #90 |

## Everything else

| What | Pointer |
|---|---|
| **The parallel `--lib` SIGSEGV, diagnosed and fixed.** `WorkerCommand::current_exe()` re-executes the current binary and relies on the `AURA_SCAN_WORKER` guard in `main`; a libtest binary has none, so two lib tests' plugin scans ran **the whole suite again** as their own subprocess worker — invisibly, because the worker env is inherited: the recursive suite's own hidden worker case emitted correct descriptors, so the parent got the right answer and nothing failed or logged. Each cost 6–30 s blocked on that recursive suite (every line it prints resets `LINE_TIMEOUT`, so the timeout never fires and there is no respawn) and, via its audio streams, up to +414 file descriptors on the PipeWire **daemon**, which systemd caps at a soft `RLIMIT_NOFILE` of 1024. Past that the daemon could not finish a client handshake, wireplumber died with SIGSEGV, and the test process segfaulted inside `libpipewire` — **not** the LV2 thread-safety the backlog had suspected for three weeks; the crash reproduces with every `plugins::` test skipped and no AURA frame appears in the faulting stack. Fixed by routing `current_exe()` through the hidden worker entry under `cfg(test)`, which closes the class rather than the two call sites. **SIGSEGV in 3 of 4 runs on the parent commit, 0 of 12 here**, alternated on one machine in one sitting; daemon fd peak 1024 → 412–578; the two tests 6–30 s → 0.33 s. `--test-threads=1` is green (1409 passed) and is still the gate: parallel runs still fail 1–18 tests depending on how loaded the audio server is (item 4's engine starvation, same names before and after), and fixing the SIGSEGV made the NEXT failure reachable — an `X Error … BadWindow` abort in about half of parallel runs, where Xlib's default handler calls `exit(1)` on a restack of an already-closed plugin window. Zero of four on `main` against two of four here, because `main` segfaulted before reaching it. | PR #123. Backlog: [`backlog/ci-hardening.md`](backlog/ci-hardening.md) item 5; traps: [`TRAPS.md`](TRAPS.md) |
| **Control surface v0.2.1 — racks.** A device stops being a page *mode* and becomes an object on the page: `+ → RACK` appends a faceplate as a widget group (`rack:<id>`), so two LPD8s sit side by side, either removes on its own, and `+` never destroys what is already on the deck — the reported defect. `DEVICE_RACKS` is data (knobs and their column span, faders, what a knob drives, pad block, mode silkscreen), so Launchpad 8×8, MCU 8-strip and nanoKONTROL render from `Rack.svelte` with no per-device code; this is the shape Plan V's V8 hardware map binds to. The rows sit one level down behind `Add rack ›` (Escape peels the level before it closes the menu) — a drill-down rather than a flyout, because the popover is already height-capped against the bottom edge of the window. `Clear page` becomes an explicit action, `template:blank`/`template:mixer` are gone, and layout v2 migrates a saved v1 `lpd8` deck into a rack group. Two neighbours fell out of driving the real panel: `addStrip` no longer refuses a strip just because a rack knob drives that gain, and a `Fader` label can no longer overflow onto its neighbour. | PR #116. Backlog: [`backlog/control-surface.md`](backlog/control-surface.md) |
| **Control surface v0.1** — a performable bottom panel (SURFACE, beside ROLL / PITCH): channel strips, analog VU gauges, faders/knobs, mute-solo-arm lamps, a named clip list, N×M pad grids whose LEDs breathe with the meter bus, and homage templates (AKAI LPD8, Mixer). One-click recipes fill it from the open project; a bind picker points any inserted widget at a track gain/pan/mute, a MIDI clip or a plugin param, and a pad-grid cell at a clip. Host chrome, not a plugin: the layout lives in localStorage per project dir and every widget emits an EXISTING mix/launch/plugin command, on the same gesture tail as `TrackHeader` so one drag is one undo entry. | PR #113. Backlog: [`backlog/control-surface.md`](backlog/control-surface.md); design: [`2026-08-26-control-surface-design.md`](superpowers/specs/2026-08-26-control-surface-design.md); research: [`research/12-control-surfaces.md`](research/12-control-surfaces.md) |
| **RT engine primitives + a Cranelift-fused fader kernel** (`aura-engine/`, a STANDALONE crate — nothing in `src-tauri/` changed, the frozen manifest stays frozen). `sync::TripleBuffer<T>` (wait-free latest-value publish, allocates only in `new`, recycles instead of freeing — which is what lets `JITModule::free_memory`'s unsafe precondition be discharged); `metrics::AllocCounter`, a counting global allocator that turns dossier 10's gap 19 into a test; `strip::plan`, which cuts a block into straight-line stretches; and `jit`, a Cranelift kernel fusing gain·ramp·pan·mute plus the meter fold, two frames per iteration. Bit-identical to the scalar plan, and to `mixer::apply_fader_into` in the flat case — within 1e-5 once a ramp moves, so **no bounce byte-comparison can validate it**. New CI job `engine`. **The audit is the other half of the deliverable**: three of the brief's six items already existed, and the Carla bridge is declined. **NOT wired into the app, and §8 of the audit recommends the JIT never is** — cranelift 0.134 has no `MAP_JIT` path, so macOS notarization would need the broad `allow-unsigned-executable-memory` entitlement, and the JIT is worth ~0.2% of one core over the plain-Rust plan. Treat the crate as a proving ground, not a shipped component. The track is not ear-checkable: no code path reaches it. | PR #112. Audit: [`GAP_ANALYSIS.md`](GAP_ANALYSIS.md). Track: [`backlog/jit-engine.md`](backlog/jit-engine.md). Brief: `jit.md` |
| **The param panel follows automation** (Track D ruling 2's recorded consequence). Plugin-param automation is driven host-only, so the panel painted the document value while the plugin moved. The engine now publishes its driver's own writes on the meter frame (`MeterFrame.drivenParams`, an upsert set — `tick` suppresses unchanged values, so deltas alone would blank a held param 60×/s) and the row paints them: chip, toggle, enum and fader, a magenta fill, and an AUTO flag. Cleared on transport stop and on rebuild. Display only; ruling 2 is unchanged. | PR #108. Backlog: [`backlog/history-and-automation.md`](backlog/history-and-automation.md); handoff: [`PHASE4-PLAN.md`](PHASE4-PLAN.md) "Track D handoff" |
| **`Undo to here`** — guarded linear walk back to a retained revision: `ControlPlane::undo_to` validates the target against the live undo path and the caller's observed `(epoch, head rev)`, then repeats the ordinary undo step under one `history_gate` hold; additive `history_undo_to` command, `onUndoPath`/`epoch`/`headRev` on the overview, and the action in the HISTORY dock | PR #107. Contract: `PHASE4-PLAN.md` "Plan F handoff" carry-forward (e). Plan: [`2026-08-23-undo-to-here.md`](superpowers/plans/2026-08-23-undo-to-here.md) |
| **Extract melody to MIDI** — segmenting audio clip pitch frames to editable MIDI clip, auto-creating/targeting MIDI tracks with undo, and selecting as Pitch Coach reference track | PR #91. Product doc: [`pitch-track.md`](pitch-track.md) |
| **Pitch analysis action on clip view** — analyse clip button on selected audio clips, persisted APTF cache rebuild, teardown/generation guards, and PitchCoach report cache invalidation | PR #87 `25af6ae`. |
| **Write / Touch / Latch automation modes** — Off / Read / Write / Touch / Latch per track, real-time control-thread point recorder, single-op commit on stop/release with undo | PR #85 `d496903`. Design spec: [`2026-08-18-automation-write-touch-latch-design.md`](superpowers/specs/2026-08-18-automation-write-touch-latch-design.md). Plan: [`2026-08-18-automation-write-touch-latch.md`](superpowers/plans/2026-08-18-automation-write-touch-latch.md) |
| **MIDI output — per-track and per-clip patchbay routing** | PR #84 `cbbc240`. Handoff: [`midi-output.md`](midi-output.md) |
| **Undo / Version graph read-only browser** | PR #82 `b741251`. |
| **DOM test environment (jsdom)** — mounted component tests for `AutomationTrackRow` | PR #80 `ca67b7d`. |
| **CI: apt no longer hangs the Rust job, and the Rust cache now exists.** The job was ~27 min of which the 1231 tests were 20 s; `apt-get update` had stalled 23.5 min and been cancelled. apt is now 3 min (`--no-install-recommends` + bounded retries), and `push: branches: [main]` finally lets `Swatinem/rust-cache` save a cache a PR can restore — without it every PR compiled ~600 crates from scratch. | PR #78 `2b00e91` |
| **MIDI output — eight fixes** (re-cue storm, per-note channel, routed track no longer doubles its internal synth, undo-after-delete silence, forced-channel persistence, piano-roll note channel, ALSA re-address, live port list). An owner ear-check on a real drum machine is still owed. | PR #77 `55257e8`. Handoff: [`midi-out-note-channel.md`](handoff/midi-out-note-channel.md) |
| G1 Task 6 — PDC (`DelayLine` + `compile_pdc`) | PR #71 `9e0b884`. Handoff: [`g1-insert-fx.md`](handoff/g1-insert-fx.md). Known gap for Task 7: [`backlog/insert-fx-pdc.md`](backlog/insert-fx-pdc.md) |
| Clip Delete/Backspace after a plain pointer-click | PR #70 `7011e81`. Pointer-select never focused the clip element, so its own keydown handler never fired; a window-level fallback now reads `clipSelection` directly. Single-clip only — batch-deleting a multi-selection is still open. |
| G1 Task 5 — mixer strip: source-sum → inserts REPLACE → shared fader (`InsertNode`/`compile_inserts`) | PR #66 `c99293b`. Handoff: [`g1-insert-fx.md`](handoff/g1-insert-fx.md) |
| **The Composer, Plan H1** — a pure music-theory library, the harmony document in the core, five generators (progression, voice-led chords, bass, melody, groove), the COMPOSER panel, and a piano roll that tints its keys by what each note does over the chord. Owner ear-checked 2026-08-18: works. **Composer is deprioritized — do not start H2+ unless asked.** | PR [#65](https://github.com/knobo/aura-daw/pull/65) `63cb7fa`. Product doc: [`composer-assistant.md`](backlog/composer-assistant.md). Plan + rulings H-1…H-12: [`2026-08-17-plan-h-composer.md`](superpowers/plans/2026-08-17-plan-h-composer.md). Handoff: [`composer-h1.md`](handoff/composer-h1.md). ARCHITECTURE §16 |
| Theme system — token contract, eight built-in themes, user themes from JSON | PR #63 `46df20d`. User docs: [`themes.md`](themes.md). |
| Pitch Coach **phase 3** — per-note scoring, stored pitch curve, take report | PR #61 `c14916d`. [`pitch-coach-PROGRESS.md`](superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md) |
| Lanes UX — rename, fold (lane + group), group, drag-reorder, and the timeline scroll/alignment fix | PR #60 `5f891cb`. Handoff: [`lanes-ux.md`](handoff/lanes-ux.md) |
| Pitch Coach **phase 2** — panel, frame bus, lane geometry, prefs | PR #58 `7c3cb87`. Adds `pitch_unsubscribe` (additive). |
| G1 Tasks 2–4 — insert ops, commands, `HostRole::Effect`, HostForward restore | PR #55 `118ae23`. Handoff: [`g1-insert-fx.md`](handoff/g1-insert-fx.md) |
| G1 Task 1 — `InsertSlot` on `TrackState.inserts` | PR #52 `5c338ff` |
| Pitch Coach phase 1 | PR #49 `84b0313` + PR #54 `f451a5a` (detection off the RT callback, listen mid-take, `pitch_check`). [`pitch-coach-PROGRESS.md`](superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md) |
| MIDI launch v0.1 | PR #42 + #50. Hardware GATE / sustain still open: [`backlog/midi-launch.md`](backlog/midi-launch.md) |
| External audio editor | PR #48 |
| Gesture tokens | PR #47 |
| Tracks A–F / Plan F | log: [`handoff/landed-tracks.md`](handoff/landed-tracks.md). Rulings: [`PHASE4-PLAN.md`](PHASE4-PLAN.md) |
