# `Undo to here` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `Undo to here` action that walks the linear undo ancestry
back to a chosen retained revision, guarded so it can never apply against a
document that moved under it.

**Architecture:** The backend owns everything. `ControlPlane::undo_to` takes
one `history_gate` hold, validates the target against the live undo stack and
the caller's observed `(epoch, head_rev)` pair, and then repeats the ordinary
undo step — the same `pop_undo` → `commit_replay` → `push_redo` cycle
`ControlPlane::undo` already runs — until the top of the undo stack is the
target revision. Nothing new touches the document: N ordinary undos, so the
redo chain, the journal, `project://changed` and the epoch guards all behave
exactly as they do for N presses of Ctrl+Z. The frontend only reports the
`(epoch, headRev)` it observed and the row the user picked.

**Tech Stack:** Rust (`src-tauri`, parking_lot, tauri commands), Svelte 5
runes + TypeScript, `cargo test` / `vitest`.

**Spec:** [`docs/PHASE4-PLAN.md`](../../PHASE4-PLAN.md) — "Plan F handoff"
(2026-08-16), carry-forward **(e)**, the paragraph headed **Ordered next
step**. Quoted verbatim:

> **Ordered next step:** add an `Undo to here` action only for revisions on
> the current linear undo ancestry. The backend must own target validation and
> stepping under `history_gate`; the request must carry the observed epoch and
> current revision, abort if either changed, preserve the resulting redo chain,
> and emit the ordinary project-change event. Cover stale-target, eviction,
> epoch-swap and round-trip tests before exposing the button. Do not implement
> arbitrary branch checkout or direct snapshot replacement as part of that
> step: those still require a separate mutation, persistence and undo contract.

## Global Constraints

Copied from [`docs/STANDING-CONSTRAINTS.md`](../../STANDING-CONSTRAINTS.md)
and the spec; every task inherits them.

- **Frozen command/event names stay frozen; new commands are additive.**
  `undo`, `redo`, `history_overview`, `history_version` keep their names,
  payloads and return shapes. The new command is `history_undo_to`. New
  fields on `HistoryOverview` / `HistoryVersion` are additive (serde
  `camelCase`, TypeScript optional-free but new).
- **Thin renderer (ADR 0006):** no authoritative state, business logic or
  time math frontend-side. The panel passes the observed `(epoch, headRev)`
  and the picked `rev` and renders what comes back. Target validation is the
  backend's, and is re-done there even though the button is disabled for
  invalid rows.
- **Lock order is `gesture -> session -> epoch -> history/journal/versions`**
  (`control/history.rs` module doc). `history_gate` is taken by
  `ControlPlane`, outside `HistoryLog`. It is a plain `parking_lot::Mutex`
  and is **not reentrant** — `undo_to` must NOT call `ControlPlane::undo`.
- **`transact` closures must not panic.** Validate before mutating.
- **No new op, no `OP_FORMAT_VERSION` bump.** `undo_to` emits only the
  ordinary undo commits, which are already journaled as `v == 2` batches.
- **Out of scope, per the spec:** arbitrary branch checkout, direct snapshot
  replacement, redo-to-here.
- **Tests run foreground, `timeout`-guarded, single-threaded:**
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1`
  and `timeout 300 npx vitest run`. (Parallel `cargo test` intermittently
  SIGSEGVs on Cardinal CLAP teardown — `docs/TRAPS.md`.)
- **Dated-count convention:** the task that moves test counts updates
  `README.md` + `CONTRIBUTING.md` in the same commit, with the date, from a
  measured run — never a copied number.
- **Theme tokens only** in any `<style>` block (`no-literals.test.ts` fails
  CI on a raw colour).

## Two decisions this plan makes, and why

**1. "Here" means the state AFTER the selected revision — exclusive.**
`undo_to(R)` undoes every undo-stack entry with `rev > R` and leaves `R`
applied. The HISTORY dock's detail pane already shows
`history_version(rev)` = `materialize_version(rev)`, which is the document
**as of** that revision. The action must land on the document the pane is
describing; the inclusive reading would leave the user with a different
document than the numbers they were just looking at. Selecting the head
revision is therefore a legal no-op (`steps: 0`).

**2. The "current revision" guard is the HISTORY HEAD rev, not
`Session::rev`.** `Session::rev` advances on TRANSIENT commits too —
`transport play`, `transport stop`, `transport set loop` and every
mid-gesture fold are `TxMeta::…transient()` and bump it
(`control/mod.rs`). Guarding on it would abort `Undo to here` because the
user pressed play, which is not a change to the undo ancestry at all. The
head rev — `History`'s top undo entry's `rev`, `None` for an empty stack —
is exactly "the linear undo ancestry has not moved", which is what the guard
is for. Record this in `docs/TRAPS.md` (Task 5); it is the kind of thing the
next agent would get wrong.

---

### Task 1: Undo-stack introspection

The stacks are private to `History`. `undo_to` needs to know which revisions
are on the undo path and which one is on top, and `history_overview` needs
the same list to mark rows. Both reads must be one step with respect to a
concurrent epoch boundary, so they go through `HistoryLog` under the same
`epoch -> history` order `pop_undo` uses.

**Files:**
- Modify: `src-tauri/src/control/history.rs` (`impl History` after
  `undo_len`/`redo_len` ~line 308; `impl HistoryLog` after `depths` ~line 877)
- Test: `src-tauri/src/control/history.rs` (the existing `#[cfg(test)] mod
  tests` at the end of the file)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `History::head_rev(&self) -> Option<u64>`
  - `History::undo_revs(&self) -> Vec<u64>` (ascending, oldest first)
  - `HistoryLog::undo_path(&self) -> UndoPath`
  - `pub struct UndoPath { pub epoch: u64, pub revs: Vec<u64> }` with
    `impl UndoPath { pub fn head(&self) -> Option<u64> }` — exported from
    `control::history`.

- [ ] **Step 1: Write the failing tests**

Append to `history.rs`'s test module. Use the module's existing entry
constructor helper if there is one; otherwise build entries with
`HistoryEntry::from_committed(&TxMeta::user("x"), &[], &[], rev)` — read the
neighbouring tests first and match their style.

```rust
    #[test]
    fn undo_revs_are_reported_oldest_first_and_head_is_the_top() {
        let mut h = History::new();
        for rev in [4u64, 5, 6] {
            h.record(HistoryEntry::from_committed(
                &TxMeta::user(format!("edit {rev}")),
                &[],
                &[],
                rev,
            ));
        }
        assert_eq!(h.undo_revs(), vec![4, 5, 6], "ascending, the stack's own order");
        assert_eq!(h.head_rev(), Some(6), "the head is what a plain undo would pop");
    }

    #[test]
    fn an_empty_undo_stack_has_no_head_and_no_revs() {
        let h = History::new();
        assert_eq!(h.head_rev(), None);
        assert!(h.undo_revs().is_empty());
    }

    #[test]
    fn undo_revs_forget_an_entry_that_moved_to_the_redo_stack() {
        let mut h = History::new();
        h.record(HistoryEntry::from_committed(&TxMeta::user("a"), &[], &[], 1));
        h.record(HistoryEntry::from_committed(&TxMeta::user("b"), &[], &[], 2));
        let popped = h.pop_undo().expect("an entry to undo");
        h.push_redo(popped);
        assert_eq!(h.undo_revs(), vec![1], "an undone step is no longer on the undo path");
        assert_eq!(h.head_rev(), Some(1));
    }
