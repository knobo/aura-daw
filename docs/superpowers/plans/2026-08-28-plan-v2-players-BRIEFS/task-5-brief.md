## Task 5: Slot derivation over tracks and players

**Files:**
- Modify: `src-tauri/src/audio/types.rs:394-451`

**Interfaces:**
- Consumes: `Player` (Task 1).
- Produces: `derive_slots_with_players(tracks, players) -> HashMap<TrackId, usize>`, `mixer_slot_count_with_players(tracks, players) -> usize`, `send_slot_count_with_players(tracks, players) -> usize`, `derive_send_slots_with_players(tracks, players) -> HashMap<String, usize>`. Players occupy the slots after the tracks', in document order.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/audio/types.rs`'s `mod tests`:

```rust
    #[test]
    fn players_take_slots_after_every_track_and_never_move_one() {
        use crate::audio::player::Player;
        use crate::ids::PlayerId;

        let tracks = vec![test_track("a"), {
            let mut t = test_track("auto");
            t.kind = "automation".into();
            t
        }, test_track("b")];
        let players = vec![
            Player::new(PlayerId::from("p1"), "PAD 1"),
            Player::new(PlayerId::from("p2"), "PAD 2"),
        ];

        let plain = derive_slots(&tracks);
        let with = derive_slots_with_players(&tracks, &players);

        assert_eq!(with[&TrackId::from("a")], plain[&TrackId::from("a")]);
        assert_eq!(with[&TrackId::from("b")], plain[&TrackId::from("b")]);
        assert_eq!(with[&TrackId::from("p1")], 2);
        assert_eq!(with[&TrackId::from("p2")], 3);
        assert!(!with.contains_key(&TrackId::from("auto")), "automation owns no slot");
        assert_eq!(mixer_slot_count_with_players(&tracks, &players), 4);
    }

    /// A raw player has no sends by construction (V-6), so it must not
    /// consume a send lane either — the lane map and the compiled node have
    /// to agree or a knob write lands in a lane no graph reads.
    #[test]
    fn a_raw_player_consumes_no_send_lane() {
        use crate::audio::player::Player;
        use crate::ids::PlayerId;

        let tracks = vec![test_track("a")];
        let mut p = Player::new(PlayerId::from("p1"), "RAW");
        p.raw = true;
        p.node.sends.push(SendSlot {
            id: "s-ghost".into(),
            dest: "bus1".into(),
            amount_db: 0.0,
            pre_fader: false,
        });

        let players = vec![p];
        assert_eq!(send_slot_count_with_players(&tracks, &players), 0);
        assert!(!derive_send_slots_with_players(&tracks, &players).contains_key("s-ghost"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib audio::types::tests::players 2>&1 | tail -20`
Expected: FAIL — `cannot find function derive_slots_with_players`.

- [ ] **Step 3: Write the minimal implementation**

```rust
/// [`derive_slots`] plus the document's players, which take the slots AFTER
/// every track's (Plan V). Same purity contract: nothing is stored, every
/// rebuild derives fresh, so there is no allocation state to alias.
///
/// Appending rather than interleaving is what makes adding a player a
/// non-event for every existing slot: a fader gesture that resolved a slot
/// before the player was added still resolves the same strip after.
pub fn derive_slots_with_players(
    tracks: &[TrackState],
    players: &[crate::audio::player::Player],
) -> HashMap<TrackId, usize> {
    let mut out = derive_slots(tracks);
    let base = mixer_slot_count(tracks);
    for (i, p) in players.iter().enumerate() {
        out.insert(TrackId::from(p.id.as_str()), base + i);
    }
    out
}

/// [`mixer_slot_count`] plus one slot per player. Sized by COUNT, not by
/// the slot map's length, for the same reason `mixer_slot_count` is: a
/// duplicate id would drop a slot and shift every later row.
pub fn mixer_slot_count_with_players(
    tracks: &[TrackState],
    players: &[crate::audio::player::Player],
) -> usize {
    mixer_slot_count(tracks) + players.len()
}

/// The send-amount lanes over tracks AND players. A raw player contributes
/// none: `MixNode::from(&Player)` emits no sends for it (V-6), so a lane
/// here would be one nothing reads.
pub fn derive_send_slots_with_players(
    tracks: &[TrackState],
    players: &[crate::audio::player::Player],
) -> HashMap<String, usize> {
    let mut out = derive_send_slots(tracks);
    let mut n = send_slot_count(tracks);
    for p in players.iter().filter(|p| p.chain_applies()) {
        for s in &p.node.sends {
            out.insert(s.id.clone(), n);
            n += 1;
        }
    }
    out
}

/// Number of send-amount lanes for `tracks` + `players`, counting every
/// row (duplicate ids included) exactly as [`send_slot_count`] does.
pub fn send_slot_count_with_players(
    tracks: &[TrackState],
    players: &[crate::audio::player::Player],
) -> usize {
    send_slot_count(tracks)
        + players
            .iter()
            .filter(|p| p.chain_applies())
            .map(|p| p.node.sends.len())
            .sum::<usize>()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib audio::types 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/types.rs
git commit -m "feat(players): mixer slots for players, appended after tracks"
```

---

