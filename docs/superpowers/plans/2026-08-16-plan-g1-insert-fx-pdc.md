# Plan G1 — Insert FX Chains + PDC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a track host an ordered chain of CLAP/LV2 *effects* (not a stock FX suite) and keep every track time-aligned at the master by compiling plugin-delay compensation in the same slice.

**Architecture:** Insert membership is document state on `TrackState.inserts` (stable slot UUIDs). Plugin *instances* stay in `PluginDoc` — same host, param UI, and state blobs as instruments. Rebuild (Plan F published snapshot, session lock released for assembly) compiles `source sum → inserts in order → PDC delay → existing gain/pan/mute`. Delay lines and insert nodes are allocated on the control thread and pointer-swapped with the graph (RCU). The audio callback learns a new walk, not a new graph language. Offline bounce uses the same compile helpers.

**Tech Stack:** Rust (Tauri backend, `parking_lot`, `serde`, `clack-host`/`clack-extensions`, `livi`), Svelte 5 runes + TypeScript frontend, `cargo test` + `vitest`.

**Spec:** `docs/backlog/insert-fx-sends-sidechain.md` (product decision; **G1 only** this PR). Binding architecture: `docs/SCALABILITY.md` §1 (compiled schedule, PDC before sends, stable node UUIDs, channel-counted buffers), ADR 0006 (thin renderer), ADR 0008 (insert params are existing `TargetRef::PluginParam`), `docs/PHASE4-PLAN.md` (Plan F snapshot rebuild; Track D mixer-ramp ruling; Track F modulation; standing PDC-before-sends).

## Global Constraints

