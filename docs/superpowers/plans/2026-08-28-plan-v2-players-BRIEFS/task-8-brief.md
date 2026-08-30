## Task 8: Every scene gets its own clock; the transport stops being hijacked

**Files:**
- Modify: `src-tauri/src/midi/launch.rs:731-812` (`launch_fire_from`)
- Modify: `src-tauri/src/audio/engine.rs` (`rebuild`: size the clock table by the launch map)
- Modify: `src-tauri/src/control/mod.rs` (per-binding clock resolution)

**Interfaces:**
- Consumes: `ClockTable` (Task 6), the helpers from Task 7.
- Produces: `ControlPlane::scene_clock_for(binding_id: &str) -> Option<u32>`; `ControlPlane::fire_scene(binding_id: &str, track_ids: &[String], start: u64, end: u64)`; `ControlPlane::stop_scene(binding_id: &str) -> bool`. `SCENE_CLOCK` is deleted.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/midi/launch.rs`'s test module:

```rust
    /// Design §2.2's defect, killed: pressing a pad must not move the
    /// user's arrangement. This is the whole reason `FireOrigin::Hardware`
    /// existed as a separate arm.
    #[test]
    fn firing_from_hardware_does_not_move_the_transport() {
        let cp = test_control_plane_with_one_clip_binding();
        cp.transport(TransportAction::Seek { position_samples: 96_000 }).unwrap();
        cp.transport(TransportAction::Play).unwrap();
        let before = cp.transport_state();

        cp.launch_fire_from("b1", FireOrigin::Hardware).unwrap();

        let after = cp.transport_state();
        assert_eq!(after.position_samples, before.position_samples);
        assert_eq!(after.loop_enabled, before.loop_enabled);
        assert_eq!(after.state, "playing");
    }

    /// Two scenes sounding at once is what the single overlay could never
    /// do — and it is the reason a scene needs its OWN clock rather than a
    /// shared one.
    #[test]
    fn two_scenes_sound_at_once_on_different_clocks() {
        let cp = test_control_plane_with_two_region_bindings();
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        cp.launch_fire_from("b2", FireOrigin::Drive).unwrap();

        let c1 = cp.scene_clock_for("b1").expect("b1 has a clock");
        let c2 = cp.scene_clock_for("b2").expect("b2 has a clock");
        assert_ne!(c1, c2);
        let tables = cp.tables_for_tests();
        assert!(tables.clocks.is_on(c1));
        assert!(tables.clocks.is_on(c2), "firing b2 must not have stopped b1");
    }

    /// V-14. Two scenes naming the same track is newly expressible; ending
    /// the first must not take the track away from the second.
    #[test]
    fn stopping_one_scene_does_not_steal_a_track_the_other_now_owns() {
        let cp = test_control_plane_with_two_region_bindings_sharing_a_track();
        cp.launch_fire_from("b1", FireOrigin::Drive).unwrap();
        cp.launch_fire_from("b2", FireOrigin::Drive).unwrap();
        let c2 = cp.scene_clock_for("b2").unwrap();

        cp.stop_drive_launch("b1");

        let tables = cp.tables_for_tests();
        let slot = tables.slots[&TrackId::from("shared")];
        assert_eq!(tables.clocks.clock_of(slot), c2, "b2 still owns it");
    }
```

Read the existing fixtures at the bottom of `launch.rs` first; the three
`test_control_plane_*` helpers above should be built from whatever that
module already uses to construct a `ControlPlane`, not from scratch.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib midi::launch 2>&1 | tail -20`
Expected: FAIL — `firing_from_hardware_does_not_move_the_transport` fails on
the seek, and `scene_clock_for` does not exist.

- [ ] **Step 3: Size the clock table by the document**

In `engine.rs::rebuild`, replace the hardcoded `2`:

