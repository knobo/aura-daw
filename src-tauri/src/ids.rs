//! Typed ProjectId families (round-2 §2, ADR 0001). One newtype per family
//! so a ClipId cannot be passed where a TrackId is expected. All families
//! serialize TRANSPARENTLY — the wire and every JSON schema see plain
//! strings — which is what lets OP_FORMAT_VERSION stay 1 (gated by the
//! exact-JSON tests in control/op.rs). Families are additive: TakeId,
//! PluginInstanceId, LineageId arrive with the rounds that use them.

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord,
                 serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Mint a fresh 128-bit UUID id. IDs are never reused (ADR 0001).
            pub fn mint() -> Self { Self(uuid::Uuid::new_v4().to_string()) }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl From<&str> for $name { fn from(s: &str) -> Self { Self(s.to_string()) } }
        impl From<String> for $name { fn from(s: String) -> Self { Self(s) } }
        impl PartialEq<&str> for $name { fn eq(&self, o: &&str) -> bool { self.0 == *o } }
        impl PartialEq<str> for $name { fn eq(&self, o: &str) -> bool { self.0 == o } }
        impl PartialEq<String> for $name { fn eq(&self, o: &String) -> bool { &self.0 == o } }
        impl std::borrow::Borrow<str> for $name { fn borrow(&self) -> &str { &self.0 } }
    };
}

string_id!(/// Mixer/arranger track. Family of `TrackState.id`.
    TrackId);
string_id!(/// A placement on the timeline (audio clip today, midi clip too;
    /// round-2 §5 makes every placement a ClipId).
    ClipId);
string_id!(/// A content object (round-2 §5). Declared now, first populated by
    /// the C/D content/placement split; Task 3's note ids are scoped to the
    /// clip that owns them until then.
    ContentId);
string_id!(/// An audio source asset (`audio/<sourceId>.wav`, round-2 §2.2).
    SourceId);
string_id!(/// A lane within a track (round-2 §5, ADR 0004). Every track
    /// gets one default lane at v3 migration/creation; placements
    /// reference a lane, never a track directly — the lane resolves to
    /// its track (`LaneId -> TrackId`). Multi-lane UI and takes stay
    /// deferred; only the indirection ships.
    LaneId);
string_id!(/// A player (Plan V): a mixer node with its own playhead, fired
    /// from a pad. Family of `audio::player::Player::id`. NOT a `TrackId`
    /// — ruling V-2 keeps players out of the timeline — though a player's
    /// compiled `MixNode::id` borrows the `TrackId` newtype, which is the
    /// mixer graph's node-identity family (see `audio::node`).
    PlayerId);

/// Fixed namespace for [`LaneId::default_for_track`]'s deterministic
/// minting — a project-specific constant, minted once and frozen forever
/// (same discipline as `audio::project`'s `AURA_SOURCE_NS`).
const AURA_LANE_NS: uuid::Uuid = uuid::uuid!("9d2e5c14-6b3a-4f8e-b7d1-3a5c9e0f2b44");

impl LaneId {
    /// The one default lane every track has (round-2 §5). Deterministic
    /// (UUIDv5 over the track id) so the SAME track always resolves to the
    /// SAME default lane id regardless of path — a v2 project migrated to
    /// v3, a fresh clip created live on an existing track, and a re-
    /// migration of an un-resaved v2 file must all agree. Multi-lane UI
    /// (minting additional, non-default lanes) is deferred; this is the
    /// only lane-minting path that exists today.
    pub fn default_for_track(track_id: &str) -> Self {
        Self(uuid::Uuid::new_v5(&AURA_LANE_NS, track_id.as_bytes()).to_string())
    }
}

/// Namespace for a player minted BY THE LAUNCH MIGRATION, so re-running it
/// on the same clip yields the same id.
const AURA_MIGRATED_PLAYER_NS: uuid::Uuid = uuid::uuid!("1f7b3c92-8e04-4a6d-9c15-2b8de6417a03");

impl PlayerId {
    /// The player the launch migration mints for a clip. Derived from the
    /// clip id rather than random, because the migration is **in-memory
    /// only** — it re-runs on every open until the project is saved, and a
    /// random id would be a different player each time.
    ///
    /// What that costs if it is random: a control-surface pad bound to a
    /// migrated player stores `player:<id>` in localStorage, which outlives
    /// the session. Close without saving, reopen, and the pad points at an
    /// id nothing has any more — a dead pad, silently. Deriving the id makes
    /// the migration idempotent in IDENTITY, not merely in count.
    pub fn for_migrated_clip(clip_id: &str) -> Self {
        Self(uuid::Uuid::new_v5(&AURA_MIGRATED_PLAYER_NS, clip_id.as_bytes()).to_string())
    }
}

/// Per-content sequential note id (round-2 §2.1). `0` is the wire sentinel
/// for "not yet assigned" — every note inside the document has id >= 1,
/// minted from its clip's persisted watermark. Values above `i32::MAX` must
/// never cross the CLAP boundary (ADR 0001).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct NoteId(pub u32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_transparently() {
        // ADR 0001 + the plan's version rule: typed families MUST look like
        // plain strings/numbers on the wire, or OP_FORMAT_VERSION bumps.
        let t = TrackId::from("t-1");
        assert_eq!(serde_json::to_string(&t).unwrap(), "\"t-1\"");
        let back: TrackId = serde_json::from_str("\"t-1\"").unwrap();
        assert_eq!(back, t);
        assert_eq!(serde_json::to_string(&NoteId(7)).unwrap(), "7");
        // Minted ids are 128-bit UUIDs, unique.
        assert_ne!(TrackId::mint(), TrackId::mint());
        // Borrow<str>: string lookups against typed-key maps.
        let mut m = std::collections::HashMap::new();
        m.insert(TrackId::from("a"), 1usize);
        assert_eq!(m.get("a"), Some(&1));
    }

    #[test]
    fn default_lane_is_deterministic_per_track_and_differs_across_tracks() {
        assert_eq!(LaneId::default_for_track("t1"), LaneId::default_for_track("t1"));
        assert_ne!(LaneId::default_for_track("t1"), LaneId::default_for_track("t2"));
    }
}
