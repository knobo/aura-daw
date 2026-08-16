# AURA — Phase 4: the core redesign build

**Status: ACTIVE (2026-08-13).** Implements
[`CORE-REDESIGN-ROUND-2.md`](CORE-REDESIGN-ROUND-2.md) (ACCEPTED) and ADRs
0001–0007 (all Accepted). This document is the orchestration plan: it cuts
the round into six sub-plans, fixes their order and gates, and records the
rules that hold across all of them. Detailed, bite-sized task plans live in
`docs/superpowers/plans/` and are written **just-in-time** — each sub-plan
is authored when its predecessor lands, against the tree as it then exists,
not speculatively.

Convention note: phases 2 and 3 used ownership zones for parallel agents
(`PHASE2-PLAN.md`, `PHASE3-PLAN.md`). Phase 4 keeps the zone idea but the
zones are *sequenced*, because this phase rebuilds the foundation the zones
would otherwise share.

---

## The six sub-plans

| # | Sub-plan | Implements | Depends on |
|---|---|---|---|
| **A** | Session + the transaction channel | round-2 §4.1–§4.4, §4.6 (Session over Store+MidiStore, `transact`/`Tx`, ops with inverses, actor/run, effect descriptors, revision, op-format v1) | — |
| **B** | Identity groundwork | §2 (typed id families, `note_id` + persisted watermark, source-keyed assets/cache, derived slots + versioned ParamTable/meter routing, MAX_TRACKS removal) | A (ops address ids) |
| **C** | Time + project v3 | §3 (`Ticks`/`Samples` newtypes, supertick tempo, meterMap, section table, steady_time, v2→v3 migration, wire/frontend bijection) | A (tempo edits are ops) |
| **D** | Content/placement split | §5 (`ContentId`, placements with `LaneId`, default lanes) — **ships inside C's v3 migration**: one format bump, not two | C (same migration) |
| **E** | The side-channel totality | §4.5 (all 14+ paths through the channel, engine-thread inversion, gesture begin/end IPC, `clip.move` op + command, id-preserving piano roll, mutating readers become pure, remaining three stores into Session) | A, B, C/D |
| **F** | History storage | §6 (COW B-tree session, version-graph retention, replay-only nodes at 64 KB, placement-offset routing, janitor thread, budget/eviction) | A; informs E's undo exposure |

Order of execution: **A → B → C+D → E → F**, with F's tree buildable as a
standalone module (property tests against `benches/bulkbench`'s reference
behavior) any time after A. E is last on purpose: it is the totality
migration, and its exit gate is the one that turns the op log on.

## Gates (from round-2 §7 — the definition of done)

- **Gate A:** property tests 1 (undo round-trip), 3 (atomicity) and 5
  (attribution) pass against the A-slice ops; nesting is
  compile-prevented in-tree and runtime-trapped across the handle.
- **Gate B:** test 2 (delete-then-undo preserves identity and inbound
  references) passes; slot aliasing test (delete→add→old graph still
  playing) passes; meter blocks carry graph generation.
- **Gate C/D:** test 6 (section-table bound < 64 samples), test 7 (tempo
  round-trip + lossless v2→v3 against a fixture corpus, including
  meterMap and the placement/content split); frontend consumes the
  shipped section table (TS TempoMap deleted).
- **Gate E:** the side-channel inventory inside the channel is **total**
  (checked against dossier 10 §2.3 + review additions, all 14+ paths);
  test 4 (Figma invariant — reads mutate nothing) passes; **only then
  does the op log/journal turn on.** This gate is the round's whole
  point; it does not get negotiated down.
  (Plan F, 2026-08-14, marked correction M-5): the Figma invariant
  replays ops through its *own* commits. The shipped Ctrl+Z path is
  covered by `tests/journal_and_history.rs`.
- **Gate F:** delete-then-undo rides version retention; replay-only
  nodes reproduce byte-identical state (seeded PRNG, minted-id
  payloads); eviction respects the floor/ceiling; janitor keeps drops
  off-RT (test with `assert_no_alloc`, `disable_release` OFF).

## Rules that bind every sub-plan

1. **The op log stays dark until Gate E.** No journal file is written, no
   undo UI is exposed, before the inventory is total. (ADR 0003.)
2. **Thin renderer** (ADR 0006, owner-accepted): no new authoritative
   state, business logic, or time math lands frontend-side; hot rendering
   goes behind the texture-targeting renderer interface. Any frontend task
   in these plans is renderer/chrome work or op emission — nothing else.
3. **Frozen names stay frozen.** Command *names* and event names remain;
   bodies become wrappers over the channel. New commands are additive.
4. **Evidence policy** (ADR 0007): performance claims cite `benches/`;
   corrections to any doc are marked, never silent.
5. **Prepare-outside/commit-inside** for anything that does I/O; blocking
   engine round-trips inside a transaction are banned (round-2 §4.2/§4.4).
6. **Every sub-plan ends green:** `cargo test --manifest-path
   src-tauri/Cargo.toml` (277 baseline) and `npm test` (17 baseline) plus
   its own new tests; counts updated in README/CONTRIBUTING per the
   dated-count convention.

## Written plans

- **A:** [`docs/superpowers/plans/2026-08-13-plan-a-session-transaction-channel.md`](superpowers/plans/2026-08-13-plan-a-session-transaction-channel.md) — executed.
- **B:** [`docs/superpowers/plans/2026-08-13-plan-b-identity-groundwork.md`](superpowers/plans/2026-08-13-plan-b-identity-groundwork.md) — executed.
- **C+D:** [`docs/superpowers/plans/2026-08-14-plan-c-d-time-project-v3.md`](superpowers/plans/2026-08-14-plan-c-d-time-project-v3.md) — executed.
- E–F: authored when their predecessor lands (just-in-time, against the
  then-current tree). Each follows the same skill template as A.

## Plan A handoff (2026-08-13, post whole-branch review)

Whole-branch review of Plan A found round-2 §4.1's clause "the graph
rebuild reads an immutable snapshot rather than holding the Session lock"
is **not** delivered: `engine::rebuild` (`src-tauri/src/audio/engine.rs`)
still takes `self.session.lock()` and holds it for the entire graph build
(track/clip walk, RT-clip assembly, live-node binding). This is a widened
lock footprint versus pre-merge — every command now blocks on that same
merged `Session` lock during a rebuild, where before the split stores gave
some commands a narrower lock to contend with. Plan A's own written plan
never named this clause on an exclusion list, so per ADR 0007
(no-silent-corrections) it is recorded here explicitly rather than left
implicit: **snapshot-based rebuild is deferred, not delivered, by Plan A.**
It requires Plan F's copy-on-write history store (§6) to give `rebuild` an
immutable view it can read without holding the live document lock; until
Plan F lands, rebuild latency (proportional to track/clip count) sits
directly in the critical path of every command that touches the session.

The following are binding carry-forwards for later plans, recorded here
so they aren't lost between sub-plan hand-offs:

- Plan E first item: `with_synced_store`/`apply_hum_clip` hold the
  document lock across midi disk I/O — move persistence outside the lock.
- Before Gate E turns the log on: `fold_ops` coalesces Sets across
  intervening structural ops — harmless dark, wrong for replay; constrain
  it before `Committed.ops` is ever persisted.
- Plan B: add the duplicate-id guard on TrackAdd (rollback removes-by-id
  can mis-target); make clip restore positional or define canonical clip
  order (gate test 2 will fail on interleaved clip vecs otherwise); delete
  dead `ops::apply_track_mix`; add the clamp round-trip test (999→24).
- Standing caveat: `transact` has no panic rollback — **LIFTED (Plan F
  Task 8)**. Closures must still not panic (ruling F-3: containment, not
  license).
- Op wire form: `ObjectRef` ids are raw Strings; typed id families must
  serialize transparently or bump OP_FORMAT_VERSION.
- Clip carriage is asymmetric by design: `TrackAdd.clips` authoritative,
  `TrackRemove.clips` advisory (store truth wins).

## Plan B handoff (2026-08-14, post whole-branch review)

Plan B (identity groundwork, round-2 §2 + ADR 0001) is **IMPLEMENTED**
(`a31104f..a70bf67`): typed id families (`TrackId`/`ClipId`/`ContentId`/
`SourceId`/`NoteId`) end to end through `ObjectRef` and the op wire form
(`OP_FORMAT_VERSION` unchanged — transparent serialization held); persisted
note identity (`note_id` + per-clip `next_note_id` watermark, AMEV
columnMask-honest reader); source-keyed asset naming and decode cache;
`Store`-owned slots deleted in favor of per-graph derived slots
(`derive_slots`) with a `GraphTables`/`SharedGraphTables` publish under the
session lock (round-2 §2.4, O-13's aliasing window closed by construction,
not by discipline); meter blocks stamped with the graph generation and
folded through `GenerationMaps`; `MAX_TRACKS` removed, chunked meter blocks
replace the fixed-array/u64-mask assumption. Gate B green: delete-then-undo
identity property, the slot-aliasing pin, and the meter-generation pin all
pass (`src-tauri/tests/identity_properties.rs`). Suites:
**346 backend + 80 frontend**, all green, counts dated in
README/CONTRIBUTING.

The following are binding carry-forwards for later plans, recorded here so
they aren't lost between sub-plan hand-offs:

- **Plan E:** the id-preserving piano roll gesture path
  (`midi_set_notes` still replaces arrays wholesale; ids churn on every
  replace — the *allocator/keep-rule* is correct per Task 3/round-2 §2.1
  (`assign_incoming_note_ids` in `src-tauri/src/midi/mod.rs` keeps an
  incoming id iff it is non-zero, unique in the payload, and currently
  live), but the frontend's `MidiNote` TS type (`src/lib/types/ipc.ts`)
  carries no `noteId` field at all, so every gesture round-trip mints
  fresh ids in practice — wiring the id through the wire type and gesture
  path so preservation is actually observable is Plan E's job). Midi
  mutations still bypass the transaction channel entirely; note-id
  allocation is lock-serialized (correct, never reused) but not yet
  op-attributed — no `Op` records a note mutation, so it carries no actor/
  run and produces no inverse.
- **Plan C/D:** `next_note_id` moves from `MidiClip` to the content object
  in the v3 split, semantics unchanged (still a persisted, monotonic,
  never-reset watermark); `ContentId` is declared (Task 2) but unpopulated
  — nothing mints one yet. Round-2 §2.1's split/merge/copy rules bind the
  content ops when they exist and not before: split keeps both halves' ids
  and records the partition, merge remints the absorbed side, copy mints
  fresh. No such operation exists yet; C/D's content-op work is bound by
  these rules from the day it lands, not retrofitted after.
- **Ledgered, not taken:** the waveform pyramid cache is still keyed by
  clip id, not source id — a dedup-by-source opportunity Plan B's
  source-keyed decode cache (Task 4) does NOT extend to the pyramid cache;
  left for whichever later round touches waveform rendering. The
  missing-pyramid rebuild path re-reads the source file's raw samples
  (fix commit `c0a2bfc`; see `AudioEngine::ensure_loaded`,
  `src-tauri/src/audio/engine.rs` ~854-890) rather than building from the
  engine-rate-resampled decode cache, so pyramids are always
  source-rate-correct per the AWTF tile protocol.
  `GraphTables` under a `parking_lot::Mutex` is fine at today's track
  counts and rebuild rates — revisit only with a measurement (round-2
  §10.3's rule: no speculative lock-free rewrite without a bench showing
  contention). Plan E re-adds a `GenerationMaps` resolve accessor if/when
  its per-op attribution needs one. `create_project` and
  `save_project_as` (`src-tauri/src/control/mod.rs`) resetting
  `session.midi.dirty` is RESOLVED via the shared `adopt_midi_dir` helper
  (fix commit `c0a2bfc`; `src-tauri/src/control/mod.rs` ~145).
- **Coverage note:** `channel_properties.rs`'s clip-extension property
  (Gate A) is a same-track-contiguous oracle; it cannot independently
  catch positional restore bugs across interleaved clip vecs. Dedicated
  coverage for that lives in `src-tauri/tests/identity_properties.rs`
  instead (documented in its module comment) — this is by design, not a
  gap to backfill in `channel_properties.rs`.
- **Standing:** snapshot-rebuild deferral unchanged from the Plan A
  handoff above — `engine::rebuild` still holds the session lock for the
  whole graph build; Plan B's per-graph `GraphTables` changes what gets
  published, not when the lock is held. Still requires Plan F's
  copy-on-write history store. No panic rollback in `transact` (Plan F);
  transaction closures must stay non-panicking. Split/merge/copy remint
  rules (round-2 §2.1) bind future content ops from the day they land —
  see the Plan C/D item above; repeated here because it binds Plan E's
  content-adjacent work too, not only C/D's.

## Plan C/D handoff (2026-08-14, solo session, own-review)

Plan C+D (time + project v3, round-2 §3 + §5, ADRs 0002 + 0004) is
**IMPLEMENTED** (`57acfa9..9aee08a`, 10 tasks, one v2→v3 format bump):
`Ticks`/`Samples` newtypes with no cross-domain `Ord` (§3.1); integer-
period supertick tempo, quantized once at entry, exact thereafter (§3.3,
O-9); a persisted meter map defaulting to `[{0,4,4}]`, fixing the active
4/4-clobber data-loss bug (§3.3, O-10); a precomputed section table whose
ramp subdivision is property-tested to a **<64-sample bound** against the
exact closed-form bijection (§3.4 — **Gate C/D test 6, green**); the
v2→v3 project migration (schema gate widened `1..=2` → `1..=3`,
`project.json.v2.bak` mirroring the v1→v2 backup chain, a fixture corpus
under `src-tauri/tests/fixtures/project_v2/`); the content/placement
identity split (`ContentId`/`LaneId` populated, ADR 0004) — full for MIDI
(`content[]`/`placements[]`/`lanes[]` JSON arrays), addressing-only for
audio (scope ruling below); and the frontend's shipped-section-table
consumption (`src/lib/sectionTable.ts` is now the one bijection
implementation left client-side — the TS `TempoMap` duplicate and three
independent piecewise re-derivations are gone). **Gate C/D test 7
(lossless v2→v3, meterMap + placement/content split) is green for
tempo/meter/MIDI; audio's split is addressing-only, per the scope ruling.**
Suites: **372 backend + 85 frontend**, all green, counts dated in
README/CONTRIBUTING.

Executed solo (no subagent dispatch — token economy, the owner's binding
session constraint for this run), TDD, one commit per task, foreground
`timeout`-guarded test gates, self-review standing in for the missing
external reviewer. Plan document:
`docs/superpowers/plans/2026-08-14-plan-c-d-time-project-v3.md`; SDD
ledger (gitignored, per the established convention): `.superpowers/sdd/
plan-cd/progress.md`.

Three scope rulings were made explicitly, before implementation, and are
binding carry-forwards — recorded here so they aren't lost between
sub-plan hand-offs, per ADR 0007 (corrections marked, never silent):

- **Audio content/placement split is addressing-only.** MIDI clips get the
  full round-2 §5 split (`content[]`/`placements[]`/`lanes[]` JSON arrays,
  `ContentId`/`LaneId` populated, deterministic minting on migration).
  Audio clips (`audio::types::Clip`) get real, populated `content_id`/
  `lane_id` fields — addressing is genuine, `LaneId::default_for_track` is
  the SAME function both domains use, so a track's default lane is one id
  regardless of which domain's clip asks — but the JSON stays a single
  clip row; no `content[]`/`placements[]` array split for audio. Audio's
  higher consumer count (recorder, engine RT graph walk, waveform, offline
  bounce) and MIDI's round-2-stated priority ("instancing arrives free for
  MIDI; for audio it is a byproduct") made this the responsible cut for a
  solo session with no reviewer backup. No format bump is needed to finish
  the split later — the fields to key off already exist.
- **No Rust-level `Content`/`Placement` struct replaces `MidiClip`/`Clip`
  at runtime.** `Store`/`MidiStore` still hold `Clip`/`MidiClip`, now
  carrying `content_id`/`lane_id`; the `content`/`placements` JSON shape is
  assembled from and collapsed back into `MidiClip` at the `midi::persist`
  boundary only (`parse_clips_v3_native`/`save_into_project`). Editing
  shared content does not yet update every placement (ADR 0004's stated
  consequence is not yet true) because nothing mints two placements
  sharing one `ContentId` — no split/merge/copy content-op exists (same
  observation the Plan B handoff already recorded; still true, and now
  explicitly the thing round-2 §2.1's remint rules bind FROM, the day such
  an op lands).
- **`steady_time` (round-2 §3.5) and the per-block `Arc<TempoMap>` swap are
  OUT of this plan.** `clap_host.rs`'s per-node `self.steady: u64` counter
  is UNCHANGED — still resets to 0 whenever its node is re-created
  (instrument rebind, sample-rate change, a track leaving and re-entering
  the live set), the exact hazard round-2 §3.5 describes. This is RT
  engine-thread wiring (replacing a per-node counter with one engine-
  global, threaded through the `RtNode` trait used by 9+ node types) — a
  different risk class from this plan's pure/compute and persistence work,
  and not required by Gate C/D's own test list. Bound to Plan E, which
  already inventories engine-thread work.

Other carry-forwards, not new rulings but confirmed still true:

- MIDI content/placement work is bound by round-2 §2.1's split/merge/copy
  remint rules (split keeps both halves' ids and records the partition,
  merge remints the absorbed side, copy mints fresh) from the day such an
  op lands — the Plan B handoff named this bound for "C/D's content-op
  work"; C/D itself minted `ContentId`/`LaneId` but never split/merged/
  copied one, so the binding passes forward to Plan E (or whichever round
  first adds a content-editing gesture) unconsumed.
- `docs/ipc-schemas/midi-clip.schema.json` / `project-v2.schema.json` are
  **not yet updated** for the v3 content/placement/lane shape. Checked: no
  Rust test references `docs/ipc-schemas/` (grep, this session), so it's
  non-blocking for Gate C/D — but the files are stale documentation now,
  and should be brought current before anyone treats them as the wire
  format's source of truth.
- `project.svelte.ts`'s `samplesPerBeat`/`samplesPerBar` (used for grid
  snapping in `App.svelte`/`ClipView.svelte`/`Timeline.svelte`/
  `view.svelte.ts`) is round-2 §3.6's THIRD independent bijection, still
  flat-tempo. The other two (`midi.svelte.ts`, `demo.ts`) now consume the
  shipped section table; this one needs a position-aware refactor across
  four call sites — out of Task 9's reach, flagged inline in
  `project.svelte.ts` and here.
