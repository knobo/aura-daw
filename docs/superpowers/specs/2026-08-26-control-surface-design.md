# Control surface — design

Date: 2026-08-26
Track: [`docs/backlog/control-surface.md`](../../backlog/control-surface.md)
Research: [`docs/research/12-control-surfaces.md`](../../research/12-control-surfaces.md)

## 1. Goal

A configurable virtual control surface in AURA that is more delightful
to look at and faster to populate than a MIDI-learn overlay or a
generic plugin rack, and that is the data model a later hardware
template ("give me an LPD8") stamps onto.

Success for v0.1: a user with a project of tracks, MIDI clips and a
couple of automation lanes hits SURFACE → Add all, gets a mixer plus
a pad deck, deletes the strips they do not want, and performs mute /
level / clip launch from that panel. The pads pulse with the sound.

## 2. Architecture

```
                 ┌─────────────────────────────────────┐
  project/midi/  │  Surface store (chrome)             │
  launch/meters  │    layout: SurfaceLayout            │
  plugins cache  │    pages, widgets, bindings         │
                 └──────────────┬──────────────────────┘
                                │ emit existing commands
            ┌───────────────────┼───────────────────┐
            ▼                   ▼                   ▼
     set_track_*          launch_fire          plugin_set_param
     (gain/pan/mute       (preview/bypass      (rAF-batched)
      /solo/arm)           = shadow playhead)
```

The layout algebra is pure TypeScript in `utils/control-surface.ts`
(no runes, no DOM, no stores) so recipes and templates are unit-tested
in node. The Svelte store holds one `SurfaceLayout`, hydrates it from
`localStorage` on project open, and translates widget gestures into
the commands TrackHeader / LaunchMap / PluginParamPanel already use.

No new op. No rebuild. No `OP_FORMAT_VERSION` bump.

## 3. Widget catalogue

| Kind | Looks like | Target | Gesture |
|---|---|---|---|
| `knob` | existing `Knob.svelte` | gain, pan, plugin param | vertical drag, Shift fine, dbl-click reset |
| `fader` | motorised-looking cap in a milled slot | gain (or any numeric) | vertical drag, same brackets |
| `gauge` | analog VU, needle, clip LED | `meter` (read-only) | none |
| `pad` | rubber pad, LED | clip launch or lamp-like toggle | press; momentary or toggle |
| `lamp` | hardware mute/solo/arm | mute / solo / arm | click |
| `clipList` | named rows | the project's MIDI clips | play; drive-chip |
| `padGrid` | N×M pads | one clip (or binding) per cell | same as pad |

A **channel strip** is not a widget kind. It is a recipe that inserts
a `groupId`'d bundle: lamp mute, lamp solo, lamp arm, gauge, knob
gain, knob pan. Removing the group removes the bundle.

## 4. Recipes and templates

`addRecipe(layout, recipe, ctx)` is pure and additive:

- `tracks` — one strip per non-automation track, skip if that
  track's gain target is already present.
- `clips` — one `clipList` (if absent) plus a pad grid large enough
  for the MIDI clips (2×4 if ≤8, else 4×4, else 8×8), cells filled
  in LPD8 order (bottom-left origin).
- `automations` — a knob per automation-matrix row whose target is a
  `trackParam` (gain/pan) or a `pluginParam` already in `paramCache`.
- `all` — tracks, then clips, then automations.

Templates replace the active page's widgets (after the user has an
empty page, or as a named new page):

- `lpd8` — homage faceplate: 8 knobs, 2×4 pads, PROG/PAD/CC/NOTE
  lamps as chrome, AURA wordmark. Knobs bind to the first 8 track
  gains; pads to the first 8 MIDI clips.
- `mixer` — the tracks recipe as the whole page.
- `blank` — empty page.

## 5. Pad LED contract

Read `latestMeter(trackId)` inside the pad's rAF loop.

```
brightness = toggleOn ? 1
           : 0.12 + 0.88 * clamp(rmsLin / 0.25, 0, 1)
```

`toggleOn` for a clip pad is `launch.overlay?.id === bindingId` (the
clip is sounding on the shadow playhead) **or** the widget's local
latch when `padMode === "toggle"` and the target is a lamp. A
momentary pad never latches; it flashes on pointerdown and follows
RMS otherwise.

No Svelte reactivity on the meter path.

## 6. Persistence

v0.1: `localStorage["aura.surface.v1:" + (projectDir || "session")]`
as JSON of `SurfaceLayout`. Lost across machines; survives reload of
the same project on this box. Stale track/clip ids render as a dead
widget with a "missing" legend and a remove button.

v0.3: `Op::ControlSurfaceSet { layout }` on the session, written only
when non-default, same pattern as harmony. That cut also makes the
layout undoable. Do not mix the two.

## 7. UI placement

`BottomPanel` grows `"surface"`. App.svelte:

```
pitch → PitchCoach
surface → SurfacePanel
else → PianoRoll
```

Shared height `ui.rollHeight`. Tabs on all three panels. Transport
chip SURFACE. No new dock shortcut (the dock is full of letters;
pitch itself has none).

## 8. Wow factor (the one aesthetic risk)

The signature is **a physical faceplate sitting in the bottom of the
DAW**: analog VU needles, rubber pads that breathe, milled fader
caps. Not a settings grid, not a table of sliders, not a generic
dark-neon "DJ deck" (AURA Dark already is the screen; Console Noir
is the object — the surface has to be both, via tokens).

Spend the boldness on the gauge needle and the pad LED. Everything
else is Knob's language: silkscreen legends, grain, bevel, sheen.

## 9. Testing

- Pure: recipes, templates, add/remove, skip-duplicates, LPD8 pad
  order, serialize round-trip, stale-target detection.
- DOM: Add menu opens, Add all tracks inserts strips, remove
  deletes, a mute lamp calls the mute spy, a pad click calls the
  fire spy.
- No new Rust tests in v0.1 (no new commands).

## 10. Follow-ons another agent can take without this conversation

See the backlog cuts v0.2–v0.5 and the handoff. The layout type is
the API; do not rename widget kinds without a version bump of
`SurfaceLayout.version`.
