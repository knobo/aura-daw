## Task 2: `Store::players` and `project.json` persistence

**Files:**
- Modify: `src-tauri/src/audio/types.rs:339-372` (`Project`, `Store`)
- Modify: `src-tauri/src/audio/project.rs:223-240` (`open_project`), `:373-397` (`from_store`)

**Interfaces:**
- Consumes: `Player` from Task 1.
- Produces: `Store::players: Vec<Player>`, `Project::players: Vec<Player>`. Both default to empty and serialize away when empty.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/audio/project.rs`'s `mod tests`:

```rust
/// The harmony rule (`midi/persist.rs:296`), applied to players: a project
/// that has never had one must resave byte-diff-free, so `schemaVersion`
/// never moves and nobody's stored project grows a key it does not use.
#[test]
fn a_project_without_players_writes_no_players_key() {
    let parent = tmp_parent("players-absent");
    let (project, _dir) = create(&parent, "NoPads", 48_000, 120.0).unwrap();
    let v = serde_json::to_value(&project).unwrap();
    assert!(
        v.get("players").is_none(),
        "an empty players list must not reach project.json: {v}"
    );
}

#[test]
fn players_round_trip_through_project_json() {
    use crate::audio::player::{Player, PlayerSource};
    use crate::ids::{ClipId, PlayerId};

    let parent = tmp_parent("players-roundtrip");
    let (mut project, dir) = create(&parent, "Pads", 48_000, 120.0).unwrap();
    let mut p = Player::new(PlayerId::from("p1"), "KICK");
    p.source = PlayerSource::AudioClip { clip_id: ClipId::from("c1") };
    p.raw = true;
    project.players.push(p.clone());
    save(&project, &dir).unwrap();

    let loaded = load(&dir).unwrap();
    assert_eq!(loaded.players, vec![p]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib audio::project::tests::players 2>&1 | tail -20`
Expected: FAIL — `no field players on type Project`.

- [ ] **Step 3: Write the minimal implementation**

In `src-tauri/src/audio/types.rs`, add to `Project` after the `clips` field:

```rust
    /// Plan V players (V-1). Additive and OPTIONAL, exactly like the
    /// Composer's `harmony` block: a project that has never had a player
    /// resaves byte-diff-free and `schemaVersion` does not move.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub players: Vec<crate::audio::player::Player>,
```

and to `Store`:

```rust
    /// Plan V players (V-1). NOT tracks (V-2) — a separate list precisely
    /// so no timeline code has to learn to skip them.
    pub players: Vec<crate::audio::player::Player>,
```

`Store` derives `Default`, so `players` needs nothing else there. Every
struct-literal construction of `Project` must gain `players: Vec::new()`;
`cargo build` enumerates them (`audio/project.rs:58`, `:382`, `:668`).

In `audio/project.rs::from_store`, add `players: store.players.clone(),`
beside `tracks:`. In `open_project` (line ~228), replace the store's list
the same way the tracks are replaced:

```rust
    store.players = project.players.clone();
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib audio::project 2>&1 | tail -20`
Expected: PASS, including the pre-existing round-trip tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/types.rs src-tauri/src/audio/project.rs
git commit -m "feat(players): persist players in project.json, additively"
```

---

