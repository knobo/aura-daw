# Plan B — Identity Groundwork Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Typed per-family ids (`TrackId`, `ClipId`, `ContentId`, `SourceId`, `NoteId`), persisted note identity with a per-content watermark in the AMEV header, source-keyed assets and decode cache, slots as derived per-graph state with graph-versioned param tables and generation-tagged meter blocks, and the `MAX_TRACKS = 64` ceiling removed — closed by the Gate B test suite.

**Architecture:** A new crate-level `ids` module holds `#[serde(transparent)]` newtypes so no wire format moves. The AMEV chunk grows a length-prefixed appended-column mechanism (mask bit `0x0002`) carrying note ids + the `next_note_id` watermark. The engine's decode cache re-keys from clip id to `SourceId`. `Store`'s slot table (`alloc_slot`/`free_slot`) is deleted: every graph rebuild derives slots from display order and carries **its own** `ParamTable` + slot map, published to the control side as `GraphTables` versioned by a generation counter; meter blocks carry that generation and are folded under the matching slot map. With per-graph sizing the fixed `[_; MAX_TRACKS]` arrays and the `u64` presence mask become per-graph vectors and chunked meter blocks.

**Tech Stack:** Rust (src-tauri crate), serde, uuid, byteorder, rtrb, parking_lot 0.12, proptest (dev-dependency, already present).

**Spec:** `docs/CORE-REDESIGN-ROUND-2.md` §2 (identity: §2.1 note ids, §2.2 sources, §2.3 deletion contract, §2.4 slots) + §0.1 rows O-2/O-3/O-12/O-13/O-14; ADR 0001. Orchestration: `docs/PHASE4-PLAN.md` (sub-plan B row, Gate B, §"Plan A handoff" carry-forwards).

## Global Constraints

- **The op log stays dark** — no journal file, no undo UI (PHASE4-PLAN rule 1, ADR 0003).
- **Thin renderer** (ADR 0006): no frontend task exists in this plan. Ids serialize transparently; the wire sees plain strings/numbers.
- **Frozen command/event names unchanged.** New fields on wire types are additive only (D-06: readers ignore unknown fields; `#[serde(default)]` on every new field).
- **Prepare-outside/commit-inside**; no blocking `EngineHandle::request` inside `transact`; transaction closures must not panic (no panic rollback until Plan F).
- **The snapshot-rebuild deferral stands** (PHASE4-PLAN §"Plan A handoff"): `engine::rebuild` keeps holding the session lock during graph build. Do NOT try to fix that here — Plan F owns it. This plan only changes *what* rebuild computes (derived slots, per-graph tables), not its locking.
- **`OP_FORMAT_VERSION` stays 1.** Typed ids serialize `#[serde(transparent)]` (wire unchanged); new op fields are additive with `#[serde(default)]` (old payloads still parse). Both are verified by the exact-JSON tests in `control/op.rs`. Rationale, recorded per ADR 0007: no journal has ever been written (the log is dark), so additive evolution without a bump is safe; a *breaking* wire change would still bump.
- **Foreground test runs only, always `timeout`-guarded:** `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml` (a Plan A implementer wedged three times on background waits). Baseline at plan start: **296 backend + 17 frontend**, all green.
- Corrections to docs are marked, never silent (ADR 0007).

**Symbol anchors were verified at `405af0b`** — grep for symbols; line numbers drift.

---

### Task 1: Plan A carry-forward batch

Four small, same-shape fixes ledgered in PHASE4-PLAN §"Plan A handoff". The positional clip restore is a **Gate B prerequisite**: `channel_properties.rs`'s header documents the strategy exclusion it forces (clip-carrying ops are not byte-stable under undo because restored clips are APPENDED, not reinserted).

**Files:**
- Modify: `src-tauri/src/control/ops.rs` (delete `apply_track_mix`, its two tests near lines 198/218, and the now-unused imports it held)
- Modify: `src-tauri/src/control/op.rs` (`clip_indices` field on `Op::TrackAdd`/`Op::TrackRemove`; extend wire tests)
- Modify: `src-tauri/src/control/session.rs` (duplicate-id guard; positional restore; clamp round-trip test; update doc comments that describe `apply_track_mix` as live code to past tense)
- Modify: `src-tauri/tests/channel_properties.rs` (delete the "STRATEGY EXCLUSION" header block — Task 8 lifts it for real)

