# Plan V — V2: one player, real — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the engine N independent players — document objects that are
mixer nodes with their own playhead — so a pad can fire a WAV raw or a MIDI
clip through an instrument no track owns, and retire the single-shadow-playhead
launch overlay onto them.

**Architecture:** A **per-graph clock table** replaces the overlay's single
atomic set. Clock 0 is the transport (its `on` flag *is* the transport's play
state); every player owns one clock; every scene (`LaunchTarget::Region`) owns
one. A `slot_clock` lane maps each mixer slot to the clock it reads, written
atomically at fire time so a pad press never rebuilds the graph. Players
compile to `MixNode` (V1's indirection), so `compile_inserts`, `compile_routing`,
PDC, metering and the fader path do not learn that a new producer exists.

**Tech Stack:** Rust (`src-tauri/`), Tauri 2 commands, Svelte 5 renderer
(`src/lib/`), Vitest + `cargo test`.

**Spec:** [`docs/superpowers/specs/2026-08-26-plan-v-players-design.md`](../specs/2026-08-26-plan-v-players-design.md)
Track file: [`docs/backlog/plan-v-players.md`](../../backlog/plan-v-players.md)
Predecessor: [`docs/superpowers/plans/2026-08-27-plan-v1-mixnode.md`](2026-08-27-plan-v1-mixnode.md)

## Global Constraints

- **V-1** A player is a document object: it lives in the session, mutates through `Op`s, and is undoable.
- **V-2** A player is **not** a track. It never enters `store.tracks`, and the offline bounce never renders one.
- **V-3** Tracks, buses and players all compile to `MixNode`. New compiler code targets `MixNode`, never `TrackState`.
- **V-4** Every player has its own playhead. The overlay's single atomic set is **deleted**, not extended.
- **V-6** `raw: true` is absolute: no inserts, no sends, no envelopes, no macros, unity gain, centre pan, straight to master.
- **Frozen command/event names stay frozen** (`docs/STANDING-CONSTRAINTS.md` §Architecture). `launch_fire`, `launch_stop`, `launch_get`, `launch_set`, `launch_set_map`, `launch_set_drive`, `launch_set_drive_focus`, `launch_learn_arm`, `launch_learn_take` keep their names; bodies become wrappers. New commands are additive.
- **Thin renderer** (ADR 0006): no authoritative state, business logic or time math lands frontend-side.
- **`OP_FORMAT_VERSION` stays 2** and `schemaVersion` stays 3. New `Op` variants are additive; new `project.json` keys are written only when non-empty (the `harmony` pattern, `midi/persist.rs:296`).
- **RT contract:** no allocation, no locks, no syscalls on the audio callback. Every table is sized at graph-build time on the control thread, exactly as `ParamTable::with_slots_and_sends` is.
- **Performance gate** (`CLAUDE.md`): this plan touches `mixer.rs`, `rt.rs`, `engine.rs`, `offline.rs` and `midi/playback.rs`. `scripts/perf-check.sh --measure` on `origin/main` and `scripts/perf-check.sh --budget <that × 1.3>` on the branch, **in the same sitting**, both numbers quoted in the PR.
- **Out of scope** (later cuts): voice cap, choke groups, quantized start, velocity→gain (V3); per-node automation clock and modulation ports (V4); macros (V5); recording (V6); the pad inspector (V7).

## Rulings this plan adds

Numbered to continue the design doc's series, and to be copied into
`docs/backlog/plan-v-players.md` when the work lands.

| # | Ruling |
|---|---|
| **V-13** | **Clock 0 is the transport, and its `on` flag is the transport's play state.** "Only launched tracks render while stopped" (today's `LaunchPlayhead::exclusive`) stops being a special case: a node whose clock is off renders nothing, and when the transport is stopped clock 0 is off. |
| **V-14** | **A slot's clock binding is last-writer-wins, and released by compare-exchange.** Two scenes naming the same track is expressible now that scenes are no longer singular. Stopping scene A must not steal a track that scene B has since claimed, so release compares against the clock the releaser owns. |
| **V-15** | **Players do not exist in the offline bounce.** `offline::render_project` compiles `mix_nodes(&store.tracks)` — tracks only. A pad performance is not arrangement material; V-8 makes recording the way it becomes arrangement material. This is V-2's own argument made executable. |
| **V-16** | **A raw player plays the clip's source region at unity**: the source file's samples from `clip.offset` for `clip.len`, with no clip gain, no fades, no chain, centre pan, straight to master. This is what makes the owner's ear-check (bit-identical to browser audition) a real test rather than an approximate one. |

---

## File Structure

**New files**

