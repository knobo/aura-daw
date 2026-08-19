# Automation modes — Off / Read / Write / Touch / Latch for track-gain

**Status:** approved by the owner (2026-08-18), from an in-chat brainstorming
round. This document is the written record; rulings R1–R3 are the owner's,
everything else is the implementer's, taken under R3.

Track D (`docs/PHASE4-PLAN.md` "Track D handoff") landed track-gain
automation as **always-on**: whenever a track has a gain lane, the compiled
ramp overrides the fader for the entire playthrough, with no way to turn it
off, and no way to record a new curve by moving the fader during playback.
This is the last of Track D's actionable leftovers — the DOM test
environment leftover landed as PR #80, and the other leftover (plugin-param
automation not applied in a bounce) is not actionable here: fixing it needs
private per-bounce plugin instances, explicitly deferred to round-2 §8's
plugin-node work (`docs/PHASE4-PLAN.md` "KNOWN DIVERGENCE: export ignores
PLUGIN-PARAM automation").

Adds one persisted per-track field, one control-thread recorder, and one
small UI control. Changes no existing command name, no `OP_FORMAT_VERSION`,
and no plugin-param automation behavior.

---

## 1. Scope (R1 — owner)

**Track-gain automation only.** Plugin-parameter lanes keep today's
behavior unconditionally: the `ParamAutomationDriver` always overrides the
host value during playback (Track D ruling 2), with no mode concept. Adding
modes to plugin-param automation is a deferred follow-up (§7).

## 2. Data model

`TrackState` (`src-tauri/src/audio/types.rs:110`) gets one additive field:

```rust
/// Track-gain automation playback/record mode (this feature). Additive:
/// absent on pre-existing files reads as `Read`, which is exactly today's
/// always-on behavior — no migration, no OP_FORMAT_VERSION bump.
#[serde(default)]
pub automation_mode: AutomationMode,
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutomationMode {
    Off,
    #[default]
    Read,
    Write,
    Touch,
    Latch,
}
```

Written through the same op family that already carries `gain_db` / `pan` /
`muted` / `soloed` per track (`TrackMixChange` and its `Op` — see
`src-tauri/src/control/mod.rs` around the `TrackMixChange` definition and
its `apply`/`session.rs` arm). Add `automation_mode: Option<AutomationMode>`
to `TrackMixChange` alongside the existing optional fields, following the
exact pattern `gain_db: Option<f64>` already uses (read-modify-write against
the current value, `None` = unchanged). This is a pure UI/chrome write —
setting the mode is not automation data, it is a track property, so it does
not touch `Op::AutomationSetLane` and does not go through gesture-scoped
persist deferral itself (a mode change is a discrete click, not a drag).

**Frontend:** the mode selector is a small 5-way control (dropdown or
segmented control) added to the track header or mixer strip, next to the
existing fader. It calls the same IPC command `TrackMixChange` already goes
through (check the exact command name in `src/lib/tauri.ts` /
`src/lib/state/project.svelte.ts` — likely `track_mix_set` or equivalent;
use whatever `gain_db`/`pan` already use, do not add a new command name).

## 3. Rebuild / bounce: `Off` and `Read`

`compile_gain_ramps` (`src-tauri/src/plugins/automation.rs:571`) is called
from both `engine::rebuild` (`src-tauri/src/audio/engine.rs:1125`) and
`audio::offline::build_graph` (the bounce path). Both call sites pass it the
session's automation lanes and a track-id→slot map. Add one filter at the
top of `compile_gain_ramps`: skip a lane whose owning track's
`automation_mode == AutomationMode::Off`, in BOTH call sites, so a bounce
matches live playback exactly (same requirement Track D's close-out task
already enforced for track-gain generally). `Read` (the default) is
today's behavior, unchanged: the lane is always compiled and applied.

`Write` / `Touch` / `Latch` still compile the stored lane, but live recording
ownership temporarily bypasses that compiled gain ramp. This makes the audible
gain follow the fader exactly once while Write/Touch owns it and while Latch
holds its last touched value. On release/Stop, ownership ends and the newly
committed relative lane resumes normal Read-style playback. A track in Write
mode with an empty lane stays at the live fader value until a pass adds points.

## 4. Recording (Write / Touch / Latch)

### 4.1 Where it runs, and why

Recording runs on the **engine control thread's existing ≤2 ms tick** — the
same loop that already drives `ParamAutomationDriver::tick` for
plugin-param automation (`src-tauri/src/plugins/automation.rs:622`). It
does **not** run in the frontend on a `requestAnimationFrame` loop. This is
not a performance nicety: turning a live fader-drag event stream into
tick-indexed automation points is playhead-position math, and the "thin
renderer" rule (ADR 0006, standing constraint in `next-prompt.md`) bans time
math and business logic from landing frontend-side. The control thread is
also where `Op::AutomationSetLane` already commits (`control/session.rs`),
so committing recorded points from the same thread that reads them keeps
the whole feature in one place.

### 4.2 Reading the live fader value

`ParamTable::gain` (`src-tauri/src/audio/rt.rs:233`) is already a
`Vec<AtomicU32>`, written in real time by `set_gain_linear`
(`rt.rs:270`) on every fader-drag rAF batch, with `Ordering::Relaxed`.
Add a paired getter:

```rust
pub fn gain_linear(&self, slot: usize) -> f32 {
    self.gain.get(slot).map_or(1.0, |g| f32::from_bits(g.load(Ordering::Relaxed)))
}
```

This is the value the recorder samples each tick — no new live-value
channel needed.

### 4.3 The recorder

New type, `AutomationRecorder`, living beside `ParamAutomationDriver` in
`src-tauri/src/plugins/automation.rs` (or a new sibling module if that file
is already large — implementer's call). One instance per rebuild, holding
per-track in-progress recording state:

```rust
struct InProgressRecording {
    track_id: TrackId,
    start_tick: u64,
    points: Vec<AbsParamEvent>, // reuse the existing point type
}
```

Each control-thread tick, for every track currently playing:

- **Write**: always sample `(current_tick, gain_linear(slot))` and push a
  point (deduplicate near-identical consecutive values the same way
  `ParamAutomationDriver` already does with `EPSILON`/`REASSERT_TICKS`, to
  avoid an unbounded point list from a held-still fader).
- **Touch** / **Latch**: sample only while the track is "touched" — see
  §4.4. Latch additionally keeps sampling (holding the last touched value
  flat) after touch ends, until the recording stops (§4.5).
- **Off** / **Read**: never sample.

### 4.4 "Touched" — derived, not a new IPC flag

A track is touched when there is a currently-open gesture whose commits are
writing that track's `gain_db` — i.e. the user has an active "gain drag"
(the same gesture the fader already opens via `beginGesture`/`endGesture`,
I-8's gesture-scoped persist deferral). The control plane already tracks
open-gesture state (`OpenGesture`, `close_gesture`,
`src-tauri/src/control/mod.rs`) and already has a `CoalesceTarget` concept
for what an open gesture addresses. Add whatever minimal lookup lets the
recorder ask "is track `T`'s gain currently being dragged" from that
existing state — do not add a second, parallel touched-flag IPC path.

### 4.5 Stopping a recording pass and committing

A recording pass for a track ends on any of: playback stop, the track's
mode changing away from Write/Touch/Latch, or (Touch only) the drag gesture
closing. Latch keeps recording (holding the last value flat) past gesture
close, and only truly stops at playback stop or a mode change.

At the moment a pass ends:

1. Build the merged lane: take the track's current `AutomationLane`, drop
   any existing points whose tick falls inside
   `[start_tick, end_tick)`, and splice in the newly recorded points sorted
   by tick — mirroring the existing whole-lane-replace contract
   `Op::AutomationSetLane` already has (Track D ruling 5 / the manual
   lane-edit path).
2. Commit it as **one** `Op::AutomationSetLane` for that track's gain lane
   key, through the same transact/undo path manual lane edits use, so one
   recording pass is exactly one undo entry (matching "a drag is one undo
   entry", the convention every other gesture-shaped commit in this
   codebase already follows). Attribute it distinctly (e.g. `"record
   automation: <track name>"`) so undo history reads sensibly.
3. This is the engine control thread authoring an `Op` on its own
   initiative, not in response to a frontend-originated command. That is
   new for this codebase, but it does not violate ADR 0006 (which restricts
   the FRONTEND, not the Rust control layer) and it is the only way this
   feature can work — call this out explicitly in code comments at the
   commit site so a future reader does not mistake it for a violation.
4. The commit closure must not panic (standing RT/transact rule) and must
   respect the existing gesture-before-session lock order.

### 4.6 A record pass with zero points

If a Write/Touch/Latch pass produces no points (e.g. transport started and
stopped instantly, or Touch armed but the drag never actually moved the
value), skip the commit — do not create a phantom undo entry, matching the
guard `close_gesture` already has for a no-op batch (`control/mod.rs`
`close_gesture_executes_a_deferred_persist_even_when_the_gesture_nets_to_no_ops`
is the adjacent case: a deferred *persist* still fires even when *ops* net
to nothing; a *recording* pass with literally zero sampled points is the
stronger case — no ops, no persist, no undo entry).

## 5. UI

One small mode control per track (Off / Read / Write / Touch / Latch),
placed next to the existing fader in the track header or mixer strip —
implementer's call on exact placement, follow whatever existing per-track
controls (mute/solo) use for layout. Sends `automation_mode` through the
existing `TrackMixChange`-shaped command (§2). No new command name. Covered
by the DOM test environment (PR #80,
`src/lib/components/automation-track-row.dom.test.ts` is the precedent to
follow for testing a mounted `.svelte` component's behavior).

## 6. Testing

- **Rust, `compile_gain_ramps`**: a track in `Off` mode is skipped by both
  the live-rebuild call site and the offline/bounce call site, given the
  same lane data (parity test — bounce must match live exactly, the same
  property Track D's close-out task already asserts for the non-Off case).
- **Rust, recorder unit tests**: synthetic `ParamTable::gain` atomic writes
  at known ticks; assert the recorder produces the expected point sequence
  for Write (continuous), Touch (only while "touched", per a fake open
  gesture), and Latch (continues flat after "touch" ends).
- **Rust, integration**: a Touch pass followed by release re-reads the
  PRE-existing curve for ticks after release (not the recorded flat value);
  a Latch pass holds the last value for ticks after release until stop.
- **Rust**: a zero-point recording pass commits nothing (§4.6) — no new
  undo entry, verified against `Session::history`/undo depth.
- **Frontend, DOM test**: the mode selector renders all five states and
  calls the expected command with the expected value on change (jsdom,
  PR #80's environment).

## 7. Non-goals (deferred, recorded so the next round knows these were choices)

- **Recording across a loop boundary.** No special handling: each loop pass
  records into the same range, and the last pass before recording stops
  wins (whatever was recorded in the final pass through that range is what
  persists). A dedicated punch-in/punch-out-per-lap UI is a follow-up.
- **Write / Touch / Latch for plugin-parameter lanes.** Plugin-param
  automation keeps today's always-on-during-playback behavior
  unconditionally (Track D ruling 2); this feature does not add a mode
  concept there. A follow-up would need the `ParamAutomationDriver` itself
  to gate on mode and touched-state the same way §4 does for track-gain.
- **A version-graph-visible "automation take" concept.** Each recording
  pass is an ordinary undo entry; there is no separate UI for browsing past
  takes beyond normal undo/redo.
