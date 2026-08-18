# Automation modes (Off/Read/Write/Touch/Latch) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give track-gain automation an Off/Read/Write/Touch/Latch mode per
track, replacing today's hardcoded always-on ("Read") behavior, including
recording new automation points by moving the fader during playback.

**Architecture:** One additive `TrackState.automation_mode` field, written
through the existing `TrackMixChange`/`PropPath` machinery (§2 of the spec).
`Off` is enforced once, in the single shared `audio::engine::compile_track_ramps`
function both live rebuild and offline bounce already call, so live playback
and a bounce always agree. Write/Touch/Latch recording runs on the engine
control thread's existing ≤2 ms tick (the same loop `ParamAutomationDriver`
already runs on), reads the fader's existing live RT atomic
(`ParamTable::gain`), and commits finished recording passes through the
engine's own `Committer` — the exact mechanism `audio::engine::Control`
already uses to finalize an audio take (`commit_recording_finalize`,
`TxMeta::engine("stop recording")`), so a recording pass becomes one real
undo entry with no new cross-thread commit path invented.

**Tech Stack:** Rust (Tauri backend, `src-tauri/`), TypeScript/Svelte 5
(frontend, `src/`), `cargo test`, `vitest`.

**Spec:** [`docs/superpowers/specs/2026-08-18-automation-write-touch-latch-design.md`](../specs/2026-08-18-automation-write-touch-latch-design.md)

## Global Constraints

- Additive only: no `OP_FORMAT_VERSION` bump. Every new struct field is
  `#[serde(default)]` (or `Option`, `skip_serializing_if` as the codebase's
  existing optional-field convention uses).
- `transact` closures must not panic. Validate before mutating, every time.
- Gesture lock order is gesture-before-session, everywhere a gesture and the
  session lock are both touched.
- A recording pass with zero recorded points commits nothing — no phantom
  undo entry (spec §4.6).
- Track-gain automation modes only in this plan — plugin-parameter lanes are
  untouched (spec §1, §7).
- Foreground, `timeout`-guarded test runs only:
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml` and
  `timeout 300 npx vitest run`. Run the whole suite (not just the new tests)
  before the close-out task's count update.
- A task that changes the measured test count updates README.md +
  CONTRIBUTING.md's dated count line in the SAME commit — done once, at
  close-out (Task 11), not per intermediate task (mirrors Track D's own
  close-out task).

---

### Task 1: `AutomationMode` enum + `TrackState.automation_mode` field

**Files:**
- Modify: `src-tauri/src/audio/types.rs:110-151` (`TrackState`)
- Modify: `docs/ipc-schemas/track-state.schema.json`
- Test: `src-tauri/src/audio/types.rs` (bottom `#[cfg(test)] mod tests`, or
  wherever `TrackState` serde round-trip tests already live — grep
  `TrackState` serde tests in that file first and follow the existing
  pattern)

**Interfaces:**
- Produces: `pub enum AutomationMode { Off, Read, Write, Touch, Latch }`
  (`Default` = `Read`), `TrackState.automation_mode: AutomationMode` —
  every later task reads/writes this exact type and field name.

- [ ] **Step 1: Write the failing serde test**

Add to `src-tauri/src/audio/types.rs` (in its test module — if the file has
none yet, add `#[cfg(test)] mod tests { use super::*; ... }` at the bottom,
matching the `mod tests` convention used elsewhere in this codebase, e.g.
`src-tauri/src/plugins/automation.rs:889`):

```rust
#[test]
fn automation_mode_defaults_to_read_when_absent_from_json() {
    let json = serde_json::json!({
        "id": "t-1", "name": "Track", "kind": "audio", "gainDb": 0.0,
        "pan": 0.0, "muted": false, "soloed": false, "armed": false,
        "color": "#ffffff"
    });
    let t: TrackState = serde_json::from_value(json).unwrap();
    assert_eq!(t.automation_mode, AutomationMode::Read);
}

#[test]
fn automation_mode_round_trips_every_variant() {
    for mode in [
        AutomationMode::Off,
        AutomationMode::Read,
        AutomationMode::Write,
        AutomationMode::Touch,
        AutomationMode::Latch,
    ] {
        let json = serde_json::to_value(mode).unwrap();
        let back: AutomationMode = serde_json::from_value(json).unwrap();
        assert_eq!(back, mode);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml automation_mode_ -- --nocapture`
Expected: FAIL to compile — `AutomationMode` and `TrackState.automation_mode` don't exist yet.

- [ ] **Step 3: Add the enum and the field**

In `src-tauri/src/audio/types.rs`, immediately above `pub struct TrackState`:

```rust
/// Track-gain automation playback/record mode. `Read` (the default) is
/// today's behavior: the compiled lane always overrides the fader during
/// playback. `Off` skips the lane entirely (bypass). `Write`/`Touch`/
/// `Latch` additionally RECORD new points into the lane while playing —
/// see `docs/superpowers/specs/2026-08-18-automation-write-touch-latch-design.md`.
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

In `TrackState`, add after the existing `group` field (types.rs:150, just
before the closing `}` at line 151):

```rust
    /// Track-gain automation mode (this feature). Additive: absent on
    /// pre-existing files reads as `Read`, which is exactly today's
    /// always-on behavior — no migration needed.
    #[serde(default)]
    pub automation_mode: AutomationMode,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml automation_mode_ -- --nocapture`
Expected: PASS (2 tests)

- [ ] **Step 5: Update the IPC schema**

In `docs/ipc-schemas/track-state.schema.json`, add a property after
`"group"` (mirror its shape — see the file's existing `"group"` entry for
the exact JSON style):

```json
    "automationMode": {
      "description": "Track-gain automation mode: \"off\" bypasses the lane entirely, \"read\" (default) always applies it during playback, \"write\"/\"touch\"/\"latch\" additionally record new points while playing. Additive; pre-existing files omit it and default to \"read\".",
      "type": "string",
      "enum": ["off", "read", "write", "touch", "latch"]
    },
```

- [ ] **Step 6: Run the full Rust suite once for this file's area**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml audio::types`
Expected: PASS, no regressions in the module.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/audio/types.rs docs/ipc-schemas/track-state.schema.json
git commit -m "feat(automation): add AutomationMode enum and TrackState.automation_mode"
```

---

### Task 2: `PropPath::AutomationMode` — read/write/apply/rebuild-trigger wiring, `TrackMixChange`, `set_track_automation_mode` command

**Files:**
- Modify: `src-tauri/src/control/op.rs:353-354` (`PropPath` enum)
- Modify: `src-tauri/src/control/session.rs` (`read_prop`, `write_prop`,
  `read_midi_prop`, `write_midi_prop`, `read_transport_prop`,
  `write_transport_prop`, `apply_raw`'s Track `Set` arm — see exact line
  numbers below)
- Modify: `src-tauri/src/control/mod.rs:496-514` (param_writes no-op match),
  `apply_mix_changes` (~line 1399-1418)
- Modify: `src-tauri/src/control/ops.rs:21-41` (`TrackMixChange`)
- Modify: `src-tauri/src/audio/mod.rs:851-862` (add `set_track_automation_mode`
  next to `set_track_arm`)
- Modify: `src-tauri/src/lib.rs:227` area (`invoke_handler` registration list
  — add `control::set_track_automation_mode` next to `control::set_track_mix`)
- Test: `src-tauri/src/control/mod.rs` (mirror the existing
  `set_track_mix_emits_project_changed_with_updated_tracks` test at
  `control/mod.rs:6050` and the gesture-fold test at `control/mod.rs:6244`)

**Interfaces:**
- Consumes: `AutomationMode` (Task 1).
- Produces: `PropPath::AutomationMode`, `TrackMixChange.automation_mode:
  Option<AutomationMode>`, Tauri command `set_track_automation_mode(track_id:
  String, mode: AutomationMode)`. Task 4 (frontend) calls this command by
  name.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/control/mod.rs`'s test module (mirror the style at
`control/mod.rs:6050-6065`, same `test_plane_with_tracks`/`recording_control_plane`
helpers already used there):