| File | Responsibility |
|---|---|
| `src-tauri/src/audio/player.rs` | The `Player` document type and its parts (`PlayerSource`, `TriggerMode`, `PlayerNode`). Serde only — no engine, no RT. |
| `src-tauri/src/audio/clock.rs` | `ClockTable` / `ClockState`: the per-graph playhead set and the slot→clock lane. Pure and RT-safe; no knowledge of players, tracks or launch maps. |
| `src-tauri/tests/player_migration.rs` | Integration gate: a project saved with launch bindings opens with players and the same pads fire the same material. |
| `src/lib/state/players.svelte.ts` | Renderer-side mirror of `players[]` + the `player_fire`/`player_stop` calls. Chrome only. |

**Modified files**

| File | Change |
|---|---|
| `src-tauri/src/ids.rs` | `PlayerId` family. |
| `src-tauri/src/audio/types.rs` | `Store::players`, `Project::players`, `derive_slots_with_players`, `mixer_slot_count_with_players`, `send_slot_count_with_players`. |
| `src-tauri/src/audio/node.rs` | `MixNodeKind::Player`, `From<&Player> for MixNode`, `mix_nodes_with_players`. |
| `src-tauri/src/audio/rt.rs` | **Delete** `launch_on`/`launch_pos`/`launch_start`/`launch_end`/`launch_discont`/`launch_ended`, their methods, `LaunchPlayhead` and `FLAG_LAUNCH`. `RtGraph::clocks`, `RtTrack::clock_hint`. |
| `src-tauri/src/audio/mixer.rs` | `track_playhead` → `node_playhead(clocks, slot, base_pos, lp, disc)`; drop the `launch` parameter from the whole `render*` family. |
| `src-tauri/src/audio/engine.rs` | Build the clock table in `rebuild`; advance clocks per block; assemble player rows. |
| `src-tauri/src/audio/offline.rs` | V-15 guard + its regression test. |
| `src-tauri/src/audio/project.rs` | Carry `players` through `from_store` / `open_project`. |
| `src-tauri/src/control/op.rs` | `Op::PlayerAdd`, `Op::PlayerRemove`, `ObjectRef::Player`, `PropPath::Raw` / `PropPath::TriggerMode` / `PropPath::Source`. |
| `src-tauri/src/control/session.rs` | `apply_raw` arms for the three player ops. |
| `src-tauri/src/control/mod.rs` | `player_fire`, `player_stop`, `player_add`, `player_remove`, clock binding helpers; `apply_launch_audible`/`arm_drive_launch`/`clear_launch_audible` rewritten onto clocks. |
| `src-tauri/src/midi/launch.rs` | `LaunchTarget::Player`, scene clocks, `FireOrigin::Hardware` stops hijacking the transport, `migrate_clip_targets_to_players`. |
| `src-tauri/src/midi/playback.rs` | `node_for_player`: a live node keyed by player id, events scheduled with t=0 at the clip's start. |
| `src-tauri/src/lib.rs` | Register the additive commands. |
| `src/lib/utils/control-surface.ts` | `SurfaceTarget` gains `{ kind: "player"; playerId }`. |
| `src/lib/components/surface/SurfacePanel.svelte`, `Rack.svelte` | Bind a pad to a player; show source + raw on the pad. |

---

## Task 1: The `Player` document type

**Files:**
- Create: `src-tauri/src/audio/player.rs`
- Modify: `src-tauri/src/ids.rs:52` (after the `LaneId` declaration)
- Modify: `src-tauri/src/audio/mod.rs` (add `pub mod player;`)

**Interfaces:**
- Consumes: `crate::ids::{ClipId, ContentId, TrackId}`, `crate::audio::types::{InsertSlot, SendSlot}`.
- Produces: `crate::ids::PlayerId`; `crate::audio::player::{Player, PlayerSource, PlayerNode, TriggerMode}`. Every later task uses these exact names.

- [ ] **Step 1: Write the failing test**

