# Next: author and execute Plan C+D (time + project v3)

Read this file, then do the work described below. Reply to the user in
Norwegian — they write Norwegian; the repo documentation is English.

## Where you are

Working directory: `/home/knobo/prog/dav/.claude/worktrees/zrythm-arch`
(a git worktree — run everything from here, do **not** `cd` to the main
checkout, and never use bare `git stash`). Branch `worktree-zrythm-arch`,
open as PR #1 on `knobo/aura-daw`.

The project is **AURA**, an AI-native DAW: Tauri v2 + Svelte 5 around a
lock-free real-time Rust engine (`src-tauri/`), local AI sidecars, and an
embedded MCP server so agents mutate the session alongside the user.

## What has happened (all of it is in git — trust files over memory)

1. **Research round** (14 agents): `docs/research/` — eleven dossiers,
   start at `00-INDEX.md`. Dossiers 06/10 carry marked `[CORRECTED]`
   blocks from the later review. History, not law.
2. **Adversarial review**: `docs/CORE-REDESIGN-ROUND-2.md` is **ACCEPTED**
   and binding; it supersedes round 1 and lists all fourteen overturned
   positions in its §0.1. ADRs 0001-0007 in `docs/adr/` are all Accepted.
3. **Measurements**: `benches/ui-probe/RESULTS.md` (three WebKitGTK
   render-gate measurements — all PASS) and `benches/bulkbench/RESULTS.md`
   (storage gesture mix — replay-only threshold 64 KB). Evidence policy
   (ADR 0007): measured claims cite `benches/`.
4. **Plan A is IMPLEMENTED** (`ded3670..405af0b`, 12 commits): `Session`
   over Store+MidiStore behind one lock; `Session::transact` as the only
   mutation door for the A-slice (mix/add/remove track); `control::op`
   vocabulary v1 with actor/run meta; inverses derived from store truth;
   effects executed strictly after the lock. See `docs/PHASE4-PLAN.md`'s
   "Plan A handoff" section for its binding carry-forwards (snapshot-
   rebuild deferral chief among them).