```rust
#[test]
fn set_track_mix_updates_automation_mode_and_triggers_rebuild() {
    let (cp, events, engine) = recording_control_plane();
    let track = cp
        .add_track(Some("Agent Mix".into()), None, TxMeta::user("add track"))
        .unwrap();
    events.lock().clear();
    engine.lock().clear();

    let updated = cp
        .set_track_mix(
            vec![TrackMixChange {
                automation_mode: Some(AutomationMode::Off),
                ..TrackMixChange::new(track.id.as_str())
            }],
            TxMeta::user("set track mix"),
        )
        .unwrap();

    assert_eq!(updated[0].automation_mode, AutomationMode::Off);
    // Structural (mirrors InstrumentId): changing the mode must trigger a
    // rebuild, or an already-published `RtGraph::gain_ramps` slot would
    // keep applying an Off track's lane until the NEXT unrelated rebuild.
    assert!(
        engine.lock().iter().any(|m| matches!(m, ControlMsg::Rebuild)),
        "expected a Rebuild message, got {:?}",
        engine.lock()
    );
}

#[test]
fn set_track_automation_mode_round_trips_through_undo() {
    let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
    plane
        .set_track_mix(
            vec![TrackMixChange {
                automation_mode: Some(AutomationMode::Write),
                ..TrackMixChange::new("t-1")
            }],
            TxMeta::user("set mode"),
        )
        .unwrap();
    assert_eq!(
        plane.session().lock().store.tracks.iter().find(|t| t.id == "t-1").unwrap().automation_mode,
        AutomationMode::Write
    );
    plane.undo().unwrap();
    assert_eq!(
        plane.session().lock().store.tracks.iter().find(|t| t.id == "t-1").unwrap().automation_mode,
        AutomationMode::Read
    );
}
```

Before writing these, read the exact helper signatures
(`recording_control_plane`, `test_plane_with_tracks`, `plane.undo()`) at
their existing call sites near `control/mod.rs:6050` and `:6212` — use
whatever they actually return/require; the snippets above show intent, not
a guaranteed-exact match to helper signatures that may differ slightly.

- [ ] **Step 2: Run test to verify it fails**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml automation_mode -- --nocapture`
Expected: FAIL to compile (`TrackMixChange.automation_mode` doesn't exist).

- [ ] **Step 3: Add `PropPath::AutomationMode`**

In `src-tauri/src/control/op.rs`, in `enum PropPath` (starts at line 353),
add one variant. Follow the enum's existing single-line-group style at line
354 (`Gain, Pan, Muted, Soloed, Armed,`) — add `AutomationMode` to that same
line group:

```rust
    Gain, Pan, Muted, Soloed, Armed, AutomationMode,
```

- [ ] **Step 4: Wire the Track read/write arms**

In `src-tauri/src/control/session.rs`, in `read_prop` (starts line 1832),
add after the `Armed` arm (line 1838):

```rust
        PropPath::AutomationMode => Ok(serde_json::json!(t.automation_mode)),
```

In `write_prop` (starts line 1869), add after the `Armed` arm (line 1895):

```rust
        PropPath::AutomationMode => {
            let v: AutomationMode = serde_json::from_value(to.clone())
                .map_err(|e| format!("automationMode: {e}"))?;
            t.automation_mode = v;
            Ok(serde_json::json!(t.automation_mode))
        }
```

`AutomationMode` needs to be in scope in `session.rs` — add
`crate::audio::types::AutomationMode` to whatever `use crate::audio::...`
import already brings in `TrackState` at the top of the file.

- [ ] **Step 5: Add `AutomationMode` to the four catch-all "not a property" match arms**

Each of `read_prop`'s Track catch-all (session.rs:1846-1862, the arm ending
`"... is not a Track property"`), `read_midi_prop`'s catch-all
(session.rs:1976-1993), `write_midi_prop`'s catch-all (mirror of
`read_midi_prop`'s, immediately below it — same shape as the
`ContentLengthTicks`/session.rs:2051-2069 arm you already read), and
`read_transport_prop`/`write_transport_prop`'s catch-alls
(session.rs:2088-2107, session.rs:2161-2180) currently list `PropPath::Armed`
in their `|`-chain. Add `| PropPath::AutomationMode` right after
`PropPath::Armed` in EVERY one of these six match arms (the two Track-side
catch-alls don't list `Armed` — Track's own `read_prop`/`write_prop` already
handle it in Step 4 above, so only the MidiClip and Transport catch-alls
need this addition — four sites total). The compiler's exhaustiveness check
will fail to build until all four are updated; that is the mechanism that
catches a missed site.

- [ ] **Step 6: Trigger a rebuild on mode change**

In `src-tauri/src/control/session.rs`, in `apply_raw`'s `Op::Set {
object: ObjectRef::Track(id), ... }` arm (starts line 678), add a new
`if matches!(path, PropPath::AutomationMode) { ... }` block, following
EXACTLY the `InstrumentId` block's shape (session.rs:695-704) — insert it
right after that block:

```rust
            if matches!(path, PropPath::AutomationMode) {
                // Structural: which lanes `compile_track_ramps` attaches
                // depends on this field (Off skips the lane entirely), so
                // changing it must rebuild — there is no ParamTable
                // counterpart to write live, same reasoning as InstrumentId.
                effect.rebuild = true;
                effect.persist.project = true;
                return Ok(inverse);
            }
```

- [ ] **Step 7: No-op it in the live ParamTable-write match (no RT counterpart)**

In `src-tauri/src/control/mod.rs`, in the `match path` block starting at
line 464, add `op::PropPath::AutomationMode` to the no-op catch-all group
that already lists `op::PropPath::Name | op::PropPath::Armed | ...`
(mod.rs:496-514) — add it right after `op::PropPath::Armed` on line 497.

- [ ] **Step 8: `TrackMixChange` gets the new field**

In `src-tauri/src/control/ops.rs`, in `struct TrackMixChange` (line 21-28),
add after `pub armed: Option<bool>,`:

```rust
    pub automation_mode: Option<crate::audio::types::AutomationMode>,
```

And in `TrackMixChange::new` (lines 30-41), add to the struct literal:

```rust
            automation_mode: None,
```

- [ ] **Step 9: Apply it in `apply_mix_changes`**

In `src-tauri/src/control/mod.rs`, in `fn apply_mix_changes` (starts line
1399), add after the `armed` block (line 1413-1415):

```rust
        if let Some(m) = c.automation_mode {
            tx.apply(set_prop(&c.track_id, op::PropPath::AutomationMode, serde_json::json!(m)))?;
        }
```

- [ ] **Step 10: Add the Tauri command**

In `src-tauri/src/audio/mod.rs`, immediately after `set_track_arm`
(ends line 862), add:

```rust
#[tauri::command]
pub fn set_track_automation_mode(
    track_id: String,
    mode: crate::audio::types::AutomationMode,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    single_mix_change(
        &control,
        TrackMixChange { automation_mode: Some(mode), ..TrackMixChange::new(track_id) },
        "set automation mode",
    )
}
```

- [ ] **Step 11: Register the command**

In `src-tauri/src/lib.rs`, find the `invoke_handler` list that includes
`control::set_track_mix` (near line 227) and add
`control::set_track_automation_mode,` next to it. (If `set_track_gain`/
`set_track_mute`/etc. live under a different module path than
`control::` in that list — check the exact prefix used for
`set_track_mute` right above/below line 227 and match it; `audio::mod.rs`'s
commands may be re-exported under `control::` or under `audio::` in
`lib.rs` — follow whichever the neighboring per-field commands use.)

- [ ] **Step 12: Run test to verify it passes**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml automation_mode -- --nocapture`
Expected: PASS (both new tests).

- [ ] **Step 13: Run the full Rust suite**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, no regressions (this touches six match arms across
`session.rs` — the compiler's exhaustiveness check is the safety net for a
missed site, but run the full suite anyway).

- [ ] **Step 14: Commit**

```bash
git add src-tauri/src/control/op.rs src-tauri/src/control/session.rs \
        src-tauri/src/control/mod.rs src-tauri/src/control/ops.rs \
        src-tauri/src/audio/mod.rs src-tauri/src/lib.rs
git commit -m "feat(automation): wire AutomationMode through PropPath, TrackMixChange, and a new command"
```

---

### Task 3: `Off` mode skips the lane — live rebuild AND offline bounce (one shared function)

**Files:**
- Modify: `src-tauri/src/audio/engine.rs:1112-1147` (`compile_track_ramps`)
- Test: `src-tauri/src/audio/engine.rs` (near its existing
  `compile_track_ramps`/ramp tests) and
  `src-tauri/src/audio/offline.rs` (near
  `build_graph_applies_track_gain_automation_to_the_bounce`, line ~486)

**Interfaces:**
- Consumes: `AutomationMode` (Task 1), `resolve_target`/`LaneTarget` from
  `src-tauri/src/plugins/automation.rs:491-516`.
- Produces: no new symbol — behavior change only, so later tasks are
  unaffected.

- [ ] **Step 1: Write the failing test**