Create `src-tauri/src/audio/player.rs` with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The wire form is the document form: camelCase, tagged source, and a
    /// default player is the smallest thing that can exist — `source: none`,
    /// not raw, one-shot, unity node straight to master.
    #[test]
    fn default_player_round_trips_as_camel_case_json() {
        let p = Player::new(PlayerId::from("p1"), "PAD 1");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["id"], "p1");
        assert_eq!(v["name"], "PAD 1");
        assert_eq!(v["source"]["kind"], "none");
        assert_eq!(v["raw"], false);
        assert_eq!(v["trigger"]["mode"], "oneShot");
        assert_eq!(v["node"]["gainDb"], 0.0);
        assert_eq!(v["node"]["pan"], 0.0);
        assert_eq!(v["node"]["output"], serde_json::Value::Null);
        let back: Player = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn audio_source_round_trips() {
        let mut p = Player::new(PlayerId::from("p1"), "KICK");
        p.source = PlayerSource::AudioClip { clip_id: ClipId::from("c1") };
        p.raw = true;
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["source"]["kind"], "audioClip");
        assert_eq!(v["source"]["clipId"], "c1");
        assert_eq!(serde_json::from_value::<Player>(v).unwrap(), p);
    }

    #[test]
    fn midi_source_round_trips() {
        let mut p = Player::new(PlayerId::from("p2"), "PAD");
        p.source = PlayerSource::MidiClip {
            clip_id: ClipId::from("mc1"),
            instrument_id: Some("plugin:i1".into()),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["source"]["kind"], "midiClip");
        assert_eq!(v["source"]["clipId"], "mc1");
        assert_eq!(v["source"]["instrumentId"], "plugin:i1");
        assert_eq!(serde_json::from_value::<Player>(v).unwrap(), p);
    }

    /// V-6 is a property of the DOCUMENT too, not only of what the graph
    /// emits: a raw player answers "does my chain apply" with no.
    #[test]
    fn raw_reports_no_chain_whatever_the_node_says() {
        let mut p = Player::new(PlayerId::from("p1"), "RAW");
        p.raw = true;
        p.node.gain_db = -6.0;
        p.node.inserts.push(InsertSlot {
            id: "i1".into(),
            instance_id: "x".into(),
            bypassed: false,
        });
        assert!(!p.chain_applies());
        p.raw = false;
        assert!(p.chain_applies());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd src-tauri && cargo test --lib audio::player 2>&1 | tail -20`
Expected: FAIL — `cannot find type Player in this scope`.

- [ ] **Step 3: Write the minimal implementation**

Add to `src-tauri/src/ids.rs` immediately after the `LaneId` `string_id!` block (line 52):

```rust
string_id!(/// A player (Plan V): a mixer node with its own playhead, fired
    /// from a pad. Family of `audio::player::Player::id`. NOT a `TrackId`
    /// — ruling V-2 keeps players out of the timeline — though a player's
    /// compiled `MixNode::id` borrows the `TrackId` newtype, which is the
    /// mixer graph's node-identity family (see `audio::node`).
    PlayerId);
```

Write the body of `src-tauri/src/audio/player.rs` above the test module:

```rust
//! `Player`: a pad that is an instrument (Plan V, ruling V-1).
//!
//! A player is a DOCUMENT object — it lives in `Store::players`, mutates
//! through `Op`s and is undoable — and it is NOT a track (V-2): no lanes,
//! no arm, no absolute placements, and no presence in the offline bounce
//! (V-15). What it shares with a track is the mixer strip, and only that:
//! `From<&Player> for MixNode` in `audio::node` is the whole of the
//! overlap, which is why nothing in `compile_inserts` / `compile_routing`
//! learns that players exist.
//!
//! This module is serde and small predicates. No engine, no RT, no locks.

use serde::{Deserialize, Serialize};

use crate::audio::types::{InsertSlot, SendSlot};
use crate::ids::{ClipId, PlayerId, TrackId};

/// What a player sounds when it is fired.
///
/// `AudioClip` and `MidiClip` both name a PLACEMENT (`ClipId`), not a
/// content object: the placement is what carries trim (`offset`, `len`) and
/// the source binding, and V-16 defines raw playback in exactly those
/// terms. Content-keyed sources are V4's business, once envelopes are
/// evaluated at the player's own position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PlayerSource {
    /// An audio clip. With `Player::raw` set this is V-16's bit-exact path.
    AudioClip { clip_id: ClipId },
    /// A MIDI clip plus the instrument that renders it. `instrument_id`
    /// follows `TrackState::instrument_id`'s vocabulary — a sampler-bank id
    /// or `plugin:<instanceId>` — and `None` means the player is silent
    /// until one is bound. The instance it names is owned by NO track:
    /// `PluginInstanceInfo::track_id` is already optional.
    MidiClip {
        clip_id: ClipId,
        #[serde(default)]
        instrument_id: Option<String>,
    },
    /// A pad that carries knobs and nothing else (R5). Renders silence.
    None,
}

impl Default for PlayerSource {
    fn default() -> Self {
        Self::None
    }
}

/// How a press behaves. V3 adds `quantize`, `chokeGroup` and
/// `velocityToGain` to this struct as `#[serde(default)]` fields, which is
/// additive and needs no format bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TriggerMode {
    /// The press is a trigger; the source plays to its end.
    #[default]
    OneShot,
    /// Sounds while held; release cuts it.
    Gate,
    /// Repeats from the start until stopped.
    Loop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Trigger {
    pub mode: TriggerMode,
}

/// The mixer strip a player owns — the same shape a track's compiles to,
/// which is the whole reuse argument (design §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerNode {
    pub gain_db: f64,
    pub pan: f64,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub inserts: Vec<InsertSlot>,
    #[serde(default)]
    pub sends: Vec<SendSlot>,
    /// `None` = master, exactly like `TrackState::output`.
    #[serde(default)]
    pub output: Option<TrackId>,
}

impl Default for PlayerNode {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            inserts: Vec::new(),
            sends: Vec::new(),
            output: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    #[serde(default)]
    pub source: PlayerSource,
    /// V-6: absolute. See [`Player::chain_applies`].
    #[serde(default)]
    pub raw: bool,
    #[serde(default)]
    pub trigger: Trigger,
    #[serde(default)]
    pub node: PlayerNode,
}

