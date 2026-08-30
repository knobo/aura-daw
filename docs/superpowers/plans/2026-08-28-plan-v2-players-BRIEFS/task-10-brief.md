## Task 10: MIDI-clip players with an instrument no track owns

**Files:**
- Modify: `src-tauri/src/midi/playback.rs` (a player-keyed live node)
- Modify: `src-tauri/src/audio/engine.rs` (attach it in `rebuild`)

**Interfaces:**
- Consumes: `PlayerSource::MidiClip` (Task 1), the player rows from Task 9.
- Produces: `midi::playback::live_source_for_player(session, player, rate) -> Option<LiveSource>` — events scheduled with sample 0 at the clip's start.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/midi/playback.rs`'s test module:

```rust
    /// R3: a plugin instance owned by no track renders somewhere. The
    /// somewhere is a player.
    #[test]
    fn a_player_gets_a_live_node_for_an_instance_no_track_owns() {
        let session = session_with_an_untracked_instrument(); // fixture
        let player = player_with_midi_clip("mc1", "plugin:i1");
        let src = live_source_for_player(&session, &player, 48_000).expect("a live source");
        assert!(!src.events.is_empty());
    }

    /// The whole point of a player's own playhead: its events are LOCAL.
    /// A clip that sits at bar 9 of the arrangement still starts at sample
    /// 0 when a pad fires it.
    #[test]
    fn a_players_events_are_rebased_so_the_first_note_is_at_sample_zero() {
        let session = session_with_a_midi_clip_at_bar_nine();
        let player = player_with_midi_clip("mc1", "plugin:i1");
        let src = live_source_for_player(&session, &player, 48_000).unwrap();
        assert_eq!(
            src.events[0].at, 0,
            "the press is time zero, not the clip's arrangement position"
        );
    }

    #[test]
    fn a_player_with_no_instrument_has_no_live_source() {
        let session = session_with_a_midi_clip_at_bar_nine();
        let mut player = player_with_midi_clip("mc1", "plugin:i1");
        if let crate::audio::player::PlayerSource::MidiClip { instrument_id, .. } =
            &mut player.source
        {
            *instrument_id = None;
        }
        assert!(live_source_for_player(&session, &player, 48_000).is_none());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib midi::playback::tests::a_player 2>&1 | tail -20`
Expected: FAIL — `cannot find function live_source_for_player`.

- [ ] **Step 3: Write the minimal implementation**

`live_source_for_player` mirrors the existing per-track path
(`node_for_track` and the scheduling around it), with two differences,
both of which get a comment:

```rust
/// The live source for a MIDI player (Plan V, R3).
///
/// Two differences from the per-track path, both consequences of a player
/// having its own playhead:
///
/// * The node is keyed by PLAYER id, not track id. The instance it hosts is
///   owned by no track — `PluginInstanceInfo::track_id` is already optional,
///   so an untracked instance has always been legal; it simply had nowhere
///   to render until there was a player.
/// * Events are rebased so the clip's start is sample 0. A player's playhead
///   starts at 0 on press, so absolute arrangement positions would put every
///   note in the future and the pad would sound silent.
pub fn live_source_for_player(
    session: &Session,
    player: &Player,
    rate: u32,
) -> Option<LiveSource> {
    let PlayerSource::MidiClip { clip_id, instrument_id } = &player.source else {
        return None;
    };
    let instrument_id = instrument_id.as_ref()?;
    let clip = session.midi.clips.iter().find(|c| &c.id == clip_id)?;
    let node = node_for_player(session, &player.id, instrument_id, rate)?;
    let map = crate::midi::TempoMap::new(session.midi.ppq, session.midi.tempo_events.clone(), rate.max(1)).ok()?;
    let origin = map.tick_to_samples(clip.timeline_start_ticks);
    let events: Vec<AbsNoteEvent> = schedule_clip(clip, &map)
        .into_iter()
        .map(|mut e| {
            e.at = e.at.saturating_sub(origin);
            e
        })
        .collect();
    Some(LiveSource { node, events: Arc::new(events) })
}
```

`node_for_player` is `node_for_track` with the cache key changed from the
track id to `format!("player:{}", player_id)` — read `node_for_track`'s
live-node key construction (it includes `state_rev`, which must stay, or a
zyn patch load will never reach the player's node) and mirror it exactly.
`schedule_clip` is whatever the per-track path already uses to turn one
clip's notes into `AbsNoteEvent`s; reuse it rather than writing a second
scheduler.

In `engine.rs::rebuild`'s player loop, attach the source:

```rust
                    crate::audio::player::PlayerSource::MidiClip { .. } => {
                        let mut row = RtTrack::clips(slot, Vec::new());
                        row.live = crate::midi::playback::live_source_for_player(&s, p, self.cache_rate);
                        tracks.push(row);
                        continue;
                    }
```

and teach `player_source_length` (Task 9) to return the MIDI clip's
tick-converted length.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib midi::playback 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/midi/playback.rs src-tauri/src/audio/engine.rs
git commit -m "feat(players): MIDI players own their instrument (R3)"
```

---

