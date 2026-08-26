# 12 — Control surfaces, pad decks, and virtual mixers

Status: working research for the control-surface track (2026-08-26).
Audience: whoever picks this track up. This is **not** law; the design
spec and backlog are.

## Verdict in one paragraph

Yes, this is a winner feature — if it stays a **host-owned, templated
faceplate** that talks to existing mix/launch commands, and not a plugin
slot pretending to be a mixer. Every DAW people actually perform with has
a control-surface story (Ableton Push/Launchpad, Bitwig controller
scripts + DrivenByMoss, Mackie MCU/HUI, Reaper CSI, TouchOSC). AURA
already has the two things those stories sit on: a material-token
hardware look (Console Noir, Knob, grain/bevel/sheen) and a MIDI launch
map with a shadow playhead. What is missing is the *panel you look at
while you play* — channel strips, pad grids that breathe with the
signal, gauges, and a one-click "make this look like my LPD8".

## What other products actually ship

### Protocols, not plugins

Control surfaces talk to the host through **host-side adapters**, not
through the CLAP/LV2 graph.

| Layer | Who uses it | What it is |
|---|---|---|
| **Mackie Control Universal (MCU)** | Logic, Cubase, Live, Reaper, Studio One, Reason | 8+1 motorised faders, V-Pots, mute/solo/arm/select, bank, modes (track/send/pan/plugin/EQ). The industry default for a mixing desk. |
| **HUI** | Pro Tools (and clones) | Older sibling of MCU. Same idea, different message map. |
| **Ableton MIDI Remote Scripts** | Live 12, Push 1/2/3, Launchpad, APC, Move | Python components/layers/modes. Pads launch clips with RGB feedback; encoders bind to the selected device. |
| **Bitwig controller API + DrivenByMoss** | Push, Launchpad, APC, MCU, Maschine, OSC, gamepads | Views + modes + LED feedback. The community script pack is how Bitwig wins on hardware. |
| **Reaper CSI (Control Surface Integrator)** | Reaper | Widget-to-action mapping files. Closest cousin to "a JSON layout of knobs and pads". |
| **OSC / TouchOSC / Lemur** | Live, Bitwig, anything | Tablet faceplates. The visual wow lives on the tablet; the DAW is a dump of addresses. |
| **MIDI 1:1 learn** | Every DAW | One hardware control → one parameter. Cheap, and it is what the Akai LPD8 is stuck with in Live unless a script exists. |

None of these are audio plugins. A CLAP/LV2 "mixer plugin" would sit
on a track, introduce latency, and could not mute another track or
fire a launch binding without a back-door into the host. **Do not
solve this with the plugin manager.** Expose plugin *parameters* as
widget targets (that path already exists: `plugin_set_param`, the
automation matrix). Host the surface itself.

### Features worth stealing

Ranked by how often they show up on hardware people actually buy, and
how well they fit AURA as it stands.

1. **Clip launch with LED feedback.** Launchpad / APC / Push: empty /
   loaded / playing / recording as pad colour. AURA has launch
   bindings and a shadow playhead (`launch_fire` bypass = preview).
   Pad colour and overlay-lit state are the v1 of this.
2. **Momentary vs toggle pads.** LPD8 programs each pad as note, CC,
   or program-change, and each pad can be toggle. GATE vs ONE-SHOT is
   already on AURA launchers; hardware GATE is still open
   (`midi-launch.md`). The virtual pad must have the same two
   modes so a later LPD8 map is 1:1.
3. **Channel strip: fader + pan + mute + solo + arm + meter.** MCU,
   Launch Control XL, NanoKontrol, every mixing surface. AURA already
   has the commands (`set_track_gain/pan/mute/solo/arm`) and a meter
   bus at ~60 Hz.
4. **Banking / pages.** MCU banks of 8. Push device banks. A surface
   page is the same idea. Cheap, and it is how an LPD8's 4 programs
   should appear later.
5. **Selected-track follow.** Encoders bind to "whatever is selected"
   rather than a hard track id. Bitwig remote controls, Live's
   "selected device". Defer: v1 binds to concrete ids so Add-all is
   deterministic.
6. **Scribble strips.** LCD/OLED names above each fader. We have
   track names; silkscreen labels are the virtual version.
7. **Send / plugin / EQ encoder modes.** MCU V-Pot modes. Defer
   until sends (G2, landed) and plugin params have a page template.
8. **RGB pads + aftertouch / velocity.** LPD8 mk1 pads are velocity
   note pads with a single-colour LED you drive by sending the pad's
   note back. Mk2 is RGB. Virtual pads can be RGB for free (track
   colour). Aftertouch waits on hardware mapping.
9. **Waveform / level on the pad.** Not common on cheap hardware
   (no display). Maschine and Push 2/3 put a level or a miniature
   waveform on the pad or the screen next to it. This is the wow we
   can beat hardware on: the virtual pad *is* the screen. Drive LED
   brightness from the track's RMS in the existing meter bus.