impl Player {
    pub fn new(id: PlayerId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            source: PlayerSource::None,
            raw: false,
            trigger: Trigger::default(),
            node: PlayerNode::default(),
        }
    }

    /// V-6: a raw player's chain does not apply, whatever the document
    /// stores. The fields are kept rather than cleared so unticking `raw`
    /// restores what the user had — the flag is the authority, not the
    /// absence of data.
    pub fn chain_applies(&self) -> bool {
        !self.raw
    }
}
```

Add `pub mod player;` to `src-tauri/src/audio/mod.rs` beside `pub mod node;`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib audio::player 2>&1 | tail -20`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/player.rs src-tauri/src/audio/mod.rs src-tauri/src/ids.rs
git commit -m "feat(players): the Player document type (V-1)"
```

---

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

## Task 6: The clock table

**Files:**
- Create: `src-tauri/src/audio/clock.rs`
- Modify: `src-tauri/src/audio/mod.rs` (add `pub mod clock;`)

**Interfaces:**
- Consumes: `crate::audio::transport::LoopSpec`.
- Produces:
  - `pub const TRANSPORT_CLOCK: u32 = 0;`
  - `ClockTable::with_slots_and_clocks(n_slots: usize, n_clocks: usize) -> ClockTable`
  - `ClockTable::playhead(&self, slot: usize, base_pos: u64, lp: &LoopSpec, disc: bool) -> Playhead`
  - `Playhead { pos: u64, looping: bool, discontinuity: bool, on: bool, is_transport: bool }`
  - `ClockTable::fire(&self, clock: u32, start: u64, end: u64, looping: bool)`
  - `ClockTable::stop(&self, clock: u32)`
  - `ClockTable::bind_slot(&self, slot: usize, clock: u32)`
  - `ClockTable::release_slot_if(&self, slot: usize, clock: u32) -> bool`
  - `ClockTable::set_transport_playing(&self, playing: bool)`
  - `ClockTable::advance(&self, frames: u64)`
  - `ClockTable::any_running(&self) -> bool`
  - `ClockTable::clock_of(&self, slot: usize) -> u32`

- [ ] **Step 1: Write the failing test**

Create `src-tauri/src/audio/clock.rs` containing only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::transport::LoopSpec;

    fn table() -> ClockTable {
        ClockTable::with_slots_and_clocks(4, 3)
    }

    #[test]
    fn a_slot_defaults_to_the_transport_clock() {
        let t = table();
        t.set_transport_playing(true);
        let ph = t.playhead(0, 5_000, &LoopSpec::OFF, false);
        assert_eq!(t.clock_of(0), TRANSPORT_CLOCK);
        assert_eq!(ph.pos, 5_000);
        assert!(ph.on);
        assert!(ph.is_transport);
    }

    /// V-13: clock 0's `on` flag IS the transport's play state, so "only
    /// launched nodes render while stopped" needs no separate concept.
    #[test]
    fn a_stopped_transport_silences_transport_slots_but_not_a_running_clock() {
        let t = table();
        t.set_transport_playing(false);
        t.fire(1, 0, 1_000, false);
        t.bind_slot(2, 1);

        assert!(!t.playhead(0, 5_000, &LoopSpec::OFF, false).on);
        let ph = t.playhead(2, 5_000, &LoopSpec::OFF, false);
        assert!(ph.on);
        assert_eq!(ph.pos, 0);
        assert!(!ph.is_transport);
    }

    #[test]
    fn a_fired_clock_starts_at_its_start_and_reports_a_discontinuity_once() {
        let t = table();
        t.fire(1, 400, 900, false);
        t.bind_slot(1, 1);
        let first = t.playhead(1, 0, &LoopSpec::OFF, false);
        assert_eq!(first.pos, 400);
        assert!(first.discontinuity, "the press is a jump");
        let second = t.playhead(1, 0, &LoopSpec::OFF, false);
        assert!(!second.discontinuity, "consumed exactly once");
    }

    #[test]
    fn advance_moves_running_clocks_and_stops_one_at_its_end() {
        let t = table();
        t.fire(1, 0, 100, false);
        t.bind_slot(1, 1);
        t.advance(64);
        assert_eq!(t.playhead(1, 0, &LoopSpec::OFF, false).pos, 64);
        t.advance(64);
        assert!(!t.playhead(1, 0, &LoopSpec::OFF, false).on, "past its end");
        assert!(!t.any_running());
    }

    #[test]
    fn a_looping_clock_wraps_to_its_start_instead_of_ending() {
        let t = table();
        t.fire(1, 0, 100, true);
        t.bind_slot(1, 1);
        t.advance(96);
        t.advance(32);
        let ph = t.playhead(1, 0, &LoopSpec::OFF, false);
        assert!(ph.on, "a loop does not end");
        assert_eq!(ph.pos, 28, "wrapped: 128 - 100");
        assert!(ph.discontinuity, "the wrap is a jump the live node must hear");
    }

    #[test]
    fn advance_never_moves_the_transport_clock() {
        let t = table();
        t.set_transport_playing(true);
        t.advance(64);
        assert_eq!(
            t.playhead(0, 5_000, &LoopSpec::OFF, false).pos,
            5_000,
            "the callback owns the transport position, not this table"
        );
    }

    /// V-14: two scenes may name the same track now that scenes are not
    /// singular, so a release must not steal a slot someone else has claimed.
    #[test]
    fn release_only_frees_a_slot_still_bound_to_the_releasing_clock() {
        let t = table();
        t.bind_slot(3, 1);
        t.bind_slot(3, 2); // a second scene claims it
        assert!(!t.release_slot_if(3, 1), "clock 1 no longer owns it");
        assert_eq!(t.clock_of(3), 2);
        assert!(t.release_slot_if(3, 2));
        assert_eq!(t.clock_of(3), TRANSPORT_CLOCK);
    }

    #[test]
    fn out_of_range_indices_are_dropped_not_panics() {
        let t = table();
        t.fire(99, 0, 10, false);
        t.bind_slot(99, 1);
        t.stop(99);
        assert!(!t.release_slot_if(99, 1));
        let ph = t.playhead(99, 1_234, &LoopSpec::OFF, false);
        assert!(!ph.on, "a slot outside the table renders nothing");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test --lib audio::clock 2>&1 | tail -20`
