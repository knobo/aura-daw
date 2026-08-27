# Plan V — V1: `MixNode` as the graph compiler's input

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One node type is what the graph compiler takes as input.
`compile_inserts` and `compile_routing` stop reading `TrackState`; they read
a `MixNode`, and tracks and buses become *producers* of it. **Nothing
observable changes** — that provable neutrality is the whole point of the
cut, because it is what makes V2 (a player that is not a track) reviewable.

**Architecture:** A new `src-tauri/src/audio/node.rs` holds
`MixNode { id, kind, gain_db, pan, muted, soloed, inserts, sends, output }`
plus `From<&TrackState>` and `mix_nodes(&[TrackState]) -> Vec<MixNode>`. The
producer is **total over the document's tracks** — one node per track, in
document order, automation tracks included — so every downstream filter sees
exactly the rows it sees today. `compile_inserts` and `compile_routing`
change their first parameter's type and nothing else; their bodies swap
`is_bus_track(t)` / `is_mixer_track(t)` for methods on `MixNode`. The RT
graph, `ParamTable`, `derive_slots` and the mixer do **not** change: they
already think in slots and flags.

**Tech Stack:** Rust (`src-tauri`), `cargo test`. No frontend change, no IPC,
no document change, no schema bump.

**Spec:** [`docs/backlog/plan-v-players.md`](../../backlog/plan-v-players.md)
§ "V1", and rulings V-1…V-12 in
[`2026-08-26-plan-v-players-design.md`](../specs/2026-08-26-plan-v-players-design.md).
The two that bind this cut:

> **V-2** A player is **not a track**. […] Making players hidden tracks
> would force every timeline feature to learn to ignore them.
>
> **V-3** Tracks, buses and players all compile to one **`MixNode`**. New
> code targets `MixNode`; `Track`/`Bus`/`Player` are three producers of it.
> The RT graph already thinks in slots + flags and does not change.

## Global Constraints

From [`docs/STANDING-CONSTRAINTS.md`](../../STANDING-CONSTRAINTS.md) and the
backlog; every task inherits them.

- **`MixNode` must not inherit a timeline field.** No `clips`, no `armed`,
  no `automation_mode`, no `instrument_id`, no `color`, no `group`. The
  backlog names this explicitly: *"if `MixNode` grows a `clips` field, V1
  has failed and V2 will inherit the hidden-track trap"*. If a task seems to
  need one, stop and report rather than adding it.
- **Behaviour-neutral means byte-identical.** No test's *expectations*
  change. A test may change how it *builds* its input (a `MixNode` instead
  of a `TrackState`), never what it asserts. If an assertion has to move,
  that is a defect in the refactor, not in the test.
- **`bus::compile_routing`'s DAG/cycle tests stay untouched** apart from
  input construction.
- **No new IPC command, no document/serde change, no `OP_FORMAT_VERSION` or
  `schemaVersion` bump.** `MixNode` is a compile-time value, never
  serialized.
- **Thin renderer (ADR 0006):** nothing frontend-side changes at all.
- **Foreground, `timeout`-guarded test runs only:**

  ```sh
  timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
  timeout 300 npx vitest run
  ```

- **The dated-count convention:** if the Rust test count moves, `README.md`
  and `CONTRIBUTING.md` are updated in the same commit, with the date —
  measured, never copied from this plan.

## Task 1 — the bounce-identity gate, recorded BEFORE the refactor

The backlog's real gate is *"a bounce of the demo project is byte-identical
before and after"*. There is no demo project fixture in the repo and no
bounce CLI, so the gate has to be a test — and it must be written and its
hash recorded **while the code is still unrefactored**, or it proves
nothing.

- [ ] Add `bounce_of_a_full_strip_is_byte_stable` to `audio/offline.rs`'s
      test module, in the style of `pan_automation_survives_a_bounce`. The
      fixture must exercise every path this refactor touches:
      an audio track with a clip, a **bus** track, a **send** from the track
      into the bus, an **insert** on both the track and the bus (use the
      existing `GainHalfEffect` test double via the same route the insert
      tests use), a non-zero `gain_db` and `pan`, and one bypassed insert so
      the declared/applied PDC split is live.
- [ ] Render it through `offline::render` (or `build_graph` + the render
      loop, whichever the neighbouring tests use) and assert a **hash of the
      rendered samples** — SHA-256 over the little-endian `f32` bytes, hex,
      hardcoded in the test.
- [ ] Run it on this branch **before touching any other file** and paste the
      hash into the test. Record the same hash in the PR body.
- [ ] Comment the test with what it is for: a refactor that changes the mix
      by one sample fails here, and a genuine future change to the mix
      updates the hash *deliberately, in the same commit that changes the
      mix*.
- [ ] Gate: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
      green, the new test included.

**Verification:** the hash is in the test, the test passes, and `git diff`
touches only `audio/offline.rs`.

## Task 2 — `MixNode` and its producers

- [ ] New `src-tauri/src/audio/node.rs`, registered in `audio/mod.rs`:

  ```rust
  pub enum MixNodeKind { Source, Bus, Automation }

  pub struct MixNode {
      pub id: TrackId,
      pub kind: MixNodeKind,
      pub gain_db: f64,
      pub pan: f64,
      pub muted: bool,
      pub soloed: bool,
      pub inserts: Vec<InsertSlot>,
      pub sends: Vec<SendSlot>,
      pub output: Option<TrackId>,
  }
  ```

  `Automation` is a kind here only so the producer is **total** over the
  document's tracks: an automation track takes no mixer slot and is
  filtered out downstream exactly as it is today. V2 adds `Player` as a
  fourth producer; it is the reason `id` is the node's identity rather than
  "the track's id".
