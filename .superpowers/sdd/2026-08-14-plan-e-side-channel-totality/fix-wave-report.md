# Plan E post-merge follow-up — fix wave report

Branch: `fix/plan-e-followup`, cut from `origin/main` (`27911d8`). PR **#18**.
Scope: exactly the final whole-branch review's **FIX NOW** triage list.
Nothing HOLD-triaged was touched.

Suites, per commit and at HEAD: **506 backend** (475 lib + 31 integration,
baseline 501 + 5 new tests) / **206 frontend**, `cargo check --lib --tests`
zero warnings. Foreground, `timeout`-guarded.

| Commit | Item |
|---|---|
| `58ab05a` | C-1 — epoch-guarded history/journal sink |
| `efdb4c9` | I-2 LoopJam back-off + bound; Task-13 stale citations; M-3 debug assertion |
| `b01d347` | I-5 base64 state blobs (`OP_FORMAT_VERSION` 2) + L-1 closed |
| docs commit | I-4's L-4/L-5, inventory updates, `next-prompt.md` |

---

## 1. C-1 — the epoch guard now covers both sinks

**What was wrong.** `execute_persist` re-checked `committed.epoch` against
the live `session.epoch` and refused to write a stale document.
`record_commit`/`record_gesture`, running a few lines later in the same
post-lock window, did not — `HistoryLog` had no notion of the epoch at all.
The window is wide (it contains `execute_host_forward`'s blocking plugin
main-thread round-trips and `execute_persist`'s disk I/O), so an epoch
function landing inside it produced, from one commit, a journal line in the
NEW project's already-rotated file carrying the OLD epoch, and a live,
poppable undo entry pushed AFTER `History::clear` ran.

**The fix.** `HistoryLog` owns the epoch its two streams describe:

* advanced by `epoch_boundary` (the one place a document swap is announced),
* left alone by `snapshot_mark` (not a swap),
* checked by `record_commit`, `record_gesture` and `snapshot_mark`, which
  drop the record with a `log::warn!` when it has moved.

Same shape and same justification as the persist guard, at the one sink
that already existed.

**The one design decision worth stating.** The epoch is a `Mutex<u64>`,
not an `AtomicU64`, and it is held across the check AND the writes. An
atomic read-then-act would leave exactly the window being closed: read the
still-old epoch, get overtaken by the entire boundary (clear + rotate),
then append into the NEW journal and push onto the freshly cleared stack.
Correctness here requires the check and the write to be one step with
respect to `epoch_boundary`, and only a lock both sides take gives that.

**Lock-order impact — none.** Within `history.rs` the order is now
`epoch -> history / journal`; `epoch` is acquired first by every method
taking more than one, and `history`/`journal` are never held
simultaneously, so no order between those two exists to violate. Globally
the module stays the leaf (`gesture -> session -> epoch -> history /
journal`): nothing takes the session or gesture lock while holding any of
the three, every call site is post-`transact`, and the journal's disk write
still happens with no other lock held. The module doc records this.
The clean fix did **not** fight the lock-order rules — no BLOCKED.

**Tests.** Two, at both altitudes:

* `control::history::tests::a_stale_epoch_batch_reaches_neither_the_journal_nor_history`
  — the sink in isolation, including that the guard is not a mute button.
* `tests/journal_and_history.rs::a_commit_whose_sink_call_lands_after_a_document_swap_reaches_neither_stream`
  — a real `ControlPlane`, the review's reopen-the-SAME-project
  interleaving (identical ids: the case where a stale inverse applies
  *successfully* against the wrong revision instead of failing loudly),
  staged the way `execute_persist`'s own guard test stages its. Asserts no
  stale journal line, no stale undo entry, and that the live document still
  records normally afterwards. Verified RED before the fix.

**Residual found while fixing, NOT fixed (in-scope note, not scope creep).**
`ControlPlane::undo`/`redo` pop an entry, commit, and then push it back via
`push_redo`/`push_undo_unchanged`. If an epoch boundary lands between the
pop and the push, the entry is resurrected onto the new document's stack —
the same class C-1 describes, at the entry-MIGRATION path rather than the
recording path. The commit itself is now correctly dropped by the guard, so
the journal half is closed; the stack half needs an epoch on the migration
calls, which means plumbing `Committed.epoch` through `undo`/`redo`.
Narrower than C-1 (it needs the boundary inside a single undo call, not
inside any commit's effect phase) and it touches the same code I-6 will
restructure into `async` + `spawn_blocking`. Recommend it be bundled with
I-6 by whoever owns that.

## 2. I-2 — LoopJam's retry loop

The inner wait loop's first exit, `if !shared.playing { break }`, is taken
without sleeping — correct in itself (with the transport stopped there is
nothing to wait for), but it meant a repeatedly-failing `apply` ran
`break -> apply -> Err -> loop -> break -> apply -> …` with no sleep on the
path at all.

Fixed with both halves the review asked for: a `WATCH_INTERVAL` back-off
after every FAILED apply (the wait loop's own sleep only covers the playing
case) and `MAX_APPLY_ATTEMPTS = 20`, after which the watcher reports through
`last_error` and returns the machine to `Idle` instead of spinning. `apply`
now returns whether the watcher is done, which keeps "cancelled/superseded"
(stop, nothing to retry) distinct from "commit lost its race" (retryable) —
previously both were a bare `return`.

The constant is documented rather than asserted: the retry exists for a
narrow snapshot-vs-commit race that a re-plan against fresh truth normally
wins next attempt; a failure surviving 20 attempts is a standing condition
(target track gone, pending clip id already present), not that race.
Give-up latency is ~100 ms stopped, ~20 loop passes playing.

Test: `a_swap_that_keeps_failing_backs_off_and_gives_up_instead_of_spinning`
— the mid-air race the Task 8 ledger wanted forced rather than
code-inspected. A pending swap whose `ClipAdd` collides with an existing id
fails on every attempt, from a stopped transport. It asserts both halves:
that it terminates, and that elapsed time is at least the implied back-off
(a spin returns in ~0 ns). Note this test could not have passed against the
old code in any form — it would hang forever, which is the bug.

## 3. Task 13 — stale deadlock-audit citations

The five `request` call-site line numbers corrected to their lines at HEAD
(1301 / 1388 / 1397 / 1422 / 1433), re-verified after every other edit to
this file, with the invariant text unchanged. Added one sentence recording
that the audit's value is its navigability, so the numbers are re-checked
whenever the file moves.

## 4. I-5 + L-1 — one format edit, `OP_FORMAT_VERSION` 2

**I-5.** `PluginRemove.state` and `PluginSetState.state` now serialize as
base64 strings (`#[serde(with = ...)]`, `base64 0.22` — already in the lock
file transitively, now a direct dep). `None` still serializes as `null`, so
"no host state to save" stays distinct from "the empty blob".

**Why the version bump rather than an additive dodge.** The controller's
brief and the review agree, and the reasoning is recorded on the constant
itself: `OP_FORMAT_VERSION` became load-bearing when Task 17 wrote the first
journal line, but the journal is still WRITE-ONLY — the entire population of
v1 data is logs no code will ever parse. So the clean change is free now and
costs a migration after Plan F ships a replayer. Shipped deliberately
*without* a dual-shape reader, which is the whole point; a comment on the
constant records that this was the moment and why it will not come again.

**L-1.** `Op::PluginRemove.params` was captured since Task 9 and read by
nothing: undo restored the mirror from the copy PARKED in
`session.plugins.params`, an in-memory fact a cold replay does not have.
`apply_raw`'s `PluginRemove` arm now seeds the mirror from the op's own
field when the in-memory one is absent or empty. In-process behaviour is
unchanged — a populated mirror still wins, so an undo restores the user's
real values rather than whatever the op recorded. (I took the controller's
framing, "make apply seed from the op field when the mirror is absent",
over the report's alternative of adding a `params` slot to `PluginAdd`:
it closes the same gap, keeps `PluginAdd`'s shape fixed, and makes the
already-present field load-bearing instead of adding a second one.)

Tests: op serde asserts the base64 wire form for both fields, the `null`
case, byte-for-byte round-trip, and the size claim (base64 is more than 2x
smaller than the number array on a 4 KB blob — the "~4x" is not rhetorical);
session tests cover both directions of the params seed; the envelope-schema
test now journals a state-carrying op and asserts every line's `v` plus that
no blob reaches the wire as a number array. That last assertion has to live
in the test rather than the schema: `op-envelope.schema.json` has
`additionalProperties: true` by design (D-06), so it would validate either
shape.

## 5. I-4's caveats — recorded as L-4 and L-5

Added to `docs/SIDE-CHANNEL-INVENTORY.md`'s L-section in the L-1..L-3 style:

* **L-4** — journal FILE order is not `rev` order under concurrency; a
  reader must sort by `(epoch, rev)`. Includes the undo-stack half (a Ctrl+Z
  can apply an older batch's inverse over a newer batch's write) and notes
  that every line already carries both fields, so a reader has what it needs
  — it must use them. Structural fix left to Plan F, as triaged.
* **L-5** — a panicking `transact` closure diverges the log from the
  document permanently: `record_commit` runs only on the `Ok` path and there
  is no rollback, so nothing later reconciles it and no `(epoch, rev)` gap
  marks the spot.

Records, not fixes, per the triage.

## 6. M-3 — the invariant is now checked

`debug_assert_transient_invariant` runs in `commit_with_rebuild_mode` right
after `transact` returns. A transient batch may address only
`ObjectRef::Transport`; the one sanctioned exception is a mid-gesture fold,
recognized by a `#[cfg(debug_assertions)]` thread-local set across
`GestureState::commit_transient_and_fold` — the same device `session.rs`'s
`IN_TX` uses, and necessary because the commit reaches `Committer` several
frames away through a closure.

**The report's literal sketch would not have worked**, and this is worth
flagging for the re-review: "assert no op's `CoalesceKey` targets a
non-`Transport` object" fires on every mid-gesture fold, since those are
`Set{Track, Gain}` by construction. The exemption is what makes the
assertion expressible at all; without it the check is either always-failing
or so weak it catches nothing.

The assertion immediately earned its keep by catching a *test*:
`transient_commits_reach_neither_history_nor_the_journal` used a bare
transient track-gain commit as its "transient edit on a normal document
path" — exactly the unsanctioned shape the invariant forbids. That case now
runs through the mid-gesture path, which is what production actually
produces, asserted while the gesture is still open. It tests the same thing
(the `!meta.transient` gate at the sink) through a shape that is legal.

## What I did NOT do

HOLD-triaged and untouched: I-1, I-3, I-6, I-7, I-8, M-1, M-2, M-4, M-5,
M-6, M-7, M-8, and the frontend M-3. None turned out to be entangled with
C-1's fix; the only adjacency found is the undo/redo entry-migration
residual above, which is recorded, not fixed.

`next-prompt.md` — RECONCILED after the coordinator pointed out that my
"older revision" observation was sharper than I knew: the post-merge
rewrite exists as `001a241` on `plan-e-side-channels`, committed AFTER
PR #12's squash, so it never reached `main` and this PR had been editing
the pre-merge file. `001a241` is now cherry-picked onto this branch
(`517b420`) with its rewrite as the BASE and this PR's edits merged on
top; only the "R-1..R-3 / L-1..L-3" line genuinely conflicted. The
cherry-pick touched the two docs files and nothing else — code and both
suites are unmoved (506 / 206). Two things it forced, both done: Track
A's "Consumes" block still called the journal v1 and listed L-1 as open
(now v2, L-1 closed, L-4/L-5 added with the note that L-4's structural
fix and L-5's panic rollback are both Track A's), and the HOLD list now
carries the C-1 residual above, tagged to bundle with I-6.
