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
- Standing caveat: `transact` has no panic rollback — transaction
  closures must stay non-panicking until Plan F's snapshot rollback.
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
- Standing from Plan A/B, unchanged: snapshot-rebuild deferral
  (`engine::rebuild` still holds the session lock for the whole graph
  build — needs Plan F); no panic rollback in `transact` (Plan F);
  `fold_ops` coalescing constraint before `Committed.ops` is ever
  persisted (Gate E); the op log stays dark — none of this plan's tempo/
  meter/content work is `Session::transact`-routed, `set_tempo_map` and
  the MIDI clip commands remain outside the A-slice channel exactly as
  before this plan, binding for Plan E's side-channel inventory (§4.5) to
  close.

## Plan E handoff (2026-08-14, subagent-driven session, external review layer)

Plan E (the side-channel totality, round-2 §4.5, ADRs 0003 + 0006) is
**IMPLEMENTED** (commit range `ac65b76..531d790`, 51 commits, including 7
merges — 6 side-branch task merges (`task-9-plugin-doc`,
`task-10-automation-doc`, `task-11-frontend-stems`, `task-12-transport`,
`task-15-noteid-schemas`, `task-16-steady-time`) plus one `origin/main`
merge (`f886306`, pulling in PR #9 hscrollbars / #10 zoom-pref / #11
note-ops mid-flight)). **Gate E is CLOSED**: the Figma invariant (§7 test 4
— reads mutate nothing, a scripted mixed session over every op family, undo
through new commits, a pure-reader sweep, redo, byte-identical canonical
snapshot) is green against the code exactly as Tasks 1-16 left it —
provably so, because the commit that adds the test, `73bb9a7`, touches
**zero production files** (`src-tauri/tests/figma_invariant.rs` only, 748
insertions). The 34-row side-channel inventory closes total, recorded in
`docs/SIDE-CHANNEL-INVENTORY.md` (landed form of the plan doc's re-derived
table, with three residual documented non-op writes R-1..R-3 and three
recorded replay limitations L-1..L-3 — see that doc, not repeated here).
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

### Mid-flight rulings (from the SDD ledger, each with its one-line why)

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
- **The journal is write-only until Plan F.** `JournalWriter` appends
  `journal.ndjson`; nothing in the product reads it back. Plan F's
  version-graph retention is what gives the journal a reader.
- **Undo is bounded at 200 entries, bottom-eviction.** In-memory `Vec`,
  cleared at epochs (scope ruling 9) — Plan F replaces the storage
  substrate, not the exposure.
- **Snapshot-rebuild deferral STILL standing** (from the Plan A handoff,
  reconfirmed unchanged by every subsequent plan): `engine::rebuild`
  still holds the session lock across the entire graph build. Plan E's
  Task 13 gave the engine a `Committer` that submits instead of holding
  the lock for writes, but rebuild-the-read-side is untouched — it needs
  Plan F's copy-on-write session store to read an immutable view without
  the live document lock.
- **No panic rollback in `transact`** (Plan F). Every closure in this
  plan validates before mutating, per the standing constraint; the
  constraint itself does not lift until Plan F's snapshot rollback lands.
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
  gesture batch that closes over them) — nothing CHECKS this; it
  constrains what may ever be marked transient. Binding on any future
  transient-tx addition.
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
  undo — see L-1 above (Task 9).
- `plan_region_replacement`'s doc cites `replace_region_clips` but
  diverges on right-only-trim (`TrimRight` in place); the LoopJam mid-air
  race is code-inspected, not test-forced (Task 8).
- Deadlock-audit comment's five call-site line citations went stale by
  +129 lines after later edits — sites and the invariant itself stayed
  correct (Task 13).
- `midi/synth.rs` test-literal compile fixup, touched despite the
  `midi/*` scope note — mechanical, zero behavior (Task 16).
- `ClapNode::reset()` not re-verified to leave `steady_fallback` alone —
  consistent with pre-change semantics, not re-audited (Task 16).
- A stale line-number citation in a doc comment (516 vs. 508);
  `tempoBpm` required-vs-conditional tension pre-existing in the v2
  schema (Task 15).