```

- [ ] **Step 2: Run them and watch them fail**

```bash
timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib control::history -- --test-threads=1
```

Expected: FAIL — `no method named 'undo_revs' found`, `no method named 'head_rev' found`.

- [ ] **Step 3: Implement the two `History` readers**

In `impl History`, directly after `redo_len`:

```rust
    /// The `rev` of the entry a plain undo would pop — `None` when there is
    /// nothing to undo. This is the HEAD of the linear undo ancestry, and
    /// the value `ControlPlane::undo_to` guards on. NOT `Session::rev`:
    /// that one advances on transient commits (transport play/stop, gesture
    /// folds), which do not touch this stack at all.
    pub fn head_rev(&self) -> Option<u64> {
        self.undo.back().map(|e| e.rev)
    }

    /// Every revision currently on the undo path, oldest first — the stack's
    /// own ascending order (see [`Self::record`]'s insertion rule). An entry
    /// that has been undone is on the redo stack and is deliberately absent:
    /// it is no longer part of the ancestry the document sits on.
    pub fn undo_revs(&self) -> Vec<u64> {
        self.undo.iter().map(|e| e.rev).collect()
    }
```

- [ ] **Step 4: Implement `UndoPath` and the `HistoryLog` reader**

Add the type next to `History` in the same file:

```rust
/// The linear undo ancestry as one atomic observation: the epoch it was read
/// under, and the revisions on it (oldest first). Read as ONE step so a
/// caller cannot pair revisions from one document with the epoch of another
/// — the same reason [`HistoryLog::pop_undo`] returns its epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoPath {
    pub epoch: u64,
    pub revs: Vec<u64>,
}

impl UndoPath {
    /// The revision a plain undo would consume next.
    pub fn head(&self) -> Option<u64> {
        self.revs.last().copied()
    }
}
```

And in `impl HistoryLog`, after `depths`:

```rust
    /// The undo ancestry and the epoch it belongs to, read under the module's
    /// `epoch -> history` order so the pair is consistent with respect to a
    /// concurrent [`Self::epoch_boundary`] — an interleaved boundary either
    /// clears the stack before this read (empty path, new epoch) or lands
    /// after it (the caller's later guard sees the epoch move).
    pub fn undo_path(&self) -> UndoPath {
        let epoch = self.epoch.lock();
        let revs = self.history.lock().undo_revs();
        UndoPath { epoch: *epoch, revs }
    }
```

- [ ] **Step 5: Run the tests**

```bash
timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib control::history -- --test-threads=1
```

Expected: PASS, all three new tests plus the existing `control::history` tests.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/control/history.rs
git commit -m "feat(history): report the undo ancestry (head rev + undo revs)"
```

---

### Task 2: `ControlPlane::undo_to` — validation and stepping

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (`ControlPlane::undo` ~line 3613 —
  split out the ungated step; add `undo_to` after `redo` ~line 3676)
- Test: `src-tauri/tests/journal_and_history.rs` (append an
  `// undo to here` section after the undo/redo section)

**Interfaces:**
- Consumes: `HistoryLog::undo_path() -> UndoPath` and `UndoPath::head()`
  (Task 1); the existing `HistoryLog::pop_undo/push_redo/push_undo_unchanged`
  and `ControlPlane::commit_replay`.
- Produces:
  - `pub struct UndoToOutcome { pub steps: usize, pub label: Option<String> }`
    in `control::mod`
  - `ControlPlane::undo_to(&self, target_rev: u64, expected_epoch: u64, expected_head_rev: Option<u64>) -> Result<UndoToOutcome, String>`
  - `ControlPlane::undo_step(&self) -> Result<Option<String>, String>` —
    the gate-free body `undo` and `undo_to` share.

- [ ] **Step 1: Write the failing tests**

Append to `src-tauri/tests/journal_and_history.rs`. `fixture()`, `add_track`,
`set_gain`, `gain_of`, `tmp_parent` and `journal_lines`/`batches` already
exist in that file — reuse them, do not redefine.

