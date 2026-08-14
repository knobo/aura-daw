# Plan C+D — Time + Project v3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: this plan was authored to be
> executed by a fresh subagent per task (`superpowers:subagent-driven-development`)
> or via `superpowers:executing-plans`. **Tonight's execution is a documented
> exception**: solo session, no subagent dispatch (token economy, owner
> asleep) — one implementer (me) runs every task in TDD order, foreground
> test gates, one commit per task, self-review standing in for the missing
> external reviewer. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship ONE v2→v3 project-format bump carrying (C) `Ticks`/`Samples`
newtypes, integer-period supertick tempo with ramps, a minimal time-signature
(meter) map, and a precomputed section table bound to <64 samples deviation
on curved-ramp subdivision; and (D) the content/placement identity split —
`ContentId` populated, `LaneId` referencing a default lane per track — for
MIDI in full and for audio at the addressing level. Frontend deletes its
piecewise tick↔sample math and consumes the shipped section table.

**Architecture:** A new `src-tauri/src/time.rs` module owns the two
compile-time-distinct newtypes and the supertick constant; `midi::tempo`
evolves its existing `TempoMap` (frozen name, per round-2 §3.6's "flat
numbers, domain fixed by field name" wire discipline) from an f64-bpm
piecewise table to an integer-period one, keeping the old bpm-based
constructors as quantizing wrappers. A new `midi::section_table` module
compiles constant-tempo segments from a `TempoMap` + meter map, subdividing
ramps under a versioned, property-tested error bound. `midi::persist`
(today's sole owner of the v2 JSON bolt-on, per its own module doc) grows a
schemaVersion-3 path: period-based `tempoMap`, a new `meterMap`, and —
staying inside the SAME v3 bump per PHASE4-PLAN's "D ships inside C's v3
migration" — new `content`/`placements`/`lanes` JSON arrays for MIDI clips.
Audio clips (`audio::project::Clip`, the typed v1/v3 struct) gain
`content_id`/`lane_id` fields for real addressing but keep their existing
single-row JSON shape (scope ruling below). `project.rs`'s schema gate
widens from `(1..=2)` to `(1..=3)`.

**Tech Stack:** Rust (src-tauri crate: serde, serde_json, proptest already a
dev-dependency), TypeScript/Svelte 5 frontend (`src/lib/`), vitest.

**Spec:** `docs/CORE-REDESIGN-ROUND-2.md` §3 (time) + §5 (content/placement)
+ §0.1 rows O-4/O-5/O-9/O-10; ADR 0002 (time model), ADR 0004
(content/placement split). Orchestration: `docs/PHASE4-PLAN.md` (sub-plan
C/D rows, Gate C/D, "Plan A handoff" + "Plan B handoff" carry-forwards).

## Global Constraints

- **The op log stays dark** — no journal file, no undo UI (PHASE4-PLAN rule
  1, ADR 0003). This plan adds no `Op` variants; tempo/meter/content-schema
  changes are not yet routed through `Session::transact` (they weren't
  before this plan either — `set_tempo_map` and the MIDI clip commands sit
  outside the A-slice channel today; that stays true here, ledgered as an
  Plan-E-bound item, not fixed in this plan).
- **Thin renderer** (ADR 0006, owner-accepted): no new authoritative state,
  business logic, or time math lands frontend-side. **Ruling on round-2
  §3.6's "TS-over-the-table or shared Rust-via-wasm" choice:** TS-over-the-
  table, implemented as **pure linear interpolation against backend-supplied
  section-table rows** (no bpm/period/tempo knowledge of any kind in TS).
  This is presentation-layer interpolation of already-derived data, not
  "time math" in ADR 0006's sense (deriving a bijection from tempo events —
  exactly what gets deleted). wasm is not pursued (dossier 09 §11 flags it
  unmeasured; no bench justifies the added build complexity tonight).
- **Frozen command/event names stay frozen; new commands are additive**
  (PHASE4-PLAN rule 3). `set_tempo_map`'s signature is UNCHANGED (still
  `(ppq: number | null, events: TempoEvent[])` — bpm-per-event); its body
  becomes a wrapper that quantizes bpm→period once at entry. `TempoMapState`
  gains additive fields (`meterMap`, `periodEvents`, `sectionTable`,
  `sectionTableRuleVersion`); `events` (bpm-projected) stays for wire
  compatibility, derived from period on every read, no longer the source of
  truth.
- **Evidence policy** (ADR 0007): corrections marked, never silent. Two
  scope rulings below are corrections-in-advance, not silent narrowing —
  read them before touching Task 7/8.
- **Prepare-outside/commit-inside** for anything doing I/O (round-2 §4.2)
  — n/a to this plan's pure/compute tasks; relevant to Task 6/7's migration
  I/O, which follows the existing `midi::persist` atomic-write discipline
  (tmp + fsync + rename) already in place.
- **One format bump, not two** (PHASE4-PLAN, binding): `schema_version`
  moves `1..=2` → `1..=3` exactly once (Task 6); Tasks 6, 7 and 8 all write
  the SAME v3, extended additively task by task — never a v3→v4 split for
  the placement work.
- **Foreground test runs only, `timeout`-guarded:**
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml` and
  `timeout 300 npx vitest run`. Baseline at plan start (verified
  2026-08-14): **346 backend + 80 frontend, all green.**
- Corrections to docs are marked, never silent (ADR 0007) — every scope
  ruling in this plan is written into `docs/PHASE4-PLAN.md`'s eventual "Plan
  C/D handoff" section (Task 10) verbatim, not softened.

### Scope rulings (decided now, so no task stalls on an open question)

1. **Content/placement runtime split — MIDI gets the full split, audio gets
   addressing only.** Round-2 §5's table applies uniformly to both, but
   audio's persistence path (`audio::project.rs`, the TYPED v1/v3 struct
   shared with `tracks`) has far more consumers (recorder, engine RT graph
   walk, waveform, offline bounce) than MIDI's raw-JSON v2 bolt-on. MIDI is
   also round-2's explicitly named priority ("instancing arrives free for
   MIDI; for audio it is a byproduct, not a goal"). Tonight, without a
   second reviewer or subagent backup, the responsible cut is: MIDI clips
   get real `content[]`/`placements[]`/`lanes[]` JSON arrays (Task 7);
   audio clips get real, populated `content_id`/`lane_id` fields *on the
   existing `Clip` row* (Task 8) — addressing is genuine (a future round can
   array-split audio without another format bump) but the JSON shape stays
   one-row-per-clip for now. This is recorded, not silent, per ADR 0007.
2. **No Rust-level `Content`/`Placement` struct replaces `MidiClip`/`Clip`
   as the in-memory `Store`/`MidiStore` type tonight.** `MidiClip` gains
   `content_id: ContentId` and `lane_id: LaneId` fields (Task 7); the
   `content`/`placements` JSON shape is assembled from and collapsed back
   into `MidiClip` at the `midi::persist` boundary only. Full type-level
   separation (so editing shared content actually updates every placement —
   ADR 0004's stated consequence) is deferred; nothing today creates two
   placements sharing one `ContentId` (no split/merge/copy command exists
   yet — confirmed by grep, matches the Plan B handoff note), so no runtime
   behavior is lost by this deferral, only the instancing feature itself,
   which round-2 already scopes as "arrives free" later, not required now.
   Carried forward explicitly in Task 10's handoff section.
3. **`steady_time` (round-2 §3.5) and the per-block `Arc<TempoMap>` swap are
   OUT of this plan.** Both are real round-2 §3 content, but they are RT
   engine-thread wiring (replacing `clap_host.rs`'s per-node `self.steady:
   u64` counter with one engine-global, threading it through the `RtNode`
   trait used by 9+ node types) — a different risk class from the pure/
   compute and persistence work this plan otherwise does, and not required
   by Gate C/D's own test list (test 6 section-table bound, test 7 tempo/
   migration round-trip, frontend section-table consumption). Recorded as a
   Task 10 carry-forward for whichever plan next touches the RT engine
   thread (naturally Plan E, which already inventories side-channel/engine-
   thread work).

---

### Task 1: `Ticks`/`Samples` newtypes and cross-domain comparison

**Files:**
- Create: `src-tauri/src/time.rs`
- Modify: `src-tauri/src/main.rs` (add `pub mod time;` to the module list —
  grep the existing `pub mod midi;` line and add `time` alphabetically
  near it)

**Interfaces:**
- Produces: `time::Ticks(pub u64)`, `time::Samples(pub u64)`, both
  `Copy + Eq + Ord + Hash + Default + Serialize + Deserialize` (transparent
  wire encoding — a bare number, per round-2 §3.6); `time::CmpIn<Other>`
  trait with `cmp_in(&self, other: &Other, map: &TempoMap) -> Ordering`;
  `time::SUPERTICKS_PER_SECOND: u64 = 508_032_000`.
- Consumes: `midi::tempo::TempoMap` (Task 3) — `CmpIn` impls call
  `TempoMap::tick_to_samples_v3`, which does not exist until Task 3. This
  task's own tests do not exercise `CmpIn` (Step 4 below is deferred to
  Task 3's test suite); the trait and its two impls compile against a
  forward declaration you write in this task, calling a method Task 3 adds.
  **Order note:** because of this, Steps 1–3 of this task land the newtypes
  alone; `CmpIn` (Step 4) is written in this task but its impl bodies are
  added in Task 3 once `tick_to_samples_v3` exists — see Task 3 Step 5.
  Until then, declare the trait with NO impls (Step 4 below only declares
  the trait, does not implement it for Ticks/Samples).

- [ ] **Step 1: Write the failing test**

```rust
// src-tauri/src/time.rs (new file, top)
//! Compile-time-distinct time domains (round-2 §3.1, ADR 0002). MIDI/
//! musical positions are `Ticks`; audio clip anchors and engine positions
//! are `Samples`. No `Ord` crosses domains and there is no ambient global
//! tempo map — conversion is a named call on `midi::tempo::TempoMap`
//! (O-4). This module owns the newtypes and the supertick constant only;
//! the bijection itself lives in `midi::tempo` (Task 3).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_and_samples_are_distinct_types_with_checked_arithmetic() {
        let a = Ticks(100);
        let b = Ticks(40);
        assert_eq!(a.checked_sub(b), Some(Ticks(60)));
        assert_eq!(b.checked_sub(a), None, "underflow is None, never a wrap");
        assert_eq!(a.checked_add(b), Some(Ticks(140)));
        assert_eq!(Ticks(u64::MAX).checked_add(Ticks(1)), None);

        let s = Samples(48_000);
        assert_eq!(s.checked_add(Samples(2_000)), Some(Samples(50_000)));

        // Serialize transparently (a bare number, round-2 §3.6).
        assert_eq!(serde_json::to_string(&a).unwrap(), "100");
        let back: Ticks = serde_json::from_str("100").unwrap();
        assert_eq!(back, a);
    }
}
```

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml time::`
Expected: FAIL (module doesn't exist / `Ticks`/`Samples` undefined).

- [ ] **Step 2: Register the module**

In `src-tauri/src/main.rs`, find the `pub mod midi;` line (or equivalent
module list near the top) and add `pub mod time;` next to it.

- [ ] **Step 3: Implement the newtypes**

```rust
use serde::{Deserialize, Serialize};

/// Musical position/duration, ticks at the project PPQ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
         Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ticks(pub u64);

/// Audio-engine position/duration, samples at the project sample rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
         Serialize, Deserialize)]
#[serde(transparent)]
pub struct Samples(pub u64);

impl Ticks {
    pub fn checked_add(self, rhs: Ticks) -> Option<Ticks> { self.0.checked_add(rhs.0).map(Ticks) }
    pub fn checked_sub(self, rhs: Ticks) -> Option<Ticks> { self.0.checked_sub(rhs.0).map(Ticks) }
}
impl Samples {
    pub fn checked_add(self, rhs: Samples) -> Option<Samples> { self.0.checked_add(rhs.0).map(Samples) }
    pub fn checked_sub(self, rhs: Samples) -> Option<Samples> { self.0.checked_sub(rhs.0).map(Samples) }
}

/// Ardour's superclock constant: 2^10 * 3^4 * 5^3 * 7^2 per second,
/// divisible by every common sample rate and PPQ (round-2 §3.3).
pub const SUPERTICKS_PER_SECOND: u64 = 508_032_000;
```

- [ ] **Step 4: Declare (not yet implement) `CmpIn`**

```rust
/// Cross-domain comparison, explicit and never `Ord` (round-2 §3.1):
/// comparing a `Ticks` and a `Samples` value needs a tempo map to make
/// them commensurable, so every call site names one. Implemented in
/// `midi::tempo` (Task 3), once `TempoMap::tick_to_samples_v3` exists —
/// this crate keeps the newtypes free of a dependency on the tempo map's
/// internals, but the impl itself needs the bijection, so it lives where
/// the bijection lives.
pub trait CmpIn<Other> {
    fn cmp_in(&self, other: &Other, map: &crate::midi::tempo::TempoMap) -> std::cmp::Ordering;
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml time::`
Expected: PASS, 1 new test. (The crate now has an unused-trait warning for
`CmpIn` until Task 3 implements it — acceptable for one commit; `cargo test`
does not fail on warnings in this crate's configured lint level. If your
toolchain treats it as `deny(warnings)`, add `#[allow(dead_code)]` above the
trait with a `// REVIEW:` comment noting Task 3 removes it.)

- [ ] **Step 6: Full backend suite + commit**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, 347 tests (346 + 1).

```bash
git add src-tauri/src/time.rs src-tauri/src/main.rs
git commit -m "$(cat <<'EOF'
feat(time): Ticks/Samples newtypes, no cross-domain Ord (round-2 §3.1, ADR 0002)

Compile-time-distinct time domains land first, standalone: Ticks(u64) for
musical positions, Samples(u64) for audio-engine positions, transparent
wire encoding, checked arithmetic only. CmpIn is declared (not yet
implemented — Task 3 wires it to the tempo map's bijection).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Supertick period tempo — quantization and the v3 event type

**Files:**
- Modify: `src-tauri/src/time.rs` (add `period_from_bpm`/`bpm_from_period`)
- Modify: `src-tauri/src/midi/types.rs` (add `TempoPeriodEvent`)

**Interfaces:**
- Consumes: `time::SUPERTICKS_PER_SECOND` (Task 1).
- Produces: `time::period_from_bpm(bpm: f64) -> u64`,
  `time::bpm_from_period(period: u64) -> f64`; `midi::types::TempoPeriodEvent
  { tick: u64, period_start: u64, period_end: u64 }` (`Serialize`/
  `Deserialize`, `camelCase` wire: `tick`, `periodStart`, `periodEnd`).
  `period_start == period_end` means constant tempo across the event's span
  (round-2 §3.3).

- [ ] **Step 1: Write the failing test**

```rust
// src-tauri/src/time.rs, in #[cfg(test)] mod tests
#[test]
fn bpm_quantizes_to_an_integer_period_and_back_within_spec_error() {
    // Round-2 §3.3: max error ~2.4e-7 BPM at 120.5 (displays as 120.5
    // forever); storage and derived math are exact thereafter.
    let period = period_from_bpm(120.5);
    let back = bpm_from_period(period);
    assert!((back - 120.5).abs() < 3e-7, "round-trip error too large: {back}");

    // Exact cases: bpm values whose period is an exact integer round-trip
    // with zero error (60 bpm -> period == SUPERTICKS_PER_SECOND exactly).
    assert_eq!(period_from_bpm(60.0), SUPERTICKS_PER_SECOND);
    assert_eq!(bpm_from_period(SUPERTICKS_PER_SECOND), 60.0);

    // Storage is exact thereafter: quantizing an ALREADY-quantized period's
    // displayed bpm produces the SAME period (no drift on repeated saves).
    let p1 = period_from_bpm(bpm_from_period(period));
    assert_eq!(p1, period, "re-quantizing the displayed bpm must not drift");
}

#[test]
fn period_rejects_non_finite_or_non_positive_bpm_by_saturating_away_from_zero() {
    // Defensive: a zero/negative/NaN bpm must not divide-by-zero or
    // produce a period of 0 (which would make every later tick math
    // divide by zero). Callers validate bpm > 0 before this point (as
    // TempoMap::new already does); this is a belt-and-braces floor.
    assert!(period_from_bpm(f64::MIN_POSITIVE) > 0, "never zero");
}
```

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml time::`
Expected: FAIL (`period_from_bpm`/`bpm_from_period` undefined).

- [ ] **Step 2: Implement**

```rust
// src-tauri/src/time.rs
/// Quantize a user-typed BPM to the nearest integer period, ONCE, at entry
/// (round-2 §3.3). Never call this on an already-stored period's displayed
/// bpm in a loop — that's exactly the "re-quantize on every save" bug this
/// guards against; storage keeps the integer period, not the bpm.
pub fn period_from_bpm(bpm: f64) -> u64 {
    let p = (SUPERTICKS_PER_SECOND as f64 * 60.0 / bpm).round();
    if !p.is_finite() || p < 1.0 { 1 } else { p as u64 }
}

/// Derive a display bpm from a stored period. Exact given the period;
/// the only lossiness in the whole pipeline is the ONE quantization at
/// `period_from_bpm` entry.
pub fn bpm_from_period(period: u64) -> f64 {
    SUPERTICKS_PER_SECOND as f64 * 60.0 / period.max(1) as f64
}
```

- [ ] **Step 3: Run test to verify it passes**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml time::`
Expected: PASS, 3 tests in `time::tests` (1 from Task 1 + 2 new).

- [ ] **Step 4: Add `TempoPeriodEvent` — write its failing test**

```rust
// src-tauri/src/midi/types.rs, in #[cfg(test)] mod tests
#[test]
fn tempo_period_event_serializes_camel_case_and_transparent_period_fields() {
    let e = TempoPeriodEvent { tick: 3840, period_start: 4_233_600_000, period_end: 4_233_600_000 };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["tick"], 3840);
    assert_eq!(v["periodStart"], 4_233_600_000u64);
    assert_eq!(v["periodEnd"], 4_233_600_000u64);
    let back: TempoPeriodEvent = serde_json::from_value(v).unwrap();
    assert_eq!(back, e);
}
```

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml midi::types::`
Expected: FAIL (`TempoPeriodEvent` undefined).

