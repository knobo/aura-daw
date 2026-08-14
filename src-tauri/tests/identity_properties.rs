//! Gate B (round-2 §7 test 2 + §2.4 obligations): identity survives
//! deletion; inbound references survive undo; generations do not alias.
//!
//! The slot-aliasing test (round-2 O-13) lives IN-CRATE instead of here:
//! `EngineHandle::for_tests` (src/audio/engine.rs) is `#[cfg(test)]`-gated,
//! which makes it reachable only when the crate compiles its own test
//! target, not when pulled in as a library dependency of an integration-test
//! binary under `tests/` (this file). Widening its visibility (a `Cargo.toml`
//! feature) was ruled out by the controller for this task — see
//! `src/control/mod.rs`'s `tests` module,
//! `old_graph_never_sees_the_new_tracks_params`, for that test.
//!
//! Harness style copied from `tests/channel_properties.rs`'s local-helper
//! convention: this file defines its OWN `test_track`/`test_clip`/
//! `user_meta`/`snapshot`, independent of the crate-private `#[cfg(test)]`
//! ones in `session.rs` / `control/mod.rs`.

use std::collections::HashSet;

use proptest::prelude::*;

use aura_lib::audio::types::{Clip, Store, TrackState};
use aura_lib::control::op::{Actor, Op, TxMeta};
use aura_lib::control::Session;
use aura_lib::ids::{ClipId, ContentId, LaneId, SourceId, TrackId};
use aura_lib::midi::MidiStore;

// ---------------------------------------------------------------------------
// Local test helpers (independent of session.rs's / control/mod.rs's
// private #[cfg(test)] ones — same convention as channel_properties.rs).
// ---------------------------------------------------------------------------

fn test_track(id: &TrackId) -> TrackState {
    TrackState {
        id: id.clone(),
        name: format!("Track {id}"),
        kind: "audio".into(),
        gain_db: 0.0,
        pan: 0.0,
        muted: false,
        soloed: false,
        armed: false,
        color: "#7c9cff".into(),
        instrument_id: None,
    }
}

fn test_clip(id: &ClipId, track_id: &TrackId) -> Clip {
    Clip {
        id: id.clone(),
        track_id: track_id.clone(),
        name: format!("Clip {id}"),
        source_path: "audio/x.wav".into(),
        // The empty/default SourceId is the documented "unassigned" sentinel
        // (audio/types.rs's `Clip::source_id` doc) — fine here since these
        // clips never reach the engine's decode cache.
        source_id: SourceId::default(),
        source_channels: 2,
        source_sample_rate: 48_000,
        source_length_samples: 48_000,
        timeline_start_samples: 0,
        offset_samples: 0,
        length_samples: 48_000,
        gain_db: 0.0,
        fade_in_samples: 0,
        fade_out_samples: 0,
        content_id: ContentId::mint(),
        lane_id: LaneId::default_for_track(track_id.as_str()),
    }
}

fn user_meta(label: &str) -> TxMeta {
    TxMeta { actor: Actor::User, run: "identity-prop-run".into(), label: label.into() }
}

/// Byte-identical snapshot of session STATE, canonical JSON — the same
/// oracle style as Gate A's `channel_properties.rs::snapshot`, restricted to
/// `(tracks, clips, midi.clips)`. `Store` has no `slots` field (round-2
/// §2.4: slots are per-graph-generation state derived fresh by the engine,
/// never part of the document), so there is nothing RT-side left to
/// snapshot here.
fn snapshot(m: &parking_lot::Mutex<Session>) -> String {
    #[derive(serde::Serialize)]
    struct Snap<'a> {
        tracks: &'a Vec<TrackState>,
        clips: &'a Vec<Clip>,
        midi_clips: &'a Vec<aura_lib::midi::MidiClip>,
    }
    let g = m.lock();
    let snap = Snap { tracks: &g.store.tracks, clips: &g.store.clips, midi_clips: &g.midi.clips };
    serde_json::to_string(&snap).expect("snapshot fields are all plain-data Serialize")
}

