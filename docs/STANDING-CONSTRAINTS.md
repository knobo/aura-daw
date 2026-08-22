# Standing constraints — these bind every task, on every track

Read this before your first commit on a branch. Nothing here is
negotiable by a single task; changing one of these is its own PR with its
own argument.

Historical plans called this `next-prompt.md` §2.

## Branch and worktree rules

- **Every job starts in its own worktree, cut from `origin/main`:**

  ```sh
  git fetch origin
  git worktree add .worktrees/<short-name> -b <branch-name> origin/main
  ```

  Both halves matter, and each one has already cost this project a
  session. [`CLAUDE.md`](../CLAUDE.md) carries the why. A worktree whose
  branch has merged is **spent** — do not reopen it and do not branch
  from it.
- **Claim the job in [`next-prompt.md`](../next-prompt.md) before you
  write code**, and push that claim immediately. Two agents finishing the
  same task is the most expensive failure this project has had; see the
  protocol at the top of that file.
- **Continuing branches merge `origin/main` in** whenever it has
  advanced, at a task boundary — don't let a long-running branch drift.
- **Never use bare `git stash`.** If you need to shelve work, commit it
  or use a named stash; bare `stash` has bitten prior sessions when a
  worktree switch lost track of it.
- **Foreground test runs only, `timeout`-guarded.** No backgrounding a
  test run and moving on — every gate in this project has been foreground
  since Plan A.

  ```sh
  timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
  timeout 300 npx vitest run
  ```

## Architecture

- **Thin renderer** (ADR 0006) still holds: no new authoritative state,
  business logic, or time math lands frontend-side. Every frontend change
  is op emission, gesture emission, or UI/chrome.
- **Frozen command/event names stay frozen**; bodies become wrappers; new
  commands are additive (the same rule that shaped every Plan E task).
- **`src-tauri/src/theory/` is PURE and stays pure** (Plan H1, ruling
  H-5): no `tauri`, `parking_lot`, `std::fs`, `crate::control`,
  `crate::audio`, and no `thread_rng` — every generator takes an explicit
  `seed`. The only sanctioned crate dependency is `crate::midi::MidiNote`.
  A generator that needs project data is passed it. Same class of rule as
  the RT contract: it is what keeps that suite fast and that library
  reusable.
- **The harmony document is a map pair, not a track** (H-3/H-5): one op
  (`Op::HarmonySet`), no `rebuild` effect, `OP_FORMAT_VERSION` still 2,
  `schemaVersion` unmoved, and the `project.json` key written only when
  the document is non-empty. Do not add a `TrackKind` or a second harmony
  op.

## The op log and undo

- **The op log is ON.** `journal.ndjson` is a **persisted format** from
  now on: `OP_FORMAT_VERSION` (**2** since the post-merge follow-up PR —
  base64 state blobs, I-5) is load-bearing the moment any project has a
  journal file. Additive `#[serde(default)]` fields on an op or on
  `TxMeta` stay non-breaking; anything else (renaming a field, changing a
  variant's shape, removing a path) needs a version bump AND a reader
  that understands both shapes. Plan F's journal reader
  (`control/replay.rs`) requires `v == 2` on batch lines (ruling F-5); v1
  lines are skipped with a warn. A change of this kind now costs a
  migration.
- **`transact` closures must not panic.** Plan F landed panic
  *containment* (`catch_unwind` + restore from the pre-tx snapshot;
  ruling F-3) — that is a crash-consistency net, not a license. Validate
  before mutating, every time, exactly as every landed op arm does.
- **The M-3 redo invariant**: a transient write must never touch a
  document field an entry's `ops` can address, or a pending redo silently
  lands on a different state than the entry recorded. If you add a new
  transient transaction (transport-like, engine-thread, or gesture
  mid-flight), this is the check to run before shipping it. As of the
  post-merge follow-up PR a `debug_assert!` in the commit path enforces
  it in debug builds (transient ops may address only
  `ObjectRef::Transport`, unless the batch is a mid-gesture fold) — so a
  violation now fails the test suite instead of waiting to be noticed.
- **Undo is bounded**: 200 entries, in-memory, bottom-eviction, cleared
  at epochs (project open/create/save-as). The journal is unbounded and
  append-only; Plan F added a reader (detection + primitive, no
  auto-apply — ruling F-8).
- **Gesture lock order is gesture-before-session, everywhere** (Task 14's
  fix) — if a track adds a new gesture-shaped commit path, follow this
  order or reintroduce the TOCTOU that fix closed.

## Tests and docs

- **The dated-count convention**: any task that changes test counts
  updates `README.md` + `CONTRIBUTING.md` in the same commit, with the
  date. Current counts live there — measure, do not copy a number from a
  briefing.
- **Pure logic goes in `src/lib/utils/` with a `*.test.ts`**; component
  behaviour goes in `*.dom.test.ts`.
- **Theme tokens only** in every `<style>` block — see
  [`TRAPS.md`](TRAPS.md).
