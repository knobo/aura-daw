//! Multi-track recorder: the disk-writer thread.
//!
//! The cpal input callback pushes interleaved f32 samples into one rtrb SPSC
//! ring per recorded track (≥ 2 s of headroom); this thread drains the rings,
//! streams WAV (f32, via hound), folds the samples into a waveform-pyramid
//! builder, and on stop finalizes files, writes the pyramid cache and reports
//! the finished `Clip`s.
//!
//! FLAC: `flacenc` is available in Cargo.toml for OFFLINE archival transcodes
//! (arch §5) — capture always writes WAV f32; a transcode task is future work.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::ids::SourceId;

use super::pitch_store::{write_track, PitchFolder};
use super::types::Clip;
use super::waveform::PyramidBuilder;

/// Everything the writer needs to finalize one recorded track.
pub struct RecSpec {
    pub track_id: String,
    /// Pre-assigned clip UUID (also the WAV file stem for cache/pyramid dirs).
    pub clip_id: String,
    /// Pre-minted source identity (round-2 §2.2) — the take's wav is named
    /// by this, not by `clip_id` (the decode cache is source-keyed).
    pub source_id: SourceId,
    pub take_name: String,
    /// Absolute WAV destination (`<proj>/audio/<sourceId>.wav`).
    pub wav_path: PathBuf,
    /// Project-relative source path stored in the clip ("audio/<sourceId>.wav").
    pub rel_path: String,
    /// Waveform cache dir (`<proj>/cache/waveforms/<clipId>`).
    pub cache_dir: PathBuf,
    /// Pitch-track destination (`<proj>/cache/pitch/<clipId>.bin`). `None`
    /// skips the fold entirely — a take with no project dir behind it has
    /// nowhere to put one.
    pub pitch_path: Option<PathBuf>,
    /// Timeline position where the take started (engine samples).
    pub start_pos: u64,
}

/// Handle held by the engine control thread while recording.
pub struct DiskWriter {
    pub stop: Arc<AtomicBool>,
    pub result_rx: crossbeam_channel::Receiver<Result<Vec<Clip>, String>>,
    pub join: Option<JoinHandle<()>>,
}

impl DiskWriter {
    /// Signal stop, wait for the finalized clips (producers must already be
    /// dropped so the rings drain to empty).
    pub fn finish(mut self, timeout: Duration) -> Result<Vec<Clip>, String> {
        self.stop.store(true, Ordering::Release);
        let res = self
            .result_rx
            .recv_timeout(timeout)
            .map_err(|_| "disk writer did not finish in time".to_string())?;
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        res
    }
}

/// Spawn the disk-writer thread. `consumers[i]` carries interleaved
/// `channels`-channel f32 audio for `specs[i]` at `sample_rate` Hz.
pub fn spawn(
    specs: Vec<RecSpec>,
    consumers: Vec<rtrb::Consumer<f32>>,
    channels: u16,
    sample_rate: u32,
) -> Result<DiskWriter, String> {
    assert_eq!(specs.len(), consumers.len());
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let (result_tx, result_rx) = crossbeam_channel::bounded(1);

    let join = std::thread::Builder::new()
        .name("aura-disk-writer".into())
        .spawn(move || {
            let res = run_writer(specs, consumers, channels, sample_rate, &stop2);
            let _ = result_tx.send(res);
        })
        .map_err(|e| e.to_string())?;

    Ok(DiskWriter { stop, result_rx, join: Some(join) })
}

struct TrackWriter {
    spec: RecSpec,
    wav: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    pyramid: PyramidBuilder,
    /// Folded alongside the pyramid, from the same drained samples. Absent
    /// when the take has nowhere to write one.
    pitch: Option<PitchFolder>,
    samples_written: u64,
}

