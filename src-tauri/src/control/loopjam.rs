//! AI loop-jam ("evolve") backend — wave 1B.
//!
//! The idea: the transport loop region is a jam cell. With a loop enabled,
//! `evolve` renders the region's existing clip audio on a target track,
//! submits an ACE-Step **repaint** job over that render, and — when the job
//! finishes — hot-swaps the region's clips for the regenerated audio at the
//! NEXT loop wrap ("next-ready-wrap": playback is never blocked; if a job
//! isn't done by wrap N it applies at wrap N+1). While the transport is
//! stopped there is no wrap, so a ready result applies immediately (nothing
//! is audible, nothing can glitch).
//!
//! State machine (one in-flight evolve max, cancellable at any point):
//!
//! ```text
//!   idle ──evolve──▶ generating ──job done──▶ ready ──loop wrap──▶ applied
//!     ▲                  │  │                   │                     │
//!     └──── cancel ──────┘  └─ job error ──▶ idle (lastError)        │
//!     ▲                                                              │
//!     └────────────────────── (applied allows the next evolve) ◀─────┘
//! ```
//!
//! Every transition is announced as a `loopjam://state` app event carrying
//! [`LoopJamStatus`] (same emitter pattern as `transport://state`).
//!
//! The swap itself mirrors the RT discipline everywhere else in AURA: the
//! result is decoded, copied into the project and pyramid-built OFF the
//! supervisor thread while the old loop keeps playing; the apply is a store
//! mutation + `ControlMsg::Rebuild` (RCU graph swap) — no locks on the RT
//! path, no audible stall.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::audio::dsp::linear_resample;
use crate::audio::mixer;
use crate::audio::rt::{ParamTable, RtClip, RtClipData, RtGraph, RtTrack};
use crate::audio::transport::LoopSpec;
use crate::audio::types::{Clip, Store};
use crate::audio::waveform::{pyramid_exists, Pyramid};
use crate::sidecars::jobs::{self, EventSink, JobSpec};
use crate::sidecars::{JobKind, SidecarEvent};

use super::import::{decode_audio, write_f32_wav};
use super::op::{Op, ObjectRef, PropPath, TxMeta};
use super::{session, ControlPlane};

/// How often the wrap-watcher samples the playhead. 5 ms ≈ 240 samples at
/// 48 kHz — comfortably inside one UI frame, far coarser than a buffer.
const WATCH_INTERVAL: Duration = Duration::from_millis(5);

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Options for one evolve cycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvolveOptions {
    /// Target track; None = first audio track with clip audio in the region.
    pub track_id: Option<String>,
    /// Style prompt forwarded to the repaint worker (optional).
    pub prompt: Option<String>,
    /// Generation seed; None = derived from the generation counter.
    pub seed: Option<u64>,
    /// Force/deny simulate mode; None = follow `AURA_SIDECAR_SIMULATE`.
    pub simulate: Option<bool>,
}

/// Snapshot of the loop-jam state machine (the `loopjam://state` payload).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopJamStatus {
    /// "idle" | "generating" | "ready" | "applied"
    pub state: String,
    pub job_id: Option<String>,
    pub track_id: Option<String>,
    pub loop_start_samples: u64,
    pub loop_end_samples: u64,
    /// Completed hot-swaps since construction.
    pub generation: u64,
    pub last_error: Option<String>,
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Generating,
    Ready,
    Applied,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Generating => "generating",
            Phase::Ready => "ready",
            Phase::Applied => "applied",
        }
    }
    /// Can a new evolve start from here? (one in-flight max)
    fn accepts_evolve(self) -> bool {
        matches!(self, Phase::Idle | Phase::Applied)
    }
}

/// A finished generation waiting for the next loop wrap: the clip is fully
/// prepared (file in `audio/`, pyramid built) — applying is ONE commit
/// (`Op::ClipRemove`/`Set`/`ClipAdd`, Plan E Task 8). `Clone`: `apply` reads
/// this out of `Inner` and may need to retry it at the NEXT wrap if the
/// commit races a concurrent store change (see `apply`'s doc).
#[derive(Clone)]
struct PendingSwap {
    clip: Clip,
    track_id: String,
    region: (u64, u64),
}

struct Inner {
    phase: Phase,
    /// Bumped on cancel/new evolve; stale worker threads observe and exit.
    epoch: u64,
    job_id: Option<String>,
    track_id: Option<String>,
    region: (u64, u64),
    pending: Option<PendingSwap>,
    generation: u64,
    last_error: Option<String>,
}

/// The loop-jam service. lib.rs constructs one over the shared control plane
/// and manages it (`app.manage(Arc::new(LoopJam::new(control)))`).
pub struct LoopJam {
    control: Arc<ControlPlane>,
    inner: Mutex<Inner>,
}

impl LoopJam {
    pub fn new(control: Arc<ControlPlane>) -> Self {
        Self {
            control,
            inner: Mutex::new(Inner {
                phase: Phase::Idle,
                epoch: 0,
                job_id: None,
                track_id: None,
                region: (0, 0),
                pending: None,
                generation: 0,
                last_error: None,
            }),
        }
    }

    pub fn status(&self) -> LoopJamStatus {
        let i = self.inner.lock();
        LoopJamStatus {
            state: i.phase.as_str().to_string(),
            job_id: i.job_id.clone(),
            track_id: i.track_id.clone(),
            loop_start_samples: i.region.0,
            loop_end_samples: i.region.1,
            generation: i.generation,
            last_error: i.last_error.clone(),
        }
    }

    fn emit_state(&self) {
        let status = self.status();
        (self.control.emit)(
            "loopjam://state",
            serde_json::to_value(&status).unwrap_or_default(),
        );
    }

    // -- evolve -----------------------------------------------------------

    /// Start one evolve cycle: render the loop region on the target track,
    /// submit the ACE-Step repaint job, arm the next-wrap hot-swap. Must be
    /// called on a tokio runtime (job submission spawns a supervisor task).
    pub fn evolve(self: &Arc<Self>, opts: EvolveOptions) -> Result<LoopJamStatus, String> {
        let simulate = opts
            .simulate
            .unwrap_or_else(|| matches!(
                std::env::var("AURA_SIDECAR_SIMULATE").as_deref(),
                Ok("1") | Ok("true") | Ok("yes")
            ));
        self.evolve_with(opts, |params| repaint_spec(params, simulate))
    }