`compile_track_ramps` is `pub(crate)`, callable directly from
`engine.rs`'s own test module. Add near its existing tests (find them by
searching `compile_track_ramps` inside `engine.rs`'s `#[cfg(test)] mod
tests` — if none test it directly yet, add this near the
`build_graph_applies_track_gain_automation_to_the_bounce`-style test in
`offline.rs` instead, calling `offline::build_graph` end-to-end, which is
simpler to set up honestly and exercises the SAME shared function):

```rust
#[test]
fn build_graph_skips_a_gain_lane_when_the_track_is_off() {
    let mut store = Store::default();
    let track = Track { id: "t-1".into(), automation_mode: AutomationMode::Off, ..Track::default() };
    store.tracks.push(track);
    let automation = AutomationDoc {
        lanes: vec![AutomationLane {
            id: "a-1".into(),
            target_node: "track:t-1".into(),
            param_id: TRACK_PARAM_GAIN,
            points: vec![
                AutomationPoint { tick: 0, value: 0.0 },
                AutomationPoint { tick: 1000, value: 1.0 },
            ],
        }],
    };
    let graph = build_graph(&store, &MidiStore::default(), &PluginDoc::default(), &automation, &ModulationDoc::default(), None, 48_000);
    // Off: the compiled ramp table must have NO entry for this track's slot
    // — same as if the lane didn't exist.
    assert!(graph.graph.gain_ramps()[0].is_none());
}
```

Before writing this, read `offline.rs`'s existing
`build_graph_applies_track_gain_automation_to_the_bounce` test in full
(line ~486) and copy its EXACT setup shape (`Store`/`Track`/`AutomationDoc`
construction helpers, and however it reads back the compiled ramp table —
`gain_ramps()` above is illustrative; use whatever accessor that existing
test actually uses).

- [ ] **Step 2: Run test to verify it fails**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml skips_a_gain_lane_when_the_track_is_off -- --nocapture`
Expected: FAIL — the lane is still applied (Off is not yet respected).

- [ ] **Step 3: Filter `Off`-mode tracks' lanes in `compile_track_ramps`**

In `src-tauri/src/audio/engine.rs`, `compile_track_ramps` (starts line
1112), it currently passes `lanes` straight to `compile_gain_ramps` (line
1125). Change the `else` branch (lines 1124-1131) to filter first:

```rust
    let mut ramps: Vec<TrackRamps> = if lanes.is_empty() {
        (0..n_slots).map(|_| TrackRamps::default()).collect()
    } else {
        let live_lanes: Vec<_> = lanes
            .iter()
            .filter(|lane| {
                // Off bypasses the lane entirely — same rule for a live
                // rebuild and an offline bounce, since both funnel through
                // this one function.
                match crate::plugins::automation::resolve_target(lane) {
                    Some(crate::plugins::automation::LaneTarget::TrackGain(tid)) => store
                        .tracks
                        .iter()
                        .find(|t| t.id.as_str() == tid)
                        .is_none_or(|t| t.automation_mode != crate::audio::types::AutomationMode::Off),
                    _ => true, // not a track-gain lane; unaffected by this filter
                }
            })
            .cloned()
            .collect();
        crate::plugins::automation::compile_gain_ramps(&live_lanes, map, n_slots, &|tid| {
            slots.get(tid).copied()
        })
        .into_iter()
        .map(|gain| TrackRamps { gain, pan: None })
        .collect()
    };
```

`AutomationLane` must be `Clone` for the `.cloned()` above — it already
derives `Clone` (`plugins/automation.rs:76-77`), so no change needed there.
`compile_track_ramps` already takes `store: &Store` as a parameter
(engine.rs:1115), so no signature change is needed.

- [ ] **Step 4: Run test to verify it passes**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml skips_a_gain_lane_when_the_track_is_off -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Add the parity test — Read is unaffected**

Add a sibling test, `build_graph_still_applies_a_gain_lane_when_the_track_is_read`
(same setup, `automation_mode: AutomationMode::Read`, asserts the ramp IS
present) — this is the regression guard that Step 3's filter didn't
accidentally drop the default case.

- [ ] **Step 6: Run both new tests, then the full Rust suite**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml gain_lane_when_the_track -- --nocapture`
Expected: PASS (2 tests).
Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, no regressions (this is a shared function — both
`engine::rebuild`'s live path and `offline::build_graph`'s bounce path call
it, so the existing Track D audibility/bounce-parity tests are the
regression net here).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/audio/engine.rs src-tauri/src/audio/offline.rs
git commit -m "feat(automation): Off mode skips a track's gain lane in both live rebuild and bounce"
```

---

### Task 4: Frontend TS type + Backend method + project store method

**Files:**
- Modify: `src/lib/types/ipc.ts:159-179` (`TrackState` interface)
- Modify: `src/lib/tauri.ts:158-162` (`Backend` interface), `:606-608`
  (real `TauriBackend` impl)
- Modify: `src/lib/demo.ts` (mock backend — find its `set_track_arm`-style
  mock near the existing `"set_track_mix"` reference at demo.ts:3453 and
  add a matching mock for the new command)
- Modify: `src/lib/state/project.svelte.ts:183-194` (add
  `setAutomationMode` next to `toggleArm`)
- Test: `src/lib/state/project.svelte.test.ts` (mirror the
  `beginGesture("gain drag")` test area, or wherever `toggleMute`/`setMute`
  already have coverage — grep `setMute` in that file first)

**Interfaces:**
- Consumes: nothing new from earlier tasks (this task is frontend-only;
  the backend command from Task 2 must exist for a manual smoke test, but
  the unit tests here mock `backend`).
- Produces: `AutomationMode` TS type (`"off" | "read" | "write" | "touch" |
  "latch"`), `TrackState.automationMode`, `project.setAutomationMode(trackId,
  mode)` — Task 5 calls this method by name.

- [ ] **Step 1: Write the failing test**

In `src/lib/state/project.svelte.test.ts`, add (mirror whatever existing
`setMute`/`toggleMute` test does for its `backend` mock — read that test
first and copy its exact mock-setup shape):

```typescript
it("setAutomationMode patches the track and calls the backend", async () => {
  const setTrackAutomationMode = vi.fn().mockResolvedValue(undefined);
  // ... use this test file's existing pattern for injecting a mock backend
  // (however setMute's test wires `backend`), assigning
  // `setTrackAutomationMode` onto it.
  await project.setAutomationMode("t-1", "write");
  expect(setTrackAutomationMode).toHaveBeenCalledWith("t-1", "write");
  expect(project.trackById("t-1")?.automationMode).toBe("write");
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `timeout 120 npx vitest run project.svelte.test -t setAutomationMode`
Expected: FAIL — `setAutomationMode` doesn't exist.

- [ ] **Step 3: Add the TS type and field**

In `src/lib/types/ipc.ts`, above `export interface TrackState` (line 159),
add:

```typescript
export type AutomationMode = "off" | "read" | "write" | "touch" | "latch";
```

In `TrackState` (lines 159-179), add after `armed: boolean;` (line 169):

```typescript
  /** Track-gain automation mode. Additive; absent on older projects means "read". */
  automationMode: AutomationMode;
```

- [ ] **Step 4: Add the Backend method**

In `src/lib/tauri.ts`, in the `Backend` interface (near line 158-162), add
after `setTrackArm`:

```typescript
  setTrackAutomationMode(trackId: string, mode: AutomationMode): Promise<void>;
```

In the real implementation (near line 606-608, right after `setTrackArm`'s
impl), add:

```typescript
  async setTrackAutomationMode(trackId: string, mode: AutomationMode) {
    await invoke("set_track_automation_mode", { trackId, mode });
  }
```

- [ ] **Step 5: Add the demo/mock backend method**

In `src/lib/demo.ts`, find how `set_track_arm` (or another per-field mix
command) is mocked and add a matching case for `"set_track_automation_mode"`
that updates the demo store's in-memory track and returns it, following
that file's existing pattern exactly (read the surrounding 20 lines around
the `"set_track_mix"` reference at demo.ts:3453 before writing this).

- [ ] **Step 6: Add `project.setAutomationMode`**

In `src/lib/state/project.svelte.ts`, after `toggleArm` (ends line ~194),
add:

```typescript
  async setAutomationMode(trackId: string, mode: AutomationMode) {
    const t = this.trackById(trackId);
    if (!t || t.automationMode === mode) return;
    this.patchTrack(trackId, { automationMode: mode });
    await backend.setTrackAutomationMode(trackId, mode);
  }