```rust
// ---------------------------------------------------------------------------
// undo to here (Plan F carry-forward (e), ordered next step)
// ---------------------------------------------------------------------------

/// Four edits, then a walk back to the second one: the document is the one
/// that revision describes, every step in between is on the redo stack in
/// the right order, and redoing twice puts the document back.
#[test]
fn undo_to_walks_back_to_a_revision_and_leaves_a_redo_chain() {
    let f = fixture();
    let parent = tmp_parent("undoto-roundtrip");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");

    // Distinct labels so the 350 ms same-key merge cannot fold them.
    for (label, gain) in [("g1", -3.0), ("g2", -6.0), ("g3", -9.0)] {
        f.cp.commit(TxMeta::user(label), |tx| tx.apply(set_gain(&t, gain))).unwrap();
    }
    let path = f.log.undo_path();
    assert_eq!(path.revs.len(), 4, "add track + three gain edits");
    let target = path.revs[1]; // the state after g1

    let out = f.cp.undo_to(target, path.epoch, path.head()).expect("undo to here");
    assert_eq!(out.steps, 2, "g3 and g2 are undone; g1 stays applied");
    assert_eq!(gain_of(&f.cp, &t), -3.0, "the document is the one revision g1 describes");
    assert_eq!(f.log.depths(), (2, 2), "two steps left to undo, two to redo");
    assert_eq!(f.log.undo_path().head(), Some(target), "the head is now the target");

    // The redo chain is ordered, not merely present.
    assert_eq!(f.cp.redo().unwrap().as_deref(), Some("g2"));
    assert_eq!(f.cp.redo().unwrap().as_deref(), Some("g3"));
    assert_eq!(gain_of(&f.cp, &t), -9.0, "redo restores the walked-back edits");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// Selecting the head is legal and does nothing — the document is already
/// the one that revision describes.
#[test]
fn undo_to_the_head_revision_is_a_no_op() {
    let f = fixture();
    let parent = tmp_parent("undoto-head");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();

    let path = f.log.undo_path();
    let out = f.cp.undo_to(path.head().unwrap(), path.epoch, path.head()).unwrap();
    assert_eq!(out.steps, 0);
    assert_eq!(out.label, None);
    assert_eq!(gain_of(&f.cp, &t), -6.0);
    assert_eq!(f.log.depths(), (2, 0), "nothing moved between the stacks");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// STALE TARGET: the ancestry moved between the read and the request (a new
/// edit landed). The request carries the head it saw, so it aborts whole —
/// no partial walk, no consumed step.
#[test]
fn undo_to_aborts_when_the_undo_head_moved_under_it() {
    let f = fixture();
    let parent = tmp_parent("undoto-stale");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("g1"), |tx| tx.apply(set_gain(&t, -3.0))).unwrap();
    let path = f.log.undo_path();
    let target = path.revs[1];

    // Something else commits before the request arrives.
    f.cp.commit(TxMeta::user("g2"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();

    let err = f.cp.undo_to(target, path.epoch, path.head()).unwrap_err();
    assert!(err.contains("changed"), "the message must say the history moved: {err}");
    assert_eq!(gain_of(&f.cp, &t), -6.0, "nothing was applied");
    assert_eq!(f.log.depths(), (3, 0), "no step was consumed");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// EVICTION / off-path target: a revision that is not on the undo stack is
/// refused. An already-undone step is the reachable case (it sits on the
/// redo stack); a bottom-evicted one behaves identically because both are
/// simply absent from `undo_revs`.
#[test]
fn undo_to_refuses_a_revision_that_is_not_on_the_undo_path() {
    let f = fixture();
    let parent = tmp_parent("undoto-offpath");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("g1"), |tx| tx.apply(set_gain(&t, -3.0))).unwrap();
    f.cp.commit(TxMeta::user("g2"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    let gone = f.log.undo_path().head().unwrap(); // g2's rev

    f.cp.undo().unwrap(); // g2 moves to the redo stack
    let path = f.log.undo_path();
    assert!(!path.revs.contains(&gone));

    let err = f.cp.undo_to(gone, path.epoch, path.head()).unwrap_err();
    assert!(err.contains("not on the undo path"), "unexpected message: {err}");
    assert_eq!(gain_of(&f.cp, &t), -3.0, "nothing was applied");
    assert_eq!(f.log.depths(), (2, 1), "the stacks are untouched");

    // A revision that never existed is refused the same way.
    let err = f.cp.undo_to(9_999, path.epoch, path.head()).unwrap_err();
    assert!(err.contains("not on the undo path"), "unexpected message: {err}");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// EPOCH SWAP: the document was replaced between the read and the request.
/// The epoch guard refuses before any op is applied — undoing across a
/// document swap is corruption, not undo (`History::clear`'s doc).
#[test]
fn undo_to_aborts_when_the_document_was_swapped() {
    let f = fixture();
    let parent = tmp_parent("undoto-epoch");
    f.cp.create_project(parent.to_str().unwrap(), "A").unwrap();
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("g1"), |tx| tx.apply(set_gain(&t, -3.0))).unwrap();
    let path = f.log.undo_path();
    let target = path.revs[1];

    // A second project = a document swap = an epoch boundary.
    f.cp.create_project(parent.to_str().unwrap(), "B").unwrap();
    assert_eq!(f.log.depths(), (0, 0), "the swap cleared history");

    let err = f.cp.undo_to(target, path.epoch, path.head()).unwrap_err();
    assert!(err.contains("epoch"), "the message must name the swap: {err}");
    assert_eq!(f.log.depths(), (0, 0), "nothing was pushed onto the new document's stacks");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// Every step is an ORDINARY undo commit: journaled, `HistoryMode::Replay`,
/// no new history entry. Two steps = two journal batches, not one.
#[test]
fn undo_to_journals_one_batch_per_step_and_creates_no_history_entry() {
    let f = fixture();
    let parent = tmp_parent("undoto-journal");
    let project = f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let dir = std::path::PathBuf::from(project.dir.clone().expect("project dir"));
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("g1"), |tx| tx.apply(set_gain(&t, -3.0))).unwrap();
    f.cp.commit(TxMeta::user("g2"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    let before = batches(&journal_lines(&dir)).len();

    let path = f.log.undo_path();
    let target = path.revs[0];
    let out = f.cp.undo_to(target, path.epoch, path.head()).unwrap();
    assert_eq!(out.steps, 2);
    assert_eq!(out.label.as_deref(), Some("g1"), "the label of the LAST step undone");

    let lines = journal_lines(&dir);
    let after = batches(&lines).len();
    assert_eq!(after - before, 2, "one journal batch per undo step");
    assert_eq!(f.log.depths(), (1, 2), "two entries migrated, none created");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}
```

