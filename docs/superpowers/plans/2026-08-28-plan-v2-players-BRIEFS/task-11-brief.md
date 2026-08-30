## Task 11: Trigger modes

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (`player_fire`, `player_stop`)

**Interfaces:**
- Consumes: `TriggerMode` (Task 1), `ClockTable::fire`'s `looping` argument (Task 6).
- Produces: no new public names — `player_fire` and `player_stop` gain behaviour.

- [ ] **Step 1: Write the failing test**

Add to `control/mod.rs`'s test module:

```rust
    #[test]
    fn one_shot_plays_to_its_end_and_stops_itself() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = cp.add_audio_player("c1", false).unwrap();
        cp.set_trigger_mode(&id, TriggerMode::OneShot).unwrap();
        cp.player_fire(&id).unwrap();
        let clock = cp.player_clock_for(&id).unwrap();
        let tables = cp.tables_for_tests();
        assert!(tables.clocks.is_on(clock));
        tables.clocks.advance(cp.source_length_for_tests("c1"));
        assert!(!tables.clocks.is_on(clock), "a one-shot ends on its own");
    }

    #[test]
    fn gate_stops_on_release_before_the_source_ends() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = cp.add_audio_player("c1", false).unwrap();
        cp.set_trigger_mode(&id, TriggerMode::Gate).unwrap();
        cp.player_fire(&id).unwrap();
        cp.player_stop(&id).unwrap();
        assert!(!cp.tables_for_tests().clocks.is_on(cp.player_clock_for(&id).unwrap()));
    }

    #[test]
    fn loop_wraps_instead_of_ending() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = cp.add_audio_player("c1", false).unwrap();
        cp.set_trigger_mode(&id, TriggerMode::Loop).unwrap();
        cp.player_fire(&id).unwrap();
        let clock = cp.player_clock_for(&id).unwrap();
        let tables = cp.tables_for_tests();
        tables.clocks.advance(cp.source_length_for_tests("c1") * 2);
        assert!(tables.clocks.is_on(clock), "a loop does not end");
    }

    /// A retrigger rewinds THIS player and nothing else — the property the
    /// single overlay could not have.
    #[test]
    fn retriggering_one_player_leaves_another_sounding() {
        let cp = test_control_plane_with_two_audio_clips();
        let a = cp.add_audio_player("c1", false).unwrap();
        let b = cp.add_audio_player("c2", false).unwrap();
        cp.player_fire(&a).unwrap();
        cp.player_fire(&b).unwrap();
        let tables = cp.tables_for_tests();
        tables.clocks.advance(128);
        cp.player_fire(&a).unwrap();
        assert!(tables.clocks.is_on(cp.player_clock_for(&b).unwrap()), "b untouched");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib control::tests::one_shot 2>&1 | tail -20`
Expected: FAIL on the loop and gate cases.

- [ ] **Step 3: Write the minimal implementation**

`player_fire` already passes `looping` (Task 9). Gate and one-shot differ
only in who stops the clock: the release for gate, `ClockTable::advance`
for one-shot. Both already work — the implementation here is the
`set_trigger_mode` helper that commits `Op::Set { object:
ObjectRef::Player(id), path: PropPath::TriggerMode, .. }`, plus whatever
the tests reveal is missing.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib control 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/control/mod.rs
git commit -m "feat(players): one-shot, gate and loop trigger modes"
```

---

