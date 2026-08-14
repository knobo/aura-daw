# Plan F — History Storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking. Execution mode for this run is
> decided by the owner at execution time and recorded in the execution note
> at the bottom of this file.

**Goal:** Give the op log its storage substrate — a copy-on-write,
structurally-shared session snapshot store with version-graph retention,
replay-only nodes at the measured 64 KB threshold, placement-offset routing,
and a janitor thread for off-RT eviction — lift the snapshot-rebuild
deferral standing since the Plan A handoff (`engine::rebuild` reads an
immutable snapshot instead of holding the session lock), give the journal
its first reader (cold tail replay), and first fix the two live data-loss
bugs (I-1, I-7) and the blocking/undo-migration bugs (I-6 + C-1 residual)
the Plan E whole-branch review held for this track.

**Architecture:** The live document keeps its landed shapes (`Store`,
`MidiStore`, `AutomationDoc`, `PluginDoc` behind the one `Session` lock —
scope ruling F-1 below explains why). What changes is that every successful
`transact` now also *captures* an immutable `SessionSnapshot` — an
Arc-structurally-shared image where only the collections a batch touched are
re-allocated (per-clip granularity for MIDI content, matching RESULTS.md's
per-pattern recommendation) — and *publishes* it inside the same lock.
`engine::rebuild` consumes the published snapshot instead of holding the
session lock across the graph build (the payoff). `HistoryLog` grows a
`VersionGraph` behind the same handle: one node per non-transient commit,
materialized (snapshot + byte charge) or replay-only (ops + inverses) at the
measured 64 KB own-created-bytes threshold, bounded by a bytes ceiling and a
steps floor, with evicted loads dropped on a janitor thread. The snapshot
primitive also unlocks panic rollback in `transact` (closing L-5) and cold
journal replay (the reader L-4's sort discipline was written for). Today's
`History` (undo/redo stacks, 200 entries, 350 ms merge) remains the
user-visible exposure layer, unchanged in depth.

**Tech Stack:** Rust (src-tauri crate: serde, serde_json, parking_lot,
std::sync::Arc, std::sync::mpsc), TypeScript/Svelte 5 frontend (`src/lib/`)
touched only for one additive command binding, vitest.

**Spec:** `docs/CORE-REDESIGN-ROUND-2.md` §6 (storage: COW tree,
replay-only nodes, caps, janitor) + §2.3 (retention IS the version graph) +
§5 (placement offsets); ADR 0005 (the decision this plan implements); ADR
0003 (op log is a persisted format; graph rebuilds read an immutable
snapshot, never the lock); measured evidence `benches/bulkbench/RESULTS.md`
(64 KB replay-only threshold, per-op-class caps, placement-offset lever,
janitor timing); scope definition `next-prompt.md` §3 "Track A — Plan F";
inherited rulings `docs/PHASE4-PLAN.md` "Plan E handoff" (esp. "Standing
carry-forwards for Plan F+"); residuals/limitations
`docs/SIDE-CHANNEL-INVENTORY.md` (R-3, L-2, L-4, L-5); held review findings
`.superpowers/sdd/2026-08-14-plan-e-side-channel-totality/final-review-report.md`
(I-1, I-6, I-7; M-1, M-2, M-4, M-5) and `fix-wave-report.md` (the C-1
entry-migration residual) in the same directory.

## Global Constraints

Every task's requirements implicitly include this section. The first block
carries the standing constraints from `next-prompt.md` §2, in force for all
tracks; the second block is this plan's own additions.

- **The op log is ON.** `journal.ndjson` is a **persisted format**;
  `OP_FORMAT_VERSION` is **2** (base64 state blobs since PR #18). Additive
  `#[serde(default)]` fields on an op or on `TxMeta` stay non-breaking;
  anything else (renaming a field, changing a variant's shape, removing a
  path) needs a version bump AND a reader that understands both shapes. The
  v1→v2 bump shipped WITHOUT a dual-shape reader on purpose, because the
  journal was write-only — **that freedom ends at this plan's Task 9**, the
  moment the journal gets a reader; after it lands, a change of this kind
  costs a migration (see ruling F-5).
- **Thin renderer** (ADR 0006): no new authoritative state, business logic,
  or time math lands frontend-side. This plan's only frontend touch is the
  additive `midi_set_clip_placement` binding in Task 12.
- **Frozen command/event names stay frozen**; bodies become wrappers; new
  commands are additive. Additive in this plan: `midi_set_clip_placement`
  (Task 12). `undo`/`redo` keep their names and payload shape — Task 4 only
  changes their Rust-side execution to `async` + `spawn_blocking`, which is
  invisible on the wire.
- **`transact` closures must not panic — validate before mutating.** This
  discipline HOLDS THROUGH THE WHOLE PLAN. Task 8 adds snapshot-restore
  panic *containment* as a safety net (closing L-5), which does NOT license
  panicking closures — see ruling F-3.
- **Prepare-outside/commit-inside** for I/O; no blocking engine round-trips
  inside a transaction (ADR 0003, round-2 §4.2/§4.4).
- **The M-3 redo invariant**: transient writes never touch document fields
  an entry's `ops` can address (only `ObjectRef::Transport`, unless the
  batch is a mid-gesture fold). `debug_assert_transient_invariant` enforces
  it in debug builds; nothing in this plan may weaken or route around it.
- **Gesture lock order is gesture-before-session, everywhere.** The full
  landed order is `gesture -> session -> epoch -> history / journal`
  (history.rs module doc). New leaf locks added by this plan (the published-
  snapshot slot in Task 5, the version-graph mutex in Task 7) slot in
  leaf-most and are documented at their declaration, with the order comment
  updated in the same commit.
- **Foreground timeout-guarded test runs only:**
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml` and
  `timeout 300 npx vitest run`. Never backgrounded.
- **Dated test-count convention:** any task that changes test counts
  updates README.md + CONTRIBUTING.md in the same commit, with the date.
- **Corrections to docs are marked, never silent** (ADR 0007).
- **Baseline at branch base** (`origin/main` = `3340aa8`, verified by
  running both suites while authoring this plan, 2026-08-14): **527 backend
  + 206 frontend, all green.** KNOWN DISCREPANCY, decided here so no
  implementer stalls on it: README.md:389 and CONTRIBUTING.md:62 still say
  "506 tests (counted 2026-08-14)" — PR #17 (midi input slice 1) added 21
  backend tests without updating the dated counts. Task 1, the first
  count-changing task, corrects the count to its own new total and notes
  the PR #17 gap in the same commit as a marked correction (ADR 0007). If
  your first run does not say 527/206, stop and find out what changed
  underneath you before proceeding.
- **Branch/worktree rules** (verbatim from next-prompt.md): work on branch
  `plan-f-history` from `origin/main`; merge `origin/main` in at task
  boundaries when it advances; never bare `git stash`.

## Scope rulings (decided now, marked per ADR 0007, so no task stalls)

- **F-1 — The COW store lands as the snapshot/history substrate at
  clip/row granularity; the live document keeps its landed `Vec` shapes;
  the within-clip summarising B-tree is deferred, with its trigger named.**
  This is the plan's largest reconciliation of spec vs landed code, so it
  is argued, not asserted. ADR 0005's literal decision ("the in-memory
  session structure is a summarising COW B-tree") was written against the
  event-model document (10⁶-event patterns, 20-byte records, point-edit
  gestures — "sorted insert is what a piano roll *is*"). The landed
  document disagrees on all three counts: (a) `MidiClip.notes` is a
  `Vec<MidiNote>` woven through 74 call sites (persist/AMEV, midifile
  import/export, events, hum, playback, clap_host, session ops) plus the
  IPC wire and the v3 file format; (b) the landed op vocabulary has **no
  note-delta op** — every note edit is `Op::MidiSetNotes`, a whole-clip
  value replacement, and a whole-clip replacement rewrites every tree leaf,
  so within-clip structural sharing has NO producer (RESULTS.md finding 2
  says exactly this: whole-clip ops at ~104–131 KB ARE the replay-only
  class); (c) migrating the live store's note representation is a
  plan-sized rewrite that would starve this plan's actual deliverables.
  What this plan therefore builds is the version store at **per-pattern
  granularity realized as per-clip `Arc<MidiClip>` sharing** — for the
  landed whole-value vocabulary this is byte-equivalent to per-pattern
  trees (a changed clip charges its whole content, an unchanged clip
  charges one pointer), and every measured cap and threshold from
  RESULTS.md applies unchanged (point ≤ 8 KB, bulk ≤ 256 KB, replay-only
  at 64 KB, eviction defends the budget). The summarising COW B-tree
  (reference implementation: `benches/bulkbench/src/tree.rs`, generic,
  measured) lands **in the round that adds a note-delta op** — the moment
  a point edit stops being a whole-clip replace, per-clip Arc granularity
  starts over-charging point edits and the tree earns its keep; that round
  migrates `MidiClip.notes` and inherits this ruling as its trigger.
  Recorded as a MARKED deviation from ADR 0005's letter in Task 13's doc
  sweep; the orchestrator may veto by requiring the tree now, at the cost
  of roughly doubling this plan.
- **F-2 — User-visible undo depth does not change.** `UNDO_STACK_LIMIT`
  stays 200; the `History` stacks remain the exposure layer and remain
  self-sufficient (entries carry their own ops/inverses, so version-graph
  eviction can never shorten undo). The version graph is bounded
  independently: bytes ceiling `VER_BUDGET_BYTES` = 512 MiB (round-2 §6's
  defended budget), steps floor `VER_STEPS_FLOOR` = 200 (aligned with the
  undo depth so browsing never covers less than undo can reach).
- **F-3 — Panic rollback is IN SCOPE (Task 8), as containment, not
  license.** The old ground rule said panic rollback becomes possible in
  Plan F; the snapshot primitive makes it nearly free, and L-5 (a
  panicking closure diverges log from document PERMANENTLY and silently)
  is the strongest standing argument. Decision: `Session::transact` wraps
  the closure in `catch_unwind`; on panic it restores the live document
  from the pre-transaction published snapshot and returns
  `Err("transaction panicked: …")` — the app stays alive, the document
  and the log stay consistent, the panic is logged at error level. The
  "validate before mutating" discipline REMAINS BINDING (a rollback is a
  crash-consistency net, not an error-handling strategy); L-5 is updated
  to CLOSED in the inventory as a marked correction (Task 13).
- **F-4 — L-4's structural fix is ordered consumption, not a
  commit-sequence lock.** The inventory offered two shapes: serialize the
  record step under a commit-sequence lock, or buffer out-of-order revs in
  `record_commit`. Both are rejected, with reasons: a commit-sequence
  ticket taken under the transact lock would make a fast command's
  `record_commit` WAIT for a slower concurrent committer's effect phase —
  which contains blocking plugin round-trips and disk I/O — coupling
  command latency to plugin I/O and adding a wedge risk exactly where C-1's
  guard just removed one; and buffering cannot know when a missing rev
  will never arrive, because transient commits consume revs without
  producing journal lines, so rev gaps are ordinary. Instead: (a) the
  journal READER sorts by `(epoch, rev)` — Task 9, exactly what L-4's text
  prescribes and every line already carries; (b) the UNDO STACK becomes
  rev-ordered — `HistoryEntry` gains a `rev` field and `History::record`
  inserts in rev order (Task 11), so a late-arriving older batch can no
  longer sit ABOVE a newer one and be popped first; (c) journal FILE order
  remains unordered and is DOCUMENTED as a format rule ("line order is not
  rev order; consumers must sort"), inventory L-4 updated to "closed by
  reader discipline + ordered undo stack" as a marked correction (Task
  13).
- **F-5 — The journal reader requires `v == 2` on batch lines; v1 lines
  are skipped with a warn.** PR #18's v1→v2 bump deliberately shipped
  without a dual-shape reader because the entire v1 population is "logs no
  code will ever parse" (fix-wave report §4); this reader honors that
  contract rather than resurrecting v1. A skipped v1 line is counted and
  reported, never silent. From Task 9 forward the standing rule activates
  in full: any non-additive wire change needs a bump AND a reader for both
  shapes.
- **F-6 — I-1 is fixed as the review's option (b)** —
  `save_project_as_epoch` writes the plugin and automation snapshots into
  the new dir alongside midi. Option (a) (epoch functions flush the
  outgoing document's pending persists before the swap, which is what the
  epoch guard's comment literally promises) is deferred: it would put the
  full persist ladder synchronously inside every epoch function, and Task
  3's M-2 fix (Ctrl+S flushes failed auto-persists) gives the user a
  recovery path for the same window. The residual — `open_project_epoch`
  still writes nothing for the outgoing document, so an in-flight persist
  for the project being CLOSED is dropped with a warn — stays recorded;
  Task 13 updates the guard's justification comment and the inventory so
  the comment stops over-promising (marked correction).
- **F-7 — Undo/redo AUTO-CLOSE an open gesture (M-4) rather than refuse.**
  Mirrors `gesture_begin`'s own stale-gesture discipline (it auto-closes
  too), keeps Ctrl+Z from appearing dead mid-drag, and commits the
  gesture's accumulated fold BEFORE the undo pops — so the undo undoes the
  fold, which is what the user sees on screen. Lock order is respected by
  construction: the gesture close completes (gesture → session) before the
  undo's own commit begins.
- **F-8 — Cold replay ships as save-mark tail recovery: detection + a
  primitive + fidelity tests; no auto-apply, no recovery UI.** A journal
  segment's base state is not journaled (epoch boundaries swap in a
  document loaded from disk), so the general replay identity is:
  document(now) = on-disk snapshot ⊕ journal lines after the last `"save"`
  mark in the current epoch. Task 9 builds exactly that reader,
  `open_project_epoch` logs a warn when an unsaved tail exists, and
  auto-recovery UX is a later round's call (the journal has no fsync — a
  torn tail is legal and must be skipped, not trusted).
- **F-9 — L-2 is benign for tail replay, and the test asserts it
  strongly.** `Op::MidiSetNotes` `noteId: 0` entries are mint sentinels
  that re-mint on replay — but minting is DETERMINISTIC from the clip's
  `next_note_id` watermark, and the watermark is part of the on-disk
  snapshot the tail replays from. Replaying the same op sequence from the
  same base therefore reproduces the same minted ids exactly. Task 9's
  fidelity test asserts byte-identical documents WITHOUT id normalization
  (stronger than the Figma oracle needs); if a future op breaks this, the
  test failure is the signal that L-2 stopped being benign. Inventory L-2
  gains this argument as a marked addition (Task 13).
- **F-10 — Replay-only nodes store the batch's ops + inverses verbatim;
  §6's non-deterministic-id exclusion is satisfied vacuously and
  asserted.** Every landed op is absolute-valued and every structural op
  carries its minted ids in its own payload (`ClipAdd`/`MidiClipAdd`/
  `PluginAdd` carry whole rows), so no landed op re-mints object ids on
  replay — there is nothing to exclude. Task 7 adds a test asserting the
  property over the whole vocabulary so a future op that violates it fails
  loudly. Random ops (humanize-class) do not exist yet; when they land,
  §6's "must be seeded" constraint binds them (recorded, not implemented).
- **F-11 — The version graph is linear-by-rev within an epoch.** Undo and
  redo are FORWARD commits (`HistoryMode::Replay`, Plan E's design), so
  rev history never branches; "version graph" degenerates to a rev-ordered
  chain rooted at each epoch boundary. Branch-shaped history (true DAG,
  collaboration) is a later round; the node/edge naming is kept so that
  round extends rather than renames.
- **F-12 — R-3 closes fully: the demo's Zyn bootstrap becomes ops inside
  the ONE seed transaction, including state blobs.** `Op::PluginAdd` per
  instance plus `Op::PluginSetState` carrying the freshly-loaded patch
  state, applied in `seed_demo_project`'s existing single commit. The
  direct row pushes, their rollback, and the manual
  `save_snapshot_into_project(..., with_host_state: true)` block are all
  deleted — persistence rides `PersistEffect` like every other plugin op,
  the demo becomes one undoable step including its instruments, and a cold
  replay reconstructs the demo's plugins from the journal (the reason R-3
  was assigned to this plan).

### Non-goals (each with its one-line reason)

- **No live-document B-tree migration** — ruling F-1; trigger recorded.
- **No history-browser UI or `history_stats`-style command** — the version
  graph's product surface belongs to the round that builds time-travel UX;
  this plan lands substrate + tests (YAGNI on speculative exposure).
- **No fsync/group-commit durability work** on the journal — recorded in
  `JournalWriter`'s doc as accepted; tail recovery (Task 9) is this plan's
  answer to the same risk class.
- **No auto-apply of a detected journal tail** — ruling F-8.
- **No commit-sequence lock** — ruling F-4.
- **M-1/M-2 minors are IN scope** (Task 3 — they fall out of the
  save-path work); **M-4 and M-7 are IN scope** (Tasks 4/11 — same code).
  NUMBERING NOTE: the final review's M-7 (VecDeque for the undo stack) is
  called "M-5" in the Track A brief; this plan uses the review report's
  numbering, under which M-5 is the Gate E precision sentence — carried as
  a free marked correction in Task 13 since it edits PHASE4-PLAN anyway.
  NOT in scope, with reasons: **M-8** (Figma oracle's
  undocumented derived-field omissions — doc-only, no Plan F code touches
  that oracle's exclusion list); **frontend M-3** (undo re-pull misses
  automation/plugin panels — Track D owns the panels); **I-3/M-6**
  (`execute_host_forward` writeback — Track D's plugin-host
  neighbourhood, per next-prompt); **I-8** (per-knob persist frequency —
  Track D / gesture path, per next-prompt).
- **No MCP tool additions** — roster frozen (ARCHITECTURE §12.2).
- **No note-delta / transpose GESTURE UI** — Task 12 lands the placement
  fields, op paths, playback application, and the additive command; the
  gesture that drives them is UI work for a later slice (thin renderer).

## File structure (locked in before tasks)

New files:

- `src-tauri/src/control/snapshot.rs` — `SessionSnapshot`, `MidiSnapshot`,
  `ChangeSet`, capture/publish/charge accounting, `Session::from_snapshot`
  materialization. One responsibility: the immutable document image.
- `src-tauri/src/control/vergraph.rs` — `VersionGraph`, `VersionNode`,
  classification constants, eviction, `Janitor`. One responsibility:
  retention.
- `src-tauri/src/control/replay.rs` — journal reader: NDJSON parse,
  `(epoch, rev)` sort, save-mark tail extraction, tail replay onto a
  scratch `Session`. One responsibility: reading the persisted format.
- `src-tauri/tests/journal_replay.rs` — end-to-end cold-replay fidelity.
- `src-tauri/tests/snapshot_store.rs` — snapshot/live equivalence sweep +
  version-graph retention integration.

Modified (task-by-task, never speculatively):

- `src-tauri/src/plugins/state.rs` (Task 1), `src-tauri/src/control/mod.rs`
  (Tasks 1–4, 5, 10), `src-tauri/src/control/session.rs` (Tasks 5, 8, 12),
  `src-tauri/src/control/history.rs` (Tasks 4, 7, 11),
  `src-tauri/src/audio/engine.rs` (Task 6 ONLY — see the execution note on
  cross-track sequencing), `src-tauri/src/audio/project.rs` (Task 5,
  republish at the ensure-project swap), `src-tauri/src/midi/playback.rs`
  (Tasks 6, 12), `src-tauri/src/plugins/automation.rs` (Task 5 republish),
  `src-tauri/src/midi/types.rs` + `src-tauri/src/control/op.rs` (Task 12),
  `src/lib/tauri.ts` (Task 12), docs (Task 13).

---

### Task 1: I-7 — a new/opened project must not inherit the previous project's plugin rows

The held review finding I-7, verbatim reachability: `create_project_at`
clears tracks/clips/transport/midi and relies on
`plugins::state::adopt_open_project` to reset the plugin half — but a
fresh `project.json` has no `plugins` key, so `read_restored_rows`
(`src-tauri/src/plugins/state.rs:494`) returns `Ok(None)` and
`adopt_open_project` (`:743`) returns BEFORE `install_restored_rows` (the
only thing that clears `session.plugins`). The previous project's
`instances`/`params`/`pending_state` survive into the new document and are
written into the new project's `project.json` by the next plugin persist —
cross-project file contamination. Same path on `open_project_epoch` for any
project whose `project.json` predates plugin persistence. Automation
already does this right (`automation::adopt_open_project`,
`src-tauri/src/plugins/automation.rs:405`: `Ok(None) => clear`) — this task
makes plugins match, with the same justification automation's doc states:
an epoch adopt runs only at the exact moment a project is (re)adopted, so
"no `plugins` field" genuinely means "this (now open) project has none".

**Files:**
- Modify: `src-tauri/src/plugins/state.rs` (`adopt_open_project`, ~:743;
  the doc comments on `read_restored_rows` ~:489 and `restore_into_session`
  ~:598 which currently assert "the caller must NOT clear then")
- Test: `src-tauri/src/plugins/state.rs` `#[cfg(test)]` module (pattern:
  `adopt_open_project_end_to_end_with_a_registered_session_does_not_deadlock`,
  ~:1673) + README.md/CONTRIBUTING.md count refresh (this is the plan's
  first count-changing commit — see Global Constraints for the PR #17
  correction it also carries)

**Interfaces:**
- Consumes: `read_restored_rows(dir) -> Result<Option<Vec<RestoredRow>>, String>`
  (unchanged — `Ok(None)` still means "no `plugins` field", `Err` still
  means "cannot read"); `install_restored_rows(&mut Session, Vec<RestoredRow>)
  -> (usize, Vec<(String, String)>)` (unchanged).
- Produces: `adopt_open_project(dir)` with three distinct arms:
  `Ok(Some(rows))` → install (as today); `Ok(None)` → install an EMPTY row
  set (which clears `instances`/`params`/`pending_state` and returns
  `prior_hosted` for the caller's host unregistration, exactly like a
  restore of zero rows) **and clears `plugins.dirty_state`** (a stale dirty
  id from the outgoing project must not mark a fresh document dirty —
  `install_restored_rows` deliberately leaves `dirty_state` alone for the
  restore path, so the clear happens in the `Ok(None)`/`Ok(Some)` epoch
  arms of `adopt_open_project` itself, both under the same short lock as
  the install); `Err(e)` → warn and leave the document alone (unchanged —
  an unreadable file is not evidence the project has no plugins).

- [ ] **Step 1: Write the failing test** (state.rs tests; use the existing
  registered-session fixture pattern from the `:1673` test — a local
  `Session` behind `Arc<Mutex<_>>`, registered via the same helper that
  test uses, plus a temp project dir):

```rust
#[test]
fn adopting_a_project_without_a_plugins_field_clears_the_previous_sessions_rows() {
    // Previous session left a row + params + a pending blob + a dirty id.
    let (session, _guard) = register_test_session(); // same fixture the :1673 test builds
    {
        let mut s = session.lock();
        s.plugins.instances.push(test_row("inst-old"));
        s.plugins.params.insert("inst-old".into(), vec![]);
        s.plugins.pending_state.insert("inst-old".into(), vec![1, 2, 3]);
        s.plugins.dirty_state.insert("inst-old".into());
    }
    // A fresh project dir whose project.json has NO `plugins` key.
    let dir = temp_project_dir_without_plugins_field();
    adopt_open_project(&dir);
    let s = session.lock();
    assert!(s.plugins.instances.is_empty(), "rows must not leak across projects (I-7)");
    assert!(s.plugins.params.is_empty());
    assert!(s.plugins.pending_state.is_empty());
    assert!(s.plugins.dirty_state.is_empty(), "stale dirty ids must not survive the swap");
}

#[test]
fn an_unreadable_plugins_file_leaves_the_document_alone() {
    let (session, _guard) = register_test_session();
    session.lock().plugins.instances.push(test_row("inst-old"));
    let dir = temp_project_dir_with_corrupt_project_json(); // invalid JSON → read error
    adopt_open_project(&dir);
    assert_eq!(session.lock().plugins.instances.len(), 1,
        "Err is not evidence of an empty project — leave alone, as today");
}
```

(`test_row(id)` builds a minimal `PluginInstanceInfo` — copy the field
shape from `read_restored_rows`'s own construction at `:505`. If no
`register_test_session` helper exists, extract the registration lines the
`:1673` test already contains into one; do not invent a parallel fixture.)

- [ ] **Step 2: Run to verify the first test fails** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml adopting_a_project_without_a_plugins_field -- --nocapture`
  Expected: FAIL — instances survive (today's early return).

- [ ] **Step 3: Implement.** In `adopt_open_project`, replace the early
  return on `Ok(None)` with an install of `Vec::new()` through the SAME
  locked-install + host-unregister sequence the `Ok(Some)` arm uses, and
  clear `dirty_state` in both `Ok` arms under the same lock as the
  install. Update the `read_restored_rows` / `restore_into_session` doc
  comments: the "must NOT clear" clause now applies only to
  `restore_into_session`'s non-epoch callers, and the epoch-adopt clear is
  I-7's fix — cite I-7 in the comment so the next reader finds the
  reasoning (`restore_into_session` itself keeps its `(0, vec![])`
  behavior: it is a restore primitive, not an epoch function).

- [ ] **Step 4: Full backend suite** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  Expected: 527 + 2 new = 529, all green.

- [ ] **Step 5: Update README.md + CONTRIBUTING.md counts** to 529
  backend (dated 2026-08-14), including the marked PR #17 correction from
  Global Constraints ("506 → 527 was PR #17's unrecorded delta; corrected
  here per ADR 0007").

- [ ] **Step 6: Commit** —
  `git commit -m "fix(plugins): epoch adopt clears plugin rows when the project has none (I-7)"`

---

### Task 2: I-1 — Save-As writes plugin state and automation into the new project

`save_project_as_epoch` (`src-tauri/src/control/mod.rs:2181`) writes
`project::save` + the midi snapshot and NOTHING else: `<newdir>/plugins/*.state`
and the `plugins[]` array are absent, the new dir gets no automation
chunks, and on the next cold open `automation::adopt_open_project` sees no
lanes and CLEARS them (and, after Task 1, plugins clear the same way) — a
Save-As silently destroys plugin state and automation. Fix per ruling F-6:
snapshot both under the same short lock the midi snapshot already uses,
write both after the lock drops, with the same failure semantics
(`log::warn!`, never a failed epoch).

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (`save_project_as_epoch`, ~:2181)
- Test: `src-tauri/src/control/mod.rs` `#[cfg(test)]` module (the tauri-free
  `ControlPlane` fixture the epoch tests already use — grep
  `save_project_as_epoch` in the test module for the existing shape) +
  count refresh

**Interfaces:**
- Consumes: `Session::plugin_snapshot() -> PluginDoc` (session.rs:126);
  `s.automation.lanes.clone() -> Vec<AutomationLane>`;
  `plugins::state::save_snapshot_into_project(&dir, &doc, /*with_host_state*/ false)
  -> Result<Vec<String>, String>`;
  `plugins::automation::save_into_project(&dir, &lanes) -> Result<(), String>`
  (both already exactly what `execute_persist` calls — mirror its calls,
  including `with_host_state: false` and its stated reason: pending_state
  is kept current by the op arms, and Save-As must not round-trip hosts).
- Produces: `save_project_as_epoch` whose snapshot block additionally
  captures `(plugin_snapshot, automation_snapshot)` inside the existing
  locked block, and whose post-lock I/O section writes both after the midi
  write, before the `project://changed` emit. Dirty-state clearing for
  written ids mirrors `execute_persist`'s post-write re-lock (`:581-596`),
  including Task 3's byte-compare guard once that task lands (Task 3 is
  ordered after this one; this task ships the plain `remove`, Task 3
  tightens BOTH call sites — noted there).