- **Baseline at branch point: MEASURE IT.** README at plan-write time (after #49/#50 on `origin/main`) says **938 backend (904 lib + 34 integration, plus 2 `#[ignore]`) + 488 frontend**. Never merge a task that lowers either count without replacing what it removed. Re-measure before Task 1 and write the real numbers into the first commit message if they differ.
- **G1 only.** No sends, no `kind: "bus"`, no sidechain taps, no envelope-follower, no stock FX (`aura.reverb` etc.). `dsp::Effect` is the *slot* contract; first implementations are CLAP/LV2 adapters plus test doubles.
- **RT discipline ([C1], ARCHITECTURE §13/§15.1):** no allocation, no locks, no tick→sample math, no blocking host calls on the audio callback. Delay lines, insert nodes, and scratch are allocated at rebuild.
- **Compiled schedule + RCU swap.** No in-place graph mutation. Add / remove / reorder / bypass / latency change = rebuild + pointer swap. Knob writes stay on the existing param ring.
- **Every edit goes through the transaction channel** as an op, one undo. No new side channels. `transact` closures must not panic (Plan F F-3).
- **New op enum arms are additive** and do **NOT** bump `OP_FORMAT_VERSION` (it stays **2**). `ChangeSet::from_ops` is an exhaustive match — a new arm is a compile error until mapped.
- **Frozen command *names* stay frozen**; new commands are additive. `plugin_instantiate` remains the instrument path. Register every new command in `src-tauri/src/lib.rs` `generate_handler!` (this repo allows additive registration; the file header is historical).
- **Thin renderer (ADR 0006):** frontend emits ops/gestures and renders pushed state. No authoritative insert list on the client.
- **Do not grow the frozen MCP roster** to drive inserts.
- **Channel-counted buffers.** G1 stays interleaved stereo (`ProcessBlock { samples, channels }`) like the rest of the mixer. Do not invent a stereo-only buffer type and do not invent planar buffers.
- **Rust edition/style:** follow the surrounding file. Doc comments explain *why*.
- Run `cargo test` (in `src-tauri/`), `npm test` and `npx svelte-check` before every commit that touches the corresponding half. Foreground, `timeout`-guarded.

---

## Scope rulings (G-1 .. G-12)

Recorded up front so a task implementer does not re-litigate them. Same job Plan F's F-1..F-12 does.

1. **G-1 — Inserts live on `TrackState.inserts`.** An ordered `Vec<InsertSlot>` with `#[serde(default)]`. Each slot is `{ id, instanceId, bypassed }`. `id` is a stable UUIDv4 (never an index — undo and automation survive reorder). Plugin instances stay in `PluginDoc`. `SessionSnapshot.tracks` already captures `TrackState`, so inserts ride for free. Not a top-level `session.inserts` document.

2. **G-2 — PDC delay lines are not RT-allocated.** `DelayLine` buffers are sized at rebuild (`delay_samples + MAX_LIVE_BLOCK` frames, interleaved) and stored on `RtTrack.pdc`. The callback only advances a write index. A plugin reporting new latency is a structural recompile, never an RT computation.

3. **G-3 — Insert FX are document-modulated like any plugin param.** Bindings already address `TargetRef::PluginParam { instance_id, param_id }`. No new `TargetRef` arm. Bypass is a *slot* property, not a plugin param, and is **not** modulated in G1 (same reservation Track F made for mute).

4. **G-4 — Dedicated additive ops; `OP_FORMAT_VERSION` stays 2.** `InsertAdd` / `InsertRemove` / `InsertReorder` / `InsertSetBypass`. No new `ObjectRef` or `PropPath` variant. Whole-list replace was rejected: inverses would be opaque and folding would fight reorder.

5. **G-5 — Bypass is a document field; toggling it is a rebuild.** The mixer skips `process()` for a bypassed slot (true bypass). **Bypassed inserts still report latency**, so toggling bypass does not make the mix jump. This is G1 coarseness (a click-rebuild, not a new atomic table). A later round may lift it to `AtomicBool` on the reused node, like mute.

6. **G-6 — `plugin_instantiate` stays instrument-only.** New `insert_add` instantiates effects. Instruments are rejected on insert slots; effects are rejected on the instrument slot (`set_track_instrument` grows that check). CLAP/LV2 host negotiation today *also* rejects effects (no note port) — Task 4 grows an explicit `HostRole::{Instrument, Effect}`. The existing test `rejects_effects_and_bad_bundles_politely` stays on the instrument path.

7. **G-7 — One instance, one slot.** An instance cannot be both instrument and insert, nor appear on two tracks. `insert_add` always instantiates a new instance (no "bind this existing effect"). `plugin_remove` sweeps any insert slot that references the removed id.

8. **G-8 — Inserts are legal on `audio` and `midi` tracks.** Illegal on `automation` (no mixer slot) and on `bus` (`add_track` still rejects `bus`). Pre-fader: source → inserts → fader.

9. **G-9 — Track path latency = instrument `latency_samples()` + sum of insert latencies (bypass still counts).** Compensating delay sits post-insert, pre-fader. G1 has one sink (master). Latency is read from the **host on the control thread** (`clap_host::latency_samples` / `lv2_host::latency_samples`), never from the RT node. A change versus the last compiled table queues `ControlMsg::Rebuild`.

10. **G-10 — `TrackRemove` apply does not `PluginRemove`.** Slots ride on `TrackState`, so undo of a track delete restores the list. Plugin instances would leak if nobody destroyed them: `ControlPlane::remove_track` and the `insert_remove` command **compose** `PluginRemove` in the same transaction. `apply_raw` `PluginRemove` sweeps dangling slots so a journal replay of `PluginRemove` alone cannot leave a stale `instanceId`.

11. **G-11 — Mixer walk is source-sum → inserts in order → PDC → existing gain/pan/mute.** Empty inserts + zero delay is **byte-identical** to today's path (the pin that Task 5 must not lose). Clips and the live instrument are summed *first* so an insert sees the track, not one source. Offline bounce calls the same compile helpers. Monitoring (`render_live_input_only`) and launch overlay walk the same strip.

12. **G-12 — No MCP, no stock FX, no sends, no schemaVersion bump.** `schemaVersion` stays at whatever Track F writes (v4). Readers tolerate a missing `inserts` key. New Tauri commands are additive and listed as `generate_handler!` lines in the task that introduces them.

---

## File Structure

**Created:**

| File | Responsibility |
|---|---|
| `src-tauri/src/audio/insert.rs` | `InsertNode`, `InsertNodeCell`, `InsertNodeRegistry`, test doubles (`GainHalfEffect`, `LatencyDummy`), `compile_inserts` |
| `src-tauri/src/audio/pdc.rs` | `DelayLine`, `track_latency`, `compile_pdc` |
| `src/lib/components/InsertChain.svelte` | per-track insert list (add / bypass / reorder / remove) |
| `src/lib/state/inserts.svelte.test.ts` | frontend store tests for insert commands |
| `src/lib/utils/plugin-binding.test.ts` | extended: insert membership (file already exists) |

**Modified:** `src-tauri/src/audio/types.rs` (`InsertSlot`, `TrackState.inserts`), `src-tauri/src/audio/dsp.rs` (`AudioProcessor::latency_samples`, `Effect` used for real), `src-tauri/src/audio/rt.rs` (`RtTrack.inserts` / `pdc` / `track_buf`), `src-tauri/src/audio/mixer.rs` (strip walk), `src-tauri/src/audio/engine.rs` (rebuild compile + latency poll), `src-tauri/src/audio/offline.rs` (same compile), `src-tauri/src/audio/mod.rs` (`mod insert; mod pdc;`), `src-tauri/src/control/op.rs` (four arms), `src-tauri/src/control/session.rs` (`apply_raw` + PluginRemove sweep), `src-tauri/src/control/snapshot.rs` (`ChangeSet::from_ops`), `src-tauri/src/control/mod.rs` (`insert_*` + `remove_track` composition), `src-tauri/src/plugins/mod.rs` (`instantiate_and_activate_effect`, commands), `src-tauri/src/plugins/clap_host.rs` (`HostRole`, audio-in, `latency_samples`), `src-tauri/src/plugins/lv2_host.rs` (same), `src-tauri/src/lib.rs` (`generate_handler!` lines), `docs/ipc-schemas/track-state.schema.json`, `src/lib/types/ipc.ts`, `src/lib/tauri.ts`, `src/lib/state/plugins.svelte.ts`, `src/lib/utils/plugin-binding.ts`, `src/lib/components/plugins/PluginBrowser.svelte`, `src/lib/components/TrackHeader.svelte`, `src/lib/demo.ts` (fake compressor becomes an insert, not a rejection-only fixture).

**Deliberately untouched:** MCP roster, `OP_FORMAT_VERSION`, `kind: "bus"`, send edges, sidechain taps, `GainAutomatedNode`, Track D gesture/undo shape, `TargetRef` (no new arm), stock DSP.

---

# Phase G1a — Document + channel

## Task 1: InsertSlot on TrackState — LANDED PR #52 (`5c338ff`)

**Files:**
- Modify: `src-tauri/src/audio/types.rs` (`InsertSlot`, `TrackState.inserts`, `testutil::test_track`)
- Modify: every `TrackState {` literal in the tree (compiler will list them). Known sites: `control/ops.rs`, `control/op.rs`, `control/session.rs`, `control/snapshot.rs`, `control/history.rs`, `control/vergraph.rs`, `control/replay.rs`, `control/mod.rs`, `audio/project.rs`, `audio/offline.rs`, `audio/engine.rs`, `midi/playback.rs`, `midi_out/mod.rs`, `plugins/clap_host.rs`, `plugins/lv2_host.rs`, `src-tauri/tests/identity_properties.rs`, `src-tauri/tests/channel_properties.rs`
- Modify: `docs/ipc-schemas/track-state.schema.json` (optional `inserts` array; `additionalProperties` stays false)
- Test: inline `#[cfg(test)]` in `types.rs`

**Interfaces:**
- Consumes: existing `TrackState` serde (`rename_all = "camelCase"`).
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertSlot {
    pub id: String,
    pub instance_id: String,
    #[serde(default)]
    pub bypassed: bool,
}

// on TrackState:
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub inserts: Vec<InsertSlot>,
```

- [x] **Step 1: Write the failing test**

In `src-tauri/src/audio/types.rs`:

```rust
#[cfg(test)]
mod insert_slot_tests {
    use super::*;

    #[test]
    fn insert_slot_wire_is_camel_case() {
        let s = InsertSlot {
            id: "slot-1".into(),
            instance_id: "inst-1".into(),
            bypassed: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""instanceId":"inst-1""#), "got: {json}");
        assert!(json.contains(r#""bypassed":true"#), "got: {json}");
        let back: InsertSlot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn missing_inserts_key_deserializes_as_empty() {
        let v = serde_json::json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "name": "Vox",
            "kind": "audio",
            "gainDb": 0.0,
            "pan": 0.0,
            "muted": false,
            "soloed": false,
            "armed": false,
            "color": "#7c9cff"
        });
        let t: TrackState = serde_json::from_value(v).unwrap();
        assert!(t.inserts.is_empty(), "pre-G1 project.json must load");
    }

    #[test]
    fn empty_inserts_are_omitted_from_the_wire() {
        let t = testutil::test_track("t-1");
        let v = serde_json::to_value(&t).unwrap();
        assert!(v.get("inserts").is_none(), "skip_serializing_if empty keeps old files small");
    }
}
```

- [x] **Step 2: Run the test to verify it fails**

Run: `cd src-tauri && cargo test audio::types::insert_slot_tests`
Expected: FAIL — `InsertSlot` does not exist / `TrackState` has no `inserts`.

- [x] **Step 3: Write the implementation**

Add `InsertSlot` next to `TrackState` in `types.rs`. Add the field with the serde attrs above. Put `inserts: Vec::new()` on **every** struct literal the compiler names — do not add `Default` to `TrackState` to hide missing fields. Update `testutil::test_track` so new tests inherit an empty list.

In `track-state.schema.json` add (not required, so old files validate):

```json
"inserts": {
  "description": "Ordered insert-FX slots. Slot id is a stable UUID; instanceId names a PluginInstanceInfo.id. Absent means empty.",
  "type": "array",
  "items": {
    "type": "object",
    "required": ["id", "instanceId"],
    "properties": {
      "id": { "type": "string", "format": "uuid" },
      "instanceId": { "type": "string" },
      "bypassed": { "type": "boolean", "default": false }
    }
  }
}
```

Do not add `instrumentId` to the schema in this task (it is already a known additive gap).

- [x] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS — baseline plus the three new tests. Every `TrackState {` site compiles.

- [x] **Step 5: Commit**

```bash
git add src-tauri/src src-tauri/tests docs/ipc-schemas/track-state.schema.json
git commit -m "feat(inserts): InsertSlot on TrackState, empty-default for old projects"
```

---

## Task 2: Channel ops, apply_raw, ChangeSet

**Files:**
- Modify: `src-tauri/src/control/op.rs` (four new `Op` arms at the end of the enum, before the closing `}`)
- Modify: `src-tauri/src/control/session.rs` (`apply_raw` arms + `PluginRemove` sweep)
- Modify: `src-tauri/src/control/snapshot.rs` (`ChangeSet::from_ops` exhaustive match)
- Test: inline `#[cfg(test)]` in `session.rs` next to the plugin-op tests (~line 3553)

**Interfaces:**
- Consumes: `InsertSlot`, `Session.store.tracks`, `EngineEffect.rebuild` / `persist.project`.
- Produces:

```rust
// Op arms — all #[serde(rename_all = "camelCase")] via the enum
InsertAdd { track_id: TrackId, slot: InsertSlot, index: usize },
InsertRemove { track_id: TrackId, slot: InsertSlot, index: usize },
InsertReorder { track_id: TrackId, slot_id: String, from: usize, to: usize },
InsertSetBypass { track_id: TrackId, slot_id: String, bypassed: bool },
```

Inverses: `InsertAdd` ↔ `InsertRemove` (store-truth index); `InsertReorder` swaps `from`/`to` after the move; `InsertSetBypass` flips the bool from store truth.

- [ ] **Step 1: Write the failing tests**

In `session.rs` tests (reuse the existing `session_mutex` / `commit` helpers the plugin tests use):

```rust
fn slot(id: &str, instance: &str) -> crate::audio::types::InsertSlot {
    crate::audio::types::InsertSlot {
        id: id.into(),
        instance_id: instance.into(),
        bypassed: false,
    }
}

#[test]
fn insert_add_appends_and_inverse_removes() {
    let m = session_with_one_audio_track("t-1");
    let c = commit(&m, |tx| {
        tx.apply(Op::InsertAdd {
            track_id: "t-1".into(),
            slot: slot("s-1", "p-1"),
            index: usize::MAX,
        })
    });
    assert!(c.effect.rebuild, "REBUILD PIN: mixer reads inserts");
    assert_eq!(m.lock().store.tracks[0].inserts[0].id, "s-1");
    // inverse is InsertRemove
    match &c.inverses[0] {
        Op::InsertRemove { slot, .. } => assert_eq!(slot.id, "s-1"),
        other => panic!("expected InsertRemove, got {other:?}"),
    }
    commit(&m, |tx| tx.apply(c.inverses[0].clone()));
    assert!(m.lock().store.tracks[0].inserts.is_empty(), "undo removes the slot");
}

#[test]
fn insert_add_rejects_unknown_track_duplicate_slot_and_automation_kind() {
    let m = session_with_one_audio_track("t-1");
    assert!(commit_err(&m, |tx| tx.apply(Op::InsertAdd {
        track_id: "nope".into(),
        slot: slot("s-1", "p-1"),
        index: 0,
    }))
    .contains("unknown track"));
    commit(&m, |tx| {
        tx.apply(Op::InsertAdd { track_id: "t-1".into(), slot: slot("s-1", "p-1"), index: 0 })
    });
    assert!(commit_err(&m, |tx| tx.apply(Op::InsertAdd {
        track_id: "t-1".into(),
        slot: slot("s-1", "p-2"),
        index: 0,
    }))
    .contains("duplicate"));
    // automation track
    let m2 = session_with_kind("t-a", "automation");
    assert!(commit_err(&m2, |tx| tx.apply(Op::InsertAdd {
        track_id: "t-a".into(),
        slot: slot("s-1", "p-1"),
        index: 0,
    }))
    .contains("automation"));
}

#[test]
fn insert_reorder_is_a_rebuild_and_undo_restores_order() {
    let m = session_with_one_audio_track("t-1");
    commit(&m, |tx| {
        tx.apply(Op::InsertAdd { track_id: "t-1".into(), slot: slot("s-a", "p-a"), index: 0 })?;
        tx.apply(Op::InsertAdd { track_id: "t-1".into(), slot: slot("s-b", "p-b"), index: 1 })
    });
    let c = commit(&m, |tx| {
        tx.apply(Op::InsertReorder {
            track_id: "t-1".into(),
            slot_id: "s-b".into(),
            from: 1,
            to: 0,
        })
    });
    let ids: Vec<_> = m.lock().store.tracks[0].inserts.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["s-b", "s-a"]);
    assert!(c.effect.rebuild);
    commit(&m, |tx| tx.apply(c.inverses[0].clone()));
    let ids: Vec<_> = m.lock().store.tracks[0].inserts.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["s-a", "s-b"], "undo restores order; slot ids unchanged");
}

#[test]
fn insert_set_bypass_is_a_rebuild_and_keeps_the_slot() {
    let m = session_with_one_audio_track("t-1");
    commit(&m, |tx| {
        tx.apply(Op::InsertAdd { track_id: "t-1".into(), slot: slot("s-1", "p-1"), index: 0 })
    });
    let c = commit(&m, |tx| {
        tx.apply(Op::InsertSetBypass {
            track_id: "t-1".into(),
            slot_id: "s-1".into(),
            bypassed: true,
        })
    });
    assert!(m.lock().store.tracks[0].inserts[0].bypassed);
    assert!(c.effect.rebuild, "G-5: bypass is a rebuild in G1");
    commit(&m, |tx| tx.apply(c.inverses[0].clone()));
    assert!(!m.lock().store.tracks[0].inserts[0].bypassed);
}

#[test]
fn plugin_remove_sweeps_dangling_insert_slots() {
    let m = session_with_one_audio_track("t-1");
    // Seed a plugin row so PluginRemove is legal.
    commit(&m, |tx| {
        tx.apply(Op::PluginAdd {
            row: crate::plugins::PluginInstanceInfo {
                id: "p-1".into(),
                uid: "clap:/x.clap#fx".into(),
                name: "Fx".into(),
                format: "clap".into(),
                status: "stub".into(),
                track_id: Some("t-1".into()),
            },
            index: usize::MAX,
        })?;
        tx.apply(Op::InsertAdd { track_id: "t-1".into(), slot: slot("s-1", "p-1"), index: 0 })
    });
    commit(&m, |tx| {
        tx.apply(Op::PluginRemove {
            row: m.lock().plugins.instances[0].clone(),
            index: 0,
            state: None,
            params: vec![],
        })
    });
    assert!(
        m.lock().store.tracks[0].inserts.is_empty(),
        "G-10: PluginRemove must not leave a dangling instanceId"
    );
}

#[test]
fn changeset_from_insert_ops_flags_tracks() {
    use crate::control::snapshot::ChangeSet;
    let add = Op::InsertAdd {
        track_id: "t-1".into(),
        slot: slot("s-1", "p-1"),
        index: 0,
    };
    let cs = ChangeSet::from_ops(&[add]);
    assert!(cs.tracks, "insert list is on TrackState");
    assert!(!cs.plugins, "InsertAdd does not touch PluginDoc; PluginAdd beside it does");
}
```

Copy the exact `commit` / `session_mutex` helper names from the plugin-op tests already in this file (`session_owns_plugin_rows` neighbourhood). If the helper is `transact` rather than `commit`, use that. Do not invent a second session fixture.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test insert_add_appends -- --nocapture`
Expected: FAIL — unresolved `Op::InsertAdd` / non-exhaustive `ChangeSet::from_ops`.

- [ ] **Step 3: Write the implementation**

`apply_raw` rules:
- `InsertAdd`: reject unknown track; reject `kind` not in `audio|midi`; reject empty `slot.id` / `slot.instance_id`; reject duplicate `slot.id` on that track; reject duplicate `instance_id` anywhere in the session (G-7, including `instrument_id == "plugin:<id>"`); clamp `index` to `inserts.len()`; `effect.rebuild = true`; `effect.persist.project = true`.
- `InsertRemove`: find by `slot.id` (store truth wins over `index`); inverse carries the store-truth index.
- `InsertReorder`: find `slot_id`; `from` is advisory; move to `to.min(len-1)`.
- `InsertSetBypass`: find `slot_id`; write store-truth inverse.
- `PluginRemove` (existing arm): after removing the row, walk every track's `inserts` and `retain(|s| s.instance_id != removed.id)`.

`ChangeSet::from_ops`: map all four insert arms to `cs.tracks = true`. No wildcard.

`OP_FORMAT_VERSION` stays 2. Do not add fields to `TrackAdd` — inserts ride on `track: TrackState`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS — baseline plus the new session/changeset tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/control
git commit -m "feat(inserts): InsertAdd/Remove/Reorder/SetBypass ops, PluginRemove sweep"
```

---

## Task 3: Commands, effect instantiate gate, lib.rs registration

**Files:**
- Modify: `src-tauri/src/plugins/mod.rs` (`instantiate_and_activate_effect`, `insert_add` / `insert_remove` / `insert_reorder` / `insert_set_bypass`)
- Modify: `src-tauri/src/control/mod.rs` (`ControlPlane::insert_add` etc.; `remove_track` composes `PluginRemove`)
- Modify: `src-tauri/src/audio/mod.rs` (`set_track_instrument` rejects `!is_instrument` instances)
- Modify: `src-tauri/src/lib.rs` — **register in `generate_handler!`** (see Step 3)
- Test: extend `plugins/mod.rs` `instantiate_requires_scan_and_known_instrument_uid`; new control-plane tests next to `set_track_instrument`

**Interfaces:**
- Consumes: `instantiate_and_activate` (unchanged, still rejects effects), `Op::PluginAdd` + `Op::InsertAdd` in one `commit`.
- Produces:

```rust
pub fn instantiate_and_activate_effect(
    registry: &Arc<Mutex<PluginRegistry>>,
    uid: &str,
) -> Result<(PluginInstanceInfo, Vec<ParamInfo>), String>;

#[tauri::command]
pub async fn insert_add(track_id: String, uid: String, index: Option<usize>, ...) -> Result<InsertSlot, String>;
#[tauri::command]
pub fn insert_remove(track_id: String, slot_id: String, ...) -> Result<(), String>;
#[tauri::command]
pub fn insert_reorder(track_id: String, slot_id: String, to_index: usize, ...) -> Result<(), String>;
#[tauri::command]
pub fn insert_set_bypass(track_id: String, slot_id: String, bypassed: bool, ...) -> Result<(), String>;
```

- [ ] **Step 1: Write the failing tests**

In `plugins/mod.rs` tests, next to `instantiate_requires_scan_and_known_instrument_uid`:

```rust
#[test]
fn instantiate_effect_rejects_instruments_and_unknown_uids() {
    let reg = Arc::new(Mutex::new(scanned_registry()));
    assert!(
        instantiate_and_activate_effect(&reg, "lv2:urn:test:synth").is_err(),
        "instruments stay on the instrument slot"
    );
    assert!(instantiate_and_activate_effect(&reg, "lv2:urn:nope").is_err());
    // The *gate* accepts an effect uid. The host call is Task 4; this test
    // only asserts we no longer bounce !is_instrument before looking up.
    // With the fake scanned "TestFx", the function must get past the
    // is_instrument check and fail at the host (or succeed once Task 4 lands).
    let err = instantiate_and_activate_effect(&reg, "clap:/x.clap#fx").err();
    if let Some(e) = err {
        assert!(
            !e.contains("v1 hosts instruments"),
            "effect rejection must not use the instrument-only message, got: {e}"
        );
    }
}

#[test]
fn instantiate_still_rejects_effects_on_the_instrument_path() {
    let reg = Arc::new(Mutex::new(scanned_registry()));
    let err = instantiate_and_activate(&reg, "clap:/x.clap#fx").unwrap_err();
    assert!(
        err.contains("effect") || err.contains("instruments"),
        "plugin_instantiate stays instrument-only, got: {err}"
    );
}
```

In `control/mod.rs` tests (same fixture as `set_track_instrument_goes_through_the_channel_and_rebuilds_once` — `test_plane_with_tracks`):

```rust
fn fx_row(id: &str, track: &str) -> crate::plugins::PluginInstanceInfo {
    crate::plugins::PluginInstanceInfo {
        id: id.into(),
        uid: "clap:/x.clap#fx".into(),
        name: "Fx".into(),
        format: "clap".into(),
        status: "stub".into(),
        track_id: Some(track.into()),
    }
}

#[test]
fn insert_add_is_one_transaction_plugin_plus_slot() {
    let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
    plane
        .commit(TxMeta::user("insert add"), |tx| {
            tx.apply(Op::PluginAdd { row: fx_row("p-1", "t-1"), index: usize::MAX })?;
            tx.apply(Op::InsertAdd {
                track_id: "t-1".into(),
                slot: crate::audio::types::InsertSlot {
                    id: "s-1".into(),
                    instance_id: "p-1".into(),
                    bypassed: false,
                },
                index: usize::MAX,
            })
        })
        .unwrap();
    {
        let s = plane.session().lock();
        assert_eq!(s.plugins.instances.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["p-1"]);
        assert_eq!(s.store.tracks[0].inserts[0].instance_id, "p-1");
    }
    plane.undo().unwrap();
    let s = plane.session().lock();
    assert!(s.plugins.instances.is_empty(), "undo removes the PluginAdd");
    assert!(s.store.tracks[0].inserts.is_empty(), "undo removes the InsertAdd");
}

#[test]
fn remove_track_composes_plugin_remove_for_insert_instances() {
    let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
    plane
        .commit(TxMeta::user("seed inserts"), |tx| {
            tx.apply(Op::PluginAdd { row: fx_row("p-1", "t-1"), index: usize::MAX })?;
            tx.apply(Op::PluginAdd { row: fx_row("p-2", "t-1"), index: usize::MAX })?;
            tx.apply(Op::InsertAdd {
                track_id: "t-1".into(),
                slot: crate::audio::types::InsertSlot {
                    id: "s-1".into(),
                    instance_id: "p-1".into(),
                    bypassed: false,
                },
                index: 0,
            })?;
            tx.apply(Op::InsertAdd {
                track_id: "t-1".into(),
                slot: crate::audio::types::InsertSlot {
                    id: "s-2".into(),
                    instance_id: "p-2".into(),
                    bypassed: false,
                },
                index: 1,
            })
        })
        .unwrap();
    plane.remove_track("t-1", TxMeta::user("remove track")).unwrap();
    assert!(
        plane.session().lock().plugins.instances.is_empty(),
        "G-10: remove_track must PluginRemove insert instances so they do not leak"
    );
    plane.undo().unwrap();
    let s = plane.session().lock();
    assert_eq!(s.store.tracks[0].id.as_str(), "t-1");
    assert_eq!(s.store.tracks[0].inserts.len(), 2, "TrackAdd restores slots");
    assert_eq!(s.plugins.instances.len(), 2, "undo restores the PluginAdds");
}

#[test]
fn set_track_instrument_rejects_an_effect_instance() {
    let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
    plane
        .commit(TxMeta::user("seed fx"), |tx| {
            tx.apply(Op::PluginAdd { row: fx_row("p-fx", "t-1"), index: usize::MAX })
        })
        .unwrap();
    let err = plane
        .set_track_instrument("t-1", Some("plugin:p-fx".into()), TxMeta::user("bind fx as inst"))
        .unwrap_err();
    assert!(
        err.contains("effect") || err.contains("insert"),
        "an effect instance cannot occupy the instrument slot, got: {err}"
    );
    assert_eq!(plane.session().lock().store.tracks[0].instrument_id, None);
}
```

`set_track_instrument` today only checks `plugin_exists`. This task adds the `!is_instrument` / already-an-insert rejection. The scanned-uid lookup may not be available on `test_plane_with_tracks` (no scan). If the fixture has no registry, key the rejection off `PluginInstanceInfo` once Task 4 stamps a `role` — until then, reject when the instance id already appears in any `inserts` list, **and** when a new `PluginInstanceInfo.kind` / descriptor lookup says effect. Prefer looking up `uid` against the last `plugin_scan` cache (`PluginRegistry::scanned`); in the unit test, seed that cache the same way `scanned_registry()` does, or reject on uid suffix `#fx` only if you cannot reach the registry — do not ship a suffix heuristic in production code.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test instantiate_effect_rejects`
Expected: FAIL — `instantiate_and_activate_effect` does not exist.

- [ ] **Step 3: Write the implementation**

`instantiate_and_activate_effect` is a copy of `instantiate_and_activate` with the gate inverted (`if desc.is_instrument { return Err(...) }`). Host calls stay `clap_host::instantiate` / `lv2_host::register_instance` until Task 4; a real effect will fail host negotiation until then. That is accepted.

`insert_add` (async, `spawn_blocking`, prepare-outside — copy `plugin_instantiate`):
1. Validate track exists and `kind` is `audio|midi` (read under session lock, then drop it).
2. `instantiate_and_activate_effect`.
3. `commit` one tx: `PluginAdd { row, index: usize::MAX }` then `InsertAdd { track_id, slot: InsertSlot { id: new UUID, instance_id: row.id, bypassed: false }, index }`.
4. On commit failure, `destroy_orphan` (existing helper).

`insert_remove`: one tx: `InsertRemove` then `PluginRemove` (capture blob + params first, same as `plugin_remove`).

`insert_reorder` / `insert_set_bypass`: one op each.

`ControlPlane::remove_track`: inside the existing `commit`, after `TrackRemove`, `PluginRemove` each insert instance (blob captured before the lock, like `plugin_remove`). Order: `TrackRemove` first (slots leave with the row), then `PluginRemove`s (sweep is then a no-op). Undo applies inverses in reverse: `PluginAdd`s then `TrackAdd` (with inserts).

**Register in `src-tauri/src/lib.rs`** inside `generate_handler!`, immediately after `plugins::plugin_set_param`:

```rust
            plugins::insert_add,
            plugins::insert_remove,
            plugins::insert_reorder,
            plugins::insert_set_bypass,
```

`set_track_instrument`: after `plugin_exists`, look up the instance's `uid` in the scanned registry (or a `ControlPlane` helper that reads `PluginDoc` + last scan). If the descriptor is `!is_instrument`, reject. If the instance is already referenced from any `inserts` list, reject (G-7).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS. `svelte-check` is N/A (no frontend yet).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/plugins/mod.rs src-tauri/src/control/mod.rs src-tauri/src/audio/mod.rs src-tauri/src/lib.rs
git commit -m "feat(inserts): insert_add/remove/reorder/bypass commands, instrument/effect gates"
```

---

# Phase G1b — Host audio-in + mixer walk

## Task 4: Host negotiation for effects + latency_samples

**Files:**
- Modify: `src-tauri/src/audio/dsp.rs` (`AudioProcessor::latency_samples` default 0)
- Modify: `src-tauri/src/plugins/clap_host.rs` (`HostRole`, `negotiate_ports`, `ClapNode` input copy + replace mode, `latency_samples` API)
- Modify: `src-tauri/src/plugins/lv2_host.rs` (`register` role, `Lv2Node` input copy + replace mode, `latency_samples` API)
- Modify: `src-tauri/src/plugins/mod.rs` (`instantiate_and_activate_effect` calls `instantiate_effect` / `register_instance_effect`; `pub fn latency_of`)
- Test: `clap_host.rs` tests (keep `rejects_effects_and_bad_bundles_politely` on the instrument path); new effect-path tests that skip when no FX is installed; `dsp.rs` `null_effect_contract`

**Interfaces:**
- Consumes: existing `negotiate_ports`, `ClapNode::process` (ADD, silent inputs), `Lv2Node::process` (ADD).
- Produces:

```rust
pub enum HostRole { Instrument, Effect }

pub fn instantiate_effect(instance_id: &str, uid: &str) -> Result<Vec<ParamInfo>, String>;
pub fn latency_samples(instance_id: &str) -> usize; // clap_host and lv2_host

// AudioProcessor:
fn latency_samples(&self) -> usize { 0 }

// ClapNode / Lv2Node:
pub fn set_io_mode(&mut self, mode: IoMode); // Add (instrument) | Replace (effect)
```

`HostRole::Effect` negotiation: require ≥1 audio input and a mono-or-stereo main output. Note ports optional. Inputs of 1 or 2 channels are accepted (mono is duplicated). `HostRole::Instrument` is today's function, unchanged.

- [ ] **Step 1: Write the failing tests**

In `dsp.rs` tests:

```rust
#[test]
fn audio_processor_default_latency_is_zero() {
    let e = NullEffect::default();
    assert_eq!(e.latency_samples(), 0);
}
```

In `clap_host.rs` tests, **keep** `rejects_effects_and_bad_bundles_politely` (it calls `instantiate`, the instrument path). Add:

```rust
#[test]
fn instantiate_effect_accepts_a_stereo_fx_when_installed() {
    let Some(fx) = installed()
        .into_iter()
        .find(|d| !d.is_instrument && !d.uid.to_lowercase().contains("cardinal"))
    else {
        eprintln!("note: no CLAP effect installed; effect-host path skipped");
        return;
    };
    let id = format!("fx-{}", uuid::Uuid::new_v4());
    let params = instantiate_effect(&id, &fx.uid).expect("effect must host as HostRole::Effect");
    let _ = params;
    let lat = latency_samples(&id);
    // just a number; 0 is legal
    let _ = lat;
    remove(&id).expect("remove");
}

#[test]
fn clap_node_replace_mode_copies_input_and_does_not_add() {
    // Unit-level: construct a ProcessBlock of [0.5, 0.5, ...], run a
    // LatencyDummy/GainHalf through IoMode::Replace, assert the output
    // equals the processed input, not input+processed.
    // If this test cannot reach a real ClapNode without a plugin, put the
    // replace-vs-add assertion on GainHalfEffect in insert.rs (Task 5)
    // AND assert ClapNode::process in Replace mode writes (not +=) when a
    // plugin is present. Do not leave the ADD-vs-REPLACE pin untested.
}
```

Mirror the skip-cleanly pattern for LV2 if a known effect URI is present (Calf / Zam). Do not fail CI when none are installed.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test instantiate_effect_accepts`
Expected: FAIL — `instantiate_effect` does not exist. On a machine with no FX plugin the new test returns early; then fail `audio_processor_default_latency_is_zero` until the default method exists.

- [ ] **Step 3: Write the implementation**

`negotiate_ports(instance, clap_id, role)`:
- Shared: audio-ports ext, mono/stereo main out.
- `Instrument`: require note-ports + ≥1 note input (today's code path, verbatim).
- `Effect`: require `n_in >= 1` and each recorded input channel count in `{1, 2}`; **do not** require note ports.

`ClapNode::process`:
- Always copy `io.samples` into `in_bufs` when `io_mode == Replace` (de-interleave; mono input duplicates L into both plugin channels if the port is stereo).
- When `io_mode == Add` (default): keep feeding silence to inputs and `+=` into `io.samples` (instrument contract — do not change).
- When `io_mode == Replace`: **assign** plugin main-out into `io.samples` (overwrite). Mono plugin out duplicates to L/R.

`Lv2Node`: same split. Today's `audio_in` is already a buffer; fill it from `io.samples` in Replace mode.

Read CLAP latency at activate (already logged) and **store it on `ClapNode`**. Add `pub fn latency_samples(instance_id: &str) -> usize` that reads `PluginLatency::get` on the **hosted** main-thread instance (not the RT node). If the ext is missing, return 0.

LV2: if a control-output port has the `lv2:latency` designation (or is named `latency` as a fallback), report its current value as `usize`; else 0. Do not invent a Calf-specific path.

`instantiate_and_activate_effect` now calls `clap_host::instantiate_effect` / `lv2_host::register_instance_effect`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS. Instrument tests unchanged. `rejects_effects_and_bad_bundles_politely` still red on `instantiate(&fx.uid)`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/dsp.rs src-tauri/src/plugins
git commit -m "feat(plugins): HostRole::Effect, audio-in replace mode, latency_samples()"
```

---

## Task 5: Mixer strip — source → inserts → fader

**Files:**
- Create: `src-tauri/src/audio/insert.rs`
- Modify: `src-tauri/src/audio/mod.rs` (`pub mod insert;`)
- Modify: `src-tauri/src/audio/rt.rs` (`RtTrack.inserts`, `RtTrack.pdc`, `RtGraph.track_buf`; `RtGraph::new` allocates `track_buf` whenever any track has inserts **or** always when we unify the strip)
- Modify: `src-tauri/src/audio/mixer.rs` (`render_impl`, `render_live`, `render_live_input_only`)
- Test: `insert.rs` unit tests + new mixer tests next to `render_applies_gain_and_pan`. **All existing mixer tests must stay green.**

**Interfaces:**
- Consumes: `AudioProcessor`, `InsertSlot` (document) → compiled `InsertNode`.
- Produces:

```rust
pub struct InsertNode {
    pub slot_id: String,
    pub instance_id: String,
    pub bypassed: bool,
    pub latency: usize,
    pub proc: Arc<InsertNodeCell>,
}

pub struct InsertNodeRegistry { /* keyed by instance_id, same shape as LiveNodeRegistry */ }

pub fn compile_inserts(
    tracks: &[TrackState],
    plugins: &PluginDoc,
    rate: u32,
    nodes: &mut InsertNodeRegistry,
) -> HashMap<TrackId, Vec<InsertNode>>;

pub struct GainHalfEffect { pub bypassed: bool } // test double, implements AudioProcessor + Effect
pub struct InvertEffect; // test double: each sample *= -1
```

`InsertNodeCell` is `LiveNodeCell`'s sibling (`UnsafeCell<Box<dyn AudioProcessor>>`) with the same "one RT thread, one snapshot" safety comment.

- [ ] **Step 1: Write the failing tests**

In `insert.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::ProcessBlock;

    #[test]
    fn gain_half_halves_in_place() {
        let mut e = GainHalfEffect { bypassed: false };
        let mut buf = vec![1.0f32, -0.5, 0.25, 0.0];
        let mut io = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: 48_000, steady: None };
        e.process(&mut io);
        assert_eq!(buf, vec![0.5, -0.25, 0.125, 0.0]);
    }
}
```

In `mixer.rs` tests (reuse `clip`, `one_track_graph`, `render_simple`):

```rust
fn insert_on(g: &mut RtGraph, slot: usize, node: Box<dyn crate::audio::dsp::AudioProcessor>, bypassed: bool) {
    // helper the implementer writes: attach one InsertNode to tracks[slot]
}

#[test]
fn empty_inserts_are_byte_identical_to_today() {
    let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
    g.params.set_pan(0, -1.0);
    let mut a = vec![0.0f32; 8];
    render_simple(&mut g, 0, &LoopSpec::OFF, &mut a, 2);
    // attach an empty inserts vec explicitly and render again
    let mut b = vec![0.0f32; 8];
    render_simple(&mut g, 0, &LoopSpec::OFF, &mut b, 2);
    assert_eq!(a, b, "G-11 pin: no inserts must not change a single sample");
}

#[test]
fn insert_sees_the_sum_and_runs_before_the_fader() {
    let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
    g.params.set_gain_linear(0, 0.5);
    g.params.set_pan(0, -1.0);
    insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), false);
    let mut out = vec![0.0f32; 8];
    render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
    // clip 1.0 → insert 0.5 → fader 0.5 → 0.25 on the left
    assert!((out[0] - 0.25).abs() < 1e-6, "got {}", out[0]);
    assert!(out[1].abs() < 1e-6, "hard left");
}

#[test]
fn two_inserts_run_in_document_order() {
    let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
    g.params.set_pan(0, -1.0);
    insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), false);
    insert_on(&mut g, 0, Box::new(InvertEffect), false);
    let mut out = vec![0.0f32; 8];
    render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
    assert!((out[0] + 0.5).abs() < 1e-6, "1.0 → half → invert = -0.5, got {}", out[0]);
}

#[test]
fn bypassed_insert_is_true_bypass() {
    let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
    g.params.set_pan(0, -1.0);
    insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), true);
    let mut out = vec![0.0f32; 8];
    render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
    assert!((out[0] - 1.0).abs() < 1e-6, "bypassed GainHalf must not run, got {}", out[0]);
}

#[test]
fn clips_and_live_are_summed_before_inserts() {
    use super::super::dsp::AudioProcessor;
    use super::super::rt::{LiveNodeCell, LiveSource};
    struct ConstNode(f32);
    impl AudioProcessor for ConstNode {
        fn prepare(&mut self, _: u32, _: usize) {}
        fn process(&mut self, io: &mut ProcessBlock<'_>) {
            for s in io.samples.iter_mut() {
                *s += self.0;
            }
        }
        fn reset(&mut self) {}
    }
    impl crate::audio::dsp::LiveInstrument for ConstNode {
        fn queue_event(&mut self, _: crate::midi::synth::BlockNoteEvent) -> bool { true }
        fn all_notes_off(&mut self) {}
    }
    let mut g = one_track_graph(0, clip(0, 0, 4, vec![0.25; 4], 1));
    g.tracks[0].live = Some(LiveSource {
        node: LiveNodeCell::new(Box::new(ConstNode(0.25))),
        events: Arc::new(vec![]),
    });
    g.params.set_pan(0, -1.0);
    insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), false);
    let mut out = vec![0.0f32; 8];
    render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
    assert!(
        (out[0] - 0.25).abs() < 1e-6,
        "sum 0.50 then half = 0.25 (not per-source half). got {}",
        out[0]
    );
}

#[test]
fn launch_overlay_still_plays_the_scene_through_inserts() {
    let mut g = one_track_graph(0, clip(100, 0, 4, vec![1.0; 4], 1));
    g.params.set_flag(0, FLAG_LAUNCH, true);
    g.params.set_pan(0, -1.0);
    insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), false);
    let mut out = vec![0.0f32; 8];
    render_rt_launch(
        &mut g,
        0,
        &LoopSpec::OFF,
        &mut out,
        2,
        48_000,
        false,
        0,
        None,
        Some(LaunchPlayhead { pos: 100, discontinuity: true, exclusive: false, ended: false }),
        None,
    );
    assert!(
        (out[0] - 0.5).abs() < 1e-5,
        "overlay must hear the scene through the insert, got {}",
        out[0]
    );
}
```

**Sabotage check (Track D review lesson):** after these pass, temporarily skip the insert walk and confirm `insert_sees_the_sum_and_runs_before_the_fader` goes RED. A test that passes with inserts disabled is vacuous.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test audio::mixer insert_sees_the_sum`
Expected: FAIL — `RtTrack` has no `inserts` / helper missing.

- [ ] **Step 3: Write the implementation**

Mixer strip for each track, in runs of `MAX_LIVE_BLOCK` so `track_buf` stays preallocated:

```
zero track_buf[run*2]
mix clips for this run into track_buf          // no gain/pan yet
render live ADD into track_buf                 // extract from render_live; no gain/pan
for insert in tr.inserts:
    if insert.bypassed { continue }
    process Replace on track_buf
apply DelayLine if tr.pdc is Some              // Task 6 attaches this; Task 5 leaves None
apply gain * ramp * pan, mute, mix_out
fold meters from the post-fader samples
```

`RtGraph::new`: allocate `track_buf: vec![0.0; MAX_LIVE_BLOCK * 2]` whenever any track has `live` **or** `inserts` **or** `pdc`. Existing clip-only tests construct graphs without inserts — they must keep working. If the unified strip always uses `track_buf`, allocate it for every graph (simplest; 32 KiB). Prefer always-allocate so the empty-insert path is the same code as the insert path.

`render_live` currently applies gain/pan itself. Split: `render_live_into(track_buf)` (ADD, no fader) then the shared fader. Do not leave two fader implementations.

`render_live_input_only`: same strip minus clips (monitoring through FX).

Loop wrap: keep the existing `frame_pos` / `LoopSpec` split that `render_live` already does; a wrap is a run boundary. Clip mixing inside a run uses the same `frame_pos` as today.

Do **not** implement DelayLine here. `tr.pdc` stays `None`. Task 6 fills it.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test audio::mixer audio::insert`
Expected: PASS — every pre-existing mixer test (gain, pan, mute, loop, overlap, launch overlay, exclusive) plus the new ones.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio
git commit -m "feat(mixer): source-sum then insert chain then fader"
```

---

# Phase G1c — PDC + the same schedule offline

## Task 6: PDC compiler and DelayLine

**Files:**
- Create: `src-tauri/src/audio/pdc.rs`
- Modify: `src-tauri/src/audio/mod.rs` (`pub mod pdc;`)
- Modify: `src-tauri/src/audio/rt.rs` (`RtTrack.pdc: Option<DelayLine>`)
- Modify: `src-tauri/src/audio/mixer.rs` (apply `tr.pdc` in the strip where Task 5 left the hook)
- Modify: `src-tauri/src/audio/insert.rs` (`LatencyDummy` test double)
- Test: inline in `pdc.rs` + one mixer/offline alignment test

**Interfaces:**
- Consumes: per-track path latency (`usize`), `MAX_LIVE_BLOCK`.
- Produces:

```rust
pub struct DelayLine { /* private: buf: Vec<f32>, w: usize, delay: usize, channels: usize */ }
impl DelayLine {
    pub fn new(delay_samples: usize, max_block: usize, channels: usize) -> Self;
    /// RT-safe. `io` is interleaved `frames * channels`. Does not allocate.
    pub fn process(&mut self, io: &mut [f32]);
    pub fn delay(&self) -> usize;
    pub fn buf_len(&self) -> usize; // tests: sized at new(), never grows
}

pub fn track_latency(instrument: usize, insert_latencies: &[usize]) -> usize;
pub fn compile_pdc(path_latencies: &[usize]) -> Vec<usize>; // max - each

pub struct LatencyDummy { /* owns a DelayLine of `latency` samples */ }
impl LatencyDummy {
    pub fn new(latency: usize) -> Self;
}
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn compile_pdc_delays_the_shorter_paths() {
    assert_eq!(compile_pdc(&[256, 0, 64]), vec![0, 256, 192]);
    assert_eq!(compile_pdc(&[0, 0]), vec![0, 0]);
    assert_eq!(compile_pdc(&[]), Vec::<usize>::new());
}

#[test]
fn track_latency_sums_instrument_and_inserts() {
    assert_eq!(track_latency(10, &[20, 30]), 60);
    assert_eq!(track_latency(0, &[]), 0);
}

#[test]
fn delay_line_shifts_an_impulse_by_exactly_n_and_does_not_allocate() {
    let mut d = DelayLine::new(4, 8, 2);
    let cap = d.buf_len();
    let mut block = vec![1.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]; // 4 frames stereo
    d.process(&mut block);
    assert_eq!(block, vec![0.0; 8], "first block is the pre-roll of silence");
    let mut block2 = vec![0.0f32; 8];
    d.process(&mut block2);
    assert!((block2[0] - 1.0).abs() < 1e-9 && (block2[1] - 1.0).abs() < 1e-9);
    assert_eq!(d.buf_len(), cap, "G-2: process must not grow the buffer");
}

fn impulse_clip() -> RtClip {
    let mut data = vec![0.0f32; 512];
    data[0] = 1.0;
    clip(0, 0, 512, data, 1)
}

#[test]
fn two_tracks_line_up_when_one_has_256_samples_of_latency() {
    let mut dummy = LatencyDummy::new(256);
    dummy.prepare(48_000, crate::audio::rt::MAX_LIVE_BLOCK);
    let mut g = RtGraph::new(
        vec![
            RtTrack::clips(0, vec![impulse_clip()]),
            RtTrack::clips(1, vec![impulse_clip()]),
        ],
        1,
        Arc::new(ParamTable::default()),
    );
    g.params.set_pan(0, -1.0);
    g.params.set_pan(1, -1.0);
    insert_on(&mut g, 0, Box::new(dummy), false);
    g.tracks[0].pdc = None; // wet path: plugin supplies the 256
    g.tracks[1].pdc = Some(DelayLine::new(256, crate::audio::rt::MAX_LIVE_BLOCK, 2));
    let mut out = vec![0.0f32; 512 * 2];
    render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    let peak = left
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap();
    assert_eq!(peak.0, 256, "both impulses must land at 256, got {}", peak.0);
    assert!(
        (left[256] - 2.0).abs() < 1e-4,
        "A + B impulses coincide (2.0), got {}",
        left[256]
    );
}

#[test]
fn bypassed_insert_still_contributes_latency() {
    let mut dummy = LatencyDummy::new(256);
    dummy.prepare(48_000, crate::audio::rt::MAX_LIVE_BLOCK);
    let mut g = RtGraph::new(
        vec![
            RtTrack::clips(0, vec![impulse_clip()]),
            RtTrack::clips(1, vec![impulse_clip()]),
        ],
        1,
        Arc::new(ParamTable::default()),
    );
    g.params.set_pan(0, -1.0);
    g.params.set_pan(1, -1.0);
    insert_on(&mut g, 0, Box::new(dummy), true); // true bypass
    // G-5: compile_pdc still sees 256 on A, so BOTH tracks get 256 of PDC
    // (A's plugin does not delay; A's DelayLine does).
    g.tracks[0].pdc = Some(DelayLine::new(256, crate::audio::rt::MAX_LIVE_BLOCK, 2));
    g.tracks[1].pdc = Some(DelayLine::new(256, crate::audio::rt::MAX_LIVE_BLOCK, 2));
    let mut out = vec![0.0f32; 512 * 2];
    render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    assert!(
        (left[256] - 2.0).abs() < 1e-4,
        "bypass must not make A early; both at 256. left[0]={} left[256]={}",
        left[0],
        left[256]
    );
    assert!(left[0].abs() < 1e-6, "nothing at t=0");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test audio::pdc`
Expected: FAIL — module missing.

- [ ] **Step 3: Write the implementation**

`DelayLine::new`: `buf = vec![0.0; (delay_samples + max_block) * channels]`. If `delay_samples == 0`, `process` is a no-op (or the caller stores `None`).

`process`: for each frame, write incoming to `buf[w]`, read `buf[(w + buf_frames - delay) % buf_frames]` back into `io`, advance `w`. No `Vec` growth, no `log::`.

`LatencyDummy`: a test `AudioProcessor` that is itself a `DelayLine` of `latency` samples (so it *is* a lookahead stand-in). `latency_samples()` returns that `latency`. Bypass is the insert node's flag, not the dummy's.

Attach `Some(DelayLine::new(comp, MAX_LIVE_BLOCK, 2))` on `RtTrack.pdc` when `comp > 0`. Mixer Task 5 hook: `if let Some(d) = tr.pdc.as_mut() { d.process(track_buf_run) }`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test audio::pdc audio::mixer`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio
git commit -m "feat(pdc): compensating delay lines compiled from path latency"
```

---

## Task 7: Rebuild, offline bounce, latency-change recompile

**Files:**
- Modify: `src-tauri/src/audio/engine.rs` (`Control` grows `insert_nodes: InsertNodeRegistry`; `rebuild` phase 1 calls `compile_inserts` + `compile_pdc`; control-thread tick polls `plugins::latency_of`)
- Modify: `src-tauri/src/audio/offline.rs` (`build_graph` calls the same two helpers with a *private* `InsertNodeRegistry`)
- Modify: `src-tauri/src/plugins/mod.rs` (`pub fn latency_of(instance_id: &str) -> usize` dispatching clap/lv2)
- Test: `engine.rs` tests next to `rebuild_compiles_track_gain_lanes_by_slot`; `offline.rs` tests next to the demo bounce

**Interfaces:**
- Consumes: `SessionSnapshot.tracks` / `PluginDoc`, `compile_inserts`, `compile_pdc`, `track_latency`, `clap_host::latency_samples`, `lv2_host::latency_samples`.
- Produces: `RtTrack.inserts` + `RtTrack.pdc` attached **before** the graph is published (RCU, Track D ruling 1). A public(crate) helper both callers use:

```rust
pub(crate) fn attach_inserts_and_pdc(
    tracks: &mut [RtTrack],
    store_tracks: &[TrackState],
    plugins: &PluginDoc,
    rate: u32,
    nodes: &mut InsertNodeRegistry,
);
```

Latency poll (engine control thread, same ≤2 ms tick as `ParamAutomationDriver`):

```rust
fn poll_insert_latency(&mut self) {
    // compare plugins::latency_of(id) to the last compiled table
    // (a HashMap<String /*instance*/, usize> on Control).
    // mismatch → queue ControlMsg::Rebuild. Do not read the RT node.
}
```

Also include the track's instrument instance (if `instrument_id` is `plugin:<id>`) in the path latency (G-9).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn attach_inserts_and_pdc_puts_delay_on_the_dry_track() {
    // two tracks, one with a LatencyDummy insert of 256 (inject via the
    // #[cfg(test)] factory in insert.rs: insert_node_for_test("latency:256")),
    // one dry. After attach, dry track's pdc.delay() == 256, wet track's
    // pdc is None (or delay 0).
}

#[test]
fn offline_bounce_matches_the_live_compile() {
    // build_graph and attach_inserts_and_pdc on the same Store produce
    // the same per-track compensating delays. Then render 512 frames:
    // impulses line up at 256, same as Task 6's mixer test.
}

#[test]
fn latency_change_on_the_host_queues_a_rebuild() {
    // Unit-test the poll helper: compiled table says 0, latency_of stub
    // returns 256 → function returns true ("needs rebuild"). Next call
    // with matching 256 returns false.
}
```

Headless `engine::rebuild` in most backend tests does **not** assemble tracks (see the `headless` early-return). Do not try to assert inserts through a headless rebuild. The compile helper + `offline::build_graph` are the integration path.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test attach_inserts_and_pdc`
Expected: FAIL — helper missing.

- [ ] **Step 3: Write the implementation**

`insert_node_for` (production): look up `PluginDoc` row, `live_node_for`-style but `IoMode::Replace`, key `insert:{id}@{rate}#{state_rev}!{status}`. Reuse across rebuilds (plugin memory / delay tails survive).

`#[cfg(test)] insert_node_for_test(kind)`: `"gainhalf"` and `"latency:{n}"` so unit tests do not need a real plugin.

`attach_inserts_and_pdc`:
1. For each mixer-slot track, compile insert nodes in document order.
2. Path latency = instrument `latency_of` + each insert's compiled `latency` (bypass still counts).
3. `comps = compile_pdc(&path_latencies)`.
4. Attach `DelayLine` when `comps[i] > 0`.

`offline::build_graph`: after clips + `append_from`, call `attach_inserts_and_pdc` with a fresh registry (never the engine's). Same private-instance rule as live instruments.

Engine tick: call `poll_insert_latency` next to the param-automation driver. On change, `do_rebuild()` / the existing rebuild queue. Do not rebuild from the RT callback.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS — including existing offline demo bounce (no inserts → identical audio).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio src-tauri/src/plugins/mod.rs
git commit -m "feat(engine): compile inserts+PDC on rebuild and offline bounce"
```

---

# Phase G1d — UI (thin renderer)

## Task 8: IPC types, backend adapter, plugin-binding

**Files:**
- Modify: `src/lib/types/ipc.ts` (`InsertSlot`, `TrackState.inserts`)
- Modify: `src/lib/tauri.ts` (interface + `backend` methods)
- Modify: `src/lib/demo.ts` (demo backend implements the four commands; the fake compressor is addable as an insert)
- Modify: `src/lib/state/plugins.svelte.ts` (`insertAdd` / `insertRemove` / `insertReorder` / `insertSetBypass`; `remove` already exists)
- Modify: `src/lib/utils/plugin-binding.ts` + `plugin-binding.test.ts`
- Test: `src/lib/state/inserts.svelte.test.ts`, `src/lib/utils/plugin-binding.test.ts`

**Interfaces:**
- Consumes: the four Tauri commands from Task 3.
- Produces (TS):

```ts
export interface InsertSlot {
  id: string;
  instanceId: string;
  bypassed: boolean;
}
// TrackState.inserts?: InsertSlot[];

insertAdd(trackId: string, uid: string, index?: number): Promise<InsertSlot>;
insertRemove(trackId: string, slotId: string): Promise<void>;
insertReorder(trackId: string, slotId: string, toIndex: number): Promise<void>;
insertSetBypass(trackId: string, slotId: string, bypassed: boolean): Promise<void>;
```

- [ ] **Step 1: Write the failing tests**

`src/lib/utils/plugin-binding.test.ts` — add (keep existing instrument tests):

```ts
it("tracksBoundToInstance also finds an insert slot on an audio track", () => {
  const vox = {
    id: "a1",
    name: "Vox",
    kind: "audio" as const,
    inserts: [{ id: "s-1", instanceId: "comp-1", bypassed: false }],
  };
  expect(tracksBoundToInstance([vox], "comp-1").map((t) => t.id)).toEqual(["a1"]);
});

it("instanceConnectionLabel names an insert connection", () => {
  const vox = {
    id: "a1",
    name: "Vox",
    kind: "audio" as const,
    inserts: [{ id: "s-1", instanceId: "comp-1", bypassed: false }],
  };
  expect(instanceConnectionLabel([vox], "comp-1")).toBe("insert: Vox");
});
```

`src/lib/state/inserts.svelte.test.ts` (mirror `modulation.svelte.test.ts`: mock `backend.insertAdd` etc. via `vi.mock("../tauri")`):

```ts
const addCalls: { trackId: string; uid: string; index?: number }[] = [];
const bypassCalls: { trackId: string; slotId: string; bypassed: boolean }[] = [];
const reorderCalls: { trackId: string; slotId: string; toIndex: number }[] = [];

it("insertAdd invokes once and the returned slot is what the store keeps", async () => {
  // backend.insertAdd resolves to { id: "s-1", instanceId: "p-1", bypassed: false }
  // plugins.insertAdd("t-1", "clap:/x.clap#fx") returns that slot
  // addCalls has exactly one entry { trackId: "t-1", uid: "clap:/x.clap#fx" }
});

it("insertSetBypass sends the slot id and the bool, not a whole list", async () => {
  await plugins.insertSetBypass("t-1", "s-1", true);
  expect(bypassCalls).toEqual([{ trackId: "t-1", slotId: "s-1", bypassed: true }]);
});

it("insertReorder sends the slot id and the target index, not a whole list", async () => {
  await plugins.insertReorder("t-1", "s-1", 0);
  expect(reorderCalls).toEqual([{ trackId: "t-1", slotId: "s-1", toIndex: 0 }]);
});
```

Fill the first test's mock the same way `modulation.svelte.test.ts` fills `modulationSetCurve` — resolve a minted slot, push onto `addCalls`. Do not keep an authoritative insert array in `plugins.svelte.ts`; after each command call `project.reload()` (or `project.patchTrackLocal` with the returned track). The assertions above pin the **invoke shape** (ADR 0006).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/lib/utils/plugin-binding.test.ts src/lib/state/inserts.svelte.test.ts`
Expected: FAIL — `inserts` not on the binding helper / store methods missing.

- [ ] **Step 3: Write the implementation**

Extend `tracksBoundToInstance` to also match `t.inserts?.some(s => s.instanceId === instanceId)`. Connection label: if bound as instrument, keep `connection: …`; if bound only as insert, `insert: …`; if both (must not happen, G-7), prefer `connection:`.

`plugins.svelte.ts`: methods call `backend.insert*`, then `project.reload()` **or** patch `project.tracks` from the returned snapshot. Prefer a `project.reload()` after each successful command (thin renderer; do not keep an authoritative insert list in the plugin store). `plugin.remove` already unbinds `instrumentId`; also rely on the backend sweep (G-10) and reload.

`demo.ts`: implement the four commands against in-memory tracks so `vite dev` without Tauri still lists inserts. The existing fake compressor descriptor (`isInstrument: false`) becomes addable via `insertAdd`.

`tauri.ts`: add the four methods next to `pluginInstantiate`. Do not change `pluginInstantiate`'s signature.

- [ ] **Step 4: Run BOTH suites and the type checker**

Run: `cd src-tauri && cargo test` then `npx vitest run` then `npx svelte-check`
Expected: baseline + new frontend tests, svelte-check clean.

- [ ] **Step 5: Commit**

```bash
git add src/lib
git commit -m "feat(inserts): IPC types, backend adapter, insert membership in plugin-binding"
```

---

## Task 9: Insert chain UI

**Files:**
- Create: `src/lib/components/InsertChain.svelte`
- Modify: `src/lib/components/TrackHeader.svelte` (insert-count chip; opens the chain)
- Modify: `src/lib/components/plugins/PluginBrowser.svelte` (effects: "add to track…" for audio+midi; drop the "fx — v1 hosts instruments only" copy)
- Modify: `src/lib/components/plugins/PluginParamPanel.svelte` only if it assumes `instrumentId` (it must open for an insert instance too — `openPluginParams(instanceId)` already keys by id)
- Test: `src/lib/utils/plugin-binding.test.ts` already covers membership. Add a small pure helper if the chip label needs one (`insertSummary(track) => "2 fx"` / `"2 fx · 1 bypassed"`) with a unit test. **Do not** add a DOM test — there is no jsdom environment (Track D leftover). Keep logic out of the `.svelte` file.

**Interfaces:**
- Consumes: `plugins.insertAdd/Remove/Reorder/SetBypass`, `project.tracks`, `openPluginParams`.
- Produces: chrome only. Every click is one command.

- [ ] **Step 1: Write the failing test**

```ts
// src/lib/utils/insert-summary.test.ts
import { insertSummary } from "./insert-summary";

it("summarises an empty chain as empty string", () => {
  expect(insertSummary({ inserts: [] })).toBe("");
});

it("counts slots and bypassed", () => {
  expect(
    insertSummary({
      inserts: [
        { id: "a", instanceId: "1", bypassed: false },
        { id: "b", instanceId: "2", bypassed: true },
      ],
    }),
  ).toBe("2 fx · 1 bypassed");
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `npx vitest run src/lib/utils/insert-summary.test.ts`
Expected: FAIL — module missing.

- [ ] **Step 3: Write the implementation**

`InsertChain.svelte`: list `track.inserts` in order. Each row: plugin name (from `plugins.byId`), bypass toggle, up/down, params, remove. Footer: a `<select>` of `plugins.descriptors.filter(d => !d.isInstrument)` that calls `plugins.insertAdd(track.id, uid)`. Up/down call `insertReorder`. Bypass calls `insertSetBypass`. Remove calls `insertRemove`.

`TrackHeader.svelte`: for `audio` and `midi` tracks, a chip showing `insertSummary(track)` (hidden when empty). Click sets a local `chainOpen` flag and renders `InsertChain`. Do not put the list in a new mixer view.

`PluginBrowser.svelte`:
- Effects get the same instantiate `<select>` as instruments, but the options are **audio + midi** tracks and the handler calls `insertAdd`, not `instantiate`+`bind`.
- Delete the `fx — v1 hosts instruments only` note and the `.plug.fx { opacity: 0.55 }` dead-style if nothing else uses it.
- Keep the "instruments" filter checkbox; default can stay instruments-only so the instrument workflow does not get noisier.

ADR 0006: no time math, no local insert array that the backend does not own.

- [ ] **Step 4: Run frontend tests and svelte-check**

Run: `npx vitest run` then `npx svelte-check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib
git commit -m "feat(inserts): track insert chain UI, browser can add effects"
```

---

# Phase G1e — Close-out

## Task 10: Handoff, counts, pointers

**Files:**
- Modify: `docs/PHASE4-PLAN.md` (a "Plan G1 handoff" section in the shape Track F's uses: what landed, per-task commits, G-1..G-12, non-goals held, divergences)
- Modify: `docs/backlog/insert-fx-sends-sidechain.md` (status: G1 implemented, pointer to the plan + PR)
- Modify: `docs/backlog/00-ROADMAP-real-alternative.md` (Tier-1 row: G1 done, G2 not started)
- Modify: `next-prompt.md` (G1 landed; do **not** start G2; remaining ear-check)
- Modify: `README.md` + `CONTRIBUTING.md` (re-measured counts, dated)
- Modify: `docs/PHASE3-PLAN.md` §6 one-line note that "effects on audio tracks" is lifted by G1 (do not rewrite the phase-3 doc)

**Interfaces:** none.

- [ ] **Step 1: Re-measure both suites**

```
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
timeout 300 npx vitest run
```

Write the dated counts into README + CONTRIBUTING in the same commit.

- [ ] **Step 2: Write the Plan G1 handoff** in `docs/PHASE4-PLAN.md`: G-1..G-12 verbatim, per-task SHAs, anything that diverged (especially if bypass stayed a rebuild, and whether a real LV2/CLAP effect was heard). Name the owner's **ear check**: add a hosted EQ/comp to a vocal, play, bypass, confirm the dry/wet change and that a lookahead limiter on drums does not make the vocal early.

- [ ] **Step 3: Point next-prompt at G2 as later.** G2 is bus+sends and is blocked on this handoff existing. Do not start it in the same session.

- [ ] **Step 4: Commit**

```bash
git add docs README.md CONTRIBUTING.md next-prompt.md
git commit -m "docs: Plan G1 handoff, dated test counts"
```

---

## Non-goals (held, not silent)

- G2 bus tracks + sends, G3 sidechain taps, G4 envelope-follower.
- First-party reverb / delay / EQ / compressor DSP.
- VST3, Bitwig-style nested racks, per-voice FX, hardware insert, Live's "reduced latency when monitoring".
- Sample-accurate plugin-param automation (Track D/F leftover).
- Private per-bounce plugin instances (Track D leftover: plugin-param lanes still do not bounce).
- MCP tools for inserts.
- Live (non-rebuild) bypass.
- Feedback / cycles.

---

## Self-review

**Spec coverage** (`docs/backlog/insert-fx-sends-sidechain.md` G1 bullets):

| Requirement | Task |
|---|---|
| Per-track ordered insert list, stable slot UUIDs | T1, T2 |
| Lift `isInstrument` rejection for insert slots only | T3, T4 |
| Instruments stay on the instrument slot | T3 (`plugin_instantiate` + `set_track_instrument`) |
| Mixer: source → inserts in order → existing gain/pan/mute | T5 |
| Bypass per slot | T2, T5 (true bypass; latency kept — G-5) |
| Reorder = rebuild | T2 |
| `latency_samples()`, compensating delay | T4, T6 |
| Plugin latency change = recompile | T7 |
| Offline bounce same schedule | T7 |
| Additive ops, `OP_FORMAT_VERSION` = 2 | T2 |
| New commands registered in `lib.rs` | T3 |
| No stock FX suite; host CLAP/LV2 | T4, T5 doubles only |
| No sends / no sidechain | held |
| Compiled schedule + RCU | T5–T7 |
| Channel-routed mutations, one op = one undo | T2, T3 |
| Do not grow MCP roster | held |
| Channel-counted buffers | G1 stays interleaved `ProcessBlock` |
| UI: track inspector insert list, reuse browser, filter `!isInstrument` | T9 |
| Suggested cut G1a / G1b / G1c | phases G1a / G1b / G1c |

**Placeholder scan:** no TBD / TODO / "write tests for the above" / "similar to Task N" left in the steps. Helpers that already exist (`session` plugin-op fixture, `one_track_graph`, `scanned_registry`, clap `installed()` skip) are named, not paraphrased.

**Type consistency:** `InsertSlot { id, instance_id, bypassed }` in Rust ↔ `{ id, instanceId, bypassed }` on the wire. Ops are `InsertAdd` / `InsertRemove` / `InsertReorder` / `InsertSetBypass`. Commands are `insert_add` / `insert_remove` / `insert_reorder` / `insert_set_bypass`. Compile helpers are `compile_inserts`, `compile_pdc`, `track_latency`, `attach_inserts_and_pdc`. Host split is `HostRole::{Instrument, Effect}` and `IoMode::{Add, Replace}`.

**Known thin spots, called out rather than hidden:** Task 3's `insert_add_is_one_transaction_*` and `remove_track_composes_*` bodies depend on the control-plane test fixture already in `control/mod.rs` (`set_track_instrument` neighbourhood) — copy that harness, do not invent a second `ControlPlane` constructor. Task 4's replace-mode pin on a real `ClapNode` skips when no FX plugin is installed; `GainHalfEffect` in Task 5 is the CI-stable REPLACE pin. Task 7 cannot assert inserts through a headless `engine::rebuild` (existing [I5] early-return); `attach_inserts_and_pdc` + `offline::build_graph` are the path.