Note on `project.dir` in the last test: check how the existing
`undo_and_redo_commits_are_journaled_but_create_no_new_history_entry` test
(same file, ~line 237) obtains the project directory and copy that exact
expression — it already solved this.

- [ ] **Step 2: Run them and watch them fail**

```bash
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml --test journal_and_history -- --test-threads=1
```

Expected: FAIL — `no method named 'undo_to' found for struct 'ControlPlane'`.

- [ ] **Step 3: Split the gate-free step out of `undo`**

In `control/mod.rs`, `ControlPlane::undo` currently closes an open gesture,
takes `history_gate`, and does one pop/commit/push cycle. Keep its doc
comment where it is and reduce the body to the gate plus a call:

```rust
    pub fn undo(&self) -> Result<Option<String>, String> {
        if let Some(g) = self.gesture.end(None) {
            self.close_gesture(g);
        }
        let _gate = self.history_gate.lock();
        self.undo_step()
    }

    /// One undo step with the gate ALREADY HELD — the shared body of
    /// [`Self::undo`] and [`Self::undo_to`]. `history_gate` is a plain
    /// `parking_lot::Mutex` and is not reentrant, so a caller that holds it
    /// must never route back through [`Self::undo`].
    ///
    /// Everything the single-step contract promises still holds here: the
    /// entry's inverses go through the normal commit path (journaled, own
    /// rebuild, `project://changed`), `HistoryMode::Replay` suppresses a new
    /// history entry, the ORIGINAL entry migrates onto the redo stack, and a
    /// failed commit puts it back untouched.
    fn undo_step(&self) -> Result<Option<String>, String> {
        let Some((entry, popped_epoch)) = self.committer.log().pop_undo() else { return Ok(None) };
        let meta = op::TxMeta {
            actor: op::Actor::User,
            run: entry.run.clone(),
            label: format!("undo: {}", entry.label),
            transient: false,
        };
        let ops = entry.inverses.clone();
        match self.commit_replay(meta, ops, popped_epoch) {
            Ok(()) => {
                let label = entry.label.clone();
                self.committer.log().push_redo(entry, popped_epoch);
                Ok(Some(label))
            }
            Err(e) => {
                self.committer.log().push_undo_unchanged(entry, popped_epoch);
                Err(e)
            }
        }
    }
```

- [ ] **Step 4: Add `UndoToOutcome` and `undo_to`**

Put the struct next to `HistoryStep` (~line 5143) so the IPC types stay
together, and the method directly after `redo`:

```rust
/// What a completed [`ControlPlane::undo_to`] walk did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoToOutcome {
    /// How many undo steps were applied. 0 is a legal, successful answer:
    /// the target was already the head.
    pub steps: usize,
    /// The label of the LAST step undone — what a toast shows ("back to
    /// <label>" is wrong; this is the step the walk ended on). `None` when
    /// `steps == 0`.
    pub label: Option<String>,
}
```

```rust
    /// Walk the linear undo ancestry back to `target_rev` — the Plan F
    /// carry-forward (e) "ordered next step".
    ///
    /// SEMANTICS: EXCLUSIVE. Every undo entry ABOVE `target_rev` is undone;
    /// `target_rev` itself stays applied, so the document ends as the one
    /// `materialize_version(target_rev)` describes — the document the
    /// HISTORY dock's detail pane is showing when the user picks that row.
    /// Picking the head is therefore a successful no-op.
    ///
    /// THE GUARDS, and what each is for:
    /// * `expected_epoch` — the document must still be the one the caller
    ///   read. Undoing across a document swap is corruption, not undo
    ///   (`History::clear`'s doc). Checked here AND again per step inside
    ///   `commit_replay`, which is what makes "nothing applied" true rather
    ///   than merely "nothing recorded".
    /// * `expected_head_rev` — the undo ancestry must not have moved. NOT
    ///   `Session::rev`: transient commits (transport play/stop, gesture
    ///   folds) bump that without touching this stack, so guarding on it
    ///   would abort because the user pressed play.
    /// * `target_rev` must be ON the current undo path. An already-undone
    ///   step (now on the redo stack) and a bottom-evicted one
    ///   (`UNDO_STACK_LIMIT`) are both simply absent, and both are refused
    ///   with the same message: the request describes an ancestry this
    ///   document does not have.
    ///
    /// ONE GATE FOR THE WHOLE WALK: `history_gate` is taken once and held
    /// across every step, so a concurrent `undo`/`redo` cannot interleave
    /// into the middle of a walk. The gate is not reentrant, hence
    /// [`Self::undo_step`] rather than [`Self::undo`].
    ///
    /// M-4, an open gesture is closed FIRST, exactly as [`Self::undo`] does
    /// — the drag becomes the finished step it already looks like. That
    /// close records a fresh entry, which MOVES the head, so a walk
    /// requested mid-drag then fails its own head guard and the user
    /// retries against the ancestry they can now see. Aborting is the right
    /// answer: the row they clicked was chosen before the drag existed.
    ///
    /// PARTIAL WALKS ARE REPORTED, NOT HIDDEN. Each step is a real
    /// committed transaction; if step `k` fails (an epoch swap landing
    /// mid-walk is the reachable case), the `k-1` steps before it stay
    /// applied and the error says so. The caller re-reads the overview.
    pub fn undo_to(
        &self,
        target_rev: u64,
        expected_epoch: u64,
        expected_head_rev: Option<u64>,
    ) -> Result<UndoToOutcome, String> {
        if let Some(g) = self.gesture.end(None) {
            self.close_gesture(g);
        }
        let _gate = self.history_gate.lock();

        let path = self.committer.log().undo_path();
        if path.epoch != expected_epoch {
            return Err(format!(
                "the project was replaced under this request (epoch {expected_epoch} -> {}) \
                 — nothing applied",
                path.epoch
            ));
        }
        if path.head() != expected_head_rev {
            return Err(format!(
                "the edit history changed under this request (head {:?} -> {:?}) \
                 — nothing applied",
                expected_head_rev,
                path.head()
            ));
        }
        let Some(at) = path.revs.iter().position(|r| *r == target_rev) else {
            return Err(format!(
                "revision {target_rev} is not on the undo path — it was undone already, \
                 or dropped from the {} most recent steps history keeps",
                history::UNDO_STACK_LIMIT
            ));
        };
        let steps = path.revs.len() - 1 - at;

        let mut label = None;
        for done in 0..steps {
            match self.undo_step() {
                Ok(Some(l)) => label = Some(l),
                Ok(None) => {
                    return Err(format!(
                        "the undo history ran out after {done} of {steps} steps — \
                         stopped at the earliest step it still keeps"
                    ))
                }
                Err(e) => {
                    return Err(format!(
                        "undo to revision {target_rev} stopped after {done} of {steps} \
                         steps: {e}"
                    ))
                }
            }
        }
        Ok(UndoToOutcome { steps, label })
    }