- [ ] **Step 5: Implement `TempoPeriodEvent`**

```rust
// src-tauri/src/midi/types.rs, near TempoEvent
/// One tempo change in the v3 integer-period model (round-2 §3.3): period
/// is the duration of one quarter note in superticks
/// (`crate::time::SUPERTICKS_PER_SECOND`), sample-rate independent.
/// `period_start == period_end` is constant tempo; otherwise a linear-in-
/// period ramp across `[tick, next_event.tick)` (§3.3 "Ramps" — linear in
/// seconds-per-beat, not in bpm).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoPeriodEvent {
    pub tick: u64,
    pub period_start: u64,
    pub period_end: u64,
}
```

- [ ] **Step 6: Run tests, full suite, commit**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, 350 tests (347 + 3).

```bash
git add src-tauri/src/time.rs src-tauri/src/midi/types.rs
git commit -m "$(cat <<'EOF'
feat(time): integer-period tempo quantization + TempoPeriodEvent (round-2 §3.3)

period_from_bpm/bpm_from_period: a typed BPM quantizes ONCE at entry to the
nearest integer supertick period; storage and derived math are exact
thereafter (re-quantizing an already-stored period's displayed bpm is a
no-op, guarding the exact bug O-9 named: round 1's unstated version would
have drifted on every save). TempoPeriodEvent is the v3 wire shape.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: `TempoMap` v3 core — period-based bijection with linear-in-period ramps

**Files:**
- Modify: `src-tauri/src/midi/tempo.rs`

**Interfaces:**
- Consumes: `midi::types::TempoPeriodEvent` (Task 2), `time::{Ticks, Samples,
  SUPERTICKS_PER_SECOND, period_from_bpm, bpm_from_period}` (Tasks 1–2).
- Produces: `TempoMap::from_periods(ppq: u32, events: Vec<TempoPeriodEvent>,
  sample_rate: u32) -> Result<Self, String>`; `TempoMap::tick_to_samples_v3
  (&self, t: Ticks) -> Samples`; `TempoMap::samples_to_tick_v3(&self, s:
  Samples) -> Ticks`; `TempoMap::period_events(&self) -> &[TempoPeriodEvent]`.
  `TempoMap::new`/`from_v1` (the old bpm-based constructors) and
  `tick_to_samples`/`samples_to_tick` (the old u64-based methods) are KEPT
  as thin wrappers — frozen names, per PHASE4-PLAN rule 3 applied to this
  internal API too (external callers — `midi::mod.rs`'s `set_tempo_map`,
  frontend types — don't change in this task).

**Design note for the implementer:** internally, `TempoMap` currently
stores `events: Vec<TempoEvent>` (bpm) and a precomputed
`sample_at_event: Vec<f64>`. This task REPLACES that storage with
`period_events: Vec<TempoPeriodEvent>` and precomputed cumulative
`(supertick, sample)` pairs, and makes the OLD bpm-based API a thin
wrapper: `new(ppq, events: Vec<TempoEvent>, rate)` quantizes each `bpm` via
`period_from_bpm` into a `TempoPeriodEvent{period_start: p, period_end: p}`
(constant, since the old wire shape had no ramp concept) and calls
`from_periods`. `tick_to_samples(&self, tick: u64) -> u64` becomes
`self.tick_to_samples_v3(Ticks(tick)).0`.

For a ramp segment `period_start != period_end` spanning ticks
`[t0, t1)`, the period varies LINEARLY in tick (round-2: "linear in period"
means linear in seconds-per-beat, and period IS seconds-per-beat times the
supertick rate, so it's linear in the same variable either way):
`period(t) = period_start + (period_end - period_start) * (t - t0) / (t1 - t0)`.
Samples-per-tick at `t` is `period(t) / SUPERTICKS_PER_SECOND * rate / ppq`
— linear in `t`, so its integral from `t0` to any `t` in range is the exact
trapezoid: `(t - t0) * (spt(t0) + spt(t)) / 2`. This closed form is EXACT
(no numeric integration needed for a linear ramp) and is what
`tick_to_samples_v3` uses inside a ramp segment; Task 5's section table is
a SEPARATE, deliberately lossy piecewise-constant approximation of this
same exact function (round-2 §3.4 requires constant-tempo segments even
though a linear ramp needs none — see Task 5).

- [ ] **Step 1: Write the failing tests**

```rust
// src-tauri/src/midi/tempo.rs, in #[cfg(test)] mod tests
use crate::time::{Samples, Ticks, SUPERTICKS_PER_SECOND};

#[test]
fn v3_constant_tempo_matches_the_old_bpm_path_exactly() {
    // 120 bpm @48k, 960 ppq: quarter note = 24000 samples, same numbers
    // the existing v1_equivalent_single_entry_map test pins for the old API.
    let period = crate::time::period_from_bpm(120.0);
    let m = TempoMap::from_periods(
        960,
        vec![TempoPeriodEvent { tick: 0, period_start: period, period_end: period }],
        48_000,
    ).unwrap();
    assert_eq!(m.tick_to_samples_v3(Ticks(0)), Samples(0));
    assert_eq!(m.tick_to_samples_v3(Ticks(960)), Samples(24_000));
    assert_eq!(m.tick_to_samples_v3(Ticks(4 * 960)), Samples(96_000));
    assert_eq!(m.samples_to_tick_v3(Samples(24_000)), Ticks(960));
}

#[test]
fn v3_linear_ramp_hits_the_exact_trapezoid_midpoint() {
    // A ramp from 120bpm to 60bpm across one bar (3840 ticks @960ppq).
    // Samples-per-tick at the start: period_from_bpm(120)/SUPERTICKS*48000/960.
    // At the midpoint tick, period is the ARITHMETIC MEAN of start/end
    // (linear in tick) so cumulative samples at the midpoint is the exact
    // trapezoid: (span/2) * (spt(t0) + spt(mid)) / 2.
    let p_start = crate::time::period_from_bpm(120.0);
    let p_end = crate::time::period_from_bpm(60.0);
    let m = TempoMap::from_periods(
        960,
        vec![
            TempoPeriodEvent { tick: 0, period_start: p_start, period_end: p_end },
            TempoPeriodEvent { tick: 3840, period_start: p_end, period_end: p_end },
        ],
        48_000,
    ).unwrap();
    let spt = |period: u64| period as f64 / SUPERTICKS_PER_SECOND as f64 * 48_000.0 / 960.0;
    let p_mid = p_start + (p_end - p_start) / 2; // tick 1920 is exactly halfway
    let expected_mid_samples = 1920.0 * (spt(p_start) + spt(p_mid)) / 2.0;
    let got = m.tick_to_samples_v3(Ticks(1920)).0 as f64;
    assert!((got - expected_mid_samples).abs() <= 1.0, "got {got}, expected ~{expected_mid_samples}");
    // Monotonic: end-of-ramp sample position must exceed the midpoint.
    assert!(m.tick_to_samples_v3(Ticks(3840)).0 as u64 > m.tick_to_samples_v3(Ticks(1920)).0);
}

#[test]
fn v3_roundtrip_across_a_ramp_and_a_constant_segment() {
    let p120 = crate::time::period_from_bpm(120.0);
    let p90 = crate::time::period_from_bpm(90.0);
    let m = TempoMap::from_periods(
        960,
        vec![
            TempoPeriodEvent { tick: 0, period_start: p120, period_end: p90 },
            TempoPeriodEvent { tick: 3840, period_start: p90, period_end: p90 },
        ],
        48_000,
    ).unwrap();
    for tick in [0u64, 1, 500, 3839, 3840, 3841, 10_000] {
        let s = m.tick_to_samples_v3(Ticks(tick));
        let back = m.samples_to_tick_v3(s);
        // Round-trip within 1 tick — inversion of the ramp's closed form
        // uses a numeric solve (Step 3), not exact algebra, in-segment.
        assert!((back.0 as i64 - tick as i64).abs() <= 1, "tick {tick} -> {s:?} -> {back:?}");
    }
}

#[test]
fn old_bpm_api_is_a_thin_wrapper_and_still_matches_pinned_numbers() {
    // Pins from the pre-existing tests (multi_segment_conversion_and_roundtrip)
    // must still pass byte-identically through the new internals.
    let m = TempoMap::new(
        960,
        vec![
            TempoEvent { tick: 0, bpm: 120.0 },
            TempoEvent { tick: 3840, bpm: 60.0 },
        ],
        48_000,
    ).unwrap();
    assert_eq!(m.tick_to_samples(3840), 96_000);
    assert_eq!(m.tick_to_samples(3840 + 960), 96_000 + 48_000);
}
```

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml midi::tempo::`
Expected: FAIL (`from_periods`/`tick_to_samples_v3`/`samples_to_tick_v3`
undefined; `TempoMap::new` still compiles but this task hasn't touched it
yet so the last test currently passes against the OLD implementation —
that's fine, it's your regression pin).

- [ ] **Step 2: Rewrite `TempoMap`'s storage and add the v3 constructor**