- Standing from Plan A/B: snapshot-rebuild deferral **LIFTED (Plan F
  Task 6)**; no panic rollback **LIFTED (Plan F Task 8)**. Historical
  remainder of this bullet (`fold_ops` before Gate E; the op log stays
  dark; tempo/meter/content still outside the A-slice channel) was
  Plan C/D's state — Plan E closed the channel, the log is ON.

## Plan E handoff (2026-08-14, subagent-driven session, external review layer)

Plan E (the side-channel totality, round-2 §4.5, ADRs 0003 + 0006) is
**IMPLEMENTED and MERGED to `main`** — the owner ordered the merge; `main`
carries squash commit `27911d8` ("Plan E — the side-channel totality; Gate
E closed, op log ON"). Branch `plan-e-side-channels` is kept (not deleted)
so every commit SHA cited below still resolves. A post-merge whole-branch
review is running as a follow-up; this handoff is not gated on its
outcome. (commit range `ac65b76..531d790`, 51 commits, including 7
merges — 6 side-branch task merges (`task-9-plugin-doc`,
`task-10-automation-doc`, `task-11-frontend-stems`, `task-12-transport`,
`task-15-noteid-schemas`, `task-16-steady-time`) plus one `origin/main`
merge (`f886306`, pulling in PR #9 hscrollbars / #10 zoom-pref / #11
note-ops mid-flight)). **Gate E is CLOSED**: the Figma invariant (§7 test 4
— reads mutate nothing, a scripted mixed session over every op family, undo
through new commits, a pure-reader sweep, redo, byte-identical canonical
snapshot) is green against the code exactly as Tasks 1-16 left it —
(Plan F, 2026-08-14, marked correction M-5: that undo is the *test's own*
commits, not the shipped Ctrl+Z path; `tests/journal_and_history.rs` is
what covers the product undo/redo commands) —
provably so, because the commit that adds the test, `73bb9a7`, touches
**zero production files** (`src-tauri/tests/figma_invariant.rs` only, 748
insertions). The 34-row side-channel inventory closes total, recorded in
`docs/SIDE-CHANNEL-INVENTORY.md` (landed form of the plan doc's re-derived
table, with three residual documented non-op writes R-1..R-3 and the
recorded replay limitations L-1..L-5 — see that doc, not repeated here;
L-1 was closed and L-4/L-5 added by the post-merge follow-up PR).
The op log is now **ON**: `journal.ndjson` (append-only NDJSON, no fsync)
plus in-memory undo/redo (bounded `Vec`, 200 entries, bottom-eviction),
wired to Ctrl+Z / Ctrl+Shift+Z in `src/App.svelte`. Suites: **501 backend +
174 frontend**, all green, counts dated in README/CONTRIBUTING
(2026-08-14).

**Execution note.** Subagent-driven (owner's explicit choice via
AskUserQuestion), with parallel isolated worktrees for Tasks 9, 10, 11, 12,
15, 16 once the mainline tasks (1-8, then 13-14, then 17) stopped
contending for the same files — a fresh implementer per task, a task
review after each, and scoped re-reviews on every fix round. Every one of
the 18 tasks cleared review in **0 or 1 fix rounds** (no task needed a
second round). Two **Critical** findings were caught by the review layer,
both in Task 9 (plugin rows into Session, the widest diff in the plan — 55
plugin tests churned): a reentrant session-lock deadlock in
`adopt_open_project` (invisible to tests because the registered global was
`None` there) and `PluginSetState` not actually durable (the persist
ladder's `file.exists()` check beat the new authoritative `pending_state`,
so a second patch load never persisted). Both were fixed in Task 9's one
fix round. The review layer also caught real concurrency bugs outside
Critical severity worth citing as evidence it earned its cost: Task 13's
review (opus) found the deadlock-audit comment named the wrong invariant
(the real rule is "no thread may hold the session lock across an
`EngineHandle::request`") and that `finalize` bundled a `TransportState`
`Set` into the non-transient tx, landing it in the undo surface; Task 14's
review found a TOCTOU between a gesture-open check, a transient commit, and
`fold_in` that could silently lose a fold under a concurrent `gesture_end`.
Both are recorded as mid-flight rulings below. SDD ledger (gitignored, per
the established convention): `.superpowers/sdd/2026-08-14-plan-e-side-channel-totality/progress.md`;
plan document:
`docs/superpowers/plans/2026-08-14-plan-e-side-channel-totality.md`.

### Scope rulings (carried verbatim from the plan doc, per ADR 0007)

1. **Op kind naming keeps the landed style** (`"set"`, `"trackAdd"`, …).
   D-03's draft envelope schema prescribed dotted kinds (`clip.move`); the
   three landed kinds don't dot, no journal has ever been written, and
   renaming landed kinds would churn Plan A/B tests for zero replay
   benefit. The envelope schema's `kind` pattern is corrected to match the
   implementation, as a marked correction. Round-2's "`clip.move` op"
   therefore lands as `Op::Set{object: Clip, path: TimelineStartSamples}` +
   the additive `move_clip` command — the capability round-2 demands,
   under the house naming.
2. **Transport ops are transient**: through the channel, never journaled,
   never undoable. `transport.state`/loop window/stop-at-end writes become
   `Op::Set` on `ObjectRef::Transport` paths so they are attributed,
   atomic, and single-writer, but they carry `TxMeta.transient = true`: no
   history entry, no journal line (no DAW undoes play/stop).
   `transport_seek` stays a pure RT atomic (position is engine state, not
   document state). `sample_rate` writeback is likewise transient (a
   device property mirrored into the document for save).
3. **Undo-round-trip oracle masks the note-id watermark.** ADR 0001 makes
   `next_note_id` monotonic and never-rewinding; an undo of `MidiSetNotes`
   restores the notes but NOT the watermark (ids are never reused). The
   byte-identical snapshot oracle (Gate A test 1, and Gate E's Figma
   invariant) normalizes `next_note_id` to 0 in both snapshots before
   comparing, and separately asserts it never decreases. This is the test
   honoring the ADR, not the test being weakened.
4. **Epoch boundaries are not ops** (round-2 §4.5 carve-out, verbatim):
   `open_project` is a log epoch boundary (document swap, history root);
   `save_project`/`save_project_as` are snapshot marks; `create_project`
   and the engine's `ensure_project` are epoch boundaries too (document
   birth). They become *sanctioned* epoch functions: single-lock swap
   discipline, eager adopt of midi/plugins/automation, history + redo
   stack cleared, journal rotated — but they never appear as ops in the
   log.
5. **`SamplerBank` stays outside `Session`.** At HEAD the bank has no
   document half: loaded instruments are never persisted
   (`load_into_registered_bank` writes only the process-global bank;
   nothing lands in project.json), and the bank is exactly the "live
   plugin host handles / loaded voice data" class §4.1 keeps outside the
   document. The document half of instrument assignment is
   `TrackState.instrument_id`, which DOES become an op (Task 3). If a
   later round persists bank contents, that round moves them into
   Session.
6. **No MCP MIDI tools in this plan.** The MCP tool roster is frozen
   (ARCHITECTURE §12.2) and PHASE4-PLAN's E row doesn't include it. The
   channel work makes future MIDI tools one-liners over ControlPlane, but
   adding them is not Gate E material.
7. **Track reorder gets no op.** The only caller of the frontend-only
   `placeTrackAfter` is the demo stems path, which Task 11 deletes;
   backend append order is the product's track order today. A
   `track.move` op is deferred to whichever round adds a real reorder
   gesture — recorded, not silent.
8. **`midi_export_file` writes the exported `.mid` file but stays
   classified a read**: it reads the document and writes *outside* it (an
   export artifact, like `export_song`). What made it a §7-test-4
   violation was the lazy resync, which Task 6 removes.
9. **History is in-memory in this plan; Plan F owns persistence/retention.**
   Task 17's undo/redo stack is a bounded `Vec` of committed batches
   (cleared at epochs); the journal file is append-only NDJSON. Plan F
   (history storage, §6) replaces the storage substrate; the *exposure*
   (undo/redo commands, journal format) is Plan E's per PHASE4-PLAN ("F …
   informs E's undo exposure").

### Preflight conflict-scan rulings (decided before Task 1 started)

- **`Op::MidiSetNotes` addresses by a `ClipId` field, not `ObjectRef`.**
  `fold_ops` only groups `Op::Set`, so two `MidiSetNotes` in ONE tx do not
  fold; coalescing for notes happens at the gesture (Task 14) and
  history-350ms (Task 17) layers instead, as the plan's §4.4 note says.
  Cost if wrong: an op-vocabulary asymmetry normalized in a later round;
  replay unaffected.
- **Task 9's `PluginInstanceRow` names whatever the existing registry row
  struct is** (`plugins/mod.rs`) — the implementer reuses the landed type
  rather than inventing a new one. Cost if wrong: a rename diff.
- **Task 12's `commit_with(emit_project_changed=false)` is an accepted
  wart**, noted before implementation: the frozen `project://changed`
  contract must not start firing per play/stop, so transport commits
  suppress the emit rather than the frozen event growing a new firing
  condition.

### Mid-flight rulings (from the SDD ledger, each with its one-line why)

- **Task 12's Play/Stop fixed twice**: (1) `Play`'s check-then-act TOCTOU
  (a recording-downgrade window across two lock acquisitions) is closed
  by re-checking state INSIDE the commit closure via a `Tx` read accessor
  (the same shape as Task 7's contract, so the later merge deduped it);
  (2) `Stop`'s ordering is restored to send `StopRecording` FIRST, then
  commit the `stopped` `Set` — the dispatch text had mandated the reverse
  order while also saying "mirror today's ordering," a self-contradiction
  the review caught; the fix preserves the old visibility window instead
  of exposing "stopped" while finalize is still running.
- **`ContentLengthTicks == 0` must be rejected on the command surface**
  (Task 5 ruling, mandatory for Task 7's dispatch): Task 5's validation
  was weaker than `apply_clip_bounds`'s reject; carried forward so the
  rewired `midi_set_clip_bounds` closes the gap rather than shipping a
  silent weakening.
- **Multi-clip selection/paste is deliberately deferred until Gate E
  closes** (owner feature request, captured mid-Plan-E into
  `docs/backlog/multi-clip-selection-and-paste.md`): every gesture in
  that request maps 1:1 onto the finished channel's primitives (gesture
  batches, multi-op transactions, one undo entry per gesture); building
  it pre-gate would have recreated the side-channels this plan removes.
  This is why it ships as Track C in `next-prompt.md` rather than as a
  Plan E task.
- **Task 15 revised after PR #11** (noteId optional convention, not
  required-0-sentinel): PR #11 landed mid-flight and already added an
  optional `noteId` to the TS `MidiNote` type plus strip-on-copy in
  `note-ops.ts` — Task 15 adopted that convention instead of inventing a
  parallel one, shrinking its own scope to `generation.svelte.ts` infill
  rules, the v3 schema, and the `ipc.ts` `Project` type refresh.
- **`split_stems_for_clip` landed as an additive command** (Task 11
  ruling): `import_audio_clip_split_stems` always re-imported (duplicate
  clip/ghost track) with no existing-clip stem path; an additive command
  resolving the existing clip + absolute source path and submitting the
  demucs job anchored on that clip avoided a re-import while staying
  inside binding rule 3 ("new commands are additive").
- **`finalize` splits into a `ClipAdds` tx + a separate transient state
  `Set`** (Task 13 ruling, overriding the task's own "one tx" wording):
  bundling the `TransportState` `Set` into the non-transient clip
  registration tx would have landed transport state in the undo surface;
  atomicity between clip registration and the state mirror was judged not
  load-bearing, so splitting them keeps the undo surface honest at the
  cost of two revs per finalize instead of one.
- **Gesture lock order: gesture before session, always** (Task 14
  ruling): a TOCTOU between a gesture-open check, a transient commit, and
  `fold_in` in `set_track_mix` could let a concurrent `gesture_end` (a
  fire-and-forget frontend invoke) silently lose the fold; holding the
  gesture lock across check+commit+fold, with lock order gesture-then-
  session everywhere, closes the race by construction and is documented
  at `GestureState`.
- **`ensure_project_epoch` deliberately does NOT adopt+Rebuild** (Task 6
  ruling): adding adopt/rebuild parity would have been the "obvious" fix
  to the review's finding, but behavior parity with the engine's existing
  `ensure_project` is load-bearing for Task 13; the fix was an explicit
  justifying comment + a marker at the actual document-swap site instead
  of a behavior change. Cost if wrong: Task 13 would discover a
  standalone caller needing rebuild after all — cheap to add then, and it
  didn't happen.
- **`execute_persist` gained an epoch guard** (Task 7 fix round 1): the
  original design re-locked the session after `commit` returned to do
  disk I/O; an epoch swap (project open/save-as) landing in that window
  would silently drop the edit being persisted — a pre-existing data-loss
  class since Task 2. Fix: an epoch counter on `Session`, bumped by epoch
  functions, captured inside the `transact` lock at commit time;
  `execute_persist` skips and warns on a mismatch instead of writing
  stale data over fresh.
- **Net-noop batches record nothing** (Task 17 fix round 1, finding I-1):
  `fold_ops` legitimately drops a net-noop `Set` group (a `move_clip` back
  to the sample a clip already sits on, a gain write of the current
  value), producing an empty `Committed`. Without a guard this created a
  phantom undo step and an `"ops":[]` journal line; the fix drops the
  record inside `HistoryLog::record_commit` itself when the ops list is
  empty.

### Standing carry-forwards for Plan F+

- **R-3**: `try_seed_zyn_demo_instruments` pushes three plugin instance
  rows into `session.plugins.instances` directly, outside any op, before
  the demo's channel transaction runs (no track references those ids yet,
  so nothing user-visible is undoable). Recommended for Plan F: fold this
  into the seed transaction as `Op::PluginAdd`s.
- **L-1**: `Op::PluginRemove.params` is captured on the op (self-
  describing, additive field) but undo restores the param mirror from the
  in-memory PARKED copy, not the op field, because `Op::PluginAdd` has no
  params slot. Fine in-process; on a cold journal replay (no parked
  mirror) an undo-of-remove would restore the row without its params.
  Needs an op-format change — Plan F territory.
- **L-2**: `Op::MidiSetNotes`'s `noteId: 0` entries are mint sentinels
  that RE-MINT on replay. Invisible whenever a redo of `Op::MidiClipAdd`
  restores the clip row (watermark included) before the notes op
  replays — which is what the Figma invariant exercises. A redo that does
  NOT re-create the clip re-mints from the current watermark, so re-added
  notes get fresh ids: ADR 0001 behaving as designed, the same asymmetry
  scope ruling 3 masks for the watermark itself.
- **The journal is write-only until Plan F.** **LIFTED (Plan F Task 9).**
  `control/replay.rs` reads `journal.ndjson` (sort by `(epoch, rev)`,
  save-mark tail, open-time detection). No auto-apply (ruling F-8).
- **Undo is bounded at 200 entries, bottom-eviction.** Unchanged
  (ruling F-2). Storage substrate is now the version graph; exposure
  is still the 200-deep stack.
- **Snapshot-rebuild deferral STILL standing.** **LIFTED (Plan F Task 6).**
  `rebuild` assembly and `ensure_loaded` read the published
  `SessionSnapshot`; the remaining `session.lock()` sites are short
  (param/slot compile, meter fold, transport snapshot, recording
  resolution). See the Plan F handoff and the inventory grep-gate.
- **No panic rollback in `transact`.** **LIFTED (Plan F Task 8).**
  `catch_unwind` + restore from the pre-tx published image.
- **Host-GUI plugin tweaks are uncaptured** until a host-state poll
  lands: a user twiddling a plugin's own native GUI (not AURA's param
  UI) produces no op today — `Op::Set{Plugin, Param}` only fires through
  AURA's control surface. Recorded as a gap, not a defect of this plan.