```

- [ ] **Step 5: Run the tests**

```bash
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml --test journal_and_history -- --test-threads=1
```

Expected: PASS — the six new tests and every pre-existing test in the file
(the `undo`/`redo` split must not change their behaviour).

- [ ] **Step 6: Run the whole backend suite**

```bash
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1
```

Expected: PASS. Record the total count — Task 5 writes it into README and
CONTRIBUTING.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/control/mod.rs src-tauri/tests/journal_and_history.rs
git commit -m "feat(history): guarded undo_to walks the linear undo ancestry"
```

---

### Task 3: The IPC surface

The panel needs three things it cannot compute: which rows are on the undo
path, the epoch, and the head rev. All three are additive fields on the
existing read-only overview; the walk itself is one new command.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (`HistoryVersion` ~line 5154,
  `HistoryOverview` ~line 5163, `history_overview` ~line 5185, new command
  after `redo` ~line 5270)
- Modify: `src-tauri/src/lib.rs:251` (the `invoke_handler` list)
- Modify: `src/lib/types/ipc.ts:1434-1450`
- Modify: `src/lib/tauri.ts` (the `BackendApi` interface ~line 291 and the
  Tauri implementation ~line 797)
- Test: `src-tauri/src/control/mod.rs`'s own `#[cfg(test)]` module is
  tauri-free and cannot build a `State`; the command is a thin delegate and
  is covered by Task 2's `ControlPlane` tests plus Task 4's DOM tests. Do not
  invent a test harness for the wrapper.

**Interfaces:**
- Consumes: `ControlPlane::undo_to`, `UndoToOutcome` (Task 2);
  `HistoryLog::undo_path`, `UndoPath` (Task 1).
- Produces:
  - Rust: `ControlPlane::undo_path(&self) -> history::UndoPath`;
    `#[tauri::command] pub async fn history_undo_to(target_rev: u64, expected_epoch: u64, expected_head_rev: Option<u64>, control: State<'_, Arc<ControlPlane>>) -> Result<UndoToOutcome, String>`
  - Wire (camelCase): `history_undo_to { targetRev, expectedEpoch, expectedHeadRev }`
  - TS: `HistoryVersion.onUndoPath: boolean`, `HistoryOverview.epoch: number`,
    `HistoryOverview.headRev: number | null`, `UndoToOutcome { steps: number; label: string | null }`,
    `backend.historyUndoTo?(targetRev, expectedEpoch, expectedHeadRev): Promise<UndoToOutcome>`

- [ ] **Step 1: Extend the Rust response structs**

`HistoryVersion` gains one field, `HistoryOverview` gains two:

```rust
pub struct HistoryVersion {
    pub rev: u64,
    pub materialized: bool,
    pub charged_bytes: usize,
    pub label: String,
    pub actor: String,
    /// True when this revision is on the CURRENT linear undo ancestry, i.e.
    /// when `history_undo_to` will accept it. Retained revisions that are
    /// not: the undo commits themselves, anything already undone (now on the
    /// redo stack), and anything dropped by `UNDO_STACK_LIMIT` while the
    /// version graph still keeps it.
    pub on_undo_path: bool,
}

pub struct HistoryOverview {
    pub undo_depth: usize,
    pub redo_depth: usize,
    pub retained_bytes: usize,
    pub materialized: usize,
    pub replay_only: usize,
    pub versions: Vec<HistoryVersion>,
    /// The document epoch these versions belong to. Handed back verbatim by
    /// `history_undo_to` so the backend can refuse a request that was
    /// composed against a document that has since been replaced.
    pub epoch: u64,
    /// The revision a plain undo would consume next — the head of the undo
    /// ancestry, `None` when there is nothing to undo. The second half of
    /// the guard pair. NOT the live `Session::rev`: transient commits
    /// (transport play/stop, gesture folds) move that without touching the
    /// undo stack.
    pub head_rev: Option<u64>,
}
```

- [ ] **Step 2: Fill them in `history_overview`**

```rust
#[tauri::command]
pub fn history_overview(control: State<'_, Arc<ControlPlane>>) -> HistoryOverview {
    let (stats, versions) = control.version_overview();
    let (undo_depth, redo_depth) = control.history_depths();
    let path = control.undo_path();
    let on_path: std::collections::HashSet<u64> = path.revs.iter().copied().collect();
    HistoryOverview {
        undo_depth,
        redo_depth,
        retained_bytes: stats.retained_bytes,
        materialized: stats.materialized,
        replay_only: stats.replay_only,
        versions: versions
            .into_iter()
            .map(|v| HistoryVersion {
                on_undo_path: on_path.contains(&v.rev),
                rev: v.rev,
                materialized: v.materialized,
                charged_bytes: v.charged_bytes,
                label: v.label,
                actor: v.actor,
            })
            .collect(),
        epoch: path.epoch,
        head_rev: path.head(),
    }
}
```