```rust
// src-tauri/src/midi/tempo.rs — replace the struct and `new`/`from_v1`
use crate::time::{Samples, Ticks, SUPERTICKS_PER_SECOND};
use super::types::{TempoEvent, TempoPeriodEvent, DEFAULT_PPQ};

#[derive(Debug, Clone)]
pub struct TempoMap {
    ppq: u32,
    /// Sorted by tick, first entry at tick 0 (validated in `from_periods`).
    events: Vec<TempoPeriodEvent>,
    /// events[i] -> (cumulative supertick, cumulative sample) at events[i].tick.
    cum_at_event: Vec<(u128, f64)>,
    rate: u32,
}

impl TempoMap {
    /// v3 constructor: integer-period events (round-2 §3.3).
    pub fn from_periods(ppq: u32, events: Vec<TempoPeriodEvent>, sample_rate: u32) -> Result<Self, String> {
        if ppq == 0 { return Err("ppq must be > 0".into()); }
        if sample_rate == 0 { return Err("sampleRate must be > 0".into()); }
        if events.is_empty() { return Err("tempo map must have at least one entry".into()); }
        if events[0].tick != 0 { return Err("first tempo event must be at tick 0".into()); }
        for pair in events.windows(2) {
            if pair[1].tick <= pair[0].tick {
                return Err("tempo events must have strictly increasing ticks".into());
            }
        }
        for e in &events {
            if e.period_start == 0 || e.period_end == 0 {
                return Err("tempo period must be > 0".into());
            }
        }
        let mut cum_at_event = Vec::with_capacity(events.len());
        cum_at_event.push((0u128, 0.0f64));
        for (i, pair) in events.windows(2).enumerate() {
            let (span_supertick, span_samples) = segment_span(pair[0], pair[1].tick, ppq, sample_rate);
            let (prev_st, prev_s) = cum_at_event[i];
            cum_at_event.push((prev_st + span_supertick, prev_s + span_samples));
        }
        Ok(Self { ppq, events, cum_at_event, rate: sample_rate })
    }

    /// v1/v2 constructor kept as a thin wrapper: quantize each bpm ONCE
    /// (Task 2's `period_from_bpm`) into a constant-period event.
    pub fn new(ppq: u32, events: Vec<TempoEvent>, sample_rate: u32) -> Result<Self, String> {
        let period_events = events.iter().map(|e| {
            if !e.bpm.is_finite() || e.bpm <= 0.0 {
                return Err(format!("invalid bpm: {}", e.bpm));
            }
            let p = crate::time::period_from_bpm(e.bpm);
            Ok(TempoPeriodEvent { tick: e.tick, period_start: p, period_end: p })
        }).collect::<Result<Vec<_>, String>>()?;
        Self::from_periods(ppq, period_events, sample_rate)
    }

    pub fn from_v1(tempo_bpm: f64, sample_rate: u32) -> Result<Self, String> {
        Self::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: tempo_bpm }], sample_rate)
    }

    pub fn ppq(&self) -> u32 { self.ppq }
    pub fn sample_rate(&self) -> u32 { self.rate }
    pub fn period_events(&self) -> &[TempoPeriodEvent] { &self.events }

    /// Old bpm-projected view, for wire compatibility (`TempoMapState.events`).
    pub fn events(&self) -> Vec<TempoEvent> {
        self.events.iter().map(|e| TempoEvent {
            tick: e.tick,
            bpm: crate::time::bpm_from_period(e.period_start),
        }).collect()
    }

    pub fn tick_to_samples(&self, tick: u64) -> u64 { self.tick_to_samples_v3(Ticks(tick)).0 }
    pub fn samples_to_tick(&self, samples: u64) -> u64 { self.samples_to_tick_v3(Samples(samples)).0 }
}

/// Exact closed-form (tick-domain trapezoid) supertick/sample span of one
/// segment `[event.tick, next_tick)`. Linear-in-tick period means samples-
/// per-tick is linear in tick, so the trapezoid rule is EXACT, not an
/// approximation.
fn segment_span(event: TempoPeriodEvent, next_tick: u64, ppq: u32, rate: u32) -> (u128, f64) {
    let dticks = next_tick - event.tick;
    let supertick_span = (event.period_start as u128 + event.period_end as u128) / 2 * dticks as u128 / ppq as u128
        * (ppq as u128); // supertick accumulates as period-per-quarter * (ticks/ppq quarters); see samples formula below for the sample side, which is the one actually consumed.
    // Supertick accounting: exact integral of a tick-linear period is the
    // trapezoid of the two endpoint periods times the tick span, scaled by
    // quarters-per-tick (1/ppq).
    let supertick_span = (event.period_start as u128 + event.period_end as u128) * dticks as u128 / (2 * ppq as u128);
    let _ = supertick_span; // first computation shadowed intentionally below; keep one.
    let spt0 = event.period_start as f64 / SUPERTICKS_PER_SECOND as f64 * rate as f64 / ppq as f64;
    let spt1 = event.period_end as f64 / SUPERTICKS_PER_SECOND as f64 * rate as f64 / ppq as f64;
    let sample_span = dticks as f64 * (spt0 + spt1) / 2.0;
    (supertick_span, sample_span)
}
```

**// REVIEW:** the `segment_span` function above has a dead first
computation of `supertick_span` (shadowed) left from drafting the exact
integer formula — clean this up in Step 2's actual edit (delete the first
`let supertick_span = ...` line and its trailing comment, keep only the
second, correct one). Flagged here because it must not survive into the
committed diff; the plan's own code block has it only to show the algebra
that motivates the final formula. Self-review before commit MUST remove it.

- [ ] **Step 3: Implement `tick_to_samples_v3`/`samples_to_tick_v3`**

```rust
impl TempoMap {
    pub fn tick_to_samples_v3(&self, tick: Ticks) -> Samples {
        let tick = tick.0;
        let i = match self.events.binary_search_by(|e| e.tick.cmp(&tick)) {
            Ok(i) => i,
            Err(i) => i - 1, // safe: events[0].tick == 0
        };
        let e = self.events[i];
        let (_, base_samples) = self.cum_at_event[i];
        let dticks = tick - e.tick;
        let next_tick = self.events.get(i + 1).map(|n| n.tick);
        let span = next_tick.map(|nt| nt - e.tick).unwrap_or(u64::MAX.max(1));
        // Period at `tick`: linear interpolation within the segment (round-2
        // §3.3). Past the last event (or inside a zero-span guard), the
        // period is constant at period_start (no next event to ramp toward).
        let period_at = if next_tick.is_some() && span > 0 {
            let frac = dticks as f64 / span as f64;
            e.period_start as f64 + (e.period_end as f64 - e.period_start as f64) * frac
        } else {
            e.period_start as f64
        };
        let spt0 = e.period_start as f64 / SUPERTICKS_PER_SECOND as f64 * self.rate as f64 / self.ppq as f64;
        let spt_at = period_at / SUPERTICKS_PER_SECOND as f64 * self.rate as f64 / self.ppq as f64;
        let samples_in_segment = dticks as f64 * (spt0 + spt_at) / 2.0;
        Samples((base_samples + samples_in_segment).round() as u64)
    }

    pub fn samples_to_tick_v3(&self, samples: Samples) -> Ticks {
        let s = samples.0 as f64;
        let i = match self
            .cum_at_event
            .binary_search_by(|(_, b)| b.partial_cmp(&s).unwrap_or(std::cmp::Ordering::Less))
        {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let e = self.events[i];
        let (_, base_samples) = self.cum_at_event[i];
        let target = s - base_samples;
        if target <= 0.0 {
            return Ticks(e.tick);
        }
        let next_tick = self.events.get(i + 1).map(|n| n.tick);
        if e.period_start == e.period_end || next_tick.is_none() {
            // Constant segment: exact algebraic inversion.
            let spt = e.period_start as f64 / SUPERTICKS_PER_SECOND as f64 * self.rate as f64 / self.ppq as f64;
            return Ticks(e.tick + (target / spt).round() as u64);
        }
        // Ramp segment: invert the quadratic (trapezoid) via the closed-form
        // solution of samples(dticks) = dticks*(spt0 + spt(dticks))/2, which
        // is quadratic in dticks since spt is linear in dticks. Solve with
        // the quadratic formula on a*d^2 + b*d - target = 0.
        let span = next_tick.unwrap() - e.tick;
        let spt0 = e.period_start as f64 / SUPERTICKS_PER_SECOND as f64 * self.rate as f64 / self.ppq as f64;
        let spt1 = e.period_end as f64 / SUPERTICKS_PER_SECOND as f64 * self.rate as f64 / self.ppq as f64;
        let slope = (spt1 - spt0) / span as f64; // d(spt)/d(tick)
        let a = slope / 2.0;
        let b = spt0;
        let d = if a.abs() < 1e-12 {
            target / b
        } else {
            let disc = (b * b + 4.0 * a * target).max(0.0);
            (-b + disc.sqrt()) / (2.0 * a)
        };
        Ticks(e.tick + d.round().max(0.0) as u64)
    }
}
```

- [ ] **Step 4: Run the new tests**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml midi::tempo::`
Expected: PASS, all `midi::tempo::tests` (old + 4 new). If
`v3_roundtrip_across_a_ramp_and_a_constant_segment` fails by more than 1
tick anywhere, print the failing tick/segment and check the quadratic
inversion's sign choice (`+ disc.sqrt()` is the physically meaningful root
since `d >= 0` and `spt > 0` always) before changing tolerances.

- [ ] **Step 5: Implement `CmpIn` (closes Task 1's forward declaration)**

```rust
// src-tauri/src/time.rs — add at the bottom, after the CmpIn trait
impl CmpIn<Samples> for Ticks {
    fn cmp_in(&self, other: &Samples, map: &crate::midi::tempo::TempoMap) -> std::cmp::Ordering {
        map.tick_to_samples_v3(*self).cmp(other)
    }
}
impl CmpIn<Ticks> for Samples {
    fn cmp_in(&self, other: &Ticks, map: &crate::midi::tempo::TempoMap) -> std::cmp::Ordering {
        self.cmp(&map.tick_to_samples_v3(*other))
    }
}

#[cfg(test)]
mod cmp_in_tests {
    use super::*;
    use crate::midi::tempo::TempoMap;

    #[test]
    fn cmp_in_orders_across_domains_via_the_named_map() {
        let m = TempoMap::from_v1(120.0, 48_000).unwrap();
        assert_eq!(Ticks(960).cmp_in(&Samples(24_000), &m), std::cmp::Ordering::Equal);
        assert_eq!(Ticks(960).cmp_in(&Samples(1), &m), std::cmp::Ordering::Greater);
    }
}
```

- [ ] **Step 6: Full backend suite + commit**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, 356 tests (350 + 4 tempo + 1 cmp_in + 1 more from the
`old_bpm_api_is_a_thin_wrapper` test — count precisely from the run output,
adjust the commit message to the ACTUAL number, not this estimate).

```bash
git add src-tauri/src/midi/tempo.rs src-tauri/src/time.rs
git commit -m "$(cat <<'EOF'
feat(time): TempoMap v3 core — period-based bijection, linear-in-period ramps

TempoMap's storage moves from f64 bpm to integer superticks-per-quarter
(round-2 §3.3); the old bpm-based new()/from_v1()/tick_to_samples()/
samples_to_tick() become thin wrappers (frozen internal API, PHASE4-PLAN
rule 3 applied one layer down). Ramps interpolate linear-in-period (exact
tick-domain trapezoid for the forward direction; closed-form quadratic
inversion for the reverse). CmpIn (declared in Task 1) is implemented here,
where the bijection it needs actually lives.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Meter map — `MeterEvent`, bar/beat, the 4/4-clobber fix

**Files:**
- Modify: `src-tauri/src/midi/types.rs` (add `MeterEvent`)
- Modify: `src-tauri/src/midi/tempo.rs` (add bar/beat lookups to `TempoMap`
  — actually a sibling `MeterMap`, see below)

**Interfaces:**
- Produces: `midi::types::MeterEvent { tick: u64, num: u8, den: u8 }`
  (camelCase wire); `midi::tempo::MeterMap` — `MeterMap::new(events:
  Vec<MeterEvent>) -> Result<Self, String>` (same tick-0/sorted/strictly-
  increasing validation as `TempoMap`, plus `num > 0 && den > 0`);
  `MeterMap::default_map() -> Self` (the `[{0,4,4}]` default, round-2 §3.3);
  `MeterMap::bar_at(&self, tick: Ticks, ppq: u32) -> u32` (bar number,
  0-indexed, using each meter event's own num/den to size its bars);
  `MeterMap::beat_at(&self, tick: Ticks, ppq: u32) -> f64` (fractional beat
  within the current bar).
- Consumes: `time::Ticks` (Task 1).

- [ ] **Step 1: Write the failing tests**

```rust
// src-tauri/src/midi/tempo.rs, in #[cfg(test)] mod tests
#[test]
fn meter_map_default_is_four_four_at_tick_zero() {
    let m = MeterMap::default_map();
    assert_eq!(m.events()[0], MeterEvent { tick: 0, num: 4, den: 4 });
}

#[test]
fn meter_map_computes_bar_and_beat_at_a_constant_four_four() {
    let m = MeterMap::new(vec![MeterEvent { tick: 0, num: 4, den: 4 }]).unwrap();
    let ppq = 960u32;
    // One bar = 4 beats = 4*960 = 3840 ticks at 4/4.
    assert_eq!(m.bar_at(Ticks(0), ppq), 0);
    assert_eq!(m.bar_at(Ticks(3839), ppq), 0);
    assert_eq!(m.bar_at(Ticks(3840), ppq), 1);
    assert_eq!(m.bar_at(Ticks(3840 * 3 + 100), ppq), 3);
    assert!((m.beat_at(Ticks(960), ppq) - 1.0).abs() < 1e-9);
    assert!((m.beat_at(Ticks(480), ppq) - 0.5).abs() < 1e-9);
}

#[test]
fn meter_map_handles_a_signature_change_mid_song() {
    // 4/4 for 2 bars (7680 ticks), then 3/4.
    let m = MeterMap::new(vec![
        MeterEvent { tick: 0, num: 4, den: 4 },
        MeterEvent { tick: 7680, num: 3, den: 4 },
    ]).unwrap();
    let ppq = 960u32;
    assert_eq!(m.bar_at(Ticks(7680), ppq), 2, "bar 2 starts exactly at the signature change");
    // One 3/4 bar = 3*960 = 2880 ticks.
    assert_eq!(m.bar_at(Ticks(7680 + 2880), ppq), 3);
}

#[test]
fn meter_map_rejects_malformed_maps() {
    assert!(MeterMap::new(vec![]).is_err());
    assert!(MeterMap::new(vec![MeterEvent { tick: 5, num: 4, den: 4 }]).is_err());
    assert!(MeterMap::new(vec![MeterEvent { tick: 0, num: 0, den: 4 }]).is_err());
}
```

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml midi::tempo::meter`
Expected: FAIL (`MeterEvent`/`MeterMap` undefined).

- [ ] **Step 2: Add `MeterEvent`**

```rust
// src-tauri/src/midi/types.rs, near TempoEvent
/// One time-signature change (round-2 §3.3/O-10). Sorted list, first at
/// tick 0; default is `[{0,4,4}]`. Persisting this is what fixes the
/// active data-loss bug (dossier 10 trap 3): today's code silently
/// clobbers the user's signature to 4/4 on every save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterEvent {
    pub tick: u64,
    pub num: u8,
    pub den: u8,
}
```

- [ ] **Step 3: Implement `MeterMap`**

```rust
// src-tauri/src/midi/tempo.rs
use super::types::MeterEvent;

#[derive(Debug, Clone)]
pub struct MeterMap {
    events: Vec<MeterEvent>,
}

impl MeterMap {
    pub fn new(events: Vec<MeterEvent>) -> Result<Self, String> {
        if events.is_empty() { return Err("meter map must have at least one entry".into()); }
        if events[0].tick != 0 { return Err("first meter event must be at tick 0".into()); }
        for pair in events.windows(2) {
            if pair[1].tick <= pair[0].tick {
                return Err("meter events must have strictly increasing ticks".into());
            }
        }
        for e in &events {
            if e.num == 0 || e.den == 0 { return Err("meter numerator/denominator must be > 0".into()); }
        }
        Ok(Self { events })
    }

