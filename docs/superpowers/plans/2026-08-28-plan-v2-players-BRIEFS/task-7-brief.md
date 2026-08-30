## Task 7: The mixer reads clocks; the overlay is deleted

**This is the behaviour-neutral swap.** Nothing new sounds after this task
— the launch overlay is re-expressed on top of one scene clock and every
existing launch test still passes, unedited where possible. Treat an edited
launch assertion as a finding to explain in the PR, not a chore.

**Files:**
- Modify: `src-tauri/src/audio/rt.rs:23-27` (`FLAG_LAUNCH`), `:56-67` (`LaunchPlayhead`), `:152-263` (the launch atomics and their methods), `RtGraph`
- Modify: `src-tauri/src/audio/mixer.rs:430-455` (`track_playhead`), `:585-700` (the `render*` family), `:700-720` (the strip prologue)
- Modify: `src-tauri/src/audio/engine.rs:723-745`, `:795-812`, `rebuild`
- Modify: `src-tauri/src/audio/offline.rs` (construct a `ClockTable` for the bounce graph)
- Modify: `src-tauri/src/control/mod.rs:1790-1835`

**Interfaces:**
- Consumes: `ClockTable`, `Playhead`, `TRANSPORT_CLOCK` (Task 6).
- Produces: `RtGraph::clocks: Arc<ClockTable>`; `GraphTables::clocks: Arc<ClockTable>`; `mixer::node_playhead(clocks: &ClockTable, slot: usize, base_pos: u64, lp: &LoopSpec, disc: bool) -> (u64, LoopSpec, bool, bool)` returning `(pos, loop_spec, discontinuity, audible)`. `render`, `render_rt`, `render_rt_with_input` keep their current signatures **minus** the `launch` parameter; `render_rt_launch` is **deleted**.

- [ ] **Step 1: Write the failing test**

Rewrite the two overlay tests in `mixer.rs` against clocks, keeping their
names and their claims. Replace
`launch_overlay_plays_the_scene_not_the_arrangement_playhead` (line 1201)
and `launch_overlay_still_plays_the_scene_through_inserts` (line 1899) with:

```rust
    /// The claim the overlay test made, now made of clocks: a node bound to
    /// a running non-transport clock renders at THAT clock's position, not
    /// the arrangement's.
    #[test]
    fn a_node_on_a_scene_clock_plays_the_scene_not_the_arrangement_playhead() {
        let clocks = ClockTable::with_slots_and_clocks(1, 2);
        clocks.set_transport_playing(true);
        clocks.fire(1, 100, 10_000, false);
        clocks.bind_slot(0, 1);

        let mut graph = graph_with_one_clip_track(); // existing fixture
        graph.clocks = Arc::new(clocks);
        let mut out = vec![0.0f32; 8];
        render(&mut graph, 50_000, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);

        assert!(out.iter().any(|s| *s != 0.0), "the scene sounds");
        assert_eq!(out[0], sample_of_the_clip_at(100), "at the clock's position");
    }

    /// V-13: with the transport stopped, a transport-clock node renders
    /// nothing while a fired clock still sounds. This is what
    /// `LaunchPlayhead::exclusive` used to say.
    #[test]
    fn a_stopped_transport_silences_arrangement_nodes_but_not_a_fired_one() {
        let clocks = ClockTable::with_slots_and_clocks(2, 2);
        clocks.set_transport_playing(false);
        clocks.fire(1, 0, 10_000, false);
        clocks.bind_slot(1, 1);
        // slot 0 stays on the transport

        let mut graph = graph_with_two_clip_tracks(); // existing fixture
        graph.clocks = Arc::new(clocks);
        let mut out = vec![0.0f32; 8];
        render(&mut graph, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);

        assert_eq!(
            out[0],
            sample_of_track_one_at(0),
            "only the fired node contributes"
        );
    }
```

Read the existing fixtures in `mixer.rs`'s test module before writing this
— reuse whatever `launch_overlay_plays_the_scene_not_the_arrangement_playhead`
already builds rather than inventing `graph_with_one_clip_track`; the names
above are placeholders for those.

Add a real grep gate as `src-tauri/src/audio/rt.rs`'s own test:

```rust
    /// V-4's gate. The overlay's single atomic set is DELETED, not
    /// deprecated: `audio::clock` is the only playhead mechanism now, and a
    /// reintroduced `launch_*` atomic here would silently give the engine a
    /// second, contradictory notion of where a node is.
    #[test]
    fn the_launch_overlay_is_gone_from_this_file() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/audio/rt.rs"),
        )
        .expect("rt.rs is readable");
        // Skip this test's own body, which necessarily names them.
        let body = src.split("fn the_launch_overlay_is_gone_from_this_file").next().unwrap();
        for banned in [
            "launch_on",
            "launch_pos",
            "launch_start",
            "launch_end",
            "launch_discont",
            "launch_ended",
            "LaunchPlayhead",
            "FLAG_LAUNCH",
        ] {
            assert!(
                !body.contains(banned),
                "{banned} is back in rt.rs — see audio::clock and ruling V-4"
            );
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib audio::rt::tests::the_launch_overlay 2>&1 | tail -20`
Expected: FAIL — the banned symbols are still present.