`control.undo_path()` is a one-line delegate next to `version_overview`
(~line 3745):

```rust
    /// The linear undo ancestry, for the browsing surface's `Undo to here`
    /// affordance and the guard pair it must hand back.
    pub fn undo_path(&self) -> history::UndoPath {
        self.committer.log().undo_path()
    }
```

- [ ] **Step 3: Add the command**

Directly after the `redo` command:

```rust
/// Walk the undo ancestry back to `target_rev` (Plan F carry-forward (e)) —
/// thin delegate over [`ControlPlane::undo_to`]. Additive command; `undo`
/// and `redo` are untouched.
///
/// `expected_epoch` / `expected_head_rev` are the values the caller read
/// from [`history_overview`]. The backend refuses the walk if either has
/// moved — the renderer never decides that, it only reports what it saw.
///
/// Async for [`undo`]'s reason (I-6), and more so: this is N undos, any one
/// of which can re-instantiate a plugin.
#[tauri::command]
pub async fn history_undo_to(
    target_rev: u64,
    expected_epoch: u64,
    expected_head_rev: Option<u64>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<UndoToOutcome, String> {
    let cp = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cp.undo_to(target_rev, expected_epoch, expected_head_rev))
        .await
        .map_err(|e| e.to_string())?
}
```

Register it in `src-tauri/src/lib.rs`, in the undo/redo block:

```rust
            control::history_overview,
            control::history_version,
            control::history_undo_to,
```

- [ ] **Step 4: Build the backend**

```bash
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1
```

Expected: PASS (a compile error here means the struct literals missed a
field — every `HistoryVersion`/`HistoryOverview` construction must be
updated).

- [ ] **Step 5: Mirror the types in TypeScript**

`src/lib/types/ipc.ts`:

```ts
export interface HistoryVersion {
  rev: number;
  materialized: boolean;
  chargedBytes: number;
  label: string;
  actor: string;
  /** True when `historyUndoTo` will accept this revision. */
  onUndoPath: boolean;
}

export interface HistoryOverview {
  undoDepth: number;
  redoDepth: number;
  retainedBytes: number;
  materialized: number;
  replayOnly: number;
  /** Newest first. */
  versions: HistoryVersion[];
  /** Handed back verbatim by `historyUndoTo` — the document guard. */
  epoch: number;
  /** The revision a plain undo would consume next — the history guard. */
  headRev: number | null;
}

export interface UndoToOutcome {
  /** How many steps were undone. 0 means the target was already the head. */
  steps: number;
  /** The label of the last step undone; null when `steps` is 0. */
  label: string | null;
}
```

`src/lib/tauri.ts` — the optional-method pattern, so the demo backend (no op
log) stays out of it. In the `BackendApi` interface next to
`historyVersion?`:

```ts
  historyUndoTo?(targetRev: number, expectedEpoch: number, expectedHeadRev: number | null): Promise<UndoToOutcome>;
```

and in the Tauri implementation next to `historyVersion`:

```ts
  historyUndoTo(targetRev: number, expectedEpoch: number, expectedHeadRev: number | null) {
    return invoke<UndoToOutcome>("history_undo_to", { targetRev, expectedEpoch, expectedHeadRev });
  }
```

Add `UndoToOutcome` to the `import type { … } from "./types/ipc"` list at the
top of `tauri.ts` (find the line that already imports `HistoryOverview`).

- [ ] **Step 6: Typecheck and run the frontend suite**

```bash
timeout 300 npx svelte-check --threshold error 2>&1 | tail -5
timeout 300 npx vitest run
```

Expected: no new type errors; existing tests pass. Any test fixture building
a `HistoryOverview` literal needs the two new fields — fix those fixtures
here, not later.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/control/mod.rs src-tauri/src/lib.rs src/lib/types/ipc.ts src/lib/tauri.ts
git commit -m "feat(history): history_undo_to command and the guard pair on the overview"
```

---

### Task 4: The HISTORY dock action

**Files:**
- Modify: `src/lib/state/projectops.svelte.ts` (next to `undo`/`redo`, ~line 223)
- Modify: `src/lib/state/history.svelte.ts` (the `HistoryBrowser` class)
- Modify: `src/lib/components/history/HistoryPanel.svelte`
- Test: `src/lib/components/history/history-panel.dom.test.ts`

**Interfaces:**
- Consumes: `backend.historyUndoTo`, `HistoryOverview.epoch`,
  `HistoryOverview.headRev`, `HistoryVersion.onUndoPath` (Task 3).
- Produces:
  - `projectops.undoTo(targetRev: number, expectedEpoch: number, expectedHeadRev: number | null): Promise<boolean>` — resolves `true` when the walk landed
  - `historyBrowser.canUndoToSelected: boolean` (a `$derived` getter)
  - `historyBrowser.undoToSelected(): Promise<void>`

- [ ] **Step 1: Write the failing DOM tests**

The existing file mocks the backend and `projectops`. Both mocks need the new
members — extend them in place:

```ts
const historyUndoTo = vi.fn();
const undoTo = vi.fn(() => Promise.resolve(true));