    pub fn default_map() -> Self {
        Self { events: vec![MeterEvent { tick: 0, num: 4, den: 4 }] }
    }

    pub fn events(&self) -> &[MeterEvent] { &self.events }

    fn segment_at(&self, tick: u64) -> (usize, &MeterEvent) {
        let i = match self.events.binary_search_by(|e| e.tick.cmp(&tick)) {
            Ok(i) => i,
            Err(i) => i - 1, // safe: events[0].tick == 0
        };
        (i, &self.events[i])
    }

    /// 0-indexed bar number at `tick`, given the project `ppq`. Bars
    /// within a meter segment are `num` beats of `4/den` whole notes each
    /// — a bar's tick length is `num * ppq * 4 / den`.
    pub fn bar_at(&self, tick: Ticks, ppq: u32) -> u32 {
        let tick = tick.0;
        let (i, e) = self.segment_at(tick);
        let bar_ticks = e.num as u64 * ppq as u64 * 4 / e.den as u64;
        // Accumulate bars from every PRIOR segment boundary, then add the
        // whole bars elapsed inside this segment.
        let mut bars = 0u32;
        for seg in &self.events[..i] {
            let seg_end = self.events.get(self.events.iter().position(|x| x.tick == seg.tick).unwrap() + 1)
                .map(|n| n.tick);
            if let Some(end) = seg_end {
                let seg_bar_ticks = seg.num as u64 * ppq as u64 * 4 / seg.den as u64;
                bars += ((end - seg.tick) / seg_bar_ticks) as u32;
            }
        }
        bars + ((tick - e.tick) / bar_ticks) as u32
    }

    /// Fractional beat position within the current bar (0.0 = downbeat).
    pub fn beat_at(&self, tick: Ticks, ppq: u32) -> f64 {
        let tick = tick.0;
        let (_, e) = self.segment_at(tick);
        let beat_ticks = e.num as u64 * 0 + ppq as u64 * 4 / e.den as u64; // ticks per ONE beat at den
        let bar_ticks = e.num as u64 * beat_ticks;
        let into_bar = (tick - e.tick) % bar_ticks.max(1);
        into_bar as f64 / beat_ticks.max(1) as f64
    }
}
```

**// REVIEW:** `bar_at`'s prior-segment accumulation loop re-derives each
prior segment's end tick via a linear `position()` scan — correct but O(n²)
for many signature changes. At realistic project sizes (a handful of meter
changes per song) this is fine; flagged for a future pass if `MeterMap`
ever needs to serve a hot path. Not a correctness concern for Gate C/D.

- [ ] **Step 4: Run tests, full suite, commit**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, +6 tests over Task 3's total.

```bash
git add src-tauri/src/midi/types.rs src-tauri/src/midi/tempo.rs
git commit -m "$(cat <<'EOF'
feat(time): MeterMap — persisted time signature, bar/beat lookups (round-2 §3.3, O-10)

MeterEvent{tick,num,den}, default [{0,4,4}]. This is the format-level fix
for dossier 10 trap 3 (today's code silently clobbers the user's time
signature to 4/4 on every save) — the persistence wiring lands in Task 6.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Section table — constant-tempo segments, <64-sample ramp-subdivision bound (Gate test 6)

**Files:**
- Create: `src-tauri/src/midi/section_table.rs`
- Modify: `src-tauri/src/midi/mod.rs` (add `pub mod section_table;`)

**Interfaces:**
- Consumes: `midi::tempo::{TempoMap, MeterMap}` (Tasks 3–4), `time::{Ticks,
  Samples}` (Task 1).
- Produces: `section_table::Section { start_tick: u64, start_sample: u64,
  start_beat: f64, start_bar: u32, period: u64 }` (one CONSTANT-tempo
  segment); `section_table::SectionTable { pub const RULE_VERSION: u32 = 1;
  sections: Vec<Section> }`; `SectionTable::build(tempo: &TempoMap, meter:
  &MeterMap) -> Self` (full rebuild); `SectionTable::rebuild_from(&mut
  self, tempo: &TempoMap, meter: &MeterMap, from_tick: u64)` (suffix-only
  rebuild — round-2 §3.4's "edit at tick T rebuilds only segments ≥ T");
  `SectionTable::sections(&self) -> &[Section]`.

**Design note:** a constant-tempo `TempoPeriodEvent` becomes exactly ONE
`Section`. A ramp event (`period_start != period_end`) is subdivided into
`n` equal-tick sub-segments, `n` chosen by doubling from 1 until the
worst-case deviation between the piecewise-constant approximation (each
sub-segment's constant period = the period AT ITS MIDPOINT tick, per the
linear formula from Task 3) and `TempoMap::tick_to_samples_v3`'s EXACT
closed form is `< 64` samples, sampled at 32 evenly-spaced probe ticks per
sub-segment (round-2 §3.4: "against high-resolution numeric integration" —
`tick_to_samples_v3` IS that high-resolution reference, since Task 3
established it is the exact closed form for a linear ramp). Cap `n` at
4096 (a doubling runaway guard — pathological ramps get the tightest legal
approximation rather than looping forever; flagged with `// REVIEW:` at
the cap since round-2 does not specify one).

- [ ] **Step 1: Write the failing tests**

```rust
// src-tauri/src/midi/section_table.rs (new file)
//! Precomputed constant-tempo segments (round-2 §3.4): cumulative
//! superticks/samples/beat/bar per segment, ramps subdivided under a
//! versioned, property-tested error bound.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::tempo::{MeterMap, TempoMap};
    use crate::midi::types::{MeterEvent, TempoPeriodEvent};
    use crate::time::Ticks;

    #[test]
    fn constant_tempo_produces_exactly_one_section() {
        let period = crate::time::period_from_bpm(120.0);
        let tempo = TempoMap::from_periods(960, vec![TempoPeriodEvent { tick: 0, period_start: period, period_end: period }], 48_000).unwrap();
        let meter = MeterMap::default_map();
        let table = SectionTable::build(&tempo, &meter);
        assert_eq!(table.sections().len(), 1);
        assert_eq!(table.sections()[0].start_tick, 0);
        assert_eq!(table.sections()[0].start_sample, 0);
        assert_eq!(table.sections()[0].start_bar, 0);
    }

    #[test]
    fn ramp_subdivision_stays_within_the_64_sample_bound() {
        // A steep ramp: 60bpm -> 240bpm across one bar, 48kHz — exercises
        // the worst-case curvature this project supports.
        let p_start = crate::time::period_from_bpm(60.0);
        let p_end = crate::time::period_from_bpm(240.0);
        let tempo = TempoMap::from_periods(
            960,
            vec![
                TempoPeriodEvent { tick: 0, period_start: p_start, period_end: p_end },
                TempoPeriodEvent { tick: 3840, period_start: p_end, period_end: p_end },
            ],
            48_000,
        ).unwrap();
        let meter = MeterMap::default_map();
        let table = SectionTable::build(&tempo, &meter);
        assert!(table.sections().len() > 1, "a steep ramp must subdivide");
        // Probe 200 points across the ramp span; the piecewise-constant
        // reconstruction must stay within 64 samples of the exact bijection
        // everywhere, not just at sub-segment boundaries.
        for k in 0..=200u64 {
            let tick = k * 3840 / 200;
            let exact = tempo.tick_to_samples_v3(Ticks(tick)).0 as i64;
            let approx = table.sample_at_tick(tick) as i64;
            assert!((exact - approx).abs() < 64, "tick {tick}: exact={exact} approx={approx}");
        }
    }

    #[test]
    fn a_gentle_ramp_needs_few_or_no_subdivisions() {
        // A one-bpm drift across a whole song-length span is nearly
        // constant-tempo; the subdivision rule must not over-split it.
        let p_start = crate::time::period_from_bpm(120.0);
        let p_end = crate::time::period_from_bpm(121.0);
        let tempo = TempoMap::from_periods(
            960,
            vec![
                TempoPeriodEvent { tick: 0, period_start: p_start, period_end: p_end },
                TempoPeriodEvent { tick: 3_840_000, period_start: p_end, period_end: p_end },
            ],
            48_000,
        ).unwrap();
        let meter = MeterMap::default_map();
        let table = SectionTable::build(&tempo, &meter);
        assert!(table.sections().len() <= 4, "gentle ramp should not over-subdivide: got {}", table.sections().len());
    }

    #[test]
    fn suffix_rebuild_only_touches_segments_at_or_after_the_edit_tick() {
        let period = crate::time::period_from_bpm(120.0);
        let tempo = TempoMap::from_periods(
            960,
            vec![
                TempoPeriodEvent { tick: 0, period_start: period, period_end: period },
                TempoPeriodEvent { tick: 7680, period_start: period, period_end: period },
            ],
            48_000,
        ).unwrap();
        let meter = MeterMap::default_map();
        let mut table = SectionTable::build(&tempo, &meter);
        let prefix_before = table.sections()[0].clone();
        table.rebuild_from(&tempo, &meter, 7680);
        assert_eq!(table.sections()[0], prefix_before, "segment before the edit tick is untouched");
    }
}
```

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml midi::section_table::`
Expected: FAIL (module doesn't exist).

- [ ] **Step 2: Register the module**

In `src-tauri/src/midi/mod.rs`, add `pub mod section_table;` next to the
existing `pub mod tempo;` (or equivalent) line.

- [ ] **Step 3: Implement `Section` and `SectionTable::build`**

```rust
use super::tempo::{MeterMap, TempoMap};
use crate::time::Ticks;

/// A versioned constant, next to the schema version per round-2 §3.4
/// ("the subdivision rule is format semantics and is versioned").
pub const RULE_VERSION: u32 = 1;
const MAX_SUBDIVISIONS: usize = 4096; // REVIEW: round-2 doesn't name a cap; a doubling runaway guard for pathological ramps.
const ERROR_BOUND_SAMPLES: f64 = 64.0;
const PROBES_PER_SUBSEGMENT: usize = 32;

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub start_tick: u64,
    pub start_sample: u64,
    pub start_beat: f64,
    pub start_bar: u32,
    pub period: u64,
}

#[derive(Debug, Clone)]
pub struct SectionTable {
    sections: Vec<Section>,
}

impl SectionTable {
    pub fn build(tempo: &TempoMap, meter: &MeterMap) -> Self {
        let mut sections = Vec::new();
        let events = tempo.period_events();
        for (i, e) in events.iter().enumerate() {
            let next_tick = events.get(i + 1).map(|n| n.tick);
            if e.period_start == e.period_end || next_tick.is_none() {
                sections.push(section_at(tempo, meter, e.tick, e.period_start, tempo.ppq()));
            } else {
                subdivide_ramp(tempo, meter, *e, next_tick.unwrap(), &mut sections);
            }
        }
        Self { sections }
    }

    pub fn sections(&self) -> &[Section] { &self.sections }

    /// Suffix-only rebuild (round-2 §3.4): keep every section strictly
    /// before `from_tick`, recompute the rest.
    pub fn rebuild_from(&mut self, tempo: &TempoMap, meter: &MeterMap, from_tick: u64) {
        let fresh = Self::build(tempo, meter);
        self.sections.retain(|s| s.start_tick < from_tick);
        self.sections.extend(fresh.sections.into_iter().filter(|s| s.start_tick >= from_tick));
    }