    /// Spec-injectable core (tests drive it with fake `/bin/sh` sidecars).
    pub(crate) fn evolve_with(
        self: &Arc<Self>,
        opts: EvolveOptions,
        make_spec: impl FnOnce(&serde_json::Value) -> Result<JobSpec, String>,
    ) -> Result<LoopJamStatus, String> {
        // Gate on the state machine FIRST (one in-flight max).
        {
            let i = self.inner.lock();
            if !i.phase.accepts_evolve() {
                return Err(format!(
                    "an evolve is already in flight ({}); cancel it first",
                    i.phase.as_str()
                ));
            }
        }
        // The loop region comes from the RT atomics — the engine truth.
        let lp = self.control.shared.loop_spec();
        if !lp.active() {
            return Err(
                "transport loop region is not enabled — set a loop (transport_set_loop) \
                 before evolving"
                    .into(),
            );
        }
        let engine_rate = self.control.shared.sample_rate.load(Relaxed);

        // Resolve target track + the region's clips under the store lock;
        // decode/render after it is dropped.
        let (project_dir, track_id, clip_sources) = {
            let session = self.control.session.lock();
            let s = &session.store;
            let dir = s
                .project_dir
                .clone()
                .ok_or("no project open — loop-jam needs a project (results are stored in it)")?;
            let track_id = match &opts.track_id {
                Some(id) => {
                    let t = s
                        .tracks
                        .iter()
                        .find(|t| &t.id == id)
                        .ok_or_else(|| format!("unknown track: {id}"))?;
                    if t.kind != "audio" {
                        return Err(format!(
                            "track {id} is a {} track — loop-jam evolves audio clips",
                            t.kind
                        ));
                    }
                    crate::ids::TrackId::from(id.clone())
                }
                None => s
                    .tracks
                    .iter()
                    .filter(|t| t.kind == "audio")
                    .find(|t| {
                        s.clips
                            .iter()
                            .any(|c| c.track_id == t.id && clip_overlaps(c, lp.start, lp.end))
                    })
                    .map(|t| t.id.clone())
                    .ok_or("no audio track has clips inside the loop region")?,
            };
            let sources: Vec<(Clip, PathBuf)> = s
                .clips
                .iter()
                .filter(|c| c.track_id == track_id && clip_overlaps(c, lp.start, lp.end))
                .filter_map(|c| s.abs_path(&c.source_path).map(|p| (c.clone(), p)))
                .collect();
            if sources.is_empty() {
                return Err(format!(
                    "loop region [{}, {}) contains no audio clips on track {track_id}",
                    lp.start, lp.end
                ));
            }
            (dir, track_id, sources)
        };

        // Render the region through the REAL mixer path (gain/pan/fades) and
        // stage it as the repaint input.
        let region_audio = render_region_stereo(&clip_sources, lp.start, lp.end, engine_rate)?;
        let jam_dir = project_dir.join("cache").join("loopjam");
        std::fs::create_dir_all(&jam_dir).map_err(|e| e.to_string())?;
        let tag = uuid::Uuid::new_v4();
        let input_path = jam_dir.join(format!("{tag}-in.wav"));
        let output_path = jam_dir.join(format!("{tag}-out.wav"));
        write_f32_wav(&input_path, 2, engine_rate, &region_audio)?;

        let region_secs = (lp.end - lp.start) as f64 / engine_rate as f64;
        let (generation, epoch) = {
            let mut i = self.inner.lock();
            // Re-check under the lock (evolve could have raced).
            if !i.phase.accepts_evolve() {
                return Err("an evolve is already in flight; cancel it first".into());
            }
            i.epoch += 1;
            (i.generation, i.epoch)
        };
        let mut params = serde_json::json!({
            "inputPath": input_path,
            "startSeconds": 0.0,
            "endSeconds": region_secs,
            "outputPath": output_path,
            "seed": opts.seed.unwrap_or(0x5EED + generation),
        });
        if let Some(p) = &opts.prompt {
            params["prompt"] = serde_json::Value::String(p.clone());
        }
        let spec = make_spec(&params)?;

        // Job sink: Done arms the swap, Error resets to idle. Heavy work
        // (decode + pyramid) happens on a dedicated thread, never on the
        // job supervisor.
        let jam = Arc::clone(self);
        let fallback_out = output_path.clone();
        let region = (lp.start, lp.end);
        let track_for_sink = track_id.clone();
        let sink: EventSink = Arc::new(move |ev: SidecarEvent| match &ev {
            SidecarEvent::Done { result, .. } => {
                let out = result
                    .get("outputPath")
                    .and_then(|p| p.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| fallback_out.clone());
                let jam = Arc::clone(&jam);
                let track_id = track_for_sink.clone();
                std::thread::spawn(move || {
                    jam.on_job_done(epoch, out, track_id.to_string(), region);
                });
            }
            SidecarEvent::Error { message, .. } => {
                jam.on_job_failed(epoch, message.clone());
            }
            _ => {}
        });
        let job_id = self.control.jobs.submit(spec, sink);
        {
            let mut i = self.inner.lock();
            i.phase = Phase::Generating;
            i.job_id = Some(job_id);
            i.track_id = Some(track_id.to_string());
            i.region = region;
            i.pending = None;
            i.last_error = None;
        }
        self.emit_state();
        Ok(self.status())
    }

    /// Cancel whatever is in flight (generating job or a ready-but-unapplied
    /// swap). Idempotent; applied/idle stay as they are.
    pub fn cancel(&self) -> LoopJamStatus {
        let job = {
            let mut i = self.inner.lock();
            i.epoch += 1; // stale-ify every worker/watcher thread
            let job = i.job_id.take();
            i.pending = None;
            if matches!(i.phase, Phase::Generating | Phase::Ready) {
                i.phase = Phase::Idle;
            }
            job
        };
        if let Some(id) = job {
            let _ = self.control.jobs.cancel(&id); // done/gone is fine
        }
        self.emit_state();
        self.status()
    }

    // -- job callbacks (worker threads) ------------------------------------

    fn on_job_failed(&self, epoch: u64, message: String) {
        {
            let mut i = self.inner.lock();
            if i.epoch != epoch || i.phase != Phase::Generating {
                return; // cancelled/superseded meanwhile
            }
            i.phase = Phase::Idle;
            i.job_id = None;
            i.last_error = Some(message);
        }
        self.emit_state();
    }

    /// Job finished: prepare the swap (decode, land in `audio/`, pyramid),
    /// mark ready, then watch the playhead and apply at the next wrap.
    fn on_job_done(
        self: Arc<Self>,
        epoch: u64,
        output: PathBuf,
        track_id: String,
        region: (u64, u64),
    ) {
        let prepared = self.prepare_swap(&output, &track_id, region);
        {
            let mut i = self.inner.lock();
            if i.epoch != epoch || i.phase != Phase::Generating {
                return; // cancelled while we were preparing
            }
            match prepared {
                Ok(pending) => {
                    i.phase = Phase::Ready;
                    i.pending = Some(pending);
                }
                Err(e) => {
                    i.phase = Phase::Idle;
                    i.job_id = None;
                    i.last_error = Some(format!("evolve result import failed: {e}"));
                }
            }
        }
        self.emit_state();
        if self.inner.lock().phase != Phase::Ready {
            return;
        }
        self.watch_and_apply(epoch, region);
    }