vi.mock("../../tauri", () => ({
  backend: { mode: "tauri", historyOverview, historyVersion, historyUndoTo, on },
}));
vi.mock("../../state/projectops.svelte", () => ({
  projectops: { undo, redo, undoTo },
}));
```

and the `overview()` helper gains the three new fields (r9 is the head, r8 is
on the path, r7 is an undo commit the walk must refuse):

```ts
function overview(undoDepth = 2, redoDepth = 1) {
  return {
    undoDepth,
    redoDepth,
    retainedBytes: 384,
    materialized: 1,
    replayOnly: 1,
    epoch: 4,
    headRev: 9,
    versions: [
      { rev: 9, materialized: false, chargedBytes: 128, label: "set gain", actor: "You", onUndoPath: true },
      { rev: 8, materialized: true, chargedBytes: 256, label: "move clip", actor: "You", onUndoPath: true },
      { rev: 7, materialized: false, chargedBytes: 64, label: "undo: trim clip", actor: "You", onUndoPath: false },
    ],
  };
}
```

Add a `detail(rev: number)` helper next to it so the new cases can stub
`historyVersion` per row:

```ts
const detail = (rev: number) => ({
  rev, projectName: "Song", trackCount: 3, audioClipCount: 1, midiClipCount: 2, automationLaneCount: 1,
});
```

Then the four cases:

```ts
  it("enables Undo to here for a selected revision on the undo path", async () => {
    historyOverview.mockResolvedValue(overview());
    historyVersion.mockImplementation((rev: number) => Promise.resolve(detail(rev)));

    render(HistoryPanel);
    await fireEvent.click(await screen.findByRole("option", { name: /move clip r8/i }));

    const button = await screen.findByRole("button", { name: /undo to here/i });
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
  });

  it("disables Undo to here for a revision that is not on the undo path", async () => {
    historyOverview.mockResolvedValue(overview());
    historyVersion.mockImplementation((rev: number) => Promise.resolve(detail(rev)));

    render(HistoryPanel);
    await fireEvent.click(await screen.findByRole("option", { name: /undo: trim clip r7/i }));

    const button = await screen.findByRole("button", { name: /undo to here/i });
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(true));
  });

  it("disables Undo to here for the head revision — there is nothing to walk back", async () => {
    historyOverview.mockResolvedValue(overview());
    historyVersion.mockImplementation((rev: number) => Promise.resolve(detail(rev)));

    render(HistoryPanel);
    // r9 is selected by default (newest first) and is the head.
    const button = await screen.findByRole("button", { name: /undo to here/i });
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(true));
  });

  it("hands the backend the epoch and head rev it rendered, then refreshes", async () => {
    historyOverview.mockResolvedValue(overview());
    historyVersion.mockImplementation((rev: number) => Promise.resolve(detail(rev)));

    render(HistoryPanel);
    await fireEvent.click(await screen.findByRole("option", { name: /move clip r8/i }));
    const button = await screen.findByRole("button", { name: /undo to here/i });
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
    await fireEvent.click(button);

    await waitFor(() => expect(undoTo).toHaveBeenCalledWith(8, 4, 9));
    await waitFor(() => expect(historyOverview).toHaveBeenCalledTimes(2));
  });

  it("shows the reason a walk was refused", async () => {
    historyOverview.mockResolvedValue(overview());
    historyVersion.mockImplementation((rev: number) => Promise.resolve(detail(rev)));
    undoTo.mockRejectedValueOnce(new Error("the edit history changed under this request"));

    render(HistoryPanel);
    await fireEvent.click(await screen.findByRole("option", { name: /move clip r8/i }));
    const button = await screen.findByRole("button", { name: /undo to here/i });
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
    await fireEvent.click(button);

    expect(await screen.findByText(/the edit history changed under this request/i)).toBeTruthy();
  });
```

The three pre-existing cases in the file keep passing unchanged — the new
`overview()` fields are additive and the extra r7 row does not match any of
their queries. If the first case's `findByText("3")` becomes ambiguous
because a second detail pane renders, scope it with `within(...)` rather than
weakening the assertion.

- [ ] **Step 2: Run them and watch them fail**

```bash
timeout 300 npx vitest run src/lib/components/history/history-panel.dom.test.ts
```

Expected: FAIL — no such button in the DOM.

- [ ] **Step 3: Add the store call**

In `projectops.svelte.ts`, beside `undo`/`redo` (reuse `repull` and `toasts`,
and keep the optional-method guard):

```ts
  /**
   * Walk the undo ancestry back to `targetRev` (Plan F carry-forward (e)).
   * Thin: the backend validates the target and owns the stepping — this
   * passes the guard pair the caller observed and reports the outcome.
   */
  async undoTo(targetRev: number, expectedEpoch: number, expectedHeadRev: number | null) {
    const call = backend.historyUndoTo;
    if (!call) return false; // demo backend: no op log to walk
    try {
      const out = await call.call(backend, targetRev, expectedEpoch, expectedHeadRev);
      if (out.steps === 0) return true; // already there — nothing to announce
      await this.repull();
      toasts.info("UNDO", `${out.steps} steps back to ${out.label ?? `r${targetRev}`}`);
      return true;
    } catch (err) {
      toasts.error("UNDO TO HERE FAILED", String(err));
      throw err;
    }
  }
```

- [ ] **Step 4: Add the browser-state glue**

In `history.svelte.ts`, inside `HistoryBrowser`:

```ts
  /** The selected row, or null. */
  get selected() {
    return this.overview?.versions.find((v) => v.rev === this.selectedRev) ?? null;
  }

  /**
   * `Undo to here` is offered for a selected revision that is on the undo
   * ancestry and is not already the head (walking to the head is a no-op).
   */
  get canUndoToSelected() {
    const s = this.selected;
    return Boolean(s?.onUndoPath && this.overview && s.rev !== this.overview.headRev);
  }

  async undoToSelected() {
    const o = this.overview;
    const rev = this.selectedRev;
    if (!o || rev == null || !this.canUndoToSelected) return;
    this.error = null;
    let failure: string | null = null;
    try {
      await projectops.undoTo(rev, o.epoch, o.headRev);
    } catch (err) {
      failure = String(err);
    }
    // ORDER MATTERS: `loadOnce` clears `error` on its way in, so a refused
    // walk's reason is assigned AFTER the refresh, not in a `finally` before
    // it — otherwise the panel refreshes the message away in the same tick.
    await this.refresh();
    if (failure) this.error = failure;
  }
