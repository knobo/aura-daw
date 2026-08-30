## Task 4: `MixNode` gains a `Player` producer

**Files:**
- Modify: `src-tauri/src/audio/node.rs` (`MixNodeKind`, `From<&Player>`, `mix_nodes_with_players`)

**Interfaces:**
- Consumes: `Player` (Task 1), `MixNode`/`MixNodeKind`/`mix_nodes` (already in the tree).
- Produces: `MixNodeKind::Player`; `impl From<&Player> for MixNode`; `pub fn mix_nodes_with_players(tracks: &[TrackState], players: &[Player]) -> Vec<MixNode>` — total and order-preserving over `tracks`, then over `players`, in that order.

- [ ] **Step 1: Write the failing test**

Add to `src-tauri/src/audio/node.rs`'s `mod tests`:

```rust
    use crate::audio::player::{Player, PlayerNode, PlayerSource};
    use crate::ids::{ClipId, PlayerId};

    fn test_player(id: &str) -> Player {
        Player::new(PlayerId::from(id), "PAD")
    }

    #[test]
    fn a_player_compiles_to_a_source_kind_node_carrying_its_own_id() {
        let mut p = test_player("p1");
        p.node.gain_db = -6.0;
        p.node.pan = 0.5;
        p.node.output = Some("bus1".into());
        let n = MixNode::from(&p);
        assert_eq!(n.id, TrackId::from("p1"));
        assert_eq!(n.kind, MixNodeKind::Player);
        assert_eq!(n.gain_db, -6.0);
        assert_eq!(n.pan, 0.5);
        assert_eq!(n.output, Some(TrackId::from("bus1")));
        assert!(n.takes_mixer_slot());
        assert!(!n.is_bus());
    }

    /// V-6, at the one place it can be enforced once for the whole engine:
    /// a raw player emits a node with nothing on it. Everything downstream
    /// — inserts, sends, PDC, the fader — then does the right thing without
    /// knowing the word "raw".
    #[test]
    fn a_raw_player_emits_a_bare_node_whatever_the_document_stores() {
        let mut p = test_player("p1");
        p.raw = true;
        p.node.gain_db = -6.0;
        p.node.pan = -1.0;
        p.node.muted = true;
        p.node.output = Some("bus1".into());
        p.node.inserts.push(InsertSlot {
            id: "i1".into(),
            instance_id: "x".into(),
            bypassed: false,
        });
        p.node.sends.push(SendSlot {
            id: "s1".into(),
            dest: "bus1".into(),
            amount_db: 0.0,
            pre_fader: false,
        });

        let n = MixNode::from(&p);
        assert_eq!(n.gain_db, 0.0, "unity");
        assert_eq!(n.pan, 0.0, "centre");
        assert!(n.inserts.is_empty(), "no chain");
        assert!(n.sends.is_empty(), "no sends");
        assert_eq!(n.output, None, "straight to master");
        assert!(!n.muted, "raw is not silenced by a stale mute");
    }

    #[test]
    fn mix_nodes_with_players_appends_players_after_every_track() {
        let tracks = vec![test_track("a"), {
            let mut t = test_track("b");
            t.kind = "bus".into();
            t
        }];
        let players = vec![test_player("p1"), test_player("p2")];
        let nodes = mix_nodes_with_players(&tracks, &players);

        assert_eq!(nodes.len(), 4);
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "p1", "p2"]);
        assert_eq!(
            nodes.iter().map(|n| n.kind).collect::<Vec<_>>(),
            vec![
                MixNodeKind::Source,
                MixNodeKind::Bus,
                MixNodeKind::Player,
                MixNodeKind::Player,
            ]
        );
    }

    /// The prefix property is what keeps every track's slot where it was:
    /// adding a player must never renumber a track.
    #[test]
    fn mix_nodes_with_players_has_mix_nodes_as_its_prefix() {
        let tracks = vec![test_track("a"), test_track("b"), test_track("c")];
        let plain = mix_nodes(&tracks);
        let with = mix_nodes_with_players(&tracks, &[test_player("p1")]);
        assert_eq!(with.len(), plain.len() + 1);
        for (i, n) in plain.iter().enumerate() {
            assert_eq!(with[i].id, n.id);
            assert_eq!(with[i].kind, n.kind);
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib audio::node 2>&1 | tail -20`
Expected: FAIL — `no variant Player`, `cannot find function mix_nodes_with_players`.

- [ ] **Step 3: Write the minimal implementation**

Add the variant to `MixNodeKind`, with a doc comment in the module's voice:

```rust
    /// A Plan V player: a strip with its own playhead and its own source,
    /// owned by no track (V-2). Takes a mixer slot and is not a bus, so
    /// every routing and insert decision treats it exactly like a source —
    /// which is the point. What separates it from `Source` downstream is
    /// only the clock its slot reads (`audio::clock`) and the fact that the
    /// offline bounce never compiles one (V-15).
    Player,
```

`takes_mixer_slot` already returns true for anything that is not
`Automation`; `is_bus` already returns false for anything that is not
`Bus`. Neither needs a change — add nothing.

```rust
impl From<&crate::audio::player::Player> for MixNode {
    /// V-6 lives HERE, once, rather than in every consumer: a raw player
    /// emits a node with no chain, no sends, unity gain, centre pan and a
    /// master output. `compile_inserts` and `compile_routing` then produce
    /// nothing for it without ever testing a `raw` flag, and the mixer's
    /// fader sees 0 dB because that is what the document compiled to.
    fn from(p: &crate::audio::player::Player) -> Self {
        if p.raw {
            return MixNode {
                id: TrackId::from(p.id.as_str()),
                kind: MixNodeKind::Player,
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                inserts: Vec::new(),
                sends: Vec::new(),
                output: None,
            };
        }
        MixNode {
            id: TrackId::from(p.id.as_str()),
            kind: MixNodeKind::Player,
            gain_db: p.node.gain_db,
            pan: p.node.pan,
            muted: p.node.muted,
            // A player is never soloed: solo is an arrangement gesture over
            // tracks, and a pad that goes silent because someone soloed a
            // track is the deck cutting out mid-performance.
            soloed: false,
            inserts: p.node.inserts.clone(),
            sends: p.node.sends.clone(),
            output: p.node.output.clone(),
        }
    }
}

/// [`mix_nodes`] plus the document's players, appended AFTER every track.
///
/// The order is load-bearing twice over. Downstream code zips this against
/// `Store::tracks` by position, so `mix_nodes(tracks)` must remain an exact
/// PREFIX of this — which is also what keeps a track's mixer slot where it
/// was when a player is added, so a fader knob written against the old
/// numbering never lands on a different strip.
pub fn mix_nodes_with_players(
    tracks: &[TrackState],
    players: &[crate::audio::player::Player],
) -> Vec<MixNode> {
    let mut out = mix_nodes(tracks);
    out.extend(players.iter().map(MixNode::from));
    out
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib audio::node 2>&1 | tail -20`
Expected: PASS, 9 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/node.rs
git commit -m "feat(players): Player as a third MixNode producer (V-3, V-6)"
```

---