5. **Plan B is IMPLEMENTED** (`a31104f..a70bf67`): identity groundwork
   — typed id families end to end through the op wire form; persisted note
   identity (AMEV columnMask-honest); source-keyed assets + decode cache;
   `Store`-owned slots deleted, replaced by per-graph `derive_slots` +
   `GraphTables` (round-2 O-13's aliasing window closed by construction);
   meter blocks carry the graph generation; `MAX_TRACKS` removed. Gate B
   green (`src-tauri/tests/identity_properties.rs`). See
   `docs/PHASE4-PLAN.md`'s "Plan B handoff" section for its binding
   carry-forwards (id-preserving piano roll deferred to Plan E;
   `next_note_id`'s move to the content object and the split/merge/copy
   rules bind Plan C/D directly). Suites: **346 backend + 80 frontend**,
   all green, counts dated in README/CONTRIBUTING.

## Your task

**Author Plan C+D — time + project v3 — then execute it.** Scope was
owner-approved in round 2. These two sub-plans ship as ONE format bump, not
two (`docs/PHASE4-PLAN.md`'s sub-plan table: "D ships inside C's v3
migration") — do not split them into separate schema versions. Do not start
implementation before the plan document exists.

### Step 1 — read, in this order

1. `docs/PHASE4-PLAN.md` — sub-plan C and D rows, Gate C/D, the binding
   rules, and **both** the "Plan A handoff" and "Plan B handoff" sections
   (carry-forwards: snapshot-rebuild deferral; `fold_ops` coalescing
   constraint before Gate E; `next_note_id`'s move to the content object;
   split/merge/copy rules binding future content ops; the waveform pyramid
   cache and `GraphTables`-under-Mutex items ledgered, not taken).
2. `docs/CORE-REDESIGN-ROUND-2.md` §3 (time — supertick tempo, the section
   table, `steady_time`, the v2→v3 migration) and §5 (content and
   placement — `ContentId`, placements with `LaneId`, default lanes), plus
   §0.1 rows O-4, O-5, O-9 (time) and whichever rows §5's own text points
   at for the placement split. ADR 0002 (time model) and ADR 0004
   (content/placement split), both in `docs/adr/`.
3. The landed code (round-2 §3/§5 predate Plans A/B — the tree wins):
   - Time: `src-tauri/src/midi/types.rs` (`TempoEvent { tick, bpm }`,
     `MidiClip.next_note_id`), `src-tauri/src/audio/types.rs`
     (`TransportState.tempo_bpm`, sample-based fields throughout —
     `position_samples`/`loop_start_samples`/`loop_end_samples` etc.;
     everything here is samples-or-bpm today, no `Ticks`/`Samples`
     newtypes yet), the frontend tempo/tick math: `src/lib/tauri.ts`,
     `src/lib/demo.ts`, `src/lib/types/ipc.ts` (`TempoMapState`,
     `TempoEvent`), `src/lib/state/midi.svelte.ts` (its own header comment
     says it mirrors `midi::TempoMap` — the piecewise tick<->sample math
     (`ticksToSamples`/`samplesToTicks`) and `tempoEvents` state live here,
     not behind any single named TS type; Gate C/D's frontend exit
     condition is this piecewise math deleted, replaced by consuming the
     shipped section table — read what it does today before deleting it).
   - Project v3: `src-tauri/src/audio/project.rs` (`schema_version` field,
     the `(1..=2).contains(&project.schema_version)` validation gate near
     line 161 — this is what a v3 bump must extend, and what a fixture
     corpus for lossless v2→v3 round-trip must exercise against).
   - Content/placement: `src-tauri/src/ids.rs`'s `ContentId` (declared,
     unpopulated — Plan B's handoff note); `src-tauri/src/audio/types.rs`'s
     `Clip` (today IS the placement+content conflation §5 splits apart).
4. Only if re-litigating a decision: dossier 02 (DAW engine architecture —
   §5 on Ardour/Tracktion tempo maps and time representation), dossier
   07's relevant rules, dossier 10's relevant sections, via
   `docs/research/00-INDEX.md`.

**Verified code anchors** — re-checked against the tree at Plan B's real
closing commit (`a70bf67`) while writing this file. Symbols still drift
over time; grep for the name, don't trust the line number.

### Step 2 — write the plan

Load `superpowers:writing-plans`; save as
`docs/superpowers/plans/<today>-plan-c-d-time-project-v3.md` with the
standard header (Spec: round-2 §3 + §5, ADRs 0002 + 0004; orchestration
PHASE4-PLAN). The plan author owns the task cut — Plan B's own plan
document (`docs/superpowers/plans/2026-08-13-plan-b-identity-groundwork.md`)
is a reasonable template for shape (files/interfaces/steps/gate per task),
and its SDD ledger (`.superpowers/sdd/2026-08-13-plan-b-identity-groundwork/`)
shows how a compressed, deadline-driven pipeline was actually run if that's
useful precedent.

Gate C/D, verbatim from `docs/PHASE4-PLAN.md`: *test 6 (section-table bound
< 64 samples), test 7 (tempo round-trip + lossless v2→v3 against a fixture
corpus, including meterMap and the placement/content split); frontend
consumes the shipped section table (TS TempoMap deleted).*

### Step 3 — execute

`superpowers:subagent-driven-development`: fresh implementer per task, task
review after each, scoped re-review per fix round, final whole-branch
review on the most capable model, ledger in the plan's own SDD workspace
(`.superpowers/sdd/`). Model policy that worked for Plans A and B:
implementers sonnet (haiku only for pure transcription), task reviewers
sonnet, riskiest task(s) + the final review on the most capable model.
**Foreground test runs only, with `timeout` guards.** Commit per task; push
to PR #1 when the final review is clean (established flow). Update dated
test counts when totals change.

## Ground rules (binding, from PHASE4-PLAN + ADRs)

- **The op log stays dark until Gate E.** No journal writes, no undo UI.
- **Thin renderer** (ADR 0006, owner-accepted): no authoritative state,
  business logic, or time math lands frontend-side.
- **Frozen command/event names stay frozen**; bodies become wrappers; new
  commands are additive.
- **Prepare-outside/commit-inside** for I/O; no blocking engine
  round-trips inside a transaction; `transact` closures must not panic
  (no panic rollback until Plan F).
- **One format bump, not two** — C and D ship inside the same v3
  migration; do not stage a separate schema version for the placement
  split.
- Corrections to docs are marked, never silent (ADR 0007).
- The user's standing instruction: *"ikke ver skjenert"* — use as many
  agents as the work needs, and say plainly when something is wrong.

---

*Working note, committed on the PR branch for session continuity. When
Plan C+D lands: update this file for the next stage (Plan E — the
side-channel totality) in the same commit that closes Plan C+D.*
