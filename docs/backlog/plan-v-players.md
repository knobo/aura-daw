# Backlog: Plan V — players (a pad that is an instrument)

**Opened 2026-08-26.** V1 landed 2026-08-27 (PR #118). V2 landed 2026-08-30
(PR #121) — V3 is unclaimed.

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
| **V1** | `MixNode` as the compiler's input; tracks and buses become producers. Behaviour-neutral. | landed — PR #118 |
| **V2** | One player, real: document + ops, graph slot, audio/MIDI sources, own playhead, one-shot/gate/loop, `player_fire`/`player_stop`. Retires the overlay; migrates launch bindings. | landed — PR #121 |
| V3 | Polyphony: voice cap, choke groups, quantized start, velocity → gain. | **unblocked — start here** |
| V4 | Per-node automation clock; modulation §8.1 ports. | blocked on V2 (V3 not required) |
| V5 | Macros (§8.3): document rows, cycle check, surface knobs bound through them. | blocked on V4 |
| V6 | Recording: elements + automation, V-9 target rule, V-10 lane rule, V-11 deck arm. | blocked on V3 + V4 |
| V7 | Pad inspector, per-pad record display, `Op::ControlSurfaceSet` (was control-surface v0.3). | blocked on V6 |
| V8 | Hardware map (was v0.4) and more templates (was v0.5), against players. | blocked on V7 |

**Do not merge cuts.** Each one is a foundation; V1 exists precisely so V2
is not also a refactor.

## V1 — `MixNode` (landed, PR #118)

**Goal.** One node type is what the graph compiler takes as input. `Track`,
`Bus` and later `Player` are producers of it. Nothing observable changed.

**Why first.** V2 needs a node that is not a track (ruling V-2), and the
cheapest way to get one was to stop `compile_routing` and `compile_inserts`
from reading `TrackState` directly. Done alone, with the whole suite plus a
bounce-identity hash as the gate, it is provably behaviour-neutral — which
is exactly the property that makes V2 reviewable.

**Shape.** `MixNode { id, kind, gain_db, pan, muted, soloed, inserts, sends,
output }` in `audio/node.rs`, produced by `From<&TrackState>` and
`mix_nodes()`. The RT graph, `ParamTable` and the mixer did **not** change:
they already think in slots and flags. `compile_inserts` and
`compile_routing` now take `&[MixNode]`; `bus::would_cycle` deliberately
still takes `&[TrackState]` — see its doc comment.

**Gate — V2's inherited gate.** Two characterization tests, written and
hashed BEFORE the refactor, still green and unedited after it:
- `audio::offline::tests::bounce_of_a_full_strip_is_byte_stable` — a
  rendered bounce (clip + bus + send + output + gain/pan) hashed
  FNV-1a-64, hardcoded in the test.
- `audio::bus::tests::routing_plan_of_a_full_strip_is_stable` — the exact
  routing plan (`track_pdc`, `out_delay`, `output`, `bus_ids`, send
  delays), with a bypassed insert so declared PDC ≠ applied PDC is live.

Plus: every existing Rust and frontend test green, unchanged; no new IPC,
no document change, no schema bump.

**Trap (closed).** `TrackState` carries timeline fields (`automationMode`,
clips, arm) that `MixNode` must not inherit — it does not: no `clips`
field, no serde. If a later cut adds one, V1's guarantee is broken and V2
inherits the hidden-track trap (research §4).

## V2 — one player, real (landed, PR #121)

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

### Owner ear-check — done 2026-08-30

Open SURFACE, put a WAV on a pad with `raw` ticked, start the arrangement,
hit the pad. The pad must sound bit-identical to auditioning that file in
the browser, and the arrangement's playhead must not move. Anything else
means V-6/V-16 is not implemented as designed.

The owner confirmed the pad fires and sounds **while the arrangement is
playing** — which is the design §2.2 defect this cut exists to kill, and the
half no test can stand in for. The bit-identity half rests on V-16 and on
`raw`'s own tests; if a future ear ever disagrees with them, believe the ear
and start at `Player::chain_applies()`.

### Rulings V-13…V-16 (full text and V-17 in the design doc's ruling table, §5)

| Ruling | What it says |
|---|---|
| **V-13** | Clock 0 is the transport, and its `on` flag is the transport's play state. "Only launched tracks render while stopped" (the old `LaunchPlayhead::exclusive`) stops being a special case: a node whose clock is off renders nothing, and when the transport is stopped clock 0 is off. |
| **V-14** | A slot's clock binding is last-writer-wins, and released by compare-exchange. Two scenes naming the same track is expressible now that scenes are no longer singular. Stopping scene A must not steal a track that scene B has since claimed, so release compares against the clock the releaser owns. |
| **V-15** | Players do not exist in the offline bounce. `offline::render_project` compiles `mix_nodes(&store.tracks)` — tracks only. A pad performance is not arrangement material; V-8 (recording) is the way it becomes arrangement material. This is V-2's own argument made executable. |
| **V-16** | A raw player plays the clip's source region at unity: the source file's samples from `clip.offset` for `clip.len`, with no clip gain, no fades, no chain, centre pan, straight to master. This is what makes the owner's ear-check (bit-identical to browser audition) a real test rather than an approximate one. |

V-17 (delay compensation follows where a player is routed: unpadded to
master, compensated to a bus it feeds) is not reproduced here — it is long,
has two load-bearing sub-clauses (send-edge compensation, and a player's
tail outliving its trigger), and its full text is under "Rulings this plan
adds" in
[`docs/superpowers/plans/2026-08-28-plan-v2-players.md`](../superpowers/plans/2026-08-28-plan-v2-players.md).
The design doc's own §10 records the two known limits V-17 and this cut
produced.

### Known gaps and deferred debts

- **`idle_players_block_cost` has no cross-branch baseline, and never
  will.** It is an `#[ignore]`d perf test this branch added (32 idle pads,
  512-frame block, release build), and V-15 keeps players out of the
  offline-bounce harness every other perf gate reads — so at the branch's
  merge-base the test does not exist (0 passed, filtered out), not "passed
  at a different number". Four sittings, all read as unmoved within this
  harness's normal spread, but **the absolute figures do not travel between
  sittings on this machine** — only read them against each other: 1.89 µs
  (bf0cb58), 2.27 µs (round 4), 3.26 µs (task 10, and again identically at
  task 12), 2.69 µs (task 14). Any future gate-runner brief for this test
  must ask for the branch figure alone, named against these sittings for
  context, and must not ask for a base-side figure — there isn't one.
- **The blocks-rendered counter deferred from Task 8.** `flush_pending_for`
  (used to gate a scene's slot release on its flush having actually run)
  proves a render block *began*, not that a node *read* inside it — the
  gap is sub-millisecond (one poll landing inside one callback between
  `begin_block` and a node's read) and costs at most one missed
  `all_notes_off`. The airtight fix is a blocks-rendered counter, a new
  RT-visible mechanism; parked as a ci/backlog follow-up rather than
  built at the tail of an already-long task.
- **`loopjam.rs:913` is correct for a reason that could stop being true.**
  Its row is `RtTrack::clips(...)` with no live node, so the rate it
  computes reaches only `graph.rate` and nothing on that path consumes it
  — a test there could only assert code shape, not behaviour, so none was
  written. If that row ever gains a live node, the site goes silently
  wrong with no test to catch it; recheck it first if a future cut adds a
  live node to a loop-jammed row.
- **`live_tail_frames(0)` is a debug-only guard; `recompute_tail_frames` is
  `pub`, and a release build returns a 0 flush window if it is ever called
  in one without a debug assertion catching the mistake first.**
  Reachable only from test code today. Keeping the one clamp (rather than
  a second one purely for release builds) was the right call to keep the
  documentation honest, but the gap is real if this function ever grows a
  second caller.
- **Two pre-existing defects, found but not fixed (pre-existing on
  `origin/main`, untouched by this branch's diff):**
  - `TransportAction::Stop` releases a slot's clock in the same breath as
    the stop, dropping the flush frame — the same class of bug V-17(b)
    exists to prevent, just on the stop path rather than the per-block
    strip.
  - `stop_drive_launch` (`launch.rs:892-902`) lets a stale
    `FireCmd::Release` cut a scene that was re-fired after the release was
    already queued.
  Neither is created or worsened by V2; both are candidates for a future
  cut that revisits the stop/release paths.

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

## Decided while building V2 (Task 9)

**Stopping the transport does NOT cut a sounding pad. Escape / stop-all
does.** Two calls, and the split is deliberate:

* `ControlPlane::clear_launch_audible` is the transport-stop path. It cuts
  every SCENE and no player. A scene is a region of the arrangement that a
  pad borrowed, so ending the song ends it. A player is not in the song at
  all (V-2), and cutting a performance because someone stopped the
  transport is the deck going quiet mid-set.
* `ControlPlane::stop_launch_overlay` is Escape / stop-all. It cuts
  everything sounding, scenes and players alike.

Players were missing from the second one until Task 9's first fix round,
and their absence made a `TriggerMode::Loop` pad unstoppable: a looping
clock never ends itself (`ClockTable::advance` wraps it), and
`ClockTable::any_running` keeps the output callback rendering with the
transport stopped, so the pad sounded indefinitely. The only thing that
could reach it was `player_stop(id)`, which no frontend calls yet.

The consequence to keep in view: **while V2 has no UI (Task 13), there is no
PER-PAD stop a user can reach.** Stop-ALL already reaches a pad — Escape runs
`stopAllSound` → `launchStop` → the `launch_stop` command →
`stop_launch_overlay` — so a runaway pad is recoverable today. What has no
surface is stopping one pad while the others keep sounding, which is
`player_stop(id)`, and Task 13 owes both it and a stop-all binding of its
own.

## Open questions (owner)

Recorded in the design doc §8 with recommendations: voice cap, choke-group
shape, whether players appear in the mixer, retrospective capture as its own
cut, and whether a raw player ignores the deck's output stage. V3 and V6
cannot be sized until 1, 2 and 4 are answered.