Expected: FAIL — `cannot find type ClockTable`.

- [ ] **Step 3: Write the minimal implementation**

Add `pub mod clock;` to `src-tauri/src/audio/mod.rs`, then write the body
above the test module:

```rust
//! `ClockTable`: N independent playheads, one per graph (Plan V, V-4).
//!
//! This replaces the launch overlay's single atomic set
//! (`launch_on`/`launch_pos`/`launch_start`/`launch_end` + `FLAG_LAUNCH`),
//! which could only ever express ONE sounding thing: two pads could not
//! sound at once and a retrigger rewound whatever was playing.
//!
//! Two vectors and nothing else:
//!
//! * `clocks` — the playheads. Index [`TRANSPORT_CLOCK`] is the transport:
//!   its position comes from the callback (`base_pos`), never from here,
//!   and its `on` flag is the transport's play state (V-13). Every other
//!   index is a player's or a scene's, and owns its own position.
//! * `slot_clock` — which clock each MIXER SLOT reads. A player's entry is
//!   written once at graph build; a scene's is written at FIRE time, which
//!   is why it is atomic: a pad press must never rebuild the graph.
//!
//! Sized per-graph and built on the control thread, exactly like
//! `ParamTable` (`rt.rs`, round-2 §2.4) — a retired graph keeps reading the
//! table it was built with, so a renumbering cannot bleed into it. Nothing
//! here allocates, locks or blocks after construction.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

use crate::audio::transport::LoopSpec;

/// The transport's clock. Its position is the callback's `base_pos`; its
/// `on` flag is the transport's play state (V-13).
pub const TRANSPORT_CLOCK: u32 = 0;

/// What one node reads for one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Playhead {
    pub pos: u64,
    /// The clock's own loop is a start/end pair, not a `LoopSpec`; the
    /// arrangement's `LoopSpec` applies only to the transport clock.
    pub looping: bool,
    /// This block does not continue the previous one — a live node owes an
    /// `all_notes_off` before it processes.
    pub discontinuity: bool,
    /// False = render nothing at all for this node this block.
    pub on: bool,
    /// True for [`TRANSPORT_CLOCK`], so the caller knows the arrangement's
    /// `LoopSpec` and automation ramps apply.
    pub is_transport: bool,
}

struct ClockState {
    on: AtomicBool,
    pos: AtomicU64,
    start: AtomicU64,
    end: AtomicU64,
    looping: AtomicBool,
    /// Set by `fire` and by a loop wrap; consumed by the first `playhead`
    /// that reads it, exactly as the overlay's `launch_discont` was.
    discont: AtomicBool,
}

impl ClockState {
    fn idle() -> Self {
        Self {
            on: AtomicBool::new(false),
            pos: AtomicU64::new(0),
            start: AtomicU64::new(0),
            end: AtomicU64::new(0),
            looping: AtomicBool::new(false),
            discont: AtomicBool::new(false),
        }
    }
}

pub struct ClockTable {
    clocks: Vec<ClockState>,
    slot_clock: Vec<AtomicU32>,
}

impl Default for ClockTable {
    /// One transport clock and no slots — what an empty or headless graph
    /// needs, and what every test that does not care about clocks gets.
    fn default() -> Self {
        Self::with_slots_and_clocks(0, 1)
    }
}

impl ClockTable {
    /// A table for `n_slots` mixer slots and `n_clocks` clocks (at least
    /// one, which is the transport's). Every slot starts on the transport.
    pub fn with_slots_and_clocks(n_slots: usize, n_clocks: usize) -> Self {
        Self {
            clocks: (0..n_clocks.max(1)).map(|_| ClockState::idle()).collect(),
            slot_clock: (0..n_slots).map(|_| AtomicU32::new(TRANSPORT_CLOCK)).collect(),
        }
    }

    pub fn clocks(&self) -> usize {
        self.clocks.len()
    }

    pub fn slots(&self) -> usize {
        self.slot_clock.len()
    }

    #[inline]
    pub fn clock_of(&self, slot: usize) -> u32 {
        self.slot_clock.get(slot).map_or(TRANSPORT_CLOCK, |c| c.load(Relaxed))
    }

    /// V-13. Called by the control plane on play/stop.
    pub fn set_transport_playing(&self, playing: bool) {
        if let Some(c) = self.clocks.first() {
            c.on.store(playing, Relaxed);
        }
    }

    /// Start a clock at `start`, running to `end`. Retrigger rewinds THIS
    /// clock and nothing else — which is the whole difference from the
    /// overlay it replaces.
    pub fn fire(&self, clock: u32, start: u64, end: u64, looping: bool) {
        let Some(c) = self.clocks.get(clock as usize) else { return };
        if clock == TRANSPORT_CLOCK {
            return; // the transport is driven by the callback, not fired
        }
        c.start.store(start, Relaxed);
        c.end.store(end.max(start.saturating_add(1)), Relaxed);
        c.pos.store(start, Relaxed);
        c.looping.store(looping, Relaxed);
        c.discont.store(true, Relaxed);
        c.on.store(true, Relaxed);
    }

    pub fn stop(&self, clock: u32) {
        let Some(c) = self.clocks.get(clock as usize) else { return };
        if clock == TRANSPORT_CLOCK {
            return;
        }
        c.on.store(false, Relaxed);
    }

    pub fn is_on(&self, clock: u32) -> bool {
        self.clocks.get(clock as usize).is_some_and(|c| c.on.load(Relaxed))
    }

    /// Any non-transport clock running: what tells the output callback to
    /// render a graph even though the transport is stopped.
    pub fn any_running(&self) -> bool {
        self.clocks.iter().skip(1).any(|c| c.on.load(Relaxed))
    }

    /// Point a mixer slot at a clock. Last writer wins (V-14).
    pub fn bind_slot(&self, slot: usize, clock: u32) {
        if clock as usize >= self.clocks.len() {
            return;
        }
        if let Some(c) = self.slot_clock.get(slot) {
            c.store(clock, Relaxed);
        }
    }

    /// Return a slot to the transport, but ONLY if it still reads `clock`
    /// (V-14): a scene that ends must not steal back a track another scene
    /// has since claimed. Returns whether the release happened.
    pub fn release_slot_if(&self, slot: usize, clock: u32) -> bool {
        let Some(c) = self.slot_clock.get(slot) else { return false };
        c.compare_exchange(clock, TRANSPORT_CLOCK, Relaxed, Relaxed).is_ok()
    }

    /// What `slot` reads this block. `base_pos` and `lp` are the
    /// transport's, used only when the slot is on [`TRANSPORT_CLOCK`].
    #[inline]
    pub fn playhead(&self, slot: usize, base_pos: u64, _lp: &LoopSpec, disc: bool) -> Playhead {
        if slot >= self.slot_clock.len() {
            return Playhead {
                pos: base_pos,
                looping: false,
                discontinuity: disc,
                on: false,
                is_transport: true,
            };
        }
        let idx = self.clock_of(slot);
        let Some(c) = self.clocks.get(idx as usize) else {
            return Playhead {
                pos: base_pos,
                looping: false,
                discontinuity: disc,
                on: false,
                is_transport: true,
            };
        };
        let on = c.on.load(Relaxed);
        if idx == TRANSPORT_CLOCK {
            return Playhead {
                pos: base_pos,
                looping: false,
                discontinuity: disc,
                on,
                is_transport: true,
            };
        }
        Playhead {
            pos: c.pos.load(Relaxed),
            looping: c.looping.load(Relaxed),
            discontinuity: c.discont.swap(false, Relaxed),
            on,
            is_transport: false,
        }
    }

    /// Advance every running non-transport clock by `frames`. The transport
    /// is advanced by the callback that owns `SharedRt::position`, never
    /// here — two writers on one playhead is the bug this whole table
    /// exists to make impossible.
    ///
    /// Called once per output block, after the render.
    pub fn advance(&self, frames: u64) {
        for c in self.clocks.iter().skip(1) {
            if !c.on.load(Relaxed) {
                continue;
            }
            let start = c.start.load(Relaxed);
            let end = c.end.load(Relaxed);
            let next = c.pos.load(Relaxed).saturating_add(frames);
            if next < end {
                c.pos.store(next, Relaxed);
                continue;
            }
            if c.looping.load(Relaxed) {
                let span = end.saturating_sub(start).max(1);
                c.pos.store(start + (next - start) % span, Relaxed);
                c.discont.store(true, Relaxed);
            } else {
                c.pos.store(end, Relaxed);
                c.on.store(false, Relaxed);
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test --lib audio::clock 2>&1 | tail -20`
Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/clock.rs src-tauri/src/audio/mod.rs
git commit -m "feat(players): the per-graph clock table (V-4, V-13, V-14)"
```

---

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

## Task 13: The renderer — a pad is a player

**Files:**
- Create: `src/lib/state/players.svelte.ts`
- Modify: `src/lib/utils/control-surface.ts:120-130` (`SurfaceTarget`), `:250-280` (`targetKey`, the bind list)
- Modify: `src/lib/components/surface/SurfacePanel.svelte`, `src/lib/components/surface/Rack.svelte`
- Test: `src/lib/utils/control-surface.test.ts`, `src/lib/components/surface/surface-panel.dom.test.ts`

**Interfaces:**
- Consumes: the `player_fire` / `player_stop` / `players_get` / `player_add` commands (Task 9).
- Produces: `SurfaceTarget` variant `{ kind: "player"; playerId: string }`; `players.svelte.ts` exporting `players` (a `$state` list), `refreshPlayers()`, `firePlayer(id)`, `stopPlayer(id)`, `addAudioPlayer(clipId, raw)`.

- [ ] **Step 1: Write the failing test**

Add to `src/lib/utils/control-surface.test.ts`:

```ts
describe("player targets", () => {
  it("gives a player target a stable key distinct from a clip's", () => {
    expect(targetKey({ kind: "player", playerId: "p1" })).toBe("player:p1");
    expect(targetKey({ kind: "player", playerId: "p1" })).not.toBe(
      targetKey({ kind: "clipLaunch", clipId: "p1" }),
    );
  });

  it("accepts a player target on a pad and on a pad grid cell", () => {
    const layout = addWidget(blankLayout(), "pad");
    const pad = layout.pages[0].widgets[0];
    const bound = bindWidget(layout, pad.id, { kind: "player", playerId: "p1" });
    expect(bound.pages[0].widgets[0].target).toEqual({ kind: "player", playerId: "p1" });
  });
});
```

Add to `src/lib/components/surface/surface-panel.dom.test.ts`:

```ts
it("shows a player pad's source and raw flag", async () => {
  // Read the existing DOM-test fixtures in this file before writing this;
  // mount the panel the same way the neighbouring tests do.
  const { getByTestId } = renderPanelWithPlayerPad({
    id: "p1",
    name: "KICK",
    source: { kind: "audioClip", clipId: "c1" },
    raw: true,
  });
  expect(getByTestId("pad-p1-source").textContent).toContain("KICK");
  expect(getByTestId("pad-p1-raw")).toBeTruthy();
});