```rust
            // Plan V: clock 0 is the transport, then one per player
            // (Task 9), then one per scene binding. Sizing from the
            // document means a fire is an atomic write into a lane that
            // already exists — a pad press must never rebuild the graph.
            let scene_ids: Vec<String> = store_scene_binding_ids(&session.midi.launch_maps);
            let n_clocks = 1 + store.players.len() + scene_ids.len();
            let clocks = Arc::new(crate::audio::clock::ClockTable::with_slots_and_clocks(
                n_slots, n_clocks,
            ));
```

with a free function beside it:

```rust
/// Every binding that needs a clock of its own, in document order — which
/// is what makes a clock index stable between one rebuild and the next for
/// an unchanged map. Region targets only: a Clip target is a player after
/// the migration (Task 13), and a player's clock is assigned above.
fn store_scene_binding_ids(maps: &[crate::midi::launch::LaunchMap]) -> Vec<String> {
    maps.iter()
        .flat_map(|m| m.bindings.iter())
        .filter(|b| matches!(b.target, crate::midi::launch::LaunchTarget::Region { .. }))
        .map(|b| b.id.clone())
        .collect()
}
```

Publish `scene_clocks: HashMap<String, u32>` (binding id → clock) on
`GraphTables` beside `slots`, built as `1 + players.len() + i`.

- [ ] **Step 4: Rewrite the fire path**

In `control/mod.rs`, replace the `SCENE_CLOCK` constant with:

```rust
    /// The clock a scene binding fires, or `None` when the graph has not
    /// been rebuilt since the binding was added. A missing clock drops the
    /// fire with a warn rather than firing the wrong one — the same
    /// "unknown index means drop the write" rule `ParamTable`'s setters use.
    pub fn scene_clock_for(&self, binding_id: &str) -> Option<u32> {
        self.tables.lock().scene_clocks.get(binding_id).copied()
    }

    pub fn fire_scene(&self, binding_id: &str, track_ids: &[String], start: u64, end: u64) {
        let tables = self.tables.lock();
        let Some(&clock) = tables.scene_clocks.get(binding_id) else {
            log::warn!("launch: no clock for binding {binding_id} — dropping the fire");
            return;
        };
        for id in track_ids {
            if let Some(&slot) = tables.slots.get(&TrackId::from(id.as_str())) {
                tables.clocks.bind_slot(slot, clock);
            }
        }
        tables.clocks.fire(clock, start, end, false);
    }

    pub fn stop_scene(&self, binding_id: &str) -> bool {
        let tables = self.tables.lock();
        let Some(&clock) = tables.scene_clocks.get(binding_id) else { return false };
        let was_on = tables.clocks.is_on(clock);
        tables.clocks.stop(clock);
        // V-14: release only what this clock still owns.
        for slot in 0..tables.params.len() {
            tables.clocks.release_slot_if(slot, clock);
        }
        was_on
    }
```

In `midi/launch.rs::launch_fire_from`, collapse the three origin arms to
one — the difference between them was which hack fired:

```rust
        runtime().set_audible_tracks(track_ids.clone());
        log::info!(
            "launch: fire id={id} name={name} origin={origin:?} start={start} end={end} tracks={track_ids:?}"
        );
        // Every origin now does the same thing, because the reason they
        // differed is gone: `Hardware` used to SetLoop + Seek + Play on the
        // arrangement transport (design §2.2), which moved the user's
        // playhead every time they touched a pad. A scene has its own clock
        // now, so firing one is firing one, whoever pressed it.
        self.fire_scene(id, &track_ids, start, end);
```

`FireOrigin` stays — `LaunchFired::follow_view` still distinguishes a
hardware press (the view follows) from a drive-clip fire (it does not).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib midi::launch 2>&1 | tail -20`
Expected: PASS.

Run: `cd src-tauri && cargo test --lib 2>&1 | tail -20`
Expected: PASS. Any test asserting that a hardware fire seeks the transport
is now asserting the defect — delete it and say so in the commit message.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/midi/launch.rs src-tauri/src/audio/engine.rs src-tauri/src/control/mod.rs
git commit -m "fix(launch): a scene owns its clock; a pad press stops moving the transport"
```

---