/// Deterministic pseudo-random INDEX selection from a `u64` seed — no `rand`
/// dependency (SplitMix64; public-domain-status generator, test-only index
/// selection, not cryptographic, modulo bias accepted at these tiny `n`).
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        // Avoid an all-zero state degenerating to a fixed low-entropy stream
        // on seed = 0 — XOR in a nonzero constant first.
        Self(seed ^ 0x9E3779B97F4A7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A pseudo-random index in `0..n` (returns 0 for `n == 0`).
    fn next_range(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

// ---------------------------------------------------------------------------
// Step 1: delete-then-undo preserves identity and inbound references
// (Tasks 1-4 machinery only).
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Round-2 §2.3: "delete then undo preserves identity and every inbound
    /// reference." N tracks (2..6), clips scattered and INTERLEAVED across
    /// them (0..4 each — a round-robin build, not grouped by track, so the
    /// resulting `store.clips` vec genuinely mixes ownership), a random
    /// subset of tracks removed one transaction at a time (deterministic
    /// selection from `seed`), undone in reverse via fresh transactions. The
    /// store must come back byte-identical: same ids (never re-minted), same
    /// clip->track references, same VEC ORDER (Task 1's positional clip
    /// restore).
    #[test]
    fn delete_then_undo_preserves_identity_and_references(
        seed in any::<u64>(), n_tracks in 2usize..6, removals in 1usize..4,
    ) {
        let mut rng = SplitMix64::new(seed);

        let track_ids: Vec<TrackId> =
            (0..n_tracks).map(|i| TrackId::from(format!("track-{i}").as_str())).collect();

        let mut store = Store::default();
        for id in &track_ids {
            store.tracks.push(test_track(id));
        }

        // Build 0..4 clips per track, then INTERLEAVE them into one vec via
        // round-robin pop (not simple concatenation) so store.clips mixes
        // track ownership — this is what actually exercises Task 1's
        // positional restore rather than a same-track-contiguous shortcut.
        let mut clip_counter: u32 = 0;
        let mut per_track_clips: Vec<Vec<Clip>> = track_ids
            .iter()
            .map(|tid| {
                let n_clips = rng.next_range(4); // 0..=3
                (0..n_clips)
                    .map(|_| {
                        let cid = ClipId::from(format!("clip-{clip_counter}").as_str());
                        clip_counter += 1;
                        test_clip(&cid, tid)
                    })
                    .collect()
            })
            .collect();
        let mut clips = Vec::new();
        loop {
            let mut pushed_any = false;
            for bucket in per_track_clips.iter_mut() {
                if let Some(c) = bucket.pop() {
                    clips.push(c);
                    pushed_any = true;
                }
            }
            if !pushed_any {
                break;
            }
        }
        store.clips = clips;

        let session = parking_lot::Mutex::new(Session::new(store, MidiStore::default()));
        let before_snapshot = snapshot(&session);
        let (before_track_ids, before_clip_ids): (HashSet<TrackId>, HashSet<ClipId>) = {
            let g = session.lock();
            (
                g.store.tracks.iter().map(|t| t.id.clone()).collect(),
                g.store.clips.iter().map(|c| c.id.clone()).collect(),
            )
        };

        // Remove a random subset of tracks, one transaction at a time.
        let mut alive = track_ids.clone();
        let mut undo_stack: Vec<Vec<Op>> = Vec::new();
        for _ in 0..removals {
            if alive.is_empty() {
                break;
            }
            let idx = rng.next_range(alive.len());
            let id = alive.remove(idx);
            // `track`/`index`/`clips`/`clip_indices` on the op payload are
            // advisory only for TrackRemove — `apply_raw` finds the row by
            // `track.id` and collects the real clips + positions from store
            // truth (see control/session.rs's TrackRemove arm).
            let committed = Session::transact(&session, user_meta("remove"), |tx| {
                tx.apply(Op::TrackRemove {
                    track: test_track(&id), index: 0, clips: vec![], clip_indices: vec![],
                })
            })
            .expect("removing a currently-alive track must not fail");
            undo_stack.push(committed.inverses);
        }

        // Undo in reverse: each transaction's own inverses are already
        // "ready to apply" (Committed.inverses doc comment), so undo the
        // LAST removal's inverses first.
        for inv in undo_stack.into_iter().rev() {
            Session::transact(&session, user_meta("undo"), |tx| {
                for op in inv {
                    tx.apply(op)?;
                }
                Ok(())
            })
            .expect("undo of a just-applied removal must not fail");
        }

        prop_assert_eq!(
            snapshot(&session), before_snapshot,
            "delete-then-undo must be byte-identical (round-2 §2.3)"
        );

        // Explicit identity half, named per the never-reuse contract (ADR
        // 0001) — equality of the serialized snapshot already implies this,
        // but the brief asks for it stated directly.
        let (after_track_ids, after_clip_ids): (HashSet<TrackId>, HashSet<ClipId>) = {
            let g = session.lock();
            (
                g.store.tracks.iter().map(|t| t.id.clone()).collect(),
                g.store.clips.iter().map(|c| c.id.clone()).collect(),
            )
        };
        prop_assert_eq!(
            after_track_ids, before_track_ids,
            "track ids must never be re-minted across delete-then-undo (ADR 0001: ids are never reused)"
        );
        prop_assert_eq!(
            after_clip_ids, before_clip_ids,
            "clip ids must never be re-minted across delete-then-undo (ADR 0001: ids are never reused)"
        );
    }
}

// ---------------------------------------------------------------------------
// Step 3: meter blocks carry the graph generation, end to end through a real
// `derive_slots` before/after pair (integration-level companion to Task 6's
// hand-built-maps unit test, `meters.rs::blocks_fold_under_the_slot_map_of_their_own_generation`).
// ---------------------------------------------------------------------------

/// Unlike Task 6's own unit test (which hand-builds `HashMap<TrackId, usize>`
/// slot maps), this test derives both generations' slot maps from real
/// `TrackState` lists via `derive_slots`, proving the derivation and the
/// fold agree end to end.
#[test]
fn meter_blocks_carry_generation_end_to_end_through_derive_slots() {
    use aura_lib::audio::meters::{GenerationMaps, MeterAccum, RawMeterBlock};
    use aura_lib::audio::types::derive_slots;

    let track_a = TrackId::from("track-a");
    let track_b = TrackId::from("track-b");
    let track_c = TrackId::from("track-c");

    // gen 1: display order a, b, c -> slots 0, 1, 2.
    let before = vec![test_track(&track_a), test_track(&track_b), test_track(&track_c)];
    let gen1_slots = derive_slots(&before);

    // gen 2 (after removing "a"): display order b, c -> slots 0, 1. The SAME
    // numeric slot 0 now means a different track — the exact shape Task 6
    // exists to keep the meter fold correct across.
    let after = vec![test_track(&track_b), test_track(&track_c)];
    let gen2_slots = derive_slots(&after);

    let mut maps = GenerationMaps::default();
    maps.publish(1, &gen1_slots);
    maps.publish(2, &gen2_slots);

    let mut acc = MeterAccum::default();

    // `RawMeterBlock::new(generation, position, frames)` +
    // `set_slot_local(lane, peak_l, peak_r, ss_l, ss_r)` with an implicit
    // `base_slot == 0` (a single-chunk block — both graphs here have <64
    // tracks).
    let mut b1 = RawMeterBlock::new(1, 0, 100);
    b1.set_slot_local(0, 0.5, 0.5, 1.0, 1.0); // slot 0 under gen 1 == "a"
    acc.fold(&b1, &maps);

    let mut b2 = RawMeterBlock::new(2, 100, 100);
    b2.set_slot_local(0, 0.9, 0.9, 2.0, 2.0); // slot 0 under gen 2 == "b"
    acc.fold(&b2, &maps);

    let order = [track_a.clone(), track_b.clone(), track_c.clone()];
    let frame = acc.take_frame(0, &order, 0);

    assert!(
        (frame.tracks[0].peak_l - 0.5).abs() < 1e-6,
        "\"a\" must keep its gen-1 level (0.5), not \"b\"'s gen-2 level at the same numeric slot"
    );
    assert!(
        (frame.tracks[1].peak_l - 0.9).abs() < 1e-6,
        "\"b\" must show its OWN gen-2 level (0.9), not \"a\"'s gen-1 level from the same slot 0"
    );
}
