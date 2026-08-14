# Next: author and execute Plan E (the side-channel totality)

Read this file, then do the work described below. Reply to the user in
Norwegian — they write Norwegian; the repo documentation is English.

## Where you are

Working directory: `/home/knobo/prog/dav/.claude/worktrees/zrythm-arch`
(a git worktree — run everything from here, do **not** `cd` to the main
checkout, and never use bare `git stash`). Branch `plan-cd-time-v3`, open
as PR #7 on `knobo/aura-daw` (merge it into `main` before starting Plan E,
or branch Plan E's own work from wherever the merge lands — the previous
session's own judgment call, not a rule this file dictates).

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
   carry-forwards.
6. **Plan C+D is IMPLEMENTED** (`57acfa9..9aee08a`, 10 tasks, one v2→v3
   format bump): `Ticks`/`Samples` newtypes; integer-period supertick
   tempo (quantized once at entry, exact thereafter); a persisted meter
   map (fixes the active 4/4-clobber data-loss bug); a section table with
   a property-tested <64-sample ramp-subdivision bound (**Gate C/D test
   6, green**); the v2→v3 migration with a fixture corpus (**Gate C/D
   test 7, green for tempo/meter/MIDI content-placement**); the
   content/placement identity split — full for MIDI, addressing-only for
   audio (scope ruling); the frontend's shipped-section-table consumption
   (`src/lib/sectionTable.ts` is the one bijection implementation left
   client-side, TS `TempoMap` duplicate deleted). Suites: **372 backend +
   85 frontend**, all green. See `docs/PHASE4-PLAN.md`'s "Plan C/D
   handoff" section for its binding carry-forwards — **read it in full
   before starting Plan E**; it names three scope rulings (audio
   addressing-only, no runtime Content/Placement struct split,
   `steady_time` out) that Plan E inherits, plus a stale-docs item
   (`docs/ipc-schemas/midi-clip.schema.json`/`project-v2.schema.json` not
   yet updated for v3) worth fixing while Plan E is already touching
   this area.

## Your task

**Author Plan E — the side-channel totality — then execute it.** This is
round-2's own stated turning point: "E is last on purpose: it is the
totality migration, and its exit gate is the one that turns the op log
on" (PHASE4-PLAN's sub-plan table). Do not start implementation before
the plan document exists.

### Step 1 — read, in this order

1. `docs/PHASE4-PLAN.md` — sub-plan E row, Gate E, the binding rules, and
   **all three** of the "Plan A handoff", "Plan B handoff", and "Plan C/D
   handoff" sections. Plan E's own row lists its scope: "all 14+ paths
   through the channel, engine-thread inversion, gesture begin/end IPC,
   `clip.move` op + command, id-preserving piano roll, mutating readers
   become pure, remaining three stores into Session."
2. `docs/CORE-REDESIGN-ROUND-2.md` §4.5 (the side-channel migration — was
   §6 in round 1; O-1 folded it into THIS round, O-8's corrected ~14+-path
   inventory) and §4.2-§4.4 (the engine thread submits/never holds; two
   API tiers and anti-nesting; gestures/effects/inverses). §7 (testing)
   for test 4 (the Figma invariant — reads mutate nothing) and Gate E's
   exact exit condition.
3. The landed code — grep for the actual side channels rather than
   trusting the count above (dossier 10 §2.3 + the review's additions is
   the inventory Gate E checks against; the previous sessions did NOT
   re-verify it against current HEAD, so re-derive it): known items
   already named across the three handoff sections —
   `with_synced_store`/`apply_hum_clip` holding the document lock across
   midi disk I/O (Plan A handoff), two *read* commands that mutate (lazy
   disk resync, O-8), clip-move never reaching the backend (O-8, still
   true — `midi.svelte.ts`'s `moveClip` is explicitly "frontend-only
   placement move (no midi clip-move command in the surface)" per its own
   doc comment, unchanged by Plan C+D), `midi_set_notes` still replacing
   arrays wholesale so ids churn on every gesture even though the
   allocator/keep-rule is correct (Plan B handoff — the frontend
   `MidiNote` TS type still carries no `noteId` field, check whether that
   changed), `fold_ops` coalescing Sets across intervening structural ops
   (harmless dark, wrong for replay — must be constrained before
   `Committed.ops` is ever persisted, i.e. before this gate turns the log
   on).
4. Only if re-litigating a decision: dossier 10 (side channels' original
   home) via `docs/research/00-INDEX.md`.

### Step 2 — write the plan

Load `superpowers:writing-plans`; save as
`docs/superpowers/plans/<today>-plan-e-side-channel-totality.md` with the
standard header (Spec: round-2 §4.5 + §4.2-§4.4, ADR 0003 (op log stays
dark until this gate) + ADR 0006 (thin renderer); orchestration
PHASE4-PLAN). Plan C+D's own document
(`docs/superpowers/plans/2026-08-14-plan-c-d-time-project-v3.md`) is a
reasonable template for shape (global constraints, scope rulings decided
up front and documented rather than discovered mid-task, per-task
Files/Interfaces/Steps/commit, a self-review section, an execution note).

Gate E, verbatim from `docs/PHASE4-PLAN.md`: *the side-channel inventory
inside the channel is total (checked against dossier 10 §2.3 + review
additions, all 14+ paths); test 4 (Figma invariant — reads mutate
nothing) passes; only then does the op log/journal turn on. This gate is
the round's whole point; it does not get negotiated down.*

### Step 3 — execute

Ask the user (or check for a standing instruction like the one Plan C+D's
session received) whether tonight's execution is solo or
subagent-driven — Plan E's default per `docs/PHASE4-PLAN.md` line 120 is
`superpowers:subagent-driven-development` (fresh implementer per task,
task review after each, final whole-branch review on the most capable
model), but the last two sessions both ran solo under a binding
per-session constraint from the owner. Don't assume either way.

**Foreground test runs only, with `timeout` guards.** Commit per task;
push to a new PR when the first task lands green, then at every
subsequent task boundary (same flow as Plan C+D). Update dated test
counts when totals change. Current baseline: **372 backend + 85
frontend**, both green — verify this before Task 1, the same way every
prior session did.

## Ground rules (binding, from PHASE4-PLAN + ADRs)

- **The op log stays dark until Gate E.** No journal writes, no undo UI —
  until THIS gate closes, at which point it turns on. Read ADR 0003
  before touching anything that looks like journal-adjacent code.
- **Thin renderer** (ADR 0006, owner-accepted): no authoritative state,
  business logic, or time math lands frontend-side. Plan E's own scope
  includes "mutating readers become pure" and moving `clip.move` behind a
  real op + command — both are thin-renderer compliance work, not
  exceptions to it.
- **Frozen command/event names stay frozen**; bodies become wrappers; new
  commands are additive.
- **Prepare-outside/commit-inside** for I/O; no blocking engine
  round-trips inside a transaction; `transact` closures must not panic
  (no panic rollback until Plan F).
- Corrections to docs are marked, never silent (ADR 0007).
- The user's standing instruction: *"ikke ver skjenert"* — use as many
  agents as the work needs, and say plainly when something is wrong.

---

*Working note, committed on the PR branch for session continuity. When
Plan E lands: update this file for the next stage (Plan F — history
storage) in the same commit that closes Plan E.*