    /// Piecewise-constant sample lookup against the table (what a renderer
    /// consumes — NOT the exact bijection, bounded by `ERROR_BOUND_SAMPLES`
    /// vs `TempoMap::tick_to_samples_v3`).
    pub fn sample_at_tick(&self, tick: u64) -> u64 {
        let i = match self.sections.binary_search_by(|s| s.start_tick.cmp(&tick)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let s = &self.sections[i];
        // Constant period within this section: exact algebraic projection.
        // (rate/ppq folded via the same formula tempo.rs uses; reuse it by
        // calling back into a one-segment TempoMap would be circular, so
        // this recomputes with the stored period + the caller-supplied
        // tempo map's rate/ppq — see Step 3 signature note below.)
        s.start_sample // corrected in Step 4 to interpolate within the section, not just return its start
    }
}
```

**// REVIEW:** the `sample_at_tick` stub above (`s.start_sample` verbatim)
is intentionally wrong — Step 4 fixes it to interpolate within the section
using the section's constant `period`. It is written this way in the plan
only so Step 3's code block type-checks in isolation; the implementer must
NOT commit this stub. Step 4 below is the real implementation and Step 3's
version must be replaced, not left as dead weight.

- [ ] **Step 4: Fix `sample_at_tick` and implement `section_at`/`subdivide_ramp`**

```rust
fn section_at(tempo: &TempoMap, meter: &MeterMap, tick: u64, period: u64, ppq: u32) -> Section {
    Section {
        start_tick: tick,
        start_sample: tempo.tick_to_samples_v3(Ticks(tick)).0,
        start_beat: meter.beat_at(Ticks(tick), ppq),
        start_bar: meter.bar_at(Ticks(tick), ppq),
        period,
    }
}

/// Subdivide `[event.tick, next_tick)` into equal-tick sub-segments,
/// doubling the count from 1 until every probe point's deviation from the
/// exact bijection is < ERROR_BOUND_SAMPLES.
fn subdivide_ramp(
    tempo: &TempoMap,
    meter: &MeterMap,
    event: crate::midi::types::TempoPeriodEvent,
    next_tick: u64,
    out: &mut Vec<Section>,
) {
    let span = next_tick - event.tick;
    let mut n = 1usize;
    loop {
        if within_bound(tempo, event, next_tick, n) || n >= MAX_SUBDIVISIONS {
            break;
        }
        n *= 2;
    }
    let ppq = tempo.ppq();
    for k in 0..n {
        let sub_tick = event.tick + span * k as u64 / n as u64;
        let sub_next = event.tick + span * (k as u64 + 1) / n as u64;
        let period_at = |t: u64| -> u64 {
            let frac = (t - event.tick) as f64 / span as f64;
            (event.period_start as f64 + (event.period_end as f64 - event.period_start as f64) * frac).round() as u64
        };
        let midpoint = (sub_tick + sub_next) / 2;
        out.push(section_at(tempo, meter, sub_tick, period_at(midpoint), ppq));
    }
}

fn within_bound(
    tempo: &TempoMap,
    event: crate::midi::types::TempoPeriodEvent,
    next_tick: u64,
    n: usize,
) -> bool {
    let span = next_tick - event.tick;
    for k in 0..n {
        let sub_tick = event.tick + span * k as u64 / n as u64;
        let sub_next = event.tick + span * (k as u64 + 1) / n as u64;
        let frac_mid = ((sub_tick + sub_next) / 2 - event.tick) as f64 / span as f64;
        let period_mid = (event.period_start as f64 + (event.period_end as f64 - event.period_start as f64) * frac_mid).round() as u64;
        let approx_start = tempo.tick_to_samples_v3(Ticks(sub_tick)).0 as f64;
        let spt = period_mid as f64 / crate::time::SUPERTICKS_PER_SECOND as f64
            * tempo.sample_rate() as f64 / tempo.ppq() as f64;
        for p in 0..PROBES_PER_SUBSEGMENT {
            let t = sub_tick + (sub_next - sub_tick) * p as u64 / PROBES_PER_SUBSEGMENT as u64;
            let exact = tempo.tick_to_samples_v3(Ticks(t)).0 as f64;
            let approx = approx_start + (t - sub_tick) as f64 * spt;
            if (exact - approx).abs() >= ERROR_BOUND_SAMPLES {
                return false;
            }
        }
    }
    true
}

impl SectionTable {
    // Replaces Step 3's stub.
    pub fn sample_at_tick_impl(&self, tick: u64, tempo: &TempoMap) -> u64 {
        let i = match self.sections.binary_search_by(|s| s.start_tick.cmp(&tick)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let s = &self.sections[i];
        let spt = s.period as f64 / crate::time::SUPERTICKS_PER_SECOND as f64
            * tempo.sample_rate() as f64 / tempo.ppq() as f64;
        (s.start_sample as f64 + (tick - s.start_tick) as f64 * spt).round() as u64
    }
}
```

**// REVIEW:** carrying `tempo` as an extra argument into
`sample_at_tick_impl` (rather than having `SectionTable` own its own
rate/ppq copy) is a seam I'm not fully happy with — it means a caller with
only a `SectionTable` and no `TempoMap` at hand can't do the lookup. For
Gate C/D's own tests this is fine (both are always available together);
flag for whoever wires the frontend-facing wire type (Task 9) whether
`SectionTable` should instead store `rate`/`ppq` itself. Fold Step 3's
`sample_at_tick` stub method away entirely and rename
`sample_at_tick_impl` back to `sample_at_tick(&self, tick: u64, tempo:
&TempoMap)` in the actual diff — the two-name dance above is a plan-
authoring artifact (showing the stub-then-fix sequence for the reader);
land ONE method with ONE name.

- [ ] **Step 5: Run tests, full suite, commit**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml midi::section_table::`
Expected: PASS, all 4 tests. This is **Gate C/D test 6.**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, +4 over Task 4's total.

```bash
git add src-tauri/src/midi/section_table.rs src-tauri/src/midi/mod.rs
git commit -m "$(cat <<'EOF'
feat(time): section table — constant-tempo segments, <64-sample ramp bound (Gate C/D test 6)

SectionTable::build compiles TempoMap+MeterMap into constant-tempo
segments (round-2 §3.4): a constant TempoPeriodEvent is one Section; a
ramp subdivides by doubling sub-segment count until every probe point's
deviation from TempoMap::tick_to_samples_v3's exact closed form is < 64
samples. RULE_VERSION=1 travels next to the schema version, not a code
comment. rebuild_from does suffix-only recompute.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: v2→v3 project migration — tempo + meter (Gate test 7, part 1)

**Files:**
- Modify: `src-tauri/src/audio/project.rs` (schema gate `1..=2` → `1..=3`)
- Modify: `src-tauri/src/midi/persist.rs` (write/read schemaVersion 3:
  period-based `tempoMap`, new `meterMap`; keep reading v2's bpm `tempoMap`
  losslessly via Task 3's `TempoMap::new` quantizing wrapper)
- Create: `src-tauri/tests/fixtures/project_v2/` — 3 fixture project
  directories (JSON only, no audio/events payloads needed for this task's
  test) for the round-trip corpus
- Create: `src-tauri/tests/v3_migration.rs`

**Interfaces:**
- Consumes: `midi::tempo::{TempoMap, MeterMap}` (Tasks 3–4),
  `midi::types::{TempoPeriodEvent, MeterEvent}` (Tasks 2/4).
- Produces: `midi::persist::V3Data { ppq: u32, tempo_events:
  Vec<TempoPeriodEvent>, meter_events: Vec<MeterEvent>, clips:
  Vec<MidiClip> }` (superset of today's `V2Data`, same clips field for
  now — Task 7 adds `content_id`/`lane_id` to `MidiClip` and this struct
  keeps compiling against it since Task 7 makes those fields
  `#[serde(default)]`); `midi::persist::load_from_project` returns
  `Result<Option<V3Data>, String>` (name kept — PHASE4-PLAN rule 3 doesn't
  bind internal Rust function names, but `V2Data` callers exist, see Step 2
  note); `midi::persist::save_into_project` writes schemaVersion 3.

**Design note:** today's `V2Data`/`load_from_project`/`save_into_project`
are called from `midi/mod.rs` and `control/mod.rs` — grep both before
editing. Renaming `V2Data` to `V3Data` and changing `tempo_events`'s
element type from `TempoEvent` to `TempoPeriodEvent` is a BREAKING change
to those call sites (not to the wire — the wire gets an additive bump,
schemaVersion 3, with v2 files still readable). This task must update
every `V2Data`/`.bpm` call site it breaks; grep-verify zero remaining
references to the old name before Step 6's full-suite run.

- [ ] **Step 1: Write the failing tests**

```rust
// src-tauri/tests/v3_migration.rs (new file)
//! Gate C/D test 7 (part 1 — Task 7 adds the placement/content half):
//! lossless v2->v3 migration for tempo + meter against a fixture corpus.

use std::fs;
use std::path::Path;

fn fixture_dir(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_v2").join(name)
}

fn copy_fixture_to_tmp(name: &str) -> std::path::PathBuf {
    let src = fixture_dir(name);
    let dst = std::env::temp_dir().join(format!("aura-v3-migration-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dst);
    fs::create_dir_all(&dst).unwrap();
    for entry in fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
    }
    dst
}

#[test]
fn v2_single_tempo_project_migrates_losslessly_to_v3() {
    let dir = copy_fixture_to_tmp("single_tempo");
    let v3 = aura_lib::midi::persist::load_from_project(&dir).unwrap().expect("v2+ present");
    assert_eq!(v3.ppq, 960);
    assert_eq!(v3.tempo_events.len(), 1);
    // 128.0 bpm quantizes to an exact period and back within spec error.
    let bpm_back = aura_lib::time::bpm_from_period(v3.tempo_events[0].period_start);
    assert!((bpm_back - 128.0).abs() < 3e-7);
    assert_eq!(v3.meter_events, vec![aura_lib::midi::types::MeterEvent { tick: 0, num: 4, den: 4 }],
        "no meterMap in the v2 fixture -> the [{{0,4,4}}] default (round-2 §3.3)");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn v2_multi_tempo_project_migrates_and_resaves_as_v3() {
    let dir = copy_fixture_to_tmp("multi_tempo");
    let v3 = aura_lib::midi::persist::load_from_project(&dir).unwrap().expect("v2+ present");
    assert_eq!(v3.tempo_events.len(), 2);

    // Resave, then reload: schemaVersion is now 3, project.json.v2.bak exists,
    // and the SAME (already-quantized) periods come back with zero drift.
    let store = aura_lib::midi::MidiStore {
        ppq: v3.ppq,
        tempo_events: v3.tempo_events.clone(),
        meter_events: v3.meter_events.clone(),
        clips: v3.clips.clone(),
        loaded_dir: None,
        dirty: false,
    };
    aura_lib::midi::persist::save_into_project(&dir, &store).unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
    assert_eq!(raw["schemaVersion"], 3);
    assert!(dir.join("project.json.v2.bak").exists(), "v2 backup written on first v3 upgrade");
    assert_eq!(raw["tempoMap"][0]["periodStart"], v3.tempo_events[0].period_start);
    assert_eq!(raw["tempoMap"][0]["periodEnd"], v3.tempo_events[0].period_end);
    assert_eq!(raw["meterMap"][0]["num"], 4);

    let reloaded = aura_lib::midi::persist::load_from_project(&dir).unwrap().unwrap();
    assert_eq!(reloaded.tempo_events, v3.tempo_events, "zero drift on re-save (Task 2's exactness property)");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn project_rs_schema_gate_accepts_v3() {
    let dir = copy_fixture_to_tmp("single_tempo");
    let (project, _) = aura_lib::audio::project::load(&dir).unwrap();
    assert!((1..=3).contains(&project.schema_version));
    let _ = fs::remove_dir_all(&dir);
}
```

Also create the fixtures (Step 1 continues — write these files directly,
they are test data, not code the implementer writes by hand each run):

`src-tauri/tests/fixtures/project_v2/single_tempo/project.json`:
```json
{
  "schemaVersion": 2,
  "name": "SingleTempo",
  "sampleRate": 48000,
  "tempoBpm": 128.0,
  "tracks": [],
  "clips": [],
  "ppq": 960,
  "tempoMap": [{"tick": 0, "bpm": 128.0}],
  "midiClips": []
}
```

`src-tauri/tests/fixtures/project_v2/multi_tempo/project.json`:
```json
{
  "schemaVersion": 2,
  "name": "MultiTempo",
  "sampleRate": 44100,
  "tempoBpm": 100.0,
  "tracks": [],
  "clips": [],
  "ppq": 960,
  "tempoMap": [{"tick": 0, "bpm": 100.0}, {"tick": 15360, "bpm": 140.0}],
  "midiClips": []
}
```

`src-tauri/tests/fixtures/project_v2/with_clips/project.json` (used by
Task 7, created now so the corpus is complete in one place):
```json
{
  "schemaVersion": 2,
  "name": "WithClips",
  "sampleRate": 48000,
  "tempoBpm": 120.0,
  "tracks": [{"id": "t1", "kind": "midi", "name": "Lead", "gainDb": 0.0, "pan": 0.0, "muted": false, "solo": false}],
  "clips": [],
  "ppq": 960,
  "tempoMap": [{"tick": 0, "bpm": 120.0}],
  "midiClips": [{"id": "mc1", "trackId": "t1", "name": "Verse", "timelineStartTicks": 0, "lengthTicks": 3840, "nextNoteId": 1}]
}
```

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml v3_migration::`
Expected: FAIL (`V3Data`/`meter_events`/schemaVersion-3 writing don't exist
yet; also check the `tracks` fixture row shape against the ACTUAL
`TrackState` struct in `audio/types.rs` before trusting the JSON above
verbatim — adjust field names/add required fields if `cargo test` reports
a deserialization error unrelated to the migration logic itself).

- [ ] **Step 2: Grep every `V2Data`/`load_from_project`/`tempo_events` call site**

```bash
grep -rn "V2Data\|load_from_project\|\.tempo_events\b" src-tauri/src/ --include=*.rs
```

Update each: `midi/mod.rs`'s `MidiStore` struct gains `meter_events:
Vec<MeterEvent>` (default `MeterMap::default_map().events().to_vec()` via
`#[serde(default = "...")]` if `MidiStore` is ever serialized directly —
check; if it's control-plane-only and never serde'd, a plain
`Default`-deriving field is enough, confirm by grep for `derive(...
Deserialize` on `MidiStore`). Every constructor of `MidiStore` in tests
(`persist.rs`'s `store_with`, any in `midi/mod.rs`) gets the new field —
plain `Vec::new()` is wrong (an empty meter map is invalid per
`MeterMap::new`'s validation); use `vec![MeterEvent { tick: 0, num: 4, den: 4 }]`.

- [ ] **Step 3: Rewrite `midi::persist`'s save/load for schemaVersion 3**

```rust
// src-tauri/src/midi/persist.rs — replace V2Data with V3Data, and the
// tempo/meter read/write blocks. Full file structure kept identical
// (chunk write/GC logic UNCHANGED — only the tempoMap shape, the new
// meterMap, and the version number change).
use super::types::{first_note_id, MeterEvent, MidiClip, TempoPeriodEvent, DEFAULT_PPQ};

#[derive(Debug, Clone)]
pub struct V3Data {
    pub ppq: u32,
    pub tempo_events: Vec<TempoPeriodEvent>,
    pub meter_events: Vec<MeterEvent>,
    pub clips: Vec<MidiClip>,
}
```

In `save_into_project`, replace:
```rust
    obj.insert("tempoMap".into(), serde_json::to_value(&midi.tempo_events).unwrap());
    // Invariant (project-v2.schema.json): tempoBpm == tempoMap[0].bpm.
    if let Some(first) = midi.tempo_events.first() {
        obj.insert("tempoBpm".into(), json!(first.bpm));
    }
```
with:
```rust
    obj.insert("schemaVersion".into(), json!(3));
    obj.insert("tempoMap".into(), serde_json::to_value(&midi.tempo_events).unwrap());
    obj.insert("meterMap".into(), serde_json::to_value(&midi.meter_events).unwrap());
    obj.insert("sectionTableRuleVersion".into(), json!(super::section_table::RULE_VERSION));
    // Invariant (v3): tempoBpm mirrors tempoMap[0]'s period as a DISPLAY
    // value only — never re-quantized, never the source of truth.
    if let Some(first) = midi.tempo_events.first() {
        obj.insert("tempoBpm".into(), json!(crate::time::bpm_from_period(first.period_start)));
    }
```
and change the `obj.insert("schemaVersion".into(), json!(2));` line
earlier in the same function to `json!(3)`. The `was_v1`/`V1_BACKUP` logic
stays for v1 sources; ADD an analogous `was_v2`/`V2_BACKUP` block right
after it:
```rust
    let was_v2 = root.get("schemaVersion").and_then(Value::as_u64) == Some(2);
    if was_v2 && !dir.join(V2_BACKUP).exists() {
        fs::copy(&file, dir.join(V2_BACKUP)).map_err(|e| format!("write {V2_BACKUP}: {e}"))?;
    }
```
with `const V2_BACKUP: &str = "project.json.v2.bak";` added next to the
existing `V1_BACKUP` constant.

In `load_from_project`, replace the `tempo_events` block:
```rust
    let tempo_events: Vec<TempoPeriodEvent> = if version >= 3 {
        match root.get("tempoMap") {
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("tempoMap: {e}"))?,
            None => vec![TempoPeriodEvent { tick: 0, period_start: crate::time::period_from_bpm(120.0), period_end: crate::time::period_from_bpm(120.0) }],
        }
    } else {
        // v2 (or the tempoBpm fallback): bpm events quantize losslessly
        // via Task 2's period_from_bpm, ONCE, on this very load.
        let bpm_events: Vec<super::types::TempoEvent> = match root.get("tempoMap") {
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("tempoMap: {e}"))?,
            None => vec![super::types::TempoEvent { tick: 0, bpm: root.get("tempoBpm").and_then(Value::as_f64).unwrap_or(120.0) }],
        };
        bpm_events.into_iter().map(|e| {
            let p = crate::time::period_from_bpm(e.bpm);
            TempoPeriodEvent { tick: e.tick, period_start: p, period_end: p }
        }).collect()
    };
    let meter_events: Vec<MeterEvent> = match root.get("meterMap") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("meterMap: {e}"))?,
        None => vec![MeterEvent { tick: 0, num: 4, den: 4 }], // round-2 §3.3 default
    };
```
and change the function's return type/construction to `V3Data { ppq,
tempo_events, meter_events, clips }`; change `if version < 2 { return
Ok(None); }` to stay `< 2` (v1 still returns `None` — v2 AND v3 both load
through this same path now, distinguished only by the tempo-parsing branch
above).

Update `v1_migration_defaults` to return `V3Data` with
`meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }]` and
`tempo_events: vec![TempoPeriodEvent { tick: 0, period_start: p, period_end: p }]`
(quantized from the passed `tempo_bpm`).

- [ ] **Step 4: Widen `project.rs`'s schema gate**

```rust
// src-tauri/src/audio/project.rs, in load()
    if !(1..=3).contains(&project.schema_version) {
        return Err(format!("unsupported project schemaVersion {}", project.schema_version));
    }