it("fires the player on pointerdown and stops it on pointerup in gate mode", async () => {
  const calls: string[] = [];
  const { pad } = renderPanelWithPlayerPad({ id: "p1", trigger: { mode: "gate" } }, calls);
  await fireEvent.pointerDown(pad);
  await fireEvent.pointerUp(pad);
  expect(calls).toEqual(["player_fire:p1", "player_stop:p1"]);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npm test -- control-surface surface-panel 2>&1 | tail -20`
Expected: FAIL.

- [ ] **Step 3: Write the minimal implementation**

In `control-surface.ts`, add the variant and its key:

```ts
  | { kind: "player"; playerId: string }
```

```ts
    case "player":
      return `player:${target.playerId}`;
```

`players.svelte.ts` is a thin mirror — invoke and hold, no time math and no
authoritative state (ADR 0006):

```ts
import { invoke } from "$lib/tauri";

export type PlayerSource =
  | { kind: "audioClip"; clipId: string }
  | { kind: "midiClip"; clipId: string; instrumentId: string | null }
  | { kind: "none" };

export interface Player {
  id: string;
  name: string;
  source: PlayerSource;
  raw: boolean;
  trigger: { mode: "oneShot" | "gate" | "loop" };
}

export const players = $state<{ list: Player[] }>({ list: [] });

export async function refreshPlayers(): Promise<void> {
  players.list = await invoke<Player[]>("players_get");
}

export const firePlayer = (id: string) => invoke("player_fire", { id });
export const stopPlayer = (id: string) => invoke("player_stop", { id });

/// Make a pad out of an audio clip. `raw` is V-6: the file at unity,
/// bypassing the source track entirely.
export async function addAudioPlayer(clipId: string, raw: boolean): Promise<string> {
  const id = await invoke<string>("player_add", {
    source: { kind: "audioClip", clipId },
    raw,
  });
  await refreshPlayers();
  return id;
}
```

In `SurfacePanel.svelte`'s bind menu, add an `Add pad from clip ›` entry
that drills into the project's audio clips (following the `Add rack ›`
drill-down PR #116 landed, for the reason it landed: a flat list runs into
the bottom of the window), with a `raw` checkbox. In `Rack.svelte`, a pad
whose target is a player renders the player's name and a `RAW` marker.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npm test 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib
git commit -m "feat(surface): a pad can be a player"
```

---

## Task 14: The performance gate, run properly

**Files:**
- Modify: `src-tauri/tests/plugin_load_profile.rs` (an idle-players case)

**Interfaces:** none — this task produces numbers, not names.

- [ ] **Step 1: Add the idle-player case to the harness**

Read `plugin_load_profile.rs` first and follow its existing case shape. The
new case is the question the owner actually asked: what do configured but
silent pads cost?

```rust
/// Plan V's cost question, made measurable: 32 players configured, 8
/// sounding. A pad that is not sounding must cost one atomic load per
/// block and nothing else — if idle players show up in this number, the
/// early-out in the strip is not where it should be.
#[test]
fn thirty_two_idle_players_and_eight_sounding() {
    // ... build the same session the existing cases build, plus players ...
}
```

- [ ] **Step 2: Measure the baseline on `origin/main`**

```bash
git stash
git checkout origin/main
scripts/perf-check.sh --measure    # record: BASELINE µs
git checkout -
git stash pop
```

- [ ] **Step 3: Measure the branch, in the same sitting**

```bash
scripts/perf-check.sh --budget $(( BASELINE * 13 / 10 ))
```

Expected: exit 0. If it fails, the fix is not a bigger budget — bisect the
tasks with `scripts/perf-check.sh --harness-from main` per
`docs/STANDING-CONSTRAINTS.md` §Performance.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/tests/plugin_load_profile.rs
git commit -m "perf: harness case for idle players under load"
```

---

## Task 15: Documentation and releasing the claim

**Files:**
- Modify: `docs/backlog/plan-v-players.md` (V2 → landed; rulings V-13…V-16)
- Modify: `docs/LANDED.md`
- Modify: `docs/TRAPS.md` (only if something cost an hour)
- Modify: `next-prompt.md` (delete the claim row; V3 becomes the next unclaimed item)

- [ ] **Step 1: Record the outcome**

Move V2's row in the backlog file's status table to `landed — PR #121`,
copy rulings V-13…V-16 into it, and write the owner's ear-check under a
heading of its own: *open SURFACE, put a WAV on a pad with `raw` ticked,
start the arrangement, hit the pad. The pad must sound bit-identical to
auditioning that file in the browser, and the arrangement's playhead must
not move.*

- [ ] **Step 2: Release the claim**

Delete the `Plan V — V2` row from `next-prompt.md`'s *Active claims* table,
leaving `| _(none)_ | | | |` if it is the last one, and update the "Next up"
list: V2 is done, **V3 — polyphony** is the next cut.

- [ ] **Step 3: Verify before claiming completion**

Run all four, and quote the output in the PR:

```bash
cd src-tauri && cargo test --lib 2>&1 | tail -5
cd src-tauri && cargo test --tests -- --test-threads=1 2>&1 | tail -5
npm test 2>&1 | tail -5
scripts/perf-check.sh --budget <BASELINE × 1.3>
```

- [ ] **Step 4: Commit and take the PR out of draft**

```bash
git add docs next-prompt.md
git commit -m "docs: Plan V V2 landed; release the claim"
git push
gh pr ready 121
```

---

## Self-review

**Spec coverage.** R1 (a pad fires an audio clip) — Task 9. R2 (raw) —
Tasks 4, 9, and V-16. R3 (an instrument no track owns) — Task 10. R4 (mix
and match) — falls out of Tasks 9-10, since players are independent nodes;
the *polyphony guarantees* (voice cap, choke) are V3 by the spec's own
cut table. R5 (the deck's own knobs) is V5 and R6 (recording) is V6 — both
explicitly out of scope above. V2's four gate lines map to: Task 9's
`firing_an_audio_player_sounds_without_touching_the_transport`, Task 10's
live-node tests, Task 7's grep gate, Task 12's migration file, and Task 3's
undo test.

**Design §8's open questions.** Question 5 (does a raw player ignore the
deck's output stage) is answered by V-16 and Task 4: a raw player's own node
is unity/centre/master, and any bus it feeds is an ordinary mixer node.
Questions 1, 2 and 4 size V3 and V6 and stay open. Question 3 (are players
visible in the mixer) is V7's, and nothing here forecloses it.

**Known gaps, stated rather than hidden.** Two fixtures this plan names —
`cp.render_one_block_for_tests()` and `cp.source_samples_for_tests()` in
Task 9 — do not exist yet; that task's implementer builds them alongside the
test, following `control/mod.rs`'s existing `for_tests` helpers. Task 7's
mixer tests use placeholder fixture names (`graph_with_one_clip_track`) that
must be replaced with the real ones in that module.
