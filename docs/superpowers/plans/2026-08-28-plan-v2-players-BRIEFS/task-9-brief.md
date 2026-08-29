## Task 9: Audio-clip players in the live graph

**Files:**
- Modify: `src-tauri/src/audio/engine.rs` (`rebuild` phase 1 and phase 2)
- Modify: `src-tauri/src/audio/offline.rs` (V-15 regression test)
- Modify: `src-tauri/src/control/mod.rs` (`player_fire`, `player_stop`)
- Modify: `src-tauri/src/lib.rs` (register the commands)

**Interfaces:**
- Consumes: everything from Tasks 1-8.
- Produces: `ControlPlane::player_fire(&self, id: &str) -> Result<(), String>`, `ControlPlane::player_stop(&self, id: &str) -> Result<(), String>`, `ControlPlane::player_clock_for(&self, id: &str) -> Option<u32>`; tauri commands `player_fire`, `player_stop`, `player_add`, `player_remove`, `players_get`.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/audio/offline.rs`'s test module:

```rust
    /// V-15, and V-2's own argument made executable: the bounce has no
    /// concept of a pad press, so a player in the document must contribute
    /// nothing to it. Rendering one would put a pad's clip at bar 1 of the
    /// export.
    #[test]
    fn a_player_contributes_nothing_to_the_bounce() {
        let mut store = store_with_one_audio_track_and_clip(); // existing fixture
        let without = render_project_to_samples(&store);

        let mut p = crate::audio::player::Player::new(
            crate::ids::PlayerId::from("p1"),
            "PAD",
        );
        p.source = crate::audio::player::PlayerSource::AudioClip {
            clip_id: store.clips[0].id.clone(),
        };
        store.players.push(p);
        let with = render_project_to_samples(&store);

        assert_eq!(with, without, "a player must not reach the bounce");
    }