```

Add `AutomationMode` to this file's existing `import type { TrackState,
... } from "../types/ipc";` line.

- [ ] **Step 7: Run test to verify it passes**

Run: `timeout 120 npx vitest run project.svelte.test -t setAutomationMode`
Expected: PASS.

- [ ] **Step 8: Run the full frontend suite**

Run: `timeout 300 npx vitest run`
Expected: PASS, no regressions (a new required `TrackState.automationMode`
field can break other tests' fixture objects — fix any fixture that
constructs a `TrackState` literal without it by adding
`automationMode: "read"`, matching this feature's default).

- [ ] **Step 9: Commit**

```bash
git add src/lib/types/ipc.ts src/lib/tauri.ts src/lib/demo.ts src/lib/state/project.svelte.ts src/lib/state/project.svelte.test.ts
git commit -m "feat(automation): frontend AutomationMode type, backend command, and project store method"
```

---

### Task 5: `AutomationModeSelector.svelte` + wire it into `TrackHeader.svelte`

**Files:**
- Create: `src/lib/components/AutomationModeSelector.svelte`
- Create: `src/lib/components/automation-mode-selector.dom.test.ts`
- Modify: `src/lib/components/TrackHeader.svelte:324-358` (the `.toggles`
  row)

**Interfaces:**
- Consumes: `project.setAutomationMode` (Task 4), `TrackState.automationMode`
  (Task 4).
- Produces: `<AutomationModeSelector mode={track.automationMode}
  onchange={(m) => project.setAutomationMode(track.id, m)} />` — a small,
  independently-testable dumb component (kept separate from the already
  large `TrackHeader.svelte`, per this repo's file-structure convention:
  smaller, focused, independently testable units).

- [ ] **Step 1: Write the failing DOM test**

Create `src/lib/components/automation-mode-selector.dom.test.ts`. Read
`src/lib/components/automation-track-row.dom.test.ts` FIRST (PR #80's
precedent — the only existing example of mounting a `.svelte` component
under this repo's jsdom environment) and copy its exact mount/cleanup
pattern (imports, `render`/`mount` helper, teardown). Then:

```typescript
import { describe, it, expect, vi } from "vitest";
// follow automation-track-row.dom.test.ts's exact mount helper import/usage here
import AutomationModeSelector from "./AutomationModeSelector.svelte";

describe("AutomationModeSelector", () => {
  it("renders all five modes and calls onchange with the clicked mode", async () => {
    const onchange = vi.fn();
    // mount(AutomationModeSelector, { props: { mode: "read", onchange } })
    // using whatever mount helper automation-track-row.dom.test.ts uses.
    // Then: find and click the "write" option/button, and assert:
    // expect(onchange).toHaveBeenCalledWith("write");
  });

  it("marks the current mode as selected", async () => {
    // mount with mode: "latch", assert the "latch" control has whatever
    // selected/aria-pressed/aria-current attribute the implementation uses.
  });
});
```

(The mount-helper specifics are deliberately left to be copied verbatim
from `automation-track-row.dom.test.ts` rather than guessed here — that
file is the one precedent for this repo's jsdom setup and its exact API
must be matched, not re-invented.)

- [ ] **Step 2: Run test to verify it fails**

Run: `timeout 120 npx vitest run automation-mode-selector.dom -t AutomationModeSelector`
Expected: FAIL — the component doesn't exist yet.

- [ ] **Step 3: Write the component**

Create `src/lib/components/AutomationModeSelector.svelte`:

```svelte
<script lang="ts">
  import type { AutomationMode } from "../types/ipc";

  let {
    mode,
    onchange,
  }: { mode: AutomationMode; onchange: (mode: AutomationMode) => void } = $props();

  const MODES: { value: AutomationMode; label: string; title: string }[] = [
    { value: "off", label: "Off", title: "Automation off — bypass the lane" },
    { value: "read", label: "R", title: "Read — always apply the lane" },
    { value: "write", label: "W", title: "Write — continuously record while playing" },
    { value: "touch", label: "T", title: "Touch — record while the fader is held" },
    { value: "latch", label: "L", title: "Latch — record while held, then hold the last value" },
  ];
</script>

<div class="automode" role="group" aria-label="Automation mode">
  {#each MODES as m (m.value)}
    <button
      type="button"
      class="tog"
      class:on={mode === m.value}
      aria-pressed={mode === m.value}
      title={m.title}
      onclick={() => onchange(m.value)}
    >{m.label}</button>
  {/each}
</div>

<style>
  .automode {
    display: flex;
    gap: 2px;
  }
  /* Follow the existing `.tog`/`.on` styling already used by the
     arm/mute/solo buttons in TrackHeader.svelte (lines 324-358) — reuse
     those exact token references (no-literals.test.ts guards raw color
     literals in every component's <style> block) rather than inventing new
     ones here. */
</style>
```

The `<style>` block's placeholder comment must be replaced with real,
token-based CSS before this task is done — copy the token variables
`TrackHeader.svelte`'s own `.tog`/`.on` classes use (check its `<style>`
block) so `no-literals.test.ts` passes (see Step 6).

- [ ] **Step 4: Wire it into `TrackHeader.svelte`**

In `src/lib/components/TrackHeader.svelte`, add
`import AutomationModeSelector from "./AutomationModeSelector.svelte";` to
the top imports (near line 19-20). In the `.toggles` div (lines 325-358),
add after the closing `</span>` of the "auto" lane-picker button (line
357), still inside `.toggles`:

```svelte
        <AutomationModeSelector
          mode={track.automationMode}
          onchange={(mode) => project.setAutomationMode(track.id, mode)}
        />
```

- [ ] **Step 5: Run test to verify it passes**

Run: `timeout 120 npx vitest run automation-mode-selector.dom -t AutomationModeSelector`
Expected: PASS.

- [ ] **Step 6: Run the full frontend suite (covers `no-literals.test.ts`)**

Run: `timeout 300 npx vitest run`
Expected: PASS, including `no-literals.test.ts` — if it fails on the new
component's `<style>` block, replace any raw color literal with the
existing token variable `TrackHeader.svelte` uses for `.tog`/`.on`, not a
`theme-exempt:` comment (there is no reason this control needs an
exemption).

- [ ] **Step 7: Commit**

```bash
git add src/lib/components/AutomationModeSelector.svelte \
        src/lib/components/automation-mode-selector.dom.test.ts \
        src/lib/components/TrackHeader.svelte
git commit -m "feat(automation): mode selector UI in the track header"
```

---

### Task 6: `AutomationRecorder` — pure recording state machine

**Files:**
- Modify: `src-tauri/src/plugins/automation.rs` (add near
  `ParamAutomationDriver`, i.e. after its `impl` block, before the
  `#[cfg(test)] mod tests` at line 889)
- Test: same file's `mod tests` block

**Interfaces:**
- Consumes: `AutomationMode` (Task 1), `AutomationPoint` (already exists,
  `automation.rs:70-73`).
- Produces:
  ```rust
  pub struct AutomationRecorder { /* private */ }
  impl AutomationRecorder {
      pub fn new() -> Self;
      /// Call once per track per control-thread tick while the transport is
      /// playing. `tick` is the current playhead position in musical ticks
      /// (already converted from samples by the caller — see Task 9).
      /// `touched` is ignored for `Write` (always records) and for `Off`/
      /// `Read` (never records).
      pub fn sample(&mut self, track_id: &str, tick: u32, mode: AutomationMode, touched: bool, value: f32);
      /// Call when a track's recording pass ends (gesture close for Touch,
      /// or mode change / transport stop for any mode). Returns the
      /// recorded points for that track since the pass started, or `None`
      /// if nothing was recorded (spec §4.6 — caller must not commit on
      /// `None`). Clears the track's in-progress buffer either way.
      pub fn finish(&mut self, track_id: &str) -> Option<Vec<AutomationPoint>>;
  }
  ```
  Task 9/10 call `sample` every tick and `finish` at pass-end.

- [ ] **Step 1: Write the failing tests**

Add to `automation.rs`'s `mod tests` (near the existing
`compile_gain_ramps` tests at line ~1569-1591):