- [ ] **Step 3: Delete the overlay from `rt.rs`**

Remove: `FLAG_LAUNCH` (line 25), `LaunchPlayhead` (lines 56-67), the six
`launch_*` atomics on `SharedRt` (lines 152-161), their initialisers, and
`arm_launch`, `clear_launch`, `end_launch`, `take_launch_ended`,
`launch_overlay`, `advance_launch` (lines 207-263), plus the two tests at
lines 983-1002 that exercise them.

Add to `RtGraph`:

```rust
    /// THIS graph's playheads (Plan V). Versioned with the snapshot for the
    /// same reason `params` is: a retired graph keeps reading the table it
    /// was built with, so a rebuild that renumbers clocks cannot bleed into
    /// a render already in flight.
    pub clocks: Arc<crate::audio::clock::ClockTable>,
```

and `clocks: Arc::new(ClockTable::default())` to `RtGraph::new`. Add the
same field to `GraphTables` so the control plane can reach the CURRENT
graph's clocks to fire into.

- [ ] **Step 4: Rewrite `track_playhead` as `node_playhead`**

In `mixer.rs`, replace `track_playhead` (lines 430-455) with:

```rust
/// Where this node's playhead is for THIS block, whether that position is a
/// discontinuity, and whether the node renders at all.
///
/// One indexed atomic load, which is what `track_playhead` cost before the
/// clock table replaced the overlay — the flag test became a clock lookup.
///
/// A non-transport clock has no `LoopSpec`: its loop is the start/end pair
/// the fire recorded, and `ClockTable::advance` wraps it. The arrangement's
/// `LoopSpec` therefore applies only to nodes on the transport clock, which
/// is exactly the rule the overlay had (`&LoopSpec::OFF` for a launched
/// track).
#[inline]
fn node_playhead(
    clocks: &crate::audio::clock::ClockTable,
    slot: usize,
    base_pos: u64,
    lp: &LoopSpec,
    discontinuity: bool,
) -> (u64, LoopSpec, bool, bool) {
    let ph = clocks.playhead(slot, base_pos, lp, discontinuity);
    let spec = if ph.is_transport { *lp } else { LoopSpec::OFF };
    (ph.pos, spec, ph.discontinuity, ph.on)
}
```

`LoopSpec` must be `Copy` for this; it is a small struct — if it is not,
return `&'a LoopSpec` borrowed from `lp` or `&LoopSpec::OFF` exactly as
`track_playhead` did, and keep the lifetime.

- [ ] **Step 5: Rewrite the render family**

Delete the `launch: Option<LaunchPlayhead>` parameter from `render_impl`,
delete `render_rt_launch` entirely, and update `render`, `render_rt` and
`render_rt_with_input` to stop passing `None`/`launch`.

In the prologue (line ~695):

```rust
    for tr in tracks.iter_mut() {
        tr.win = super::rt::TrackWindow::default();
        if tr.slot >= n_slots {
            continue;
        }
        let (_, _, track_disc, _) = node_playhead(&clocks, tr.slot, base_pos, lp, discontinuity);
        tr.win.disc = track_disc;
        let live_in_events = live_in.filter(|b| b.slot == tr.slot).map(|b| b.events).unwrap_or(&[]);
        prime_live(tr, track_disc, live_in_events);
    }
```

with `let clocks = graph.clocks.clone();` taken beside `let params =
graph.params.clone();` and `clocks` added to the destructuring `RtGraph {
.. }` exclusion list.

In the strip body (line ~700), the `on` computation collapses:

```rust
            let flags = params.flags[tr.slot].load(Relaxed);
            let (track_base, track_lp, _, clock_on) =
                node_playhead(&clocks, tr.slot, base_pos, lp, discontinuity);
            let on_clock = clocks.clock_of(tr.slot) != crate::audio::clock::TRANSPORT_CLOCK;
            // A node on its own clock is heard regardless of another track's
            // solo — a pad that goes silent because someone soloed a vocal is
            // the deck cutting out mid-performance. This is what FLAG_LAUNCH
            // used to say, now derived rather than stored.
            let on = clock_on
                && audible_with_launch(
                    flags & FLAG_MUTE != 0,
                    flags & FLAG_SOLO != 0,
                    any_solo,
                    on_clock,
                );
```

`audible_with_launch` keeps its name and body — its fourth argument is now
"reads a non-transport clock" rather than "carries FLAG_LAUNCH", which is
the same predicate stated in the new vocabulary. Rename its parameter from
`launch` to `own_clock` and update its doc comment.

