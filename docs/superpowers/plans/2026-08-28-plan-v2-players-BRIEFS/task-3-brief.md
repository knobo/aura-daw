## Task 3: Ops — `PlayerAdd`, `PlayerRemove`, `ObjectRef::Player`

**Files:**
- Modify: `src-tauri/src/control/op.rs:378-389` (`ObjectRef`), `:395-410` (`PropPath`), and the `Op` enum
- Modify: `src-tauri/src/control/session.rs:697+` (`apply_raw`)

**Interfaces:**
- Consumes: `Player` (Task 1), `Store::players` (Task 2).
- Produces: `Op::PlayerAdd { player, index }`, `Op::PlayerRemove { player, index }`, `ObjectRef::Player(PlayerId)`, `PropPath::Raw`, `PropPath::TriggerMode`, `PropPath::PlayerSource`. `apply_raw` returns the exact inverse for each.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/control/session.rs`'s `#[cfg(test)] mod tests`:

```rust
    /// Structural ops carry their own payload so the inverse restores the
    /// row byte-identically — the rule every landed structural op follows
    /// (`TrackAdd`'s doc comment).
    #[test]
    fn player_add_then_undo_restores_byte_identically() {
        use crate::audio::player::{Player, PlayerSource};
        use crate::ids::{ClipId, PlayerId};

        let mut session = test_session();
        let mut p = Player::new(PlayerId::from("p1"), "KICK");
        p.source = PlayerSource::AudioClip { clip_id: ClipId::from("c1") };
        p.raw = true;
        let before = serde_json::to_string(&session.store.players).unwrap();

        let mut effect = EngineEffect::default();
        let inverse = apply_raw(
            &mut session,
            &Op::PlayerAdd { player: p.clone(), index: 0 },
            &mut effect,
        )
        .unwrap();
        assert_eq!(session.store.players, vec![p.clone()]);
        assert!(effect.rebuild, "a new node changes the graph");

        apply_raw(&mut session, &inverse, &mut EngineEffect::default()).unwrap();
        assert_eq!(serde_json::to_string(&session.store.players).unwrap(), before);
    }

    /// `PlayerRemove`'s payload is advisory beyond the id — store truth
    /// wins, mirroring `TrackRemove`.
    #[test]
    fn player_remove_takes_its_payload_from_the_store_not_the_caller() {
        use crate::audio::player::Player;
        use crate::ids::PlayerId;

        let mut session = test_session();
        let real = Player::new(PlayerId::from("p1"), "REAL NAME");
        session.store.players.push(real.clone());

        let mut lie = real.clone();
        lie.name = "WRONG".into();
        let inverse = apply_raw(
            &mut session,
            &Op::PlayerRemove { player: lie, index: 0 },
            &mut EngineEffect::default(),
        )
        .unwrap();

        match inverse {
            Op::PlayerAdd { player, index } => {
                assert_eq!(player, real, "the inverse restores store truth");
                assert_eq!(index, 0);
            }
            other => panic!("expected PlayerAdd, got {other:?}"),
        }
    }

    #[test]
    fn set_on_a_player_writes_raw_and_inverts() {
        use crate::audio::player::Player;
        use crate::ids::PlayerId;

        let mut session = test_session();
        session.store.players.push(Player::new(PlayerId::from("p1"), "PAD"));

        let inverse = apply_raw(
            &mut session,
            &Op::Set {
                object: ObjectRef::Player(PlayerId::from("p1")),
                path: PropPath::Raw,
                from: serde_json::json!(false),
                to: serde_json::json!(true),
            },
            &mut EngineEffect::default(),
        )
        .unwrap();
        assert!(session.store.players[0].raw);

        apply_raw(&mut session, &inverse, &mut EngineEffect::default()).unwrap();
        assert!(!session.store.players[0].raw);
    }

    #[test]
    fn set_on_an_unknown_player_is_an_error_not_a_silent_noop() {
        let mut session = test_session();
        let err = apply_raw(
            &mut session,
            &Op::Set {
                object: ObjectRef::Player(crate::ids::PlayerId::from("ghost")),
                path: PropPath::Raw,
                from: serde_json::json!(false),
                to: serde_json::json!(true),
            },
            &mut EngineEffect::default(),
        )
        .unwrap_err();
        assert!(err.contains("unknown player"), "got: {err}");
    }
```

If `test_session()` does not exist in that module, use whatever fixture the
neighbouring `apply_raw` tests use — read the module's existing helpers
before writing, and reuse them rather than adding a second fixture.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib control::session::tests::player 2>&1 | tail -20`
Expected: FAIL — `no variant PlayerAdd`.

- [ ] **Step 3: Write the minimal implementation**

In `control/op.rs`, add to `ObjectRef`:

```rust
    /// A Plan V player (V-1). Its own family, not `Track`: a player is not
    /// a track (V-2), and `ObjectRef` is what disambiguates which document
    /// list a `PropPath` addresses.
    Player(crate::ids::PlayerId),
```

Add to `PropPath`:

```rust
    /// Player: V-6's absolute raw flag (wire: JSON bool).
    Raw,
    /// Player: `"oneShot" | "gate" | "loop"` (wire: JSON string).
    TriggerMode,
    /// Player: the whole tagged `PlayerSource` object. One path rather than
    /// three, because changing a pad's source changes `kind` and its fields
    /// together — two ops could leave a `midiClip` source carrying an
    /// `audioClip`'s id between them.
    PlayerSource,