fn run_writer(
    specs: Vec<RecSpec>,
    mut consumers: Vec<rtrb::Consumer<f32>>,
    channels: u16,
    sample_rate: u32,
    stop: &AtomicBool,
) -> Result<Vec<Clip>, String> {
    let wav_spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writers = Vec::with_capacity(specs.len());
    for spec in specs {
        if let Some(parent) = spec.wav_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let wav = hound::WavWriter::create(&spec.wav_path, wav_spec)
            .map_err(|e| format!("create {}: {e}", spec.wav_path.display()))?;
        let pitch = spec.pitch_path.as_ref().map(|_| PitchFolder::new(sample_rate, channels));
        writers.push(TrackWriter {
            spec,
            wav,
            pyramid: PyramidBuilder::new(channels as usize),
            pitch,
            samples_written: 0,
        });
    }

    // Drain loop.
    loop {
        let mut moved = false;
        for (i, cons) in consumers.iter_mut().enumerate() {
            let avail = cons.slots();
            if avail == 0 {
                continue;
            }
            let chunk = match cons.read_chunk(avail) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let (a, b) = chunk.as_slices();
            let w = &mut writers[i];
            for &s in a.iter().chain(b.iter()) {
                w.wav.write_sample(s).map_err(|e| e.to_string())?;
            }
            w.pyramid.push_interleaved(a);
            w.pyramid.push_interleaved(b);
            if let Some(p) = w.pitch.as_mut() {
                p.push_interleaved(a);
                p.push_interleaved(b);
            }
            w.samples_written += avail as u64;
            chunk.commit_all();
            moved = true;
        }
        if !moved {
            let done = stop.load(Ordering::Acquire)
                || consumers.iter().all(|c| c.is_abandoned());
            if done {
                // One final sweep in case samples landed between checks.
                let residue: usize = consumers.iter().map(|c| c.slots()).sum();
                if residue == 0 {
                    break;
                }
            } else {
                std::thread::sleep(Duration::from_millis(3));
            }
        }
    }

    // Finalize.
    let mut clips = Vec::with_capacity(writers.len());
    for w in writers {
        w.wav.finalize().map_err(|e| e.to_string())?;
        let frames = w.samples_written / channels.max(1) as u64;
        w.pyramid
            .finish()
            .write_dir(&w.spec.cache_dir)
            .map_err(|e| format!("waveform cache: {e}"))?;
        // A pitch track is a cache, not a take. Losing one costs a
        // re-analysis, so a failure here must not fail the recording that
        // is already safely on disk.
        if let (Some(folder), Some(path)) = (w.pitch, w.spec.pitch_path.as_ref()) {
            if let Err(e) = write_track(path, &folder.finish()) {
                eprintln!("[recorder] pitch track not written: {e}");
            }
        }
        let lane_id = crate::ids::LaneId::default_for_track(&w.spec.track_id);
        clips.push(Clip {
            id: w.spec.clip_id.into(),
            track_id: w.spec.track_id.into(),
            name: w.spec.take_name,
            source_path: w.spec.rel_path,
            source_id: w.spec.source_id,
            source_channels: channels,
            source_sample_rate: sample_rate,
            source_length_samples: frames,
            timeline_start_samples: w.spec.start_pos,
            offset_samples: 0,
            length_samples: frames,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id,
        });
    }
    Ok(clips)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("aura-rec-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn spec(dir: &PathBuf, clip_id: &str, start_pos: u64) -> RecSpec {
        let source_id = SourceId::mint();
        RecSpec {
            track_id: "track-1".into(),
            clip_id: clip_id.into(),
            wav_path: dir.join(format!("audio/{source_id}.wav")),
            rel_path: format!("audio/{source_id}.wav"),
            source_id,
            take_name: "Take 1".into(),
            cache_dir: dir.join(format!("cache/waveforms/{clip_id}")),
            pitch_path: Some(dir.join(format!("cache/pitch/{clip_id}.bin"))),
            start_pos,
        }
    }

    #[test]
    fn wav_roundtrip_mono() {
        let dir = tmp_dir("mono");
        let (mut prod, cons) = rtrb::RingBuffer::<f32>::new(96_000);
        let writer = spawn(vec![spec(&dir, "clipA", 1234)], vec![cons], 1, 48_000).unwrap();

        // 480 samples: a ramp -1..1
        let samples: Vec<f32> =
            (0..480).map(|i| i as f32 / 479.0 * 2.0 - 1.0).collect();
        for &s in &samples {
            prod.push(s).unwrap();
        }
        drop(prod); // simulate input stream teardown
        let clips = writer.finish(Duration::from_secs(10)).unwrap();

        assert_eq!(clips.len(), 1);
        let c = &clips[0];
        assert_eq!(c.source_length_samples, 480);
        assert_eq!(c.length_samples, 480);
        assert_eq!(c.timeline_start_samples, 1234);
        assert_eq!(c.source_channels, 1);
        assert_eq!(c.source_sample_rate, 48_000);
        assert!(!c.source_id.as_str().is_empty(), "a source id was minted");
        assert_eq!(c.source_path, format!("audio/{}.wav", c.source_id), "asset named by source, not clip");

        // Read back and compare bit-exact.
        let mut r = hound::WavReader::open(dir.join(&c.source_path)).unwrap();
        assert_eq!(r.spec().sample_format, hound::SampleFormat::Float);
        assert_eq!(r.spec().sample_rate, 48_000);
        let back: Vec<f32> = r.samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(back, samples);

        // Waveform cache exists with the right bin count (480/256 -> 2 bins).
        let tile = super::super::waveform::read_tile(
            &dir.join("cache/waveforms/clipA"),
            0,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(tile.0, 1);
        assert_eq!(tile.1.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multitrack_stereo_capture() {
        let dir = tmp_dir("multi");
        let (mut p1, c1) = rtrb::RingBuffer::<f32>::new(96_000);
        let (mut p2, c2) = rtrb::RingBuffer::<f32>::new(96_000);
        let writer = spawn(
            vec![spec(&dir, "c1", 0), spec(&dir, "c2", 0)],
            vec![c1, c2],
            2,
            44_100,
        )
        .unwrap();

        // 100 stereo frames each, distinct content per track.
        for f in 0..100 {
            p1.push(f as f32 * 0.001).unwrap();
            p1.push(-(f as f32) * 0.001).unwrap();
            p2.push(0.5).unwrap();
            p2.push(-0.5).unwrap();
        }
        drop(p1);
        drop(p2);
        let mut clips = writer.finish(Duration::from_secs(10)).unwrap();
        clips.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(clips.len(), 2);
        assert!(clips.iter().all(|c| c.source_length_samples == 100));
        assert!(clips.iter().all(|c| c.source_channels == 2));

        let c2 = clips.iter().find(|c| c.id == "c2").unwrap();
        let mut r = hound::WavReader::open(dir.join(&c2.source_path)).unwrap();
        let back: Vec<f32> = r.samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(back.len(), 200);
        assert_eq!(back[0], 0.5);
        assert_eq!(back[1], -0.5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The take carries its own pitch curve out of the same drain that
    /// writes the WAV — nothing has to re-read the audio afterwards.
    #[test]
    fn a_take_folds_a_pitch_track_beside_the_waveform_cache() {
        let dir = tmp_dir("pitch");
        let (mut prod, cons) = rtrb::RingBuffer::<f32>::new(96_000 * 2);
        let writer = spawn(vec![spec(&dir, "clipP", 0)], vec![cons], 1, 48_000).unwrap();
        // 0.5 s of A3 (220 Hz) — long enough for the detector to lock.
        for i in 0..24_000 {
            let s = (2.0 * std::f32::consts::PI * 220.0 * i as f32 / 48_000.0).sin() * 0.5;
            prod.push(s).unwrap();
        }
        drop(prod);
        writer.finish(Duration::from_secs(10)).unwrap();

        let track =
            super::super::pitch_store::read_track(&dir.join("cache/pitch/clipP.bin")).unwrap();
        assert_eq!(track.rate, 48_000);
        assert_eq!(track.hop, 480);
        let voiced: Vec<_> = track.frames.iter().filter(|f| f.voiced).collect();
        assert!(voiced.len() > 20, "only {} voiced frames", voiced.len());
        let mid = voiced[voiced.len() / 2];
        assert!((mid.hz - 220.0).abs() < 2.0, "detected {} Hz", mid.hz);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A take without a project behind it still records; it just has
    /// nowhere to keep a curve.
    #[test]
    fn no_pitch_path_means_no_fold_and_no_failure() {
        let dir = tmp_dir("nopitch");
        let (mut prod, cons) = rtrb::RingBuffer::<f32>::new(4096);
        let mut s = spec(&dir, "clipN", 0);
        s.pitch_path = None;
        let writer = spawn(vec![s], vec![cons], 1, 48_000).unwrap();
        for i in 0..480 {
            prod.push(i as f32 / 480.0).unwrap();
        }
        drop(prod);
        let clips = writer.finish(Duration::from_secs(10)).unwrap();
        assert_eq!(clips[0].source_length_samples, 480);
        assert!(!dir.join("cache/pitch").exists(), "nothing written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_drains_after_stop_signal() {
        // Samples pushed, stop set immediately WITHOUT dropping the producer
        // first: writer must still drain everything before finalizing.
        let dir = tmp_dir("drain");
        let (mut prod, cons) = rtrb::RingBuffer::<f32>::new(4096);
        let writer = spawn(vec![spec(&dir, "d", 0)], vec![cons], 1, 48_000).unwrap();
        for i in 0..1000 {
            prod.push(i as f32).unwrap();
        }
        let clips = writer.finish(Duration::from_secs(10)).unwrap();
        drop(prod);
        assert_eq!(clips[0].source_length_samples, 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