    /// Decode the worker's output and stage a fully-prepared clip: file in
    /// `<project>/audio/<sourceId>.wav`, waveform pyramid built. Pure
    /// preparation — the store is not touched yet.
    fn prepare_swap(
        &self,
        output: &Path,
        track_id: &str,
        region: (u64, u64),
    ) -> Result<PendingSwap, String> {
        let (channels, rate, samples) = decode_audio(output)?;
        if channels == 0 || samples.is_empty() {
            return Err(format!("evolve result has no samples: {}", output.display()));
        }
        let engine_rate = self.control.shared.sample_rate.load(Relaxed);
        let frames = (samples.len() / channels as usize) as u64;
        // Frames as the engine will see them after resample-at-load.
        let engine_frames = if rate == engine_rate || rate == 0 {
            frames
        } else {
            frames * engine_rate as u64 / rate as u64
        };
        let project_dir = self
            .control
            .session
            .lock()
            .store
            .project_dir
            .clone()
            .ok_or("project closed while evolving")?;
        let clip_id = uuid::Uuid::new_v4().to_string();
        // H-4: the loop-evolve path is a production asset-creating site — the
        // evolved-loop wav is named by a freshly-minted SourceId, same as
        // import.rs and start_recording, so it enters the source-keyed
        // decode cache correctly (skipping this site would feed the
        // empty-sentinel bucket).
        let source_id = crate::ids::SourceId::mint();
        let rel = format!("audio/{source_id}.wav");
        let dst = project_dir.join(&rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(output, &dst).map_err(|e| format!("copy into project failed: {e}"))?;
        let cache_dir = Store::cache_dir_for(&project_dir, &clip_id);
        if !pyramid_exists(&cache_dir) {
            Pyramid::from_interleaved(&samples, channels as usize)
                .write_dir(&cache_dir)
                .map_err(|e| format!("waveform cache build failed: {e}"))?;
        }
        let generation = self.inner.lock().generation;
        let clip = Clip {
            id: clip_id.into(),
            track_id: track_id.into(),
            name: format!("evolve {}", generation + 1),
            source_path: rel,
            source_id,
            source_channels: channels,
            source_sample_rate: rate,
            source_length_samples: frames,
            timeline_start_samples: region.0,
            offset_samples: 0,
            length_samples: (region.1 - region.0).min(engine_frames.max(1)),
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track(track_id),
        };
        Ok(PendingSwap { clip, track_id: track_id.to_string(), region })
    }

    /// Wait for the next loop wrap, then apply. Semantics:
    /// * transport playing: apply the instant the playhead wraps back into
    ///   the region start (position decreases) — next-ready-wrap;
    /// * transport stopped: apply immediately (nothing is audible);
    /// * cancelled/superseded (epoch bump): exit without applying.
    ///
    /// Outer retry loop (Plan E Task 8): `apply` can fail cleanly if the
    /// store changed between its read snapshot and its commit (a genuine
    /// race, not a cancel) — `pending` stays intact in that case and this
    /// loops back to wait for the NEXT wrap and try again, same liveness as
    /// the old lock-then-apply had, just without the lost-update window a
    /// direct mutation used to risk. The inner wait loop's epoch/phase guard
    /// is what actually stops the loop (on either a successful apply, which
    /// flips `phase` to `Applied`, or a cancel, which bumps `epoch`).
    fn watch_and_apply(&self, epoch: u64, region: (u64, u64)) {
        let shared = &self.control.shared;
        loop {
            let mut last = shared.position.load(Relaxed);
            loop {
                {
                    let i = self.inner.lock();
                    if i.epoch != epoch || i.phase != Phase::Ready {
                        return; // cancelled, or a previous iteration applied
                    }
                }
                if !shared.playing.load(Relaxed) {
                    break; // stopped: nothing audible, swap now
                }
                let pos = shared.position.load(Relaxed);
                if pos < last && last >= region.0 {
                    break; // wrapped (or seeked back — equally safe to apply)
                }
                last = pos;
                std::thread::sleep(WATCH_INTERVAL);
            }
            self.apply(epoch);
        }
    }

    /// The hot-swap: replace the region's clips on the target track with the
    /// prepared clip, all through ONE `commit` (Plan E Task 8, round-2
    /// inventory row 20) — `ClipRemove`×n + `Set{Clip,…}` trims +
    /// `ClipAdd`, one atomic, undoable batch. The diff (which clips to
    /// remove/trim/add) is computed from a READ snapshot taken here, with NO
    /// lock held across the computation or the commit that follows —
    /// [`plan_region_replacement`]'s output is plain data, and the commit
    /// closure below re-validates every id against `tx.store()` truth right
    /// before writing. A race — the store changed between the snapshot and
    /// the commit — surfaces as a clean `Err` from the closure; `pending`
    /// is left untouched (not consumed) so `watch_and_apply`'s outer loop
    /// retries the whole thing at the next wrap. `commit`'s own
    /// `effect.rebuild`/`effect.persist.project`/`project://changed` emit
    /// replace the old direct mutation + manual `Rebuild` send + manual
    /// `project::save` + manual emit.
    fn apply(&self, epoch: u64) {
        let pending = {
            let i = self.inner.lock();
            if i.epoch != epoch || i.phase != Phase::Ready {
                return; // cancelled/superseded meanwhile — nothing to do
            }
            match &i.pending {
                Some(p) => p.clone(),
                None => return,
            }
        };

        let snapshot_clips = self.control.session.lock().store.clips.clone();
        let (edits, adds) = plan_region_replacement(
            &snapshot_clips,
            &pending.track_id,
            pending.region.0,
            pending.region.1,
            pending.clip.clone(),
        );

        let commit_result = commit_region_replacement(&self.control, &edits, &adds);

        match commit_result {
            Ok(_) => {
                {
                    let mut i = self.inner.lock();
                    if i.epoch == epoch {
                        i.pending = None;
                        i.phase = Phase::Applied;
                        i.job_id = None;
                        i.generation += 1;
                    }
                    // else: cancelled between the commit and this re-lock —
                    // the store swap already landed (undoable like any
                    // other commit, this isn't a lost update), but the
                    // state machine belongs to the new epoch now.
                } // guard dropped BEFORE emit_state() re-locks `inner` below
                self.emit_state();
            }
            Err(e) => {
                // Race: the store moved between the snapshot and the commit.
                // `pending` is untouched, `phase` is still `Ready` — the
                // caller's outer retry loop (`watch_and_apply`) waits for
                // the next wrap and tries again.
                log::warn!("loopjam apply: commit failed, will retry at the next wrap: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

#[inline]
fn clip_overlaps(c: &Clip, start: u64, end: u64) -> bool {
    let cs = c.timeline_start_samples;
    let ce = cs + c.length_samples;
    cs < end && ce > start
}

/// One existing clip row's participation in a region replacement — the
/// non-destructive, op-shaped counterpart of what [`replace_region_clips`]
/// does with a direct `Vec` rewrite (Plan E Task 8). Computed from a READ
/// snapshot by [`plan_region_replacement`]; turned into `Op::Set`/
/// `Op::ClipRemove` calls by [`LoopJam::apply`]'s commit closure.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegionEdit {
    /// The row keeps its id, shrunk to the span BEFORE the cut
    /// (`cs < start`, no material survives after `end`).
    TrimLeft { id: crate::ids::ClipId, new_length_samples: u64 },
    /// The row keeps its id, repositioned to the span AFTER the cut
    /// (`cs >= start`, material survives past `end`, and there is no
    /// separate `TrimLeft` for this same id claiming it — i.e. the clip
    /// didn't ALSO have material before `start`).
    TrimRight {
        id: crate::ids::ClipId,
        new_timeline_start: u64,
        new_offset: u64,
        new_length: u64,
    },
    /// The row disappears entirely: fully covered by the region, or its
    /// only surviving remainder was claimed by a fresh-id `adds` entry (a
    /// clip straddling BOTH edges — the left remainder keeps the id via
    /// `TrimLeft`, so the right remainder must be a new row).
    Remove(Clip),
}

/// Compute the region-replacement diff from a store SNAPSHOT — no lock held
/// past this call (Plan E Task 8: the commit that applies the diff is the
/// only place truth is re-checked). Mirrors [`replace_region_clips`]'s exact
/// left/right classification for every clip on `track_id` overlapping
/// `[start, end)`:
/// * fully covered → `RegionEdit::Remove`;
/// * material only before the cut (`cs < start`, nothing survives after
///   `end`) → `RegionEdit::TrimLeft` (same id, shrunk);
/// * material only after the cut (`cs >= start`, something survives past
///   `end`) → `RegionEdit::TrimRight` (same id, repositioned) — UNLIKE
///   `replace_region_clips`, which always mints a fresh id for the "right"
///   piece: here, when there is no competing left remainder to also claim
///   the id, keeping it gives a nicer undo (one row's bounds change, not a
///   remove+add) with no observable difference in what plays;
/// * straddles both edges → BOTH a `TrimLeft` (keeps the id) AND a fresh-id
///   `adds` entry (the right piece) — a single id can't name two rows, so
///   here the fresh-id split IS unavoidable, same as `replace_region_clips`.
///
/// `new_clip` (the evolved region's replacement audio) is appended to
/// `adds` unconditionally.
fn plan_region_replacement(
    clips: &[Clip],
    track_id: &str,
    start: u64,
    end: u64,
    new_clip: Clip,
) -> (Vec<RegionEdit>, Vec<Clip>) {
    let mut edits = Vec::new();
    let mut adds = Vec::new();
    for c in clips {
        if c.track_id != track_id || !clip_overlaps(c, start, end) {
            continue;
        }
        let cs = c.timeline_start_samples;
        let ce = cs + c.length_samples;
        let left = cs < start;
        let right = ce > end;
        match (left, right) {
            (true, true) => {
                // Straddles both edges: left piece keeps the id (shrunk),
                // right piece is a brand-new row (fresh id — matches
                // `replace_region_clips`; a single id can't name two rows).
                edits.push(RegionEdit::TrimLeft { id: c.id.clone(), new_length_samples: start - cs });
                let mut right_clip = c.clone();
                right_clip.id = crate::ids::ClipId::mint();
                right_clip.timeline_start_samples = end;
                right_clip.offset_samples = c.offset_samples + (end - cs);
                right_clip.length_samples = ce - end;
                adds.push(right_clip);
            }
            (true, false) => {
                edits.push(RegionEdit::TrimLeft { id: c.id.clone(), new_length_samples: start - cs });
            }
            (false, true) => {
                edits.push(RegionEdit::TrimRight {
                    id: c.id.clone(),
                    new_timeline_start: end,
                    new_offset: c.offset_samples + (end - cs),
                    new_length: ce - end,
                });
            }
            (false, false) => {
                // Fully covered — no remainder on either side.
                edits.push(RegionEdit::Remove(c.clone()));
            }
        }
    }
    adds.push(new_clip);
    (edits, adds)
}

/// Turn a computed diff ([`plan_region_replacement`]'s output) into ONE
/// commit — `Op::ClipRemove`×n + `Op::Set{Clip,…}` trims + `Op::ClipAdd`,
/// `TxMeta::system("loopjam apply")`, NOT transient (a user can undo a
/// LoopJam replacement). Every edit re-validates its target id against
/// `tx.store()` truth right before writing — the store may have moved since
/// the caller's read snapshot — so a stale id fails the WHOLE commit
/// cleanly (rolled back atomically by `Session::transact`) rather than
/// applying a partial swap. Shared by [`LoopJam::apply`] (the production
/// hot-swap) and this module's own tests (which call it directly to get the
/// [`session::Committed`] back — ops for the "ops present" assertion,
/// inverses for the real undo test).
pub(crate) fn commit_region_replacement(
    control: &Arc<ControlPlane>,
    edits: &[RegionEdit],
    adds: &[Clip],
) -> Result<session::Committed, String> {
    control.commit(TxMeta::system("loopjam apply"), |tx| {
        for edit in edits {
            match edit {
                RegionEdit::TrimLeft { id, new_length_samples } => {
                    if !tx.store().clips.iter().any(|c| &c.id == id) {
                        return Err(format!(
                            "loopjam apply: clip {id} no longer exists (store changed \
                             since the region snapshot)"
                        ));
                    }
                    tx.apply(Op::Set {
                        object: ObjectRef::Clip(id.clone()),
                        path: PropPath::LengthSamples,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(new_length_samples),
                    })?;
                }
                RegionEdit::TrimRight { id, new_timeline_start, new_offset, new_length } => {
                    if !tx.store().clips.iter().any(|c| &c.id == id) {
                        return Err(format!(
                            "loopjam apply: clip {id} no longer exists (store changed \
                             since the region snapshot)"
                        ));
                    }
                    tx.apply(Op::Set {
                        object: ObjectRef::Clip(id.clone()),
                        path: PropPath::TimelineStartSamples,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(new_timeline_start),
                    })?;
                    tx.apply(Op::Set {
                        object: ObjectRef::Clip(id.clone()),
                        path: PropPath::OffsetSamples,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(new_offset),
                    })?;
                    tx.apply(Op::Set {
                        object: ObjectRef::Clip(id.clone()),
                        path: PropPath::LengthSamples,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(new_length),
                    })?;
                }
                RegionEdit::Remove(c) => {
                    if !tx.store().clips.iter().any(|x| x.id == c.id) {
                        return Err(format!(
                            "loopjam apply: clip {} no longer exists (store changed \
                             since the region snapshot)",
                            c.id
                        ));
                    }
                    tx.apply(Op::ClipRemove { clip: c.clone(), index: 0 })?;
                }
            }
        }
        for c in adds {
            let index = tx.store().clips.len();
            tx.apply(Op::ClipAdd { clip: c.clone(), index })?;
        }
        Ok(())
    })
}

/// Remove the audible span `[start, end)` from every clip of `track_id`:
/// fully-covered clips are dropped, straddling clips are trimmed (left piece
/// keeps the id, a right piece gets a fresh id + shifted offset). Pure.
///
/// Kept as its own pure `Vec`-rewrite helper for [`plan_region_replacement`]'s
/// doc comment to cite and this file's own unit test to pin the trim
/// arithmetic against — [`LoopJam::apply`] itself now goes through
/// `plan_region_replacement` + a commit instead of calling this directly
/// (Plan E Task 8). `#[cfg(test)]`: its only remaining caller is that test.
#[cfg(test)]
pub(crate) fn replace_region_clips(
    clips: &mut Vec<Clip>,
    track_id: &str,
    start: u64,
    end: u64,
) {
    let mut result: Vec<Clip> = Vec::with_capacity(clips.len());
    for c in clips.drain(..) {
        if c.track_id != track_id || !clip_overlaps(&c, start, end) {
            result.push(c);
            continue;
        }
        let cs = c.timeline_start_samples;
        let ce = cs + c.length_samples;
        if cs < start {
            // Left remainder keeps the original id/fades-in.
            let mut left = c.clone();
            left.length_samples = start - cs;
            left.fade_out_samples = 0;
            result.push(left);
        }
        if ce > end {
            // Right remainder: fresh id, offset advanced past the cut.
            let mut right = c.clone();
            right.id = crate::ids::ClipId::mint();
            right.timeline_start_samples = end;
            right.offset_samples = c.offset_samples + (end - cs);
            right.length_samples = ce - end;
            right.fade_in_samples = 0;
            result.push(right);
        }
        // Fully covered: dropped.
    }
    *clips = result;
}

/// Render the loop region of one track's clips to interleaved stereo f32 at
/// the engine rate, through the REAL mixer render path (clip gain, fades,
/// default unity track params) — what the listener hears looping.
fn render_region_stereo(
    sources: &[(Clip, PathBuf)],
    start: u64,
    end: u64,
    engine_rate: u32,
) -> Result<Vec<f32>, String> {
    let mut rt_clips = Vec::with_capacity(sources.len());
    for (c, path) in sources {
        let (channels, rate, samples) = decode_audio(path)?;
        let data = linear_resample(&samples, channels as usize, rate, engine_rate);
        rt_clips.push(RtClip {
            start: c.timeline_start_samples,
            offset: c.offset_samples,
            len: c.length_samples,
            gain: mixer::db_to_linear(c.gain_db),
            fade_in: c.fade_in_samples,
            fade_out: c.fade_out_samples,
            samples: Arc::new(RtClipData { channels, data }),
        });
    }
    let mut graph = RtGraph::new(
        vec![RtTrack::clips(0, rt_clips)],
        0,
        Arc::new(ParamTable::default()),
    );
    let frames = (end - start) as usize;
    let mut buf = vec![0.0f32; frames * 2];
    let mut pos = start;
    for chunk in buf.chunks_mut(1024 * 2) {
        mixer::render(
            &mut graph,
            pos,
            &LoopSpec::OFF,
            chunk,
            2,
            engine_rate,
            false,
            None,
        );
        pos += (chunk.len() / 2) as u64;
    }
    Ok(buf)
}

/// Build the real ACE-Step repaint job spec (worker schema: inputPath /
/// startSeconds / endSeconds / outputPath [+ prompt, seed]).
fn repaint_spec(params: &serde_json::Value, simulate: bool) -> Result<JobSpec, String> {
    let python = jobs::resolve_python()?;
    let script = jobs::resolve_script("ace_step_worker.py")?;
    let mut args = vec![
        script.to_string_lossy().into_owned(),
        "--task".into(),
        "repaint".into(),
        "--params-json".into(),
        params.to_string(),
    ];
    if simulate {
        args.push("--simulate".into());
    }
    Ok(JobSpec {
        kind: JobKind::AceStepRepaint,
        program: python,
        args,
        grace: Duration::from_secs(3),
        env: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// Commands (need registration in the frozen lib.rs — see wave report):
//   control::loopjam::loopjam_evolve
//   control::loopjam::loopjam_cancel
//   control::loopjam::loopjam_status
// plus managed state after `app.manage(control_plane)`:
//   app.manage(std::sync::Arc::new(control::loopjam::LoopJam::new(
//       app.state::<std::sync::Arc<control::ControlPlane>>().inner().clone(),
//   )));
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn loopjam_evolve(
    options: Option<EvolveOptions>,
    jam: tauri::State<'_, Arc<LoopJam>>,
) -> Result<LoopJamStatus, String> {
    jam.inner().evolve(options.unwrap_or_default())
}

#[tauri::command]
pub fn loopjam_cancel(jam: tauri::State<'_, Arc<LoopJam>>) -> Result<LoopJamStatus, String> {
    Ok(jam.inner().cancel())
}

#[tauri::command]
pub fn loopjam_status(jam: tauri::State<'_, Arc<LoopJam>>) -> Result<LoopJamStatus, String> {
    Ok(jam.inner().status())
}

// ---------------------------------------------------------------------------
// Tests — fake-sidecar-driven state machine (tauri-free, headless engine),
// plus a simulate-mode E2E over the real python worker (gated on python3).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::engine::load_wav;
    use crate::audio::rt::testutil::empty_tables;
    use crate::control::{ImportClipRequest, TransportAction};
    use crate::midi::MidiStore;
    use crate::sidecars::jobs::JobManager;
    use std::time::Instant;

    struct NullEvents;
    impl crate::audio::engine::EventSink for NullEvents {
        fn emit(&self, _e: &str, _p: serde_json::Value) {}
    }

    /// Headless control plane + project + one audio track with a 1 s clip,
    /// loop region set over the clip's first half. Also collects every
    /// emitted app event (loopjam://state etc.).
    fn jam_fixture(
        name: &str,
    ) -> (
        Arc<LoopJam>,
        Arc<ControlPlane>,
        PathBuf,
        Arc<Mutex<Vec<(String, serde_json::Value)>>>,
        (u64, u64),
    ) {
        let shared = Arc::new(crate::audio::rt::SharedRt::default());
        let tables = empty_tables();
        let session = Arc::new(Mutex::new(crate::control::Session::new(
            Store::default(),
            MidiStore::default(),
        )));
        let engine = crate::audio::engine::start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(NullEvents),
            crate::control::testutil::test_committer(&session, &shared, &tables),
        );
        // Event-driven readiness: the control thread opens (or fails to
        // open) its output stream BEFORE draining its first message, so one
        // synchronous round-trip guarantees the real sample rate is settled
        // — a fixed sleep raced the stream open under full-suite load and
        // let the fixture compute the loop region at the wrong rate.
        engine
            .request(|reply| crate::audio::engine::ControlMsg::SelectInput {
                device_id: None,
                reply,
            })
            .expect("engine control thread responds");
        let events: Arc<Mutex<Vec<(String, serde_json::Value)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let ev2 = Arc::clone(&events);
        let cp = Arc::new(ControlPlane::new(
            session,
            shared.clone(),
            tables,
            engine,
            Arc::new(JobManager::new(2, Duration::ZERO)),
            Box::new(move |e, p| ev2.lock().push((e.to_string(), p))),
            std::sync::Arc::new(crate::control::HistoryLog::new()),
        ));
        // (Sample rate settled by the round-trip above — no sleep needed.)

        let parent = std::env::temp_dir().join(format!(
            "aura-loopjam-test-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&parent).unwrap();
        cp.create_project(parent.to_str().unwrap(), "Jam").unwrap();

        // 1 s clip at the engine rate on an auto-created audio track.
        let rate = shared.sample_rate.load(Relaxed);
        let src = parent.join("loopbed.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&src, spec).unwrap();
        for i in 0..rate as usize {
            let v = ((i as f32 * 0.03).sin() * 9_000.0) as i16;
            w.write_sample(v).unwrap();
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
        cp.import_audio_clip_impl(ImportClipRequest {
            path: src.to_string_lossy().into_owned(),
            track_id: None,
            at_samples: Some(0),
        })
        .unwrap();

        // Loop over the clip's first half.
        let region = (0u64, rate as u64 / 2);
        cp.transport(TransportAction::SetLoop {
            enabled: true,
            start_samples: region.0,
            end_samples: region.1,
        })
        .unwrap();

        let jam = Arc::new(LoopJam::new(Arc::clone(&cp)));
        (jam, cp, parent, events, region)
    }

    /// Fake repaint sidecar: copies inputPath -> outputPath (so the "result"
    /// is a real wav), optionally sleeping first.
    fn fake_repaint_spec(parent: &Path, sleep_s: f64) -> impl FnOnce(&serde_json::Value) -> Result<JobSpec, String> + '_ {
        move |params: &serde_json::Value| {
            let input = params["inputPath"].as_str().unwrap().to_string();
            let output = params["outputPath"].as_str().unwrap().to_string();
            let script = parent.join(format!("fake-repaint-{}.sh", uuid::Uuid::new_v4()));
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\n\
                     echo '{{\"type\":\"progress\",\"progress\":0.3,\"stage\":\"diffusing\"}}'\n\
                     sleep {sleep_s}\n\
                     cp \"$1\" \"$2\"\n\
                     echo \"{{\\\"type\\\":\\\"done\\\",\\\"result\\\":{{\\\"kind\\\":\\\"aceStepRepaint\\\",\\\"outputPath\\\":\\\"$2\\\"}}}}\"\n"
                ),
            )
            .unwrap();
            Ok(JobSpec {
                kind: JobKind::AceStepRepaint,
                program: PathBuf::from("/bin/sh"),
                args: vec![script.to_string_lossy().into_owned(), input, output],
                grace: Duration::from_millis(500),
                env: Vec::new(),
            })
        }
    }

    /// Like [`fake_repaint_spec`] but the job blocks until `gate` exists,
    /// letting tests hold the job "in flight" for exactly as long as they
    /// need (event-driven — no sleep-length guessing under load).
    fn fake_repaint_spec_gated(
        parent: &Path,
        gate: PathBuf,
    ) -> impl FnOnce(&serde_json::Value) -> Result<JobSpec, String> + '_ {
        move |params: &serde_json::Value| {
            let input = params["inputPath"].as_str().unwrap().to_string();
            let output = params["outputPath"].as_str().unwrap().to_string();
            let script = parent.join(format!("fake-repaint-gated-{}.sh", uuid::Uuid::new_v4()));
            std::fs::write(
                &script,
                "#!/bin/sh\n\
                 echo '{\"type\":\"progress\",\"progress\":0.3,\"stage\":\"diffusing\"}'\n\
                 while [ ! -e \"$3\" ]; do sleep 0.05; done\n\
                 cp \"$1\" \"$2\"\n\
                 echo \"{\\\"type\\\":\\\"done\\\",\\\"result\\\":{\\\"kind\\\":\\\"aceStepRepaint\\\",\\\"outputPath\\\":\\\"$2\\\"}}\"\n",
            )
            .unwrap();
            Ok(JobSpec {
                kind: JobKind::AceStepRepaint,
                program: PathBuf::from("/bin/sh"),
                args: vec![
                    script.to_string_lossy().into_owned(),
                    input,
                    output,
                    gate.to_string_lossy().into_owned(),
                ],
                grace: Duration::from_millis(500),
                env: Vec::new(),
            })
        }
    }

    fn wait_for_state(jam: &LoopJam, want: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            let st = jam.status();
            if st.state == want {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "never reached `{want}`: {st:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // -- pure helper -------------------------------------------------------

    #[test]
    fn replace_region_trims_splits_and_drops() {
        let mk = |id: &str, track: &str, start: u64, len: u64| Clip {
            id: id.into(),
            track_id: track.into(),
            name: id.into(),
            source_path: format!("audio/{id}.wav"),
            source_id: crate::ids::SourceId::default(),
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: len,
            timeline_start_samples: start,
            offset_samples: 0,
            length_samples: len,
            gain_db: 0.0,
            fade_in_samples: 5,
            fade_out_samples: 7,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track(track),
        };
        let mut clips = vec![
            mk("straddle", "t", 0, 1000),   // spans the whole region
            mk("inside", "t", 300, 100),    // fully covered -> dropped
            mk("before", "t", 0, 200),      // ends at region start -> kept
            mk("other-track", "x", 300, 400), // different track -> kept
        ];
        replace_region_clips(&mut clips, "t", 200, 600);

        let ids: Vec<&str> = clips.iter().map(|c| c.id.as_str()).collect();
        assert!(!ids.contains(&"inside"), "{ids:?}");
        assert!(ids.contains(&"before"));
        assert!(ids.contains(&"other-track"));
        // The straddler became a left piece (same id) + a right piece.
        let left = clips.iter().find(|c| c.id == "straddle").unwrap();
        assert_eq!(left.timeline_start_samples, 0);
        assert_eq!(left.length_samples, 200);
        assert_eq!(left.fade_out_samples, 0, "cut edge has no fade-out");
        let right = clips
            .iter()
            .find(|c| c.timeline_start_samples == 600 && c.track_id == "t")
            .expect("right piece");
        assert_ne!(right.id, "straddle");
        assert_eq!(right.offset_samples, 600);
        assert_eq!(right.length_samples, 400);
        assert_eq!(right.fade_in_samples, 0);
        assert_eq!(clips.len(), 4);
    }

    /// Shared builder for `plan_region_replacement`'s tests (mirrors
    /// `replace_region_trims_splits_and_drops`'s local `mk`, factored out so
    /// more than one test fn can use it).
    fn clip_at(id: &str, track: &str, start: u64, len: u64) -> Clip {
        Clip {
            id: id.into(),
            track_id: track.into(),
            name: id.into(),
            source_path: format!("audio/{id}.wav"),
            source_id: crate::ids::SourceId::default(),
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: len,
            timeline_start_samples: start,
            offset_samples: 0,
            length_samples: len,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track(track),
        }
    }

    /// Plan E Task 8: `plan_region_replacement`'s own dedicated coverage —
    /// the op-shaped counterpart of `replace_region_trims_splits_and_drops`
    /// above. A clip straddling both edges keeps its id for the LEFT
    /// remainder (`TrimLeft`) and gets a fresh-id `adds` entry for the
    /// right one (a single id can't name two rows); a fully-covered clip is
    /// `Remove`d; an untouched/different-track clip produces no edit at
    /// all.
    #[test]
    fn plan_region_replacement_classifies_left_right_and_straddle() {
        let clips = vec![
            clip_at("straddle", "t", 0, 1000),     // spans the whole region
            clip_at("inside", "t", 300, 100),      // fully covered -> Remove
            clip_at("before", "t", 0, 200),        // ends at region start -> untouched
            clip_at("other-track", "x", 300, 400), // different track -> untouched
        ];
        let new_clip = clip_at("new", "t", 200, 400);
        let (edits, adds) = plan_region_replacement(&clips, "t", 200, 600, new_clip.clone());

        assert!(
            edits.iter().any(|e| matches!(
                e, RegionEdit::TrimLeft { id, new_length_samples }
                    if id == "straddle" && *new_length_samples == 200
            )),
            "{edits:?}"
        );
        assert!(
            edits.iter().any(|e| matches!(e, RegionEdit::Remove(c) if c.id == "inside")),
            "{edits:?}"
        );
        let touched: Vec<&str> = edits
            .iter()
            .map(|e| match e {
                RegionEdit::TrimLeft { id, .. } | RegionEdit::TrimRight { id, .. } => id.as_str(),
                RegionEdit::Remove(c) => c.id.as_str(),
            })
            .collect();
        assert!(!touched.contains(&"before"), "{touched:?}");
        assert!(!touched.contains(&"other-track"), "{touched:?}");
        assert_eq!(edits.len(), 2, "{edits:?}"); // straddle's TrimLeft + inside's Remove

        // adds: the straddler's fresh-id right remainder + `new_clip` itself.
        assert_eq!(adds.len(), 2, "{adds:?}");
        assert!(adds.iter().any(|c| c.id == new_clip.id));
        let right = adds
            .iter()
            .find(|c| c.id != new_clip.id)
            .expect("straddle's right remainder");
        assert_ne!(right.id, "straddle", "fresh id, matching replace_region_clips");
        assert_eq!(right.timeline_start_samples, 600);
        assert_eq!(right.offset_samples, 600);
        assert_eq!(right.length_samples, 400);
    }

    /// A clip with material ONLY after the cut (no competing left remainder
    /// for the same id) is trimmed IN PLACE — `TrimRight`, same id — rather
    /// than a remove-then-fresh-id-add, which is the whole point of adding
    /// `PropPath::OffsetSamples`/`LengthSamples` (Task 8 brief).
    #[test]
    fn plan_region_replacement_trims_right_in_place_when_no_left_remainder() {
        let clips = vec![clip_at("right-only", "t", 0, 1000)]; // cs == start
        let new_clip = clip_at("new", "t", 0, 500);
        let (edits, adds) = plan_region_replacement(&clips, "t", 0, 500, new_clip.clone());

        assert_eq!(edits.len(), 1, "{edits:?}");
        match &edits[0] {
            RegionEdit::TrimRight { id, new_timeline_start, new_offset, new_length } => {
                assert_eq!(id, "right-only");
                assert_eq!(*new_timeline_start, 500);
                assert_eq!(*new_offset, 500);
                assert_eq!(*new_length, 500);
            }
            other => panic!("expected TrimRight, got {other:?}"),
        }
        // No fresh-id split needed — just the new (evolved) clip.
        assert_eq!(adds.len(), 1, "{adds:?}");
        assert_eq!(adds[0].id, new_clip.id);
    }

    /// Plan E Task 8, written for real (brief-mandated): `commit_region_
    /// replacement` — the transaction `LoopJam::apply` drives — is ONE
    /// atomic commit (a single rev bump for the whole batch, every expected
    /// op present in that ONE `Committed`), and undo is the real thing:
    /// applying the `Committed`'s OWN inverses through a second commit (not
    /// a hand-rolled revert) restores the pre-apply clip set byte-for-byte.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn commit_region_replacement_is_one_atomic_undoable_batch() {
        let (_jam, cp, parent, _events, region) = jam_fixture("commit-atomic");

        let mut before_clips = cp.project_state().clips;
        before_clips.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        assert_eq!(before_clips.len(), 1, "fixture starts with exactly the imported clip");
        let track_id = before_clips[0].track_id.clone();
        let before_rev = cp.session().lock().rev;

        // A plausible "evolved" replacement for the fixture's loop region,
        // built the same way `LoopJam::prepare_swap` would.
        let new_clip = Clip {
            id: crate::ids::ClipId::mint(),
            track_id: track_id.clone(),
            name: "evolve 1".into(),
            source_path: before_clips[0].source_path.clone(),
            source_id: before_clips[0].source_id.clone(),
            source_channels: before_clips[0].source_channels,
            source_sample_rate: before_clips[0].source_sample_rate,
            source_length_samples: before_clips[0].source_length_samples,
            timeline_start_samples: region.0,
            offset_samples: 0,
            length_samples: region.1 - region.0,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id: before_clips[0].lane_id.clone(),
        };

        let snapshot_clips = cp.session().lock().store.clips.clone();
        let (edits, adds) = plan_region_replacement(
            &snapshot_clips, track_id.as_str(), region.0, region.1, new_clip.clone(),
        );
        assert_eq!(edits.len(), 1, "the fixture clip starts exactly at the region: TrimRight only");
        assert_eq!(adds.len(), 1, "no fresh-id split needed");

        let committed =
            commit_region_replacement(&cp, &edits, &adds).expect("commit succeeds");

        // One atomic commit — exactly one rev bump for the WHOLE batch.
        assert_eq!(cp.session().lock().rev, before_rev + 1, "one commit, one rev bump");

        // Every expected op landed in this ONE Committed: the ClipAdd for
        // the evolved clip, plus the three Sets that repositioned the
        // original clip's surviving remainder in place.
        assert_eq!(committed.ops.len(), 4, "{:?}", committed.ops);
        assert!(
            committed.ops.iter().any(|op| matches!(
                op, Op::ClipAdd { clip, .. } if clip.id == new_clip.id
            )),
            "{:?}", committed.ops
        );
        for path in [PropPath::TimelineStartSamples, PropPath::OffsetSamples, PropPath::LengthSamples] {
            assert!(
                committed.ops.iter().any(|op| matches!(
                    op, Op::Set { object: ObjectRef::Clip(id), path: p, .. }
                        if id == &clip_id_of_original(&before_clips) && *p == path
                )),
                "missing Set{{Clip,{path:?}}}: {:?}", committed.ops
            );
        }

        // Undo, FOR REAL: the Committed's OWN inverses, through a second
        // commit — not a hand-rolled "restore the old Vec" revert.
        let inverses = committed.inverses.clone();
        cp.commit(TxMeta::system("undo loopjam apply (test)"), |tx| {
            for inv in &inverses {
                tx.apply(inv.clone())?;
            }
            Ok(())
        })
        .expect("undo commit succeeds");

        let mut after_undo = cp.project_state().clips;
        after_undo.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        assert_eq!(after_undo, before_clips, "undo restores the pre-apply clip set exactly");

        let _ = std::fs::remove_dir_all(parent);
    }

    /// Helper for the test above: the ORIGINAL fixture clip's id (the one
    /// `TrimRight` repositions in place) — named to keep the assertion loop
    /// readable.
    fn clip_id_of_original(before_clips: &[Clip]) -> crate::ids::ClipId {
        before_clips[0].id.clone()
    }

    // -- state machine over fake sidecars ---------------------------------

    /// Full happy path with the transport STOPPED: generating -> ready ->
    /// applied (stopped transport applies immediately, no wrap needed), and
    /// the region's clips are actually swapped.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn evolve_generates_and_applies_when_stopped() {
        let (jam, cp, parent, events, region) = jam_fixture("stopped");
        assert_eq!(jam.status().state, "idle");

        // Preconditions are enforced.
        cp.transport(TransportAction::SetLoop {
            enabled: false,
            start_samples: 0,
            end_samples: 0,
        })
        .unwrap();
        let e = jam.evolve(EvolveOptions::default()).unwrap_err();
        assert!(e.contains("loop region is not enabled"), "{e}");
        cp.transport(TransportAction::SetLoop {
            enabled: true,
            start_samples: region.0,
            end_samples: region.1,
        })
        .unwrap();

        // Gate the fake job so it stays in flight until we've verified the
        // one-in-flight rule (a sleep-less job can finish before the second
        // evolve call on a fast/idle machine — timing-race otherwise).
        let gate = parent.join("evolve-release-gate");
        let st = jam
            .evolve_with(
                EvolveOptions::default(),
                fake_repaint_spec_gated(&parent, gate.clone()),
            )
            .unwrap();
        assert_eq!(st.state, "generating");
        assert!(st.job_id.is_some());

        // One in-flight max.
        let e = jam
            .evolve_with(EvolveOptions::default(), fake_repaint_spec(&parent, 0.0))
            .unwrap_err();
        assert!(e.contains("already in flight"), "{e}");

        // Release the gated job and let it land.
        std::fs::write(&gate, b"go").unwrap();
        wait_for_state(&jam, "applied", Duration::from_secs(20));
        let st = jam.status();
        assert_eq!(st.generation, 1);
        assert!(st.job_id.is_none());

        // The region now plays the evolved clip: exactly one clip named
        // `evolve 1` at [region.0, region.1), plus the trimmed right piece.
        let snap = cp.project_state();
        let evolved = snap
            .clips
            .iter()
            .find(|c| c.name == "evolve 1")
            .expect("evolved clip registered");
        assert_eq!(evolved.timeline_start_samples, region.0);
        assert_eq!(evolved.length_samples, region.1 - region.0);
        // The original clip's overlap was cut away (right remainder stays).
        let rest: Vec<_> = snap
            .clips
            .iter()
            .filter(|c| c.id != evolved.id && c.track_id == evolved.track_id)
            .collect();
        assert_eq!(rest.len(), 1, "{rest:?}");
        assert_eq!(rest[0].timeline_start_samples, region.1);
        // The evolved source landed in the project and decodes.
        let s = cp.project_state();
        let dir = PathBuf::from(s.project_dir.unwrap());
        let (ch, _, samples) = load_wav(&dir.join(&evolved.source_path)).unwrap();
        assert_eq!(ch, 2);
        assert!(!samples.is_empty());
        // UI events: loopjam://state announced every phase. The `applied`
        // EVENT can land a beat after the status itself flips (the emit is
        // not under the status lock), so wait on the event stream instead of
        // asserting a snapshot.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let states: Vec<String> = events
                .lock()
                .iter()
                .filter(|(e, _)| e == "loopjam://state")
                .map(|(_, p)| p["state"].as_str().unwrap_or("?").to_string())
                .collect();
            if states.contains(&"applied".to_string()) {
                assert!(states.contains(&"generating".to_string()), "{states:?}");
                assert!(states.contains(&"ready".to_string()), "{states:?}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "`applied` event never emitted: {states:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(parent);
    }

    /// Slow-job path while PLAYING: wraps pass during `generating` with no
    /// swap (next-ready-wrap: a job missing wrap N applies at wrap N+1); the
    /// swap happens only after ready + the next wrap, and playback never
    /// leaves the loop region.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_job_applies_at_next_wrap_after_ready() {
        let (jam, cp, parent, _events, region) = jam_fixture("slowjob");
        let shared_rate = {
            let st = cp.transport_state();
            st.sample_rate as u64
        };
        // Play inside the region: headless (or device) playback wraps the
        // 0.5 s region continuously.
        cp.transport(TransportAction::Seek { position_samples: region.0 })
            .unwrap();
        cp.transport(TransportAction::Play).unwrap();

        // ~1.2 s job: at least two wraps pass while generating.
        jam.evolve_with(EvolveOptions::default(), fake_repaint_spec(&parent, 1.2))
            .unwrap();
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(
            jam.status().state,
            "generating",
            "wraps during a slow job must not swap anything"
        );
        assert_eq!(cp.project_state().clips.iter().filter(|c| c.name.starts_with("evolve")).count(), 0);

        wait_for_state(&jam, "applied", Duration::from_secs(30));
        // Applied while playing: playhead is still inside the region.
        let pos = cp.transport_state().position_samples;
        assert!(
            pos >= region.0 && pos < region.1 + shared_rate / 10,
            "playhead escaped the loop during the swap: {pos}"
        );
        assert!(cp
            .project_state()
            .clips
            .iter()
            .any(|c| c.name == "evolve 1"));
        cp.transport(TransportAction::Stop).unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }

    /// Cancel during `generating` kills the job and returns to idle; a
    /// subsequent evolve works (epoch guard: the cancelled job's late events
    /// are ignored).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_during_generating_returns_to_idle() {
        let (jam, cp, parent, _events, _region) = jam_fixture("cancel");
        let st = jam
            .evolve_with(EvolveOptions::default(), fake_repaint_spec(&parent, 30.0))
            .unwrap();
        let job_id = st.job_id.clone().unwrap();

        // Wait until the job is actually running, then cancel.
        let deadline = Instant::now() + Duration::from_secs(5);
        while cp.job_status(&job_id).unwrap().state == "queued" {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let st = jam.cancel();
        assert_eq!(st.state, "idle");
        assert!(st.job_id.is_none());

        // The underlying job terminates as cancelled.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let js = cp.job_status(&job_id).unwrap();
            if js.state == "cancelled" {
                break;
            }
            assert!(Instant::now() < deadline, "{js:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Nothing was swapped; a fresh evolve starts cleanly.
        assert_eq!(
            cp.project_state().clips.iter().filter(|c| c.name.starts_with("evolve")).count(),
            0
        );
        let st = jam
            .evolve_with(EvolveOptions::default(), fake_repaint_spec(&parent, 0.0))
            .unwrap();
        assert_eq!(st.state, "generating");
        wait_for_state(&jam, "applied", Duration::from_secs(20));
        assert_eq!(jam.status().generation, 1);
        let _ = std::fs::remove_dir_all(parent);
    }

    /// Cancel in `ready` (transport playing, wrap not yet reached): the
    /// watcher must exit via the epoch guard and never apply.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_while_ready_never_applies() {
        let (jam, cp, parent, _events, region) = jam_fixture("cancel-ready");
        // Play, but park the playhead BEFORE the region so no wrap occurs
        // (positions only increase toward the region).
        cp.transport(TransportAction::SetLoop {
            enabled: true,
            start_samples: region.0 + 1_000_000_000,
            end_samples: region.1 + 2_000_000_000,
        })
        .unwrap();
        // Region far away => watcher sees playing + no wrap. But evolve needs
        // clips inside the region — so evolve against the REAL region first,
        // then move the loop far away while the job runs.
        cp.transport(TransportAction::SetLoop {
            enabled: true,
            start_samples: region.0,
            end_samples: region.1,
        })
        .unwrap();
        jam.evolve_with(EvolveOptions::default(), fake_repaint_spec(&parent, 0.3))
            .unwrap();
        // Playhead runs free far below the (soon moved) region; playing=true
        // keeps the watcher in its wrap-wait loop.
        cp.transport(TransportAction::Seek { position_samples: region.0 })
            .unwrap();
        cp.transport(TransportAction::SetLoop {
            enabled: true,
            start_samples: 4_000_000_000,
            end_samples: 5_000_000_000,
        })
        .unwrap();
        cp.transport(TransportAction::Play).unwrap();
        wait_for_state(&jam, "ready", Duration::from_secs(15));

        let st = jam.cancel();
        assert_eq!(st.state, "idle");
        // Give a stale watcher every chance to misbehave, then verify.
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(jam.status().state, "idle");
        assert_eq!(
            cp.project_state().clips.iter().filter(|c| c.name.starts_with("evolve")).count(),
            0,
            "cancelled ready swap must never apply"
        );
        cp.transport(TransportAction::Stop).unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }

    /// A failing worker resets to idle and reports lastError.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_job_resets_to_idle_with_error() {
        let (jam, _cp, parent, _events, _region) = jam_fixture("jobfail");
        let script = parent.join("boom.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho '{\"type\":\"error\",\"message\":\"ACE-Step is not installed\"}'\nexit 1\n",
        )
        .unwrap();
        jam.evolve_with(EvolveOptions::default(), |_p| {
            Ok(JobSpec {
                kind: JobKind::AceStepRepaint,
                program: PathBuf::from("/bin/sh"),
                args: vec![script.to_string_lossy().into_owned()],
                grace: Duration::from_millis(200),
                env: Vec::new(),
            })
        })
        .unwrap();
        wait_for_state(&jam, "idle", Duration::from_secs(10));
        let st = jam.status();
        assert!(
            st.last_error.as_deref().unwrap().contains("ACE-Step"),
            "{st:?}"
        );
        // Recoverable: a new evolve is accepted.
        let st = jam
            .evolve_with(EvolveOptions::default(), fake_repaint_spec(&parent, 0.0))
            .unwrap();
        assert_eq!(st.state, "generating");
        wait_for_state(&jam, "applied", Duration::from_secs(20));
        assert!(jam.status().last_error.is_none(), "error cleared on evolve");
        let _ = std::fs::remove_dir_all(parent);
    }

    /// Evolving with no clips in the region (or no loop) is a clean error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn evolve_preconditions_are_clean_errors() {
        let (jam, cp, parent, _events, _region) = jam_fixture("precond");
        // Move the loop somewhere empty.
        cp.transport(TransportAction::SetLoop {
            enabled: true,
            start_samples: 4_000_000_000,
            end_samples: 4_100_000_000,
        })
        .unwrap();
        let e = jam.evolve(EvolveOptions::default()).unwrap_err();
        assert!(e.contains("no audio track has clips"), "{e}");
        // Unknown/wrong-kind track ids.
        let e = jam
            .evolve(EvolveOptions { track_id: Some("ghost".into()), ..Default::default() })
            .unwrap_err();
        assert!(e.contains("unknown track"), "{e}");
        assert_eq!(jam.status().state, "idle");
        let _ = std::fs::remove_dir_all(parent);
    }

    // -- simulate-mode E2E over the real worker (gated on python3) ---------

    /// The whole loop-jam path against the REAL ace_step_worker.py in
    /// --simulate mode: evolve -> repaint job -> hot-swap. Skips politely
    /// when python3 is missing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn simulate_e2e_repaints_the_loop_region() {
        if jobs::resolve_python().is_err() {
            eprintln!("skipping loop-jam simulate E2E: python3 not found");
            return;
        }
        let (jam, cp, parent, _events, region) = jam_fixture("sim-e2e");
        let st = jam
            .evolve(EvolveOptions {
                prompt: Some("dubby half-time rework".into()),
                seed: Some(1234),
                simulate: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(st.state, "generating");
        wait_for_state(&jam, "applied", Duration::from_secs(60));

        let snap = cp.project_state();
        let evolved = snap
            .clips
            .iter()
            .find(|c| c.name == "evolve 1")
            .expect("evolved clip");
        assert_eq!(evolved.timeline_start_samples, region.0);
        assert_eq!(evolved.length_samples, region.1 - region.0);
        // The worker's simulate output is a real PCM wav and it differs from
        // the input region (the repaint patched it).
        let dir = PathBuf::from(snap.project_dir.clone().unwrap());
        let (ch, _rate, samples) = load_wav(&dir.join(&evolved.source_path)).unwrap();
        assert_eq!(ch, 2);
        assert!(samples.iter().any(|s| s.abs() > 0.01), "not silent");
        // Job bookkeeping: exactly one loop-jam job ran to done.
        let jobs_list = cp.list_jobs();
        assert!(jobs_list
            .iter()
            .any(|j| j.kind == "aceStepRepaint" && j.state == "done"));
        let _ = std::fs::remove_dir_all(parent);
    }
}
