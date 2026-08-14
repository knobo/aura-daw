# MIDI Clip Looping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Exception for this run:** executed SOLO, in-session, task-by-task, one
> commit per task, self-reviewed — no subagents (binding constraint from the
> handoff, knobo asleep). The checkboxes below are still the authoritative
> task list and gate order.

**Goal:** Drag a MIDI clip's right edge on the timeline to loop its content
(repeat until the placement ends, one clip box, edits apply to every
repetition), then add Ctrl+C/V/D clip stamping (independent copies).

**Architecture:** One new optional content-length field on `MidiClip`
(`contentLengthTicks`, absent = today's semantics) threaded through the
single scheduling seam `midi/schedule.rs::clip_events()` as a repetition
loop, an additive `midi_set_clip_bounds` command that also closes the
pre-existing "drag-move never persists" hole, and a frontend edge-drag
gesture + repeat-aware rendering. Stamping reuses the existing
`midi_add_clip` + `midi_set_notes` commands (no new backend surface).

**Tech Stack:** Rust (Tauri backend, `src-tauri/src/midi/**`), TypeScript +
Svelte 5 runes (`src/lib/state/midi.svelte.ts`,
`src/lib/components/MidiClipView.svelte`), `cargo test`, `vitest`.

**Spec:** `docs/superpowers/specs/2026-08-13-midi-clip-looping-design.md`
(companion handoff: `docs/superpowers/specs/2026-08-13-midi-clip-looping-handoff.md`).
The plan argues from the spec — read both; this document does not repeat the
spec's rationale, only the concrete steps.

## Global Constraints

- All musical positions/lengths are integer ticks at the project PPQ — never
  seconds, never samples (SCALABILITY / D-02, spec §3).
- `contentLengthTicks` is additive: `Option<u64>` in Rust, `#[serde(default)]`,
  never `required` in the JSON schema or in any Rust struct literal outside
  this feature's own writers.
- `MidiClip.lengthTicks` keeps meaning *placement* length (ADR 0004); the new
  field is the *content* (loop/native) length. Absent ⇒ content length =
  `lengthTicks` (today's behavior, byte-identical).
- All scheduling flows through `midi/schedule.rs::clip_events()` — offline
  export and live playback must not fork (spec §4).
- The new command `midi_set_clip_bounds` takes `crate::ids::ClipId` (typed),
  per the handoff's binding instruction.
- TDD: a failing test before the implementation, for every task that has
  backend or store-level logic. Gesture/rendering-only changes (no test
  infra per spec §7) are hand-verified by careful reading, `svelte-check`,
  and the full `vitest run` / `cargo test` gates.
- Backend gate: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  (baseline 346 passing at ae7c868, expect it to grow).
- Frontend gate: `timeout 300 npx vitest run` (baseline 80 passing).
- One commit per task, trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- Push at each task boundary (spec says push at milestones; this plan pushes
  every commit since force-pushing/rewriting is never used).

---

## File Structure

- `src-tauri/src/midi/types.rs` — `MidiClip.content_length_ticks` field +
  `effective_content_length_ticks()` accessor.
- `src-tauri/src/midi/schedule.rs` — `clip_events()` repetition loop.
- `src-tauri/src/midi/persist.rs` — `PersistedClip` row gains
  `content_length_ticks`; writer/reconstruct round-trip it.
- `src-tauri/src/midi/amt.rs` — `infill_params` validates against content
  length, not placement length.
- `src-tauri/src/midi/mod.rs` — new `midi_set_clip_bounds` command.
- `src-tauri/src/lib.rs` — register the new command (zone C block).
- `docs/ipc-schemas/midi-clip.schema.json` — additive `contentLengthTicks`
  in `$defs/clip` and `$defs/persistedClip`.
- `src/lib/types/ipc.ts` — `MidiClip.contentLengthTicks?`.
- `src/lib/tauri.ts` — `midiSetClipBounds` wrapper.
- `src/lib/state/midi.svelte.ts` — `setClipBounds`, drag-move reroute,
  content-length-aware `open()`, copy/paste/duplicate (`clipboard`,
  `copySelected`, `pasteAtPlayhead`, `duplicateSelected`).
- `src/lib/state/clip-edit-loop.test.ts` — update the wiring test for
  content-length bounds.
- `src/lib/components/MidiClipView.svelte` — right-edge drag gesture, repeat
  separators, content-relative mini preview.
- `src/lib/demo.ts` — modulo the browser demo engine's voice scheduling and
  meter envelope by content length.
- `src/App.svelte` — edge-jump tempo-map bug fix; Ctrl+C/V/D wiring.
- `src/lib/state/midi-stamp.test.ts` — new test file for copy/paste/duplicate
  placement math.

---

## Task 1: Data model — `contentLengthTicks`

**Files:**
- Modify: `src-tauri/src/midi/types.rs`
- Modify (mechanical, compiler-driven): every `MidiClip { .. }` literal in
  `src-tauri/src/midi/{mod,schedule,persist,midifile,playback,amt}.rs`,
  `src-tauri/src/audio/offline.rs`, `src-tauri/src/control/{mod,hum}.rs`,
  `src-tauri/src/plugins/{lv2_host,clap_host}.rs`.

**Interfaces:**
- Produces: `MidiClip.content_length_ticks: Option<u64>` (serde
  `contentLengthTicks`, `#[serde(default)]`); `MidiClip::effective_content_length_ticks(&self) -> u64`
  (returns `content_length_ticks.unwrap_or(length_ticks)`, minimum 1).

- [x] **Step 1: Write the failing test** (append to `types.rs`'s `#[cfg(test)] mod tests`)

```rust
#[test]
fn effective_content_length_defaults_to_placement_length() {
    let mut clip = MidiClip {
        id: "c-1".into(), track_id: "t-1".into(), name: "c".into(),
        timeline_start_ticks: 0, length_ticks: 3840,
        notes: vec![], next_note_id: 1, content_length_ticks: None,
    };
    assert_eq!(clip.effective_content_length_ticks(), 3840, "absent -> placement length");
    clip.content_length_ticks = Some(960);
    assert_eq!(clip.effective_content_length_ticks(), 960, "explicit value wins");
    // Defensive floor: a stray 0 (should never be constructed, but the
    // accessor must never hand back 0 — a zero-length repeat period would
    // divide-by-zero in the scheduler).
    clip.content_length_ticks = Some(0);
    assert_eq!(clip.effective_content_length_ticks(), 1, "floors at 1");
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib midi::types:: 2>&1 | tail -40`
Expected: FAIL to compile — no field `content_length_ticks`, no method
`effective_content_length_ticks`.

- [x] **Step 3: Add the field and accessor**

In `MidiClip` (after `next_note_id`):

```rust
    /// Content (loop/native) length in ticks — ADR 0004's content half of
    /// the content/placement split; `length_ticks` stays the placement
    /// length. Absent (the wire default) means "same as `length_ticks`"
    /// (today's pre-looping semantics, byte-identical). Set the first time
    /// a clip's right edge is dragged past its content: content length is
    /// pinned to whatever `length_ticks` was at that moment.
    #[serde(default)]
    pub content_length_ticks: Option<u64>,
```

And in `impl MidiClip` (after `ensure_note_ids`):

```rust
    /// The length `clip_events()` repeats content over: the explicit
    /// content length when set, else the placement length (today's
    /// semantics). Never 0 (a defensive floor — a 0 period would make the
    /// scheduler's repeat loop divide by zero; `content_length_ticks: Some(0)`
    /// should never be constructed, but this accessor is the single seam
    /// every reader goes through, so it's the one place worth guarding).
    pub fn effective_content_length_ticks(&self) -> u64 {
        self.content_length_ticks.unwrap_or(self.length_ticks).max(1)
    }
```

- [x] **Step 4: Fix every other `MidiClip { .. }` literal (compiler-driven)**

Run: `cd src-tauri && cargo build 2>&1 | grep "missing structure field\|-->" | head -80`

For every reported call site, add `content_length_ticks: None,` — EXCEPT:
none of the existing call sites need `Some(..)`; they are all either fresh
clips (creation), migration/import paths, or test helpers, and "absent"
(today's semantics) is correct for every one of them. Do this file by file;
after each file, re-run `cargo build` to confirm that file's errors clear.

- [x] **Step 5: Run test to verify it passes, and the whole crate still builds**

Run: `cd src-tauri && cargo build 2>&1 | tail -20 && cargo test --lib midi::types:: 2>&1 | tail -20`
Expected: builds clean; `effective_content_length_defaults_to_placement_length` PASSES.

- [x] **Step 6: Run the full backend suite**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | tail -30`
Expected: 347 passed (346 baseline + 1 new), 0 failed.

- [x] **Step 7: Commit**

```bash
git add -A -- src-tauri/src
git commit -m "$(cat <<'EOF'
feat(midi): add MidiClip.contentLengthTicks (content/placement split)

ADR 0004's content half: content_length_ticks: Option<u64> (wire
contentLengthTicks, serde default, absent = today's semantics = same as
lengthTicks). effective_content_length_ticks() is the one accessor every
reader (scheduler, amt validation, frontend bounds) goes through.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push -u origin midi-clip-looping
```

---

## Task 2: Repeat expansion in `clip_events()`

**Files:**
- Modify: `src-tauri/src/midi/schedule.rs`

**Interfaces:**
- Consumes: `MidiClip::effective_content_length_ticks()` (Task 1).
- Produces: `clip_events()` keeps its existing signature
  `fn clip_events(clip: &MidiClip, map: &TempoMap) -> Vec<AbsNoteEvent>` —
  callers (`playback.rs`, `offline.rs`) need no changes (spec §4: shared seam).

- [x] **Step 1: Write the failing tests** (append to `schedule.rs`'s test module)

```rust
    #[test]
    fn repeats_content_across_a_longer_placement() {
        // Content is one beat (960 ticks) long, placement is 2 beats — the
        // single note repeats once, at content-length offset.
        let mut c = clip(0, 1920, vec![note(0, 480, 60, 100)]);
        c.content_length_ticks = Some(960);
        let ev = clip_events(&c, &map_120());
        assert_eq!(
            ev,
            vec![
                AbsNoteEvent { sample: 0, key: 60, velocity: 100 },
                AbsNoteEvent { sample: 480 * 25, key: 60, velocity: 0 },
                AbsNoteEvent { sample: 960 * 25, key: 60, velocity: 100 },
                AbsNoteEvent { sample: (960 + 480) * 25, key: 60, velocity: 0 },
            ],
            "note repeats once at the content-length offset"
        );
    }

    #[test]
    fn partial_final_repeat_clamps_the_note_tail() {
        // Content is one beat (960 ticks); placement is 1.5 beats (1440) —
        // the second repeat's note-on lands inside the placement, but its
        // note-off must clamp to the placement end, not run past it.
        let mut c = clip(0, 1440, vec![note(0, 960, 60, 100)]); // near-full-beat note
        c.content_length_ticks = Some(960);
        let ev = clip_events(&c, &map_120());
        assert_eq!(ev.len(), 4, "both repeats' onsets are before placement end (1440)");
        assert_eq!(ev[0], AbsNoteEvent { sample: 0, key: 60, velocity: 100 });
        assert_eq!(ev[1], AbsNoteEvent { sample: 960 * 25, key: 60, velocity: 0 });
        assert_eq!(ev[2], AbsNoteEvent { sample: 960 * 25, key: 60, velocity: 100 });
        // Off would naturally land at (960+960)*25 = 1920*25, but placement
        // ends at 1440*25 — clamp there.
        assert_eq!(ev[3], AbsNoteEvent { sample: 1440 * 25, key: 60, velocity: 0 });
    }

    #[test]
    fn content_equal_to_placement_is_a_no_op() {
        // No content_length_ticks set (None) and an explicit Some equal to
        // length_ticks must produce IDENTICAL output to today's behavior.
        let c = clip(3840, 3840, vec![note(960, 960, 60, 100)]);
        let mut c_explicit = c.clone();
        c_explicit.content_length_ticks = Some(3840);
        let baseline = clip_events(&c, &map_120());
        assert_eq!(clip_events(&c_explicit, &map_120()), baseline, "explicit == placement matches absent");
        assert_eq!(baseline.len(), 2, "single repeat, unchanged from the pre-looping behavior");
    }

    #[test]
    fn tempo_change_mid_repetition_moves_the_later_repeat() {
        // 120bpm for bar 1, 60bpm after. Content is one bar (3840 ticks);
        // placement is two bars — repeat 2 starts exactly at the tempo
        // change and must use the new tempo's sample rate.
        let map = TempoMap::new(
            960,
            vec![TempoEvent { tick: 0, bpm: 120.0 }, TempoEvent { tick: 3840, bpm: 60.0 }],
            48_000,
        )
        .unwrap();
        let mut c = clip(0, 7680, vec![note(0, 480, 64, 90)]);
        c.content_length_ticks = Some(3840);
        let ev = clip_events(&c, &map);
        // Repeat 0: on at tick 0 (120bpm) -> sample 0.
        assert_eq!(ev[0], AbsNoteEvent { sample: 0, key: 64, velocity: 90 });
        // Repeat 1: on at tick 3840 (exactly the tempo change) -> 3840*25 = 96000.
        assert_eq!(ev[2].sample, 96_000);
        assert_eq!(ev[2].velocity, 90);
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test --lib schedule:: 2>&1 | tail -60`
Expected: the four new tests FAIL (no repetition — today's `clip_events`
only ever emits one pass over `clip.notes`).

- [x] **Step 3: Implement the repetition loop**

Replace `clip_events`'s body in `schedule.rs`:

```rust
pub fn clip_events(clip: &MidiClip, map: &TempoMap) -> Vec<AbsNoteEvent> {
    let clip_end_tick = clip.timeline_start_ticks.saturating_add(clip.length_ticks);
    let content_len = clip.effective_content_length_ticks();
    let repeats = clip.length_ticks.div_ceil(content_len).max(1);
    let mut out = Vec::with_capacity(clip.notes.len() * 2 * repeats as usize);
    for rep in 0..repeats {
        let rep_start_tick = clip.timeline_start_ticks.saturating_add(rep.saturating_mul(content_len));
        for n in &clip.notes {
            if n.velocity == 0 || n.length_ticks == 0 {
                continue;
            }
            let on_tick = rep_start_tick.saturating_add(n.tick as u64);
            if on_tick >= clip_end_tick {
                continue;
            }
            let off_tick = on_tick
                .saturating_add(n.length_ticks as u64)
                .min(clip_end_tick);
            let on_s = map.tick_to_samples(on_tick);
            let mut off_s = map.tick_to_samples(off_tick);
            if off_s <= on_s {
                off_s = on_s + 1;
            }
            out.push(AbsNoteEvent { sample: on_s, key: n.key, velocity: n.velocity });
            out.push(AbsNoteEvent { sample: off_s, key: n.key, velocity: 0 });
        }
    }
    out.sort_by_key(|e| (e.sample, e.velocity));
    out
}
```

This keeps every existing rule (crop at placement end, note-off clamp,
zero-length floor) exactly as-is — the only change is the outer `for rep`
loop and the offset added to each note's abs tick. When `content_len ==
clip.length_ticks` (the `None` default), `repeats == 1` and `rep_start_tick
== clip.timeline_start_ticks` for the only iteration: byte-identical to the
old body.

- [x] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib schedule:: 2>&1 | tail -40`
Expected: all schedule.rs tests (old + 4 new) PASS.

- [x] **Step 5: Run the full backend suite**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | tail -30`
Expected: 351 passed, 0 failed.

- [x] **Step 6: Commit**

```bash
git add src-tauri/src/midi/schedule.rs
git commit -m "$(cat <<'EOF'
feat(midi): repeat content across the placement in clip_events()

Wraps the existing note loop in a repetition loop over
effective_content_length_ticks(), keeping every existing crop/clamp rule
unchanged (placement-end crop, note-off clamp, zero-length floor). content
== placement (the None default) is byte-identical to the pre-looping
behavior — verified by an explicit test. Offline export and live playback
share this seam untouched (spec §4).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Task 3: Persistence round-trip + schema docs

**Files:**
- Modify: `src-tauri/src/midi/persist.rs`
- Modify: `docs/ipc-schemas/midi-clip.schema.json`

**Interfaces:**
- Consumes: `MidiClip.content_length_ticks` (Task 1).
- Produces: `PersistedClip.content_length_ticks: Option<u64>`, written to the
  JSON row as `contentLengthTicks` only when `Some` (never emit the key when
  `None` — keeps old projects byte-diff-free on resave).

- [x] **Step 1: Write the failing tests** (append to `persist.rs`'s test module)

```rust
    #[test]
    fn content_length_ticks_round_trips_when_present() {
        let parent = tmp_parent("content-length-present");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let mut c = clip("t1", some_notes(2));
        c.length_ticks = 7680;
        c.content_length_ticks = Some(3840);
        let midi = store_with(vec![c]);
        save_into_project(&dir, &midi).unwrap();

        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(raw["midiClips"][0]["contentLengthTicks"], 3840);

        let v2 = load_from_project(&dir).unwrap().unwrap();
        assert_eq!(v2.clips[0].content_length_ticks, Some(3840));
        assert_eq!(v2.clips[0].length_ticks, 7680);
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn content_length_ticks_absent_round_trips_as_none() {
        let parent = tmp_parent("content-length-absent");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let midi = store_with(vec![clip("t1", some_notes(1))]); // content_length_ticks: None
        save_into_project(&dir, &midi).unwrap();

        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert!(
            raw["midiClips"][0].get("contentLengthTicks").is_none(),
            "absent stays absent on disk — never writes null or the placement length"
        );

        let v2 = load_from_project(&dir).unwrap().unwrap();
        assert_eq!(v2.clips[0].content_length_ticks, None);
        let _ = fs::remove_dir_all(&parent);
    }
```

Also update the `clip(...)` test helper at the top of `persist.rs`'s test
module to set `content_length_ticks: None` (the compiler will point this
out — see Task 1 Step 4's pattern).

- [x] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test --lib persist:: 2>&1 | tail -60`
Expected: FAIL to compile (`PersistedClip`/row-writer have no
`content_length_ticks` / `contentLengthTicks`), or the round-trip assertions
fail once it compiles.

- [x] **Step 3: Implement**

In `PersistedClip` (add field):

```rust
    #[serde(default)]
    content_length_ticks: Option<u64>,
```

In `save_into_project`'s row-building loop, right after the `nextNoteId`
insert:

```rust
        if let Some(cl) = clip.content_length_ticks {
            row["contentLengthTicks"] = json!(cl);
        }
```

In `load_from_project`'s clip reconstruction, add the field to the
`MidiClip { .. }` literal:

```rust
            content_length_ticks: row.content_length_ticks,
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib persist:: 2>&1 | tail -40`
Expected: all persist.rs tests PASS, including the 2 new ones.

- [x] **Step 5: Update the JSON schema (additive, D-06)**

In `docs/ipc-schemas/midi-clip.schema.json`, add to both `$defs/clip` and
`$defs/persistedClip`'s `properties` (NOT `required`):

```json
        "contentLengthTicks": {
          "description": "Content (loop/native) length in ticks — ADR 0004's content half of the placement/content split; lengthTicks stays the placement length. Absent means \"same as lengthTicks\" (pre-looping semantics). When lengthTicks > contentLengthTicks the content repeats to fill the placement; when shorter, content is cropped. Additive (D-06).",
          "type": "integer", "minimum": 1, "maximum": 18446744073709551615
        }
```

Place it directly after `"lengthTicks"` in each `$defs` block.

- [x] **Step 6: Run the full backend suite**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | tail -30`
Expected: 353 passed, 0 failed.

- [x] **Step 7: Commit**

```bash
git add src-tauri/src/midi/persist.rs docs/ipc-schemas/midi-clip.schema.json
git commit -m "$(cat <<'EOF'
feat(midi): persist contentLengthTicks (round-trips, additive)

PersistedClip row writer only emits contentLengthTicks when Some (never
writes null/placement-length for old projects, so a resave of a
pre-looping project is byte-identical). Schema docs updated additively in
both clip and persistedClip $defs (D-06: no additionalProperties:false,
optional, never required).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Task 4: AMT infill validates against content length

**Files:**
- Modify: `src-tauri/src/midi/amt.rs`

**Interfaces:**
- Consumes: `MidiClip::effective_content_length_ticks()` (Task 1).

- [x] **Step 1: Write the failing test** (append to `amt.rs`'s test module — check the existing tests first for the helper clip-builder pattern and mirror it)

```rust
    #[test]
    fn region_validates_against_content_length_not_placement_length() {
        use crate::ids::NoteId;
        let mut clip = MidiClip {
            id: "c1".into(), track_id: "t1".into(), name: "c".into(),
            timeline_start_ticks: 0, length_ticks: 7680, // placement: 2 bars
            notes: vec![], next_note_id: 1, content_length_ticks: Some(3840), // content: 1 bar
        };
        // A region inside the CONTENT (1 bar) succeeds even though the
        // PLACEMENT is 2 bars.
        assert!(infill_params(960, 120.0, &clip, 0, 3840, None, None).is_ok());
        // A region past the content (but still inside the placement) is
        // rejected — infill always targets the one repeated bar of content,
        // never a placement-relative region.
        let err = infill_params(960, 120.0, &clip, 0, 3841, None, None).unwrap_err();
        assert!(err.contains("exceeds"), "got: {err}");
        clip.content_length_ticks = None; // absent -> falls back to placement length, unchanged behavior
        assert!(infill_params(960, 120.0, &clip, 0, 7680, None, None).is_ok());
        let _ = NoteId(0); // silence unused import if the file doesn't already use it
    }
```

(Drop the trailing `let _ = NoteId(0);` line if `amt.rs`'s test module
already imports `NoteId` elsewhere — check before pasting to avoid an
unused-import warning either way.)

- [x] **Step 2: Run test to verify it fails**

Run: `cd src-tauri && cargo test --lib amt:: 2>&1 | tail -40`
Expected: FAIL — a region of length 3840 against a 7680-length clip
currently validates fine either way (region_end <= clip.length_ticks), so
the region=3841 rejection assertion is what actually fails first (today it
would NOT error, since 3841 <= 7680 placement length).

- [x] **Step 3: Implement**

In `infill_params`, change the bound check:

```rust
    let content_len = clip.effective_content_length_ticks();
    if region_end as u64 > content_len {
        return Err(format!(
            "region end {region_end} exceeds content length {content_len}"
        ));
    }
```

(replaces the old `if region_end as u64 > clip.length_ticks` block.)

- [x] **Step 4: Run test to verify it passes**

Run: `cd src-tauri && cargo test --lib amt:: 2>&1 | tail -40`
Expected: all amt.rs tests PASS.

- [x] **Step 5: Run the full backend suite**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | tail -30`
Expected: 354 passed, 0 failed.

- [x] **Step 6: Commit**

```bash
git add src-tauri/src/midi/amt.rs
git commit -m "$(cat <<'EOF'
fix(midi): AMT infill region validates against content length

infill_params bounded regionEndTicks against the PLACEMENT length
(length_ticks); once a clip loops, infill always targets the one repeated
bar of content, so it must validate against effective_content_length_ticks()
instead. Absent contentLengthTicks falls back to length_ticks — unchanged
behavior for every clip that has never been drag-looped.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Task 5: `midi_set_clip_bounds` command

**Files:**
- Modify: `src-tauri/src/midi/mod.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `crate::ids::ClipId` (from the identity-groundwork merge).
- Produces: a pure, directly-testable core `fn apply_clip_bounds(clips: &mut [MidiClip], clip_id: &crate::ids::ClipId, timeline_start_ticks: u64, length_ticks: u64, content_length_ticks: Option<u64>) -> Result<MidiClip, String>`,
  plus the thin `#[tauri::command] pub fn midi_set_clip_bounds(clip_id: crate::ids::ClipId, timeline_start_ticks: u64, length_ticks: u64, content_length_ticks: Option<u64>, state: State<'_, MidiState>, audio: State<'_, AudioState>) -> Result<MidiClip, String>`
  wrapper that calls it through `with_synced_store`, registered in `lib.rs`'s
  `generate_handler!` zone-C block.

**Correction found while reading `mod.rs`'s existing tests (logged in
progress.md):** this module has NO harness anywhere for constructing
`tauri::State<'_, MidiState>` / `State<'_, AudioState>` in a unit test —
`midi_add_clip` and `midi_set_notes` are themselves never called directly by
a test. The established convention in this file (see
`assign_incoming_note_ids`, `sync_midi_store`, `synced_to_dir`) is: pull the
command's validation + mutation logic out into a plain function over owned
data, test THAT directly, and leave the `#[tauri::command]` wrapper itself
untested (same as every other command here). `midi_set_clip_bounds` follows
that convention via `apply_clip_bounds`, below — matching, not deviating
from, the codebase.

- [x] **Step 1: Write the failing tests** (append to `mod.rs`'s test module,
  near `assign_incoming_note_ids`'s tests)

```rust
    fn clip_for_bounds(id: &str, length_ticks: u64) -> MidiClip {
        MidiClip {
            id: id.into(), track_id: "t1".into(), name: "c".into(),
            timeline_start_ticks: 0, length_ticks, notes: Vec::new(),
            next_note_id: 1, content_length_ticks: None,
        }
    }

    #[test]
    fn apply_clip_bounds_moves_and_resizes() {
        let mut clips = vec![clip_for_bounds("c1", 1920), clip_for_bounds("c2", 500)];
        let updated = apply_clip_bounds(&mut clips, &"c1".into(), 960, 3840, Some(1920)).unwrap();
        assert_eq!(updated.timeline_start_ticks, 960);
        assert_eq!(updated.length_ticks, 3840);
        assert_eq!(updated.content_length_ticks, Some(1920));
        // Written into the slice, not just the return value.
        assert_eq!(clips[0].timeline_start_ticks, 960);
        assert_eq!(clips[0].content_length_ticks, Some(1920));
        // The other clip is untouched.
        assert_eq!(clips[1].timeline_start_ticks, 0);
    }

    #[test]
    fn apply_clip_bounds_can_clear_content_length_back_to_absent() {
        let mut clips = vec![clip_for_bounds("c1", 1920)];
        clips[0].content_length_ticks = Some(480);
        apply_clip_bounds(&mut clips, &"c1".into(), 0, 1920, None).unwrap();
        assert_eq!(clips[0].content_length_ticks, None, "explicit None clears a previously-set content length");
    }

    #[test]
    fn apply_clip_bounds_rejects_zero_length_and_zero_content_length() {
        let mut clips = vec![clip_for_bounds("c1", 1920)];
        assert!(apply_clip_bounds(&mut clips, &"c1".into(), 0, 0, None).is_err());
        assert!(apply_clip_bounds(&mut clips, &"c1".into(), 0, 100, Some(0)).is_err());
        // Rejected calls must not have mutated the clip.
        assert_eq!(clips[0].length_ticks, 1920);
    }

    #[test]
    fn apply_clip_bounds_rejects_unknown_clip() {
        let mut clips = vec![clip_for_bounds("c1", 1920)];
        assert!(apply_clip_bounds(&mut clips, &"no-such-clip".into(), 0, 100, None).is_err());
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cd src-tauri && cargo test --lib midi::mod:: 2>&1 | tail -60`
Expected: FAIL to compile — no `apply_clip_bounds` function yet.

- [x] **Step 3: Implement the pure core + the thin command wrapper** (add to
  `mod.rs`, after `midi_set_notes`/`assign_incoming_note_ids`)

```rust
/// Pure core of `midi_set_clip_bounds`: validates and applies a clip's new
/// placement (+ optional content length) against an owned clip slice —
/// unit-testable without a tauri State harness (mirrors
/// `assign_incoming_note_ids`'s split for `midi_set_notes`). `None` for
/// `content_length_ticks` explicitly CLEARS a previously-set content length
/// back to "same as placement" — the command always sends the caller's
/// current intent, never merges partial updates.
fn apply_clip_bounds(
    clips: &mut [MidiClip],
    clip_id: &crate::ids::ClipId,
    timeline_start_ticks: u64,
    length_ticks: u64,
    content_length_ticks: Option<u64>,
) -> Result<MidiClip, String> {
    if length_ticks == 0 {
        return Err("lengthTicks must be > 0".into());
    }
    if content_length_ticks == Some(0) {
        return Err("contentLengthTicks must be > 0 when present".into());
    }
    let clip = clips
        .iter_mut()
        .find(|c| &c.id == clip_id)
        .ok_or_else(|| format!("unknown MIDI clip: {clip_id}"))?;
    clip.timeline_start_ticks = timeline_start_ticks;
    clip.length_ticks = length_ticks;
    clip.content_length_ticks = content_length_ticks;
    Ok(clip.clone())
}

/// Move and/or resize a clip's placement (and optionally pin its content
/// length) — one additive command serving both the edge-drag gesture (sets
/// placement + content length atomically) and plain clip moves, which
/// closes a pre-existing hole: `midi.svelte.ts::moveClip()` was
/// frontend-only, so a dragged clip never reached the scheduler or the
/// project file (spec §5).
#[tauri::command]
pub fn midi_set_clip_bounds(
    clip_id: crate::ids::ClipId,
    timeline_start_ticks: u64,
    length_ticks: u64,
    content_length_ticks: Option<u64>,
    state: State<'_, MidiState>,
    audio: State<'_, AudioState>,
) -> Result<MidiClip, String> {
    with_synced_store(&audio, &state, true, move |s| {
        apply_clip_bounds(&mut s.clips, &clip_id, timeline_start_ticks, length_ticks, content_length_ticks)
    })
}
```

- [x] **Step 4: Register in `lib.rs`**

In the `// ---- midi (phase 2, zone C) ----` block of `generate_handler!`,
add right after `midi::midi_set_notes,`:

```rust
            midi::midi_set_clip_bounds,
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cd src-tauri && cargo test --lib midi:: 2>&1 | tail -60`
Expected: all `mod.rs` tests PASS, including the 4 new ones.

- [x] **Step 6: Run the full backend suite**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | tail -30`
Expected: 358 passed, 0 failed.

- [x] **Step 7: Commit**

```bash
git add src-tauri/src/midi/mod.rs src-tauri/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(midi): additive midi_set_clip_bounds command

Moves/resizes a clip's placement and optionally sets its content length,
atomically, through the typed ClipId. Serves both the upcoming edge-drag
gesture and plain clip moves — closes a pre-existing hole where dragging a
MIDI clip on the timeline never reached the scheduler or project file
(frontend-only moveClip). Registered in lib.rs's zone-C block.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Task 6: Frontend wiring — types, store, edge-jump fix

**Files:**
- Modify: `src/lib/types/ipc.ts`
- Modify: `src/lib/tauri.ts`
- Modify: `src/lib/state/midi.svelte.ts`
- Modify: `src/lib/state/clip-edit-loop.test.ts`
- Modify: `src/App.svelte`

**Interfaces:**
- Consumes: `midi_set_clip_bounds` (Task 5).
- Produces: `MidiClip.contentLengthTicks?: number` (ipc.ts);
  `backend.midiSetClipBounds(clipId, timelineStartTicks, lengthTicks, contentLengthTicks?)`
  (tauri.ts); `midi.setClipBounds(clipId, timelineStartTicks, lengthTicks, contentLengthTicks?)`,
  `midi.effectiveContentLengthTicks(clip)` (midi.svelte.ts) — used by Task 7's
  gesture/rendering code and Task 9's stamping.

- [x] **Step 1: Types + backend wrapper**

In `src/lib/types/ipc.ts`, add to `MidiClip`:

```typescript
  /** Content (loop/native) length in ticks; absent = same as lengthTicks. */
  contentLengthTicks?: number;
```

In `src/lib/tauri.ts`, add to the `Backend` interface (near `midiSetNotes`):

```typescript
  midiSetClipBounds(
    clipId: string,
    timelineStartTicks: number,
    lengthTicks: number,
    contentLengthTicks: number | null,
  ): Promise<MidiClip>;
```

and the implementation (near the `midiSetNotes` impl):

```typescript
  midiSetClipBounds(
    clipId: string,
    timelineStartTicks: number,
    lengthTicks: number,
    contentLengthTicks: number | null,
  ) {
    return invoke<MidiClip>("midi_set_clip_bounds", {
      clipId,
      timelineStartTicks,
      lengthTicks,
      contentLengthTicks,
    });
  },
```

- [x] **Step 2: Write the failing wiring test** (in `clip-edit-loop.test.ts`, replace the existing "piano roll wiring (midi store)" `describe` block's clip fixture and add a content-length case)

```typescript
describe("piano roll wiring (midi store)", () => {
  // ppq 960 @ 120 BPM, 48 kHz → 25 samples per tick.
  const clip = {
    id: "c1",
    trackId: "A",
    name: "riff",
    timelineStartTicks: 960,
    lengthTicks: 960,
    notes: [],
  } as unknown as (typeof midi.clips)[number];

  it("opening a MIDI clip enters the loop with tick-converted bounds", async () => {
    midi.clips = [clip];

    midi.open("c1");

    await vi.waitFor(() => expect(mocked.transportPlay).toHaveBeenCalled());
    expect(mocked.transportSetLoop).toHaveBeenCalledWith(true, 24000, 48000);
  });

  it("a looped clip (placement longer than content) loops the CONTENT, not the placement", async () => {
    // Placement is 4 beats (3840 ticks), content is 1 beat (960 ticks) —
    // the clip-edit loop must span only the content, per spec §6.
    midi.clips = [{ ...clip, lengthTicks: 3840, contentLengthTicks: 960 }];

    midi.open("c1");

    await vi.waitFor(() => expect(mocked.transportPlay).toHaveBeenCalled());
    expect(mocked.transportSetLoop).toHaveBeenCalledWith(true, 24000, 24000 + 960 * 25);
  });

  it("closing the editor restores the pre-edit state", async () => {
    midi.clips = [clip];
    midi.open("c1");
    await vi.waitFor(() => expect(mocked.transportPlay).toHaveBeenCalled());
    vi.clearAllMocks();

    midi.closeEditor();

    await vi.waitFor(() => expect(mocked.transportStop).toHaveBeenCalled());
    expect(mocked.transportSetLoop).toHaveBeenCalledWith(false, 0, 0);
    expect(mocked.transportSeek).toHaveBeenCalledWith(0);
  });
});
```

- [x] **Step 3: Run test to verify it fails**

Run: `timeout 300 npx vitest run src/lib/state/clip-edit-loop.test.ts 2>&1 | tail -60`
Expected: the new "loops the CONTENT" test FAILS — `open()` currently uses
`clip.timelineStartTicks + clip.lengthTicks` unconditionally.

- [x] **Step 4: Implement the store changes** in `midi.svelte.ts`

Add a content-length accessor and `setClipBounds`, and change `open()` to
use it. Replace the `moveClip` method's body to keep it purely local (used
during the drag, unchanged) and add a new `commitBounds` that persists —
`MidiClipView.svelte` (Task 7) calls `commitBounds` on pointer-up instead of
invoking the backend on every `moveClip` call (D-03: one invoke per
gesture). Edit the class body:

```typescript
  /** Effective content (loop) length: explicit, else the placement length —
   * the single accessor every content-relative reader (piano roll bounds,
   * repeat rendering, the clip-edit loop) goes through. */
  effectiveContentLengthTicks(clip: MidiClip): number {
    return Math.max(1, clip.contentLengthTicks ?? clip.lengthTicks);
  }
```

(place this as a method on `MidiStore`, near `ticksPerBar`)

```typescript
  /** Frontend-only placement move (used DURING a drag; see commitBounds for
   * the persisted end of the gesture — D-03: one invoke per gesture, not
   * one per pointermove). */
  moveClip(clipId: string, timelineStartTicks: number) {
    const t = Math.max(0, Math.round(timelineStartTicks));
    this.clips = this.clips.map((c) => (c.id === clipId ? { ...c, timelineStartTicks: t } : c));
  }

  /** Persist a clip's current placement/content bounds — called once at the
   * END of a move or edge-drag gesture (pointerup), never per pointermove.
   * Closes the pre-existing hole where moveClip() never reached the
   * scheduler or the project file. */
  async setClipBounds(
    clipId: string,
    timelineStartTicks: number,
    lengthTicks: number,
    contentLengthTicks?: number,
  ): Promise<void> {
    const t = Math.max(0, Math.round(timelineStartTicks));
    const len = Math.max(1, Math.round(lengthTicks));
    const cl = contentLengthTicks === undefined ? undefined : Math.max(1, Math.round(contentLengthTicks));
    this.clips = this.clips.map((c) =>
      c.id === clipId
        ? { ...c, timelineStartTicks: t, lengthTicks: len, contentLengthTicks: cl }
        : c,
    );
    try {
      const clip = await backend.midiSetClipBounds(clipId, t, len, cl ?? null);
      this.upsert(clip);
    } catch (err) {
      console.error("[aura] midi_set_clip_bounds failed:", err);
    }
  }

  /** Commit a clip's CURRENT in-store bounds to the backend — the pointerup
   * end of a move/edge-drag gesture that used moveClip()/local mutation
   * during the drag. */
  async commitBounds(clipId: string): Promise<void> {
    const c = this.clipById(clipId);
    if (!c) return;
    await this.setClipBounds(clipId, c.timelineStartTicks, c.lengthTicks, c.contentLengthTicks);
  }
```

Change `open()`'s `clipEditLoop.enter` call to use content length:

```typescript
  open(clipId: string) {
    this.openClipId = clipId;
    this.selectedClipId = clipId;
    if (this.region?.clipId !== clipId) this.region = null;
    const clip = this.clipById(clipId);
    if (clip) {
      void clipEditLoop.enter({
        trackId: clip.trackId,
        startSamples: this.ticksToSamples(clip.timelineStartTicks),
        endSamples: this.ticksToSamples(
          clip.timelineStartTicks + this.effectiveContentLengthTicks(clip),
        ),
      });
    }
  }
```

- [x] **Step 5: Run tests to verify they pass**

Run: `timeout 300 npx vitest run src/lib/state/clip-edit-loop.test.ts 2>&1 | tail -60`
Expected: all tests in the file PASS.

- [x] **Step 6: Fix the App.svelte edge-jump tempo-map bug**

In `src/App.svelte`'s `onKeydown`, the MIDI clip-edges mapping computes
`lengthSamples` wrong under a non-constant tempo map (it converts the tick
LENGTH directly instead of taking the difference of two converted
positions). Replace:

```typescript
          ...clipEdges(
            midi.clips.map((c) => ({
              timelineStartSamples: midi.ticksToSamples(c.timelineStartTicks),
              lengthSamples: midi.ticksToSamples(c.lengthTicks),
            })),
          ),
```

with:

```typescript
          ...clipEdges(
            midi.clips.map((c) => {
              const start = midi.ticksToSamples(c.timelineStartTicks);
              return {
                timelineStartSamples: start,
                lengthSamples: midi.ticksToSamples(c.timelineStartTicks + c.lengthTicks) - start,
              };
            }),
          ),
```

- [x] **Step 7: Run the full frontend suite**

Run: `timeout 300 npx vitest run 2>&1 | tail -40`
Expected: 81 passed (80 baseline + 1 new), 0 failed.

- [x] **Step 8: Typecheck**

Run: `timeout 300 npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -60`
Expected: 0 errors (existing warnings, if any, unchanged).

- [x] **Step 9: Commit**

```bash
git add src/lib/types/ipc.ts src/lib/tauri.ts src/lib/state/midi.svelte.ts src/lib/state/clip-edit-loop.test.ts src/App.svelte
git commit -m "$(cat <<'EOF'
feat(midi): wire contentLengthTicks + midi_set_clip_bounds on the frontend

midi.setClipBounds/commitBounds persist a clip's placement+content bounds
once per gesture (D-03), closing the pre-existing moveClip()-never-persists
hole. open() now loops the clip's CONTENT, not the stretched placement
(spec §6). Also fixes a pre-existing tempo-map bug in App.svelte's edge-jump
list: it was converting the tick LENGTH directly instead of the difference
of two converted positions, which drifts under a non-constant tempo map.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Task 7: Edge-drag gesture + repeat rendering

**Files:**
- Modify: `src/lib/components/MidiClipView.svelte`

**Interfaces:**
- Consumes: `midi.effectiveContentLengthTicks(clip)`, `midi.setClipBounds`,
  `midi.commitBounds`, `midi.moveClip` (Task 6).

No automated test infra for this component (spec §7: "Gesture/rendering
layers have no component-test infra: verified live in the app"). Verify by
careful reading + `svelte-check` + a full `vitest run` (regression only) —
flagged in `progress.md` for a live look later.

- [x] **Step 1: Add the right-edge drag gesture**

In `MidiClipView.svelte`'s script section, extend the existing drag state
with an edge-drag mode. Replace the "── drag / select ──" block:

```typescript
  // ── drag / select / edge-resize (mirrors ClipView + the ruler's loop pins) ──
  let dragging = $state(false);
  let dragMode: "move" | "resize" = "move";
  let dragStartX = 0;
  let dragOrigTicks = 0;
  let dragOrigLengthTicks = 0;
  let dragOrigContentTicks = 0; // pinned once, at drag start, for a resize gesture
  let dragMoved = false;

  const EDGE_PX = 8;

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    midi.select(clip.id);
    project.select(null);
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const nearRightEdge = rect.right - e.clientX <= EDGE_PX;
    dragMode = nearRightEdge ? "resize" : "move";
    dragging = true;
    dragMoved = false;
    dragStartX = e.clientX;
    dragOrigTicks = clip.timelineStartTicks;
    dragOrigLengthTicks = clip.lengthTicks;
    // Content length is pinned the moment a resize STARTS, not re-read
    // every pointermove — the spec's "set the first time the right edge is
    // dragged" rule: if the clip has no explicit content length yet, this
    // drag's start-of-gesture placement length BECOMES the content length.
    dragOrigContentTicks = midi.effectiveContentLengthTicks(clip);
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }
  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    const dx = e.clientX - dragStartX;
    if (Math.abs(dx) > 2) dragMoved = true;
    if (!dragMoved) return;
    if (dragMode === "resize") {
      let targetSamples = midi.ticksToSamples(dragOrigTicks + dragOrigLengthTicks) + dx * view.spp;
      if (!e.altKey) targetSamples = view.snapSamples(targetSamples);
      const newLengthTicks = Math.max(
        1,
        Math.round(midi.samplesToTicks(Math.max(0, targetSamples)) - dragOrigTicks),
      );
      midi.clips = midi.clips.map((c) =>
        c.id === clip.id
          ? { ...c, lengthTicks: newLengthTicks, contentLengthTicks: dragOrigContentTicks }
          : c,
      );
    } else {
      let targetSamples = midi.ticksToSamples(dragOrigTicks) + dx * view.spp;
      if (!e.altKey) targetSamples = view.snapSamples(targetSamples);
      midi.moveClip(clip.id, midi.samplesToTicks(Math.max(0, targetSamples)));
    }
  }
  function onPointerUp(e: PointerEvent) {
    const wasDragging = dragging && dragMoved;
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    if (wasDragging) void midi.commitBounds(clip.id);
  }
  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      midi.open(clip.id);
    } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      const dir = e.key === "ArrowLeft" ? -1 : 1;
      midi.moveClip(clip.id, clip.timelineStartTicks + dir * midi.ppq);
      void midi.commitBounds(clip.id);
    }
  }
```

Add an `ew-resize` cursor for the right-edge zone — extend the template's
root `<div>` with a pointer-move-driven CSS class. Add to the `$derived`
block (near `landed`):

```typescript
  let hoverEdge = $state(false);
  function onPointerHoverMove(e: PointerEvent) {
    if (dragging) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    hoverEdge = rect.right - e.clientX <= EDGE_PX;
  }
```

Wire it on the root element: add `onpointermove={(e) => { onPointerHoverMove(e); onPointerMove(e); }}`
in place of the existing `onpointermove={onPointerMove}`, and add
`class:edge={hoverEdge || dragMode === "resize"}` next to the existing
`class:selected` etc. In `<style>`, add:

```css
  .mclip.edge {
    cursor: ew-resize;
  }
```

- [x] **Step 2: Repeat-relative rendering**

The mini note-preview currently derives `pxPerTick` from the PLACEMENT
width/length (`widthPx / clip.lengthTicks`), which stretches notes across a
looped placement instead of repeating them. Replace the mini-preview
`$effect` in `MidiClipView.svelte`: change the `pxPerTick` line and the
per-note draw loop to iterate repeats, and draw a separator at each content
boundary. Replace:

```typescript
    const rowH = Math.min(4, h / (hi - lo + 1));
    const pxPerTick = widthPx / clip.lengthTicks;
    const offset = visL; // clip-local css px of canvas origin

    const r = parseInt(track.color.slice(1, 3), 16);
    const g = parseInt(track.color.slice(3, 5), 16);
    const b = parseInt(track.color.slice(5, 7), 16);
    for (const n of notes) {
      const x = n.tick * pxPerTick - offset;
      const nw = Math.max(1.5, n.lengthTicks * pxPerTick);
      if (x + nw < 0 || x > w) continue;
      const y = h - ((n.key - lo + 1) / (hi - lo + 1)) * h;
      ctx.fillStyle = `rgba(${r},${g},${b},${0.4 + 0.6 * (n.velocity / 127)})`;
      ctx.fillRect(x, y, nw, Math.max(1.5, rowH - 1));
    }
```

with:

```typescript
    const rowH = Math.min(4, h / (hi - lo + 1));
    const contentTicks = midi.effectiveContentLengthTicks(clip);
    const pxPerTick = widthPx / contentTicks;
    const offset = visL; // clip-local css px of canvas origin
    const repeats = Math.max(1, Math.ceil(clip.lengthTicks / contentTicks));

    const r = parseInt(track.color.slice(1, 3), 16);
    const g = parseInt(track.color.slice(3, 5), 16);
    const b = parseInt(track.color.slice(5, 7), 16);
    for (let rep = 0; rep < repeats; rep++) {
      const repOffsetTicks = rep * contentTicks;
      if (repOffsetTicks >= clip.lengthTicks) break;
      for (const n of notes) {
        const tick = repOffsetTicks + n.tick;
        if (tick >= clip.lengthTicks) continue; // cropped by the placement end
        const x = tick * pxPerTick - offset;
        const nw = Math.max(1.5, n.lengthTicks * pxPerTick);
        if (x + nw < 0 || x > w) continue;
        const y = h - ((n.key - lo + 1) / (hi - lo + 1)) * h;
        ctx.fillStyle = `rgba(${r},${g},${b},${0.4 + 0.6 * (n.velocity / 127)})`;
        ctx.fillRect(x, y, nw, Math.max(1.5, rowH - 1));
      }
      // Separator line at every content boundary (skip rep 0's leading edge).
      if (rep > 0) {
        const sepX = repOffsetTicks * pxPerTick - offset;
        if (sepX >= 0 && sepX <= w) {
          ctx.fillStyle = `rgba(${r},${g},${b},0.35)`;
          ctx.fillRect(sepX, 0, 1, h);
        }
      }
    }
```

And add `contentTicks` (via `clip.contentLengthTicks`) to the `$effect`'s
reactive-dependency line (`void view.spp, view.viewStart, clip.timelineStartTicks, clip.lengthTicks, clip.contentLengthTicks, track.color;`).

- [x] **Step 3: Note-flash modulo**

The pulse `$effect`'s rAF loop currently gates on `posTicks > clip.lengthTicks`
and searches `clip.notes` directly by raw tick — under a repeat, the
playhead position needs to be taken modulo the content length before
matching against note onsets. Change:

```typescript
      const posTicks =
        midi.samplesToTicks(transport.positionAt(now)) - clip.timelineStartTicks;
      if (posTicks < 0 || posTicks > clip.lengthTicks || clip.notes.length === 0) {
        el.style.opacity = "0";
        return;
      }
      // latest onset at/before the playhead (notes are tick-sorted)
      let last = -1;
      for (const n of clip.notes) {
        if (n.tick > posTicks) break;
        if (n.tick > last) last = n.tick;
      }
```

to:

```typescript
      const posTicks =
        midi.samplesToTicks(transport.positionAt(now)) - clip.timelineStartTicks;
      const contentTicks = midi.effectiveContentLengthTicks(clip);
      const posInContent = posTicks % contentTicks;
      if (posTicks < 0 || posTicks > clip.lengthTicks || clip.notes.length === 0) {
        el.style.opacity = "0";
        return;
      }
      // latest onset at/before the playhead within the CURRENT repeat
      // (notes are tick-sorted, content-relative)
      let last = -1;
      for (const n of clip.notes) {
        if (n.tick > posInContent) break;
        if (n.tick > last) last = n.tick;
      }
```

and the age calculation right after uses `posInContent` instead of
`posTicks`:

```typescript
      const ageSec = ((posInContent - last) / midi.ppq) * (60 / project.tempoBpm);
```

- [x] **Step 4: Typecheck**

Run: `timeout 300 npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -80`
Expected: 0 errors.

- [x] **Step 5: Run the full frontend suite (regression only — no new tests for this file)**

Run: `timeout 300 npx vitest run 2>&1 | tail -40`
Expected: 81 passed (unchanged from Task 6), 0 failed.

- [x] **Step 6: Commit**

```bash
git add src/lib/components/MidiClipView.svelte
git commit -m "$(cat <<'EOF'
feat(midi): right-edge drag-to-loop gesture + repeat-aware rendering

~8px right-edge hit zone (ew-resize cursor, same idiom as the ruler's loop
pins): dragging it resizes the placement and pins the content length to
whatever it was at drag start (first drag on an unlooped clip). The mini
note-preview and note-flash pulse both go content-relative (modulo
effectiveContentLengthTicks), with a separator line drawn at each repeat
boundary. Committed once per gesture via midi.commitBounds on pointerup —
never per pointermove (D-03).

No component-test infra for this file (spec §7) — verified by
svelte-check + the full vitest regression suite; flagged in progress.md for
a live look.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Task 8: Browser demo engine (`demo.ts`) repeat modulo

**Files:**
- Modify: `src/lib/demo.ts`

**Interfaces:**
- Consumes: no store import needed — `demo.ts` is a standalone engine with
  its own `ppq` field; mirror the SAME modulo logic Task 7 used, expressed
  against `mc.contentLengthTicks ?? mc.lengthTicks`.

`demo.ts` has no dedicated test file; it is the synthetic fallback engine
used outside Tauri (spec §6: "gets the identical modulo"). Verify with
`tsc`/`svelte-check` (it's plain `.ts`, covered by the same `npx svelte-check`
run) and the full `vitest run` regression gate.

- [x] **Step 1: Voice scheduling modulo**

Locate the note-scheduling block around the existing lines:

```typescript
      const clipStart = this.ticksToSamples(mc.timelineStartTicks);
      const clipEnd = this.ticksToSamples(mc.timelineStartTicks + mc.lengthTicks);
```

and the subsequent per-note scheduling that reads `n.tick`/`n.lengthTicks`
directly against absolute clip-relative position (search for the
`ns = ... ; ne = Math.min(ns + this.ticksToSamples(n.lengthTicks), clipEnd)`
line reported by `grep -n "clipEnd" src/lib/demo.ts`). Read the surrounding
~30 lines first — this block iterates `mc.notes` once per clip per
scheduling pass. Wrap that note iteration in a repeat loop over
`effectiveContentLen = mc.contentLengthTicks ?? mc.lengthTicks`, mirroring
Task 2's Rust repetition loop exactly: for `rep` from `0` while
`rep * effectiveContentLen < mc.lengthTicks`, offset each note's `tick` by
`rep * effectiveContentLen` before converting to samples, and skip/clamp
against `clipEnd` using the SAME crop/clamp rules as the existing code (onset
at/after `clipEnd` dropped, off clamped to `clipEnd`). Keep every existing
variable name and clamp exactly as it is today for the `rep === 0` case, so
a clip with no `contentLengthTicks` schedules byte-identically to before.

- [x] **Step 2: Meter envelope modulo**

Locate the meter-envelope block (search: `grep -n "posTicks - mc.timelineStartTicks" src/lib/demo.ts`,
around the existing lines:

```typescript
          const local = posTicks - mc.timelineStartTicks;
          if (local < 0 || local >= mc.lengthTicks) continue;
          for (const n of mc.notes) {
            if (local < n.tick || local >= n.tick + n.lengthTicks) continue;
```

Change to take `local` modulo the effective content length before matching
notes (same pattern as Task 7 Step 3's pulse fix):

```typescript
          const local = posTicks - mc.timelineStartTicks;
          if (local < 0 || local >= mc.lengthTicks) continue;
          const contentLen = mc.contentLengthTicks ?? mc.lengthTicks;
          const localInContent = local % Math.max(1, contentLen);
          for (const n of mc.notes) {
            if (localInContent < n.tick || localInContent >= n.tick + n.lengthTicks) continue;
```

(and use `localInContent` in place of `local` for the rest of that
matched-note block, if it references `local` again for envelope timing —
read the following lines before editing to confirm.)

- [x] **Step 3: Typecheck**

Run: `timeout 300 npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -80`
Expected: 0 errors.

- [x] **Step 4: Run the full frontend suite (regression only)**

Run: `timeout 300 npx vitest run 2>&1 | tail -40`
Expected: 81 passed, 0 failed.

- [x] **Step 5: Commit**

```bash
git add src/lib/demo.ts
git commit -m "$(cat <<'EOF'
feat(midi): repeat-aware voice scheduling + meter envelope in demo.ts

Mirrors schedule.rs's repetition loop (Task 2) and MidiClipView's pulse
modulo (Task 7) in the browser-only demo engine, so a looped clip sounds
and meters identically whether running against the real Tauri backend or
the synthetic fallback (spec §6). content == placement (no
contentLengthTicks) schedules byte-identically to before.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Task 9: Clip stamping — store logic (copy/paste/duplicate)

**Files:**
- Create: `src/lib/state/midi-stamp.test.ts`
- Modify: `src/lib/state/midi.svelte.ts`

**Interfaces:**
- Consumes: `backend.midiAddClip`, `backend.midiSetNotes`,
  `backend.midiSetClipBounds` (existing + Task 6).
- Produces: `midi.clipboard: MidiClip | null` (state), `midi.copySelected()`,
  `midi.pasteAtPlayhead(playheadTicks: number): Promise<MidiClip | null>`,
  `midi.duplicateSelected(): Promise<MidiClip | null>`.

- [x] **Step 1: Write the failing tests**

Create `src/lib/state/midi-stamp.test.ts`:

```typescript
/**
 * Clip stamping: Ctrl+C/V/D copy independent clips (fresh backend id, fresh
 * note ids — the backend's midi_set_notes keep-rule already mints fresh ids
 * whenever the target clip has no existing notes, so no new backend surface
 * is needed here; see progress.md's ruling). Placement math only — the
 * keyboard wiring lives in App.svelte (untested, no component-test infra).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MidiClip } from "../types/ipc";

let nextId = 1;
const midiAddClip = vi.fn(
  (trackId: string, name: string | null, timelineStartTicks: number, lengthTicks: number) =>
    Promise.resolve({
      id: `new-${nextId++}`,
      trackId,
      name: name ?? "MIDI Clip",
      timelineStartTicks,
      lengthTicks,
      notes: [],
    } as MidiClip),
);
const midiSetNotes = vi.fn((clipId: string, notes: MidiClip["notes"]) =>
  Promise.resolve({ id: clipId, notes } as MidiClip),
);
const midiSetClipBounds = vi.fn(
  (clipId: string, timelineStartTicks: number, lengthTicks: number, contentLengthTicks: number | null) =>
    Promise.resolve({
      id: clipId,
      timelineStartTicks,
      lengthTicks,
      contentLengthTicks: contentLengthTicks ?? undefined,
    } as MidiClip),
);

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    midiAddClip: (...a: Parameters<typeof midiAddClip>) => midiAddClip(...a),
    midiSetNotes: (...a: Parameters<typeof midiSetNotes>) => midiSetNotes(...a),
    midiSetClipBounds: (...a: Parameters<typeof midiSetClipBounds>) => midiSetClipBounds(...a),
    getProjectState: () => Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { midi } = await import("./midi.svelte");

function sourceClip(overrides: Partial<MidiClip> = {}): MidiClip {
  return {
    id: "src",
    trackId: "A",
    name: "riff",
    timelineStartTicks: 960,
    lengthTicks: 1920,
    notes: [{ tick: 0, lengthTicks: 480, key: 60, velocity: 100 }],
    ...overrides,
  } as MidiClip;
}

beforeEach(() => {
  nextId = 1;
  vi.clearAllMocks();
  midi.clips = [];
  midi.selectedClipId = null;
  midi.clipboard = null;
});

describe("copySelected", () => {
  it("stashes the selected clip and no-ops with nothing selected", () => {
    midi.selectedClipId = null;
    midi.copySelected();
    expect(midi.clipboard).toBeNull();

    midi.clips = [sourceClip()];
    midi.selectedClipId = "src";
    midi.copySelected();
    expect(midi.clipboard?.id).toBe("src");
  });
});

describe("pasteAtPlayhead", () => {
  it("creates an independent clip at the playhead on the copied clip's track", async () => {
    midi.clips = [sourceClip()];
    midi.selectedClipId = "src";
    midi.copySelected();

    const pasted = await midi.pasteAtPlayhead(5000);

    expect(midiAddClip).toHaveBeenCalledWith("A", "riff", 5000, 1920);
    expect(midiSetNotes).toHaveBeenCalledWith("new-1", sourceClip().notes);
    expect(pasted?.id).toBe("new-1");
    expect(midi.clips.some((c) => c.id === "new-1")).toBe(true);
    // Source clip is untouched.
    expect(midi.clips.find((c) => c.id === "src")?.timelineStartTicks).toBe(960);
  });

  it("carries the content length forward via midi_set_clip_bounds when the source is looped", async () => {
    midi.clips = [sourceClip({ lengthTicks: 3840, contentLengthTicks: 960 })];
    midi.selectedClipId = "src";
    midi.copySelected();

    await midi.pasteAtPlayhead(5000);

    expect(midiSetClipBounds).toHaveBeenCalledWith("new-1", 5000, 3840, 960);
  });

  it("does nothing with an empty clipboard", async () => {
    const pasted = await midi.pasteAtPlayhead(5000);
    expect(pasted).toBeNull();
    expect(midiAddClip).not.toHaveBeenCalled();
  });

  it("targets the selected clip's track, not the copied clip's track, when one is selected", async () => {
    midi.clips = [sourceClip(), sourceClip({ id: "other", trackId: "B" })];
    midi.selectedClipId = "src";
    midi.copySelected();
    midi.selectedClipId = "other"; // selection moved to a clip on another track before paste

    await midi.pasteAtPlayhead(5000);

    expect(midiAddClip).toHaveBeenCalledWith("B", "riff", 5000, 1920);
  });
});

describe("duplicateSelected", () => {
  it("creates an independent copy immediately after the source clip, on its own track", async () => {
    midi.clips = [sourceClip()];
    midi.selectedClipId = "src";

    const dup = await midi.duplicateSelected();

    // immediately after: source start (960) + source length (1920) = 2880
    expect(midiAddClip).toHaveBeenCalledWith("A", "riff", 2880, 1920);
    expect(midiSetNotes).toHaveBeenCalledWith("new-1", sourceClip().notes);
    expect(dup?.id).toBe("new-1");
  });

  it("does nothing with no clip selected", async () => {
    midi.selectedClipId = null;
    const dup = await midi.duplicateSelected();
    expect(dup).toBeNull();
    expect(midiAddClip).not.toHaveBeenCalled();
  });
});
```

- [x] **Step 2: Run tests to verify they fail**

Run: `timeout 300 npx vitest run src/lib/state/midi-stamp.test.ts 2>&1 | tail -80`
Expected: FAIL — `midi.clipboard`/`copySelected`/`pasteAtPlayhead`/`duplicateSelected`
don't exist yet.

- [x] **Step 3: Implement** in `midi.svelte.ts`

Add state (near `selectedClipId`):

```typescript
  /** Clipboard for Ctrl+C/V/D clip stamping (spec §6) — a snapshot, not a
   * live reference, so later edits to the source clip don't leak into a
   * clip already copied. */
  clipboard = $state<MidiClip | null>(null);
```

Add methods (near `select`):

```typescript
  copySelected() {
    const c = this.selectedClip;
    this.clipboard = c ? { ...c, notes: c.notes.map((n) => ({ ...n })) } : null;
  }

  /** Independent copy at `timelineStartTicks` on the target track — the
   * SELECTED clip's track when one is selected, else the copied clip's own
   * original track. Fresh backend id + fresh note ids (midi_set_notes's
   * keep-rule mints fresh ids whenever the target clip starts with no
   * notes, which a brand-new clip always does — see progress.md). */
  async pasteAtPlayhead(timelineStartTicks: number): Promise<MidiClip | null> {
    const src = this.clipboard;
    if (!src) return null;
    const targetTrackId = this.selectedClip?.trackId ?? src.trackId;
    return this.stamp(src, targetTrackId, Math.max(0, Math.round(timelineStartTicks)));
  }

  /** Independent copy immediately after the currently selected clip, on the
   * same track. */
  async duplicateSelected(): Promise<MidiClip | null> {
    const src = this.selectedClip;
    if (!src) return null;
    return this.stamp(src, src.trackId, src.timelineStartTicks + src.lengthTicks);
  }

  private async stamp(
    src: MidiClip,
    trackId: string,
    timelineStartTicks: number,
  ): Promise<MidiClip | null> {
    try {
      let created = await backend.midiAddClip(trackId, src.name, timelineStartTicks, src.lengthTicks);
      this.upsert(created);
      if (src.contentLengthTicks !== undefined) {
        created = await backend.midiSetClipBounds(
          created.id,
          timelineStartTicks,
          src.lengthTicks,
          src.contentLengthTicks,
        );
        this.upsert(created);
      }
      if (src.notes.length > 0) {
        const copiedNotes = src.notes.map((n) => ({ ...n }));
        created = await backend.midiSetNotes(created.id, copiedNotes);
        this.upsert(created);
      }
      return created;
    } catch (err) {
      console.error("[aura] clip stamp failed:", err);
      return null;
    }
  }
```

- [x] **Step 4: Run tests to verify they pass**

Run: `timeout 300 npx vitest run src/lib/state/midi-stamp.test.ts 2>&1 | tail -80`
Expected: all tests PASS.

- [x] **Step 5: Run the full frontend suite**

Run: `timeout 300 npx vitest run 2>&1 | tail -40`
Expected: 89 passed (81 + 8 new), 0 failed.

- [x] **Step 6: Commit**

```bash
git add src/lib/state/midi-stamp.test.ts src/lib/state/midi.svelte.ts
git commit -m "$(cat <<'EOF'
feat(midi): clip stamping store logic (copy/paste/duplicate)

midi.copySelected/pasteAtPlayhead/duplicateSelected build independent
clips via the existing midi_add_clip + midi_set_notes (+ midi_set_clip_bounds
when the source is looped) — no new backend surface needed, since
midi_set_notes's keep-rule already mints fresh note ids whenever the
target clip starts empty (every freshly created clip). Paste targets the
selected clip's track when one is selected, else the copied clip's own
track (ruling logged in progress.md — no "selected track" concept exists
in this codebase yet).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Task 10: Ctrl+C/V/D keyboard wiring

**Files:**
- Modify: `src/App.svelte`

**Interfaces:**
- Consumes: `midi.copySelected`, `midi.pasteAtPlayhead`,
  `midi.duplicateSelected` (Task 9), `playhead()` (existing local helper in
  `App.svelte`).

No test infra for `App.svelte` (same as Task 6 Step 6's edge-jump fix) —
verified by careful reading + `svelte-check` + the full `vitest run`
regression gate.

- [x] **Step 1: Add the shortcuts**

In `src/App.svelte`'s `onKeydown`, after the existing `,`/`.` edge-jump
block (so it inherits the same "not in an input, not in the piano roll"
guard already in effect at that point in the function) and before the
`+`/`-`/zoom handling, add:

```typescript
    } else if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "c") {
      e.preventDefault();
      midi.copySelected();
    } else if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "v") {
      e.preventDefault();
      void midi.pasteAtPlayhead(midi.samplesToTicks(playhead()));
    } else if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "d") {
      e.preventDefault();
      void midi.duplicateSelected();
```

(Splice these `else if` branches into the existing `if (e.key === "," ||
e.key === ".") { ... } else if (e.key === "+" ...` chain — read the current
chain in the file first so the braces line up; do not create a second
top-level `if`.)

- [x] **Step 2: Typecheck**

Run: `timeout 300 npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -80`
Expected: 0 errors.

- [x] **Step 3: Run the full frontend suite (regression only)**

Run: `timeout 300 npx vitest run 2>&1 | tail -40`
Expected: 89 passed, 0 failed.

- [x] **Step 4: Run the full backend suite once more (final gate)**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml 2>&1 | tail -30`
Expected: 358 passed, 0 failed (unchanged since Task 5 — Tasks 6-10 are
frontend-only).

- [x] **Step 5: Commit**

```bash
git add src/App.svelte
git commit -m "$(cat <<'EOF'
feat(midi): wire Ctrl+C/V/D clip stamping shortcuts

Copy the selected timeline clip, paste at the playhead (selected track),
duplicate immediately after the source — same guard scope as the existing
edge-jump shortcuts (not while typing, not while the piano roll owns
keyboard focus). Pure orchestration onto Task 9's store methods.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Task 11: Piano roll content-length bounds

**Files:**
- Modify: `src/lib/components/pianoroll/PianoRoll.svelte`

**Interfaces:**
- Consumes: `midi.effectiveContentLengthTicks(clip)` (Task 6).

Spec §6 lists this as a first-class part of "Piano roll: all bounds read
*content* length" — promoted here from the plan's initial self-review note
(originally flagged as an optional follow-up) once Task 6 landed the
accessor it needs. Same no-component-test-infra situation as Tasks 7/8/10 —
verified by reading + `svelte-check` + the full `vitest run` regression gate.

- [x] **Step 1: Read the five call sites first**

Run: `grep -n "clip.lengthTicks\|c.lengthTicks" src/lib/components/pianoroll/PianoRoll.svelte`

Confirm exactly which lines are: (a) initial zoom fit, (b) note-creation
tick limit, (c) marquee-selection clamp, (d) end-of-clip shading, (e) the
playhead-overlay bound(s). Read ~15 lines of context around each before
editing — `PianoRoll.svelte` is a large file and line numbers drift; do not
edit by line number alone.

- [x] **Step 2: Swap placement length for content length at each site**

For (a) zoom fit, (b) note-creation limit, (c) marquee clamp, (d) end
shading: replace every direct read of `clip.lengthTicks` (or `c.lengthTicks`
in the same local scope) with `midi.effectiveContentLengthTicks(clip)` (or
`(c)` for whichever local variable name is in scope at that call site).
These four are a straightforward substitution — the piano roll edits and
displays exactly one repetition of content, never the stretched placement.

For (e) the playhead overlay: this one is NOT a straight substitution — per
spec §6, "the playhead overlay wraps modulo content length while the
timeline playhead runs through the repeats." Find the block computing
`posTicks` against `c.lengthTicks` (the two call sites `grep -n
"posTicks.*lengthTicks\|c.lengthTicks" src/lib/components/pianoroll/PianoRoll.svelte`
turns up). Change the bound check to test against the PLACEMENT length
(`c.lengthTicks` — the overlay should disappear once the transport plays
past the placement end, not just past one content repeat) but take the
position modulo the CONTENT length before using it to position the
overlay within the grid:

```typescript
      const contentTicks = midi.effectiveContentLengthTicks(c);
      const posInContent = ((posTicks % contentTicks) + contentTicks) % contentTicks;
```

(the double-modulo guards against a negative `posTicks` producing a
negative remainder in JS) and use `posInContent` everywhere the block
previously used `posTicks` to compute an x-coordinate or compare against
note ticks, while the `posTicks >= 0 && posTicks <= c.lengthTicks` visibility
gate keeps reading the un-moduloed `posTicks` against the PLACEMENT length.

- [x] **Step 3: Typecheck**

Run: `timeout 300 npx svelte-check --tsconfig ./tsconfig.json 2>&1 | tail -80`
Expected: 0 errors.

- [x] **Step 4: Run the full frontend suite (regression only)**

Run: `timeout 300 npx vitest run 2>&1 | tail -40`
Expected: 89 passed, 0 failed.

- [x] **Step 5: Commit**

```bash
git add src/lib/components/pianoroll/PianoRoll.svelte
git commit -m "$(cat <<'EOF'
feat(midi): piano roll bounds read content length, not placement length

Zoom fit, note-creation tick limit, marquee-selection clamp, and end-of-clip
shading all now bound against effectiveContentLengthTicks() instead of the
(possibly looped/stretched) placement length — the piano roll edits exactly
one repetition of content (spec §6). The playhead overlay is the one
exception: it stays visible for the full PLACEMENT duration but wraps the
position modulo content length, so it re-enters at the left edge on every
repeat instead of running off the right side of the grid.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push
```

---

## Self-Review

**Spec coverage:**
- §2 ADR 0004 alignment: Task 1 (field placement, doc comments cite it).
- §3 data model: Task 1 + Task 3 (persistence, schema).
- §4 playback (repeat loop, shared seam, offline export): Task 2.
- §4 AMT validates against content: Task 4.
- §5 `midi_set_clip_bounds`: Task 5 (revised during execution to a pure
  `apply_clip_bounds` core + thin command wrapper, matching this file's
  existing test convention — no `tauri::State` harness exists anywhere in
  the codebase; logged in progress.md).
- §6 edge-drag gesture: Task 7 Step 1. Rendering (mini preview + separators,
  note-flash modulo): Task 7 Steps 2-3. demo.ts modulo: Task 8. Piano roll
  content-length bounds (zoom fit, note-creation limit, marquee clamp, end
  shading, playhead-overlay modulo): Task 11.
- Stamping (Ctrl+C/V/D, independent clips via existing commands): Tasks 9-10.
- §7 testing: every backend task and Task 6/9 include the TDD tests the
  spec names; Tasks 7/8/10/11 explicitly call out the "no component-test
  infra, verify live" exception the spec itself grants.
- §8 out of scope: not touched (pattern instancing, left-edge crop,
  repeat-aware SMF export, full undo/redo for stamping, audio-clip looping).

**Placeholder scan:** every step has real code, no "TBD"/"similar to Task
N". Task 5's original pass had a placeholder-shaped gap (an assumed test
harness that doesn't exist) — caught and fixed before implementation, see
the note above and progress.md.

**Type consistency:** `effectiveContentLengthTicks` (Task 6) used
identically in Tasks 7, 8, 9, 11. `midi.setClipBounds` /
`midi.commitBounds` (Task 6) used identically in Task 7. `contentLengthTicks`
(TS) / `content_length_ticks` (Rust) used consistently throughout.
`apply_clip_bounds` (Task 5) is the one Rust helper name introduced after
the initial draft — used only within Task 5, no cross-task references to
keep in sync.