```

Import `projectops` at the top of the file
(`import { projectops } from "./projectops.svelte";`).

- [ ] **Step 5: Add the button**

In `HistoryPanel.svelte`, inside the `{#if historyBrowser.detail}` detail
section, under the `<dl>`:

```svelte
          <button
            class="undoto"
            disabled={!historyBrowser.canUndoToSelected || historyBrowser.loading}
            title={historyBrowser.canUndoToSelected
              ? "Undo every step after this revision"
              : "Only a revision still on the undo path can be walked back to"}
            onclick={() => void historyBrowser.undoToSelected()}
          >
            UNDO TO HERE
          </button>
```

and a style rule that uses tokens only (the file's `button` rule already
carries border/background/colour; add nothing but geometry):

```css
  .undoto { margin-top: 10px; width: 100%; height: 30px; border-radius: 4px; font-size: 9px; letter-spacing: .08em; }
```

Update the panel's intro sentence so the affordance is discoverable:

```svelte
  <p class="intro">Each row is an edit kept by the project. Select one to inspect it, then use Undo to here to walk the project back to it.</p>
```

- [ ] **Step 6: Run the tests**

```bash
timeout 300 npx vitest run
```

Expected: PASS, including `src/lib/theme/no-literals.test.ts`.

- [ ] **Step 7: Commit**

```bash
git add src/lib/state/projectops.svelte.ts src/lib/state/history.svelte.ts src/lib/components/history/
git commit -m "feat(history): Undo to here in the HISTORY dock"
```

---

### Task 5: Close-out — docs, counts, claim release

**Files:**
- Modify: `docs/PHASE4-PLAN.md` (Plan F handoff, carry-forward (e))
- Modify: `docs/backlog/history-and-automation.md` (the Plan F bullet)
- Modify: `docs/LANDED.md` (a row in "Everything else")
- Modify: `docs/TRAPS.md` (the `Session::rev` guard trap)
- Modify: `README.md` + `CONTRIBUTING.md` (dated test counts)
- Modify: `next-prompt.md` (delete the claim row)

- [ ] **Step 1: Measure the counts**

```bash
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1 2>&1 | tail -30
timeout 300 npx vitest run 2>&1 | tail -10
```

Write down the backend (lib + integration) and frontend totals.

- [ ] **Step 2: Update the counts in README.md and CONTRIBUTING.md**

Find the existing dated count lines and replace the numbers and the date
(2026-08-23) with the measured ones. Do not copy a number from this plan.

- [ ] **Step 3: Record the trap**

`docs/TRAPS.md` has `## Frontend`, `## Tests` and `## Runtime noise that is
not your bug`. This one is neither frontend nor a test artifact — add a
`## Backend` section after `## Frontend` and put it there:

```markdown
- **`Session::rev` is not "the current revision" for a history guard.**
  Transient commits — `transport play`, `transport stop`, `transport set
  loop`, every mid-gesture fold — go through `transact` and bump `rev`
  without reaching the undo stack. A guard built on it aborts because the
  user pressed play. The undo ancestry's own head (`HistoryLog::undo_path()`
  → `UndoPath::head()`) is the value that means "the history has not moved";
  it is what `history_undo_to` guards on.
```

- [ ] **Step 4: Close the carry-forward**

In `docs/PHASE4-PLAN.md`, carry-forward (e): keep the paragraph, and mark the
ordered next step done, in the file's own style (per ADR 0007, corrections
are marked, not rewritten):

```markdown
  **Ordered next step — DONE (2026-08-23, PR #107).** `history_undo_to`
  walks the linear undo ancestry: validation and stepping in
  `ControlPlane::undo_to` under one `history_gate` hold, guarded on the
  observed `(epoch, head rev)` pair, N ordinary undo commits so the redo
  chain, the journal and `project://changed` behave exactly as N presses of
  Ctrl+Z. Semantics are EXCLUSIVE (the target revision stays applied — the
  document the detail pane shows). Arbitrary branch checkout and direct
  snapshot replacement remain out of scope and still need their own
  mutation, persistence and undo contract.
```

Update the `docs/backlog/history-and-automation.md` bullet the same way: the
ordered follow-up is no longer open.

- [ ] **Step 5: Add the LANDED row**

In `docs/LANDED.md`'s "Everything else" table, newest first:

```markdown
| **`Undo to here`** — guarded linear walk back to a retained revision: `ControlPlane::undo_to` validates the target against the live undo path and the caller's observed `(epoch, head rev)`, then repeats the ordinary undo step under one `history_gate` hold; additive `history_undo_to` command, `onUndoPath`/`epoch`/`headRev` on the overview, and the action in the HISTORY dock | PR #107. Contract: `PHASE4-PLAN.md` "Plan F handoff" carry-forward (e). Plan: [`2026-08-23-undo-to-here.md`](superpowers/plans/2026-08-23-undo-to-here.md) |
```

- [ ] **Step 6: Release the claim**

Delete the `feat/undo-to-here` row from `next-prompt.md`'s *Active claims*
table, restoring the `| _(none)_ | | | |` placeholder if no other row remains.

- [ ] **Step 7: Full gates, then commit**

```bash
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1
timeout 300 npx vitest run
timeout 300 npx svelte-check --threshold error 2>&1 | tail -5
```

```bash
git add -A
git commit -m "docs: Undo to here landed — close Plan F carry-forward (e)"
```

---

## Ear-check owed (owner)

No suite can answer these; add them to the backlog file when this merges.

1. Make four or five edits, open the HISTORY dock, pick a row three back and
   press UNDO TO HERE — does the project land where the detail pane said it
   would?
2. Then press REDO twice: do the walked-back edits come back in order?
3. Start playback, then walk back while the transport is rolling — does it
   work (it must: transport is transient), and does the engine keep playing
   the rebuilt graph without a click?
