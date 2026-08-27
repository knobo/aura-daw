# Backlog: control surface (virtual mixer / pad deck)

**Opened 2026-08-26.** Branch `feat/control-surface`, worktree
`.worktrees/control-surface`, draft PR #113.

Research: [`docs/research/12-control-surfaces.md`](../research/12-control-surfaces.md).
Design: [`docs/superpowers/specs/2026-08-26-control-surface-design.md`](../superpowers/specs/2026-08-26-control-surface-design.md).
Handoff (pickup notes): [`docs/handoff/control-surface.md`](../handoff/control-surface.md).

## v0.2.1 — racks (landed, PR #116)

Owner report, 2026-08-26: *"når jeg klikker på + knappen, så blir innholdet
replacet istedet for added"*, and: templates should live in their own list
of MIDI gear, meant to grow.

Both are one defect. A template is currently a **page mode**
(`page.templateId === "lpd8"` drives a special case in
`SurfacePanel.svelte`), so `applyTemplate` has nowhere to put a device
except the whole page — it *must* replace, and two racks are impossible.

The fix is the model, not a patch:

- A rack is a widget **group** (`groupId: "rack:<id>"`) carrying its device
  id, appended by `addRack(layout, device, ctx)`. `groupWidgets` grows a
  `racks` bucket beside `strips`.
- `Rack.svelte` owns the per-device geometry. The LPD8's landscape layout —
  a column of mode buttons, 8 knobs in 2×4, the 4×2 pad block on the right
  — moves there out of the panel's special case. (That geometry was fixed
  in PR #113; before it, eight knobs sat in one row above the pads, which
  is a different instrument.)
- The device list is **data** (`DEVICE_RACKS`), so Launchpad 8×8, MCU
  8-strip and nanoKONTROL are rows, not code. This is the shape Plan V's V8
  hardware map binds to.
- `Clear page` is an explicit menu action. `template:blank` stops being the
  way to empty a deck.
- `parseLayout` migrates a saved v1 deck whose `templateId` is `"lpd8"` by
  wrapping its widgets in a rack group, so nobody's stored deck breaks.

Gate: two LPD8 racks on one page, each removable on its own; a rack beside
channel strips; `+` never destroys existing widgets; a saved v1 lpd8 deck
opens as a rack. All four hold — checked in the unit/DOM suites and in the
running app.

What the shape actually bought, beyond the reported defect: `DEVICE_RACKS`
now carries Launchpad 8×8, MCU 8-strip and nanoKONTROL as rows, and all
three render from `Rack.svelte` without a line of per-device code. Two
neighbouring defects fell out of driving the real panel:

- `addStrip` refused a strip whenever *anything* drove that track's gain,
  so with a rack on the page the `Channel strip` menu item silently did
  nothing. It refuses only a second **strip** now, and `choose("strip")`
  picks the first track without one rather than always the first track.
Owner review on the branch: the device rows belong **one level down**. Four
devices in a list meant to grow made the `+` menu seventeen rows against the
bottom edge of the window, so `Add rack ›` drills into them and `‹ RACK`
(or Escape) comes back. A drill-down rather than a side-opening flyout: the
popover is already height-capped against that edge, and a second one hanging
off its side would fight for the same room.

- A `Fader` label wider than its 36px unit overflowed onto its neighbour.
  Invisible while every caller passed `"LEVEL"`; unmissable with eight
  track names over an MCU. Clamped in `Fader.svelte` — `min-width: 0` is
  the load-bearing half, since a flex item's automatic minimum size beats
  `max-width` on its own — and a rack widens the unit via `--fader-w`.

## Where this track goes next

The deck itself is done. Everything the owner asked for after v0.1 — a pad
holding a raw WAV or its own instrument, knobs that belong to no track,
recording what you play — needs a second time base in the engine, not more
panel code. That is [**Plan V**](plan-v-players.md); this track's remaining
cuts live there as V7 and V8. **Audio clips on pads is V2, not a quick fix
here**: `LaunchTarget::Clip` is MIDI-only in Rust, and the region path that
would work today cannot light a pad or carry the pad's own gain.

## Why this exists

A performable, high-wow panel for mixing and launching: knobs, faders,
analog gauges, mute/solo lamps, a named clip list, and an N×M pad grid
whose pads breathe with the track's waveform. One-click recipes fill
the panel from the open project; anything unwanted is removable. The
same layout algebra is the foundation for a later "give me an Akai
LPD8" command that stamps a preconfigured faceplate (and, after the
hardware cut, talks to the physical unit).

This is **not** a plugin. Plugins are widget *targets*. The surface is
host chrome.

## Status