- **PDC (plugin delay compensation) before sends ship** — round-2's
  standing rule, relevant to the FX/bus "Plan G" candidate
  (`docs/backlog/00-ROADMAP-real-alternative.md`'s Tier-1 insert-FX item):
  whichever round adds sends/busses must have PDC first.
- **The M-3 redo invariant**: transient writes must never touch a
  document field an entry's `ops` can address, or a pending redo silently
  lands on a different state than the entry recorded. Today's transient
  writers stay clear by construction (transport `Set`s address
  `ObjectRef::Transport` only; mid-gesture folds are superseded by the
  gesture batch that closes over them). **→ now CHECKED in PR #18**
  (whole-branch review M-3): `debug_assert_transient_invariant` in the
  commit path fails a debug build on any transient batch addressing
  something other than `ObjectRef::Transport`, unless it is a mid-gesture
  fold. Still a rule about what may ever be marked transient — but one
  with teeth in debug builds. Binding on any future transient-tx addition.
- **`fold_ops` never folds away non-`Set` ops.** A no-op lane delete (or
  any structural op with no net effect) still produces a journal line —
  noted near `fold_ops` during Task 10's review as future noise once
  history persists; Plan F's retention design should account for it.

### Deferred-minors roll-up

Every `minor (deferred):` line the ledger recorded, triaged by the final
whole-branch review (Task 17's gate review) and left open by design —
collected here so a future round doesn't have to re-mine the ledger:

- Report/test-count arithmetic presentation (Task 1, report-only).
- `execute_persist` re-acquiring the session lock to flip `midi.dirty` — a
  theoretical stomp window (Task 2).
- `persist.project`'s branch lacked a dedicated unit test; covered
  end-to-end by Task 7 instead (Task 2).
- `set_track_instrument`'s validate-then-mutate is two lock acquisitions
  (TOCTOU window, benign under serial command execution) (Task 3).
- The frontend catch in `move_clip`'s commit path logs at `console.error`
  vs. `reload()`'s `console.warn` — arguably the more correct level, left
  as-is (Task 4).
- No targeted meter-only sortedness test for `TempoSet`; the fuzzer's
  meter generator is always single-entry (Task 5).
- Deletion-then-undo note-identity is covered by one unit test, not the
  fuzzer — correct per the oracle-masking ruling, not a coverage gap
  (Task 5).
- `file_stem` naming-coupling comment; `automation.rs` carried a sibling
  lazy-resync until Task 10 deleted it (Task 6).
- `sidecarSplitStems` is now dead from the UI (`tauri.ts:116/389` + the
  backend impls) — cleanup ticket for a later round (Task 11).
- No no-op-lane-delete short-circuit — still triggers a `persist.automation`
  write pass; see the `fold_ops` non-`Set` note above (Task 10).
- Dirty-but-nothing-pending branch never clears the dirty flag (redundant
  no-op persists); `Op::PluginRemove.params` captured-but-unread on
  undo — see L-1 above (Task 9). **→ the `params` half is CLOSED in
  PR #18**: `apply_raw` now seeds the mirror from the op's own field when
  the in-memory one is absent, so a cold replay restores a plugin row with
  its params (L-1 closed). The dirty-flag half is still open.
- `plan_region_replacement`'s doc cites `replace_region_clips` but
  diverges on right-only-trim (`TrimRight` in place); the LoopJam mid-air
  race is code-inspected, not test-forced (Task 8). **→ the race is now
  TEST-FORCED in PR #18** (whole-branch review I-2, which found a real
  100 % CPU spin in exactly that code): a pending swap whose commit fails
  on every attempt, from a stopped transport, asserting both the bounded
  retries and the back-off. The doc divergence is still open.
- Deadlock-audit comment's five call-site line citations went stale by
  +129 lines after later edits — sites and the invariant itself stayed
  correct (Task 13). **→ CORRECTED in PR #18**, with a note that the
  audit's value is its navigability, so the numbers are re-checked
  whenever the file moves.
- `midi/synth.rs` test-literal compile fixup, touched despite the
  `midi/*` scope note — mechanical, zero behavior (Task 16).
- `ClapNode::reset()` not re-verified to leave `steady_fallback` alone —
  consistent with pre-change semantics, not re-audited (Task 16).
  **→ RESOLVED**: the final whole-branch review re-verified it (M-9) —
  `plugins/clap_host.rs`'s `reset` sets only `pending_all_off`, the
  fallback counter is untouched. Ledger item closed.
- A stale line-number citation in a doc comment (516 vs. 508);
  `tempoBpm` required-vs-conditional tension pre-existing in the v2
  schema (Task 15).

## Plan F handoff (2026-08-16, history storage, `plan-f-history`, PR #23)

Plan F (history storage — round-2 §6, ADR 0005) is **IMPLEMENTED** on
branch `plan-f-history` (**PR #23**). Plan document:
`docs/superpowers/plans/2026-08-14-plan-f-history-storage.md`. Tasks 1–13
landed on that branch (rebased onto `origin/main` including Track F).

**What landed.** A published `SessionSnapshot` (per-clip `Arc<MidiClip>`)
is captured inside `transact` and at every `// snapshot republish:` site.
`engine::rebuild` assembles from that image with the session lock released
(phase 2 still takes a short lock for param values / slots). A version
graph retains materialized images or replay-only ops at the measured 64 KB
line, with a janitor off-RT. `transact` contains panicking closures
(restore from the pre-tx image). The journal has a reader: `(epoch, rev)`
sort, save-mark tail, open-time *detection* only. Undo/redo is async,
epoch-guarded, gesture-auto-close, and serialized (`history_gate`). R-3
closed: demo Zyn bootstrap is `PluginAdd`/`PluginSetState` in the seed tx.
MIDI placement offsets (`transpose_semitones` / `velocity_offset`) are
`Set` ops applied at schedule time, never `MidiSetNotes`.

**Lifted carry-forwards** (from Plan A/E, with the task that lifted them):
snapshot rebuild (Task 6), panic rollback (Task 8), journal reader (Task 9),
R-3 (Task 10), L-4 (Tasks 9+11), L-5 (Task 8).

### Scope rulings (carried from the plan doc, per ADR 0007)

1. **F-1 — COW store at clip/row granularity.** The live document keeps
   its landed `Vec` shapes. Per-clip `Arc<MidiClip>` sharing is the
   version substrate. The within-clip summarising B-tree is deferred to
   the round that adds a note-delta op — that is the trigger.
2. **F-2 — User-visible undo depth does not change.** `UNDO_STACK_LIMIT`
   stays 200. Version graph: `VER_BUDGET_BYTES` = 512 MiB, `VER_STEPS_FLOOR`
   = 200.
3. **F-3 — Panic rollback is containment, not license.** `catch_unwind`;
   restore from the pre-tx published snapshot; closures must still not panic.
4. **F-4 — L-4's fix is ordered consumption, not a commit-sequence lock.**
   Reader sorts by `(epoch, rev)`; undo stack is rev-ordered. File order
   remains unordered by documented rule.
5. **F-5 — Journal reader requires `v == 2` on batch lines.** v1 lines are
   skipped with a warn (counted, never silent).
6. **F-6 — I-1 is option (b):** Save-As writes plugin/automation/modulation
   into the new dir. Option (a) (flush outgoing persist before every epoch
   swap) is deferred. Residual: `open_project_epoch` drops an in-flight
   persist for the project being closed (warn). Ctrl+S recovers.
7. **F-7 — Undo/redo auto-close an open gesture** rather than refuse.
8. **F-8 — Cold replay is detection + primitive + fidelity tests.** No
   auto-apply, no recovery UI. Torn tails are skipped, not trusted.
9. **F-9 — L-2 is benign for tail replay.** Minting is deterministic from
   the persisted watermark. Pinned by
   `a_cold_tail_replay_reproduces_the_crashed_sessions_document_byte_identically`.
10. **F-10 — Replay-only nodes store ops + inverses verbatim.** No landed
    op re-mints object ids on replay. Random ops do not exist yet; when
    they land, §6's seeded-PRNG constraint binds them.
11. **F-11 — The version graph is linear-by-rev within an epoch.** Undo
    and redo are forward commits; no DAG in this plan.
12. **F-12 — R-3 closes fully.** Demo Zyn bootstrap is ops inside the one
    seed transaction, including state blobs.

### New carry-forwards

- **(a) Live-document B-tree migration** — ruling F-1. Trigger: the round
  that adds a note-delta op (point edit stops being whole-clip replace).
- **(b) I-1 option-(a) residual** — ruling F-6. Epoch functions do not
  flush the outgoing document's pending persist before a swap.
- **(c) No auto-apply of journal tails** — ruling F-8. Detection only.
- **(d) Seeded-PRNG constraint** binds future random ops — ruling F-10.
- **(e) Version-graph product surface unbuilt on purpose** — no browse UI,
  no `history_stats` command. Substrate + tests only.

Also recorded from the review follow-up on this branch: `ChangeSet::from_ops`
flags modulation+automation on `TrackAdd`/`TrackRemove` and on
`Modulation*`/`AutomationClipSet`; `midi.dirty` clears only when the live
V3 snapshot still matches the bytes written; undo/redo is serialized;
a failed dirty flush skips the journal save mark.

### Flakes (already named; not new)

`midi_out` under parallel lib runs; `apply_hum_clip_commits_synchronously_and_announces_project_changed`
can see `rev + 2` when an engine sample-rate commit races the assertion.
Re-run in isolation before believing a failure.

## Track D handoff (2026-08-14, subagent-driven session, external review layer)

Track D (automation audible + lane UI) is **IMPLEMENTED** on branch
`automation-audible` (**PR #20**), cut from `origin/main` at `3340aa8`,
with `origin/main` merged in at the Task 5/6 boundary (`8b496e0`, pulling
in Track E's `a98d7ff`). Plan document:
`docs/superpowers/plans/2026-08-14-automation-audible.md`. SDD ledger
(gitignored): `.superpowers/sdd/2026-08-14-automation-audible/progress.md`.
Ten tasks, commits `9e7871f..HEAD`.

**What landed.** `engine::rebuild` finally reads `session.automation`: it
compiles track-gain lanes into a slot-indexed `RtGraph::gain_ramps` table
(attached BEFORE the graph is published, so RCU discipline holds) and
builds a `ParamAutomationDriver` that writes plugin params on the engine
control thread's ≤2 ms tick. The REBUILD PIN in `control/session.rs` is
resolved: `Op::AutomationSetLane`'s apply arm now sets
`effect.rebuild = true`. On the UI side, a timeline overlay lane draws,
drags and deletes points, every edit gesture-wrapped through the existing
`automation_set`; plugin-parameter lanes get a target picker. Gestures grew
a **persist deferral** — a knob drag or a lane drag is now one history
entry AND one `project.json` write instead of one of each per rAF batch.
Song export compiles the same track-gain ramps into the offline graph, so a
bounce follows the curves playback obeys (close-out task; see the
divergence below for the plugin-param half, which it does NOT).

**Per task**: T1 `061786b`+`d7ecf34` (I-3/R-4/M-6), T2 `7ef1f70`+`feec7e9`
(gesture persist deferral), T3 `bb20280` (knob drags, I-8), T4 `55bb370`
(lane edits fold), T5 `2a11ed0` (frontend store + M-3 re-pull), T6
`38abcc2` (lane UI), T7 `2755647` (plugin-param targets), T8
`12429d1`+`f33c15a` (mixer ramps), T9 `c30a830`+`10aeb12` (engine), T10
close-out (export automation + this section).

**Suites: 566 backend (537 lib + 29 integration) + 258 frontend, all
green, measured on `automation-audible` 2026-08-15.** Counts dated in
README/CONTRIBUTING. `main` moved during this track (Track E merged as
`a98d7ff`) and other tracks are in flight, so the count line is a known
cross-track merge-conflict point: whoever merges last re-measures rather
than picking a side.

**Execution note.** Subagent-driven (owner's choice at run start), a fresh
implementer per task, a task review after each, scoped re-reviews on every
fix round. Task 9 (the engine task) ran on opus, as did its reviews.
Four tasks needed one fix round each; none needed two. The review layer
caught one **Critical**: Task 8's `track_gain_ramp_scales_live_output_too`
was VACUOUS (a live track with an empty event list is silent whatever the
gain — the reviewer proved it by sabotaging the gain to ×999 and watching
the test still pass). It also promoted the export divergence below from
"suggested follow-up" to a required close-out fix. Both of Task 9's
judgement calls (re-assert over invalidation; batching without a
`clap_host` API change) were upheld on re-review AGAINST the reviewer's own
prior recommendation, after four sabotage mutations all went RED.

### Scope rulings (carried verbatim from the plan doc, per ADR 0007)

1. **Track-gain automation attaches at the MIXER's per-track gain stage,
   not by wrapping live nodes in `GainAutomatedNode`.** Three reasons, each
   decisive on its own: (a) NODE REUSE — live nodes come from
   `LiveNodeRegistry::resolve_with`, keyed so voice and plugin state
   SURVIVES rebuilds; wrapping means either a fresh node (a full plugin
   re-instantiation) on every point drag, or mutating a cached node's baked
   `events` while a published snapshot may still reference it, violating
   the `LiveNodeCell` safety contract. (b) AUDIO TRACKS —
   `GainAutomatedNode` wraps a `LiveInstrument`, so only MIDI tracks could
   ever be automated; "draw a volume curve on an audio track", the primary
   use, would be impossible. (c) COST — every lane edit would force a node
   rebuild and an audible discontinuity. The mixer seam has none of these:
   a slot-indexed `Vec<Option<Arc<Vec<AbsParamEvent>>>>` on `RtGraph`,
   versioned with the snapshot exactly as `ParamTable` is, swapped by the
   ordinary RCU graph publish.
2. **Plugin-parameter lanes are driven at the ENGINE CONTROL THREAD's tick
   (≤2 ms), not sample-accurately, and their writes go to the HOST only —
   never to the document.** Sample-accurate plugin-param automation needs a
   wait-free param ring on the plugin node, which is round-2 §8 node-graph
   territory. A host param write is a blocking round-trip and is banned on
   the callback path ([C1]). The writes bypass the document deliberately:
   automation OVERRIDES the stored knob value during playback; the document
   keeps what the user set. Routing them through the channel would either
   trip the M-3 debug assert (transient) or push an undo entry and a
   `project.json` write every 2 ms (non-transient). **Recorded
   consequence:** after playing an automated section the plugin's LIVE
   value can differ from the document's stored value until the next project
   load or user edit (recorded in `docs/SIDE-CHANNEL-INVENTORY.md`).
3. **I-3 lands as an epoch guard plus a recorded carve-out (R-4), NOT as an
   op.** `status` is a field `Op::PluginAdd` carries and addresses, so a
   TRANSIENT batch writing it trips `debug_assert_transient_invariant`, and
   a NON-transient batch would push a phantom undo entry for every plugin
   instantiate and every undo-of-a-remove. So: epoch guard (the same shape
   `execute_persist` uses), residual R-4, and `execute_host_forward` added
   to the grep-gate enumeration (that omission IS M-6).
4. **I-8's real fix is gesture-scoped PERSIST DEFERRAL, not gesture folding
   alone.** A transient commit still executes its full `EngineEffect`,
   persist included — so coalescing by itself leaves row 13's frequency
   claim exactly as wrong as the review found it. The commit folds AND its
   `PersistEffect` is deferred, accumulated on the open gesture, and
   executed exactly once at `close_gesture`.
5. **Automation lanes get a coalesce target WITHOUT a new
   `op::ObjectRef` variant.** `CoalesceKey` is `pub(crate)`, in memory
   only, never serialized — so its `object` field became an internal
   `CoalesceTarget` enum with an `AutomationLane(String)` arm.
   `op::ObjectRef`, which IS serialized into `journal.ndjson`, is
   untouched, so no `OP_FORMAT_VERSION` question arises.
6. **Track-gain lane values are LINEAR GAIN MULTIPLIERS in `[0.0, 1.0]`,
   applied on top of the fader.** 0.0 is silence, 1.0 is "whatever the
   fader says" — exactly `GainAutomatedNode`'s landed semantics. The
   alternative (automation REPLACING the fader, the fader following the
   curve) needs a fader-follows-automation UI mode and a read/write
   arbitration story; deferred, recorded. `TRACK_PARAM_GAIN = 0`.
7. **Lane drags preview locally and commit ONCE on pointerup**, mirroring
   `project.moveClip`/`commitClipMove` and PR #14's commit-on-release
   convention. The gesture bracket still opens on pointerdown, because one
   pointer interaction can produce more than one commit and because the
   gesture is what defers the persist (ruling 4). Making the ramp audible
   DURING the drag would need a graph rebuild per pointermove; not done.
8. **The automation lane renders as an in-lane OVERLAY on the track's
   existing timeline row, not as an added row.** `Timeline.svelte` keeps
   the left rail and the lane column in lockstep by a shared
   `--track-height`; a sub-row would desynchronise them and require a
   height negotiation through both columns. A dedicated, resizable
   automation row is a follow-up, recorded not silent.

### Non-goals (held, so the next round knows they were choices)

No sample-accurate plugin-param automation (ruling 2). No automation for
pan / mute / solo / send levels — track gain and plugin params only; new
built-in target ids are additive later. No curve shapes beyond linear
(bezier/hold/step would change the persisted point record). **No
automation recording (write/touch/latch modes.)** No snapshot-based
rebuild (Track A's). No `GainAutomatedNode` deletion — it stays, tested,
as the per-node ramp applier.

### Findings closed by this track

- **I-3 + M-6** → `061786b` (epoch guard on `execute_host_forward`'s
  writeback, residual R-4 recorded, grep-gate enumeration corrected).
- **I-8** (inventory row 13's per-knob `project.json` rewrite) →
  `7ef1f70`/`feec7e9`/`bb20280`; row 13's wording corrected in the same
  breath.
- **frontend M-3** (undo/redo re-pull missing automation and plugin
  panels) → `2a11ed0`.

### KNOWN DIVERGENCE: export ignores PLUGIN-PARAM automation

Track-gain lanes are compiled into the offline graph (close-out task, two
tests), so a bounce follows them exactly. **Plugin-param lanes are not
evaluated by a bounce at all**, and the value the export captures is
whatever the live host instance holds — most recently, whatever the last
playthrough's driver left there. The same project can therefore bounce
differently after a playthrough than from a fresh launch.

Not fixed here, deliberately: there is exactly ONE copy of a plugin
instance in the process, and the only way to set a param is a host
round-trip. Driving params during a bounce would write the export's
automation into the user's live plugin — moving the knobs under an open
panel, and fighting the engine's own driver if the transport is rolling.
A correct fix needs the bounce to own PRIVATE plugin instances (a second
instantiation per automated instance, seeded from the live one's
`save_state`, driven per render block, disposed at the end), i.e. a
`clap_host`/`lv2_host` API addition plus an offline driver tick — round-2
§8's plugin-node work. Recorded at `audio::offline::build_graph` and in
`docs/SIDE-CHANNEL-INVENTORY.md`.

### Deferrals created by this track

- **Sample-accurate plugin-param automation** (ruling 2) — round-2 §8.
- **Non-blocking CLAP param path.** `clap_host::set_params` is
  `plugin_main().run(…)`, a BLOCKING round-trip; `lv2_host::set_params`
  already posts, so only CLAP blocks. Task 9 batched the writes (one call
  per automated CLAP instance per tick, ~3000 → ~500 round-trips/s at a
  full-rate ramp, ~2/s for a static curve), which bounds it, but an active
  ramp on two instances still costs ~1000 blocking round-trips per second
  onto the plugin-main thread that also serves the param panel,
  `instantiate` and `save_state`. The fix is a fire-and-forget sibling to
  `set_params` (post, don't `run`) plus a driver that uses it — a
  `clap_host` API change deliberately kept out of Task 9's diff.
- ~~**A GESTURE TOKEN, so `gesture_end` closes the gesture it meant to.**~~
  → **closed on `fix/gesture-end-id`** (`gesture_begin` returns the run
  id; `gesture_end(id)` no-ops on mismatch). Historical record of why it
  existed: `gesture_end` used to take **no identifier** — it closed
  "whatever is open", and was deliberately a no-op when nothing is
  (`control/mod.rs:2072`, contract stated at `:2067-2071`). Its sibling
  `gesture_begin` (`:2060`) auto-closes a stale open gesture before opening
  the new one. Together those two facts meant: **any `endGesture()` fired
  from a promise continuation can close a DIFFERENT gesture — one that
  began while it was awaiting.** No component-local flag can fix this; the
  flag Track D added to `AutomationLaneView` fixes only that component's
  own pairing. Three live instances (all now pass the token):
  - `src/lib/state/plugins.svelte.ts:243-244` (Track D's own, Task 3):
    `await this.flushParamQueue(); project.endGesture();`. Release a plugin
    knob — that awaits a rAF plus one IPC round trip — and press a track
    fader inside that window. `beginGesture("gain drag")` auto-closes the
    plugin gesture (fine, its edits are already folded), then the PENDING
    `endGesture()` lands and closes the GAIN gesture immediately. The rest
    of the fader drag then commits outside any bracket: its own undo entry
    and its own `project.json` write per rAF batch. **That is the exact I-8
    regression, inside I-8's own fix.**
  - `src/lib/state/library.svelte.ts:194-198` — same shape (`beginGesture`
    … `await` … `finally { endGesture() }`), pre-existing, not Track D's.
  - `src/lib/components/AutomationLaneView.svelte:116` — the delete path's
    `.then(() => project.endGesture())` still closes whatever is open when
    it fires, which may be a gesture a later pointerdown opened.

  That is the bug the token closed. The three call sites now hold the
  id across the await; a mismatched `gesture_end` is a no-op.
- **Per-lane commit barriers in the automation store** (accepted and open):
  `#inflight` is ONE field on a singleton store, so it is the last commit
  issued from inside any gesture, not "this lane's". Two simultaneous
  pointer interactions on two different lanes (multi-touch or stylus+touch;
  a mouse cannot produce it) let `commitLatest` await the wrong lane's
  commit and re-commit a stale set — observed payloads
  `[["lane-a",2],["lane-b",2],["lane-a",1]]`, where that trailing
  one-point commit is the click-insert bug alive again. Narrow, and the
  backend's single-gesture bracket is already degenerate under two
  simultaneous interactions, so it is documented rather than fixed. The
  honest fix is a `Map` of barriers keyed by the same identity
  `commitLatest` resolves by — the TRACK TARGET, **not** the lane id: the
  mint case the barrier exists for has no lane id yet.
- **Fader-follows-automation** (ruling 6) — needs read/write arbitration.
- **A dedicated, resizable automation row** (ruling 8).
- **Live param-panel follow** (ruling 2) — the open panel shows the
  document, not the automated value.
- **Write/touch/latch automation modes** — the non-goal with a live
  consequence, and it is NOT a transient one. **Every plugin-param lane
  this UI can create is FLAT**: the "A" button
  (`automation.automatePluginParam`) mints a lane with exactly ONE point,
  `compile_lane` yields one event, and `value_at` holds it forever — and
  "A" is the only way to make a plugin-param lane, because the lane editor
  (`AutomationLaneView`) reads `gainLaneFor` and draws TRACK GAIN only.
  There is no curve editor for a plugin param yet. So a flat lane pins its
  parameter for the WHOLE playthrough: click A on Cutoff at 0.30, press
  play, turn Cutoff to 0.80 in the plugin's own GUI, and within ~0.5 s it
  snaps back to 0.30 and stays there, while AURA's param panel still shows
  0.80. That is intended scope (automation OVERRIDES the knob during
  playback — ruling 2), not a spec violation, but a reader must not assume
  the target picker leads to a drawable curve. Drawing curves on plugin
  params, and write/touch/latch, are the two follow-ups that change it.
- **`movePoint` deletes a neighbour on a tick collision**
  (`src/lib/utils/automation-edit.ts:57-66`, review minor 6): dragging a
  point onto another point's tick silently removes the neighbour. Undo
  recovers it, but the curve loses a breakpoint mid-drag with no warning.
  Deliberately NOT fixed in the close-out round.
- **`.tog.auto.on` is byte-identical to `.tog.arm.on`**
  (`TrackHeader.svelte`, review minor 7): on `main` an automation-visible
  track will read as "this track is armed". The plan's own copy
  instruction produced it; it needs its own colour. Also NOT fixed here.

### WARNING for the next track that touches a component

**This repo has no DOM test environment** — no jsdom, no happy-dom, no
testing-library. Every frontend test runs in plain node against stores and
utils, so **nothing inside a `.svelte` file is covered by anything.**

That is not a theoretical gap. BOTH of this track's real frontend bugs
lived exclusively in `.svelte` event handlers, and both were found by
READING the code — one by a reviewer, one by the implementer while fixing
the first:
- the click-insert race (a drawn point silently erased, intermittent), and
- the alt-click delete closing its gesture twice, the second time early.

Neither could have been caught by the suite as it exists, and both survived
a task review that had looked directly at the handler. The pattern to draw
from it: when logic that ORDERS async effects sits in a component, move it
to a store where it can be tested — which is what the click-insert fix did
(`commitInGesture`/`commitLatest`) — and treat any remaining handler logic
as unverified until read line by line.

Adding a DOM environment is a cross-cutting call **nobody has made**. It
is not recorded here as a recommendation, only as the missing capability
that made the above necessary.

### OWED BY THE OWNER: the ear check

**Nobody has yet heard an automation lane change the volume during
playback.** Task 9's step 6 (start the app, draw a fade on a track, press
play, listen) was not performed — the implementing agent has no audio
device. It is the sole verification of `engine.rs:884`, the line the whole
engine task exists for; everything else is test-verified. **Do this
first.**

### Mid-flight rulings (from the SDD ledger, each with its one-line why)

- **`compile_automation`'s ramp table is sized by `store.tracks.len()`,
  not `slots.len()`** (Task 9, judged STRICTLY BETTER than the plan's own
  code): `derive_slots` keys by track id, so two tracks sharing an id
  collapse to one map entry pointing at the LAST index — sizing by the map
  would put that very slot out of range and silently unramp the highest
  slots. The offline twin follows the same rule, with its own test.
- **Held params are RE-ASSERTED, not invalidated** (Task 9 fix round 1,
  finding I-3): the reviewer proposed invalidating the driver's dedup
  cache whenever `execute_host_forward` writes the same param. The
  implementer chose a bounded re-assert (`REASSERT_TICKS = 250`, ~0.5 s)
  instead and the re-review upheld it against its own recommendation:
  invalidation only covers writes through OUR channel, while the plugin's
  own GUI, a patch load and `load_state` all move the parameter without us
  hearing about it. Re-assert covers every writer and needs no new side
  channel into the engine thread's driver.
- **CLAP batching landed WITHOUT touching `clap_host`** (Task 9 fix round
  1, finding I-2): the driver sorts compiled lanes by instance (stable) so
  a tick's writes arrive in contiguous per-instance runs the caller
  collapses into one host round-trip per plugin. The non-blocking post
  path was NOT built, because `set_params`' blocking form is its contract
  and LV2 already posts — the exposure is narrower than the finding
  assumed. See the deferral above.
- **One rebuild per lane commit, including transient folds** (Task 9,
  accepted): harmless under ruling 7 (a drag is one commit), and it would
  become expensive only if a future caller committed per pointermove.
  Noted so that caller knows.
- **`close_gesture` executes the deferred persist BEFORE its empty-batch
  early return** (Task 2 fix round 1): the early return could otherwise
  discard everything a gesture had accumulated. The fold-only contract
  (`IN_GESTURE_FOLD`) is now enforced rather than merely documented.
- **Two lanes on the same target resolve LAST-WINS, silently** — track
  gain (`compile_gain_ramps`) and plugin params
  (`ParamAutomationDriver::new`) both. No blend, no error; the UI keeps one
  lane per target, so it is documented rather than rejected.
- **The lane's commits are SERIALIZED through the store** (whole-track
  review, blocker): the overlay's pointerdown click-insert commits without
  awaiting, and the store is only written when `automation_set` RESOLVES —
  so a pointerup landing inside that round-trip window read the PRE-insert
  lane and committed it back, erasing the point the user had just drawn.
  Both commits folded into one gesture, so not even an undo entry revealed
  it, and a normal 50-150 ms click usually beats the round trip, which made
  the loss intermittent. Task 6's review had seen the double
  `automation_set` and filed it as a cosmetic "implicit fold dependency"
  minor — what it could not see was that the SECOND payload is stale, not
  merely redundant. The fix is a barrier in the store
  (`commitInGesture`/`commitLatest`): the closing commit waits for the
  insert's reply before reading. Found while fixing it, same family: the
  alt-click delete path closed its gesture from its commit's `.then`, while
  the pointerup that followed closed it AGAIN and earlier — leaving the
  delete's commit outside the bracket with its own undo entry and its own
  persist. A `gestureOpen` flag pairs them properly.

### Deferred-minors roll-up

Every `minor (deferred):` line the ledger recorded, collected so a future
round doesn't have to re-mine it. All are OPEN and accepted unless marked.

- Epoch-comment precision; a `Copy` type cloned; a dead borrow comment
  (Task 2).
- `plugin_set_param`'s doc (`plugins/mod.rs:345-351`) doesn't mention the
  gesture-fold path (Task 3). Its sibling — `for_op`'s "one wired caller"
  doc — is **CLOSED**: Task 4 corrected it to name all three wired callers
  (`control/mod.rs:825-828`, and the matching sentence in
  `for_history_op`'s doc at `:854-858`).
- `set_automation_lane` duplicates a closure body its siblings share
  (Task 4).
- `reloadOpenParams` shows no `paramsLoading` indicator; a noisy
  `paramError` in the removed-instance race (Task 5).
- A click-insert emits two `automation_set`s (an implicit dependency on
  folding — a comment was wanted); unmount mid-drag can leak a gesture (a
  shared pattern, not new here); `.tog.auto.on` is visually identical to
  arm (the plan's own copy instruction); the lane has no keyboard path
  (Task 6).
- `ParamAutomationDriver::tick` clones `String`s per write (control
  thread only); `resolve_targets` has a terse `None` arm (Task 7).
- The LIVE gain-ramp path is not tested across a loop wrap
  (`LoopSpec::OFF` in that test). Low risk: the re-seed logic lives in
  `RampCursor::value`, shared with the clip path, which HAS the wrap test.
  A note for whoever touches live-path looping (Task 8).
- A no-op delete of an unknown lane key still triggers a rebuild for
  nothing (Task 9) — now asserted by a test, accepted as behaviour.
- **CLOSED by the close-out**: the stale "No rate (headless…)" clause in
  `engine.rs`'s `compile_automation` test comment, and the missing
  last-wins note on `ParamAutomationDriver::new` (both Task 9 re-review).
- **Standing, from the plan's global constraints**: two tests are flaky
  under the default parallel thread count and pass in isolation —
  `control::hum::tests::apply_hum_clip_commits_synchronously_and_announces_project_changed`
  and `plugins::host::tests::plugin_main_thread_slots_and_tickers`. Not
  this track's to fix; re-run in isolation before calling either a
  regression.

## Track B handoff (2026-08-15, MIDI slice 2, subagent-driven, external review layer)

Track B (hardware MIDI I/O — input routing, recording, clock/sync out) is
**IMPLEMENTED** on branch `midi-slice-2` (**PR #21**), cut from
`origin/main` at `3340aa8`, with `origin/main` merged in at the Task 8/9
boundary (`85cf07c`, pulling in Track E *and* Track D). Plan document:
`docs/superpowers/plans/2026-08-14-midi-slice-2.md`. SDD ledger
(gitignored): `.superpowers/sdd/2026-08-14-midi-slice-2/progress.md`.
Twelve tasks.

**What landed.** A process-global `MidiInHub` (`audio/midi_in.rs`) owns a
wait-free SPSC ring from the midir callback thread to the RT output
callback; the mixer dispatches the drained events into the target track's
existing `LiveNodeCell`, so monitoring plays the *same* instrument node
playback uses, with the transport playing or stopped. The same hub buffers
the take control-side; at stop, `midi::capture` converts the edges to ticks
through the same `TempoMap` bijection playback schedules from, and
`Op::MidiClipAdd` rides in the SAME `commit_recording_finalize`
transaction as the audio `ClipAdd`s — one take, one undo entry, no new op
kind, no format bump. MIDI OUT is a dedicated non-RT `aura-midi-out`
thread (`midi_out.rs`) reading `SharedRt` + a tempo/event snapshot: 24 PPQN
clock, Start/Stop/Continue/SPP, and note-out from one chosen MIDI track —
it never touches `engine.rs`. Frontend: a `midiIo` store, the master
strip's midi-in/midi-out groups, and arm-a-MIDI-track → route-the-keyboard
glue. User docs: `docs/midi-input.md` (rewritten), `docs/midi-output.md`
(new).

**Suites: 662 backend (633 lib + 29 integration) + 271 frontend, all
green, measured on `midi-slice-2` 2026-08-15**; `npx svelte-check` 0
errors, 0 warnings. Doc-tests are 0 and are NOT a test target — an earlier
report in this track counted them and reported 641 where the truth was
640. Counts dated in README/CONTRIBUTING. Other tracks are in flight, so
the count line is a known cross-track merge-conflict point: whoever merges
last re-measures rather than picking a side.

### OWED BY THE OWNER: three ear checks, and no test in this chain substitutes

Nothing in this track has been heard by a human — the implementing agents
have no MIDI hardware and no audio device. **Do these first, in this
order:**

1. **Note-out to real gear** (Task 8): route a MIDI track to an external
   synth and hear it play. This is the owner's own stated priority half of
   the slice and the only thing that proves it end to end. Byte delivery
   over a real ALSA loopback IS proven by tests; "the synth makes the right
   sound" is not.
2. **Hydrogen clock sync** (Task 7): slave Hydrogen to AURA, press play,
   confirm it follows — and specifically confirm whether the loop-wrap
   re-cue (`Stop`/SPP/`Continue`, ruling 5) sounds acceptable, since that
   is the ruling most likely to be wrong for a real device.
3. **A real keyboard, the whole loop** (Task 11): arm → hear the track's
   instrument → record a few bars → stop → Ctrl+Z removes the take in one
   step → Ctrl+Shift+Z → save, reopen, the take is still there.

### THE ONE THAT CAN COST A USER WORK: recording under an active loop

**Recording while a loop is active silently produces a musically wrong
take.** Full mechanism, both candidate fixes and the reason the plan's
non-goals do *not* cover it: `docs/backlog/hardware-midi-io.md`, "Still
open after slice 2". The user-facing "don't do that" is in
`docs/midi-input.md`. Choosing between the two fixes is a **behaviour
decision for the owner**, not a refactor.

### Two more limitations found by the whole-track review (documented, not fixed)

- **Jamming over a loop silences held notes at every wrap.** `render_live`
  calls `node.all_notes_off()` on each loop wrap — correct for clip notes
  (their note-offs lie past the loop end), but the monitored live-in voice
  SHARES that node, so a chord held across the wrap decays away and the
  player must re-strike it every pass. `docs/midi-input.md` advertises
  exactly this workflow ("press play, arm a track, jam along"), so it is
  documented there. Never a hang, always a premature cut. Not a one-line
  fix: the alternative is re-issuing the held note-ons after a wrap, which
  needs the held-key mask on the mixer side.
- **Seeking during a take corrupts it the same way a loop wrap does.**
  Nothing in the seek path (`control/mod.rs`'s `TransportAction::Seek`)
  checks whether a take is running, and `capture.rs`'s
  `saturating_sub(start_ticks)` then collapses every later event toward 0.
  Documented alongside the loop-wrap limitation; the fix is the same
  decision.

### Standing hazards a future round must not rediscover the hard way

- **`live_in_release_slot` holds ONE slot and compares slot NUMBERS, not
  track ids.** A second target change inside the ~85 ms release window
  steals it (a ≤85 ms decayed fragment, never a drone). The dangerous path
  is not human speed: a rebuild that renumbers slots after a track delete
  both steals the window AND aims mask-derived note-offs at whatever track
  now owns that number, where they can cut THAT track's clip note. The real
  fix is comparing target *identity* rather than slot index — a design
  change, deliberately not attempted in a fix round.
- **`RELEASE_SECS` is PolySynth's constant applied to every instrument.** A
  sampler or plugin with a longer tail is cut mid-decay when it loses the
  target, and raising the constant trades that against a longer deaf window
  on the new target. The real answer is a per-node `release_tail()` on the
  processor trait.
- **Events drained during the release window are discarded**: a key struck
  inside it is silently swallowed (its later note-off arrives orphaned and
  harmless).
- **`render_live_input_only` applies static gain/pan/mute/solo but NOT
  Track D's compiled gain ramp**, so stopped-transport monitoring on an
  automated track monitors at a different level than it plays.
- **The engine expands `EV_ALL_OFF` into per-key note-offs before it
  reaches the mixer**, so the mixer's own `EV_ALL_OFF` arms are now reached
  only by mixer tests. Deliberate, and cross-referenced in comments
  (`mixer.rs`) precisely so nobody reads the mixer test as a live contract
  and deletes the engine-side expansion.
- **Two `RtTrack` rows exist at one slot for a midi track.** Both write the
  same meter lane via `set_slot_local`, so it is last-writer-wins, and it
  is correct ONLY because `append_from_with_input` appends the live rows
  AFTER the assembly loop. Invert that order and the midi track's meters
  silently read zero.
- **`start_recording(Some(vec!["m-1"]))` with no routing target fails with
  "no armed tracks to record"** even though the caller named a track
  explicitly — reachable from MCP's `record_take`. The plan pinned that
  wording; the missing piece is a distinct branch for a non-empty explicit
  request.
- **TOCTOU on the take's target track**: the session read lock drops before
  `take_clip` and is re-taken inside `transact`, so a concurrent
  `remove_track` in that window commits a MidiClip pointing at a dead
  track. `ClipAdd` has the same exposure; closing it means holding the
  session lock across the commit.
- **The arm-edge invariant at `begin_capture` is comment-only and
  untestable as stated**: a future refactor that moves the call up will not
  go RED anywhere.
- **The R5 guard in the release-window countdown is deliberately a comment,
  not a `debug_assert!` — controller's ruling, and the reason matters**:
  `engine.rs:714` documents a `debug_assert!(false)` that was REMOVED from
  `stale_sources` because its "unreachable" condition turned out reachable
  in production and panicked the control thread. This one would sit on the
  AUDIO CALLBACK, where a panic is worse.
- **Deleting a routed track** now clears both routings
  (`ControlPlane::remove_track` → `clear_midi_routing_for`, Task 12).
  Undoing a `TrackAdd` does not pass through there and still leaves a stale
  id until the next explicit selection. On the INPUT side that is cosmetic
  (`refresh_target` resolves an unknown id to `NO_SLOT`). On the OUTPUT
  side the close-out's first "cosmetic" claim was WRONG and is corrected
  here: a vanished track yields an EMPTY events snapshot under an
  UNCHANGED track id, which is precisely the shape that used to strand the
  note-out cursor and hang a sounding note. It is survivable only because
  `run_thread` now keys its release+reseek on snapshot CONTENT (Critical 1
  below). Do not restore an id-only comparison there. Either way, both
  selectors must keep tolerating an id the store does not have.
- **The 1–2 block window after a rebuild** where the callback still holds
  the OLD graph while the hub already resolves the NEW slot number: a key
  struck in that window can reach the wrong instrument. This is the
  note-ON half of the slot-number problem above (which covers only the
  release window), and it is the one part of `MidiInHub`'s design that a
  slot-index→identity change would also have to address.
- **A device switch during an open release window** leaves the outgoing
  node frozen mid-decay — a new `OutputCb` starts with all counters zero,
  so nothing finishes the countdown, and the fragment surfaces on that
  node's next arm. Same family as the one-slot steal.

### Rulings later rounds inherit (from the plan, ADR 0007)

1. **The MIDI-in target track is app config, not document state** — a
   `ControlPlane` seam with attribution and no op, NOT derived from
   `TrackState.armed` (deriving it would force every `armed` writer —
   commands, undo/redo, project open, MCP — to re-publish the routing, a
   side-channel by construction). The frontend calls the selection from the
   same arm click, so the UX still reads "arm a track, play it".
4. **The clock's musical position is `SharedRt::position` + the `TempoMap`
   bijection, not `SharedRt::steady`** — `steady` never stops, seeks or
   wraps, and those are exactly what Start/Stop/Continue/SPP are. The
   `SectionTable` buys nothing at 24 PPQN.
5. **A backward transport jump re-cues the slave** with `Stop` + SPP +
   `Continue` rather than free-running. Switch to a silent re-anchor if the
   owner's gear dislikes it (see ear check 2).
9. **MIDI-in timestamps are transport-position-at-arrival**, i.e. quantised
   to the audio block (~5–11 ms).
10. **Note-out is a routing carve-out, not a track field** — no document
    field, no op, no `OP_FORMAT_VERSION` bump. The track keeps sounding
    internally; muting it in AURA is the documented anti-doubling move.
11. **No persistence of selected ports/target across restarts**, because a
    port id is `"<name>#<index>"` and can name a different device after a
    replug.

### Deferred minors, rolled up (all OPEN and accepted unless marked)

- `cargo fmt` is not clean on `midi/capture.rs` — project-wide
  pre-existing, not this track's (Task 2).
- Mandated duplication `render_live` ↔ `render_live_input_only` and
  `append_from` ↔ `append_from_with_input`, with keep-in-sync comments
  (Task 4).
- A theoretical parallel-test race on the process-global hub between the
  two `ControlPlane` selection tests, and now the two Task 12 routing
  tests. The plan's own accepted trade-off; the fix if a flake appears is
  an injectable hub, not a serialized suite (Task 5).
- Unchecked `u64` multiplication in the clock's interpolation (~3 years of
  continuous anchor age before overflow); `estimated_sample()` is stale
  while stopped (Task 6).
- In RELEASE builds an unsorted event array is now marginally worse than
  before: the binary-search cursor can in principle move BACKWARD into
  already-fired territory, where the old forward walk stopped predictably.
  No live path triggers it (`track_events` sorts unconditionally), and the
  `debug_assert` covers debug builds (Task 8).
- No integration test drives `out_tick` through a real `ClockEngine` drift
  jump with a `NoteOutSnapshot` attached. Closed at primitive level
  (backward `reseek` is proven directly), so not blocking (Task 8).
- The `~85 ms`/one-block release window's own costs are the first three
  standing hazards above (Task 10/11).
- `midiClipId` on `recording://state` is now consumed (Task 12 wires the
  MIDI store to pull, select and flash the take) — **CLOSED**.
- **CLOSED by the close-out**: the dangling routing after a track delete;
  the missing note-out **track** selector in the UI (the store method
  existed and was unreachable — the plan's Task 9 never specified the
  control, while Task 12's own doc step told users to use it); the
  uncaught `stopRecording()` rejection in `TransportBar`/`HumPanel`; the
  undocumented `Err`-after-commit contract on `stop_recording`.
- **Fixed by the whole-track review round, both in the owner's priority
  half, both of which hung a note on real hardware**: (C1) `run_thread`
  keyed its note-out release+reseek on the track ID alone, so any edit to
  the ROUTED track's clips — a note delete, a clip delete, a tempo change —
  swapped a new events array under a stale cursor index; a note sounding at
  that moment never got its note-off, and an emptied array killed note-out
  for the rest of the pass. Now keyed on snapshot CONTENT, with a real
  ALSA-loopback test that goes RED (clock bytes only, no note-off) when the
  comparison is reverted. Task 8's gate missed it because its track-swap
  test calls `reseek` BY HAND, proving the explicit path rather than the
  one the thread takes. (C2) NOTHING asked the out thread to exit on app
  quit: `MidiOut::drop` never runs because Tauri's event loop ends in
  `process::exit`, so closing the window mid-note hung that note forever
  and never sent `Stop`. `lib.rs` now uses `build` + `run(callback)` and
  calls `midi_out::release_on_exit` on `RunEvent::Exit`. **The handler
  BODY is proven** (the ALSA shutdown test drives that exact function);
  **that Tauri fires the event is NOT verified here** — it needs a windowed
  run, so treat it as owner ear-check 0.
- **Carried in from the same review, still open** (each real, none
  blocking): space-bar stop after a WAV-writer failure leaves the UI stuck
  in "recording" with no message — `control/mod.rs:1432` skips
  `emit_transport` and `App.svelte:137-139` `void`s the rejection (the
  TransportBar/HumPanel catches do NOT cover the keyboard path; recoverable
  by a second keypress, no duplication risk). Every MasterBar MIDI handler
  `void`s a rejecting promise, and its selects use `<option selected={…}>`
  with no `value=` on the `<select>`, so once the user has picked an
  option the DOM cannot repaint from the store: pick a busy MIDI-out port,
  `select_port` fails, the store reverts to `null` and the dropdown still
  shows the port name with no error. House style (the audio device selects
  do the same) — but these are the first selects with a POLLING store
  behind them, which is what makes it visible. A failed `sink.send`
  mid-batch clears the sounding mask first (`midi_out.rs` `all_off` vs its
  caller), discarding the remaining note-offs with no state that knows a
  note is up — it breaks "the mask tracks reality" by construction.
  `adoptTake` can resurrect an undone take: Ctrl+Z right after a take runs
  `init()`→`applySnapshot()`, and if that resolves before `adoptTake`'s
  in-flight `midiGetClips()`, the undone take is written back and stays
  until the next refresh — needs an epoch guard.
- **Deferred minors recovered from the ledger** (they were missing from
  this roll-up's first draft): Task 3's `routing:false` duplication, and
  Task 7's three (a default-value test, teardown duplication, avoidable
  clones).
- **Standing, from the plan's global constraints**: three tests are flaky
  under the default parallel thread count and pass in isolation —
  `control::hum::tests::apply_hum_clip_commits_synchronously_and_announces_project_changed`,
  `plugins::host::tests::plugin_main_thread_slots_and_tickers` and
  `pure_readers::library_audition_core_leaves_the_document_and_the_history_untouched`.
  Re-run in isolation before calling any of them a regression. Zyn host
  spam on stderr is expected noise.

## Track C handoff (2026-08-15, subagent-driven session, external review layer)

Track C (multi-clip selection, group drag/resize, cross-track and
cross-instance paste, SMF export of a selection) is **IMPLEMENTED** on
branch `multiclip-clipboard` (**PR #22**), cut from `origin/main` at
`3340aa8`, with `origin/main` merged in twice: at the Task 9/10
boundary (`c26d01d`, pulling in Tracks E, D and B plus two commits the owner
merged himself) and again at close-out (`79ae94c`, pulling in PR #26 clip
delete and PR #29's LV2 stderr guard — see "The second merge" below). Plan document:
`docs/superpowers/plans/2026-08-14-multiclip-selection-paste.md`. SDD ledger
(gitignored): `.superpowers/sdd/2026-08-14-multiclip-selection-paste/progress.md`.
Thirteen tasks.

**What landed.** Selection is **viewer state** and nothing else: a flat
`Set<"<kind>:<id>">` in `src/lib/state/clip-selection.svelte.ts` over the
pure algebra in `src/lib/utils/clip-selection.ts` (click / shift / ctrl /
marquee), never serialized, never an op, never read by the backend — the
new commands take explicit clip-id lists. A group drag or MIDI group resize
runs through `clip-drag.svelte.ts` as ONE gesture: local preview per
pointermove, then one additive `move_clips` command whose `ControlPlane`
method copies `set_track_mix`'s gesture-aware fold verbatim (gesture mutex
held across the nested session lock), so a whole-group move is one undo
entry and audio and MIDI clips move in one cross-store transaction.
Snapping snaps the ANCHOR only and applies the resulting delta to every
clip, so relative offsets survive and a one-clip selection behaves exactly
as it did before. Copy/paste rides a backend-built
`application/x-aura-clips` JSON envelope (`control/clipboard.rs`:
`clips_copy` read command, `clips_paste` single-transaction write command
composing the existing `TrackAdd`/`ClipAdd`/`MidiClipAdd`) reaching the OS
clipboard through two thin `arboard` commands (`osclipboard.rs`) rather
than webview clipboard permissions — the frozen `capabilities/default.json`
is untouched. MIDI travels by value with note ids zeroed; audio travels by
reference with a three-step resolution on paste. Ctrl+C / Ctrl+V /
Ctrl+Shift+V (paste to new tracks) / Ctrl+Shift+M (export the selection as
a .mid, reusing the frozen `midi_export_file` clip-id filter).

**No new op kind, no new `PropPath` or `ObjectRef` variant,
`OP_FORMAT_VERSION` stays 2, no journal migration, no `engine.rs` change
(zero).** Five additive commands: `move_clips`, `clips_copy`, `clips_paste`,
`os_clipboard_write_text`, `os_clipboard_read_text`.

**Suites: 729 backend (700 lib + 29 integration, plus main's 2 `#[ignore]`d
plugin repros) + 359 frontend, all green, measured on `multiclip-clipboard`
2026-08-15 after the second `origin/main` merge**; `npx svelte-check` 0 errors,
0 warnings. Doc-tests are 0 and are NOT a test target. Counts dated in
README/CONTRIBUTING. `main` moved FIVE times during this track and this
branch absorbed all of it at `c26d01d`, so the count line is a known
cross-track merge-conflict point: whoever merges last re-measures rather
than picking a side.

### OWED BY THE OWNER: the cross-instance check, and no test substitutes

**Task 11's Step 12 was never performed end to end.** TWO links in the
cross-instance chain were unproven, and one has since been closed: the
whole-track review found that **the frontend's unconditional OS-clipboard
scan had no test either** — every paste test seeded an in-memory payload
first, so re-gating the scan (this track's own Critical 1, plan defect #14)
kept all 348 tests green. That gap is now closed by a test with `payload ===
null` and a valid envelope on the clipboard, verified to die under exactly
that re-gating. What remains is **ONE unproven link, and no test in this
repo can make it**: that instance B's `os_clipboard_read_text` receives
instance A's bytes **through
this track's own frontend orchestration**. Task 10 proved the raw
`arboard`/X11 mechanism transfers between processes; it never proved
FIDELITY, and it never exercised AURA's own read path. The numbered
procedure to follow is in the plan file at **Step 12 of Task 11**
(`docs/superpowers/plans/2026-08-14-multiclip-selection-paste.md:3950`) —
two instances, Ctrl+C in A, Ctrl+V in B, then a repeat with a multi-clip
marquee so the anchor/offset math is checked across the clipboard too, with
each FAIL mode spelled out so a failure gets recorded precisely instead of
as "didn't work". Expect the second instance to log an MCP port-41717
collision; that is known and out of scope.

**BINDING RULE, and the reason it exists: do NOT drive the real app's
keyboard or mouse with input automation (`xdotool` and similar), and do not
retry after a first inconclusive attempt.** An implementer tried exactly
that on a real X11 + `gpaste-daemon` desktop and found the X display being
driven concurrently by someone else — focus jumped mid-test, a keystroke
queued silently and fired minutes later, and one attempt **toggled the
Library dock and duplicated demo tracks** in what is almost certainly the
owner's live session. Read-only inspection of the screen is fine; synthetic
input is not. A headless `Xvfb` pass would also be **weaker than the unit
tests, not stronger**: Xvfb has no clipboard manager, which is precisely the
configuration where the transfer already works, so it would retire the step
without answering it. `xclip` from a shell only re-proves Task 10.

### THE PRODUCT CONSTRAINT: the clipboard size ceiling

Cross-instance transfer is reliable to **~165 KB** and **TOTALLY LOST — not
truncated — at ~200 KB** on X11 with a clipboard manager; the write reports
success, the cross-process read returns EMPTY, and waits up to 10 s do not
help. Measured against real `clips_copy` payloads the marginal cost is
**~80 B per MIDI note** (100 notes = 8 126 B, 1 000 = 79 788, 2 000 =
160 212), putting the cliff at **~2 000–2 500 notes** — and that is the
payload at its most compact, since `clips_copy` zeroes every note id. One
three-minute recorded take at 10 notes/second is 1 800 notes in a single
clip; a song section across 8 MIDI tracks at ~300 notes each is 2 400.
**The operation most likely to exceed the ceiling is the operation this
feature exists for.** Audio clips are irrelevant (references, hundreds of
bytes) — this is a MIDI-note ceiling.

**The harm the user cannot undo:** the local write SUCCEEDS, so AURA takes
X11 selection ownership and the previous owner loses it; the handoff to the
clipboard manager then fails and leaves an empty entry. **A failed large
copy destroys whatever was on the system clipboard beforehand — from any
application — and replaces it with nothing.** Data loss outside AURA from
an operation that reported success.

**Ruling: warn at copy time, never refuse — deliberate, twice upheld.** The
cliff belongs to the ENVIRONMENT (X11 + which clipboard manager + its
config; without a manager, megabyte INCR transfers are routine, and
macOS/Windows differ), not to AURA. A hard threshold in a cross-platform
layer is a false-refusal/false-confidence trap in both directions, and a
size guard only makes sense where the payload's MEANING is known — at the
caller that built the selection, not in a command that knows only that it
holds text. The store additionally keeps the payload in memory, which
MITIGATES the same-instance case and sidesteps the write-then-read handoff
window entirely; it is **not a fix** — cross-instance paste of a large
selection, the feature's stated reason to exist, is untouched by it. Also
note the ugly interaction: because `os_clipboard_read_text` now correctly
maps only `ContentNotAvailable` to "nothing to paste", a lost large copy
looks to the user like an EMPTY clipboard rather than a corrupt one — the
correct fix made this failure quieter, not louder.

### Scope rulings (carried from the plan doc, per ADR 0007)

1. **Paste composes existing ops in ONE transaction** — zero or more
   `TrackAdd`, then one `ClipAdd`/`MidiClipAdd` per clip, in one
   `commit`. `batch_coalesce_key` returns `None` for structural batches, so
   the 350 ms fallback can never merge two pastes either: one Ctrl+Z removes
   every pasted clip AND every track the paste created.
2. **MIDI by value, audio by REFERENCE, and a missing asset skips THAT clip,
   never the whole paste.** Resolution order before the transaction opens:
   in-project path → copy in from `sourceAbsPath` → skip and report
   `{name, reason}`. An all-skipped paste is a precise report, not an error.
3. **SMF is EXPORT-ONLY, to a file.** The clipboard slot a Tauri v2 /
   WebKitGTK app can own is plain text, so the `application/x-aura-clips`
   MIME name lives inside the JSON envelope and paste never parses SMF.
4. **Group-drag snapping snaps the ANCHOR only**; the delta is clamped (not
   the individual clips) so the leftmost clip lands at 0 and no further.
5. **Selection keys are `"<kind>:<id>"`** — a marked widening of the brief's
   `Set<ClipId>`, because two independent stores mint clip ids.
6. **The legacy single-selection fields stay** as the "focused clip"
   (`project.selectedClipId` / `midi.selectedClipId`); Ctrl+D is still
   single-clip `midi.duplicateSelected`.
7. **Group resize is MIDI-only** (ruling G) and **`contentLengthTicks` can
   be SET but not CLEARED through `move_clips`** (ruling H).
8. **The OS clipboard is reached through two Rust commands over `arboard`**,
   not the webview: `navigator.clipboard.readText()` needs a permission
   WebKitGTK does not grant, and a clipboard plugin would require editing
   the FROZEN `capabilities/default.json`.

### Mid-flight rulings and deviations (all reviewed and accepted)

- **The wire tempo domain is the NOMINAL 48 kHz rate**, not the engine's
  live rate (Task 8 Critical). Pinned by a 96 kHz regression test that dies
  if the unit rate comes back.
- **Paste validates track KIND in BOTH directions** (plan defect #11: the
  plan only checked MIDI-on-audio, but `import.rs` refuses audio clips on
  non-audio rows). Paste is the one path a FOREIGN document reaches the
  store, so it must not write a row the app itself cannot produce.
- **Paste-to-new-tracks groups on `(source track, kind)`**, not source track
  alone — identical for any AURA-produced payload, and it stops a
  hand-edited mixed row from stranding half its clips.
- **`resolve_audio_asset` carve-out:** with no project dir on EITHER side the
  relative reference travels verbatim instead of being skipped. The plan
  contradicted itself here (its own mixed-paste test asserts
  `skipped.is_empty()` in a project-less harness); plan defect #12.
- **Every incoming note id is ZEROED in phase 2** (Task 9 C-1/C-2). This one
  fix removed a fatal path (a payload with duplicate non-zero note ids made
  the WHOLE paste fail, orphaning copied wavs) and made the hardcoded
  `next_note_id: 1` correct BY CONSTRUCTION. Copy stripping ids is a
  property of AURA's own copy, never a guarantee about a foreign payload.
- **`source_path` is validated through the project's OWN
  `normalize_source_path`** (Task 9 C-3), not a hand-rolled check, so paste
  and L-1 cannot drift. Before the fix, `../secret.wav` and absolute paths
  passed straight through and produced a PERMANENTLY SILENT clip that never
  appeared in `skipped`.
- **A foreign `ppq` is RESCALED, not refused** — exact when one divides the
  other (the 480/960 case that actually occurs), else rounded to the nearest
  tick; clips that no longer fit are skipped with a reason, never wrapped.
  `ppq == 0` is refused in the envelope. Refusing would make cross-instance
  paste impossible between whole projects with nothing the user could do
  about it (ppq is fixed at project creation).
- **The tick rescale decision moved INSIDE the commit closure.** A re-check
  would have added a mechanism with its own failure modes and answered a
  divergence by SKIPPING the user's MIDI clips; not diverging is better than
  detecting divergence. The old "ppq only changes at project load" comment
  was provably FALSE (`Op::TempoSet` writes `session.midi.ppq`, and
  `set_tempo_map` is a live command).
- **In-place paste INHERITS the `SourceId` the project already uses for that
  path** (including one established by an earlier clip in the SAME paste);
  copied files mint fresh. The previous comment claiming `assign_source_ids`
  kept the decode cache warm was false and is gone.
- **Cross-instance paste is unconditional** (Task 11 Critical, plan defect
  #14): `paste()` returns "found a payload somewhere" (deliberately NOT
  "the paste succeeded"), Ctrl+V always scans the OS clipboard first and
  only then falls back to the legacy single-clip stamp. Before the fix,
  pasting into a FRESH window — the track's entire reason to exist — did
  nothing at all, silently.
- **The "nothing to paste" message lives at the CALLER**
  (`clipClipboard.pasteAtPlayhead`), which is the only Ctrl+V entry point
  and the only place that knows BOTH paths came back empty. Putting it in
  the store made the app claim there was nothing to paste while the legacy
  path pasted a clip.

### The second merge (`79ae94c`): main's clip delete meets multi-selection

PR #26 added a `×` button and Delete/Backspace on both clip components —
the same handlers this track filled with selection, anchor and drag logic.
Four conflicts, all additive-on-both-sides except one; the handler list was
verified by SET COMPARISON (ours 93, main 90, union 95, merged 95, nothing
missing, nothing invented). Two things are worth carrying forward.

**RULING — Delete with a multi-selection stays SINGLE-CLIP, deliberately.**
Delete/Backspace deletes the focused clip; three other selected clips stay.
The plan's non-goals already say "no multi-clip DELETE, split or duplicate",
and nothing about main's commit changes that reasoning — but the decisive
argument is the undo shape, not the scope line. `remove_clip` and
`midi_remove_clip` are SINGLE-clip commands, one transaction each, so
extending Delete to a selection of four would commit **four transactions and
four history entries**. The user would then press Ctrl+Z, get ONE clip back,
and have to press it three more times — a partial undo of a single gesture,
which is exactly the failure mode this whole track (and Task 9 in
particular) exists to prevent. Shipping a destructive multi-clip operation
with the wrong undo granularity, in a close-out, is strictly worse than not
shipping it. **The honest cost is real and named:** a user who has just
learned that clips select as a group will press Delete and be surprised that
only one disappears. That surprise is recoverable with one correct Ctrl+Z;
the alternative's is not.

**The follow-on, with its design question already answered:** add a
`clips_remove` batch command that composes `Op::ClipRemove` /
`Op::MidiClipRemove` for the whole selection inside ONE
`commit(TxMeta::user("delete clips"), …)`, exactly as `clips_paste`
composes the add ops — one undo entry for the batch, no new op kind, and
`OP_FORMAT_VERSION` stays 2. Then wire Delete to it. That is a task, not a
close-out fix.

**The `×` button now appears on EVERY selected clip, not just one.** In main
`selected` meant the single focused clip; on this branch it means
multi-selection membership, so `{#if selected}` renders one `×` per selected
clip. Judged coherent and kept: each `×` deletes its own clip and (after the
merge fix) no longer disturbs the selection. Named here because it is a
visible behaviour change nobody chose — it fell out of the merge, and the
next person to see four `×`s should know it was inspected rather than
missed.

**A merge-mechanics lesson worth more than the merge:** the mechanical
keep-both resolution of `tauri.ts` ate a shared closing brace and left the
file syntactically invalid — and **`npm test` still passed**, because every
store test mocks `../tauri`, so the frontend suite structurally cannot see a
broken `tauri.ts`. Only `svelte-check` caught it. It is a required gate, not
a formality, and a green `npm test` is not evidence that the frontend
compiles.

**One new test, at the seam neither parent covered:** deleting a clip
through `project.removeClip` / `midi.removeClip` while it is part of a
multi-selection drops it from the selection and leaves the rest intact. The
pre-existing live-filter test sets `project.clips` directly and never
touches the delete path; a sabotage that stops `removeClip` from updating
the store kills ONLY the new test. (Green on first run — a characterization
test of an already-correct seam, sabotage-proved rather than RED-first, and
recorded as such.) The property matters because a stale key would reach
`move_clips` as an unknown id and fail the whole NEXT group drag.

### Closed by the whole-track final review (2026-08-15)

- **CRITICAL — `clips_paste` validated track kinds and paths but NOT note
  FIELDS.** A pasted `key >= 128` reaches `midi_out.rs`'s
  `sounding[ev.key as usize]` against a `[bool; 128]`: **an out-of-bounds
  panic on the `aura-midi-out` thread**, reachable from a hand-edited
  envelope onto a MIDI track routed to hardware (Track B, now on main). It
  is also DURABLE — the raw byte survives save/load, and the user's own
  later `midi_set_notes` on that clip then fails forever. `velocity: 0`,
  `channel: 99` and `lengthTicks: 0` rode the same gap. Fixed by calling the
  document's OWN `MidiNote::validate()` in the same phase-2 loop that zeroes
  `note_id`, skip-and-report per clip (never fatal to the rest, ruling B).
  **The lesson is about the shape of the miss, not the field:** the module's
  trust-boundary doc ENUMERATED what it distrusts ("not note ids, not paths,
  not ppq, not sample windows"), and note ranges simply fell out of the
  list. An enumeration is where a trust boundary silently loses items.
- **IMPORTANT — clip `gain_db` crossed the same boundary unclamped.**
  `"gainDb": 800` → `10f64.powf(40.0) as f32` → `f32::INFINITY` → the mixer's
  `l * g` turns a silent sample into NaN, and nothing on the sample path
  checks `is_finite` (only `pan` is clamped). The TRACK gain setter already
  clamps to `-160..=24`; paste was the one writer that did not. Now clamped
  at the boundary (non-finite mapped to unity first, since `clamp` passes
  NaN through), and clamped rather than skipped — an absurd gain is a value
  error the user can see and change, not an unusable clip.
- **IMPORTANT — Track C's marquee and Track D's automation lane collided.**
  `.autolane` is `position:absolute; inset:0` inside `.lane` and does not
  stop propagation, so dragging a gain point ALSO ran `onLanesPointerDown`:
  `replace` mode wiped an existing multi-clip selection, and because the
  automation canvas had captured the pointer, every later `pointermove`
  bubbled to `.lanes` and dragged a live selection band across the timeline.
  A following Ctrl+C then copies clips the user never selected; a following
  drag group-moves them. Fixed on **Track C's side** — `.autolane` added to
  the guard, now the pure, tested `startsMarquee` in
  `src/lib/utils/clip-selection.ts` — rather than by `stopPropagation()` in
  Track D's component: adding a selector cannot change another track's
  behaviour, while swallowing the event changes the flow for every ancestor
  listener, present and future. Whoever adds the next absolutely-positioned
  overlay inside `.lane` adds it to that list.
- **The frontend's unconditional OS-clipboard scan gained the test it never
  had** (see the OWED section above) — the fifth test-that-could-not-fail on
  this track, and the first one found by an outside reviewer's sabotage
  rather than by the implementer's own.
- **`clip-drag`'s await-before-`gestureEnd` is no longer inspection-grade.**
  The existing call-order test could not see it: its mocks record
  synchronously, so `void backend.moveClips?.()` still ordered
  "moveClips" before "gestureEnd" and stayed green. The added test records
  only after the mock's promise has SETTLED, and dies under exactly that
  `await`→`void` change. Worth generalizing: **a call-ORDER assertion over
  synchronous mocks does not test asynchronous ordering** — it tests
  invocation order, which is a different property.

- **`move_clips_undo_restores_every_clip_in_the_batch` could not fail on
  identity — the SIXTH such test, and now closed.** It seeded both clips at
  0 and asserted both back at 0, so an inverse restoring the wrong clip
  passed. The clips are now seeded at distinct starts and asserted back at
  their own, looked up BY ID rather than by store index. Killed by two
  independent sabotages (an inverse rotated onto the next clip's object; an
  inverse carrying the wrong pre-value), and the OLD version was re-run
  under the same sabotage and PASSED — the evidence, not the assumption.
  **An audit for a seventh found none**: all four paste-side undo tests
  assert surviving rows by ID (one compares the whole before/after id
  tuple), and the frontend cancel-restore tests seed distinct values per
  clip and across two stores, so a swap cannot hide in them.
- **Worth knowing before sabotaging an inverse here — the trap is
  ARM-SPECIFIC.** Sabotaging the target of the inverse `apply_raw` returns is
  **live for structural ops** (`ClipAdd`, `MidiClipAdd`, `TrackAdd`), because
  `fold_ops`' `Slot::Passthrough` forwards that inverse verbatim — which is
  exactly why this track's `SH` sabotage on `Op::ClipAdd`'s inverse genuinely
  killed four paste tests. It is **dead for property-addressed `Op::Set`s
  that reach the grouped path**, where `fold_ops` REBUILDS the folded inverse
  from the original op's `object`/`path` and keeps only the values: a swap
  there is inert BY CONSTRUCTION and reports a false all-green. On those,
  sabotage the fold or the values instead. (Found by running exactly that
  sabotage and watching it change nothing.)

### Ledgered by the same review, NOT fixed

- **Ctrl-click-deselect still sets the focused clip and the drag anchor**, so
  deselecting leaves the clip focused for the Dock and targeted by a drag.
- **`begin()` while a previous `end()` is still pending** is not covered.
- **`onpointerleave` flicker** on the lane hover state.

### Deferred minors and accepted-open items (nothing from the ledger dropped)

Fixed on the way, listed so nobody re-derives them: the resize-snap
regression and the drag/resize `cancel()` restore (Task 6/7), the
UTF-16-vs-bytes size guard, `startsWith` rejecting BOM/CRLF envelopes,
duplicated pasted tracks (`upsertTrack`, id-based dedupe — the THIRD
appearance of the async-ordering trap on this project), and the copy-time
toast's scope ("the system clipboard, in any program", because there is only
one and the harm reaches outside AURA).

**Accepted and still OPEN:**

- **An empty transaction bumps `rev`.** A payload where EVERY clip fails the
  tick rescale now opens a transaction that applies no ops: `rev` bumps and
  `project://changed` fires where the earlier code returned before
  committing. Ruling B's "commits nothing" holds in the OP sense; "no rev
  bump" does not.
- **`known_sources` can produce a same-`SourceId`/different-`source_path`
  pair.** Path normalization broke the one-source-one-path invariant in the
  direction `engine.rs:462` explicitly names. Verified benign: `stale_sources`
  SETTLES (one warning per rebuild plus one extra decode, not thrash) and
  both spellings name the same file, so the audio is correct. **Two one-line
  remedies were noted**: normalize `known_sources`' keys the same way the
  in-place lookup does, or key the lookup on the RAW stored path.
- **Copied assets are NOT rolled back by undo** — real garbage accumulation
  in `<project>/audio/`, and a FAILED commit orphans them with no clip at
  all.
- **A split mixed-kind row produces two tracks with the SAME `"<name> copy"`
  name.**
- **"No panic in `transact`" is an INSPECTION-grade property**, not a tested
  one, on both the Task 5 and Task 9 arms.
- **The tempo-map TOCTOU is CLOSED but UNTESTABLE.** `at_ticks` is now
  derived inside the transaction. The implementer could not build a
  deterministic RED for the race itself and refused to dress up a surrogate:
  the window needs a commit between phase 2 releasing the lock and phase 3
  taking it, `std::fs::copy` rejects non-regular files (so the FIFO trick
  does not open it), and holding the session lock from the test thread just
  moves the race. Accepted on a specific ground worth preserving: the change
  did not ADD a compensating CHECK (a mechanism with its own failure modes,
  where "nobody proved it fires" is a real hole) — it REMOVED the second
  read. A sabotage that splits the two readings apart proves the new test
  detects exactly the regression SHAPE the race could produce.
- **`midi.clips`' remaining blind append is provably safe ONLY because
  `project://changed` carries the audio-shaped `Project`.** If that ever
  carries MIDI clips too, this becomes the same duplication bug
  `upsertTrack` just fixed.
- **`midi.pasteAtPlayhead` rejecting is an unhandled rejection** — the `void`
  in `App.svelte` swallows it with no toast. Pre-existing form, but
  `pasteAtPlayhead` now OWNS "tell the user what happened", so the `catch`
  belongs there.
- **Cosmetic/nit:** the focused-clip comment exists only in `ClipView`, not
  `MidiClipView`; `onLaneDblClick` still has raw-`clientX` zoom drift;
  marquee never updates the selection ANCHOR (latent gap for a future
  shift-click range after a marquee); no test for subtract mode's anchor
  rule; `has()`/`audioIds()` build full `Set`s per call; the cancel-resize
  test does not assert the `contentLengthTicks` revert; two fast Ctrl+C are
  last-RESOLVED-wins.

### A PATTERN, not just six fixed defects: tests that could not fail

**SIX tests on this track could not fail** — seven counting the call-order
test that could not see asynchronous ordering — and each new mechanism was
found by RUNNING a sabotage, never by reading. The fourth's mechanism was
new. Three were weak assertions (a `reason.contains("track")` that the
wrong-kind message also satisfies; a sabotage that never reached the branch;
undo assertions that checked LENGTHS and never that the survivors were the
ORIGINALS — a sabotaged inverse removing the WRONG row passed). The fourth:
a test passed because deleting the code under test raised an exception that
an outer `catch` swallowed and **converted into the same return value the
correct code produces**. Reading the assertions could never reveal that —
only RUNNING the sabotage did. Two habits earned their keep here: sabotage
every test you are tempted to call obviously-correct, and when a store
method wraps its body in a `try`, check explicitly whether the early returns
sit inside or outside it (Task 12 did, which is why its three tests hold).

The FIFTH, found by the whole-track reviewer rather than by anyone inside
the track, is the one worth generalizing: **a whole suite can pin a
behaviour's every branch and still not pin that the behaviour is REACHED.**
Every paste test seeded an in-memory payload before calling `paste()`, so
all of them exercised the code AFTER the OS-clipboard scan and none could
notice the scan being gated away again — while the gate is precisely the
defect the track already fixed once. When a fix consists of REMOVING a
condition, the test that guards it must set up the state in which that
condition used to be false.

### A REPO-LEVEL GAP — an owner decision, not another review round

**This repo has NO DOM test environment** — no `jsdom`, no `happy-dom`, no
testing-library; `vitest.config.ts` runs plain node. **Four real defects on
this track lived exclusively in `.svelte` handlers, and all four were found
by READING, not by tests**: the Ctrl+V gate that made cross-instance paste a
no-op in a fresh window, the duplicated pasted tracks, the merge's
`MidiClipView` rename/drag interaction, and the marquee/automation-lane
collision — the last of which needed a hand-built stub of `closest` before it
could be tested at all, which is the cost of this gap made concrete. Track D reports two more of the same
shape. The stores here are thoroughly tested and the components are not
tested at all, so defects accumulate exactly where nothing is looking.
Adding a DOM test environment is a cross-cutting choice every future track
inherits — cost, CI time, house style — so it is stated here as evidence for
the owner to decide, and deliberately not decided by this track.

### Flaky tests seen on this branch (rerun in isolation, and CAPTURE the name)

`control::hum::tests::apply_hum_clip_commits_synchronously_and_announces_project_changed`,
`plugins::host::tests::plugin_main_thread_slots_and_tickers`,
`pure_readers::library_audition_core_leaves_the_document_and_the_history_untouched`,
plus the two `midi_out` ones in the Track B follow-up below. Zyn host spam on
stderr is expected noise. A grep that drops the failing test's NAME turns an
absence of evidence into a reported fact — this happened twice on this track;
capture the name before concluding anything.

### FOLLOW-UP OWED TO TRACK B (found here, already on `main` without it)

Two tests in `midi_out` are flaky under parallel load:
`shutdown_flushes_note_off_before_stop_and_before_the_connection_drops` and
`editing_the_routed_track_releases_a_sounding_note_instead_of_hanging_it`.
Recorded in `docs/backlog/hardware-midi-io.md` under "Still open after slice
2" with the full reading, the recommended fix and the discriminating check
that would falsify it.

## Track F handoff (2026-08-15/16, modulation system, subagent-driven, controller-led)

Track F (modulation graph — several curves per track, automation tracks,
clip envelopes) is **IMPLEMENTED** on branch `modulation-system`, cut for
the Track F work with HEAD at the close-out commit of this section. Design:
`docs/superpowers/specs/2026-08-15-modulation-system-design.md` (owner
rulings R1–R5). Plan:
`docs/superpowers/plans/2026-08-15-modulation-system.md`. ADR:
`docs/adr/0008-modulation-graph.md`. SDD ledger (gitignored):
`.superpowers/sdd/2026-08-15-modulation-system/progress.md`. Eleven tasks
(P0 foundation through P4 docs).

**What landed.** The document model is a modulation graph: `Curve`,
`Binding`, `AutomationClip` under `session.modulation`, persisted as
`project.json` **schemaVersion 4** with a one-way v3 `automation[]` →
`modulation{}` migration. Sources expand on the **control thread at
rebuild** into the same absolute-sample event lists Track D already fed the
RT path — looping, clip envelopes, and multi-target automation tracks cost
the audio callback nothing new. Track-gain remains a per-sample
`RampCursor` multiplier on the fader; **pan** is block-boundary-evaluated
with a per-block linear lerp of `(gl, gr)` (design §6.1, deliberate trig
cost). Plugin-param bindings rebuild `ParamAutomationDriver` from the graph
(Track D ruling 2 holds). UI: a track-header target picker and several
overlay lanes per track (incl. pan), a real automation-track kind with
clips and multi-target routing, and a piano-roll clip-envelope lane keyed
by content. Old `automation_get`/`automation_set` and
`Op::AutomationSetLane` stay as a **compatibility facade** over the graph
so old journals and any leftover callers still work.

**Per task** (controller-recorded SHAs):

| Task | Commit(s) | Summary |
|---|---|---|
| T1 | `bfa252d` | Document objects (`Curve`, `Source`, `TargetRef`, `Binding`, `AutomationClip`) |
| T2 | `ec2ea18` | Target resolution and per-target arbitration |
| T3 | `e4d941a` | Source expansion, looping, combination law |
| T4 | `11608c1` | Project v4 persistence + one-way v3 migration |
| T5 | `94b489e` | Session document, ops, rebuild wiring |
| T6 | `3a40bc9` | Per-track ramp table with pan |
| T7 | `abafca2` | IPC surface, lane compatibility facade, frontend mirror |
| T8 | `231b5d9` | Several automation lanes per track + target picker + pan |
| T9 | `26d1e11` + `c86e501` | Automation tracks; fix (drag, TrackRemove, plugin-param drive) |
| T10 | `8a0798a` + `1a1d589` | Clip envelopes; fix (`plugin:` prefix on instrument resolve) |
| T11 | this section | ADR 0008, handoff, `next-prompt.md` → design §8, suite re-measure |

**Suites: 731 backend (702 lib + 29 integration) + 299 frontend, all
green, measured on `modulation-system` 2026-08-16.** Counts dated in
README/CONTRIBUTING. Other tracks may be in flight; the count line is a
known cross-track merge-conflict point — whoever merges last re-measures
rather than picking a side. Doc-tests remain 0 and are not a test target.
No new side channel appeared; `docs/SIDE-CHANNEL-INVENTORY.md` is
untouched.

### Controller rulings (R5 implementer decisions, ADR 0007-marked)

Recorded in the SDD preflight and held through the round:

1. **T3 API names follow the implementation**, not the plan's draft names:
   `expand_repeated`, `CompiledSource`, `Contribution`, `compile()`. The
   landed API is a strict superset of the plan's five behaviour tests;
   later tasks follow `compile.rs`, not the draft identifiers.
2. **T10 wires placements; it does not rewrite compile.**
   `CompileCtx.content_placements` is filled from real `MidiClip`s;
   `expand_repeated` is unchanged. Cost if wrong would have been a second
   expansion path.
3. **T5 left `Op::AutomationSetLane` applying as today; T7 routed it
   through compat.** Old journals keep replaying; both document halves
   stay coherent until the facade owns the write.
4. **midi persist still stamps `schemaVersion: 3`** (deferred). A
   midi-only write after a v4 modulation save can sit `modulation{}` under
   version 3. Load still prefers `modulation{}` when present; explicit
   save upgrades. Not fixed in this round — no suite failure depends on
   it.
5. **P0 manual v3 open/play/save was not driven in-app.** Persist, open,
   and facade paths that gate depends on are test-covered; the owner's
   hand check remains owed (below).

### Non-goals held (design §9 — choices, not omissions)

LFO / macro / MIDI-CC / envelope-follower implementations; curve shapes
beyond linear; automation recording (write/touch/latch); sample-accurate
plugin params; `mute` and send targets (format + resolver arms that return
`None`); plugin-param automation in a bounce (Track D known divergence —
private instances per render); per-voice modulation; any change to Track
D's gesture/undo shape (rulings 4 and 7 stand). The ordered path that
finishes each of these is design §8 — linked from `next-prompt.md`, not
restated here.

### Divergences and known thin spots found on the way

- **Pan is block-lerped, gain is per-sample** (design §6.1) — deliberate;
  documented at the mixer call site so a later reader does not "fix" it.
- **Audio clips are not envelope-wired.** Clip envelopes key on
  `contentId` for MIDI; audio placements are sample-based with no
  `contentLengthTicks` loop model. Piano roll is MIDI-only this round.
- **Plugin-param bounce still ignores automation** (Track D carry-forward,
  design §6.3). Track-gain and pan ramps export; plugin params do not.
- **Overlays stack by last-picked** — behind lanes are not hittable
  without re-picking; `visible` is not pruned on adopt.
- **Plugin panel `A` no longer unbinds** (mint/show only); hide is via the
  lane picker.
- **Empty automation-clip body click no longer inserts a point** after the
  T9 fix (drag the clip / hit a vertex). First click on a clip envelope
  can mint without inserting a point; hide-envelope is a no-op in places.
- **Curves are not deleted with their track** — orphan curves can remain
  until GC/edit.
- **App never driven end-to-end** for pan, multi-target automation tracks,
  or looping clip envelopes. Same constraint as Track D: implementing
  agents had no audio device. Offline render tests cover the shapes.

### OWED BY THE OWNER

1. **Ear checks that no suite substitutes:** draw pan on a track and
   hear L/R move; bind one automation track to two targets and hear both;
   loop a MIDI clip with an envelope and hear bar 1 match bar 3.
2. **P0 migration hand check:** open a real v3 project with a gain lane,
   play, save, confirm `schemaVersion: 4` and `modulation{}` with no
   behaviour change.
3. Everything still open from Track D that this round did not claim
   (gesture token, non-blocking CLAP path, plugin-param bounce, DOM test
   environment) remains open — see the Track D handoff above.

### Path forward

Design §8 is the contract for the rounds after this one. ADR 0008 records
why tagged `TargetRef` is a step toward ports. Fresh sessions start at
`next-prompt.md`, which links that section rather than restating it.