- [ ] **Step 1: Write the failing test:**

```rust
#[test]
fn save_as_carries_plugin_rows_state_blobs_and_automation_into_the_new_dir() {
    let (cp, _tmp) = test_control_plane(); // existing tauri-free fixture
    {
        let mut s = cp.session_for_test().lock(); // or the fixture's session handle
        s.plugins.instances.push(test_row("inst-1"));
        s.plugins.params.insert("inst-1".into(), vec![]);
        s.plugins.pending_state.insert("inst-1".into(), crate::plugins::state::encode_state("uid-1", &[7u8; 16]));
        s.plugins.dirty_state.insert("inst-1".into());
        s.automation.lanes.push(test_lane("track:t-1:gain"));
    }
    let dir = fresh_aura_dir(); // project::create output, like save_project_as builds
    cp.save_project_as_epoch(&dir).unwrap();
    // The new dir must contain all three durable halves.
    let pj: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    assert!(pj.get("plugins").is_some(), "plugins[] must be written by Save-As (I-1)");
    assert!(dir.join("plugins").join("inst-1.state").exists(), "state blob must land");
    assert!(pj.get("automation").is_some() || automation_chunks_exist(&dir),
        "automation must land — assert whichever on-disk form save_into_project writes \
         (check its impl: chunk files + automation[] rows)");
}
```

  (Adapt the last assertion to `automation::save_into_project`'s real
  on-disk shape — read that function first; the test must pin the same
  files `adopt_open_project` later reads, because the follow-up assertion
  is the round-trip:)