| Cut | What | State |
|---|---|---|
| **v0.1** | Layout model, add/remove recipes, LPD8 + mixer templates, 3D widgets (knob reuse, new gauge / pad / fader / lamp), bottom-panel SURFACE, clip fire via existing launch preview, mute/solo/arm/gain/pan, pad RMS blink, session persist, bind picker (per-widget target, per-cell clip) | **landed, PR #113** |
| **v0.2 (partial)** | Additive `launch_stop`: Escape stops every sound (arrangement, overlay, audition) and a toggle pad's second press cuts its own clip. Still open on this cut: overlapping voices (retrigger still cuts the single overlay) | **landed, PR #113** |
| **v0.2.1 — racks** | A template became an **object on the page**, not a page mode: `+ → RACK` appends a device faceplate (removable as a unit, two allowed side by side, mixable with strips), the device list is data so a new device is a row rather than code, and `Clear page` is an explicit action instead of an implicit wipe. Layout v2 with a v1 migration. | **landed, PR #116** |
| v0.3–v0.5 | **Moved to [Plan V](plan-v-players.md)** — project-owned layout is V7, the hardware map and further templates are V8. They need players underneath them, so they cannot be cut from this track any more. | see Plan V |
| later | Selected-track follow, send/plugin encoder modes, scene-from-arrangement, aftertouch | open |

## v0.1 product cut

- Bottom panel tab **SURFACE**, next to ROLL / PITCH. Mixers need
  width; the right dock is the wrong shape.
- `+` menu: Knob, Fader, Gauge, Pad, Mute, Solo, Arm, Clip list,
  Pad grid, Channel strip; recipes **Add all**, **Add all tracks**,
  **Add all clips**, **Add all automations**; templates **LPD8**,
  **Mixer**.