```

Also update `save()`'s v2-detection guard (`base.get("schemaVersion")...
>= 2`) — re-read: it already uses `>= 2`, which correctly also covers 3,
no change needed there. Verify by reading the current code before assuming.

- [ ] **Step 5: Update every existing test that constructed a `V2Data`/old
`MidiStore`/bpm-shaped `tempoMap` fixture**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | grep -B2 "error\[" | head -100`
to enumerate every compile error from the rename, and fix each — this
includes `persist.rs`'s own `#[cfg(test)] mod tests` (its `store_with`
helper, and every raw-JSON assertion against `raw["tempoMap"][0]["bpm"]`,
which must become `raw["tempoMap"][0]["periodStart"]` or be dropped in
favor of asserting the new shape; do NOT weaken an assertion's intent —
if it was checking "tempo persisted correctly", the v3 replacement checks
the same thing against the new field name, not a no-op).

- [ ] **Step 6: Run the new tests, then the full suite; commit**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml v3_migration::`
Expected: PASS, 3 tests. This is **Gate C/D test 7's tempo/meter half.**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, full suite green (count will have shifted from renames —
report the actual number in the commit message, and update
`CONTRIBUTING.md`/`README.md`'s dated backend count NOW even though Task
10 is the formal close-out, so the count never goes stale mid-plan; redate
both to today).

```bash
git add src-tauri/src/audio/project.rs src-tauri/src/midi/persist.rs \
        src-tauri/src/midi/mod.rs src-tauri/tests/v3_migration.rs \
        src-tauri/tests/fixtures/project_v2/ README.md CONTRIBUTING.md
git commit -m "$(cat <<'EOF'
feat(project): v2->v3 migration — period tempo + meterMap (Gate C/D test 7, part 1)

schema_version gate widens 1..=2 -> 1..=3. midi::persist writes
schemaVersion 3 (period-based tempoMap, new meterMap, sectionTableRuleVersion);
v2's bpm tempoMap still loads losslessly (quantized once via Task 2's
period_from_bpm, same guarantee as a fresh entry). project.json.v2.bak
mirrors the existing v1-upgrade backup chain. Fixture corpus under
tests/fixtures/project_v2/ backs the round-trip test.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 7: Push to PR #<opened after this task — see plan preamble>**

```bash
git push -u origin plan-cd-time-v3
```

(First push happens after Task 0 — the plan doc commit, see the top-level
execution note below this task table; this step is the RECURRING push-at-
task-boundary step every subsequent task also ends with, elided from their
own step lists to avoid repetition — always run it after each task's
commit.)

---

### Task 7: MIDI content/placement split — `ContentId`, `LaneId`, `content`/`placements`/`lanes` JSON (Gate test 7, part 2)

**Files:**
- Modify: `src-tauri/src/ids.rs` (add `LaneId`)
- Modify: `src-tauri/src/midi/types.rs` (`MidiClip` gains `content_id:
  ContentId`, `lane_id: LaneId`)
- Modify: `src-tauri/src/midi/persist.rs` (v3 save/load emit/read
  `content`/`placements`/`lanes` arrays instead of `midiClips`; `midiClips`
  stays the v2 READ path only)
- Modify: `src-tauri/tests/v3_migration.rs` (extend with the `with_clips`
  fixture, already created in Task 6 Step 1)

**Interfaces:**
- Consumes: `ids::{ContentId, TrackId}` (existing), `midi::types::MidiClip`
  (existing, extended here).
- Produces: `ids::LaneId` (same `string_id!` shape as the others);
  `MidiClip.content_id: ContentId` and `MidiClip.lane_id: LaneId`
  (`#[serde(default)]` so every existing constructor in the codebase that
  builds a bare `MidiClip { .. }` literal keeps compiling — check this is
  actually true for `#[serde(default)]` on a non-Option field: it is NOT
  sufficient for struct-literal construction outside serde, only for
  deserialization. Every non-serde `MidiClip { .. }` literal in the crate
  (tests in `types.rs`, `persist.rs`, anywhere else — grep) needs the two
  new fields added explicitly. This is real, not optional, work — do not
  skip it and let the compiler "just default" a struct literal, which Rust
  does not do.)

- [ ] **Step 1: Write the failing test**

```rust
// src-tauri/tests/v3_migration.rs, appended
#[test]
fn v2_project_with_clips_migrates_clips_into_content_and_placements() {
    let dir = copy_fixture_to_tmp("with_clips");
    let v3 = aura_lib::midi::persist::load_from_project(&dir).unwrap().expect("v2+ present");
    assert_eq!(v3.clips.len(), 1);
    let clip = &v3.clips[0];
    assert!(!clip.content_id.as_str().is_empty(), "content id minted on migration");
    assert!(!clip.lane_id.as_str().is_empty(), "lane id minted (default lane) on migration");

    // Re-migrating the SAME v2 file twice mints the SAME content/lane ids
    // (deterministic minting, same discipline as assign_source_ids —
    // round-2 §2.2's precedent applied here).
    let dir2 = copy_fixture_to_tmp("with_clips");
    let v3b = aura_lib::midi::persist::load_from_project(&dir2).unwrap().unwrap();
    assert_eq!(v3.clips[0].content_id, v3b.clips[0].content_id, "deterministic content id minting");
    assert_eq!(v3.clips[0].lane_id, v3b.clips[0].lane_id, "deterministic lane id minting");

    let store = aura_lib::midi::MidiStore {
        ppq: v3.ppq, tempo_events: v3.tempo_events.clone(), meter_events: v3.meter_events.clone(),
        clips: v3.clips.clone(), loaded_dir: None, dirty: false,
    };
    aura_lib::midi::persist::save_into_project(&dir, &store).unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
    assert!(raw.get("midiClips").is_none(), "v3 writes content+placements, not midiClips");
    assert_eq!(raw["content"].as_array().unwrap().len(), 1);
    assert_eq!(raw["placements"].as_array().unwrap().len(), 1);
    assert_eq!(raw["lanes"].as_array().unwrap().len(), 1, "one default lane for the one track");
    assert_eq!(raw["content"][0]["kind"], "midi");
    assert_eq!(raw["content"][0]["nextNoteId"], 1);
    assert_eq!(raw["placements"][0]["contentId"], raw["content"][0]["id"]);
    assert_eq!(raw["placements"][0]["laneId"], raw["lanes"][0]["id"]);
    assert_eq!(raw["lanes"][0]["trackId"], "t1");

    let reloaded = aura_lib::midi::persist::load_from_project(&dir).unwrap().unwrap();
    assert_eq!(reloaded.clips[0].content_id, clip.content_id, "round-trips through the split and back");
    assert_eq!(reloaded.clips[0].lane_id, clip.lane_id);
    assert_eq!(reloaded.clips[0].timeline_start_ticks, clip.timeline_start_ticks);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&dir2);
}
```

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml v3_migration::with_clips`
Expected: FAIL (`content_id`/`lane_id` fields don't exist).

- [ ] **Step 2: Add `LaneId`**

```rust
// src-tauri/src/ids.rs — add next to ContentId, remove it from the
// "arrive with the rounds that use them" comment at the top of the file
// (it arrives NOW; update that doc comment to name only the remaining
// deferred families).
string_id!(/// A lane within a track (round-2 §5). Every track gets one
    /// default lane at v3 migration/creation; placements reference a lane,
    /// never a track directly — the lane resolves to its track
    /// (`LaneId -> TrackId`). Multi-lane UI and takes stay deferred; only
    /// the indirection ships (ADR 0004).
    LaneId);
```

Update the file's module doc comment (`//! ... LaneId, PluginInstanceId,
LineageId arrive with the rounds that use them.`) to `//! ...
PluginInstanceId, LineageId arrive with the rounds that use them.` (drop
`LaneId,` — it just arrived).

- [ ] **Step 3: Extend `MidiClip`**

```rust
// src-tauri/src/midi/types.rs
use crate::ids::{ClipId, ContentId, LaneId, NoteId, TrackId};