```rust
#[test]
fn save_as_then_cold_open_round_trips_plugins_and_automation() {
    // Same setup as above; after save_project_as_epoch, clear the session
    // (open a different fresh project), then open_project_epoch(saved_dir)
    // and assert instances == ["inst-1"], pending_state present, lanes == 1.
}
```

- [ ] **Step 2: Run to verify both fail** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml save_as_carries -- --nocapture`
  Expected: FAIL — no `plugins[]`, no chunks.

- [ ] **Step 3: Implement** as described under Interfaces. Order inside
  the fn: epoch_boundary (unchanged) → `project::save` → midi write
  (unchanged) → plugin write → automation write → emit. Each write's
  failure is a `log::warn!` (a Save-As that persisted project.json but
  failed a blob is degraded, not aborted — same policy as
  `execute_persist`).

- [ ] **Step 4: Full backend suite; update counts** (529 + 2 = 531).

- [ ] **Step 5: Commit** —
  `git commit -m "fix(persist): Save-As writes plugin state and automation snapshots (I-1)"`

---

### Task 3: M-2 + M-1 — Ctrl+S recovers failed auto-persists; dirty flags clear only against matching bytes

Two dirty-flag hygiene bugs in the same neighbourhood. **M-2:**
`save_project_mark` (`src-tauri/src/control/mod.rs:2241`) writes only
`project.json`; if a failed `execute_persist` left `midi.dirty = true` or
`plugins.dirty_state` non-empty, Save does not flush them — with the log
ON, the journal then claims durability the snapshot does not have (the
review's own framing). **M-1:** `execute_persist`'s dirty_state clearing
(`:581-596`) is three separate lock acquisitions — snapshot, write,
`dirty_state.remove(&id)` — so a `PluginSetState` landing between them has
its dirty flag cleared without its bytes being written (Task 9's Critical-2
class, one level up).

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (`save_project_mark` ~:2241;
  `Committer::execute_persist`'s post-write clear ~:581; Task 2's
  `save_project_as_epoch` clear site gets the same guard)
- Test: `src-tauri/src/control/mod.rs` tests + count refresh

**Interfaces:**
- Produces: (M-2) `save_project_mark` reads
  `(midi_dirty, dirty_ids_nonempty)` under its existing snapshot lock and,
  when set, also captures `midi_snapshot()` / `plugin_snapshot()` there and
  writes them post-lock exactly as `execute_persist` does (same helpers,
  same warn-not-fail policy, same dirty clearing WITH the M-1 guard below).
  The `snapshot_mark` journal record keeps its position (after
  `project::save`, before the emit) — a save mark that also flushed dirty
  stores is still one mark. (M-2) does NOT extend to automation: lanes have
  no dirty flag today (automation persists are all-or-nothing lane writes);
  recorded in the commit message as the reason automation is absent.
- Produces: (M-1) a shared helper on `Committer`:

```rust
/// Clear `dirty_state` ONLY for ids whose live pending bytes still equal
/// the bytes this persist actually wrote (M-1, whole-branch review): a
/// concurrent `PluginSetState` landing between the snapshot and this
/// re-lock must keep its dirty flag, or its bytes silently never persist.
fn clear_dirty_state_matching(&self, written: &[String], snapshot: &PluginDoc) {
    let mut s = self.session.lock();
    for id in written {
        if s.plugins.pending_state.get(id) == snapshot.pending_state.get(id) {
            s.plugins.dirty_state.remove(id);
        }
    }
}
```

  called from `execute_persist`, `save_project_mark`'s new flush, and Task
  2's Save-As write.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn save_project_mark_flushes_a_failed_midi_autopersist() {
    // Force midi.dirty = true with a real unsaved midi edit (mirror how the
    // M-5 tests set the flag), then save_project_mark(); assert midi file
    // on disk reflects the edit and midi.dirty == false.
}

#[test]
fn save_project_mark_flushes_dirty_plugin_state() {
    // dirty_state = {"inst-1"} with pending bytes; save_project_mark();
    // assert plugins/inst-1.state exists and dirty_state is empty.
}

#[test]
fn a_concurrent_set_state_between_snapshot_and_clear_stays_dirty() {
    // Unit-level: build the PluginDoc snapshot, then mutate the LIVE
    // session's pending_state for the same id to different bytes, then call
    // clear_dirty_state_matching(&["inst-1"], &snapshot); assert the id is
    // STILL in dirty_state.
}
```

- [ ] **Step 2: Run to verify all three fail** (the third fails to compile
  until the helper exists — that counts).

- [ ] **Step 3: Implement** per Interfaces.

- [ ] **Step 4: Full backend suite; update counts** (531 + 3 = 534).

- [ ] **Step 5: Commit** —
  `git commit -m "fix(persist): Ctrl+S flushes failed auto-persists; dirty flags clear only on matching bytes (M-2, M-1)"`

---

### Task 4: I-6 + C-1 residual + M-4 — undo/redo: off the UI thread, epoch-safe entry migration, gesture auto-close

