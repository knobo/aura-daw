## Task 12: Migrating launch bindings to players

**Files:**
- Create: `src-tauri/tests/player_migration.rs`
- Modify: `src-tauri/src/midi/launch.rs` (`LaunchTarget::Player`, `migrate_clip_targets_to_players`)
- Modify: `src-tauri/src/audio/project.rs` (run the migration on open)

**Interfaces:**
- Consumes: `Player` (Task 1), `Store::players` (Task 2).
- Produces: `LaunchTarget::Player { player_id: PlayerId }`; `migrate_clip_targets_to_players(maps: &mut [LaunchMap], midi_clips: &[MidiClip], players: &mut Vec<Player>) -> usize` returning how many bindings were migrated.

- [ ] **Step 1: Write the failing test**

Create `src-tauri/tests/player_migration.rs`:

```rust
//! The V2 migration gate: a project saved with launch bindings opens with
//! players, and the same pads fire the same material.
//!
//! This is the test that says the overlay's retirement cost nobody their
//! work. Written as an integration test against `ControlPlane`'s public
//! surface, like every other file in this directory.

// ... the fixture boilerplate this directory already uses (see
// `pure_readers.rs`'s header for why fixtures here are local) ...

#[test]
fn a_project_with_clip_bindings_opens_with_players_that_fire_the_same_clip() {
    let (cp, dir) = control_plane_with_a_saved_project_containing_a_clip_binding();
    cp.open_project_epoch(&dir).unwrap();

    let players = cp.players();
    assert_eq!(players.len(), 1, "the binding became one player");
    assert_eq!(
        players[0].source,
        PlayerSource::MidiClip {
            clip_id: ClipId::from("mc1"),
            instrument_id: Some("plugin:i1".into()),
        },
        "the player plays what the binding played, through the clip's own instrument"
    );

    let binding = cp.launch_snapshot().maps[0].bindings[0].clone();
    assert_eq!(
        binding.target,
        LaunchTarget::Player { player_id: players[0].id.clone() },
        "and the same pad now points at it"
    );
    assert_eq!(binding.note, 36, "the note it was learned on is untouched");
}

#[test]
fn region_bindings_are_left_alone_by_the_migration() {
    let (cp, dir) = control_plane_with_a_saved_project_containing_a_region_binding();
    cp.open_project_epoch(&dir).unwrap();
    assert!(cp.players().is_empty(), "a scene is not a player");
    assert!(matches!(
        cp.launch_snapshot().maps[0].bindings[0].target,
        LaunchTarget::Region { .. }
    ));
}

#[test]
fn two_bindings_on_the_same_clip_share_one_player() {
    let (cp, dir) = control_plane_with_two_bindings_on_one_clip();
    cp.open_project_epoch(&dir).unwrap();
    let players = cp.players();
    assert_eq!(players.len(), 1, "one clip, one player");
    let maps = cp.launch_snapshot().maps;
    assert_eq!(maps[0].bindings[0].target, maps[0].bindings[1].target);
}

#[test]
fn a_binding_whose_clip_is_gone_migrates_to_nothing_and_does_not_fail_the_open() {
    let (cp, dir) = control_plane_with_a_binding_pointing_at_a_deleted_clip();
    cp.open_project_epoch(&dir).expect("the project still opens");
    assert!(cp.players().is_empty());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd src-tauri && cargo test --test player_migration 2>&1 | tail -20`
Expected: FAIL — `no variant Player` on `LaunchTarget`.

- [ ] **Step 3: Write the minimal implementation**

Add the variant to `LaunchTarget`:

```rust
    /// Plan V: the binding fires a player. This is what a `Clip` target
    /// becomes on open — see [`migrate_clip_targets_to_players`]. `Clip`
    /// is KEPT as a variant so a project saved before the migration still
    /// deserializes; nothing produces one any more.
    Player {
        #[serde(alias = "player_id")]
        player_id: crate::ids::PlayerId,
    },
```

and the migration:

```rust
/// Turn every `LaunchTarget::Clip` into a player (Plan V, V2's migration
/// gate). Idempotent: a binding already pointing at a player is left alone,
/// so opening a migrated project twice does not mint a second player.
///
/// Bindings that name the SAME clip share ONE player — a drum kit's two
/// pads on one clip is one instrument, and V-9 (the recording target is the
/// instrument, not the pad) will need exactly that identity later.
///
/// A binding whose clip is gone migrates to nothing and is dropped. The
/// alternative — failing the open — loses the user's whole project over a
/// dangling reference the old code merely logged.
pub fn migrate_clip_targets_to_players(
    maps: &mut [LaunchMap],
    midi_clips: &[crate::midi::MidiClip],
    players: &mut Vec<crate::audio::player::Player>,
) -> usize {
    use crate::audio::player::{Player, PlayerSource};

    let mut by_clip: HashMap<String, crate::ids::PlayerId> = players
        .iter()
        .filter_map(|p| match &p.source {
            PlayerSource::MidiClip { clip_id, .. } => Some((clip_id.to_string(), p.id.clone())),
            _ => None,
        })
        .collect();
    let mut migrated = 0usize;

    for map in maps.iter_mut() {
        let mut keep = Vec::with_capacity(map.bindings.len());
        for mut b in std::mem::take(&mut map.bindings) {
            let LaunchTarget::Clip { clip_id } = &b.target else {
                keep.push(b);
                continue;
            };
            let Some(clip) = midi_clips.iter().find(|c| c.id.as_str() == clip_id) else {
                log::warn!("launch: binding {} names a clip that is gone; dropping it", b.id);
                continue;
            };
            let player_id = by_clip.entry(clip_id.clone()).or_insert_with(|| {
                let mut p = Player::new(crate::ids::PlayerId::mint(), b.name.clone());
                p.source = PlayerSource::MidiClip {
                    clip_id: clip.id.clone(),
                    instrument_id: instrument_of_track(&clip.track_id),
                };
                let id = p.id.clone();
                players.push(p);
                id
            });
            b.target = LaunchTarget::Player { player_id: player_id.clone() };
            migrated += 1;
            keep.push(b);
        }
        map.bindings = keep;
    }
    migrated
}
```

`instrument_of_track` reads the source track's `instrument_id` — the
instrument the clip sounded through before the migration, so the pad keeps
sounding the same. Pass the tracks in rather than reaching for a global.

Call it from `audio/project.rs::open_project`, after the store's players
and the midi store's launch maps are both in place, and log the count.

Route `launch_fire_from` for a `Player` target straight to `player_fire`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd src-tauri && cargo test --test player_migration 2>&1 | tail -20`
Expected: PASS, 4 tests.

Run: `cd src-tauri && cargo test --test v3_migration 2>&1 | tail -10`
Expected: PASS — `schemaVersion` has not moved.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/midi/launch.rs src-tauri/src/audio/project.rs src-tauri/tests/player_migration.rs
git commit -m "feat(players): migrate clip launch bindings to players"
```

---

