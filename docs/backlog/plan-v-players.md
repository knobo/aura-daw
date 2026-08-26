# Backlog: Plan V — players (a pad that is an instrument)

**Opened 2026-08-26.** No branch yet — V1 is unclaimed.

Design + rulings V-1…V-12: [`docs/superpowers/specs/2026-08-26-plan-v-players-design.md`](../superpowers/specs/2026-08-26-plan-v-players-design.md).
Audit of what the engine already has: [`docs/research/13-players-and-performance.md`](../research/13-players-and-performance.md).
Predecessor tracks: [`control-surface.md`](control-surface.md) (the deck), [`midi-launch.md`](midi-launch.md) (the prototype this retires).

## Why this exists

A pad should be able to hold a WAV played raw, or a MIDI clip with an
instrument of its own, mix and match on one deck, carry knobs that belong to
no track — and let you keep what you played as editable clips and curves.
The engine has one time base (the transport) plus one bolted-on shadow
playhead; this track gives it N independent players, each a mixer node with
its own playhead, and retires the overlay onto them.

The document was designed for this a round earlier: modulation design §8
reserved ports (§8.1), macros (§8.3), automation recording into automation
tracks (§8.5) and per-voice modulation (§8.8). Plan V builds those arms.

## Status

| Cut | What | State |
|---|---|---|
| **V1** | `MixNode` as the compiler's input; tracks and buses become producers. Behaviour-neutral. | **unclaimed — start here** |
| V2 | One player, real: document + ops, graph slot, audio/MIDI sources, own playhead, one-shot/gate/loop, `player_fire`/`player_stop`. Retires the overlay; migrates launch bindings. | blocked on V1 |
| V3 | Polyphony: voice cap, choke groups, quantized start, velocity → gain. | blocked on V2 |
| V4 | Per-node automation clock; modulation §8.1 ports. | blocked on V2 (V3 not required) |
| V5 | Macros (§8.3): document rows, cycle check, surface knobs bound through them. | blocked on V4 |
| V6 | Recording: elements + automation, V-9 target rule, V-10 lane rule, V-11 deck arm. | blocked on V3 + V4 |
| V7 | Pad inspector, per-pad record display, `Op::ControlSurfaceSet` (was control-surface v0.3). | blocked on V6 |
| V8 | Hardware map (was v0.4) and more templates (was v0.5), against players. | blocked on V7 |

**Do not merge cuts.** Each one is a foundation; V1 exists precisely so V2
is not also a refactor.

## V1 — `MixNode` (unclaimed, start here)

**Goal.** One node type is what the graph compiler takes as input. `Track`,
`Bus` and later `Player` are producers of it. Nothing observable changes.

**Why first.** V2 needs a node that is not a track (ruling V-2), and the
cheapest way to get one is to stop `compile_routing`, `compile_inserts` and
`compile_pdc` from reading `TrackState` directly. Done alone, with the whole
suite plus the bounce as the gate, it is provably behaviour-neutral — which
is exactly the property that makes V2 reviewable.

**Shape.** `MixNode { id, kind, gain_db, pan, muted, soloed, inserts, sends,
output, flags }` in `audio/`, produced by `From<&TrackState>` and the bus
path. The RT graph, `ParamTable` and the mixer do **not** change: they
already think in slots and flags.

**Gate.**
- Every existing Rust and frontend test green, unchanged.
- A bounce of the demo project is **byte-identical** before and after (this
  is the real gate; record the WAV hash in the PR).
- `bus::compile_routing`'s DAG/cycle tests untouched and passing.
- No new IPC, no document change, no schema bump.

**Trap.** `TrackState` carries timeline fields (`automationMode`, clips, arm)
that a `MixNode` must not inherit — if `MixNode` grows a `clips` field, V1
has failed and V2 will inherit the hidden-track trap (research §4).

## V2 — one player, real

**Goal.** `session.players[]`, ops, a graph slot, and two sources: an audio
clip (raw or processed) and a MIDI clip with its own instrument. One
playhead per player, `player_fire` / `player_stop`. The launch overlay is
**deleted**, and existing launch bindings migrate to players.

**Gate.**
- Fire a WAV from a pad **while the arrangement plays**, at unity, with the
  source track's inserts bypassed (ruling V-6) — and the arrangement's
  transport does not move (the defect in design §2.2).
- Fire a MIDI clip into a player-owned plugin instance no track owns.
- `launch_on` and friends are gone from `audio/rt.rs`; a grep gate keeps
  them gone.
- Migration test: a project saved with launch bindings opens with players
  and the same pads fire the same material.
- Undo of "add player" / "change player source" restores byte-identically.

**Owner ear-check owed.** A raw WAV pad must sound bit-identical to the same
file auditioned in the browser. Anything else means V-6 is not implemented.

## V3 — polyphony

Voice cap (open question 1 — recommendation 32), voice stealing oldest-first
and visible, `chokeGroup`, quantized start (off / 1/16 / 1/8 / 1/4 / 1/1 /
bar), velocity → gain.

**Gate.** Eight pads sounding simultaneously with no allocation on the RT
thread (the existing RT-safety discipline applies: fixed-size state,
preallocated scratch). A choke group of two: the second press cuts the first
inside one block. Quantize 1/4 with the arrangement running: the fire lands
on the beat, not on the press.

## V4 — per-node automation clock

`ParamDriver::tick(position)` becomes per-node; the transport is the node
whose position is the transport's. `ClipEnvelope` evaluated at the player's
local position — which is what makes an envelope trigger-relative with no
new document type. Modulation §8.1 ports land here.

**Gate.** A filter sweep drawn as a clip envelope plays identically from bar
1 of the arrangement and from a pad press at an arbitrary moment. Existing
automation tests unchanged. The silent bug in design §2.4 has a regression
test.

## V5 — macros

Macro rows in the modulation document, `Source::Macro` produced for the
first time, cycle check at bind time (§8.3), surface knobs bound through
macros to ports. Ruling V-7: a knob never writes a param directly.

**Gate.** One knob moves three player params with independent depth and
range. Macro-controls-macro is rejected at bind time when it would cycle.

## V6 — recording

Elements, never a mixdown (V-8). MIDI player → MIDI clip; audio player → a
placement of the same source (no new file, content identity preserved);
knobs → curves through the existing write/touch/latch recorder (PR #85).
V-9's instrument-identity target rule, V-10's lane resolution, V-11's deck
arm with per-pad override.

**Gate.** Play four pads over two bars with one arm; get editable clips on
the right tracks (two pads sharing an instrument land on ONE track,
distinguished by note), a knob move as a curve on a connected automation
lane, and an undo that removes the whole take as one step.

## V7 — the performance UI

Pad inspector (ruling V-12: source, raw, chain, sends/output, macros,
trigger, record target — one panel), the resolved record target shown on
each pad before you press it, and `Op::ControlSurfaceSet` so the layout is
document-owned and undoable.

## V8 — hardware and templates

The old control-surface v0.4/v0.5, now pointed at players: physical LPD8
CC/notes ↔ pads and knobs, LED out, Launchpad 8×8 and MCU templates, and
"give me an LPD8" as a spoken command.

## Open questions (owner)

Recorded in the design doc §8 with recommendations: voice cap, choke-group
shape, whether players appear in the mixer, retrospective capture as its own
cut, and whether a raw player ignores the deck's output stage. V3 and V6
cannot be sized until 1, 2 and 4 are answered.
