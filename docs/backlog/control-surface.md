# Backlog: control surface (virtual mixer / pad deck)

**Opened 2026-08-26.** Branch `feat/control-surface`, worktree
`.worktrees/control-surface`, draft PR #113.

Research: [`docs/research/12-control-surfaces.md`](../research/12-control-surfaces.md).
Design: [`docs/superpowers/specs/2026-08-26-control-surface-design.md`](../superpowers/specs/2026-08-26-control-surface-design.md).
Handoff (pickup notes): [`docs/handoff/control-surface.md`](../handoff/control-surface.md).

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
| v0.2 | Additive `launch_stop` so a toggle pad can cut the overlay; pad "on" = overlay id | open |
| v0.3 | `Op::ControlSurfaceSet` (HarmonySet-shaped) so the layout is project-owned and undoable | open |
| v0.4 | Hardware MIDI map: physical LPD8 CCs/notes ↔ widgets; LED out | open |
| v0.5 | More templates (Launchpad 8×8, MCU 8-strip, NanoKontrol); spoken "give me an LPD8" | open |
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
  creates one first.
- Pad LED: RMS of the bound track from the meter bus (rAF, not
  reactive). Toggle pads light solid when latched.
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

## Owner ear-check owed

Open SURFACE, Add all, drag a gain knob, mute a strip, tap a pad on a
MIDI clip, confirm the clip plays on the shadow playhead and the pad
breathes with the meter. Switch to Console Noir and confirm the
faceplate still reads as milled metal, not a flat overlay.
