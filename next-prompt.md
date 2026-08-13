# Next: author and execute Plan B (identity groundwork)

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
   positions in its §0.1. ADRs 0001–0007 in `docs/adr/` are all Accepted.
3. **Measurements**: `benches/ui-probe/RESULTS.md` (three WebKitGTK
   render-gate measurements — all PASS) and `benches/bulkbench/RESULTS.md`
   (storage gesture mix — replay-only threshold 64 KB). Evidence policy
   (ADR 0007): measured claims cite `benches/`.
4. **Plan A is IMPLEMENTED** (`ded3670..405af0b`, 12 commits): `Session`
   over Store+MidiStore behind one lock; `Session::transact` as the only
   mutation door for the A-slice (mix/add/remove track); `control::op`
   vocabulary v1 with actor/run meta; inverses derived from store truth;
   commit-time fold with no-op elision; effects (param writes, ≤1
   Rebuild, exactly 1 `project://changed` with full-Project + additive
   rev/label/actor) executed strictly after the lock; structural ops
   carry their clips; Gate A property suite green
   (`src-tauri/tests/channel_properties.rs`). Suites: **296 backend + 17
   frontend**, all green, counts dated in README/CONTRIBUTING.

## Your task

**Author Plan B — identity groundwork — then execute it.** Scope was
owner-approved in round 2 (incl. the MAX_TRACKS removal). Do not start
implementation before the plan document exists.

### Step 1 — read, in this order

1. `docs/PHASE4-PLAN.md` — sub-plan B row, Gate B, the binding rules, and
   **§"Plan A handoff"** (snapshot-rebuild deferral + six carry-forwards).
2. `docs/CORE-REDESIGN-ROUND-2.md` §2 (identity) and §0.1 rows O-2, O-3,
   O-12, O-13, O-14. ADR 0001.
3. The landed code (round-2 §2 predates Plan A — the tree wins):
   `src-tauri/src/control/op.rs`, `src-tauri/src/control/session.rs`,
   `src-tauri/src/control/mod.rs`, `src-tauri/src/audio/types.rs`,
   `src-tauri/src/audio/engine.rs` (rebuild + decode cache),
   `src-tauri/src/audio/meters.rs`, `src-tauri/src/midi/events.rs`,
   `src-tauri/src/midi/types.rs`.
4. Only if re-litigating a decision: dossier 04 §4 (addressable-object
   problem), dossier 07 rules 1–4/17/18, dossier 10 §1.9/§2.3 via
   `docs/research/00-INDEX.md`.

### Step 2 — write the plan

Load `superpowers:writing-plans`; save as
`docs/superpowers/plans/<today>-plan-b-identity-groundwork.md` with the
standard header (Spec: round-2 §2 + ADR 0001; orchestration PHASE4-PLAN).

**Verified code anchors** (symbols checked at `405af0b` — grep for them;
line numbers drift):

- `OP_FORMAT_VERSION: u16 = 1` and
  `ObjectRef { Track(String), Clip(String), MidiClip(String) }` —
  `control/op.rs`. Typed id families must serialize **transparently**
  (`#[serde(transparent)]` newtypes over String) or bump the version —
  op wire form is gated by exact-JSON tests in op.rs.
- `MAX_TRACKS: usize = 64` — `audio/types.rs:13`; `slot_used:
  [bool; MAX_TRACKS]` + `alloc_slot`/`free_slot`/`track_slots` —
  `audio/types.rs` (~:205–250). The u64 presence-mask assumption lives in
  `audio/meters.rs` (`RawMeterBlock`) and `audio/rt.rs` (fixed atomic
  arrays).
- AMEV chunk format — `midi/events.rs`: `AMEV_MAGIC`, `AMEV_VERSION: u16
  = 1`, `COLUMNS_CORE: u16 = 0x0001`; header comment documents the
  columnMask + appended-column-block extension path; **the reader
  currently ignores the mask** (`let _columns =`) — the note_id column
  must fix that read path too. `MidiNote` (no id today) — `midi/types.rs`.