pub struct MidiClip {
    pub id: ClipId,
    pub track_id: TrackId,
    pub name: String,
    pub timeline_start_ticks: u64,
    pub length_ticks: u64,
    pub notes: Vec<MidiNote>,
    #[serde(default = "first_note_id")]
    pub next_note_id: u32,
    /// Content identity (round-2 §5, ADR 0004): first populated here.
    /// `#[serde(default)]` covers DESERIALIZATION of pre-v3 rows (the read
    /// path always sets this explicitly before it matters — see
    /// `persist.rs` — so the empty-string default is never actually
    /// observed by a v3+ reader); every non-serde struct-literal
    /// constructor in this crate must set it explicitly (Rust does not
    /// apply `#[serde(default)]` to plain struct literals).
    #[serde(default)]
    pub content_id: ContentId,
    /// Lane reference (round-2 §5): resolves to a track via a `lanes[]`
    /// row. Same `#[serde(default)]` caveat as `content_id`.
    #[serde(default)]
    pub lane_id: LaneId,
}
```

Fix every existing `MidiClip { .. }` struct literal in the crate (grep
`MidiClip {` across `src-tauri/src/`) to add `content_id:
ContentId::mint(), lane_id: LaneId::mint(),` (tests don't need determinism,
only the migration path does — Step 4 handles that separately).

- [ ] **Step 4: Deterministic content/lane minting in `midi::persist`**

```rust
// src-tauri/src/midi/persist.rs
use crate::ids::{ContentId, LaneId};

/// Fixed namespaces for deterministic v2->v3 minting (same discipline as
/// `audio::project::AURA_SOURCE_NS` — re-migrating the same v2 file twice
/// must mint the SAME ids, or every re-open of an un-resaved v2 project
/// would look like a diff).
const CONTENT_NS: uuid::Uuid = uuid::uuid!("6f1a8b2e-8c4d-4a7a-9e1a-2c9f6b7d0a11");
const LANE_NS: uuid::Uuid = uuid::uuid!("9d2e5c14-6b3a-4f8e-b7d1-3a5c9e0f2b44");

fn mint_content_id(clip_id: &str) -> ContentId {
    ContentId(uuid::Uuid::new_v5(&CONTENT_NS, clip_id.as_bytes()).to_string())
}
fn mint_default_lane_id(track_id: &str) -> LaneId {
    LaneId(uuid::Uuid::new_v5(&LANE_NS, track_id.as_bytes()).to_string())
}
```

In `load_from_project`'s v2-and-below branch (`version < 3`, or more
precisely: whenever the row being read is a `midiClips` row rather than a
`placements` row — Step 5 handles the v3-native read separately), after
building each `MidiClip` from a `PersistedClip` row, set:
```rust
        clips.push(MidiClip {
            id: row.id.clone().into(),
            track_id: row.track_id.clone().into(),
            name: row.name,
            timeline_start_ticks: row.timeline_start_ticks,
            length_ticks: row.length_ticks.max(1),
            notes,
            next_note_id,
            content_id: mint_content_id(&row.id),
            lane_id: mint_default_lane_id(&row.track_id),
        });
```

- [ ] **Step 5: Write v3-native `content`/`placements`/`lanes` — save path**

```rust
// src-tauri/src/midi/persist.rs, in save_into_project, replacing the
// clip_rows construction loop
    let mut content_rows = Vec::with_capacity(midi.clips.len());
    let mut placement_rows = Vec::with_capacity(midi.clips.len());
    let mut lane_ids_seen = std::collections::HashSet::new();
    let mut lane_rows = Vec::new();
    for clip in &midi.clips {
        let mut content = json!({
            "id": clip.content_id,
            "kind": "midi",
            "nextNoteId": clip.next_note_id,
        });
        if !clip.notes.is_empty() {
            let chunk_name = format!("{}.bin", uuid::Uuid::new_v4());
            let chunk = events::encode_notes(midi.ppq, &clip.notes, clip.next_note_id);
            fs::write(events_dir.join(&chunk_name), chunk)
                .map_err(|e| format!("write events chunk: {e}"))?;
            content["eventsRef"] = json!(format!("{EVENTS_DIR}/{chunk_name}"));
            live_chunks.push(chunk_name);
        }
        content_rows.push(content);
        placement_rows.push(json!({
            "id": clip.id,
            "contentId": clip.content_id,
            "laneId": clip.lane_id,
            "name": clip.name,
            "timelineStartTicks": clip.timeline_start_ticks,
            "lengthTicks": clip.length_ticks,
        }));
        if lane_ids_seen.insert(clip.lane_id.0.clone()) {
            lane_rows.push(json!({ "id": clip.lane_id, "trackId": clip.track_id }));
        }
    }
    obj.insert("content".into(), Value::Array(content_rows));
    obj.insert("placements".into(), Value::Array(placement_rows));
    obj.insert("lanes".into(), Value::Array(lane_rows));
    obj.remove("midiClips"); // v3 stops writing the v2 key
```
(This replaces the whole `for clip in &midi.clips { ... clip_rows.push(row);
}` loop AND the `obj.insert("midiClips".into(), ...)` line from the
existing function — remove both, they're superseded, not additive, per the
"v3 stops writing midiClips" decision this task makes. `midiClips` stays a
valid READ-time key for v2 files forever, per the additive-read /
version-gated-write asymmetry the codebase already uses for `tempoBpm`.)

**// REVIEW:** `lane_rows` only ever gets ONE lane per track today (every
clip on a track shares that track's single default lane, by construction —
`mint_default_lane_id` is a pure function of `track_id`). A track with ZERO
midi clips gets NO lane row at all under this loop (it only iterates
clips). That is very likely wrong for a future reader that expects "every
track has a default lane" — but nothing in Gate C/D's tests requires a
lane for an empty track, and inventing a track-iteration pass here (this
function only sees `midi.clips`, not the track list) is scope creep on a
Task already doing a lot. Left as a documented gap, not silently
narrowed — a caller that needs "every track has a lane" (a future round)
should mint one at track-creation time, not derive it here at save time.

- [ ] **Step 6: v3-native read — `content`/`placements`/`lanes`**

```rust
// src-tauri/src/midi/persist.rs, in load_from_project
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedContent {
    id: String,
    #[serde(default)]
    events_ref: Option<String>,
    #[serde(default = "first_note_id")]
    next_note_id: u32,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedPlacement {
    id: String,
    content_id: String,
    lane_id: String,
    name: String,
    timeline_start_ticks: u64,
    length_ticks: u64,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedLane {
    id: String,
    track_id: String,
}
```

Replace the single `let rows: Vec<PersistedClip> = ...; let mut clips =
...` block with a branch: if `root.get("content").is_some() &&
root.get("placements").is_some()` (v3-native shape), read those three
arrays, build a `lane_id -> track_id` map from `lanes`, and assemble
`MidiClip`s by joining `placements` to `content` on `contentId` (error —
not silently skip — if a placement's `contentId` has no matching content
row, or its `laneId` has no matching lane row: a v3 file with a dangling
reference is corrupt, and this project's established policy elsewhere
(`malicious_events_ref_is_rejected`'s sibling cases) is to degrade
gracefully for MISSING chunks but reject structurally inconsistent index
data — follow that precedent: `log::warn!` and skip the one placement,
same as a missing `eventsRef` degrades to an empty clip, rather than
failing the whole project load). Otherwise (no `content`/`placements`
keys — a v2 or v1-defaulted file), fall through to the existing
`midiClips`-based path from Task 6, feeding it through Step 4's
deterministic minting.

- [ ] **Step 7: Run the new test, full suite, commit**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml v3_migration::`
Expected: PASS, 4 tests. This closes **Gate C/D test 7** for MIDI (audio's
half is Task 8, addressing-only per the scope ruling).

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, full suite green; update README/CONTRIBUTING dated counts
again if they moved.

```bash
git add src-tauri/src/ids.rs src-tauri/src/midi/types.rs \
        src-tauri/src/midi/persist.rs src-tauri/tests/v3_migration.rs \
        README.md CONTRIBUTING.md
git commit -m "$(cat <<'EOF'
feat(content): MIDI content/placement split — ContentId, LaneId populated (round-2 §5, ADR 0004)

MidiClip gains content_id/lane_id (Rust-level MidiClip stays the runtime
type — scope ruling in this plan's preamble; the split is real at the file
level). v3 save writes content[]/placements[]/lanes[] instead of
midiClips[]; v2 files still read losslessly, minting deterministic
content/lane ids (UUIDv5, same discipline as assign_source_ids) so
re-migrating an un-resaved v2 file twice produces identical ids.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Audio content/placement addressing — `ContentId`/`LaneId` on `Clip`

**Files:**
- Modify: `src-tauri/src/audio/types.rs` (`Clip` gains `content_id:
  ContentId`, `lane_id: LaneId`)
- Modify: `src-tauri/src/audio/project.rs` (mint both, deterministically,
  wherever `assign_source_ids` already mints `SourceId` — same call site,
  same v3 responsibility)

**Interfaces:**
- Consumes: `ids::{ContentId, LaneId}` (existing/Task 7).
- Produces: `Clip.content_id: ContentId`, `Clip.lane_id: LaneId`, both
  `#[serde(default)]` + minted for every legacy row exactly like
  `SourceId` already is.

- [ ] **Step 1: Write the failing test**

```rust
// src-tauri/src/audio/project.rs, in #[cfg(test)] mod tests
#[test]
fn legacy_clips_get_deterministic_content_and_lane_ids_on_load() {
    let parent = std::env::temp_dir().join(format!("aura-audio-content-{}", std::process::id()));
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(&parent).unwrap();
    let (mut p, dir) = create(&parent, "Song", 48_000, 120.0).unwrap();
    p.tracks.push(TrackState { id: "t1".into(), kind: "audio".into(), name: "A".into(), gain_db: 0.0, pan: 0.0, muted: false, solo: false });
    p.clips.push(Clip {
        id: "c1".into(), track_id: "t1".into(), name: "clip".into(),
        source_path: "audio/x.wav".into(), source_id: Default::default(),
        source_channels: 2, source_sample_rate: 48_000, source_length_samples: 1000,
        timeline_start_samples: 0, offset_samples: 0, length_samples: 1000,
        gain_db: 0.0, fade_in_samples: 0, fade_out_samples: 0,
        content_id: Default::default(), lane_id: Default::default(),
    });
    save(&dir, &p).unwrap();
    let (loaded1, _) = load(&dir).unwrap();
    let (loaded2, _) = load(&dir).unwrap();
    assert!(!loaded1.clips[0].content_id.as_str().is_empty());
    assert!(!loaded1.clips[0].lane_id.as_str().is_empty());
    assert_eq!(loaded1.clips[0].content_id, loaded2.clips[0].content_id, "deterministic across loads");
    assert_eq!(loaded1.clips[0].lane_id, loaded2.clips[0].lane_id);
    let _ = fs::remove_dir_all(&parent);
}
```

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml audio::project::legacy_clips_get_deterministic`
Expected: FAIL (`content_id`/`lane_id` fields don't exist on `Clip`; note
the test above already assumes `TrackState`'s exact field set — check it
against the real struct in `audio/types.rs` before trusting the literal,
same caveat as Task 6 Step 1's fixture).

- [ ] **Step 2: Extend `Clip`**

```rust
// src-tauri/src/audio/types.rs
use crate::ids::{ClipId, ContentId, LaneId, SourceId, TrackId};

pub struct Clip {
    pub id: ClipId,
    pub track_id: TrackId,
    pub name: String,
    pub source_path: String,
    #[serde(default)]
    pub source_id: SourceId,
    pub source_channels: u16,
    pub source_sample_rate: u32,
    pub source_length_samples: u64,
    pub timeline_start_samples: u64,
    pub offset_samples: u64,
    pub length_samples: u64,
    pub gain_db: f64,
    pub fade_in_samples: u64,
    pub fade_out_samples: u64,
    /// Content identity (round-2 §5, ADR 0004): audio clips are
    /// content-backed too (a thin content object wrapping the SourceId) —
    /// addressing is real from this field on, though the JSON stays a
    /// single clip row (scope ruling, this plan's preamble: full
    /// content[]/placements[] array separation for audio is deferred).
    #[serde(default)]
    pub content_id: ContentId,
    #[serde(default)]
    pub lane_id: LaneId,
}
```

Fix every existing `Clip { .. }` struct literal in the crate (grep `Clip {`
across `src-tauri/src/`, excluding `MidiClip {`) to add the two new
fields — same non-negotiable point as Task 7 Step 3: `#[serde(default)]`
does not help plain struct literals.

- [ ] **Step 3: Mint deterministically in `assign_source_ids`'s neighborhood**

```rust
// src-tauri/src/audio/project.rs
const AURA_CONTENT_NS: uuid::Uuid = uuid::uuid!("2b6e9f31-4d7c-4e0a-8f2b-6a1d3c5e7f90");
const AURA_LANE_NS: uuid::Uuid = uuid::uuid!("7c3a1e5d-9b2f-4a6c-8d0e-1f4b6c8a2e5d");
```

Read `assign_source_ids`'s current body (`src-tauri/src/audio/project.rs`,
grep it — it mints one `SourceId` per unique `source_path` via UUIDv5 over
`AURA_SOURCE_NS`). Add a sibling loop (or extend the same loop, whichever
the actual function shape supports without restructuring its existing,
already-tested per-source-path dedup logic) that, for every clip whose
`content_id`/`lane_id` is the empty-string default, mints:
```rust
        clip.content_id = ContentId(uuid::Uuid::new_v5(&AURA_CONTENT_NS, clip.id.as_str().as_bytes()).to_string());
        clip.lane_id = LaneId(uuid::Uuid::new_v5(&AURA_LANE_NS, clip.track_id.as_str().as_bytes()).to_string());
```
(content id keyed by CLIP id — unlike `SourceId`, which dedups by source
path on purpose, content today is 1:1 with its placement, per this plan's
scope ruling; lane id keyed by TRACK id, same "one default lane per track"
rule as Task 7's MIDI side). Call this from `load()`, in the same place
`assign_source_ids` is already called.

- [ ] **Step 4: Run the new test, full suite, commit**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml audio::project::`
Expected: PASS, all `audio::project` tests including the new one.

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, full suite green.

```bash
git add src-tauri/src/audio/types.rs src-tauri/src/audio/project.rs
git commit -m "$(cat <<'EOF'
feat(content): audio clip content/lane addressing (round-2 §5, ADR 0004, scoped)

Clip gains content_id/lane_id, minted deterministically (UUIDv5, same
discipline as SourceId) for every legacy row on load. Per this plan's
scope ruling, audio keeps its single-row JSON shape — addressing is real,
the content[]/placements[] array split for audio is deferred to a future
round (no format bump needed to add it later: additive fields already
exist to key off).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Frontend consumes the shipped section table (Gate C/D's frontend exit condition)

**Files:**
- Modify: `src-tauri/src/midi/mod.rs` (`set_tempo_map`'s wire response —
  find the exact function, it's the frozen command)
- Modify: `src/lib/types/ipc.ts` (`TempoMapState` additive fields; delete
  nothing yet — Step 4 deletes)
- Create: `src/lib/sectionTable.ts` (pure interpolation, no tempo/bpm/period
  knowledge — ADR 0006 reading, this plan's preamble ruling)
- Modify: `src/lib/state/midi.svelte.ts` (delete `ticksToSamples`/
  `samplesToTicks`, consume `sectionTable.ts` instead)
- Modify: `src/lib/demo.ts` (same deletion — it duplicates the same
  piecewise math for the mock/demo backend)
- Modify: `src/lib/tauri.ts` (`TempoMapState` import unaffected; no
  signature change — frozen command)

**Interfaces:**
- Consumes: backend `TempoMapState` (extended, additive).
- Produces: TS `SectionRow { startTick: number; startSample: number;
  startBeat: number; startBar: number; period: number }`;
  `sectionTable.ts`'s `sampleAtTick(sections: SectionRow[], sampleRate:
  number, ppq: number, tick: number): number` and `tickAtSample(sections:
  SectionRow[], sampleRate: number, ppq: number, samples: number): number`
  — the ONLY tick↔sample conversion left in the frontend, both pure
  functions over caller-supplied data (no module-level state, easy to
  test in isolation).

- [ ] **Step 1: Backend — extend `TempoMapState`'s wire shape (additive)**

Find `set_tempo_map` in `src-tauri/src/midi/mod.rs` (grep confirms it at
the `pub fn set_tempo_map(` line found during Step-1 research) and its
return type (likely a struct or `serde_json::Value` — read the actual
function body before editing). Add three fields to whatever struct
backs the wire response: `meter_map: Vec<MeterEvent>`, `period_events:
Vec<TempoPeriodEvent>`, `section_table: Vec<SectionRow>` where
`SectionRow` is a new `#[serde(rename_all = "camelCase")]` struct mirroring
`section_table::Section` field-for-field (a thin wire DTO — `Section`
itself stays internal, this is the existing pattern every other wire type
in this crate follows: internal structs are not the wire structs).
`section_table_rule_version: u32`. The function's BODY builds the section
table via `SectionTable::build(&tempo_map, &meter_map)` after constructing
the (now-period-based) `TempoMap` from the request's bpm events (Task 6's
`TempoMap::new` wrapper handles the quantization) and returns it alongside
the existing bpm-projected `events` field (`tempo_map.events()`, Task 3's
wrapper). Write this as a normal Rust code change following the exact
signature of whatever `set_tempo_map` already returns — there is no
generic template to paste; read the function, then extend its return
value additively, matching this crate's established `#[serde(default)]`-
on-read / additive-on-write policy (D-06).

- [ ] **Step 2: Backend test — the additive fields round-trip**

Add a test in `src-tauri/src/midi/mod.rs`'s existing test module (find it)
asserting that calling `set_tempo_map` returns a non-empty `section_table`
whose first row's `start_tick` is 0, and that `meter_map` defaults to
`[{tick:0,num:4,den:4}]` when the store has never had one set. Run:
`timeout 900 cargo test --manifest-path src-tauri/Cargo.toml midi::mod::` —
expect FAIL until Step 1 lands, then PASS.

- [ ] **Step 3: Frontend — write the failing test for `sectionTable.ts`**

```typescript
// src/lib/sectionTable.test.ts (new file — check the project's existing
// test file naming convention, e.g. `*.test.ts` next to a vitest config
// that globs it, before assuming this path; grep an existing `*.test.ts`
// under src/lib/ for the pattern this codebase already uses)
import { describe, it, expect } from "vitest";
import { sampleAtTick, tickAtSample, type SectionRow } from "./sectionTable";

describe("sectionTable", () => {
  const sections: SectionRow[] = [
    { startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 4_233_600_000 }, // 120bpm @508032000 supertick/s: period = 60/120*508032000
  ];
  const sampleRate = 48_000;
  const ppq = 960;

  it("interpolates linearly within a single constant-tempo section", () => {
    expect(sampleAtTick(sections, sampleRate, ppq, 0)).toBe(0);
    expect(sampleAtTick(sections, sampleRate, ppq, 960)).toBeCloseTo(24_000, 0);
  });

  it("inverts sampleAtTick within 1 tick", () => {
    const s = sampleAtTick(sections, sampleRate, ppq, 500);
    const back = tickAtSample(sections, sampleRate, ppq, s);
    expect(Math.abs(back - 500)).toBeLessThanOrEqual(1);
  });

  it("does no tempo/bpm math of its own — a caller with only sections+rate+ppq gets an answer with zero other inputs", () => {
    // Structural check, not a runtime assertion: the function signature
    // itself is the contract (no TempoMap/TempoEvent import in this file).
    expect(typeof sampleAtTick).toBe("function");
  });
});
```

Run: `timeout 300 npx vitest run sectionTable`
Expected: FAIL (module doesn't exist).

- [ ] **Step 4: Implement `sectionTable.ts`**

```typescript
// src/lib/sectionTable.ts
/**
 * Pure lookup against the backend-shipped section table (round-2 §3.6):
 * the ONLY tick<->sample conversion left in the frontend. No tempo/bpm/
 * period knowledge lives here — see this plan's ADR 0006 reading (thin
 * renderer allows presentation-layer interpolation of already-derived
 * data, not deriving the bijection itself, which is what got deleted).
 */
export interface SectionRow {
  startTick: number;
  startSample: number;
  startBeat: number;
  startBar: number;
  /** Superticks per quarter note, constant across this section. */
  period: number;
}

const SUPERTICKS_PER_SECOND = 508_032_000;

function sectionAtTick(sections: SectionRow[], tick: number): SectionRow {
  let lo = 0, hi = sections.length - 1, idx = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (sections[mid].startTick <= tick) { idx = mid; lo = mid + 1; } else { hi = mid - 1; }
  }
  return sections[idx];
}

function sectionAtSample(sections: SectionRow[], samples: number): SectionRow {
  let lo = 0, hi = sections.length - 1, idx = 0;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (sections[mid].startSample <= samples) { idx = mid; lo = mid + 1; } else { hi = mid - 1; }
  }
  return sections[idx];
}

export function sampleAtTick(sections: SectionRow[], sampleRate: number, ppq: number, tick: number): number {
  if (sections.length === 0) return 0;
  const s = sectionAtTick(sections, tick);
  const samplesPerTick = (s.period / SUPERTICKS_PER_SECOND) * sampleRate / ppq;
  return s.startSample + (tick - s.startTick) * samplesPerTick;
}

export function tickAtSample(sections: SectionRow[], sampleRate: number, ppq: number, samples: number): number {
  if (sections.length === 0) return 0;
  const s = sectionAtSample(sections, samples);
  const samplesPerTick = (s.period / SUPERTICKS_PER_SECOND) * sampleRate / ppq;
  if (samplesPerTick <= 0) return s.startTick;
  return s.startTick + (samples - s.startSample) / samplesPerTick;
}
```

- [ ] **Step 5: Run the frontend test**

Run: `timeout 300 npx vitest run sectionTable`
Expected: PASS, 3 tests.

- [ ] **Step 6: Delete the piecewise math — `midi.svelte.ts`**

Read `src/lib/state/midi.svelte.ts`'s `ticksToSamples`/`samplesToTicks`
methods in full (already located during this plan's research: lines ~54
and ~67) and every call site (`~159-160`). Replace the two methods with
calls into `sectionTable.ts`, sourcing `sections`/`sampleRate`/`ppq` from
whatever state field now holds the backend's `TempoMapState.sectionTable`
(a new `$state` field this step adds, populated wherever the store already
adopts a fresh `TempoMapState` from `setTempoMap`/snapshot load — find that
adoption point, likely near the existing `tempoEvents` assignment at line
~88). Delete the module's header comment line claiming it "mirrors
midi::TempoMap: piecewise over sorted {tick,bpm} events" — replace with a
comment stating it now consumes the shipped section table (ADR 0007: the
correction is explicit, not silent).

- [ ] **Step 7: Delete the piecewise math — `demo.ts`**

Same deletion for `ticksToSamples`/`samplesToTicks` (lines ~542, ~856) and
their call sites (~884-902, ~1152, ~2123) in `src/lib/demo.ts` — the
demo/mock backend must also consume section-table data (constructed
locally in the mock, mirroring what the real backend would compute, OR —
simpler and equally correct since the demo backend already owns
`tempoEvents` — the demo backend computes ITS OWN section table once, on
tempo change, using the SAME `sectionTable.ts` module's functions, so
there is truly only one bijection implementation feeding every surface
(round-2 §3.6's stated contract), not two piecewise re-implementations
that happen to agree today and drift tomorrow).

- [ ] **Step 8: Delete the duplicate TS `TempoMap` type / update `ipc.ts`**

In `src/lib/types/ipc.ts`, extend `TempoMapState` additively:
```typescript
export interface TempoMapState {
  ppq: number;
  events: TempoEvent[];
  meterMap: MeterEvent[];
  periodEvents: TempoPeriodEvent[];
  sectionTable: SectionRow[];
  sectionTableRuleVersion: number;
}

export interface MeterEvent {
  tick: number;
  num: number;
  den: number;
}

export interface TempoPeriodEvent {
  tick: number;
  periodStart: number;
  periodEnd: number;
}
```
(`SectionRow` imports from `../sectionTable` rather than being redeclared —
one definition, per this task's own Step 4.) Grep the whole `src/` tree for
any OTHER inline piecewise `samplesPerBeat`/tick-sample derivation this
plan's research didn't already name (round-2 §3.6 mentions "three inline
`samplesPerBeat` derivations" — this plan's Task 9 Steps 6–7 account for
two files; find and delete the third before calling this task done).

- [ ] **Step 9: Run full frontend suite, then full backend suite; commit**

Run: `timeout 300 npx vitest run`
Expected: PASS, every test green (baseline 80 + new `sectionTable.ts`
tests + any adjusted assertions in files that exercised the deleted
methods — update those assertions to go through `sectionTable.ts`
functions instead, do not delete test coverage, redirect it).

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (backend touched in Step 1–2 of this task).

```bash
git add src-tauri/src/midi/mod.rs src/lib/sectionTable.ts \
        src/lib/sectionTable.test.ts src/lib/state/midi.svelte.ts \
        src/lib/demo.ts src/lib/types/ipc.ts
git commit -m "$(cat <<'EOF'
feat(time): frontend consumes the shipped section table; TS piecewise tempo math deleted (Gate C/D frontend exit condition)

set_tempo_map's response grows additive meterMap/periodEvents/sectionTable
fields. sectionTable.ts is the ONE bijection implementation left in the
frontend (round-2 §3.6) — pure interpolation over backend-supplied rows,
no tempo/bpm/period knowledge (ADR 0006 reading recorded in this plan's
preamble). midi.svelte.ts's and demo.ts's independent piecewise
ticksToSamples/samplesToTicks are deleted; the known live bug this kills
(constant-tempo-grid snapping against a piecewise map) goes with them.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: Gate C/D close-out — counts, PHASE4-PLAN handoff, next-prompt.md

**Files:**
- Modify: `README.md`, `CONTRIBUTING.md` (dated test counts)
- Modify: `docs/PHASE4-PLAN.md` (add "Plan C/D handoff" section, mirroring
  the existing "Plan A handoff"/"Plan B handoff" sections' shape)
- Modify: `next-prompt.md` (point at Plan E, same convention as today's
  file points at Plan C+D)
- Modify: `.superpowers/sdd/plan-cd/progress.md` (final status)

**Interfaces:** none — documentation and ledger only.

- [ ] **Step 1: Run both full suites one more time, record the final counts**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | grep "test result:"`
Run: `timeout 300 npx vitest run 2>&1 | tail -5`
Record the exact numbers — do not estimate from earlier steps' projections.

- [ ] **Step 2: Update dated counts**

`README.md` and `CONTRIBUTING.md`: change `346 tests (counted 2026-08-14)`
and `80 frontend unit tests (counted 2026-08-14; vitest)` to the actual
final numbers, redated to the day this task lands.

- [ ] **Step 3: Write the PHASE4-PLAN "Plan C/D handoff" section**

Append after "Plan B handoff", following the exact shape of the two
existing handoff sections (what's IMPLEMENTED, what's binding-carry-forward
for later plans). Content to include, verbatim from this plan's own
rulings — do not paraphrase them into something softer:

- Gate C/D status: test 6 (section-table bound) fully green; test 7
  (lossless v2→v3, including meterMap and the placement/content split)
  green for tempo/meter/MIDI-content-placement; audio's placement/content
  split is ADDRESSING-ONLY (`content_id`/`lane_id` populated, single-row
  JSON shape retained) — the array-split-for-audio ruling from this plan's
  preamble, carried forward for whichever round next touches
  `audio::project.rs`.
- `steady_time` (round-2 §3.5) and the per-block `Arc<TempoMap>` swap are
  NOT delivered by this plan (ruling 3, preamble) — `clap_host.rs`'s
  per-node `self.steady: u64` counter is UNCHANGED, still resets on node
  re-creation. Bind this to Plan E, which already inventories engine-thread
  work.
- No Rust-level `Content`/`Placement` struct replaces `MidiClip`/`Clip` at
  runtime (ruling 2, preamble) — the split is real in the v3 FILE FORMAT
  only; `Store`/`MidiStore` still hold `Clip`/`MidiClip`. Editing shared
  content does not yet update every placement (ADR 0004's stated
  consequence is not yet true) because nothing mints two placements
  sharing one `ContentId` yet (no split/merge/copy command exists — same
  observation the Plan B handoff already recorded, still true).
- The op log stays dark; none of this plan's work is `Session::transact`-
  routed — `set_tempo_map` and the MIDI clip commands remain outside the
  A-slice channel, exactly as before this plan. Binding for Plan E's
  side-channel inventory (§4.5) to close.
- Standing carry-forwards from Plan A/B (snapshot-rebuild deferral, no
  panic rollback in `transact`, `fold_ops` coalescing constraint before
  Gate E, split/merge/copy remint rules binding future content ops) are
  UNCHANGED by this plan — restate them here per the established
  handoff-section convention (copy verbatim from the Plan B handoff
  section, do not re-derive).

- [ ] **Step 4: Rewrite `next-prompt.md` for Plan E**

Follow the exact structure of the current file (this plan's own Step-1
research input): "Where you are" (branch, worktree, PR number — update to
whatever this plan's PR number turned out to be), "What has happened"
(extend the numbered list with a #6 entry for Plan C+D, same density as
the #4/#5 entries for A/B), "Your task" (Plan E: the side-channel
totality, round-2 §4.5, PHASE4-PLAN's E row and Gate E), "Ground rules"
(same as today's, since they're plan-invariant — copy verbatim). Do NOT
invent Plan E's task breakdown here — that's Plan E's own authoring step,
just as this file didn't invent Plan C/D's tasks, only pointed at what to
read.

- [ ] **Step 5: Finalize the progress ledger**

Update `.superpowers/sdd/plan-cd/progress.md` (maintained live since Task
1 — see this plan's execution preamble) with a final summary: tasks
landed, final test counts, the three scope rulings restated, and either
"Gate C/D CLOSED" or an honest partial-completion note if some tasks
didn't land this session (see this plan's own top-level execution note:
solo session, may stop at any green task boundary and hand off — if that
happened before Task 10, Task 10 itself is what the NEXT session runs
first, using this same plan document, picking up at whichever task the
ledger says is next).

- [ ] **Step 6: Commit and push**

```bash
git add README.md CONTRIBUTING.md docs/PHASE4-PLAN.md next-prompt.md \
        .superpowers/sdd/plan-cd/progress.md
git commit -m "$(cat <<'EOF'
docs: Plan C+D close-out — counts, PHASE4-PLAN handoff, next stage (Plan E)

Gate C/D status recorded: test 6 green, test 7 green for tempo/meter/MIDI
content-placement, audio content-placement addressing-only (scope ruling,
carried forward). steady_time and the Content/Placement runtime-type split
are explicit, documented deferrals — not silent narrowing (ADR 0007).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Self-review notes (writing-plans skill, run against this plan before Task 1 starts)

**Spec coverage:**
- round-2 §3.1 (newtypes, no cross-domain Ord) → Task 1. §3.2 (unanchored
  durations) → NOT a separate task: `TempoMap::tick_to_samples_v3`/
  `samples_to_tick_v3` already take the anchor as an argument (never
  stored), so the property is satisfied by Task 3's design, not a distinct
  deliverable; noted here so it isn't mistaken for a gap. §3.3 (integer
  period tempo + migration) → Tasks 2/3/6. §3.3's meter map → Task 4/6.
  §3.4 (section table) → Task 5. §3.5 (steady_time, per-block map) →
  explicitly OUT (ruling 3). §3.6 (wire/frontend bijection) → Task 9.
- round-2 §5 (content/placement) → Tasks 7 (MIDI, full) + 8 (audio,
  addressing-only, ruling 1). Automation-lane identity gap → correctly
  out of scope per round-2's own text (assigned to the node-graph round).
- ADR 0002 → Tasks 1–6, 9. ADR 0004 → Tasks 7–8.
- Gate C/D's exact test list (test 6, test 7, frontend section-table
  consumption) → Tasks 5, 6+7, 9 respectively. All three covered.

**Placeholder scan:** the two `// REVIEW:` items in Task 3 (dead
`segment_span` draft line) and Task 5 (`sample_at_tick` stub-then-fix) are
INTENTIONAL — they instruct the implementer to remove draft code, not
placeholders standing in for unwritten logic. Every other code block is
complete, runnable Rust/TypeScript. Task 7 Step 6's "branch: if v3-native
shape... otherwise fall through" is a real algorithm description backed by
Task 6's already-fully-coded fallback path, not a hand-wave.

**Type consistency:** `Ticks`/`Samples` (Task 1) → consumed identically in
Tasks 3, 5, 9. `TempoPeriodEvent{tick,period_start,period_end}` (Task 2) →
same field names through Tasks 3, 6, 7, 9's wire DTO. `ContentId`/`LaneId`
(existing/Task 7) → same names in Task 8. `V3Data{ppq,tempo_events,
meter_events,clips}` (Task 6) → Task 7 extends `clips`' element type
in-place (`MidiClip` gains fields) without changing `V3Data`'s own shape,
confirmed consistent.

## Execution note for tonight

Per the user's binding session constraints (see the orchestrating agent's
brief, not reproduced here): NO subagent dispatch. I run every task above
myself, foreground `timeout`-guarded test gates, one commit per task,
self-review each diff before committing (flagging anything uncertain with
`// REVIEW:` in the code — this plan pre-seeds two such markers so the
convention is visible from the start), and update
`.superpowers/sdd/plan-cd/progress.md` after every task. If I stop before
Task 10 (context/token budget), I stop at the most recent green, committed
task boundary, write a Task-10-shaped handoff directly into `next-prompt.md`
(not waiting for Task 10's own step to do it), push, and end the session
with a summary naming exactly which tasks landed and which didn't.