```rust
#[test]
fn write_mode_records_every_tick_unconditionally() {
    let mut rec = AutomationRecorder::new();
    rec.sample("t-1", 0, AutomationMode::Write, false, 0.5);
    rec.sample("t-1", 10, AutomationMode::Write, false, 0.75);
    rec.sample("t-1", 20, AutomationMode::Write, false, 1.0);
    let points = rec.finish("t-1").expect("write mode must record");
    assert_eq!(
        points,
        vec![
            AutomationPoint { tick: 0, value: 0.5 },
            AutomationPoint { tick: 10, value: 0.75 },
            AutomationPoint { tick: 20, value: 1.0 },
        ]
    );
}

#[test]
fn touch_mode_records_only_while_touched() {
    let mut rec = AutomationRecorder::new();
    rec.sample("t-1", 0, AutomationMode::Touch, false, 0.2); // not touched: ignored
    rec.sample("t-1", 10, AutomationMode::Touch, true, 0.6);
    rec.sample("t-1", 20, AutomationMode::Touch, true, 0.8);
    rec.sample("t-1", 30, AutomationMode::Touch, false, 0.9); // released: ignored
    let points = rec.finish("t-1").expect("touch mode recorded while touched");
    assert_eq!(
        points,
        vec![
            AutomationPoint { tick: 10, value: 0.6 },
            AutomationPoint { tick: 20, value: 0.8 },
        ]
    );
}

#[test]
fn off_and_read_never_record() {
    let mut rec = AutomationRecorder::new();
    rec.sample("t-1", 0, AutomationMode::Off, true, 0.5);
    rec.sample("t-1", 10, AutomationMode::Read, true, 0.5);
    assert_eq!(rec.finish("t-1"), None);
}

#[test]
fn latch_mode_keeps_recording_after_release_until_finish() {
    // Unlike Touch, Latch must NOT stop recording when `touched` goes back
    // to false — it keeps sampling (holding whatever value it's given,
    // which in production is the live fader value sitting still) until the
    // pass truly ends at `finish`.
    let mut rec = AutomationRecorder::new();
    rec.sample("t-1", 0, AutomationMode::Latch, false, 0.2); // not yet armed: ignored
    rec.sample("t-1", 10, AutomationMode::Latch, true, 0.6); // touched: arms it
    rec.sample("t-1", 20, AutomationMode::Latch, false, 0.6); // released: KEEPS recording
    rec.sample("t-1", 30, AutomationMode::Latch, false, 0.6); // still recording
    let points = rec.finish("t-1").expect("latch mode must keep recording after release");
    assert_eq!(
        points,
        vec![
            AutomationPoint { tick: 10, value: 0.6 },
            AutomationPoint { tick: 20, value: 0.6 },
            AutomationPoint { tick: 30, value: 0.6 },
        ]
    );
}

#[test]
fn latch_arming_does_not_carry_across_finish() {
    let mut rec = AutomationRecorder::new();
    rec.sample("t-1", 0, AutomationMode::Latch, true, 0.5);
    rec.finish("t-1");
    // A second pass must start un-armed again.
    rec.sample("t-1", 100, AutomationMode::Latch, false, 0.9);
    assert_eq!(rec.finish("t-1"), None, "a fresh Latch pass must not record before being touched again");
}

#[test]
fn finish_with_no_samples_returns_none() {
    let mut rec = AutomationRecorder::new();
    assert_eq!(rec.finish("never-touched"), None);
}

#[test]
fn finish_clears_the_track_so_a_second_pass_starts_fresh() {
    let mut rec = AutomationRecorder::new();
    rec.sample("t-1", 0, AutomationMode::Write, false, 0.1);
    assert!(rec.finish("t-1").is_some());
    assert_eq!(rec.finish("t-1"), None, "a second finish with no new samples must be None");
}

#[test]
fn tracks_are_independent() {
    let mut rec = AutomationRecorder::new();
    rec.sample("t-1", 0, AutomationMode::Write, false, 0.1);
    rec.sample("t-2", 0, AutomationMode::Write, false, 0.9);
    assert_eq!(rec.finish("t-1").unwrap(), vec![AutomationPoint { tick: 0, value: 0.1 }]);
    assert_eq!(rec.finish("t-2").unwrap(), vec![AutomationPoint { tick: 0, value: 0.9 }]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml automation_recorder -- --nocapture` (adjust the filter to match whatever prefix the test names share, or run by exact test names)
Expected: FAIL to compile — `AutomationRecorder` doesn't exist.

- [ ] **Step 3: Implement `AutomationRecorder`**

Add to `src-tauri/src/plugins/automation.rs`, after `ParamAutomationDriver`'s
`impl` block (after line ~875, before the doc comment that precedes
`#[cfg(test)]` at line 889):

```rust
/// Records new track-gain automation points while the transport plays and
/// a track's mode is Write/Touch/Latch (spec
/// `docs/superpowers/specs/2026-08-18-automation-write-touch-latch-design.md`
/// §4). CONTROL THREAD ONLY, same reasoning as `ParamAutomationDriver`:
/// ticks are musical time, and the caller (Task 9/10) is what converts a
/// sample-domain playhead position to `tick` via `TempoMap` before calling
/// `sample`. One instance lives for the process; per-track in-progress
/// buffers are looked up by track id.
#[derive(Default)]
pub struct AutomationRecorder {
    in_progress: std::collections::HashMap<String, Vec<AutomationPoint>>,
    /// Latch-only: once a track has been touched during the current pass,
    /// it keeps recording every subsequent tick — even after `touched`
    /// goes back to false — until `finish` clears this. Touch has no
    /// equivalent: it stops the instant `touched` goes false, which is why
    /// this bookkeeping exists only for Latch.
    latch_armed: std::collections::HashMap<String, bool>,
}

impl AutomationRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sample(&mut self, track_id: &str, tick: u32, mode: AutomationMode, touched: bool, value: f32) {
        let recording = match mode {
            AutomationMode::Write => true,
            AutomationMode::Touch => touched,
            AutomationMode::Latch => {
                let armed = self.latch_armed.entry(track_id.to_string()).or_insert(false);
                if touched {
                    *armed = true;
                }
                *armed
            }
            AutomationMode::Off | AutomationMode::Read => false,
        };
        if !recording {
            return;
        }
        self.in_progress.entry(track_id.to_string()).or_default().push(AutomationPoint { tick, value });
    }

    pub fn finish(&mut self, track_id: &str) -> Option<Vec<AutomationPoint>> {
        self.latch_armed.remove(track_id);
        let points = self.in_progress.remove(track_id)?;
        if points.is_empty() { None } else { Some(points) }
    }
}
```

`AutomationMode` needs to be imported in `automation.rs` — add
`use crate::audio::types::AutomationMode;` near this file's other `use`
statements.

- [ ] **Step 4: Run tests to verify they pass**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml -- --nocapture` and filter to the eight new test names.
Expected: PASS (8 tests).

- [ ] **Step 5: Run the full Rust suite**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, no regressions.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/plugins/automation.rs
git commit -m "feat(automation): AutomationRecorder — pure Write/Touch/Latch point recording"
```

---

### Task 7: Share `GestureState` across the control/engine boundary + `is_track_gain_touched`

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (`GestureState` — how it's held by
  `ControlPlane`, and wherever `audio::init` constructs `Control` and hands
  it `committer`/`tables`/`shared`/`session`)