- Decode cache — `audio/engine.rs` `cache: HashMap<String, Arc<RtClipData>>`
  keyed by **clip id** (comment "clip id -> decoded samples"); no
  invalidation when a clip's `source_path` changes under the same id.
  Re-key by source. Justify per round-2 §2.2 (dedup / GC / staleness —
  NOT the falsified "wrong audio" claim).
- Slot lifecycle today: alloc + param reset in `ControlPlane`'s track
  creation (under a session lock, before commit); free in its own lock
  after commit (`control/mod.rs::remove_track`); `engine::rebuild` maps
  slots via `store.track_slots()` while holding the session lock (the
  snapshot-rebuild deferral — do NOT try to fix that here, Plan F owns it).

**Task-cut sketch** (the plan author owns the final cut; each task ends
independently testable, TDD steps, foreground `timeout`-guarded runs):

1. Carry-forward batch (small, same-shape): delete dead
   `ops::apply_track_mix`; clamp round-trip test (999→24); duplicate-id
   guard in `apply_raw`'s `TrackAdd` arm (reject, don't clamp — rollback
   removes-by-id and can mis-target on duplicates); positional clip
   restore (or canonical clip ordering) so byte-identity holds on
   interleaved clip vecs — this is a Gate B prerequisite.
2. Typed id newtypes (`TrackId`, `ClipId`, `ContentId`, `NoteId`,
   `SourceId`, …) as `#[serde(transparent)]` wrappers; `ObjectRef`
   migrates family by family; wire-form tests extended, version stays 1.
3. `note_id: u32` on `MidiNote` + persisted per-content watermark in the
   AMEV header (one u32, new columnMask bit; v1 chunks without the column
   read tolerantly, writes upgrade); allocation only inside a
   transaction; fix the reader's ignored-mask path.
4. Source-keyed asset naming (`audio/<sourceId>.wav`) + source-keyed
   decode cache with source-change invalidation; project-file field is
   additive (D-06 readers ignore unknowns).
5. Slots become derived per-graph state: kill `free_slot` reuse-in-place;
   **each compiled graph carries its own param table** populated at build
   time; smoothing/live-node state keyed by TrackId moves across rebuilds
   (pattern exists: `midi/playback.rs` registry).
6. Meter blocks carry the graph generation; the control thread folds
   under the matching slot map (`engine.rs` meter fold + `meters.rs`).
7. MAX_TRACKS removal: per-graph sizing replaces the fixed arrays and the
   u64 presence mask (chunked meter blocks per D-05's direction).
8. Gate B property/integration tests: delete-then-undo preserves identity
   AND inbound references (now meaningful with typed ids + clips-on-ops);
   the slot-aliasing race test (delete→add→old graph still playing —
   headless, using the retire/adopt seam); meter-generation test.

Order matters: 1 → 2 → 3/4 (parallel-safe) → 5 → 6 → 7 → 8. Task 5–7 are
the RT-heavy part — their reviews go to the most capable model.

### Step 3 — execute

`superpowers:subagent-driven-development`: fresh implementer per task,
task review after each, scoped re-review per fix round, final
whole-branch review on the most capable model, ledger in the plan's own
SDD workspace (`scripts/sdd-workspace`). Model policy that worked for
Plan A: implementers sonnet (haiku only for pure transcription), task
reviewers sonnet, riskiest + final review fable. **Foreground test runs
only, with `timeout` guards** — a Plan A implementer wedged three times
on background waits. Commit per task; push to PR #1 when the final
review is clean (established flow). Update dated test counts when totals
change.

## Ground rules (binding, from PHASE4-PLAN + ADRs)

- **The op log stays dark until Gate E.** No journal writes, no undo UI.
- **Thin renderer** (ADR 0006, owner-accepted): no authoritative state,
  business logic, or time math lands frontend-side.
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
Plan B lands: update this file for the next stage (Plan C+D — time +
project v3) in the same commit that closes Plan B.*
