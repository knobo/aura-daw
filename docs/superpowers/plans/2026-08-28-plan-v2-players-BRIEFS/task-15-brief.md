## Task 15: Documentation and releasing the claim

**Files:**
- Modify: `docs/backlog/plan-v-players.md` (V2 → landed; rulings V-13…V-16)
- Modify: `docs/LANDED.md`
- Modify: `docs/TRAPS.md` (only if something cost an hour)
- Modify: `next-prompt.md` (delete the claim row; V3 becomes the next unclaimed item)

- [ ] **Step 1: Record the outcome**

Move V2's row in the backlog file's status table to `landed — PR #121`,
copy rulings V-13…V-16 into it, and write the owner's ear-check under a
heading of its own: *open SURFACE, put a WAV on a pad with `raw` ticked,
start the arrangement, hit the pad. The pad must sound bit-identical to
auditioning that file in the browser, and the arrangement's playhead must
not move.*

- [ ] **Step 2: Release the claim**

Delete the `Plan V — V2` row from `next-prompt.md`'s *Active claims* table,
leaving `| _(none)_ | | | |` if it is the last one, and update the "Next up"
list: V2 is done, **V3 — polyphony** is the next cut.

- [ ] **Step 3: Verify before claiming completion**

Run all four, and quote the output in the PR:

```bash
cd src-tauri && cargo test --lib 2>&1 | tail -5
cd src-tauri && cargo test --tests -- --test-threads=1 2>&1 | tail -5
npm test 2>&1 | tail -5
scripts/perf-check.sh --budget <BASELINE × 1.3>
```

- [ ] **Step 4: Commit and take the PR out of draft**

```bash
git add docs next-prompt.md
git commit -m "docs: Plan V V2 landed; release the claim"
git push
gh pr ready 121
```

---

## Self-review

**Spec coverage.** R1 (a pad fires an audio clip) — Task 9. R2 (raw) —
Tasks 4, 9, and V-16. R3 (an instrument no track owns) — Task 10. R4 (mix
and match) — falls out of Tasks 9-10, since players are independent nodes;
the *polyphony guarantees* (voice cap, choke) are V3 by the spec's own
cut table. R5 (the deck's own knobs) is V5 and R6 (recording) is V6 — both
explicitly out of scope above. V2's four gate lines map to: Task 9's
`firing_an_audio_player_sounds_without_touching_the_transport`, Task 10's
live-node tests, Task 7's grep gate, Task 12's migration file, and Task 3's
undo test.

**Design §8's open questions.** Question 5 (does a raw player ignore the
deck's output stage) is answered by V-16 and Task 4: a raw player's own node
is unity/centre/master, and any bus it feeds is an ordinary mixer node.
Questions 1, 2 and 4 size V3 and V6 and stay open. Question 3 (are players
visible in the mixer) is V7's, and nothing here forecloses it.

**Known gaps, stated rather than hidden.** Two fixtures this plan names —
`cp.render_one_block_for_tests()` and `cp.source_samples_for_tests()` in
Task 9 — do not exist yet; that task's implementer builds them alongside the
test, following `control/mod.rs`'s existing `for_tests` helpers. Task 7's
mixer tests use placeholder fixture names (`graph_with_one_clip_track`) that
must be replaced with the real ones in that module.