One task because one restructure covers all three (the fix-wave report's
own recommendation). **I-6:** `undo`/`redo` are sync Tauri commands
(`src-tauri/src/control/mod.rs:2867`/`:2876`); sync commands run on the
main thread, and an undo of a plugin remove re-instantiates the plugin —
the seconds-long Zyn case — freezing the window. **C-1 residual:**
`ControlPlane::undo`/`redo` (`:1725`/`:1752`) pop an entry, commit, then
push it back; an epoch boundary landing between the pop and the push
RESURRECTS the entry onto the new document's stack — and worse, a boundary
landing between pop and the commit lets the commit APPLY the stale entry's
ops to the new document (the journal line is then correctly dropped by
C-1's guard, but the mutation happened). **M-4:** undo mid-gesture goes
straight to `commit_replay`, bypassing the fold.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (`ControlPlane::undo`/`redo`
  ~:1725/:1752, `commit_replay` ~:1776, tauri commands ~:2867/:2876),
  `src-tauri/src/control/history.rs` (`HistoryLog` pop/push methods),
  `src-tauri/src/control/session.rs` (`Tx::epoch` accessor)
- Test: `src-tauri/src/control/history.rs` tests,
  `src-tauri/tests/journal_and_history.rs` (the file already staging C-1's
  swap interleavings), count refresh

**Interfaces:**
- Produces (session.rs):

```rust
impl Tx<'_> {
    /// The document epoch this transaction runs under — read inside the
    /// closure, under the SAME lock `apply` writes through, so an undo can
    /// refuse to apply an entry popped under a different document (C-1
    /// residual). Read-only, like `store()`/`midi()`.
    pub fn epoch(&self) -> u64 { self.session.epoch }
}
```

- Produces (history.rs — signatures change, all callers in this task):

```rust
/// Pop the next undo step TOGETHER with the epoch it was popped under
/// (read under the epoch mutex — the same C-1 discipline as the sinks).
pub fn pop_undo(&self) -> Option<(HistoryEntry, u64)>;
pub fn pop_redo(&self) -> Option<(HistoryEntry, u64)>;
/// Migrate an entry between stacks ONLY if the document is still the one
/// it was popped under; otherwise drop it with a warn (C-1 residual: an
/// epoch boundary between pop and push must not resurrect the entry onto
/// the new document's stack). Checked and pushed under the epoch mutex.
pub fn push_redo(&self, e: HistoryEntry, popped_epoch: u64);
pub fn push_undo_unchanged(&self, e: HistoryEntry, popped_epoch: u64);
```

- Produces (mod.rs): `commit_replay(&self, meta, ops, expected_epoch: u64)
  -> Result<Committed, String>` whose closure FIRST checks
  `tx.epoch() == expected_epoch` and returns
  `Err("document changed under undo/redo")` on mismatch (transact's Err
  path rolls back nothing — nothing was applied yet). `ControlPlane::undo`
  becomes: auto-close any open gesture (`if let Some(g) = self.gesture.end()
  { self.close_gesture(g); }` — F-7, M-4), `pop_undo()` → `(entry, popped)`,
  `commit_replay(meta, entry.inverses.clone(), popped)`, on Ok
  `push_redo(entry, popped)`, on Err `push_undo_unchanged(entry, popped)`.
  `redo` mirrors. Tauri commands become
  `pub async fn undo(control: State<'_, Arc<ControlPlane>>) -> Result<HistoryStep, String>`
  using `tauri::async_runtime::spawn_blocking` exactly like
  `seed_demo_project` (~:2899); names, payloads, and `HistoryStep` are
  byte-identical on the wire.

- [ ] **Step 1: Write the failing tests:**

```rust
// history.rs unit tests:
#[test]
fn a_push_back_after_an_epoch_boundary_drops_the_entry_instead_of_resurrecting_it() {
    let log = HistoryLog::new();
    let dir = ...; log.epoch_boundary(&dir, EpochEvent::Create, 1);
    log.record_commit(...);                       // one entry on the stack
    let (e, popped) = log.pop_undo().unwrap();
    log.epoch_boundary(&dir, EpochEvent::Open, 2); // swap between pop and push
    log.push_redo(e, popped);
    assert_eq!(log.depths(), (0, 0), "the migrated entry must not survive the swap");
}

// journal_and_history.rs integration (stage like the existing C-1 test
// a_commit_whose_sink_call_lands_after_a_document_swap_...):
#[test]
fn an_undo_whose_document_swapped_after_the_pop_applies_nothing() {
    // Arrange one committed edit; reach in between pop and commit by
    // driving ControlPlane::undo on one thread while an epoch fn runs —
    // OR unit-drive: pop via the log, swap, then call commit_replay with
    // the stale popped epoch and assert Err + document unchanged.
}

#[test]
fn undo_mid_gesture_closes_the_gesture_first_and_undoes_its_fold() {
    // gesture_begin("fader"); one folded transient mix commit; undo();
    // assert the gesture is closed, exactly ONE history entry was created
    // by the close and then consumed by the undo, and the gain is back at
    // its pre-gesture baseline.
}
```

- [ ] **Step 2: Run to verify they fail** (the first fails at compile
  until the signatures change — acceptable RED).

- [ ] **Step 3: Implement** per Interfaces. Lock-order note to carry into
  the code comment: `pop_*`/`push_*` take the epoch mutex then the history
  mutex — same internal order `record_commit` already uses
  (`epoch -> history`), so no new order exists; the session lock is NEVER
  held around these calls (`commit_replay` completes before the push).

- [ ] **Step 4: Full suites** (backend count 534 + 3 = 537; frontend
  unchanged 206 — the invoke wrapper is already promise-based). Update
  counts.

- [ ] **Step 5: Commit** —
  `git commit -m "fix(history): async undo/redo off the UI thread; epoch-safe entry migration; gesture auto-close (I-6, C-1 residual, M-4)"`

---

### Task 5: `SessionSnapshot` — capture, publish, charge (the COW substrate)

The plan's keystone. Every successful `transact` captures an immutable,
Arc-structurally-shared image of the document and publishes it INSIDE the
session lock; every sanctioned non-op mutation site (epoch swaps, adopt
installs, R-1) republishes. Downstream consumers: Task 6 (rebuild), Task 7
(version nodes), Task 8 (panic rollback), Task 9 (scratch sessions).

**Files:**
- Create: `src-tauri/src/control/snapshot.rs`
- Modify: `src-tauri/src/control/session.rs` (`Session` fields, `transact`,
  `Committed`), `src-tauri/src/control/mod.rs` (epoch fns republish;
  `set_plugin_pending_state` republish; `try_seed_zyn_demo_instruments`
  republish — deleted again by Task 10), `src-tauri/src/audio/project.rs`
  (`ensure_default_project` swap republish), `src-tauri/src/plugins/state.rs`
  (`adopt_open_project`'s locked install republishes),
  `src-tauri/src/plugins/automation.rs` (`adopt_open_project` republishes)
- Test: `snapshot.rs` unit tests + create `src-tauri/tests/snapshot_store.rs`
  (the live-vs-snapshot equivalence sweep)

**Interfaces:**
- Produces (snapshot.rs):

```rust
/// Immutable, Arc-structurally-shared image of the DOCUMENT (content
/// fields only — bookkeeping like `midi.dirty`, `midi.loaded_dir` and
/// `plugins.dirty_state` is deliberately excluded from the equivalence
/// contract; `plugins` is carried whole for simplicity but its
/// `dirty_state` field is advisory in a snapshot). Cloning is O(1).
#[derive(Clone)]
pub struct SessionSnapshot {
    pub rev: u64,
    pub epoch: u64,
    pub transport: TransportState,            // small: cloned per capture
    pub project_dir: Option<PathBuf>,
    pub project_name: Option<String>,
    pub created_at: Option<String>,
    pub tracks: Arc<Vec<TrackState>>,
    pub clips: Arc<Vec<Clip>>,                // audio placements
    pub midi: MidiSnapshot,
    pub automation: Arc<Vec<AutomationLane>>,
    pub plugins: Arc<PluginDoc>,
}

/// Per-pattern granularity (ruling F-1): the clip LIST re-allocates on
/// structural change (a Vec of pointers — cheap), but each clip's content
/// is its own Arc, reused untouched across versions that didn't edit it.
#[derive(Clone)]
pub struct MidiSnapshot {
    pub ppq: u32,
    pub tempo_events: Arc<Vec<TempoEvent>>,
    pub meter_events: Arc<Vec<MeterEvent>>,
    pub clips: Arc<Vec<Arc<MidiClip>>>,
}

/// Which parts of the document a batch touched — derived from the folded
/// ops, so capture re-allocates ONLY what changed. `all()` is the epoch/
/// non-op-writer path.
#[derive(Default, Debug, PartialEq)]
pub struct ChangeSet {
    pub transport: bool,
    pub tracks: bool,
    pub clips: bool,
    pub midi_meta: bool,                  // ppq / tempo_events / meter_events
    pub midi_structure: bool,             // clip list add/remove (re-derive whole list)
    pub midi_clips: BTreeSet<ClipId>,     // per-clip content rewrites
    pub automation: bool,
    pub plugins: bool,
    pub project_meta: bool,               // dir/name/created_at
}
impl ChangeSet {
    pub fn all() -> Self;
    /// Op → touched parts. EXHAUSTIVE match over Op (a new variant is a
    /// compile error here — this table is the capture correctness root):
    /// Set{Track,_}→tracks · Set{Clip,_}→clips · Set{MidiClip,_}→that clip
    /// (placement fields live on the row, so the row's Arc re-derives)
    /// · Set{Transport,_}→transport · Set{Plugin,_}→plugins
    /// · TrackAdd/TrackRemove→tracks+clips (TrackRemove carries clips)
    /// · ClipAdd/ClipRemove→clips · TempoSet→midi_meta+transport (bpm mirror)
    /// · MidiClipAdd/MidiClipRemove→midi_structure
    /// · MidiSetNotes{clip,..}→midi_clips.insert(clip)
    /// · AutomationSetLane→automation · PluginAdd/PluginRemove/PluginSetState→plugins
    pub fn from_ops(ops: &[Op]) -> Self;
}

/// Deep bytes of every part `changed` re-allocated — the version node's
/// charge (own-created bytes, RESULTS.md's accounting unit). Deterministic
/// and documented: a re-derived collection charges
/// `len * size_of::<Element>()` plus, for midi clips, `notes.len() *
/// size_of::<MidiNote>()` per rebuilt clip Arc; `plugins` charges rows +
/// params + pending_state byte lengths. An approximation of allocator
/// truth, exact enough for classification (the threshold is 64 KB and the
/// error is bounded by struct padding) — stated in the doc comment.
pub fn charge_of(next: &SessionSnapshot, changed: &ChangeSet) -> usize;
```

- Produces (session.rs):

```rust
pub struct Session {
    // ...existing fields...
    /// The published snapshot: ALWAYS equal (content-wise) to the live
    /// document when no lock is held — updated inside `transact` before
    /// the lock releases, and by `republish_full` at every sanctioned
    /// non-op mutation site (the grep-gate's enumerated writers). The
    /// inner Mutex is a LEAF below `session` in the lock order (held only
    /// for a pointer clone/swap; never across I/O or another lock).
    published: Arc<parking_lot::Mutex<Arc<SessionSnapshot>>>,
}
impl Session {
    /// Capture against the previous published image, re-allocating only
    /// `changed`, publish, and return the fresh image + its charge.
    pub fn capture_and_publish(&mut self, changed: &ChangeSet) -> (Arc<SessionSnapshot>, usize);
    /// `capture_and_publish(&ChangeSet::all())` — for epoch swaps and the
    /// enumerated non-op writers. Callers hold the session lock already.
    pub fn republish_full(&mut self) -> Arc<SessionSnapshot>;
    /// Handle for lock-free consumers (engine): clone the outer Arc once,
    /// then read the inner slot per use.
    pub fn published_handle(&self) -> Arc<parking_lot::Mutex<Arc<SessionSnapshot>>>;
    /// Materialize a scratch, standalone Session from an image (Task 7's
    /// replay-only materialization, Task 8's rollback restore source,
    /// Task 9's replay base). Bookkeeping fields default (dirty=false,
    /// loaded_dir=None, dirty_state empty).
    pub fn from_snapshot(snap: &SessionSnapshot) -> Session;
    /// Overwrite the LIVE document's content fields from an image (Task
    /// 8's panic restore). rev/epoch are NOT taken from the image — the
    /// caller decides (rollback keeps the live rev).
    pub fn restore_from_snapshot(&mut self, snap: &SessionSnapshot);
}
pub struct Committed {
    // ...existing fields...
    /// The image captured under the SAME lock as `rev`/`epoch`.
    pub snapshot: Arc<SessionSnapshot>,
    /// `charge_of` for this batch (own-created bytes) — Task 7 classifies on it.
    pub snapshot_charge: usize,
}
```

  `Session::transact`'s Ok arm, after `fold_ops` and the rev bump, calls
  `capture_and_publish(&ChangeSet::from_ops(&ops))` and threads the pair
  into `Committed`. Transient batches capture too (the engine's rebuild
  must see transport/sample-rate mirrors) — their charge is tiny by
  construction. The Err/rollback arm publishes nothing (the inverses
  restored the document, and the previous image still matches).

- Republish sites (each gets a one-line `// snapshot republish:` marker,
  greppable — this list is the task's checklist AND Task 13's doc
  material): `create_project_at`'s swap block, `open_project_epoch`'s swap
  block, `save_project_as_epoch`'s swap block,
  `project::ensure_default_project`'s swap,
  `plugins::state::adopt_open_project`'s locked install,
  `plugins::automation::adopt_open_project` (both write arms),
  `ControlPlane::set_plugin_pending_state` (R-1),
  `try_seed_zyn_demo_instruments` (both write sites; removed with them in
  Task 10). `execute_persist`'s `midi.dirty` flips and `dirty_state`
  removals do NOT republish — bookkeeping is outside the contract (struct
  doc says so).

- [ ] **Step 1: Write the failing unit tests** (snapshot.rs):

```rust
#[test]
fn capture_reuses_untouched_arcs_and_reallocates_touched_ones() {
    // Session with 2 tracks, 2 midi clips. transact: MidiSetNotes on clip A.
    // Assert: Arc::ptr_eq(prev.tracks, next.tracks); ptr_eq on clip B's Arc;
    // NOT ptr_eq on the clip list Arc or clip A's Arc.
}

#[test]
fn a_point_set_charges_under_the_point_cap_and_a_clip_rewrite_charges_its_content() {
    // Set{Track,Gain} → charge <= 8 * 1024 (POINT_CAP sanity, RESULTS.md).
    // MidiSetNotes with 4_000 notes → charge >= 4_000 * size_of::<MidiNote>().
}

#[test]
fn changeset_from_ops_is_exhaustive_and_correct() {
    // One assertion per Op variant against the doc table above. A new
    // variant breaks the match at compile time; this test pins the mapping.
}
```

- [ ] **Step 2: Write the failing equivalence sweep**
  (`src-tauri/tests/snapshot_store.rs`) — the test that makes "published ==
  live" a property, not a hope. Drive a real `ControlPlane` through one op
  of EVERY family (reuse `tests/figma_invariant.rs`'s scripted-session
  builder pattern — it already exercises every family), plus each epoch
  function (`create_project_at` via `create_project`, `open_project_epoch`,
  `save_project_as_epoch`, `seed_demo_project`), and after every step
  assert canonical-serialized equality between the live document (under a
  short lock) and the published snapshot, masking the bookkeeping fields
  and normalizing `next_note_id` per scope ruling 3 of the inventory. Name:
  `published_snapshot_tracks_the_live_document_across_every_op_family_and_epoch_fn`.

- [ ] **Step 3: Run to verify failure** (compile failure at first —
  acceptable RED), then implement snapshot.rs + session.rs.

- [ ] **Step 4: Wire the republish sites** (the enumerated list), running
  the equivalence sweep after each until green.

- [ ] **Step 5: Full suites; update counts** (backend grows by ~4; record
  the exact number in README/CONTRIBUTING, dated).

- [ ] **Step 6: Commit** —
  `git commit -m "feat(store): SessionSnapshot — COW capture published inside the transact lock (Plan F substrate)"`

---

### Task 6: `engine::rebuild` reads the snapshot — the session lock is released from the graph build

The payoff task, deliberately minimal and self-contained in
`engine.rs` (see the execution note: Tracks B and D queue their engine.rs
work behind THIS task). Today `rebuild`
(`src-tauri/src/audio/engine.rs:764`) holds the session lock across
`derive_slots`, param seeding, table publish, clip assembly, and
`midi::playback::append_from` — the whole graph build. After this task the
heavy assembly runs from a published `SessionSnapshot` with NO lock held,
and only the table publish takes a short session lock, preserving the
[C1] lost-param-write argument.

**The [C1] preservation argument, spelled out** (this is the task's
correctness core — keep it as a comment at the publish site): the landed
code publishes `GraphTables` inside the session lock so that
`<read doc, publish tables>` is atomic against every commit's
`<transact, execute writes>`; publishing tables built from an older read
would silently lose a param write forever. The snapshot design keeps the
atomic pair but SHRINKS the read: (1) assemble the graph from snapshot S
(no lock); (2) take the session lock; (3) re-read the LATEST published
snapshot L under it (capture happens inside `transact`, so under the
session lock L is exactly the live document); (4) build the `ParamTable`
from **L**'s tracks (cheap, O(tracks)) and publish tables + `gen_maps`
still under the lock; (5) release, push the graph. Param VALUES are
therefore never stale. Graph STRUCTURE (clips/notes) may be S ≠ L — but
any structural commit between S and L set `effect.rebuild` and its
`do_rebuild()` queued another `ControlMsg::Rebuild`, so the stale graph is
transient by the same mechanism that already covers "commit lands during a
rebuild's queue wait" today. Slot maps come from L too (`derive_slots(L)`)
so tables and slots stay mutually consistent; the RtTracks assembled from
S are keyed by track id and resolved against L's slots — a track present
in S but gone in L is skipped (it will vanish in the queued rebuild), and
a track in L but not in S contributes params but no clips until that
rebuild (exactly today's "no slot yet" tolerance, mirrored).

**Files:**
- Modify: `src-tauri/src/audio/engine.rs` (`rebuild` ~:764, `ensure_loaded`
  ~:890 and its stale-source scan lock ~:901 — both read the snapshot;
  `Control` gains the published handle at construction),
  `src-tauri/src/midi/playback.rs` (`append_from` signature borrows
  snapshot views), `src-tauri/src/lib.rs`/`src-tauri/src/audio/mod.rs`
  (thread the handle to engine init — follow how `SharedGraphTables` is
  already threaded), plus `append_from`'s offline/bounce call site
  (grep `append_from(` — update every caller to the new signature).
- Test: `src-tauri/src/audio/engine.rs` `#[cfg(test)]` +
  `src-tauri/tests/snapshot_store.rs`

**Interfaces:**
- Consumes: `Session::published_handle()` (Task 5).
- Produces: `midi::playback::append_from(midi: &snapshot::MidiSnapshot,
  tracks: &[TrackState], clips_slots: …, plugins: &PluginDoc, …)` — keep
  the parameter list as close to today's as possible; the mechanical change
  is `&session.midi → &MidiSnapshot` and `store: &Store → the snapshot's
  tracks/clips fields`; the body's clip loop iterates `Arc<MidiClip>`
  (deref-coerces to `&MidiClip`, so note-level code is unchanged).
- Produces: `engine::rebuild` restructured per the argument above. The
  six `// read-only:` session-lock sites OUTSIDE rebuild/ensure_loaded
  (`:1016` meter fold, `:1114` transport snapshot, `:1154`/`:1202`
  recording resolution) are OUT OF SCOPE — short reads, not the deferral
  (non-goal; note in commit message).

- [ ] **Step 1: Write the failing test** — the deferral's own pin:

```rust
#[test]
fn rebuild_does_not_hold_the_session_lock_across_the_graph_build() {
    // Headless Control fixture (the engine tests' existing pattern).
    // Install a probe: hold the session lock on another thread for 500ms,
    // then trigger rebuild() and assert it completes its assembly phase
    // without blocking on the lock — concretely: rebuild() with the
    // session lock HELD by the test thread must still reach graph
    // assembly (assert via a generation bump or a timeout-bounded join
    // where the OLD code deadlocks/blocks and the new code proceeds to
    // the short publish wait only).
}

#[test]
fn a_param_write_committed_during_assembly_is_never_lost() {
    // The [C1] regression pin, snapshot edition: start a rebuild whose
    // assembly is artificially slowed (test hook or large doc), commit a
    // Set{Track,Gain} mid-flight, assert the published ParamTable after
    // the rebuild carries the NEW gain (values from L, not S).
}
```

  (The existing test at engine.rs:1685 shows the fixture; the publish-
  under-lock test at the [C1] comment's test — grep `GraphTables` in the
  test module — shows how tables are asserted. If mid-flight interleaving
  proves untestable without a hook, add a `#[cfg(test)]` assembly-pause
  hook — a test-only closure slot on `Control` — rather than weakening the
  assertion.)

- [ ] **Step 2: RED**, then **Step 3: implement** rebuild + ensure_loaded
  + `append_from` signature, keeping the diff mechanical outside the
  publish-site comment.

- [ ] **Step 4: Full suites** — this task must be zero-regression across
  all 500+ engine-adjacent tests; update counts (+2).

- [ ] **Step 5: Commit** —
  `git commit -m "feat(engine): rebuild reads the published snapshot — session lock released from the graph build (Plan A deferral lifted)"`

---

### Task 7: The version graph — replay-only at 64 KB, budgeted retention, janitor eviction

`HistoryLog` grows the retention substrate ADR 0005 specifies, fed from
the same single sink `record_commit` already is. One node per
non-transient batch; classification and budget per the MEASURED numbers
(cite them in the constants' doc comments, ADR 0007 evidence policy):
replay-only at **64 KB own-created bytes** (`benches/bulkbench/RESULTS.md`
§6: at 256 KB the rule was a measured no-op — 0.7 % saving — because
whole-clip ops sit at ~104–131 KB; at 64 KB the whole-clip class is
replay-only, saving 21 % on the AGENT profile, worst measured replay chain
23 whole-clip transforms, well under a millisecond); budget 512 MiB
defended by eviction (~54 700 HUMAN / ~6 550 AGENT steps measured); the
capture effect stated so nobody re-derives replay-only as a budget defense
(replay-only bounds node charges and saves iteration bursts; EVICTION
defends the budget).

**Files:**
- Create: `src-tauri/src/control/vergraph.rs`
- Modify: `src-tauri/src/control/history.rs` (`HistoryLog` gains the
  graph; `record_commit`/`record_gesture` take `&Committed`),
  `src-tauri/src/control/mod.rs` (`Committer` call sites)
- Test: `vergraph.rs` unit tests + `src-tauri/tests/snapshot_store.rs`

**Interfaces:**
- Produces (vergraph.rs):

```rust
/// Classification threshold (RESULTS.md §6, measured): a batch whose
/// own-created bytes exceed this stores op+inverse instead of a
/// materialized image.
pub const REPLAY_ONLY_THRESHOLD: usize = 64 * 1024;
/// Bytes ceiling the janitor defends (round-2 §6's measured budget).
pub const VER_BUDGET_BYTES: usize = 512 * 1024 * 1024;
/// Never evict below this many retained steps (ruling F-2: aligned with
/// UNDO_STACK_LIMIT so browsing never covers less than undo reaches).
pub const VER_STEPS_FLOOR: usize = 200;

pub enum VersionNode {
    Materialized { snapshot: Arc<SessionSnapshot>, charge: usize },
    ReplayOnly { ops: Vec<Op>, inverses: Vec<Op>, charge: usize },
}
pub struct VersionGraph {
    /// Rev-ordered chain within the current epoch (ruling F-11). Inserted
    /// in rev order (same L-4 discipline as Task 11's stack).
    nodes: Vec<(u64 /*rev*/, VersionNode)>,
    retained_bytes: usize,
    epoch: u64,
}
impl VersionGraph {
    /// Push a node classified from `committed.snapshot_charge`; returns
    /// evicted loads for the caller to hand the janitor (never dropped on
    /// the committing thread — ADR 0005's janitor rule).
    pub fn record(&mut self, committed: &Committed) -> Vec<VersionNode>;
    /// Root at a new epoch: drain everything (returned for the janitor).
    pub fn clear(&mut self) -> Vec<VersionNode>;
    /// Materialize `rev`: nearest materialized ancestor at-or-below,
    /// `Session::from_snapshot`, apply each subsequent node's ops via a
    /// scratch transact-free `apply_raw` loop, capture. None if evicted
    /// below the floor or rev unknown. CPU-bound; called with NO HistoryLog
    /// mutex held (the caller clones the node range out first).
    pub fn materialize(&self, rev: u64) -> Option<SessionSnapshot>;
    pub fn stats(&self) -> VersionStats; // nodes/materialized/replay_only/retained_bytes
}

/// Off-thread drop sink (measured why: dossier 06's worst retire-queue
/// drop was 83.8 ms — never on a committing/UI-facing thread).
pub struct Janitor { tx: std::sync::mpsc::Sender<Vec<VersionNode>> }
impl Janitor {
    /// Spawns the named thread ("aura-janitor"); dropping the last sender
    /// ends it. `send` never blocks (unbounded channel — eviction batches
    /// are rare and the janitor only drops).
    pub fn spawn() -> Janitor;
    pub fn dispose(&self, load: Vec<VersionNode>);
}
```

- Produces (history.rs): `HistoryLog` gains `versions: Mutex<VersionGraph>`
  and `janitor: Janitor`; **`record_commit(&self, committed: &Committed,
  mode: HistoryMode)`** replaces the six-arg form (`Committed` now carries
  snapshot + charge). `record_gesture` is DIFFERENT: `close_gesture`'s
  batch is synthesized (its folds were committed transiently — there is no
  `Committed` for the net batch), so it keeps its explicit-args form and
  gains two: **`record_gesture(&self, rev, epoch, meta, ops, inverses,
  snapshot: Arc<SessionSnapshot>, charge: usize)`**, where `close_gesture`
  reads the published snapshot under the SAME short session lock it
  already takes for `rev`/`epoch` (the last transient fold published it,
  so it matches the folded state) and computes `charge` as
  `charge_of(&snapshot, &ChangeSet::from_ops(&ops))`. Internal order: epoch mutex →
  {journal, history, versions} exactly as today, `versions` never held
  with `history`/`journal` simultaneously; eviction returns drop-loads that
  are handed to the janitor AFTER all mutexes release. `epoch_boundary`
  additionally drains the graph to the janitor. Empty-batch and
  stale-epoch guards cover the graph too (same single-sink discipline —
  the guards move to cover three streams instead of two).
- Consumes: `Committed.snapshot` / `snapshot_charge` (Task 5).

- [ ] **Step 1: Failing unit tests** (vergraph.rs):

```rust
#[test]
fn a_small_batch_materializes_and_a_whole_clip_rewrite_goes_replay_only() {
    // charge 1 KB → Materialized; charge 100 KB (the measured whole-clip
    // class) → ReplayOnly. Assert via stats().
}
#[test]
fn materializing_a_replay_only_rev_replays_from_the_nearest_ancestor() {
    // Base doc, one materialized node, two replay-only MidiSetNotes nodes;
    // materialize(last_rev) == the document after applying both (compare
    // canonical serialization against a session driven normally).
}
#[test]
fn eviction_respects_the_bytes_ceiling_and_the_steps_floor() {
    // Tiny test ceiling (constructor takes budget overrides for tests):
    // push until over budget → oldest materialized nodes returned for
    // disposal, never dropping below the floor count.
}
#[test]
fn every_landed_structural_op_carries_its_minted_ids() {
    // Ruling F-10's pin: construct one of each structural op via the real
    // command paths (or op constructors) and assert serialization contains
    // the row ids — the replay-only mechanism's exclusion rule is vacuous
    // and must stay provably so.
}
#[test]
fn evicted_loads_are_dropped_on_the_janitor_thread() {
    // Node holding an Arc probe; after eviction + a bounded wait loop,
    // Arc::strong_count dropped to 1 WITHOUT this thread dropping it
    // (assert the janitor thread name did the drop via a Drop-impl probe
    // recording thread::current().name()).
}
```

- [ ] **Step 2: RED → implement vergraph.rs.**
- [ ] **Step 3: Wire `HistoryLog`** (signature change; fix both Committer
  call sites and every history.rs test — the existing tests construct
  six-arg `record_commit` calls; give the test module a
  `committed_for_test(rev, epoch, meta, ops, inverses)` builder that also
  synthesizes a snapshot + charge, so the churn is one helper).
- [ ] **Step 4: Integration** (`snapshot_store.rs`): drive the real
  `ControlPlane` through a mixed session; assert `stats()` via a
  `#[cfg(test)]` accessor on `HistoryLog`: node count == non-transient
  commits, transient commits produce no node, an epoch boundary drains to
  zero.
- [ ] **Step 5: Full suites; update counts** (+6).
- [ ] **Step 6: Commit** —
  `git commit -m "feat(store): version graph — replay-only at 64 KB, budgeted retention, janitor eviction (ADR 0005)"`

---

### Task 8: Panic containment in `transact` — L-5 closed (ruling F-3)

`Session::transact` currently has no panic story: a closure that panics
mid-`apply_raw` unwinds with the document half-mutated and NO journal
record — L-5's permanent silent divergence. The snapshot makes the fix
mechanical: the published image IS the pre-transaction state (Task 5's
equivalence invariant), so restore from it.

**Files:**
- Modify: `src-tauri/src/control/session.rs` (`Session::transact`)
- Test: session.rs tests + count refresh

**Interfaces:**
- Consumes: `Session::restore_from_snapshot` + the `published` slot
  (Task 5).
- Produces: `transact`'s closure call becomes
  `std::panic::catch_unwind(AssertUnwindSafe(|| f(&mut tx)))`. On
  `Err(payload)`: read the pre-tx image (cloned from `published` BEFORE
  running the closure — one Arc clone, effectively free),
  `restore_from_snapshot`, keep `rev`/`epoch` untouched (nothing
  committed), `log::error!("transact: closure panicked; document restored
  — this is a bug in the caller: {payload:?}")`, return
  `Err("transaction panicked: <best-effort payload string>")`. The
  `IN_TX` scopeguard already resets on unwind (verify — it uses a Drop
  guard; assert in the test). `AssertUnwindSafe` is justified IN A COMMENT
  by the restore: no state observed after the catch predates the restore.

- [ ] **Step 1: Failing test:**

```rust
#[test]
fn a_panicking_closure_leaves_the_document_and_the_channel_untouched() {
    let m = fresh_session_mutex(); // existing session.rs test fixture pattern
    let gain_before = m.lock().store.tracks[0].gain_db;
    let r = Session::transact(&m, user_meta(), |tx| {
        tx.apply(set_gain("t-1", -12.0))?;   // mutates…
        panic!("mid-transaction bug");        // …then dies half-way
    });
    assert!(r.is_err_and(|e| e.contains("panicked")));
    assert_eq!(m.lock().store.tracks[0].gain_db, gain_before, "restored (L-5)");
    // The channel still works and IN_TX was reset:
    Session::transact(&m, user_meta(), |tx| tx.apply(set_gain("t-1", -3.0))).unwrap();
}
```

- [ ] **Step 2: RED** (today the panic propagates and kills the test) →
  **Step 3: implement** → **Step 4: full suites, counts (+1)**.
- [ ] **Step 5: Commit** —
  `git commit -m "feat(channel): panic containment in transact — snapshot restore closes L-5 (ruling F-3)"`

---

### Task 9: The journal's first reader — `(epoch, rev)` sort + save-mark tail replay

The moment the format becomes read (Global Constraints: the
no-dual-shape-reader freedom ends HERE). Deliverables per ruling F-8:
parse, sort, tail-extract, replay onto a scratch session, fidelity tests,
and a detection warn at project open. Consumes L-4 (the reader MUST sort
by `(epoch, rev)` — file order is not rev order under concurrent
committers) and L-2 (ruling F-9's determinism argument, asserted
strongly).

**Files:**
- Create: `src-tauri/src/control/replay.rs`,
  `src-tauri/tests/journal_replay.rs`
- Modify: `src-tauri/src/control/mod.rs` (`open_project_epoch` tail-detect
  warn), `src-tauri/src/control/history.rs` (export `JOURNAL_FILE` reuse —
  already `pub`)
- Test: replay.rs unit tests + the integration file + counts

**Interfaces:**
- Produces (replay.rs):

```rust
/// One parsed journal line. Unknown-field-tolerant (D-06); v1 batch lines
/// are counted and skipped per ruling F-5; a torn final line (no trailing
/// newline / invalid JSON at EOF) is counted and skipped, never an error.
pub enum JournalRecord {
    Batch { v: u16, rev: u64, epoch: u64, actor: Actor, run: String, label: String, ops: Vec<Op> },
    Epoch { event: String, epoch: u64 },   // "open"|"create"|"saveAs"|"ensure"
    SaveMark { epoch: u64 },
}
pub struct ReadReport { pub records: Vec<JournalRecord>, pub skipped_v1: usize, pub torn_tail: bool }
/// Read + parse + SORT by (epoch, rev) — L-4's reader discipline; epoch
/// records sort at their epoch's head, save marks keep file position
/// within their epoch (they carry no rev; a mark's meaning is "snapshot
/// caught up HERE", so it anchors between the batch it follows in file
/// order and the next — document this in the fn doc).
pub fn read_journal(path: &Path) -> Result<ReadReport, String>;
/// The unsaved tail: batches of the LAST epoch in the report that come
/// after that epoch's LAST save mark (or all of that epoch's batches if
/// it has no mark). Empty = disk snapshot is current.
pub fn unsaved_tail(report: &ReadReport) -> Vec<&JournalRecord /* Batch only */>;
/// Apply a tail to a scratch session (Session::from_snapshot of the
/// disk-loaded document, or any Session the caller built): each batch
/// through ONE transact whose closure applies the recorded ops in order.
/// Returns how many batches applied. Stops with Err on the first failing
/// batch (a diverged log must be loud, not partially applied — the caller
/// decides what to do; nothing in this plan auto-applies).
pub fn replay_tail(session: &Mutex<Session>, tail: &[&JournalRecord]) -> Result<usize, String>;
```

- Produces (mod.rs): `open_project_epoch`, after the adopt steps, calls
  `read_journal` + `unsaved_tail` on the project's `journal.ndjson` (best
  effort — any Err is a debug log) and `log::warn!("journal: {} unsaved
  batch(es) recorded after the last save — the on-disk snapshot is behind
  the log", n)` when non-empty. No auto-apply (ruling F-8), no event, no
  UI.

- [ ] **Step 1: Failing unit tests** (replay.rs):

```rust
#[test]
fn records_are_sorted_by_epoch_then_rev_regardless_of_file_order() {
    // Write lines rev 3,1,2 (same epoch) into a temp file via JournalWriter
    // -level json (construct the json strings directly — the writer API
    // has no out-of-order mode); assert read order 1,2,3. (L-4)
}
#[test]
fn v1_batch_lines_are_skipped_and_counted_and_a_torn_tail_is_tolerated() {
    // One {"v":1,...} line + valid v2 lines + a final half-written line.
}
#[test]
fn unsaved_tail_is_the_batches_after_the_last_save_mark_of_the_last_epoch() {
    // epoch 1: batch, mark, batch, batch → tail = 2.
    // epoch advances to 2 with no mark: batch → tail = 1 (only epoch 2).
}
```

- [ ] **Step 2: Failing fidelity test** (`tests/journal_replay.rs`) — the
  headline:

```rust
#[test]
fn a_cold_tail_replay_reproduces_the_crashed_sessions_document_byte_identically() {
    // 1. Real ControlPlane, create_project (journal opens), edits across
    //    families: add_track, move_clip-style Set, MidiClipAdd,
    //    MidiSetNotes WITH noteId:0 mint sentinels (F-9's subject),
    //    TempoSet, automation_set.
    // 2. save_project (mark). 3. MORE edits after the mark (incl. another
    //    MidiSetNotes with sentinels). 4. Capture the live canonical
    //    snapshot; drop the ControlPlane (the "crash" — no further saves).
    // 5. Cold path: load the project files into a fresh Session (the same
    //    loaders open_project_epoch uses), read_journal, unsaved_tail,
    //    replay_tail. 6. Canonical-compare replayed vs captured — NO
    //    next_note_id normalization, NO note-id masking (ruling F-9: the
    //    watermark in the saved snapshot makes re-minting deterministic;
    //    this asserting byte-identity IS the L-2-is-benign proof).
}
```

- [ ] **Step 3: RED → implement replay.rs**, then the open-time warn.
- [ ] **Step 4: Full suites; counts (+5).**
- [ ] **Step 5: Commit** —
  `git commit -m "feat(journal): first reader — (epoch,rev) sort, save-mark tail replay, open-time detection (L-4 consumed, L-2 proven benign)"`

---

### Task 10: R-3 closed — the demo's Zyn bootstrap becomes ops in the seed transaction

Ruling F-12. `try_seed_zyn_demo_instruments`
(`src-tauri/src/control/mod.rs:2501`) pushes three rows into
`session.plugins.instances` directly (the R-3 residual), and
`seed_demo_project` (~:2412) hand-persists the plugin doc afterwards.
After this task the function only PREPARES (instantiate + load patch +
capture state — all host I/O, all outside any lock/transaction:
prepare-outside), and the seed's ONE existing commit applies
`Op::PluginAdd` + `Op::PluginSetState` per instance, making the demo's
instruments attributed, undoable (one step with the rest of the demo),
persisted via `PersistEffect`, and cold-replayable from the journal.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (`try_seed_zyn_demo_instruments`,
  `seed_demo_project`; delete the manual persist block ~:2412-2425 and the
  direct-push/rollback blocks ~:2538-2563; delete Task 5's republish
  markers here)
- Test: mod.rs tests + counts; update `docs/SIDE-CHANNEL-INVENTORY.md`'s
  R-3 entry (marked: "CLOSED in Plan F Task 10") — small enough to carry
  here rather than deferring wholly to Task 13

**Interfaces:**
- Produces: `try_seed_zyn_demo_instruments` renamed intent, same fn name:
  returns `Option<[PreparedZynInstance; 3]>` where

```rust
struct PreparedZynInstance {
    row: crate::plugins::PluginInstanceInfo,   // as instantiate_and_activate returned
    params: Vec<ParamInfo>,
    state: Option<Vec<u8>>,                    // bridge.save_state(&row.id) post-patch-load,
                                               // APST-encoded via encode_state (same bytes
                                               // PluginSetState's arm expects)
}
```

  touching NO session state; failure rollback shrinks to host
  `unregister_instance` calls only (there are no rows to retract). The
  seed commit's closure then applies, per prepared instance, in order:
  `Op::PluginAdd { row, index: tx-current instances len }` then, when
  `state` is `Some`, `Op::PluginSetState { instance, state, .. }`
  (construct exactly as `zyn_load_patch`'s command path does — copy that
  call site's op construction; the caller must first seed
  `set_plugin_pending_state`? NO — check the arm: `PluginSetState` carries
  the state ON the op since the I-5 format work, so no R-1 pre-seed is
  needed here; if the arm's inverse capture requires a pending_state
  pre-seed, mirror `patches.rs:322`'s sequence instead and say so in the
  commit message). The `InstrumentId` binding `Set`s and the clip adds are
  unchanged.
- Consumes: `PluginAdd`'s idempotent `Instantiate` host-forward (the host
  is already live from preparation — the executor's `has_instance` check
  no-ops it and re-syncs params); `PluginSetState`'s `LoadState` forward
  (re-offers the same bytes — idempotent by content).

- [ ] **Step 1: Failing test:**

```rust
#[test]
fn the_seed_demo_transaction_journals_its_plugin_rows() {
    // Fake/registered bridge fixture (the plugins tests' FormatStateBridge
    // pattern) so preparation succeeds without real Zyn; run
    // seed_demo_project on a ControlPlane with an open project; assert:
    // (a) session.plugins.instances.len() == 3 and every row arrived via
    //     the op path — the journal's seed line contains three pluginAdd
    //     ops and three pluginSetState ops (read journal.ndjson directly);
    // (b) ONE undo removes tracks, clips AND plugin rows (the demo is one
    //     step);
    // (c) no direct-write remains: grep-level assertion is Task 13's, here
    //     assert session.plugins is empty after the undo.
}
```

  (If the existing demo tests run without any bridge and skip Zyn, keep
  that path green: preparation returning `None` must still seed the
  PolySynth demo exactly as today — assert one existing demo test still
  passes unchanged.)

- [ ] **Step 2: RED → implement.** Note the ordering trap the old code
  documents: preparation happens BEFORE the content-emptiness re-check?
  No — keep today's order (emptiness check, then prepare, then commit);
  the commit's closure re-validates emptiness via `tx.store()` if the old
  code did (check; if it didn't, don't add — behavior parity).
- [ ] **Step 3: Full suites; counts (+1); inventory R-3 edit (marked).**
- [ ] **Step 4: Commit** —
  `git commit -m "feat(demo): Zyn bootstrap through the channel — PluginAdd/PluginSetState in the seed tx (R-3 closed)"`

---

### Task 11: Rev-ordered undo stack + `VecDeque` — L-4's stack half (ruling F-4) and M-7

Two small structural fixes inside `History`. (1) L-4's stack half: two
concurrent committers can reach the sink out of rev order, so the top of
the undo stack can be an OLDER batch than the one below it, and Ctrl+Z
applies an older inverse over a newer write. Entries learn their `rev` and
`record` inserts in rev order. (2) M-7 (the track brief's "M-5"): eviction
uses `Vec::remove(0)` — O(n) memmove per commit at the cap; `VecDeque`
makes it O(1).

**Files:**
- Modify: `src-tauri/src/control/history.rs` (`HistoryEntry`, `History`)
- Test: history.rs tests + counts

**Interfaces:**
- Produces: `HistoryEntry` gains `pub rev: u64` (populated in
  `from_committed`/`from_gesture` — both already receive the `Committed`
  after Task 7's signature change); `History { undo: VecDeque<HistoryEntry>,
  redo: VecDeque<HistoryEntry> }`; `record` finds the insertion point from
  the back by `rev` (a late-arriving older batch inserts BELOW newer
  entries; the 350 ms merge only ever merges with the entry immediately
  below the insertion point when keys/actor/label match AND revs are
  adjacent-in-order — an out-of-order arrival never merges); eviction pops
  the front. `pop_undo` still pops the back (now guaranteed the
  highest-rev entry). Redo pushes keep stack order (redo entries migrate
  in pop order — document why that is already rev-consistent: redo is
  populated only by undo pops, which are rev-descending).

- [ ] **Step 1: Failing tests:**

```rust
#[test]
fn a_late_arriving_older_batch_inserts_below_the_newer_one() {
    // record(entry rev 7); record(entry rev 6, different key) — pop order
    // must be 7 then 6, not 6 then 7.
}
#[test]
fn eviction_at_the_cap_drops_the_lowest_rev_entry() {
    // Fill past UNDO_STACK_LIMIT with ascending revs; assert front dropped,
    // newest survives (mirror the existing bounded-stack test, plus rev).
}
```

- [ ] **Step 2: RED → implement** (existing history tests churn
  mechanically for the `rev` field — extend the local `entry()` helper
  with a rev parameter defaulting to a counter).
- [ ] **Step 3: Full suites; counts (+2).**
- [ ] **Step 4: Commit** —
  `git commit -m "fix(history): rev-ordered undo stack (L-4 stack half) + VecDeque eviction (M-5)"`

---

### Task 12: Placement offsets — the measured 5.4× lever gets its document route

RESULTS.md finding 4: routing transpose/velocity-class gestures through
placement offset fields instead of leaf rewrites is worth 5.4× on the
HUMAN profile (550 → 101 MB per 10 k gestures) — "no other measured
intervention comes within a factor of 3." Round-2 §5's placement table
already assigns "transpose / velocity offset (MIDI)" to the placement row;
this task lands the fields, the op paths, the playback application, and
one additive command, plus the BINDING routing rule for future gesture
work (ruling F-10's sibling: any transpose/velocity gesture MUST address
these fields, never rewrite notes).

**Files:**
- Modify: `src-tauri/src/midi/types.rs` (`MidiClip` fields),
  `src-tauri/src/control/op.rs` (`PropPath` variants),
  `src-tauri/src/control/session.rs` (`apply_raw`'s MidiClip `Set` arm +
  `ChangeSet::from_ops` already routes `Set{MidiClip,_}` per-clip — verify
  the new paths inherit that), `src-tauri/src/midi/playback.rs` (apply at
  event-schedule time), `src-tauri/src/control/mod.rs` +
  `src-tauri/src/midi/mod.rs` (additive command, mirroring
  `midi_set_clip_bounds_core`'s shape), `src/lib/tauri.ts` (binding),
  `docs/ipc-schemas/` project schema if `midiClips` rows are schema-pinned
  (check `midi-project.schema` / v3 schema — additive `#[serde(default)]`
  fields, additive schema properties)
- Test: midi + session + playback tests, one vitest binding test if
  `tauri.ts` bindings are tested (follow the `move_clip` binding's
  precedent), counts

**Interfaces:**
- Produces (types.rs — both additive, wire- and file-compatible):

```rust
/// Placement transpose in semitones (round-2 §5 placement table; §6's
/// mandated route for transpose-class gestures — RESULTS.md finding 4).
/// Applied at schedule/bounce time; note CONTENT is never rewritten.
#[serde(default)]
pub transpose_semitones: i16,
/// Placement velocity offset, added to each note's velocity at schedule
/// time, result clamped to 1..=127.
#[serde(default)]
pub velocity_offset: i16,
```

- Produces (op.rs): `PropPath::TransposeSemitones`, `PropPath::VelocityOffset`
  (additive variants; wire = their camelCase names — confirm against
  `PropPath`'s serde attrs and add both to the paths module/schema the
  §4.6 anti-drift rule keeps in one place). `apply_raw`'s
  `ObjectRef::MidiClip` arm: write the field, inverse from store truth,
  `effect.rebuild = true`, `effect.persist.midi = true` (placement is
  persisted midi state — mirror `TimelineStartTicks`'s arm exactly).
- Produces (playback.rs): at the point notes become scheduled events, key
  = `clamp(note.key as i32 + clip.transpose_semitones as i32, 0, 127)`,
  velocity = `clamp(note.velocity as i32 + clip.velocity_offset as i32, 1, 127)`.
  Offline bounce goes through the same helper (it already shares this
  path — Task 6 kept it so); `midi_export_file` exports CONTENT unchanged
  (content vs placement — state this in the command's doc).
- Produces (commands): additive `midi_set_clip_placement { clip_id,
  transpose_semitones: Option<i16>, velocity_offset: Option<i16> }` →
  one commit applying one `Set` per provided field (both in one tx when
  both given — one undo step), validation: clip exists (inside the
  closure via `tx.midi()`). `tauri.ts`: `midiSetClipPlacement(...)`
  binding, same style as `midi_set_clip_bounds`'s.

- [ ] **Step 1: Failing tests:**

```rust
#[test]
fn placement_transpose_shifts_scheduled_keys_without_rewriting_notes() {
    // Clip with keys [60, 64]; set transpose +12 via the command core;
    // assert scheduled events carry 72/76, session.midi notes still 60/64,
    // and the commit produced Set ops only (no MidiSetNotes — the lever).
}
#[test]
fn placement_offsets_round_trip_save_open_and_undo() {
    // set both fields → save → cold open → fields survive; undo → both 0.
}
#[test]
fn velocity_offset_clamps_at_schedule_time_only() {
    // velocity 120, offset +20 → scheduled 127; stored note still 120.
}
```

- [ ] **Step 2: RED → implement** (types → op paths → apply arm →
  playback → command → binding, in that order, compiling at each stage).
- [ ] **Step 3: Full suites; counts** (backend +3; frontend +1 if a
  binding test exists — README/CONTRIBUTING both).
- [ ] **Step 4: Commit** —
  `git commit -m "feat(midi): placement transpose/velocity offsets — ops, playback, additive command (round-2 §5/§6 routing lever)"`

---

### Task 13: Close-out — inventory corrections, handoff, next-prompt

Paperwork with teeth: every ruling this plan made lands in the durable
docs, marked per ADR 0007, so the next session reads state instead of
re-deriving it.

**Files:**
- Modify: `docs/SIDE-CHANNEL-INVENTORY.md`, `docs/PHASE4-PLAN.md`,
  `next-prompt.md`, `README.md`/`CONTRIBUTING.md` (final counts),
  `docs/adr/0005-history-storage.md` (one marked implementation note)
- Test: none (docs). The grep gate re-run IS the verification step.

**Steps:**

- [ ] **Step 1: `docs/SIDE-CHANNEL-INVENTORY.md`** (each edit marked
  "(Plan F, 2026-08-14)"): R-3 → CLOSED (Task 10; the direct-push
  paragraphs replaced by the op-path description); L-2 → append ruling
  F-9's determinism argument and the byte-identical replay test name;
  L-4 → CLOSED via reader discipline + rev-ordered stack (ruling F-4;
  file order remains unordered BY DOCUMENTED RULE); L-5 → CLOSED (Task 8);
  the grep-gate section gains the snapshot-republish sites (`// snapshot
  republish:` markers) and drops R-3's row from the residuals.
- [ ] **Step 2: Re-run the grep gate**
  (`grep -rn '\.lock()' src-tauri/src --include=*.rs`) and re-verify the
  inventory's engine claim: after Task 6, `engine.rs`'s rebuild/
  ensure_loaded no longer appear as session-lock read sites — update the
  "Every `session.lock()` in engine.rs" paragraph's site list to the
  survivors (meter fold, transport snapshot, recording resolution) with
  fresh line anchors.
- [ ] **Step 3: `docs/PHASE4-PLAN.md`** — append a "Plan F handoff"
  section after the Plan E handoff, same conventions: scope rulings
  F-1..F-12 verbatim, the carry-forwards Plan F lifted (snapshot rebuild,
  panic rollback, journal reader, R-3) each marked lifted with its task,
  and the NEW carry-forwards: (a) the live-document B-tree migration +
  its trigger (ruling F-1); (b) I-1's option-(a) residual (ruling F-6);
  (c) no auto-apply of journal tails (ruling F-8); (d) seeded-PRNG
  constraint binds future random ops (ruling F-10); (e) version-graph
  product surface (browse UI/stats command) unbuilt on purpose. Also carry
  the review's M-5 here as a free marked correction while editing this
  file: one sentence at the Gate E claim noting the Figma invariant
  replays ops through its own commits, and `tests/journal_and_history.rs`
  is what covers the shipped Ctrl+Z path.
- [ ] **Step 4: `next-prompt.md`** — rewrite Track A's section to
  "landed" state (mirroring how Plan E's landing was recorded): what
  landed, the new baseline counts, the engine.rs sequencing note resolved
  (B/D may now rebase on the snapshot-rebuild), pointers to the handoff.
- [ ] **Step 5: `docs/adr/0005-history-storage.md`** — under Consequences,
  one marked note: "Implementation (Plan F, 2026-08-14): landed at
  per-clip Arc granularity with the within-clip tree deferred to the
  note-delta-op round — see the Plan F handoff, ruling F-1." (The ADR's
  decision text itself is not rewritten.)
- [ ] **Step 6: Final full suites, final counts in README/CONTRIBUTING
  (dated), commit** —
  `git commit -m "docs: Plan F close-out — inventory corrections, handoff, next-prompt (ADR 0007)"`

---

## Self-review notes (writing-plans skill, run against this plan before Task 1 starts)

1. **Spec coverage.** Round-2 §6 / ADR 0005 clause by clause: COW
   session structure → Task 5 (granularity deviation argued in ruling
   F-1, marked in Task 13); retention IS the version graph, no limbo/
   refcounts (§2.3/O-14) → Task 7 (deleted objects live in retained
   snapshots; plain model objects only — plugin instances appear as rows
   + state BLOBS in snapshots, never live handles, satisfying dossier 07
   rules 10/17/18 by construction since `PluginDoc` holds bytes);
   replay-only first-class at the measured 64 KB → Task 7 with RESULTS.md
   cited in the constant's doc; non-deterministic-id exclusion → ruling
   F-10 + test; seeded-PRNG constraint → recorded as carry-forward (no
   random ops exist); placement-offset routing → Task 12; janitor
   mandatory → Task 7; bytes ceiling/steps floor → Task 7 constants;
   capture effect stated → Task 7 preamble. ADR 0003's "graph rebuilds
   read an immutable snapshot, never the lock" → Task 6. Track A scope
   from next-prompt: journal reader → Task 9; R-3 → Task 10; L-2 → ruling
   F-9/Task 9; L-4 → Tasks 9+11/ruling F-4; L-5 → Task 8/ruling F-3;
   orchestrator-held findings I-1/I-7 early and together → Tasks 1–2;
   I-6 + C-1 residual one task → Task 4; minors M-1/M-2/M-4/M-7 in (plus
   the report's M-5 sentence riding Task 13), the rest ruled non-goals
   with reasons. Undo depth unchanged → ruling F-2.
2. **Known risks, named:** (a) Task 5's republish-site enumeration is the
   correctness root — the equivalence sweep (its Step 2) is deliberately
   an every-family + every-epoch-fn property so a missed site fails a
   test, not a user; (b) Task 6 carries the plan's subtlest concurrency
   argument ([C1] preservation) — the argument is written into the task
   so the implementer transcribes rather than re-derives, and the
   mid-flight param-write test pins it; (c) Task 7's signature change on
   `record_commit` churns history tests — bounded by the
   `committed_for_test` builder; (d) charge accounting is an approximation
   — stated in its doc, and classification only needs order-of-magnitude
   accuracy against a 64 KB line; (e) counts are re-verified at every task
   boundary against the dated baseline, and the README-said-506 trap is
   pre-cleared in Global Constraints.
3. **Type consistency check:** `Committed.snapshot`/`snapshot_charge`
   (Task 5) are what Task 7's `record(&Committed)` and Task 6's engine
   never actually consume from `Committed` (the engine reads the PUBLISHED
   slot — deliberate, stated in Task 6); `pop_undo() ->
   Option<(HistoryEntry, u64)>` (Task 4) matches Task 11's `HistoryEntry`
   gaining `rev` (field addition, tuple unchanged);
   `Session::from_snapshot`/`restore_from_snapshot` defined once (Task 5),
   consumed by Tasks 7/8/9; `ChangeSet::from_ops`'s table covers Task 12's
   new `PropPath` variants via the existing `Set{MidiClip,_}` row (noted in
   Task 12); `clear_dirty_state_matching` (Task 3) is used by Task 2's
   Save-As only AFTER Task 3 lands — Task 2 ships the plain remove and
   Task 3 tightens both sites (stated in both tasks).
4. **Placeholder scan:** performed; remaining comment-sketch test bodies
   (Tasks 2/3/9/10) each name the concrete in-repo fixture to copy (the
   `:1673` registered-session test, the tauri-free `ControlPlane` fixture,
   `figma_invariant.rs`'s scripted builder, the `FormatStateBridge`
   pattern) — the deliberate density trade Plan E's self-review also made,
   against named examples, not TBDs.
5. **Ordering sanity:** 1→2→3 are the held data-loss/persist fixes, early
   and together per the orchestrator ruling, none touching the store; 4 is
   independent of the store (history/mod only) and precedes Task 7's
   history churn so the epoch-plumbed signatures are stable before the
   graph lands on the same file; 5 is the substrate; 6 consumes 5 and is
   the ONLY engine.rs task; 7 consumes 5's `Committed` fields; 8 consumes
   5's restore; 9 consumes 5's scratch sessions and SHOULD land after 8 so
   replayed transacts enjoy panic containment; 10 consumes 9 only for its
   cold-replay motivation (no hard dep — it needs only landed op arms);
   11 is history-internal and after 7 (entry construction moved to
   `&Committed`); 12 is independent of 5–11 (kept late to avoid
   playback.rs collisions with Task 6); 13 last.

## Execution note

Worktree: `/home/knobo/prog/dav/.claude/worktrees/track-a-plan-f`, branch
`plan-f-history`, cut from `origin/main` at `3340aa8`. Foreground,
timeout-guarded test runs only; push at task boundaries once a PR exists;
merge `origin/main` in at task boundaries when it advances. SDD ledger
(gitignored, established convention):
`.superpowers/sdd/2026-08-14-plan-f-history-storage/progress.md`.

**Cross-track engine.rs sequencing (binding, from the orchestrator):**
Tracks B (MIDI slice 2) and D (automation audible) run in parallel
worktrees and BOTH queue their `engine.rs` work behind **Task 6** of this
plan — the orchestrator sequences engine.rs-touching tasks across tracks.
Therefore Task 6's engine.rs diff must stay minimal and self-contained
(rebuild + ensure_loaded + handle threading, nothing else — the other
read-lock sites are explicitly out of scope), and Task 6 should be pushed
promptly when green so B and D can rebase on it. No other task in this
plan may touch `engine.rs`.

Execution mode (solo vs subagent-driven) is the owner's call at run
start — recorded here once made: **[filled in at execution]**.