- [ ] **Step 6: Wire the engine**

In `engine.rs::OutputCb::render` (lines 723-745), delete the whole
`overlay` block and replace the render gate:

```rust
        let clocks_running = self
            .graph
            .as_ref()
            .is_some_and(|g| g.clocks.any_running());
        match (&mut self.graph, playing, clocks_running) {
```

and after the render (line ~810), replace `self.shared.advance_launch(frames)`:

```rust
        if let Some(g) = self.graph.as_ref() {
            g.clocks.advance(frames);
        }
```

`set_transport_playing` is called from the control plane's transport
handling, not from the callback: find the site that stores
`SharedRt::playing` and mirror it onto `tables.lock().clocks`.

In `rebuild`'s phase 2, build the table beside the `ParamTable`. For this
task there is exactly one non-transport clock — the scene the overlay used
to be:

```rust
            // Plan V: this graph's playheads. One scene clock for now,
            // which is the overlay re-expressed; Task 8 gives every Region
            // binding its own and Task 9 gives every player one.
            let clocks = Arc::new(crate::audio::clock::ClockTable::with_slots_and_clocks(
                n_slots, 2,
            ));
            clocks.set_transport_playing(self.shared.playing.load(Relaxed));
            for t in store.tracks.iter() {
                if !launch_ids.iter().any(|id| id == t.id.as_str()) {
                    continue;
                }
                let Some(&slot) = slots.get(&t.id) else { continue };
                clocks.bind_slot(slot, 1);
            }
```

and publish it in `GraphTables` beside `params`, and onto the assembled
`RtGraph`.

In `offline.rs`, give the bounce graph
`ClockTable::with_slots_and_clocks(n_slots, 1)` with
`set_transport_playing(true)`: a bounce is the transport and nothing else,
which is V-15 stated as construction rather than as a filter.

- [ ] **Step 7: Rewrite the control-plane launch helpers**

In `control/mod.rs` (lines 1790-1835), the three helpers keep their names
and become clock writes. `stop_launch_overlay` returns whether something
was sounding, as it does today:

```rust
    pub fn apply_launch_audible(&self, track_ids: &[String]) {
        let tables = self.tables.lock();
        for slot in 0..tables.params.len() {
            tables.clocks.release_slot_if(slot, SCENE_CLOCK);
        }
        for id in track_ids {
            if let Some(&slot) = tables.slots.get(&TrackId::from(id.as_str())) {
                tables.clocks.bind_slot(slot, SCENE_CLOCK);
            }
        }
    }

    pub fn clear_launch_audible(&self) {
        crate::midi::launch::runtime().clear_audible_tracks();
        let tables = self.tables.lock();
        tables.clocks.stop(SCENE_CLOCK);
        for slot in 0..tables.params.len() {
            tables.clocks.release_slot_if(slot, SCENE_CLOCK);
        }
    }

    pub fn arm_drive_launch(&self, track_ids: &[String], start: u64, end: u64) {
        self.apply_launch_audible(track_ids);
        self.tables.lock().clocks.fire(SCENE_CLOCK, start, end, false);
    }

    pub fn stop_launch_overlay(&self) -> bool {
        let tables = self.tables.lock();
        let was_on = tables.clocks.is_on(SCENE_CLOCK);
        tables.clocks.stop(SCENE_CLOCK);
        for slot in 0..tables.params.len() {
            tables.clocks.release_slot_if(slot, SCENE_CLOCK);
        }
        was_on
    }
```

with `const SCENE_CLOCK: u32 = 1;` local to this module, carrying a comment
that Task 8 replaces it with a per-binding index. `drive_overlay_is` and
`clear_drive_overlay` follow the same substitution.

- [ ] **Step 8: Run the whole backend suite**

Run: `cd src-tauri && cargo test --lib 2>&1 | tail -30`
Expected: PASS. Then the integration tests:
Run: `cd src-tauri && cargo test --tests -- --test-threads=1 2>&1 | tail -30`
Expected: PASS. (`--test-threads=1` because of the known SIGSEGV in the
parallel suite — `docs/backlog/ci-hardening.md` item 5. Note in the PR
whether the parallel run also passed.)

- [ ] **Step 9: Run the performance gate**

```bash
git stash && git checkout origin/main
scripts/perf-check.sh --measure          # record N µs
git checkout - && git stash pop
scripts/perf-check.sh --budget $(( N * 13 / 10 ))
```

Expected: under budget. Record both numbers — they go in the PR.

- [ ] **Step 10: Commit**

```bash
git add src-tauri/src/audio src-tauri/src/control/mod.rs
git commit -m "refactor(audio): clocks replace the launch overlay, behaviour-neutral (V-4, V-13)"
```

---