```

Add to `src-tauri/src/control/mod.rs`'s test module:

```rust
    /// The V2 gate's first line: a pad fires a WAV while the arrangement
    /// plays, and the arrangement's transport does not move.
    #[test]
    fn firing_an_audio_player_sounds_without_touching_the_transport() {
        let cp = test_control_plane_with_an_audio_clip();
        let player_id = cp.add_audio_player("c1", /* raw */ true).unwrap();
        cp.transport(TransportAction::Seek { position_samples: 96_000 }).unwrap();
        cp.transport(TransportAction::Play).unwrap();

        cp.player_fire(&player_id).unwrap();

        let clock = cp.player_clock_for(&player_id).expect("the player has a clock");
        assert!(cp.tables_for_tests().clocks.is_on(clock));
        assert_eq!(cp.transport_state().position_samples, 96_000);
    }

    #[test]
    fn firing_an_unknown_player_is_an_error() {
        let cp = test_control_plane_with_an_audio_clip();
        assert!(cp.player_fire("ghost").unwrap_err().contains("unknown player"));
    }

    /// V-16: raw is the source's samples at unity. The comparison is
    /// against the clip's own source data, read directly — the same bytes
    /// the browser audition path plays.
    #[test]
    fn a_raw_player_renders_the_sources_samples_at_unity() {
        let cp = test_control_plane_with_an_audio_clip_at_minus_six_db_on_a_gained_track();
        let player_id = cp.add_audio_player("c1", true).unwrap();
        cp.player_fire(&player_id).unwrap();

        let rendered = cp.render_one_block_for_tests();
        let source = cp.source_samples_for_tests("c1");
        assert_eq!(
            &rendered[..64],
            &source[..64],
            "raw means the file's samples, not the clip's or the track's"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib control::tests::firing 2>&1 | tail -20`
Expected: FAIL — `no method player_fire`.

- [ ] **Step 3: Assemble player rows in `rebuild`**

In phase 1, after the track loop, append one `RtTrack` per player. A player
row is a clips row whose clip sits at position 0 — the ephemeral placement
of design §3, made concrete:

```rust
            // Plan V: one row per player, appended AFTER every track's so a
            // player never renumbers a track's slot. Its clip is placed at
            // position 0, because a player's playhead starts at 0 on press
            // — that is what "an ephemeral placement" means in the renderer.
            for p in s.players.iter() {
                let Some(&slot) = slots_s.get(&TrackId::from(p.id.as_str())) else { continue };
                let clips = match &p.source {
                    crate::audio::player::PlayerSource::AudioClip { clip_id } => {
                        match s.clips.iter().find(|c| &c.id == clip_id) {
                            Some(c) => player_clip_row(c, p.raw, &self.decoded),
                            None => Vec::new(), // a deleted source is silence, not a panic
                        }
                    }
                    _ => Vec::new(), // MIDI sources are Task 10
                };
                tracks.push(RtTrack::clips(slot, clips));
            }
```

with a helper beside `audio_row_for`:

```rust
/// The one `RtClip` a player's audio source becomes: the clip's SOURCE
/// region, rebased to position 0.
///
/// V-16 lives here. A raw player takes the source's samples for the clip's
/// region at unity — no clip gain, no fades — so a raw pad is bit-identical
/// to auditioning the file. A non-raw player keeps the clip's own gain and
/// fades, because then the pad is playing the clip, not the file.
fn player_clip_row(
    c: &Clip,
    raw: bool,
    decoded: &HashMap<SourceId, Arc<RtClipData>>,
) -> Vec<RtClip> {
    let Some(samples) = decoded.get(&c.source_id).cloned() else { return Vec::new() };
    vec![RtClip {
        start: 0,
        offset: c.offset_samples,
        len: c.length_samples,
        gain: if raw { 1.0 } else { mixer::db_to_linear(c.gain_db) as f32 },
        fade_in: if raw { 0 } else { c.fade_in_samples },
        fade_out: if raw { 0 } else { c.fade_out_samples },
        samples,
    }]
}
```

Read `Clip`'s actual field names in `audio/types.rs` before writing this —
the names above follow the existing clip-assembly code in `rebuild`, and
that code is the authority.

In phase 2, extend the slot map, slot count, send lanes, `ParamTable`
initialisation and the clock table to cover players:

```rust
            let slots = derive_slots_with_players(&store.tracks, &store.players);
            let send_slots =
                crate::audio::types::derive_send_slots_with_players(&store.tracks, &store.players);
            let n_slots = mixer_slot_count_with_players(&store.tracks, &store.players);
            let params = Arc::new(ParamTable::with_slots_and_sends(
                n_slots,
                crate::audio::types::send_slot_count_with_players(&store.tracks, &store.players),
            ));
            // ... the existing track loop, unchanged ...
            // Plan V: a player's fader comes from its COMPILED node, so a
            // raw player is unity here without this loop knowing the word.
            for (i, p) in store.players.iter().enumerate() {
                let node = crate::audio::node::MixNode::from(p);
                let Some(&slot) = slots.get(&node.id) else { continue };
                params.set_gain_pair_linear(slot, mixer::db_to_linear(node.gain_db));
                params.set_pan(slot, node.pan as f32);
                params.set_flag(slot, super::rt::FLAG_MUTE, node.muted);
                for snd in &node.sends {
                    let Some(&idx) = send_slots.get(&snd.id) else { continue };
                    params.set_send_amount_linear(idx, mixer::db_to_linear(snd.amount_db));
                }
                // Each player owns clock 1 + i, bound for the life of this
                // graph: a player's clock never changes, only its state does.
                clocks.bind_slot(slot, 1 + i as u32);
            }
```

and publish `player_clocks: HashMap<PlayerId, u32>` on `GraphTables`.

The compiler input becomes `mix_nodes_with_players(&s.tracks, &s.players)`
in `rebuild` — and stays `mix_nodes(&store.tracks)` in `offline.rs`, which
is V-15.

- [ ] **Step 4: Add the commands**

In `control/mod.rs`:

```rust
    pub fn player_clock_for(&self, id: &str) -> Option<u32> {
        self.tables.lock().player_clocks.get(&PlayerId::from(id)).copied()
    }

    /// Fire a player. Transient by construction: a press is not a document
    /// change, so it commits no op and takes no undo entry — the same
    /// reasoning that keeps transport actions out of the history.
    pub fn player_fire(&self, id: &str) -> Result<(), String> {
        let (len, looping) = {
            let s = self.session.lock();
            let p = s
                .store
                .players
                .iter()
                .find(|p| p.id.as_str() == id)
                .ok_or_else(|| format!("unknown player: {id}"))?;
            (self.player_source_length(&s, p)?, p.trigger.mode == TriggerMode::Loop)
        };
        let tables = self.tables.lock();
        let clock = tables
            .player_clocks
            .get(&PlayerId::from(id))
            .copied()
            .ok_or_else(|| format!("player has no clock yet: {id}"))?;
        tables.clocks.fire(clock, 0, len, looping);
        Ok(())
    }

    pub fn player_stop(&self, id: &str) -> Result<(), String> {
        let tables = self.tables.lock();
        let clock = tables
            .player_clocks
            .get(&PlayerId::from(id))
            .copied()
            .ok_or_else(|| format!("unknown player: {id}"))?;
        tables.clocks.stop(clock);
        Ok(())
    }
```

`player_source_length` reads the clip's `length_samples` for an audio
source, the tick-converted length for a MIDI source (Task 10), and returns
0 for `PlayerSource::None` — a control-only pad has nothing to sound.

Add the `#[tauri::command]` wrappers (`player_fire`, `player_stop`,
`player_add`, `player_remove`, `players_get`) and register them in
`lib.rs`'s `generate_handler!` beside the launch commands. `player_add` and
`player_remove` commit `Op::PlayerAdd` / `Op::PlayerRemove` through
`commit`, exactly as `track_add` does.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib 2>&1 | tail -30`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src
git commit -m "feat(players): audio-clip players in the live graph (R1, R2, V-15, V-16)"
```

---