- [ ] `impl From<&TrackState> for MixNode` and
      `pub fn mix_nodes(tracks: &[TrackState]) -> Vec<MixNode>` (document
      order, one node per track, nothing filtered).
- [ ] Methods replacing the two predicates at the call sites:
      `is_bus()` (`kind == Bus`) and `takes_mixer_slot()`
      (`kind != Automation`), documented as the `MixNode` equivalents of
      `types::is_bus_track` / `types::is_mixer_track`. Leave the
      `TrackState` predicates in place — plenty of non-mixer code still uses
      them.
- [ ] Unit tests: kind maps from the `kind` string (`"audio"`/`"midi"` →
      `Source`, `"bus"` → `Bus`, `"automation"` → `Automation`, and an
      unknown string → `Source`, matching `is_mixer_track`'s
      `!= "automation"` today); `inserts`, `sends`, `output`, gain/pan/mute/
      solo carried verbatim; `mix_nodes` preserves order and length.
- [ ] Nothing else changes yet — no call site is converted in this task.
- [ ] Gate: full Rust suite green.

**Verification:** `cargo test` green; `git diff --stat` shows `node.rs` and
one line of `mod.rs`.

## Task 3 — `compile_inserts` takes `&[MixNode]`

- [ ] Change `audio::insert::compile_inserts`'s first parameter to
      `&[MixNode]`. The body is otherwise untouched: it already reads only
      `t.inserts` and `t.id`.
- [ ] Convert the two call sites — `engine::rebuild`
      (`src-tauri/src/audio/engine.rs`, the Plan G1 Task 7 block) and
      `offline::build_graph` — to build the node list once, **before**
      `compile_inserts`, and hand the *same* slice to both compilers in
      Task 4. Name it `nodes`.
- [ ] Convert `compile_inserts`'s own tests to build `MixNode`s. Assertions
      unchanged.
- [ ] While in `offline::build_graph`: the param fill loop
      (`params.set_gain_linear` / `set_pan` / `set_flag(FLAG_MUTE|FLAG_SOLO)`
      plus the per-send amounts) reads `gain_db`, `pan`, `muted`, `soloed`
      and `sends` — all of them `MixNode` fields, in the same document
      order. Convert that loop to iterate `nodes`, keeping the
      `slots.get(&t.id)` skip and the `is_bus_track` early-`continue`
      (as `node.is_bus()`) exactly where they are. The clip/instrument part
      of that loop keeps reading `store.tracks` — those are timeline
      fields and must NOT move onto `MixNode`.
- [ ] `engine::rebuild`'s param fill is **not** converted: it deliberately
      reads the LIVE document `L` in phase 2, not the assembly image `S`
      (see the `[C1]` comment). Leave it alone and note why in the commit
      message.
- [ ] Gate: full Rust suite green, **including Task 1's hash test**.

**Verification:** the hash test still passes with the recorded hash
unchanged. Paste that line of output into the task report.

## Task 4 — `compile_routing` takes `&[MixNode]`

- [ ] Change `audio::bus::compile_routing`'s first parameter to
      `&[MixNode]`. Inside, `is_bus_track(t)` → `t.is_bus()` and
      `is_mixer_track(t)` → `t.takes_mixer_slot()`; the `tracks.iter().find(
      |t| t.id == node.id)` lookup for send rows now finds a `MixNode`.
      Nothing else in the algorithm moves — the topological sort, the
      `t_in` / `ready_decl` / `ready_appl` bookkeeping and the self-edge and
      cycle filters are all untouched.
- [ ] Update both call sites to pass the `nodes` slice built in Task 3.
- [ ] Convert `bus.rs`'s tests to build `MixNode`s (a small local helper is
      fine). **Assertions unchanged** — the DAG/cycle tests especially.
- [ ] Confirm no `TrackState` remains in `compile_inserts`'s or
      `compile_routing`'s signature or body — not a whole-file grep, since
      `bus::would_cycle` legitimately still takes `&[TrackState]` (it is a
      control-plane guard on document rows, not a graph compiler; see its
      doc comment) and a `grep -n TrackState` over the whole file would
      never return nothing. Read the two compiler functions instead.
- [ ] Gate: full Rust suite green, hash test included, plus
      `timeout 300 npx vitest run` (expected untouched, run anyway — the
      backlog's gate says *every* existing test).

**Verification:** both greps empty, both suites green, hash unchanged.

## Task 5 — the paperwork that lands the cut

- [ ] `docs/backlog/plan-v-players.md`: V1's row goes from
      *unclaimed — start here* to landed with the PR number; V2's row stops
      saying "blocked on V1" and becomes the next unclaimed cut. Update the
      prose under "V1" to past tense and add the recorded hash's test name
      so V2 knows what its own gate is.
- [ ] `docs/LANDED.md`: one entry, house style, pointing at the backlog
      file.
- [ ] `next-prompt.md`: **delete the claim row** (back to `_(none)_`) and
      repoint the "Next up" item 2 at V2.
- [ ] `README.md` + `CONTRIBUTING.md`: dated test counts, **measured** from
      the final run, if and only if they moved.
- [ ] `docs/TRAPS.md`: only if something in this cut actually cost time.
- [ ] Final gate, both suites, foreground, output pasted into the PR.
- [ ] Mark PR #118 ready for review.

**Verification:** `gh pr view 118` shows the gate checklist ticked with real
evidence, and `git log --oneline origin/main..HEAD` reads as one coherent
cut.