- Modify: `src-tauri/src/audio/mod.rs` or `src-tauri/src/audio/engine.rs`
  (wherever `Control`'s fields and constructor are — search `struct
  Control {` and its `fn new`)
- Test: `src-tauri/src/control/mod.rs`'s test module

**Interfaces:**
- Consumes: `OpenGesture`, `CoalesceKey`/`CoalesceTarget`
  (`control/mod.rs:967-987`, `1134-1154`) — read-only.
- Produces: `GestureState::is_track_gain_touched(&self, track_id: &str) ->
  bool`, and `GestureState` becomes constructible/shareable as
  `Arc<GestureState>` so `Control` can hold a clone. Task 9 consumes this.

**Before Step 1:** find EXACTLY how `Control` is constructed today — grep
`Control {` (the struct literal, not the type) in `src-tauri/src/audio/`
and read the constructor that also builds `committer: Committer` (the same
constructor Task-adjacent code you already read at
`src-tauri/src/audio/engine.rs:222,251` builds `committer`). `GestureState`
must be threaded through that SAME constructor call, alongside `committer`/
`tables`/`shared`/`session` — mirror whichever of those four is passed the
same way `ControlPlane::new` also holds it (i.e. both `ControlPlane` and
`Control` end up holding a clone of the SAME `Arc<GestureState>`, exactly
like they already share one `SharedGraphTables` and one `Committer`'s
underlying `Arc<Mutex<Session>>`).

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/control/mod.rs`'s test module, near `GestureState`'s
other tests (search `GestureState` in the test module for the nearest
existing gesture test to place this beside):

```rust
#[test]
fn is_track_gain_touched_reflects_the_open_gesture() {
    let gesture = GestureState::new();
    assert!(!gesture.is_track_gain_touched("t-1"));

    let (_run_id, stale) = gesture.begin("gain drag".into(), op::Actor::User { id: None });
    assert!(stale.is_none());
    // Fold a Set{Track(t-1), Gain} commit into the open gesture the same
    // way `commit_transient_and_fold` does in production — read that
    // method's body (mod.rs, near `close_gesture`) for the exact call shape
    // to reuse here rather than reaching into `OpenGesture`'s private
    // fields directly.

    assert!(gesture.is_track_gain_touched("t-1"));
    assert!(!gesture.is_track_gain_touched("t-2"), "a different track must not read as touched");
}
```

(`GestureState::new` is currently private (`fn new`, mod.rs:1178) — if this
test needs to live outside the module that already has access, either add
the test inside `control::mod`'s own test module, which already has
visibility, or leave `new` as-is; do not widen its visibility beyond what
the test needs.)

- [ ] **Step 2: Run test to verify it fails**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml is_track_gain_touched -- --nocapture`
Expected: FAIL to compile — the method doesn't exist.

- [ ] **Step 3: Add the query method**

In `src-tauri/src/control/mod.rs`, in `impl GestureState` (starts line
1177), add:

```rust
    /// True while there is a currently open gesture with a folded commit
    /// addressing `track_id`'s gain (`PropPath::Gain`) — i.e. the user has
    /// an active fader drag on this track right now. Read-only: does not
    /// touch or close the open gesture. Used by the automation recorder
    /// (Touch/Latch modes) on the engine control thread.
    pub fn is_track_gain_touched(&self, track_id: &str) -> bool {
        let guard = self.0.lock();
        let Some(g) = guard.as_ref() else { return false };
        g.last.iter().any(|(key, _)| {
            key.matches_track_gain(track_id) // see Step 3b if CoalesceKey has no such helper yet
        })
    }
```

**Step 3b:** `CoalesceKey`'s fields are private to this module
(`kind: &'static str`, plus whatever else `CoalesceKey` holds — re-read its
full definition near `control/mod.rs:986` before writing this, the excerpt
you have only shows the `kind` field and doc comments). Since
`is_track_gain_touched` lives in the SAME module (`control::mod`),
it can match `CoalesceKey`'s actual private fields directly instead of
needing a new public helper — write the match against
`CoalesceTarget::Object(op::ObjectRef::Track(id)) if id == track_id` and
whatever field holds the `PropPath` (`Some(op::PropPath::Gain)`), following
`CoalesceKey`'s real field names as found by reading its full struct
definition, not the illustrative `key.matches_track_gain(...)` placeholder
above.

- [ ] **Step 4: Share `GestureState` as `Arc` and hand `Control` a clone**

In `ControlPlane` (mod.rs:178), change:
```rust
    gesture: GestureState,
```
to:
```rust
    gesture: Arc<GestureState>,
```
Update `ControlPlane::new` (wherever it constructs `gesture: GestureState::new()`)
to `gesture: Arc::new(GestureState::new())`, and clone it into whatever
argument list builds the engine's `Control` (the same constructor call this
step's preamble told you to find) — add a `gesture: Arc<GestureState>`
field to `Control`'s struct definition and constructor, passed the SAME
`Arc` `ControlPlane` holds (mirror exactly how `committer: Committer` is
already passed to both sides — `Committer` itself wraps `Arc<Mutex<Session>>`
internally, so this is the same sharing shape one level up).

Every existing internal use of `self.gesture` inside `ControlPlane` (there
are several — `gesture_begin`, `gesture_end`, `set_track_mix`'s
`commit_transient_and_fold` call, etc.) keeps working unchanged: `Arc<T>`
derefs to `&T` automatically, so `self.gesture.commit_transient_and_fold(...)`
compiles exactly as before.

- [ ] **Step 5: Run test to verify it passes**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml is_track_gain_touched -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Run the full Rust suite**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS. This step touches a widely-used field
(`ControlPlane.gesture`) and a cross-module constructor
(`Control`'s) — treat any compile error here as a real signal to trace
down, not noise.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/control/mod.rs src-tauri/src/audio/mod.rs src-tauri/src/audio/engine.rs
git commit -m "feat(automation): share GestureState with the engine control thread; add is_track_gain_touched"
```

---

### Task 8: Per-rebuild caching on `Control` — automation mode by slot, `TempoMap`, live gain getter

**Files:**
- Modify: `src-tauri/src/audio/rt.rs:249-300` (`ParamTable` — add
  `gain_linear` getter)
- Modify: `src-tauri/src/audio/engine.rs` (`struct Control` field list near
  line 1018-1030, and wherever `rebuild()` currently builds/stores
  `param_automation`/ramps — the same rebuild path `compile_track_ramps` is
  called from, near engine.rs:1682 and :1802)
- Test: `src-tauri/src/audio/rt.rs`'s test module (for the getter),
  `src-tauri/src/audio/engine.rs`'s test module (for the cache refresh)

**Interfaces:**
- Consumes: `AutomationMode` (Task 1), `TempoMap` (already exists,
  `src-tauri/src/midi/tempo.rs`).
- Produces: `ParamTable::gain_linear(&self, slot: usize) -> f32`; `Control`
  gains two fields, `automation_modes: Vec<AutomationMode>` (slot-indexed,
  refreshed every rebuild, defaulting an out-of-range/unmapped slot to
  `Read`) and `tempo_map: Option<TempoMap>` (or reuse whatever field
  already caches the rebuild's `TempoMap` if one already exists under a
  different name — check before adding a duplicate). Task 9 reads both
  every tick.

- [ ] **Step 1: Write the failing test for `gain_linear`**

In `src-tauri/src/audio/rt.rs`'s test module, near `ParamTable`'s other
tests:

```rust
#[test]
fn gain_linear_reads_back_what_set_gain_linear_wrote() {
    let t = ParamTable::with_slots(4);
    t.set_gain_linear(2, 0.5);
    assert_eq!(t.gain_linear(2), 0.5);
}

#[test]
fn gain_linear_out_of_range_slot_returns_unity() {
    let t = ParamTable::with_slots(2);
    assert_eq!(t.gain_linear(99), 1.0);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `timeout 60 cargo test --manifest-path src-tauri/Cargo.toml gain_linear_ -- --nocapture`
Expected: FAIL — `gain_linear` doesn't exist.

- [ ] **Step 3: Add the getter**

In `src-tauri/src/audio/rt.rs`, in `impl ParamTable` (starts line 249),
add right after `set_gain_linear` (ends line 274):

```rust
    pub fn gain_linear(&self, slot: usize) -> f32 {
        self.gain.get(slot).map_or(1.0, |g| f32::from_bits(g.load(Ordering::Relaxed)))
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `timeout 60 cargo test --manifest-path src-tauri/Cargo.toml gain_linear_ -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Cache per-slot automation mode and the tempo map at rebuild**

Read `Control`'s full field list (near engine.rs:1018-1030) and its
`rebuild()` method (the caller of `compile_track_ramps` at engine.rs:1802,
and the earlier one at :1682) before editing — confirm whether a
`TempoMap` is already cached on `Control` under some other name (search
`self.map` / `self.tempo_map` / `TempoMap` inside `Control`'s `impl` block)
so this step does not duplicate one. Then:

Add to `struct Control`'s field list (near line 1018-1030):

```rust
    /// Slot-indexed automation mode, refreshed every rebuild from
    /// `store.tracks` — read every control-thread tick by
    /// `drive_automation_recording` (Task 9) without touching the session
    /// lock on the hot path, same shape as `param_automation`/gain ramps.
    automation_modes: Vec<crate::audio::types::AutomationMode>,
```

(If no existing `TempoMap` cache is found, add one the same way — a field
plus assignment at the same point in `rebuild()` where `automation_modes`
is populated below; if one already exists, reuse it and skip adding a new
field.)

In `rebuild()`, at the point where `compile_track_ramps` is called (mirror
whichever of the two call sites — engine.rs:1682 or :1802 — is the real
production rebuild path; the other may be test-only, check both call
sites' surrounding functions before picking), add right after it:

```rust
        self.automation_modes = (0..n_slots)
            .map(|slot| {
                s.store
                    .tracks
                    .iter()
                    .find(|t| slots.get(&t.id) == Some(&slot))
                    .map_or(crate::audio::types::AutomationMode::Read, |t| t.automation_mode)
            })
            .collect();
```

(`s`/`slots`/`n_slots` names above are illustrative — use whatever the
surrounding `rebuild()` code actually calls its `SessionSnapshot`/slot-map/
slot-count locals; read the ~30 lines around the `compile_track_ramps` call
site before writing this so the variable names match exactly.)

Also initialize `automation_modes: Vec::new()` in every place `Control` is
constructed (mirror how `param_automation:
crate::plugins::automation::ParamAutomationDriver::empty()` is initialized
at all FOUR sites you already found: engine.rs:253, :1787, :1799, :3916).

- [ ] **Step 6: Write a test that the cache tracks Off/Write/etc across a rebuild**

Add a test near `Control`'s other rebuild tests (search for an existing
test that inspects `param_automation` or ramps after a rebuild and mirror
its setup) asserting that after adding a track with `automation_mode:
AutomationMode::Off` and triggering a rebuild, `automation_modes[slot] ==
AutomationMode::Off`.

- [ ] **Step 7: Run to verify it passes, then the full suite**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml automation_modes -- --nocapture`
Expected: PASS.
Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, no regressions.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/audio/rt.rs src-tauri/src/audio/engine.rs
git commit -m "feat(automation): ParamTable::gain_linear getter and per-slot automation-mode cache on Control"
```

---

### Task 9: Tick-loop wiring — `drive_automation_recording`

**Files:**
- Modify: `src-tauri/src/audio/engine.rs` (add `drive_automation_recording`
  near `drive_param_automation`, line 1835-1851; call it from `Control::run`,
  line 1244; add an `AutomationRecorder` field to `Control`)
- Test: `src-tauri/src/audio/engine.rs`'s test module

**Interfaces:**
- Consumes: `AutomationRecorder` (Task 6), `automation_modes`/`tempo_map`
  cache (Task 8), `GestureState::is_track_gain_touched` (Task 7),
  `ParamTable::gain_linear` (Task 8), `self.shared.playing`/`self.shared.position`
  (existing, `drive_param_automation`'s pattern, engine.rs:1836-1839).
- Produces: `Control.automation_recorder: AutomationRecorder`, samples fed
  every tick while playing. Task 10 calls `self.automation_recorder.finish(...)`
  at pass-end and commits the result — this task does NOT commit anything
  yet, so its own tests only assert what `finish` WOULD return, without
  wiring the commit (kept as its own reviewable step in Task 10).

- [ ] **Step 1: Write the failing test**

Add near `drive_param_automation`'s existing tests (search for how that
method's tests construct a playing `Control` with a known
`self.shared.position`/`self.shared.playing`, and a track — mirror that
setup exactly):

```rust
#[test]
fn drive_automation_recording_samples_a_write_mode_track_every_tick() {
    // Build a `Control` (or test harness) the same way
    // `drive_param_automation`'s tests do, with one track whose
    // `automation_mode` is `Write`, transport playing, a known
    // `self.shared.position`, and a known live gain
    // (`self.tables.params.set_gain_linear(slot, 0.5)` or however the
    // existing param-automation tests set up live values).
    // ...
    ctl.drive_automation_recording();
    let recorded = ctl.automation_recorder.finish(&track_id);
    assert!(recorded.is_some(), "Write mode must have sampled this tick");
}

#[test]
fn drive_automation_recording_does_nothing_when_stopped() {
    // Same setup, but `self.shared.playing` false.
    ctl.drive_automation_recording();
    assert!(ctl.automation_recorder.finish(&track_id).is_none());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml drive_automation_recording -- --nocapture`
Expected: FAIL to compile — `drive_automation_recording` and the
`automation_recorder` field don't exist.

- [ ] **Step 3: Add the field**

In `struct Control`'s field list, add:

```rust
    automation_recorder: crate::plugins::automation::AutomationRecorder,
```

Initialize it at all four `Control`-construction sites (same four as Task
8 Step 5) with `crate::plugins::automation::AutomationRecorder::new()`.

- [ ] **Step 4: Implement `drive_automation_recording`**

Add right after `drive_param_automation` (ends line 1851):

```rust
    /// Samples every playing track's live fader value into
    /// `automation_recorder`, once per control-thread tick, for any track
    /// whose cached mode is Write/Touch/Latch. Mirrors
    /// `drive_param_automation`'s guard shape exactly (same `shared.playing`/
    /// `shared.position` reads) — see spec §4.1 for why this lives on the
    /// control thread and not in the frontend.
    fn drive_automation_recording(&mut self) {
        if !self.shared.playing.load(Relaxed) {
            return;
        }
        let Some(map) = &self.tempo_map else { return }; // Task 8's cached TempoMap (actual field name per Task 8)
        let pos = self.shared.position.load(Relaxed);
        let tick = map.samples_to_tick(pos) as u32;
        for (track_id, &slot) in &self.slots {
            let Some(&mode) = self.automation_modes.get(slot) else { continue };
            if matches!(mode, crate::audio::types::AutomationMode::Off | crate::audio::types::AutomationMode::Read) {
                continue;
            }
            let touched = self.gesture.is_track_gain_touched(track_id);
            let value = self.tables.params.gain_linear(slot);
            self.automation_recorder.sample(track_id, tick, mode, touched, value);
        }
    }
```

`self.slots` (a `TrackId -> usize` map) and `self.tempo_map`/`self.gesture`/
`self.tables` are illustrative names — confirm each against `Control`'s
REAL field names before writing this (`self.tables` you already confirmed
at engine.rs's `drive_param_automation` neighbors; `self.slots` needs
confirming — `Control` may store the slot map under a different name, or
only inside `SessionSnapshot`/rebuild-local state, in which case cache a
`slots: HashMap<TrackId, usize>` field the same way `automation_modes` was
cached in Task 8 rather than assuming it already exists as a field).

- [ ] **Step 5: Call it from the tick loop**

In `Control::run` (line 1215-1251), add right after
`self.drive_param_automation();` (line 1244):

```rust
            self.drive_automation_recording();
```

- [ ] **Step 6: Run to verify it passes, then the full suite**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml drive_automation_recording -- --nocapture`
Expected: PASS (2 tests).
Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, no regressions.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/audio/engine.rs
git commit -m "feat(automation): sample live fader values into AutomationRecorder every control-thread tick"
```

---

### Task 10: Commit-on-stop — lane splice, zero-point guard, undo entry

**Files:**
- Modify: `src-tauri/src/audio/engine.rs` (add `finish_automation_recording`
  or similar near `commit_recording_finalize` (line 3021-3043); call it at
  every pass-end trigger: transport stop, mode change, and — for Touch only
  — gesture close)
- Test: `src-tauri/src/audio/engine.rs`'s test module (integration-level:
  full record-then-read-back)

**Interfaces:**
- Consumes: `AutomationRecorder::finish` (Task 6), `self.committer` (Task 9
  context — same field `commit_recording_finalize` already uses),
  `Op::AutomationSetLane` (existing).
- Produces: nothing new for later tasks — this is the last engine-side
  piece. Task 11 (close-out) only runs the suite and updates docs.

**Before Step 1:** read `commit_recording_finalize` (engine.rs:3021-3043)
and `commit_recording_stopped_state` (engine.rs:3054+) in full — they are
the exact template for this step's commit call (`self.committer.clone()`,
`commit_with_rebuild`, `op::TxMeta::engine(...)`, `Ok(())` from the closure,
`true`/`|| self.rebuild()`). Also read `normalize_lane`
(`plugins/automation.rs:116-135`) — the merge step below must produce a
lane that already satisfies its invariants (sorted, deduped) so it does not
need a second normalize pass, but calling `normalize_lane` on the merged
lane before committing is the safe, cheap way to guarantee that regardless.

- [ ] **Step 1: Write the failing integration test**

Add near `commit_recording_finalize`'s existing tests:

```rust
#[test]
fn stopping_playback_commits_a_write_pass_as_one_automation_set_lane_op() {
    // Build a playing Control with one track in Write mode, an existing
    // lane with a couple of points, and drive a few ticks of
    // `drive_automation_recording` with varying live gain (mirror Task 9's
    // test setup, but let several ticks accumulate before stopping).
    // ...
    ctl.finish_automation_recording_for_stop(); // or whatever this task names it
    let lane = ctl.session_snapshot_lane_for(&track_id); // read back via whatever
    // accessor existing tests use to inspect `session.automation.lanes`
    // after a commit.
    assert!(lane.points.iter().any(|p| p.tick == /* one of the sampled ticks */ 0));
    // And: it is ONE undo entry.
    assert_eq!(ctl.committer.log().depths().0 - undo_before, 1);
}

#[test]
fn a_pass_with_no_samples_commits_nothing() {
    // A track in Write mode, but `drive_automation_recording` never called
    // (e.g. stopped immediately) — finishing must not push an undo entry.
    let undo_before = ctl.committer.log().depths().0;
    ctl.finish_automation_recording_for_stop();
    assert_eq!(ctl.committer.log().depths().0, undo_before);
}

#[test]
fn touch_release_reverts_to_the_pre_existing_curve_after_the_recorded_range() {
    // A track in Touch mode with an existing lane point AFTER the touched
    // range. Touch-record a short range, release (gesture close), and
    // assert the pre-existing point after the recorded range is UNCHANGED
    // — i.e. the merge only replaced points inside [start_tick, end_tick).
}

#[test]
fn latch_holds_the_last_value_after_release_until_transport_stop() {
    // A track in Latch mode. Drive a few ticks touched (varying value),
    // then a few more ticks NOT touched (drive_automation_recording still
    // samples them — Task 6's latch_armed keeps it recording), THEN stop.
    // Assert the committed lane's points after the release tick hold the
    // LAST touched value flat (not the pre-existing curve, unlike Touch)
    // — this is the behavioral difference the two tests together pin down.
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml automation_set_lane_op -- --nocapture`
Expected: FAIL — the finish/commit method doesn't exist yet.

- [ ] **Step 3: Implement the merge + commit**

Add near `commit_recording_finalize` (after it, before
`commit_recording_stopped_state`):

```rust
    /// Ends every currently-recording track's pass (spec §4.5) and commits
    /// each non-empty result as one `Op::AutomationSetLane`, through the
    /// SAME `Committer` (and the same `TxMeta::engine(...)`-attributed,
    /// non-transient, undo-tracked shape) `commit_recording_finalize`
    /// already uses to register a finished audio take. Points outside the
    /// recorded range are kept; points inside are replaced (whole-lane
    /// replace, mirroring the manual lane-edit path — Track D ruling 5).
    fn finish_automation_recording_for_track(&mut self, track_id: &str) {
        let Some(mut new_points) = self.automation_recorder.finish(track_id) else { return }; // spec §4.6
        let start = new_points.iter().map(|p| p.tick).min().unwrap();
        let end = new_points.iter().map(|p| p.tick).max().unwrap();

        let lane_key = format!("track:{track_id}:gain"); // confirm the REAL lane-key convention against how the UI's manual lane-edit path (AutomationLaneView / automation_set) builds a key for a track-gain lane — do not invent a new key scheme if one already exists.
        let committer = self.committer.clone();
        let track_id = track_id.to_string();
        let _ = committer.commit_with_rebuild(
            op::TxMeta::engine(format!("record automation: {track_id}")),
            |tx| {
                let existing = tx
                    .store()
                    .automation // confirm the real accessor for `session.automation.lanes` off `tx`
                    .lanes
                    .iter()
                    .find(|l| l.target_node == format!("track:{track_id}") && l.param_id == crate::plugins::automation::TRACK_PARAM_GAIN)
                    .cloned();
                let mut lane = existing.unwrap_or_else(|| crate::plugins::automation::AutomationLane {
                    id: lane_key.clone(),
                    target_node: format!("track:{track_id}"),
                    param_id: crate::plugins::automation::TRACK_PARAM_GAIN,
                    points: Vec::new(),
                });
                lane.points.retain(|p| p.tick < start || p.tick > end);
                lane.points.append(&mut new_points);
                crate::plugins::automation::normalize_lane(&mut lane)?;
                tx.apply(op::Op::AutomationSetLane { key: lane.id.clone(), lane: Some(lane) })?;
                Ok(())
            },
            true,
            || self.rebuild(),
        );
    }

    fn finish_automation_recording_for_stop(&mut self) {
        let ids: Vec<String> = self.slots.keys().cloned().collect(); // real field name per Task 9
        for id in ids {
            self.finish_automation_recording_for_track(&id);
        }
    }
```

Before finalizing this step, resolve the two flagged uncertainties by
reading the real code rather than guessing: (1) how the manual lane-edit
command (`automation_set`, frontend `AutomationLaneView.svelte`'s call
path, backend handler) builds an `AutomationLane.id`/key for a
track-gain lane — reuse that exact convention so a recorded pass updates
the SAME lane a manual edit would, not a second shadow lane; (2) the exact
method name to read `session.automation.lanes` off an open `Tx` (`tx.store()`
above is illustrative — `Tx` may expose automation lanes through a
different accessor, check `session::Tx`'s methods).

- [ ] **Step 4: Wire the three pass-end triggers**

- Transport stop: call `self.finish_automation_recording_for_stop()`
  wherever `Control` handles a stop (the same place
  `commit_recording_stopped_state` is called from `stop_recording`, plus
  ordinary transport-stop handling if that is a different code path — check
  both).
- Mode change: this is a document edit (Task 2's `set_track_mix`), which
  already triggers `effect.rebuild = true`. `Control::rebuild()` is the
  natural place to detect "a track that WAS Write/Touch/Latch no longer is"
  by comparing the previous `automation_modes` cache (Task 8) to the
  freshly computed one, BEFORE overwriting the cached field, and call
  `finish_automation_recording_for_track` for any track whose mode changed
  away from a recording mode.
- Touch gesture close only (not Latch): `close_gesture` lives in
  `control/mod.rs`, not `audio/engine.rs` — `Control` cannot observe a
  gesture closing directly today. For THIS plan's scope, ending a Touch
  pass at transport-stop-or-mode-change (already covered above) rather than
  exactly at gesture-release is an acceptable v1 simplification IF a Touch
  pass ending slightly late (continuing to record until the next tick after
  release, since `is_track_gain_touched` already goes false the instant the
  gesture closes) already produces the correct trimmed lane — re-read
  `AutomationRecorder::sample`'s touched-gating (Task 6): once
  `is_track_gain_touched` returns false, Touch STOPS being sampled that
  same tick, so the recorded range already ends at release with no extra
  wiring needed. Confirm this reasoning holds before adding a separate
  gesture-close hook — if it does, this bullet requires no additional code
  beyond what Steps 3-4's other two triggers already cover.

- [ ] **Step 5: Run tests to verify they pass**

Run: `timeout 120 cargo test --manifest-path src-tauri/Cargo.toml -- --nocapture` filtered to this task's four new test names.
Expected: PASS (4 tests).

- [ ] **Step 6: Run the full Rust suite**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS, no regressions.

- [ ] **Step 7: Manual smoke test (owner ear-check substitute — record it as owed, do not skip silently)**

This plan cannot ear-check the feature. Note in the final report (Task 11)
that a manual pass — record a Write take on a real track, stop, reopen the
project, confirm the curve is there and audible — is still owed, matching
this project's existing convention for RT-adjacent features (see
next-prompt.md's "Owner ear-checks" list).

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/audio/engine.rs
git commit -m "feat(automation): commit a finished Write/Touch/Latch recording pass as one AutomationSetLane op"
```

---

### Task 11: Close-out — full suite, counts, handoff

**Files:**
- Modify: `README.md`, `CONTRIBUTING.md` (dated test-count lines)
- Modify: `docs/PHASE4-PLAN.md` (Track D handoff section — mark this
  leftover landed, mirroring how the DOM-test-environment leftover and the
  non-blocking-CLAP-params leftover were each closed out in that same
  section: a `~~struck~~ → closed (date, branch)` line with a one-paragraph
  summary)
- Modify: `next-prompt.md` (only if still the active handoff pointer when
  this lands — update the Landed table and the Track D leftovers bullet;
  skip if a newer handoff has superseded it by the time this task runs)

**Interfaces:** none — this task produces no code symbols, only
measurement and documentation.

- [ ] **Step 1: Run the full backend and frontend suites, foreground, timeout-guarded**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Run: `timeout 300 npx vitest run`
Record the exact pass counts from both outputs.

- [ ] **Step 2: Update the dated count lines**

In `README.md` and `CONTRIBUTING.md`, find the existing dated test-count
line(s) (search for the most recent date stamp near a test count) and add
a new dated line with today's counts from Step 1, following the file's
existing format exactly.

- [ ] **Step 3: Update `docs/PHASE4-PLAN.md`'s Track D handoff**

In the "Track D handoff" section (starts line 707), add a closing entry for
this leftover — mirror the existing closed-deferral shape at lines 871-908
(the non-blocking-CLAP-params closure): what landed, the branch name
(`automation-write-touch-latch`), the commit range, and a one-line pointer
to the spec (`docs/superpowers/specs/2026-08-18-automation-write-touch-latch-design.md`).
Note the still-owed manual ear-check from Task 10 Step 7 explicitly, so it
is not silently dropped.

- [ ] **Step 4: Commit**

```bash
git add README.md CONTRIBUTING.md docs/PHASE4-PLAN.md
git commit -m "docs(automation): close out the Write/Touch/Latch automation-modes leftover"
```