```

Add to `Op`:

```rust
    /// Structural: create a player (payload = the full row, so the inverse
    /// is `PlayerRemove`). Plan V, ruling V-1.
    PlayerAdd { player: crate::audio::player::Player, index: usize },
    /// Structural: remove a player. `player` is advisory beyond `player.id`
    /// and `index` is advisory — store truth wins, mirroring `TrackRemove`.
    PlayerRemove { player: crate::audio::player::Player, index: usize },
```

In `control/session.rs::apply_raw`, add three arms. Follow the file's
established shape: validate before mutating, read `from` out of store truth
rather than trusting the caller, and set `effect.rebuild` where the graph
changes.

```rust
        Op::PlayerAdd { player, index } => {
            if session.store.players.iter().any(|p| p.id == player.id) {
                return Err(format!("player exists: {}", player.id));
            }
            let at = (*index).min(session.store.players.len());
            session.store.players.insert(at, player.clone());
            effect.rebuild = true;
            Ok(Op::PlayerRemove { player: player.clone(), index: at })
        }
        Op::PlayerRemove { player, .. } => {
            let at = session
                .store
                .players
                .iter()
                .position(|p| p.id == player.id)
                .ok_or_else(|| format!("unknown player: {}", player.id))?;
            let removed = session.store.players.remove(at);
            effect.rebuild = true;
            Ok(Op::PlayerAdd { player: removed, index: at })
        }
        Op::Set { object: ObjectRef::Player(id), path, to, .. } => {
            let p = session
                .store
                .players
                .iter_mut()
                .find(|p| &p.id == id)
                .ok_or_else(|| format!("unknown player: {id}"))?;
            let from_now = read_player_prop(p, *path)?; // truth, not caller's `from`
            let applied = write_player_prop(p, *path, to)?;
            // Source, raw and the node's shape all change what the graph
            // compiles; trigger mode does not.
            effect.rebuild = !matches!(path, PropPath::TriggerMode);
            Ok(Op::Set {
                object: ObjectRef::Player(id.clone()),
                path: *path,
                from: applied,
                to: from_now,
            })
        }
```

with two helpers beside the existing `read_prop`/`write_prop`:

```rust
fn read_player_prop(p: &Player, path: PropPath) -> Result<serde_json::Value, String> {
    Ok(match path {
        PropPath::Raw => serde_json::json!(p.raw),
        PropPath::TriggerMode => serde_json::to_value(p.trigger.mode).unwrap(),
        PropPath::PlayerSource => serde_json::to_value(&p.source).unwrap(),
        PropPath::Name => serde_json::json!(p.name),
        PropPath::Gain => serde_json::json!(p.node.gain_db),
        PropPath::Pan => serde_json::json!(p.node.pan),
        PropPath::Muted => serde_json::json!(p.node.muted),
        other => return Err(format!("player has no property {other:?}")),
    })
}

/// Returns the value ACTUALLY written (post-clamp, post-trim), so the
/// inverse observes what the document holds — the rule `write_prop`'s
/// `LengthTicks` clamp established.
fn write_player_prop(
    p: &mut Player,
    path: PropPath,
    to: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match path {
        PropPath::Raw => {
            p.raw = to.as_bool().ok_or("raw must be a bool")?;
            Ok(serde_json::json!(p.raw))
        }
        PropPath::TriggerMode => {
            p.trigger.mode = serde_json::from_value(to.clone())
                .map_err(|e| format!("triggerMode: {e}"))?;
            Ok(serde_json::to_value(p.trigger.mode).unwrap())
        }
        PropPath::PlayerSource => {
            p.source = serde_json::from_value(to.clone())
                .map_err(|e| format!("source: {e}"))?;
            Ok(serde_json::to_value(&p.source).unwrap())
        }
        PropPath::Name => {
            let n = to.as_str().ok_or("name must be a string")?.trim().to_string();
            if n.is_empty() {
                return Err("name must not be empty".into());
            }
            p.name = n;
            Ok(serde_json::json!(p.name))
        }
        PropPath::Gain => {
            p.node.gain_db = to.as_f64().ok_or("gainDb must be a number")?.clamp(-160.0, 12.0);
            Ok(serde_json::json!(p.node.gain_db))
        }
        PropPath::Pan => {
            p.node.pan = to.as_f64().ok_or("pan must be a number")?.clamp(-1.0, 1.0);
            Ok(serde_json::json!(p.node.pan))
        }
        PropPath::Muted => {
            p.node.muted = to.as_bool().ok_or("muted must be a bool")?;
            Ok(serde_json::json!(p.node.muted))
        }
        other => Err(format!("player has no property {other:?}")),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib control::session 2>&1 | tail -20`
Expected: PASS. Fix any non-exhaustive `match` the new `Op`/`ObjectRef`/
`PropPath` variants break — `cargo build` names each one.

- [ ] **Step 5: Verify the journal replay reader still accepts the format**

Run: `cd src-tauri && cargo test --test journal_replay 2>&1 | tail -10`
Expected: PASS. `OP_FORMAT_VERSION` stays 2 — new variants are additive.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/control/op.rs src-tauri/src/control/session.rs
git commit -m "feat(players): PlayerAdd/PlayerRemove ops and ObjectRef::Player"
```

---

