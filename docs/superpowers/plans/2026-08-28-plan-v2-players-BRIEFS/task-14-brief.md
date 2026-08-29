## Task 14: The performance gate, run properly

**Files:**
- Modify: `src-tauri/tests/plugin_load_profile.rs` (an idle-players case)

**Interfaces:** none — this task produces numbers, not names.

- [ ] **Step 1: Add the idle-player case to the harness**

Read `plugin_load_profile.rs` first and follow its existing case shape. The
new case is the question the owner actually asked: what do configured but
silent pads cost?

```rust
/// Plan V's cost question, made measurable: 32 players configured, 8
/// sounding. A pad that is not sounding must cost one atomic load per
/// block and nothing else — if idle players show up in this number, the
/// early-out in the strip is not where it should be.
#[test]
fn thirty_two_idle_players_and_eight_sounding() {
    // ... build the same session the existing cases build, plus players ...
}
```

- [ ] **Step 2: Measure the baseline on `origin/main`**

```bash
git stash
git checkout origin/main
scripts/perf-check.sh --measure    # record: BASELINE µs
git checkout -
git stash pop
```

- [ ] **Step 3: Measure the branch, in the same sitting**

```bash
scripts/perf-check.sh --budget $(( BASELINE * 13 / 10 ))
```

Expected: exit 0. If it fails, the fix is not a bigger budget — bisect the
tasks with `scripts/perf-check.sh --harness-from main` per
`docs/STANDING-CONSTRAINTS.md` §Performance.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/tests/plugin_load_profile.rs
git commit -m "perf: harness case for idle players under load"
```

---