**Interfaces:**
- Produces: `Op::TrackAdd { track, index, clips, clip_indices: Vec<usize> }` and `Op::TrackRemove { track, index, clips, clip_indices: Vec<usize> }`, both `#[serde(default)]` on `clip_indices`. Semantics: on `TrackAdd`, when `clip_indices.len() == clips.len()` each clip is inserted at its recorded pre-removal index (ascending order); otherwise clips are appended (today's behavior). On `TrackRemove` the field is advisory (store truth wins), populated by `apply_raw` in the computed inverse.
- Consumes: `apply_raw`, `fold_ops` from Plan A (`control/session.rs`).

- [ ] **Step 1: Delete dead `ops::apply_track_mix`**

Delete `pub fn apply_track_mix` (`control/ops.rs:47–95`) and its two tests (the functions containing the calls at lines ~198 and ~218). Keep `TrackMixChange` (still the wire type of `set_track_mix`). Fix any now-unused imports. Update the doc comments in `session.rs` (lines ~89–100, ~153–159, ~185) and `mod.rs` (~324, ~343, ~449) that cite `ops::apply_track_mix` as the live mirror — rephrase to "the retired `ops::apply_track_mix` (deleted in Plan B, behavior preserved here)". Do not change behavior.

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS with 2 fewer tests than baseline.

- [ ] **Step 2: Write the failing clamp round-trip test**

In `session.rs` tests:

```rust
#[test]
fn clamped_write_round_trips_through_inverse() {
    // 999 dB clamps to 24.0 on write; the recorded op must carry the
    // POST-CLAMP value (store truth), and its inverse must restore the
    // original exactly — the ledgered 999→24 case.
    let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
    let c = Session::transact(&m, user_meta("clamp"), |tx| {
        tx.apply(set_gain("t-1", 999.0))
    })
    .unwrap();
    assert_eq!(m.lock().store.tracks[0].gain_db, 24.0);
    match &c.ops[0] {
        Op::Set { to, .. } => assert_eq!(to, &serde_json::json!(24.0),
            "recorded op must carry the as-applied (clamped) value"),
        other => panic!("expected Set, got {other:?}"),
    }
    Session::transact(&m, user_meta("undo"), |tx| {
        for op in c.inverses.clone() { tx.apply(op)?; }
        Ok(())
    })
    .unwrap();
    assert_eq!(m.lock().store.tracks[0].gain_db, 1.0, "back to the original");
}
```

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml clamped_write_round_trips`
Expected: This may already PASS (fold reads inverse truth). If it passes, keep it as a pin and move on; if it fails, fix `fold_ops`/`apply_raw` so the folded op carries post-clamp truth.

- [ ] **Step 3: Write the failing duplicate-id test**

```rust
#[test]
fn track_add_with_duplicate_id_is_rejected() {
    // Rollback removes-by-id: a duplicate id would make the inverse
    // mis-target. Reject, don't clamp (ledgered Plan A carry-forward).
    let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
    let before = m.lock().store.tracks.clone();
    let r = Session::transact(&m, user_meta("dup"), |tx| {
        tx.apply(Op::TrackAdd {
            track: test_track("t-1"), index: 1,
            clips: vec![], clip_indices: vec![],
        })
    });
    assert!(r.is_err());
    assert_eq!(m.lock().store.tracks, before, "store untouched");
}
```

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml track_add_with_duplicate`
Expected: FAIL to compile first (no `clip_indices` field yet) — add the fields in the same step as minimal stubs (`#[serde(default)] clip_indices: Vec<usize>` on both variants, `vec![]` at every construction site — grep `TrackAdd {` / `TrackRemove {` across `src-tauri`), then FAIL on the assertion (duplicate currently inserts).

- [ ] **Step 4: Implement the duplicate-id guard**

In `apply_raw`'s `Op::TrackAdd` arm, before `insert_track`:

```rust
if session.store.tracks.iter().any(|t| t.id == track.id) {
    return Err(format!("duplicate track id: {}", track.id));
}
```

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml track_add_with_duplicate`
Expected: PASS

- [ ] **Step 5: Write the failing positional-restore test**

```rust
#[test]
fn remove_undo_restores_interleaved_clip_order_byte_identically() {
    // store.clips = [a1, b1, a2] (a-clips on t-A, b1 on t-B). Removing t-A
    // then undoing must yield EXACTLY [a1, b1, a2] — not [b1, a1, a2].
    let m = parking_lot::Mutex::new(session_with_one_track("t-B"));
    {
        let mut g = m.lock();
        g.store.tracks.push(test_track("t-A"));
        g.store.clips.push(test_clip("a1", "t-A"));
        g.store.clips.push(test_clip("b1", "t-B"));
        g.store.clips.push(test_clip("a2", "t-A"));
    }
    let row = m.lock().store.tracks.iter().find(|t| t.id == "t-A").cloned().unwrap();
    let before = m.lock().store.clips.clone();
    let c = Session::transact(&m, user_meta("remove"), |tx| {
        tx.apply(Op::TrackRemove { track: row, index: 0, clips: vec![], clip_indices: vec![] })
    })
    .unwrap();
    Session::transact(&m, user_meta("undo"), |tx| {
        for op in c.inverses.clone() { tx.apply(op)?; }
        Ok(())
    })
    .unwrap();
    assert_eq!(m.lock().store.clips, before, "same clips, same ORDER");
}
```

(`test_clip` helper: copy the one from `control/mod.rs` tests, line ~1116, into `session.rs`'s test module.)

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml remove_undo_restores_interleaved`
Expected: FAIL — order comes back `[b1, a1, a2]`.

- [ ] **Step 6: Implement positional restore**

In `apply_raw`, `Op::TrackRemove` arm — record pre-removal indices while collecting (retain visits in order):

```rust
let mut removed_clips = Vec::new();
let mut removed_indices = Vec::new();
let mut i = 0usize;
session.store.clips.retain(|c| {
    let keep = c.track_id != track.id;
    if !keep {
        removed_clips.push(c.clone());
        removed_indices.push(i);
    }
    i += 1;
    keep
});
```

Inverse becomes `Op::TrackAdd { track: removed, index: pos, clips: removed_clips, clip_indices: removed_indices }`.

In the `Op::TrackAdd` arm, replace the plain `extend`:

```rust
if clip_indices.len() == clips.len() && !clips.is_empty() {
    // Indices are ascending pre-removal positions; inserting back in
    // ascending order reconstructs the original interleaving exactly.
    for (c, &at) in clips.iter().zip(clip_indices.iter()) {
        let at = at.min(session.store.clips.len());
        session.store.clips.insert(at, c.clone());
    }
} else {
    session.store.clips.extend(clips.iter().cloned());
}
```

The `TrackAdd` inverse stays `Op::TrackRemove { track, index, clips, clip_indices }` with the same clips/indices it inserted.

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml remove_undo_restores_interleaved`
Expected: PASS

- [ ] **Step 7: Extend the exact-JSON wire tests**

In `op.rs`'s `op_serde_round_trips_and_carries_version`: construct `TrackAdd`/`TrackRemove` with `clip_indices: vec![0]`, assert `s.contains("\"clipIndices\":[0]")`, and add a **compat assertion**: a JSON payload *without* `clipIndices` still deserializes (serde default):

```rust
let legacy = r#"{"kind":"trackRemove","track":{"id":"t-2","name":"Audio Track","kind":"audio","gainDb":0.0,"pan":0.0,"muted":false,"soloed":false,"armed":false,"color":"#7c9cff"},"index":0,"clips":[]}"#;
let parsed: Op = serde_json::from_str(legacy).expect("legacy payload without clipIndices parses");
assert!(matches!(parsed, Op::TrackRemove { ref clip_indices, .. } if clip_indices.is_empty()));
```

Also delete the "STRATEGY EXCLUSION" doc block at the top of `tests/channel_properties.rs` (lines ~18–29) and replace with one line: `// Clip-carrying structural ops are exercised by Gate B (tests/identity_properties.rs).`

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS.

- [ ] **Step 8: Commit**

```bash
git add -A src-tauri
git commit -m "fix(control): Plan A carry-forwards — dead code, clamp pin, duplicate-id guard, positional clip restore"
```

---

### Task 2: Typed id families

`ids` module + migration of `TrackId` and `ClipId` through the document types and `ObjectRef`. Wire form is unchanged everywhere (`#[serde(transparent)]`), gated by the existing exact-JSON tests. `ContentId`/`SourceId`/`NoteId` are declared here and first *used* in Tasks 3/4.

**Files:**
- Create: `src-tauri/src/ids.rs`
- Modify: `src-tauri/src/lib.rs` (`pub mod ids;` — grep for the existing `pub mod` block)
- Modify: `src-tauri/src/control/op.rs` (`ObjectRef` variants take typed ids)
- Modify: `src-tauri/src/audio/types.rs` (`TrackState.id: TrackId`, `Clip.id: ClipId`, `Clip.track_id: TrackId`, `Store.slots: HashMap<TrackId, usize>`)
- Modify: `src-tauri/src/midi/types.rs` (`MidiClip.id: ClipId`, `MidiClip.track_id: TrackId`)
- Modify: compile fallout across the crate (mechanical; the compiler drives the list — expect `control/`, `audio/`, `midi/`, `plugins/`, `sidecars/` touches)
- Test: inline in `ids.rs` + existing wire tests unchanged (that's the point)

**Interfaces:**
- Produces:

```rust
// String-backed families: TrackId, ClipId, ContentId, SourceId.
// u32-backed: NoteId. All #[serde(transparent)].
pub struct TrackId(pub String);   // + ClipId, ContentId, SourceId identical
impl TrackId {
    pub fn mint() -> Self;        // uuid::Uuid::new_v4().to_string()
    pub fn as_str(&self) -> &str;
}
// Also: Display, From<&str>, From<String>, PartialEq<&str>, PartialEq<str>,
// Borrow<str> (so HashMap<TrackId, _>::get("plain-str") works), Default,
// Clone, Eq, Hash, PartialOrd, Ord.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct NoteId(pub u32);
```

- Consumes: nothing new.

- [ ] **Step 1: Write the failing wire-transparency test**

```rust
// src-tauri/src/ids.rs  (bottom)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_transparently() {
        // ADR 0001 + the plan's version rule: typed families MUST look like
        // plain strings/numbers on the wire, or OP_FORMAT_VERSION bumps.
        let t = TrackId::from("t-1");
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"t-1\"");
        let back: TrackId = serde_json::from_str("\"t-1\"").unwrap();
        assert_eq!(back, t);
        assert_eq!(serde_json::to_string(&NoteId(7)).unwrap(), "7");
        // Minted ids are 128-bit UUIDs, unique.
        assert_ne!(TrackId::mint(), TrackId::mint());
        // Borrow<str>: string lookups against typed-key maps.
        let mut m = std::collections::HashMap::new();
        m.insert(TrackId::from("a"), 1usize);
        assert_eq!(m.get("a"), Some(&1));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml ids_serialize_transparently`
Expected: FAIL to compile — module doesn't exist.

- [ ] **Step 3: Implement the `ids` module**

```rust
//! Typed ProjectId families (round-2 §2, ADR 0001). One newtype per family
//! so a ClipId cannot be passed where a TrackId is expected. All families
//! serialize TRANSPARENTLY — the wire and every JSON schema see plain
//! strings — which is what lets OP_FORMAT_VERSION stay 1 (gated by the
//! exact-JSON tests in control/op.rs). Families are additive: TakeId,
//! LaneId, PluginInstanceId, LineageId arrive with the rounds that use them.

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord,
                 serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Mint a fresh 128-bit UUID id. IDs are never reused (ADR 0001).
            pub fn mint() -> Self { Self(uuid::Uuid::new_v4().to_string()) }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<&str> for $name { fn from(s: &str) -> Self { Self(s.to_string()) } }
        impl From<String> for $name { fn from(s: String) -> Self { Self(s) } }
        impl PartialEq<&str> for $name { fn eq(&self, o: &&str) -> bool { self.0 == *o } }
        impl PartialEq<str> for $name { fn eq(&self, o: &str) -> bool { self.0 == o } }
        impl std::borrow::Borrow<str> for $name { fn borrow(&self) -> &str { &self.0 } }
    };
}

string_id!(/// Mixer/arranger track. Family of `TrackState.id`.
    TrackId);
string_id!(/// A placement on the timeline (audio clip today, midi clip too;
    /// round-2 §5 makes every placement a ClipId).
    ClipId);
string_id!(/// A content object (round-2 §5). Declared now, first populated by
    /// the C/D content/placement split; Task 3's note ids are scoped to the
    /// clip that owns them until then.
    ContentId);
string_id!(/// An audio source asset (`audio/<sourceId>.wav`, round-2 §2.2).
    SourceId);

/// Per-content sequential note id (round-2 §2.1). `0` is the wire sentinel
/// for "not yet assigned" — every note inside the document has id >= 1,
/// minted from its clip's persisted watermark. Values above `i32::MAX` must
/// never cross the CLAP boundary (ADR 0001).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct NoteId(pub u32);
```

Add `pub mod ids;` to `src-tauri/src/lib.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml ids_serialize_transparently`
Expected: PASS

- [ ] **Step 5: Migrate the `TrackId` family**

Change `TrackState.id` to `TrackId` (`audio/types.rs:104`), `Clip.track_id` to `TrackId`, `MidiClip.track_id` to `TrackId`, `Store.slots` to `HashMap<TrackId, usize>`, and `ObjectRef::Track(TrackId)` (`control/op.rs:27`). Let the compiler drive the rest of the crate. Migration rules:

- Comparisons `t.id == some_str` keep working via `PartialEq<&str>`; `some_str == t.id` does NOT — flip operand order instead of adding more impls.
- Functions taking `track_id: &str` at IPC/engine boundaries KEEP `&str`; convert at the document edge with `.into()` / compare via `PartialEq`.
- `HashMap<TrackId, usize>::get(track_id_str)` works via `Borrow<str>`.
- `format!("{}", t.id)` works via `Display`; `t.id.clone()` where a `String` is needed becomes `t.id.0.clone()` or `t.id.to_string()`.
- Struct literals in tests: `id: "t-1".into()` already compiles unchanged.
- Do NOT touch `TrackState.instrument_id` (a `plugin:`-scheme ref string, not a plain family).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS, including the untouched exact-JSON wire tests in `op.rs` and `wire_format_is_camel_case` in `types.rs` — that is the transparency proof.

- [ ] **Step 6: Migrate the `ClipId` family**

Same procedure: `Clip.id: ClipId` (`audio/types.rs:149`), `MidiClip.id: ClipId` (`midi/types.rs:53`), `ObjectRef::Clip(ClipId)`, `ObjectRef::MidiClip(ClipId)` (both are placements — round-2 §5 collapses them into one family; the enum variants stay separate because the wire tag is frozen).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add -A src-tauri
git commit -m "feat(ids): typed id families — TrackId/ClipId end-to-end, ContentId/SourceId/NoteId declared (ADR 0001)"
```

---

### Task 3: Note identity — `note_id` + persisted watermark in AMEV

Round-2 §2.1. The note address is `(content, NoteId)`; pre-split, the `MidiClip` **is** its content object, so the watermark lives on the clip and moves to the content object in Plan C/D unchanged. The AMEV chunk gains a self-describing appended-column mechanism; **both** readers (`midi/events.rs::decode_notes` AND `plugins/automation.rs::decode_points`) stop ignoring the mask.

**Files:**
- Modify: `src-tauri/src/midi/types.rs` (`MidiNote.note_id`, `MidiClip.next_note_id`, `mint_note_id`, `ensure_note_ids`)
- Modify: `src-tauri/src/midi/events.rs` (column framing, `COLUMNS_NOTE_ID`, encode/decode signatures)
- Modify: `src-tauri/src/plugins/automation.rs` (`decode_points` honors the mask / skips appended blocks; `encode_points` unchanged semantics)
- Modify: `src-tauri/src/midi/persist.rs` (write watermark, read watermark + ids)
- Modify: minting call sites: `src-tauri/src/midi/mod.rs` (`midi_set_notes`, `midi_add_clip`), `src-tauri/src/control/hum.rs`, `src-tauri/src/midi/midifile.rs`, `src-tauri/src/midi/amt.rs`, `src-tauri/src/control/mod.rs` (`demo_seed_clips`, `demo_seed_clips_v2`)
- Test: inline in `events.rs`, `types.rs`, `persist.rs`

**Interfaces:**
- Produces:

```rust
// midi/types.rs
pub struct MidiNote { /* existing fields */, #[serde(default)] pub note_id: crate::ids::NoteId }  // wire: "noteId", 0 = unassigned
pub struct MidiClip { /* existing */, #[serde(default = "first_note_id")] pub next_note_id: u32 } // wire: "nextNoteId", default 1
impl MidiClip {
    /// Hand out the next id and advance the persisted watermark. Ids start
    /// at 1 (0 is the "unassigned" wire sentinel) and are NEVER reused —
    /// recomputing max+1 on load is forbidden (ADR 0001).
    pub fn mint_note_id(&mut self) -> crate::ids::NoteId;
    /// Mint ids for any note still carrying 0; error on duplicate real ids.
    pub fn ensure_note_ids(&mut self) -> Result<(), String>;
}

// midi/events.rs
pub const COLUMNS_NOTE_ID: u16 = 0x0002;
pub fn encode_notes(ppq: u32, notes: &[MidiNote], next_note_id: u32) -> Vec<u8>;
pub struct DecodedNotes { pub ppq: u32, pub notes: Vec<MidiNote>, pub next_note_id: u32 }
pub fn decode_notes(bytes: &[u8]) -> Result<DecodedNotes, String>;
```

- Consumes: `crate::ids::NoteId` (Task 2).

**AMEV format extension (append to the header comment in `events.rs`, verbatim — this is versioned format semantics):**

```text
After `count` 16-byte core records, zero or more APPENDED COLUMN BLOCKS,
one per columnMask bit above 0x0001, in ascending bit order:
  [colBit u16] [byteLen u32] [payload: byteLen bytes]
Readers skip blocks whose colBit they do not know via byteLen — that is
what makes future columns non-breaking (the D-06 rule, binary edition).
COLUMNS_NOTE_ID (0x0002) payload:
  [next_note_id u32] [count x note_id u32]   (byteLen = 4 + 4*count)
AMEV_VERSION stays 1: v1 chunks without the column read tolerantly (note
ids minted 1..=count on load, watermark count+1); every write includes it.
```

- [ ] **Step 1: Add the fields + allocator with a failing test**

Add `note_id`/`next_note_id` per the interface (fix every `MidiNote {`/`MidiClip {` struct literal across the crate with `note_id: NoteId(0)` / `..` as needed — mechanical). Test in `midi/types.rs`:

```rust
#[test]
fn watermark_never_reuses_ids() {
    let mut clip = MidiClip {
        id: "c-1".into(), track_id: "t-1".into(), name: "c".into(),
        timeline_start_ticks: 0, length_ticks: 3840,
        notes: vec![], next_note_id: 1,
    };
    let a = clip.mint_note_id();
    let b = clip.mint_note_id();
    assert_eq!((a.0, b.0), (1, 2));
    // Deleting the highest note and minting again must NOT reuse 2 —
    // the watermark only ever advances (ADR 0001 never-reuse).
    assert_eq!(clip.mint_note_id().0, 3);
    assert_eq!(clip.next_note_id, 4);
}

#[test]
fn ensure_note_ids_mints_only_unassigned_and_rejects_duplicates() {
    let mut clip = MidiClip {
        id: "c-1".into(), track_id: "t-1".into(), name: "c".into(),
        timeline_start_ticks: 0, length_ticks: 3840,
        notes: vec![
            MidiNote { tick: 0, length_ticks: 1, key: 60, velocity: 100, channel: 0, note_id: NoteId(0) },
            MidiNote { tick: 1, length_ticks: 1, key: 61, velocity: 100, channel: 0, note_id: NoteId(5) },
        ],
        next_note_id: 6,
    };
    clip.ensure_note_ids().unwrap();
    assert_eq!(clip.notes[0].note_id.0, 6, "unassigned got minted");
    assert_eq!(clip.notes[1].note_id.0, 5, "existing id preserved");
    clip.notes.push(MidiNote { tick: 2, length_ticks: 1, key: 62, velocity: 100, channel: 0, note_id: NoteId(5) });
    assert!(clip.ensure_note_ids().is_err(), "duplicate real ids rejected");
}
```

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml watermark_never_reuses` — FAIL until implemented, then PASS. Then run the full suite and fix literal fallout: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`.

- [ ] **Step 2: Write the failing AMEV column tests**

In `events.rs`:

```rust
#[test]
fn amev_note_id_column_round_trips() {
    let notes = vec![
        MidiNote { tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0, note_id: NoteId(7) },
        MidiNote { tick: 480, length_ticks: 240, key: 64, velocity: 90, channel: 0, note_id: NoteId(9) },
    ];
    let bytes = encode_notes(960, &notes, 10);
    let d = decode_notes(&bytes).unwrap();
    assert_eq!(d.ppq, 960);
    assert_eq!(d.notes, notes, "ids survive the chunk");
    assert_eq!(d.next_note_id, 10, "watermark persisted in the header block");
}

#[test]
fn amev_v1_chunk_without_column_reads_tolerantly() {
    // A hand-built v1 chunk: mask = COLUMNS_CORE only, no appended blocks.
    // Loader mints 1..=count and watermark count+1 (upgrade-on-read).
    let legacy = {
        let notes = vec![
            MidiNote { tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0, note_id: NoteId(0) },
            MidiNote { tick: 480, length_ticks: 240, key: 64, velocity: 90, channel: 0, note_id: NoteId(0) },
        ];
        let mut b = encode_notes(960, &notes, 3);
        // Strip the appended block and clear the mask bit to fake a v1 file:
        b.truncate(16 + 2 * 16);
        b[6] = (COLUMNS_CORE & 0xFF) as u8;
        b[7] = (COLUMNS_CORE >> 8) as u8;
        b
    };
    let d = decode_notes(&legacy).unwrap();
    assert_eq!(d.notes[0].note_id, NoteId(1));
    assert_eq!(d.notes[1].note_id, NoteId(2));
    assert_eq!(d.next_note_id, 3);
}

#[test]
fn amev_unknown_column_blocks_are_skipped() {
    let notes = vec![MidiNote { tick: 0, length_ticks: 1, key: 60, velocity: 1, channel: 0, note_id: NoteId(1) }];
    let mut bytes = encode_notes(960, &notes, 2);
    // Append a fictional future column (bit 0x0004) and set its mask bit.
    bytes.extend_from_slice(&0x0004u16.to_le_bytes());
    bytes.extend_from_slice(&3u32.to_le_bytes());
    bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
    let mask = u16::from_le_bytes([bytes[6], bytes[7]]) | 0x0004;
    bytes[6] = (mask & 0xFF) as u8;
    bytes[7] = (mask >> 8) as u8;
    let d = decode_notes(&bytes).unwrap();
    assert_eq!(d.notes, notes, "unknown trailing column ignored, known ones kept");
}
```

Note: `bytes[6..8]` is the columnMask (offset: magic u32 + version u16). The existing `amev_rejects_garbage_and_newer_versions` test pokes `bytes[4]` (version) — keep it passing.

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml amev_` — FAIL to compile (signatures).

- [ ] **Step 3: Implement the column framing**

`encode_notes(ppq, notes, next_note_id)`: mask = `COLUMNS_CORE | COLUMNS_NOTE_ID`; after the records, write `[0x0002 u16][ (4 + 4*count) u32 ][next_note_id u32][note ids u32...]`.

`decode_notes`: parse mask honestly (delete the `let _columns =` line). After `count` records, loop while ≥ 6 bytes remain: read `colBit`/`byteLen`; `COLUMNS_NOTE_ID` → read watermark + ids (error if `byteLen != 4 + 4*count`); unknown → skip `byteLen`. If the note-id column is absent: assign `NoteId(i as u32 + 1)` per note in order, `next_note_id = count + 1`. Validate on read: watermark must be > every id present (`max(id) < next_note_id`), else `Err` — a corrupt watermark silently re-minting live ids is exactly the Blender-class corruption ADR 0001 exists to prevent.

Update existing callers/tests mechanically: `encode_notes(960, &notes)` → `encode_notes(960, &notes, n)`; destructure `DecodedNotes`. The 100k round-trip test asserts `bytes.len()`: update to `16 + 100_000*16 + 6 + 4 + 100_000*4`.

In `plugins/automation.rs::decode_points`: same block-walking (skip ALL appended blocks — points carry no ids yet), delete its `let _columns =` (line ~172). `encode_points` keeps mask `COLUMNS_CORE` (automation chunks carry no note-id column).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS.

- [ ] **Step 4: Persist round-trip + minting at every production site**

`midi/persist.rs`: write path (line ~87) becomes `encode_notes(midi.ppq, &clip.notes, clip.next_note_id)`; read path (`read_chunk`, ~line 206) returns ids + watermark to the caller, which sets both on the loaded clip (find the caller — it builds `MidiClip` rows from project.json v2 + chunks).

Minting rule, applied at every site that puts notes into `session.midi` (all hold the session lock, so allocation is race-free — full *transaction* routing for midi is Plan E's job and is recorded as such in Task 9's handoff):
- `midi/mod.rs::midi_set_notes` (~line 277): after validation, set each incoming note's `note_id` per this rule — an incoming id that is non-zero, unique in the payload, and `< clip.next_note_id` is KEPT; everything else is minted via `mint_note_id`. Then `ensure_note_ids()` as the belt-and-braces check. (The frontend sends 0 today, so ids churn per replace; the id-*preserving* gesture path is Plan E, ledgered.)
- `midi_add_clip`, `hum.rs` (both `MidiClip` construction sites), `midifile.rs` import, `amt.rs`, `demo_seed_clips`/`demo_seed_clips_v2`: construct notes with `note_id: NoteId(0)`, then call `ensure_note_ids()` on the finished clip before it enters the store.

Test in `persist.rs` (repo-style tempdir test):

```rust
#[test]
fn note_ids_and_watermark_survive_save_load() {
    // Save a clip whose ids are sparse (deleted-note gap), reload, and
    // verify ids and watermark are IDENTICAL — then mint one more and
    // verify the gap id is not resurrected.
    // build MidiStore with one clip: notes ids 1 and 3, next_note_id 4
    // save_into_project(dir, &midi) -> load into a fresh MidiStore
    // assert ids == [1, 3], next_note_id == 4
    // clip.mint_note_id() == 4, never 2
}
```

(Write the body against `persist.rs`'s existing save/load pair — grep `save_into_project` / the load function used by `notify_project_opened`.)

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add -A src-tauri
git commit -m "feat(midi): persisted note identity — note_id + AMEV watermark column, mask-honest readers (round-2 §2.1)"
```

---

### Task 4: Source-keyed assets and decode cache

Round-2 §2.2, justified per O-12: deduplicated decode memory, sane asset GC, and staleness invalidation — NOT the falsified "wrong audio" claim. New assets are named `audio/<sourceId>.wav`; the engine cache re-keys by source; legacy files stay valid (`source_path` remains truth — the naming convention only applies to newly created assets).

**Files:**
- Modify: `src-tauri/src/audio/types.rs` (`Clip.source_id: SourceId`, `#[serde(default)]`)
- Modify: `docs/ipc-schemas/clip.schema.json` (additive `sourceId` field, D-06)
- Modify: `src-tauri/src/audio/project.rs` (post-load fixup: mint one `SourceId` per unique `source_path` for clips with an empty `source_id`)
- Modify: `src-tauri/src/control/import.rs` (mint `SourceId`, name the copy `audio/<sourceId>.wav`, set it on the clip — lines ~218/257/276)
- Modify: `src-tauri/src/audio/recorder.rs` + `src-tauri/src/audio/engine.rs::start_recording` (`RecSpec` carries a `source_id`; the take's wav path uses it — line ~860)
- Modify: `src-tauri/src/audio/engine.rs` (`Control.cache` re-keyed; `ensure_loaded`, `rebuild`)
- Test: inline in `engine.rs`, `project.rs`, `import.rs`

**Interfaces:**
- Produces:

```rust
// engine.rs
struct CachedSource { source_path: String, data: Arc<RtClipData> }
// Control.cache: HashMap<SourceId, CachedSource>  ("source id -> decoded samples")
```

- Consumes: `crate::ids::SourceId` (Task 2).

- [ ] **Step 1: Add the field + load fixup with a failing test**

Add `#[serde(default)] pub source_id: SourceId` to `Clip` (wire `sourceId`; `Default` = empty string = "unassigned"). Fix struct-literal fallout (`source_id: SourceId::default()` in tests, or a real mint where the site creates a genuine new asset). In `project.rs`, after `load` parses the project (find `pub fn load`), add the fixup; test:

```rust
#[test]
fn legacy_clips_get_one_source_id_per_unique_path() {
    // Two clips sharing audio/x.wav and one on audio/y.wav, none with a
    // sourceId (legacy file): after load-fixup the two sharers have the
    // SAME minted id, the third a different one; a clip that already has
    // an id keeps it.
    let mut p = /* build Project with 3 such clips + 1 pre-assigned */;
    assign_source_ids(&mut p.clips);
    assert_eq!(p.clips[0].source_id, p.clips[1].source_id);
    assert_ne!(p.clips[0].source_id, p.clips[2].source_id);
    assert_eq!(p.clips[3].source_id.as_str(), "pre-existing");
}
```

Implement `pub(crate) fn assign_source_ids(clips: &mut [Clip])` (a `HashMap<String, SourceId>` over `source_path`), call it from `load` before returning, AND from `open_project` / anywhere else a legacy `Project` enters the store (grep `project::load(`). Also add `"sourceId": { "type": "string" }` to `docs/ipc-schemas/clip.schema.json` (additive, not required).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml legacy_clips_get_one` — FAIL then PASS; then full suite.

- [ ] **Step 2: New assets are named by source**

`import.rs`: `let source_id = SourceId::mint();` → `rel = format!("audio/{source_id}.wav")`, clip gets `source_id`. Update the import test asserting `audio/<clipId>.wav` (line ~671) to assert `audio/<sourceId>.wav` and `clip.source_id` set. `start_recording` (`engine.rs` ~860) + `RecSpec` (`recorder.rs`): same change — wav path and `rel_path` from a minted `SourceId`, `RecSpec` gains `source_id: SourceId`, and the `Clip` the recorder finalizes carries it (grep `RecSpec` and the `Clip {` literal in `recorder.rs`). Waveform pyramid cache dirs stay keyed by clip id (visual cache; dedup opportunity ledgered in Task 9, not taken here).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS.

- [ ] **Step 3: Write the failing cache tests**

`ensure_loaded`/`rebuild` are private to `Control`; test the cache policy through a extracted pure helper so it is unit-testable (repo pattern: policy in pure functions). In `engine.rs`:

```rust
/// Which sources need (re)decoding: absent from the cache, or cached from
/// a different path than the clip now names (staleness — round-2 §2.2).
fn stale_sources(
    clips: &[Clip],
    cache: &HashMap<SourceId, CachedSource>,
) -> Vec<(SourceId, String)>; // (source, source_path) — deduped, in first-seen order
```

```rust
#[test]
fn cache_is_shared_per_source_and_invalidates_on_path_change() {
    let mut cache: HashMap<SourceId, CachedSource> = HashMap::new();
    let sid = SourceId::from("s-1");
    let c1 = { let mut c = test_clip("c-1", "t-1"); c.source_id = sid.clone(); c };
    let c2 = { let mut c = test_clip("c-2", "t-1"); c.source_id = sid.clone(); c };
    // Two clips, one source: exactly ONE decode wanted.
    assert_eq!(stale_sources(&[c1.clone(), c2.clone()], &cache).len(), 1);
    cache.insert(sid.clone(), CachedSource {
        source_path: c1.source_path.clone(),
        data: Arc::new(RtClipData { channels: 2, data: vec![0.0; 4] }),
    });
    // Cached and unchanged: nothing to do.
    assert!(stale_sources(&[c1.clone(), c2.clone()], &cache).is_empty());
    // source_path changes under the SAME source id: re-decode (staleness).
    let mut c1b = c1.clone();
    c1b.source_path = "audio/replaced.wav".into();
    assert_eq!(stale_sources(&[c1b], &cache).len(), 1);
}
```

(`test_clip` helper local to the test module, same shape as `control/mod.rs`'s.)

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml cache_is_shared_per_source` — FAIL.

- [ ] **Step 4: Re-key the cache**

`Control.cache: HashMap<SourceId, CachedSource>`; comment becomes "source id -> decoded samples at `cache_rate`". `ensure_loaded` uses `stale_sources` for its todo list (decode once per source; pyramid build still per *clip* dir — iterate clips of that source for `waveform_cache_dir`), retains by live source ids. `rebuild`'s lookup (line ~567) becomes `self.cache.get(&c.source_id).map(|e| e.data.clone())`. GC comment: retention by source means an asset shared by two clips survives one clip's deletion (the "sane asset GC" half of the O-12 justification).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add -A src-tauri
git commit -m "feat(audio): source-keyed assets + decode cache with staleness invalidation (round-2 §2.2)"
```

---

### Task 5: Slots become derived per-graph state; the param table versions with the graph

Round-2 §2.4 / O-13. The defect is slot **aliasing in the async rebuild window**: `remove_track` frees the slot, an `alloc_slot` in between reuses it while the old graph still plays the deleted track under it. The fix is structural: `Store` stops owning slots entirely; every rebuild derives slots from display order and builds **its own** `ParamTable`; a retired graph keeps reading its own table, so a "freed" slot cannot alias — there is nothing to free. This is the RT-heavy task: its review goes to the most capable model.

**Files:**
- Modify: `src-tauri/src/audio/types.rs` (DELETE `Store.slots`, `slot_used`, `alloc_slot`, `free_slot`, `track_slots` + their tests; ADD `derive_slots`)
- Modify: `src-tauri/src/audio/rt.rs` (`RtGraph { generation, params }`, `GraphTables`)
- Modify: `src-tauri/src/audio/engine.rs` (rebuild assigns slots + builds/publishes tables; `OutputCb` reads `graph.params`; recording slot resolution; meter pump interim)
- Modify: `src-tauri/src/control/session.rs` (`EngineEffect.param_writes` keyed by `TrackId`; `apply_raw` no longer consults slots)
- Modify: `src-tauri/src/control/mod.rs` (`ControlPlane` holds `SharedGraphTables`; `commit` resolves track→slot; `remove_track` loses its `free_slot` line; `add_track` path loses alloc)
- Modify: `src-tauri/src/control/ops.rs` (`new_track_row` loses `params` arg + alloc + the `MAX_TRACKS` limit error)
- Modify: `src-tauri/src/audio/mod.rs` (`open_project` drops the free/alloc/param-seeding block — lines ~586–612; `AudioState` field swap)
- Modify: `src-tauri/src/midi/playback.rs` (`append_from` takes the derived slot map instead of reading `store.slots`)
- Modify: `src-tauri/src/audio/offline.rs`, `src-tauri/src/control/export.rs` (derive slots instead of scratch-store alloc)
- Modify: `src-tauri/src/plugins/lv2_host.rs`, `src-tauri/src/plugins/clap_host.rs`, test fallout wherever `alloc_slot` was called
- Test: inline in `types.rs`, `rt.rs`, `engine.rs`, `mod.rs`

**Interfaces:**
- Produces:

```rust
// audio/types.rs — pure, display-order slot derivation (round-2 §2.4).
pub fn derive_slots(tracks: &[TrackState]) -> std::collections::HashMap<crate::ids::TrackId, usize>;
// slot = index in `tracks` (display order). Every track gets a slot.

// audio/rt.rs
pub struct RtGraph {
    pub tracks: Vec<RtTrack>,
    pub scratch: Vec<f32>,
    /// Monotonic graph generation; meter blocks echo it (Task 6).
    pub generation: u64,
    /// THIS graph's parameters — round-2 §2.4: the param table versions
    /// with the graph snapshot. A retired graph keeps reading its own
    /// table; knob traffic targets the newest via GraphTables.
    pub params: Arc<ParamTable>,
}
impl RtGraph { pub fn new(tracks: Vec<RtTrack>, generation: u64, params: Arc<ParamTable>) -> Self; }

/// Control-side view of the CURRENT graph's tables, published by the engine
/// control thread on every rebuild (also headless — tables exist without an
/// output device). Readers: ControlPlane::commit (knob writes), recording
/// slot resolution, the meter fold (Task 6).
pub struct GraphTables {
    pub generation: u64,
    pub params: Arc<ParamTable>,
    pub slots: std::collections::HashMap<crate::ids::TrackId, usize>,
}
pub type SharedGraphTables = Arc<parking_lot::Mutex<GraphTables>>;

// control/session.rs
pub struct EngineEffect {
    pub rebuild: bool,
    pub param_writes: Vec<(crate::ids::TrackId, PropPath, f32)>, // was (usize, ...)
    pub any_solo: Option<bool>,
}
```

- `engine::start` gains a `tables: SharedGraphTables` parameter; `ControlPlane::new` likewise (grep both call sites in `lib.rs`/`audio/mod.rs` setup).
- `ops::new_track_row(store: &mut Store, name, kind) -> Result<(TrackState, usize), String>` (no `params`, no limit error — the limit dies with the fixed arrays; final removal in Task 7).
- `midi/playback.rs::append_from(midi, store, slots: &HashMap<TrackId, usize>, rate, bank, nodes, out)`.

**Semantics that must hold (write these into code comments where they land):**
1. `rebuild` (with the session lock held, per the standing deferral) derives `slots = derive_slots(&store.tracks)`, builds a fresh `ParamTable` populated from the rows (`gain/pan/mute/solo` + `any_solo`), bumps `self.generation`, publishes `GraphTables` — **always, even headless** (no output device ⇒ no graph queued, but tables still publish so knob writes and recording keep working).
2. `ControlPlane::commit` executes `param_writes` by resolving `TrackId → slot` through the CURRENT `GraphTables`; a track without a slot yet (committed but not yet rebuilt — only possible for a `Set` racing a structural rebuild) is skipped: the in-flight rebuild populates its table from the document, which already carries the change. Same for `any_solo`.
3. `remove_track` loses `free_slot` (nothing to free); `new_track_row` loses alloc + param reset (build-time population replaces it); `open_project` loses its whole slot/param-seeding block (lines ~586–612) — adoption + `Rebuild` is enough.
4. A retired graph's audio is rendered against ITS `params` (the alias window is dead by construction). `OutputCb` drops its `params` field; `mixer::render(g, ...)` reads `&g.params` (change `render`'s signature or pass `&g.params` explicitly — keep `mixer::render`'s `&ParamTable` parameter and pass the graph's).
5. Live-node state already moves across rebuilds via `LiveNodeRegistry` keyed by track id (the pattern round-2 §2.4 names); parameter smoothing does not exist yet — when it lands (engine round), it keys by `TrackId` like the registry. Record this as a comment on `GraphTables`.

- [ ] **Step 1: Write the failing `derive_slots` + `GraphTables` tests**

```rust
// types.rs tests
#[test]
fn slots_are_display_order_and_never_reused_across_generations() {
    let tracks = vec![test_track("a"), test_track("b"), test_track("c")];
    let s = derive_slots(&tracks);
    assert_eq!((s["a"], s["b"], s["c"]), (0, 1, 2));
    // Remove "a": the NEXT derivation renumbers — that is fine, because
    // the numbering is scoped to one graph (each graph has its own table);
    // cross-generation aliasing is impossible by construction, which the
    // engine-level test (Task 8) proves end to end.
    let s2 = derive_slots(&tracks[1..]);
    assert_eq!((s2["b"], s2["c"]), (0, 1));
}
```

(`test_track` helper exists in several test modules — add one locally.)

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml slots_are_display_order` — FAIL (function absent).

- [ ] **Step 2: Implement `derive_slots`, `GraphTables`, `RtGraph` fields**

Pure additions first (nothing consumes them yet): `derive_slots`, the `GraphTables` struct + `SharedGraphTables` alias, `RtGraph::new(tracks, generation, params)` (update existing `RtGraph::new` callers with `0`/`ParamTable::default()` placeholders so the crate compiles — they are rewritten in Step 4).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml` — PASS.

- [ ] **Step 3: Re-key `EngineEffect.param_writes` by `TrackId`**

In `session.rs`, `apply_raw`'s `Op::Set` arm: delete the `session.store.slots.get(id)` lookup; ALWAYS push the four writes keyed by `TrackId` (the resolve-to-slot step moves to `commit`). Update the `EngineEffect` doc comment: the session describes *document-level* effects; slot resolution is execution-time state owned by the current graph. Update `commit` (`control/mod.rs` ~443) to resolve through `self.tables` (new `ControlPlane` field, threaded from setup):

```rust
{
    let tables = self.tables.lock();
    for (tid, path, value) in &committed.effect.param_writes {
        let Some(&slot) = tables.slots.get(tid) else { continue };
        match path {
            op::PropPath::Gain => tables.params.set_gain_linear(slot, *value),
            op::PropPath::Pan => tables.params.set_pan(slot, *value),
            op::PropPath::Muted => tables.params.set_flag(slot, FLAG_MUTE, *value != 0.0),
            op::PropPath::Soloed => tables.params.set_flag(slot, FLAG_SOLO, *value != 0.0),
            op::PropPath::Armed => {}
        }
    }
    if let Some(any_solo) = committed.effect.any_solo {
        tables.params.any_solo.store(any_solo, Relaxed);
    }
}
```

Existing `session.rs` unit tests that assert `param_writes` contents: update expectations from slot indices to track ids.

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml` — compile-error-driven; get the crate green with `ControlPlane`/`AudioState`/`engine::start` carrying `SharedGraphTables` (initialized to `GraphTables { generation: 0, params: Arc::new(ParamTable::default()), slots: HashMap::new() }`).

- [ ] **Step 4: Rebuild derives slots and publishes tables**

`engine.rs::Control`: add `tables: SharedGraphTables`, `generation: u64`. Rewrite `rebuild`:

```rust
fn rebuild(&mut self) {
    self.ensure_loaded();
    self.generation += 1;
    let (graph, tables) = {
        let session = self.session.lock();
        let store = &session.store;
        let slots = super::types::derive_slots(&store.tracks);
        let params = Arc::new(ParamTable::default());
        for (i, t) in store.tracks.iter().enumerate() {
            params.set_gain_linear(i, mixer::db_to_linear(t.gain_db));
            params.set_pan(i, t.pan as f32);
            params.set_flag(i, super::rt::FLAG_MUTE, t.muted);
            params.set_flag(i, super::rt::FLAG_SOLO, t.soloed);
        }
        params.any_solo.store(store.any_solo(), Relaxed);
        let mut tracks = Vec::with_capacity(store.tracks.len());
        for t in &store.tracks {
            let slot = slots[&t.id];
            /* clip assembly exactly as today, but slot from `slots` */
        }
        /* append_from(&session.midi, store, &slots, self.cache_rate, bank, &mut self.live_nodes, &mut tracks) */
        /* song_end exactly as today */
        (
            Box::new(RtGraph::new(tracks, self.generation, params.clone())),
            GraphTables { generation: self.generation, params, slots },
        )
    };
    *self.tables.lock() = tables; // published even headless
    let Some(out) = self.output.as_mut() else { return };
    while let Ok(gp) = out.retire_rx.pop() { drop(gp); }
    if let Err(rtrb::PushError::Full(_gp)) = out.graph_tx.push(GraphPtr::new(graph)) {
        log::warn!("audio: graph queue full, rebuild dropped");
    }
}
```

(The early `return` for headless moves BELOW the table publish — that ordering is the point.) `OutputCb`: delete the `params` field; `mixer::render(g, &g.params, ...)` — wait, `render` receives the graph already; pass `&g.params` where `&self.params` went. `start_recording`'s slot resolution (line ~848) reads `self.tables.lock().slots`. `pump_meter_frames`' `track_slots()` call becomes: lock session for display order, lock tables for slots — `store.tracks.iter().filter_map(|t| tables.slots.get(&t.id).map(|&s| (s, t.id.0.clone())))` (interim until Task 6 re-does the fold). `append_from` signature per the interface; `open_project` block deleted; `ops::new_track_row` per the interface; `remove_track`'s `free_slot` line deleted; offline/export derive slots (`derive_slots(&store.tracks)` on the snapshot — delete the scratch alloc loops); delete `Store.slots`/`slot_used`/`alloc_slot`/`free_slot`/`track_slots` and their two unit tests; fix ALL remaining call sites (tests included — test graphs now construct `RtGraph::new(tracks, gen, params)` explicitly, which most already nearly do).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS. Behavioral pins that must still hold: `engine_pumps_meter_frames_at_60hz` (its manual `alloc_slot` seeding is replaced by just pushing the track + sending `ControlMsg::Rebuild`), `remove_track_recomputes_any_solo` (now asserts through `tables.params.any_solo`), offline/export byte-stability tests.

- [ ] **Step 5: Write the aliasing regression test (unit level)**

In `engine.rs` tests (the deterministic `OutputCb`-driving pattern, line ~1043):

```rust
#[test]
fn retired_graph_keeps_its_own_params_no_slot_aliasing() {
    // Round-2 O-13: graph gen1 has track X on slot 0 (gain 0.5). A
    // delete+add rebuild produces gen2 with track Y on slot 0 (gain 0.9).
    // While gen1 is STILL the adopted graph (rebuild queued, not yet
    // adopted), Y's params must not bleed into X's rendering — the two
    // generations read two different tables.
    let p1 = Arc::new(ParamTable::default());
    p1.set_gain_linear(0, 0.5);
    let p2 = Arc::new(ParamTable::default());
    p2.set_gain_linear(0, 0.9);
    let g1 = Box::new(RtGraph::new(vec![/* one clip track, slot 0 */], 1, p1.clone()));
    let g2 = Box::new(RtGraph::new(vec![/* one clip track, slot 0 */], 2, p2.clone()));
    // Drive OutputCb: adopt g1, render, verify output level matches 0.5;
    // queue g2 but render once more BEFORE popping — still 0.5 (gen1's
    // table); next render adopts g2 — now 0.9. Use a DC-valued clip
    // (all samples 1.0) so the gain is directly readable off the output.
    /* assemble via the output_cb() test helper + a test clip as in
       graph_ptr_frees_on_queue_teardown; assert peak of `out` per phase */
}
```

Note on determinism: `OutputCb::render` adopts a queued graph at the TOP of `render` — so "render once more before adoption" means: push `g2` into `graph_rx` only AFTER the second render call. The test controls the queue, so this is fully deterministic.

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml retired_graph_keeps` — write it to FAIL first by asserting the OLD behavior is gone (if written against the new code it should pass immediately; its value is as a permanent regression pin — acceptable per TDD when the implementation landed in Step 4; note this in the test comment).

- [ ] **Step 6: Commit**

```bash
git add -A src-tauri
git commit -m "feat(audio): slots are derived per-graph state; param tables version with the graph (round-2 §2.4, O-13)"
```

---

### Task 6: Meter blocks carry the graph generation

Round-2 §2.4's second obligation: the control thread folds meter blocks **under the slot map that produced them**. Today a block from a pre-rebuild graph is folded under the post-rebuild map — with per-rebuild renumbering (Task 5) that would show track B's level on track A's meter for a frame. Blocks get a `generation` tag; the fold keeps a small window of recent generation→slot-map entries.

**Files:**
- Modify: `src-tauri/src/audio/meters.rs` (`RawMeterBlock.generation`; `MeterAccum` folds by `(generation, slot) → TrackId`)
- Modify: `src-tauri/src/audio/mixer.rs` (`render` stamps the block with `g.generation`)
- Modify: `src-tauri/src/audio/engine.rs` (`InputCb` stamps blocks with the generation its slots were resolved from; `Control` keeps the generation window and passes it to the fold; `pump_meter_frames`)
- Test: inline in `meters.rs`

**Interfaces:**
- Produces:

```rust
// meters.rs
pub struct RawMeterBlock { pub generation: u64, /* existing fields */ }
impl RawMeterBlock { pub fn new(generation: u64, position: u64, frames: u32) -> Self; }

/// generation -> (slot -> track id), kept for the adoption window.
/// Entries are pruned to the last KEPT_GENERATIONS (4) on insert — a block
/// older than that is dropped by the fold (stale beyond the window).
pub struct GenerationMaps { /* BTreeMap<u64, HashMap<usize, TrackId>> */ }
impl GenerationMaps {
    pub const KEPT_GENERATIONS: usize = 4;
    pub fn publish(&mut self, generation: u64, slots: &HashMap<TrackId, usize>);
    pub fn resolve(&self, generation: u64, slot: usize) -> Option<&TrackId>;
}

// MeterAccum: folds into per-TrackId lanes.
impl MeterAccum {
    pub fn fold(&mut self, b: &RawMeterBlock, maps: &GenerationMaps);
    /// `order`: display-order track ids for the frame.
    pub fn take_frame(&mut self, seq: u64, order: &[TrackId], idle_position: u64) -> MeterFrame;
}
```

- Consumes: `GraphTables` publish points (Task 5) — `Control::rebuild` also calls `self.gen_maps.publish(generation, &slots)`.

- [ ] **Step 1: Write the failing fold-by-generation test**

```rust
#[test]
fn blocks_fold_under_the_slot_map_of_their_own_generation() {
    // gen 1: slot 0 = "a", slot 1 = "b". gen 2 (after removing "a"):
    // slot 0 = "b". A gen-1 block reporting slot 0 and a gen-2 block
    // reporting slot 0 must land on DIFFERENT tracks.
    let mut maps = GenerationMaps::default();
    maps.publish(1, &[("a".into(), 0), ("b".into(), 1)].into_iter().collect());
    maps.publish(2, &[("b".into(), 0)].into_iter().collect());
    let mut acc = MeterAccum::default();
    let mut b1 = RawMeterBlock::new(1, 0, 100);
    b1.set_slot(0, 0.5, 0.5, 1.0, 1.0);       // "a" under gen 1
    let mut b2 = RawMeterBlock::new(2, 100, 100);
    b2.set_slot(0, 0.9, 0.9, 2.0, 2.0);       // "b" under gen 2
    acc.fold(&b1, &maps);
    acc.fold(&b2, &maps);
    let f = acc.take_frame(0, &["a".into(), "b".into()], 0);
    assert!((f.tracks[0].peak_l - 0.5).abs() < 1e-6, "a keeps its gen-1 level");
    assert!((f.tracks[1].peak_l - 0.9).abs() < 1e-6, "b gets the gen-2 level");
}

#[test]
fn blocks_from_unknown_generations_are_dropped() {
    let mut maps = GenerationMaps::default();
    for g in 1..=6u64 { maps.publish(g, &[("t".into(), 0)].into_iter().collect()); }
    let mut acc = MeterAccum::default();
    let mut stale = RawMeterBlock::new(1, 0, 100); // pruned (only 3..=6 kept)
    stale.set_slot(0, 1.0, 1.0, 1.0, 1.0);
    acc.fold(&stale, &maps);
    assert!(acc.is_empty(), "stale-generation blocks contribute nothing");
}
```

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml blocks_fold_under` — FAIL to compile.

- [ ] **Step 2: Implement**

`MeterAccum`'s fixed `[_; MAX_TRACKS]` lanes become `HashMap<TrackId, Lanes>` (`Lanes { peak: [f32; 2], sumsq: [f32; 2] }`) — this is control-side, allocation is fine. Master lanes unchanged. `fold` resolves `(b.generation, slot)` per set mask bit; unresolvable → skip. `is_empty` counts frames only from accepted blocks. `take_frame` maps `order` through the HashMap (silence for absent). `mixer::render` gains the generation from the graph it renders (it already receives `&RtGraph` — stamp `blk.generation = g.generation`). `InputCb` gains a `generation: u64` field set at `start_recording` from `tables.generation` (recording slots were resolved against that same table — the stamp says so). `Control` owns `gen_maps: GenerationMaps`; `rebuild` publishes; `pump_meter_frames` passes `&self.gen_maps` to folds... correction: `drain_meters` does the folding — thread it there. `pump_meter_frames` builds `order` from `session.store.tracks` ids.

`RawMeterBlock` stays POD/`Copy` (one added `u64` — still memcpy-able through rtrb).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS (existing meter tests updated to the new constructors/signatures; assertions unchanged in spirit).

- [ ] **Step 3: Commit**

```bash
git add -A src-tauri
git commit -m "feat(audio): meter blocks carry graph generation; fold under the matching slot map (round-2 §2.4)"
```

---

### Task 7: Remove `MAX_TRACKS`

Round-2 §2.4's closing rule: redesigning slot assignment while silently keeping the 64-cap would be a miss against the product's own scale ambition (see memory: FL-Studio-scale). Per-graph sizing replaces the fixed arrays; the meter path replaces the single `u64`-masked block with **chunked** blocks (a POD block covers 64 slots from a `base_slot`; a wide graph emits several per buffer) — D-05's direction, still fixed-size POD through rtrb.

**Files:**
- Modify: `src-tauri/src/audio/rt.rs` (`ParamTable` → per-graph `Vec` sizing; delete the const's use)
- Modify: `src-tauri/src/audio/meters.rs` (`METER_CHUNK_SLOTS: usize = 64`, `base_slot: u32` on the block)
- Modify: `src-tauri/src/audio/mixer.rs` (emit one block per 64-slot chunk; bound check by graph size)
- Modify: `src-tauri/src/audio/engine.rs` (build sized tables; `InputCb` chunk emission)
- Modify: `src-tauri/src/audio/project.rs` (delete the `MAX_TRACKS` validation + fix its test to assert the cap is GONE)
- Modify: `src-tauri/src/audio/types.rs` (delete `pub const MAX_TRACKS`)
- Modify: `src-tauri/src/control/ops.rs` (delete the remaining import if any)
- Test: inline in `rt.rs`, `meters.rs`, `project.rs`

**Interfaces:**
- Produces:

```rust
// rt.rs
impl ParamTable {
    /// A table sized for `n` slots (per-graph — round-2 §2.4). All setters
    /// bounds-check against the actual size (out-of-range writes are
    /// dropped, matching today's MAX_TRACKS guards).
    pub fn with_slots(n: usize) -> Self;
    pub fn len(&self) -> usize;
}
// fields become gain: Vec<AtomicU32>, pan: Vec<AtomicU32>, flags: Vec<AtomicU32>.
// ParamTable::default() == with_slots(0) is WRONG for tests — keep Default
// as with_slots(64) so existing tests that poke arbitrary small slots
// stay valid, and let rebuild call with_slots(store.tracks.len()).

// meters.rs
pub const METER_CHUNK_SLOTS: usize = 64;
pub struct RawMeterBlock {
    pub generation: u64,
    pub position: u64,
    pub frames: u32,
    /// First slot this chunk covers; lane i = slot base_slot + i.
    pub base_slot: u32,
    pub mask: u64,                          // presence within THIS chunk
    pub peak: [[f32; 2]; METER_CHUNK_SLOTS],
    pub sumsq: [[f32; 2]; METER_CHUNK_SLOTS],
    pub master_peak: [f32; 2],
    pub master_sumsq: [f32; 2],
}
```

- `mixer::render` signature: instead of returning ONE `RawMeterBlock`, it pushes 1..=⌈slots/64⌉ chunks into the producer it is given: `render(..., meter_tx: &mut rtrb::Producer<RawMeterBlock>)`. Master lanes ride on the first chunk only (`base_slot == 0`); the fold takes master from chunks where `base_slot == 0`.
- **RT discipline:** the chunk array lives on the render stack (fixed 64-lane POD, same as today's block); emitting N chunks is N pushes, no allocation. A full ring drops the remaining chunks and counts one xrun (today's policy, unchanged).

- [ ] **Step 1: Write the failing over-64 tests**

```rust
// rt.rs
#[test]
fn param_table_sizes_beyond_sixty_four() {
    let p = ParamTable::with_slots(100);
    p.set_gain_linear(99, 0.5);
    assert_eq!(f32::from_bits(p.gain[99].load(std::sync::atomic::Ordering::Relaxed)), 0.5);
    p.set_gain_linear(100, 0.5); // out of range: dropped, no panic
    assert_eq!(p.len(), 100);
}

// meters.rs
#[test]
fn chunked_blocks_cover_slots_past_sixty_four() {
    let mut maps = GenerationMaps::default();
    maps.publish(1, &(0..100).map(|i| (TrackId::from(format!("t{i}").as_str()), i)).collect());
    let mut acc = MeterAccum::default();
    let mut hi = RawMeterBlock::new(1, 0, 100);
    hi.base_slot = 64;
    hi.set_slot_local(99 - 64, 0.7, 0.7, 1.0, 1.0); // slot 99, lane 35
    acc.fold(&hi, &maps);
    let f = acc.take_frame(0, &(0..100).map(|i| TrackId::from(format!("t{i}").as_str())).collect::<Vec<_>>(), 0);
    assert!((f.tracks[99].peak_l - 0.7).abs() < 1e-6);
}

// project.rs — REPLACE the cap test (line ~286):
#[test]
fn projects_larger_than_sixty_four_tracks_validate() {
    // MAX_TRACKS removed in Plan B (round-2 §2.4): per-graph sizing.
    project.tracks = (0..200).map(track_n).collect();
    assert!(validate(&project).is_ok());
}
```

(`set_slot_local(lane, ...)` replaces `set_slot(slot, ...)` — lane-relative within the chunk; the fold computes `slot = base_slot + lane`.)

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml param_table_sizes` — FAIL to compile.

- [ ] **Step 2: Implement**

Per the interfaces. `mixer::render`: track loop unchanged; the meter write targets chunk `slot / 64`, lane `slot % 64` — iterate a fixed-size stack array of up to `(n + 63)/64` chunks... a `Vec` at render time is forbidden; instead render accumulates into the graph: give `RtGraph` a preallocated `meter_scratch: Vec<RawMeterBlock>` (sized at BUILD time on the control thread, `⌈tracks/64⌉` chunks) that `render` fills and pushes per chunk — mutation of the graph's own scratch on the RT thread is the existing pattern (`RtGraph.scratch`). Delete `MAX_TRACKS` from `types.rs`, delete the validation block in `project.rs::validate` (lines ~126–131), fix remaining references (`mixer.rs:228` bound → `tr.slot >= g.params.len()` guard, `ops.rs` import, `engine.rs` duplicates). Duplicate-track-id validation in `project.rs` stays (only the cap goes).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all PASS.

- [ ] **Step 3: Commit**

```bash
git add -A src-tauri
git commit -m "feat(audio): remove MAX_TRACKS — per-graph table sizing + chunked meter blocks (round-2 §2.4)"
```

---

### Task 8: Gate B property & integration suite

The gate, verbatim from PHASE4-PLAN: *test 2 (delete-then-undo preserves identity and inbound references) passes; slot aliasing test (delete→add→old graph still playing) passes; meter blocks carry graph generation.* The property suite extends Gate A's harness style; the aliasing test is headless and deterministic via the retire/adopt seam.

**Files:**
- Create: `src-tauri/tests/identity_properties.rs`
- Modify: `src-tauri/tests/channel_properties.rs` (extend `arb_op_batches` to clip-carrying structural ops — the Task 1 restore makes them byte-stable)
- Test: this task IS tests.

**Interfaces:**
- Consumes: everything above. Harness style: copy `channel_properties.rs`'s local-helper pattern (own `test_track`/`user_meta`/`snapshot`, independent of crate-private test modules).

- [ ] **Step 1: Property — delete-then-undo preserves identity and inbound references**

```rust
//! Gate B (round-2 §7 test 2 + §2.4 obligations): identity survives
//! deletion; inbound references survive undo; generations do not alias.

use proptest::prelude::*;
use aura_lib::audio::types::{Clip, Store, TrackState};
use aura_lib::control::op::{Op, TxMeta};
use aura_lib::control::Session;
use aura_lib::ids::{ClipId, TrackId};
use aura_lib::midi::MidiStore;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Round-2 §2.3: "delete then undo preserves identity and every
    /// inbound reference." Randomized: N tracks (2..6), clips scattered
    /// and INTERLEAVED across them (0..4 each), remove a random subset of
    /// tracks one transaction at a time, undo in reverse — the store must
    /// be byte-identical: same ids (never re-minted), same clip->track
    /// references, same VEC ORDER (Task 1's positional restore).
    #[test]
    fn delete_then_undo_preserves_identity_and_references(
        seed in any::<u64>(), n_tracks in 2usize..6, removals in 1usize..4,
    ) {
        // build session; snapshot; for each removal: transact TrackRemove,
        // collect Committed; then apply all inverses in reverse via new
        // transactions; assert snapshot equality (serde_json of
        // (tracks, clips) — the byte-identity oracle used by Gate A).
    }
}
```

Write the body fully (deterministic pseudo-random selection from `seed` — no `rand` dependency; index arithmetic on `seed` suffices). Also assert the *identity* half explicitly: collect all `TrackId`/`ClipId` values before and after; equality of the serialized store already covers it, but add `prop_assert_eq!` on the id sets with a message naming the never-reuse contract.

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml --test identity_properties` — PASS (the machinery landed in Tasks 1–2; this is the gate pin).

- [ ] **Step 2: The slot-aliasing race test (headless, deterministic)**

In the same file — this is the Gate B wording "delete→add→old graph still playing", end to end through the engine's real seams:

```rust
/// Round-2 O-13: the alias window. Sequence: graph gen-N plays track X.
/// X is removed and Y added (both through the channel); the rebuild for
/// gen-N+1 is queued but the callback has NOT adopted it. A param write
/// to Y (via commit) must land in gen-N+1's table only; gen-N's table —
/// which the still-playing old graph reads — must be untouched. With
/// Store-owned slots this was the aliasing bug; with per-graph tables it
/// is impossible, and this test pins it at the ControlPlane level.
#[test]
fn old_graph_never_sees_the_new_tracks_params() {
    // Harness: ControlPlane over EngineHandle::for_tests() (no engine
    // thread), tables: SharedGraphTables driven MANUALLY — publish gen-1
    // tables for {X: slot 0}; commit remove-X + add-Y; simulate the
    // control thread's rebuild by publishing gen-2 tables {Y: slot 0}
    // with a FRESH ParamTable; commit a set_track_mix(Y, gain -6);
    // assert gen-2 table slot 0 changed and the RETAINED gen-1 Arc's
    // slot 0 still holds X's original gain.
}
```

(The `test_plane_with_tracks` pattern in `control/mod.rs` tests shows the `ControlPlane` + `for_tests` wiring; replicate locally with the `tables` handle kept by the test.)

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml --test identity_properties` — PASS.

- [ ] **Step 3: Meter-generation pin + Gate A extension**

Add the integration-level meter test: fold a gen-1 block and a gen-2 block for remapped slots through `GenerationMaps` exactly as the Task 6 unit test does, but constructed from `derive_slots` outputs of a real before/after track list (the unit test uses hand-built maps; this one proves the derivation and the fold agree end to end).

Extend `channel_properties.rs`: `arb_op_batches`'s `TrackAdd`/`TrackRemove` generation now attaches 0..2 clips (with minted `ClipId`s, `track_id` = the op's track) and empty `clip_indices` — undo/redo byte-stability must hold across clip-carrying histories (the exclusion Task 1 removed).

Run: `timeout 600 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: full suite PASS — **this is Gate B green.**

- [ ] **Step 4: Commit**

```bash
git add -A src-tauri
git commit -m "test(identity): Gate B — delete/undo identity property, slot-aliasing pin, meter-generation pin"
```

---

### Task 9: Close-out — counts, handoff, next stage

**Files:**
- Modify: `README.md`, `CONTRIBUTING.md` (dated test counts — run the suites, record the real numbers)
- Modify: `docs/PHASE4-PLAN.md` (append a "Plan B handoff" section)
- Modify: `next-prompt.md` (rewrite for Plan C+D)

- [ ] **Step 1: Update dated counts**

Run both suites, foreground:

```bash
timeout 600 cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | tail -20
timeout 300 npm test 2>&1 | tail -10
```

Update the dated counts in README/CONTRIBUTING per the established convention (grep for `296` / the date-stamped count lines).

- [ ] **Step 2: Write the Plan B handoff into PHASE4-PLAN.md**

Append under the Plan A handoff, same style — binding carry-forwards:

- **Plan E:** the id-preserving piano roll gesture path (`midi_set_notes` still replaces arrays; ids churn per replace — the allocator is correct, preservation semantics arrive with per-entity ops). Midi mutations still bypass the transaction channel; note-id *allocation* is lock-serialized but not yet op-attributed.
- **Plan C/D:** `next_note_id` moves from `MidiClip` to the content object in the v3 split, unchanged semantics; `ContentId` is declared but unpopulated. Round-2 §2.1's split/merge/copy rules (split keeps ids and records the partition; merge remints the absorbed side; copy mints fresh) bind the content ops when they exist — no such operation exists yet.
- **Ledgered, not taken:** waveform pyramid cache is still keyed by clip id (dedup-by-source opportunity); `GraphTables` under a Mutex is fine at today's rates — revisit only with a measurement (round-2 §10.3's rule).
- **Standing:** snapshot-rebuild deferral unchanged (Plan F); no panic rollback in `transact` (Plan F).

- [ ] **Step 3: Rewrite `next-prompt.md` for Plan C+D**

Same structure as the current file: where you are, what has happened (add Plan B's summary + commit range), the task (author and execute Plan C+D — time + project v3, round-2 §3 + §5, ADRs 0002/0004, one format bump not two), reading order, verified anchors (re-verify symbols at the closing commit), ground rules.

- [ ] **Step 4: Commit**

```bash
git add README.md CONTRIBUTING.md docs/PHASE4-PLAN.md next-prompt.md
git commit -m "docs: Plan B close-out — counts, handoff, next stage (Plan C+D)"
```

---

## Execution notes (for the orchestrator)

- Order: **1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9.** Tasks 3 and 4 are parallel-safe *in principle* (disjoint files except `types.rs` literals) but run them sequentially — both touch widespread struct literals and a merge conflict costs more than the parallelism buys.
- Model policy (worked for Plan A): implementers sonnet (haiku only for pure transcription), task reviewers sonnet; **Task 5, 6, 7 reviews and the final whole-branch review on the most capable model** (RT-heavy).
- Foreground `timeout`-guarded test runs ONLY.
- Commit per task; push to PR #1 when the final review is clean.
- SDD ledger in the session's SDD workspace (`.superpowers/sdd/`).