- Add-all is additive and skips targets already on the page.
- Each widget has a remove control in edit mode.
- Pads bound to a MIDI clip fire `launch.preview` (shadow playhead,
  arrangement loop untouched). If no binding exists, `launch.mapClip`
  creates one first. A pad in **toggle** mode cuts its own clip on the
  second press (v0.2's `launch_stop`); a **momentary** pad retriggers.
- Pad LED: RMS of the bound track from the meter bus (rAF, not
  reactive). A pad is lit when its clip is actually on the overlay — not
  from a local latch, which could not know the clip had ended.
- Clip list: named rows, one-press play, optional drive-clip chip
  (`launch.setClipLauncher`).
- Gauges and faders use the same material tokens as `Knob.svelte`.
  No colour literals.
- Layout lives in the frontend store. Session persist is
  `localStorage` keyed by `project.projectDir`. Not an op, not in
  `project.json`. Thin renderer (ADR 0006).

## Out of v0.1 on purpose

- Hardware MIDI in/out for the virtual widgets. Launch hardware
  already exists; this panel does not steal it.
- `launch_stop` / GATE-off from a pad. `stop_drive_launch` exists in
  Rust (`midi/launch.rs`) and is **not** a Tauri command. Retrigger
  is v0.1 behaviour (same as launch overlay today).
- Plugin-instance param writes that bypass the open param panel's
  rAF batch. Track-param automations (gain/pan) are in Add-all
  automations; plugin params are offered when `paramCache` already
  has them, written through a surface-local rAF queue to
  `plugin_set_param`.
- An Akai (or any vendor) logo. Homage layout + AURA wordmark.

## Rulings (v0.1)

- **S-1** The surface is chrome. It emits existing commands and
  reads pushed state. No new authoritative document field in this
  cut.
- **S-2** Bottom panel, not dock, not a floating LaunchMap clone.
- **S-3** Meter-driven visuals (gauge needle, pad blink) are
  imperative rAF over `latestMeter`, same contract as `Meter.svelte`.
- **S-4** Gesture-wrapped mix writes. A knob/fader drag is one undo
  entry (`project.beginGesture` / `endGesture`), same as TrackHeader.
- **S-5** Frozen commands stay frozen. v0.2's `launch_stop` is
  additive.
- **S-6** Theme tokens only in every `<style>` block.

## Files (v0.1)

| Path | Role |
|---|---|
| `src/lib/utils/control-surface.ts` | Pure layout algebra |
| `src/lib/utils/control-surface.test.ts` | Recipes, templates, remove, round-trip |
| `src/lib/state/surface.svelte.ts` | Layout store + command emission |
| `src/lib/components/controls/Gauge.svelte` | Analog VU |
| `src/lib/components/controls/Pad.svelte` | Rubber pad + LED |
| `src/lib/components/controls/Fader.svelte` | Vertical fader |
| `src/lib/components/controls/Lamp.svelte` | Mute/solo/arm |
| `src/lib/components/surface/SurfacePanel.svelte` | Bottom panel |
| `src/lib/components/surface/AddMenu.svelte` | `+` dropdown |
| `src/lib/components/surface/ChannelStrip.svelte` | Grouped strip |
| `src/lib/components/surface/ClipList.svelte` | Named clip launch |
| `src/lib/components/surface/PadGrid.svelte` | N×M pads |
| `src/lib/components/surface/BottomPanelTabs.svelte` | ROLL / PITCH / SURFACE |
| `src/lib/components/surface/BindPicker.svelte` | Bind picker (widget target, pad-grid cell) |
| `src/lib/utils/popover.ts` + `popover.svelte.ts` | Viewport-aware popover placement |
| `src/lib/utils/portal.ts` | Escape `.glass`/`overflow` for popovers |
| `src/lib/components/surface/surface-panel.dom.test.ts` | Add/remove, recipes, mute/fire, bind |
| `src/lib/state/surface-gesture.test.ts` | One drag = one undo entry (I-8) |
| `src/lib/state/surface-persist.test.ts` | Deck keyed on the project it was edited in |

## Review fixes on top of the first implementation

An external review (Grok, 2026-08-26) plus a headless pass over the real
panel found these; all are in PR #113:

- **Gesture contract.** `writeGain`/`writePan` fired immediately while
  `openGesture` only queued `beginGesture`, so a pointermove could land a
  mix op outside the boundary and the trailing write after `gesture_end`
  — one drag, several undo entries. Writes now ride the same
  `gestureTail` as TrackHeader's, and `closeGesture` cancels the param
  rAF, flushes it on the tail, then ends (I-8's ordering, which the
  surface's private param queue had dropped).
- **`Gauge` drove `$state` 60×/s** through its dB readout, one per
  mixable track. It writes `textContent` inside the rAF loop now; only
  the clip latch stays reactive (ruling S-3, same as `Meter.svelte`).
- **Deck persistence** was keyed on the LIVE `project.projectDir`, so a
  project switch inside the 80 ms debounce lost the last edit and could
  write the old deck into the new project's key. Persist is keyed on
  `hydratedKey` and `hydrate` flushes first.
- **`Pad` fired from `pointerup` only** — keyboard users could not play
  it. It fires on `click` (which Enter/Space also produce); the pointer
  pair only drives the pressed visual.
- **Bind picker** (see above): `bind()`/`setGridCell` had no UI, so the
  `+` menu inserted widgets that could only be deleted.
- **Popovers were unreachable at the bottom of the window.** The `+`
  menu opened downward from a bottom-docked panel and its templates fell
  off-screen; `position: fixed` did not fix it because `.glass` is a
  containing block. See `docs/TRAPS.md`.

## Stopping sound (v0.2's first slice)

A previewed clip is rendered EXCLUSIVELY while the transport is stopped, so
`transport.stop()` does not silence it — before `launch_stop` there was no
way to cut a pad-fired clip short of waiting for its end. Now:

- `SharedRt::end_launch` ends the overlay the way reaching the clip's end
  ends it, so the engine gets its one `ended` frame and FLAG_LAUNCH tracks
  all-notes-off instead of losing the flag under a held note. The drive
  thread's existing release path then clears the flags and emits
  `launch://fired {playing:false}` — one code path for both endings.
- `state/stop-all.ts` is the composed panic: pause the transport (never
  seek — Escape must not cost you your place), cut the overlay, release the
  audition stream. Every leg no-ops when its own source is silent.
- Pad lamps now read real state instead of a local latch — the overlay for
  clip and scene pads, the track's own `muted` for a mute pad. A clip that
  ended by itself therefore leaves its pad ready to fire again, and Escape
  cannot leave a pad lit over silence.
- Escape is wired in `App.svelte`. The piano roll keeps its own Escape
  (peel selection → region → close editor) while it has focus.

## Owner ear-check owed

**v0.2.1:** open SURFACE, `+ → Add rack › AKAI LPD8`, then add a second one and a
channel strip — three objects side by side, each with its own `×`. Add an
MCU 8-strip and confirm the faders read as a scribble strip rather than a
pile of overlapping names. `Clear page` should be the only thing that
empties the deck.

**v0.1/v0.2:** Open SURFACE, Add all, drag a gain knob, mute a strip, tap a pad on a
MIDI clip, confirm the clip plays on the shadow playhead and the pad
breathes with the meter. Switch to Console Noir and confirm the
faceplate still reads as milled metal, not a flat overlay. Then tap a pad
and hit Escape mid-clip: the sound must stop cleanly, with no note left
hanging on the instrument or on external MIDI gear.
