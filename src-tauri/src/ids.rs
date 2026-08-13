//! Typed ProjectId families (round-2 §2, ADR 0001). One newtype per family
//! so a ClipId cannot be passed where a TrackId is expected. All families
//! serialize TRANSPARENTLY — the wire and every JSON schema see plain
//! strings — which is what lets OP_FORMAT_VERSION stay 1 (gated by the
//! exact-JSON tests in control/op.rs). Families are additive: TakeId,
//! LaneId, PluginInstanceId, LineageId arrive with the rounds that use them.

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
}