10. **Scene launch (top row of a Launchpad).** AURA launch *regions*
    already cover several tracks. A "scene" widget is a pad bound to
    a region binding. v1 can bind to existing launch bindings;
    auto-scene-from-arrangement is later.
11. **Learn / MIDI map overlay.** Live's MIDI map mode. AURA launch
    already has `launch_learn_arm`. Hardware mapping of the *virtual
    surface* (knob N ↔ CC 21) is the next cut after the panel exists.
12. **Stop clip / stop overlay.** Push's stop-clip. AURA has
    `stop_drive_launch` in Rust, **not** exposed as a Tauri command.
    Toggle-off for a playing pad needs that command (additive).
    Recorded as an open on the track.

### The Akai LPD8 specifically

Owner hardware: Akai Professional LPD8 (laptop pad controller).

- 8 velocity-sensitive pads, 8 270° knobs, 4 program slots.
- Pads: Note / CC / Program Change. Each pad can be **toggle**.
- LED: host sends note-on/off for the pad's note to light it. The
  next physical press overwrites the LED, so a "stay lit" mapping
  has to refresh after every hit.
- No faders, no screen, no MCU. A script/template is the only way
  this device is more than 8 CCs + 8 notes.
- Default Ableton path is MIDI-learn, one-to-one, saved per set —
  the thing Remotify exists to paper over.

v1 of the virtual surface ships an **LPD8-style faceplate**: 8 knobs
over a 2×4 pad grid, silkscreen legends, program lamps as chrome.
It is a homage layout, not an Akai logo (trademark). A later cut
loads that template *and* a MIDI map for the physical unit ("give
me an LPD8") so the hardware and the panel are the same object.

## Plugins: what they can and cannot do

| Approach | Can it mute a track? | Can it look like a 3D deck? | Verdict |
|---|---|---|---|
| CLAP/LV2 mixer plugin | No (wrong graph) | Native GUI, not AURA-themed | Reject as the surface |
| MIDI-learn onto plugin params | The param, not the track | No | Keep as a widget *target* |
| CLAP remote-controls | Host-defined | No | Future, not v1 |
| OSC plugin / TouchOSC bridge | Via host | On the tablet | Later companion |
| Host template + widgets | Yes | Yes, and it is our look | **This track** |

AURA's plugin manager (PR #93, #98, #105, #106) is the inventory of
*instruments and inserts*. The surface consumes that inventory
(pinned params, automation matrix rows) as knobs. It does not host
a plugin *as* the surface.

## What AURA already has that this sits on

- `Knob.svelte` — 270° milled pot, material tokens, gesture
  begin/end. Reuse.
- `Meter.svelte` — peak-hold VU off the non-reactive meter bus.
  Gauges and pad blink must follow the same rAF-imperative rule.
- Track mix commands: gain / pan / mute / solo / arm, already
  gesture-wrapped in `TrackHeader.svelte`.
- MIDI launch v0.1: named maps, clip and region targets, preview
  fire (`launch_fire` + `bypass` = shadow playhead).
- Automation matrix: every moving param in one list
  (`utils/automation-matrix.ts`).
- Theme materials: `bevel`, `relief`, `sheen`, `grain`,
  `--sheen-dome`, `--bevel-raised`. Console Noir is the flagship
  "this is an object" theme; the surface has to look like hardware
  under that theme and like a screen under AURA Dark, without
  per-theme markup (same contract as Knob).

## What this track must not do

- New ops / `OP_FORMAT_VERSION` bump in v1. Layout is chrome.
  Project-owned undoable layout is `Op::ControlSurfaceSet` later,
  same shape as `Op::HarmonySet`.
- Time math or meter smoothing on the Svelte reactivity graph.
  Pad blink and gauges read `latestMeter` inside rAF, like Meter.
- A dock tab. Mixers are wide. Bottom panel, next to ROLL / PITCH.
- Shipping an Akai (or Novation, or Ableton) logo. Homage layout
  plus an AURA wordmark. Brand assets are an owner drop.

## Sources (retrieved 2026-08-26)

- Bitwig controller setup, Launchpad/Push scripts:
  https://www.audeobox.com/learn/bitwig/bitwig-controller-setup/
- DrivenByMoss (Push, Launchpad, MCU, OSC, Maschine):
  https://www.mossgrabers.de/Software/Bitwig/Bitwig.html
  https://deepwiki.com/git-moss/DrivenByMoss
- Sound on Sound, 8-fader control surfaces (MCU Pro, Launch Control
  XL Mk3, NanoKontrol):
  https://www.soundonsound.com/reviews/8-fader-control-surfaces
- iCON MCU/HUI feature matrix:
  https://iconproaudio.com/2025/06/daw-controller-feature-comparison/
- Ableton Live 12 MIDI remote scripts (unofficial dump):
  https://github.com/gluon/AbletonLive12_MIDIRemoteScripts
- Akai LPD8: pads Note/CC/Prog, toggle, LED via note-on/off;
  4 programs. python-lpd8 docs, LPD8 web editor, Akai LPD8 mk2
  product page, inMusic editor guide.
- Remotify on Live's 1:1 MIDI learn vs a real LPD8 script.
