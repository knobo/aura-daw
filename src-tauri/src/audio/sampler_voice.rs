//! Sampler voice machinery — OWNED BY THE SAMPLER AGENT (phase 2, zone D).
//!
//! Pure, allocation-free-at-render-time voice pool used by both the
//! `SamplerNode` (midi-track instrument, `sampler_engine.rs`) and the preview
//! path (`sampler_preview.rs`). Everything here obeys the §2.1 RT contract in
//! its render methods: all allocation happens at construction / `prepare`
//! time on the control thread; `render` only reads the immutable compiled
//! regions and mutates preallocated voice state.
//!
//! * Region lookup: first region whose key AND velocity range contain the
//!   note (SFZ convention — regions are checked in file order).
//! * Pitch: playback ratio `2^((key − pitch_keycenter + transpose +
//!   tune/100) / 12) × sample_rate_of_sample / output_rate`, linear-interp
//!   resampling (v1 contract, sampler.rs header).
//! * Envelope: SFZ-subset AR — linear attack (`ampeg_attack` seconds) to
//!   sustain 1.0, linear release (`ampeg_release` seconds) from the current
//!   level. (The SUBSET v1 defines only attack/release; decay/sustain are
//!   fixed at 0 s / 1.0 — the degenerate ADSR.)
//! * Stealing: when the pool is full the OLDEST voice (smallest start
//!   sequence number) is stolen — deterministic by construction because the
//!   sequence counter is strictly monotonic.

use std::sync::Arc;

/// Decoded sample audio: interleaved f32 at `rate`.
pub struct SampleData {
    pub channels: u16,
    pub rate: u32,
    pub data: Vec<f32>,
}

impl SampleData {
    #[inline]
    pub fn frames(&self) -> u64 {
        (self.data.len() / self.channels.max(1) as usize) as u64
    }
}

/// One SFZ region compiled for playback (immutable once built; shared with
/// the RT thread inside an immutable instrument snapshot).
pub struct CompiledRegion {
    pub lokey: u8,
    pub hikey: u8,
    pub lovel: u8,
    pub hivel: u8,
    pub pitch_keycenter: u8,
    /// Cents, -100..=100.
    pub tune: i16,
    /// Semitones, -24..=24.
    pub transpose: i8,
    /// Left/right linear gains (SFZ `volume` dB + `pan` folded in).
    pub gain_l: f32,
    pub gain_r: f32,
    /// Sample start offset, frames.
    pub offset: u64,
    pub looped: bool,
    pub loop_start: u64,
    pub loop_end: u64,
    /// Seconds.
    pub ampeg_attack: f32,
    pub ampeg_release: f32,
    pub sample: Arc<SampleData>,
}

#[inline]
pub fn region_matches(r: &CompiledRegion, key: u8, velocity: u8) -> bool {
    key >= r.lokey && key <= r.hikey && velocity >= r.lovel && velocity <= r.hivel
}

/// First region containing (key, velocity) — SFZ file order wins.
#[inline]
pub fn find_region(regions: &[CompiledRegion], key: u8, velocity: u8) -> Option<usize> {
    regions.iter().position(|r| region_matches(r, key, velocity))
}

/// Playback ratio (source frames advanced per output frame) for `key` played
/// through `region` at engine rate `out_rate`.
#[inline]
pub fn pitch_ratio(region: &CompiledRegion, key: u8, out_rate: u32) -> f64 {
    let semis = key as f64 - region.pitch_keycenter as f64
        + region.transpose as f64
        + region.tune as f64 / 100.0;
    (semis / 12.0).exp2() * region.sample.rate as f64 / out_rate.max(1) as f64
}

/// Sentinel: note has no auto-release scheduled (waits for an explicit off).
pub const HOLD_MANUAL: u64 = u64::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Idle,
    Attack,
    Sustain,
    Release,
}

/// One preallocated voice. POD-sized; never allocates.
#[derive(Clone)]
pub struct Voice {
    stage: Stage,
    pub key: u8,
    pub velocity: u8,
    region: usize,
    /// Absolute source position, frames (starts at region offset).
    pos: f64,
    ratio: f64,
    env: f32,
    attack_step: f32,
    release_step: f32,
    vel_gain: f32,
    /// Start sequence number (stealing order).
    age: u64,
    /// Frames until auto note-off; HOLD_MANUAL = wait for note_off().
    hold_left: u64,
}

impl Voice {
    const fn idle() -> Self {
        Self {
            stage: Stage::Idle,
            key: 0,
            velocity: 0,
            region: 0,
            pos: 0.0,
            ratio: 1.0,
            env: 0.0,
            attack_step: 1.0,
            release_step: 1.0,
            vel_gain: 0.0,
            age: 0,
            hold_left: HOLD_MANUAL,
        }
    }

    #[inline]
    pub fn active(&self) -> bool {
        self.stage != Stage::Idle
    }

    fn release(&mut self) {
        if self.stage != Stage::Idle && self.stage != Stage::Release {
            self.stage = Stage::Release;
        }
    }
}

/// Fixed-size, preallocated polyphonic voice pool with oldest-note stealing.
pub struct VoicePool {
    voices: Vec<Voice>,
    next_age: u64,
}

impl VoicePool {
    /// Allocates all voices up front (control thread only).
    pub fn new(polyphony: usize) -> Self {
        Self {
            voices: vec![Voice::idle(); polyphony.max(1)],
            next_age: 0,
        }
    }

    pub fn active_count(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }

    /// Keys of active voices ordered by start sequence (test/debug helper).
    pub fn active_keys(&self) -> Vec<u8> {
        let mut v: Vec<(u64, u8)> =
            self.voices.iter().filter(|v| v.active()).map(|v| (v.age, v.key)).collect();
        v.sort_unstable();
        v.into_iter().map(|(_, k)| k).collect()
    }

    /// Start a note. Returns the voice index used, or None when no region
    /// matches (note outside the instrument's mapped ranges). RT-safe.
    pub fn note_on(
        &mut self,
        regions: &[CompiledRegion],
        key: u8,
        velocity: u8,
        out_rate: u32,
        hold_frames: u64,
    ) -> Option<usize> {
        let velocity = velocity.clamp(1, 127);
        let ri = find_region(regions, key, velocity)?;
        let r = &regions[ri];

        // Free voice, else steal the oldest (deterministic: min start seq).
        let slot = match self.voices.iter().position(|v| !v.active()) {
            Some(i) => i,
            None => self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.age)
                .map(|(i, _)| i)?,
        };

        let rate = out_rate.max(1) as f32;
        let v = &mut self.voices[slot];
        v.stage = Stage::Attack;
        v.key = key;
        v.velocity = velocity;
        v.region = ri;
        v.pos = r.offset as f64;
        v.ratio = pitch_ratio(r, key, out_rate);
        v.env = 0.0;
        v.attack_step = 1.0 / (r.ampeg_attack.max(1.0e-4) * rate);
        v.release_step = 1.0 / (r.ampeg_release.max(1.0e-4) * rate);
        v.vel_gain = {
            let n = velocity as f32 / 127.0;
            n * n
        };
        v.age = self.next_age;
        self.next_age += 1;
        v.hold_left = hold_frames;
        Some(slot)
    }

    /// Release every sounding voice with `key`. RT-safe.
    pub fn note_off(&mut self, key: u8) {
        for v in &mut self.voices {
            if v.active() && v.key == key {
                v.release();
            }
        }
    }

    /// Release everything (soft: envelopes run out). RT-safe.
    pub fn release_all(&mut self) {
        for v in &mut self.voices {
            v.release();
        }
    }

    /// Hard stop (voices silenced immediately). RT-safe.
    pub fn kill_all(&mut self) {
        for v in &mut self.voices {
            v.stage = Stage::Idle;
        }
    }

    /// Render frames `[from, to)` of the current block ADDITIVELY into `out`
    /// (interleaved, `out_ch` channels). RT-safe: no alloc/lock/syscall.
    pub fn render(
        &mut self,
        regions: &[CompiledRegion],
        out: &mut [f32],
        out_ch: usize,
        from: usize,
        to: usize,
    ) {
        let out_ch = out_ch.max(1);
        let to = to.min(out.len() / out_ch);
        if from >= to {
            return;
        }
        for v in &mut self.voices {
            if !v.active() || v.region >= regions.len() {
                continue;
            }
            let r = &regions[v.region];
            let frames = r.sample.frames();
            if frames == 0 {
                v.stage = Stage::Idle;
                continue;
            }
            let ch = r.sample.channels.max(1) as usize;
            let data = &r.sample.data;
            let loop_len = r.loop_end.saturating_sub(r.loop_start);
            let use_loop = r.looped && loop_len > 0 && r.loop_end <= frames;

            for f in from..to {
                // Envelope.
                match v.stage {
                    Stage::Attack => {
                        v.env += v.attack_step;
                        if v.env >= 1.0 {
                            v.env = 1.0;
                            v.stage = Stage::Sustain;
                        }
                    }
                    Stage::Sustain => {}
                    Stage::Release => {
                        v.env -= v.release_step;
                        if v.env <= 0.0 {
                            v.env = 0.0;
                            v.stage = Stage::Idle;
                            break;
                        }
                    }
                    Stage::Idle => break,
                }
                // Scheduled auto-release (preview one-shots).
                if v.hold_left != HOLD_MANUAL {
                    if v.hold_left == 0 {
                        v.release();
                    } else {
                        v.hold_left -= 1;
                    }
                }

                // Source position (with loop wrap).
                let mut pos = v.pos;
                if use_loop && pos >= r.loop_end as f64 {
                    pos -= loop_len as f64
                        * (((pos - r.loop_start as f64) / loop_len as f64).floor()).max(1.0);
                    v.pos = pos;
                }
                let i0 = pos as u64;
                if i0 >= frames {
                    v.stage = Stage::Idle;
                    break;
                }
                let i1 = if use_loop && i0 + 1 >= r.loop_end {
                    r.loop_start.min(frames - 1)
                } else {
                    (i0 + 1).min(frames - 1)
                };
                let t = (pos - i0 as f64) as f32;
                let b0 = i0 as usize * ch;
                let b1 = i1 as usize * ch;
                let sl = data[b0] + (data[b1] - data[b0]) * t;
                let sr = if ch > 1 {
                    data[b0 + 1] + (data[b1 + 1] - data[b0 + 1]) * t
                } else {
                    sl
                };

                let g = v.env * v.vel_gain;
                let l = sl * g * r.gain_l;
                let rch = sr * g * r.gain_r;
                let o = f * out_ch;
                if out_ch >= 2 {
                    out[o] += l;
                    out[o + 1] += rch;
                } else {
                    out[o] += 0.5 * (l + rch);
                }

                v.pos += v.ratio;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// Mono sine sample at `freq` Hz, `rate`, `secs` long, amplitude `amp`.
    pub fn sine_sample(freq: f64, rate: u32, secs: f64, amp: f32) -> Arc<SampleData> {
        let n = (rate as f64 * secs) as usize;
        let data = (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32 * amp
            })
            .collect();
        Arc::new(SampleData { channels: 1, rate, data })
    }

    pub fn region(sample: Arc<SampleData>) -> CompiledRegion {
        CompiledRegion {
            lokey: 0,
            hikey: 127,
            lovel: 1,
            hivel: 127,
            pitch_keycenter: 69,
            tune: 0,
            transpose: 0,
            gain_l: 1.0,
            gain_r: 1.0,
            offset: 0,
            looped: false,
            loop_start: 0,
            loop_end: 0,
            ampeg_attack: 0.0001,
            ampeg_release: 0.01,
            sample,
        }
    }

    /// Fundamental estimate via normalized autocorrelation over `lo..hi` Hz.
    pub fn estimate_freq(mono: &[f32], rate: u32, lo: f64, hi: f64) -> f64 {
        let min_lag = (rate as f64 / hi).floor().max(1.0) as usize;
        let max_lag = (rate as f64 / lo).ceil() as usize;
        let n = mono.len();
        assert!(n > max_lag * 2, "signal too short for f0 search");
        let mut scores = Vec::with_capacity(max_lag - min_lag + 1);
        for lag in min_lag..=max_lag {
            let m = n - lag;
            let (mut num, mut d0, mut d1) = (0.0f64, 0.0f64, 0.0f64);
            for i in 0..m {
                let a = mono[i] as f64;
                let b = mono[i + lag] as f64;
                num += a * b;
                d0 += a * a;
                d1 += b * b;
            }
            let denom = (d0 * d1).sqrt();
            scores.push((lag, if denom > 0.0 { num / denom } else { 0.0 }));
        }
        let best = scores.iter().map(|(_, s)| *s).fold(f64::MIN, f64::max);
        // Integer multiples of the true period score ~equally; the
        // FUNDAMENTAL is the smallest LOCAL PEAK within epsilon of the max
        // (a plain smallest-lag rule would grab the shoulder of the peak).
        let idx = (1..scores.len().saturating_sub(1))
            .find(|&i| {
                scores[i].1 >= scores[i - 1].1
                    && scores[i].1 >= scores[i + 1].1
                    && scores[i].1 >= best - 0.01
            })
            .unwrap_or_else(|| {
                scores
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1 .1.total_cmp(&b.1 .1))
                    .map(|(i, _)| i)
                    .unwrap()
            });
        // Parabolic sub-sample refinement around the peak.
        let lag = scores[idx].0 as f64
            + if idx > 0 && idx + 1 < scores.len() {
                let (sm, s0, sp) = (scores[idx - 1].1, scores[idx].1, scores[idx + 1].1);
                let denom = sm - 2.0 * s0 + sp;
                if denom.abs() > 1e-12 { 0.5 * (sm - sp) / denom } else { 0.0 }
            } else {
                0.0
            };
        rate as f64 / lag
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    const RATE: u32 = 48_000;

    fn stereo_out(frames: usize) -> Vec<f32> {
        vec![0.0f32; frames * 2]
    }

    // ---- pitch-ratio math ----

    #[test]
    fn pitch_ratio_reference_points() {
        let mut r = region(sine_sample(440.0, RATE, 0.1, 1.0));
        assert!((pitch_ratio(&r, 69, RATE) - 1.0).abs() < 1e-12, "root = unity");
        assert!((pitch_ratio(&r, 81, RATE) - 2.0).abs() < 1e-12, "+1 octave doubles");
        assert!((pitch_ratio(&r, 57, RATE) - 0.5).abs() < 1e-12, "-1 octave halves");
        r.transpose = 12;
        assert!((pitch_ratio(&r, 69, RATE) - 2.0).abs() < 1e-12, "transpose");
        r.transpose = 0;
        r.tune = 100;
        let semitone = 2f64.powf(1.0 / 12.0);
        assert!((pitch_ratio(&r, 69, RATE) - semitone).abs() < 1e-9, "100 cents = 1 semi");
        r.tune = 0;
        // Sample recorded at 24 kHz played on a 48 kHz engine: half speed.
        let mut r2 = region(Arc::new(SampleData { channels: 1, rate: 24_000, data: vec![0.0; 10] }));
        r2.pitch_keycenter = 60;
        assert!((pitch_ratio(&r2, 60, RATE) - 0.5).abs() < 1e-12, "rate factor folds in");
    }

    // ---- region lookup ----

    #[test]
    fn region_lookup_by_key_and_velocity() {
        let s = sine_sample(440.0, RATE, 0.01, 1.0);
        let mut a = region(s.clone());
        a.lokey = 60;
        a.hikey = 65;
        a.lovel = 1;
        a.hivel = 90;
        let mut b = region(s.clone());
        b.lokey = 60;
        b.hikey = 65;
        b.lovel = 91;
        b.hivel = 127;
        let mut c = region(s);
        c.lokey = 66;
        c.hikey = 127;
        let regions = vec![a, b, c];
        assert_eq!(find_region(&regions, 62, 50), Some(0), "soft layer");
        assert_eq!(find_region(&regions, 62, 127), Some(1), "hard layer");
        assert_eq!(find_region(&regions, 70, 50), Some(2));
        assert_eq!(find_region(&regions, 50, 64), None, "unmapped key");
    }

    // ---- voice allocation / stealing ----

    #[test]
    fn voice_stealing_is_oldest_and_deterministic() {
        let regions = vec![region(sine_sample(440.0, RATE, 1.0, 1.0))];
        let mut pool = VoicePool::new(3);
        for key in [60, 61, 62] {
            assert!(pool.note_on(&regions, key, 100, RATE, HOLD_MANUAL).is_some());
        }
        assert_eq!(pool.active_keys(), vec![60, 61, 62]);
        // Pool full: 63 steals the OLDEST (60).
        pool.note_on(&regions, 63, 100, RATE, HOLD_MANUAL).unwrap();
        assert_eq!(pool.active_keys(), vec![61, 62, 63]);
        // Next steal takes 61 — strictly age-ordered, run after run.
        pool.note_on(&regions, 64, 100, RATE, HOLD_MANUAL).unwrap();
        assert_eq!(pool.active_keys(), vec![62, 63, 64]);
        // Unmapped velocity/key never claims a voice.
        let mut narrow = region(sine_sample(440.0, RATE, 0.01, 1.0));
        narrow.lokey = 10;
        narrow.hikey = 10;
        assert!(pool.note_on(&[narrow], 60, 100, RATE, HOLD_MANUAL).is_none());
        assert_eq!(pool.active_keys(), vec![62, 63, 64], "failed note_on steals nothing");
    }

    #[test]
    fn freed_voices_are_reused_before_stealing() {
        let regions = vec![region(sine_sample(440.0, RATE, 1.0, 1.0))];
        let mut pool = VoicePool::new(2);
        pool.note_on(&regions, 60, 127, RATE, HOLD_MANUAL);
        pool.note_on(&regions, 62, 127, RATE, HOLD_MANUAL);
        pool.note_off(60);
        // Render long enough for the release (10 ms) to finish.
        let mut out = stereo_out(1024);
        pool.render(&regions, &mut out, 2, 0, 1024);
        assert_eq!(pool.active_count(), 1);
        pool.note_on(&regions, 64, 127, RATE, HOLD_MANUAL);
        assert_eq!(pool.active_keys(), vec![62, 64], "62 was never stolen");
    }

    // ---- envelope shape ----

    #[test]
    fn envelope_attack_and_release_are_linear_ar() {
        // DC sample of 1.0 so output == envelope directly.
        let dc = Arc::new(SampleData {
            channels: 1,
            rate: RATE,
            data: vec![1.0; RATE as usize],
        });
        let mut r = region(dc);
        r.ampeg_attack = 0.001; // 48 frames
        r.ampeg_release = 0.002; // 96 frames
        let regions = vec![r];
        let mut pool = VoicePool::new(1);
        pool.note_on(&regions, 69, 127, RATE, HOLD_MANUAL);

        let attack_frames = 48;
        let mut out = stereo_out(attack_frames * 2);
        pool.render(&regions, &mut out, 2, 0, attack_frames * 2);
        let l: Vec<f32> = out.iter().step_by(2).copied().collect();
        // Linear ramp: halfway point ~0.5, end of attack ~1.0, then sustain.
        assert!((l[attack_frames / 2 - 1] - 0.5).abs() < 0.03, "mid-attack {}", l[23]);
        assert!((l[attack_frames - 1] - 1.0).abs() < 0.03, "attack peak");
        assert!((l[attack_frames + 10] - 1.0).abs() < 1e-6, "sustain holds 1.0");

        pool.note_off(69);
        let release_frames = 96;
        let mut out2 = stereo_out(release_frames + 8);
        pool.render(&regions, &mut out2, 2, 0, release_frames + 8);
        let l2: Vec<f32> = out2.iter().step_by(2).copied().collect();
        assert!((l2[release_frames / 2 - 1] - 0.5).abs() < 0.03, "mid-release");
        assert!(l2[release_frames + 4].abs() < 1e-6, "silent after release");
        assert_eq!(pool.active_count(), 0, "voice freed after release");
    }

    #[test]
    fn hold_frames_auto_release() {
        let regions = vec![region(sine_sample(440.0, RATE, 1.0, 1.0))];
        let mut pool = VoicePool::new(1);
        pool.note_on(&regions, 69, 127, RATE, 100);
        let mut out = stereo_out(2048);
        pool.render(&regions, &mut out, 2, 0, 2048);
        assert_eq!(pool.active_count(), 0, "auto-released and finished");
    }

    // ---- rendered pitch accuracy ----

    #[test]
    fn rendered_pitch_matches_equal_temperament() {
        // 440 Hz sine rooted at A4 (69); play C5 (72) => 523.25 Hz.
        let regions = vec![region(sine_sample(440.0, RATE, 1.0, 0.9))];
        let mut pool = VoicePool::new(4);
        pool.note_on(&regions, 72, 127, RATE, HOLD_MANUAL);
        let frames = 24_000;
        let mut out = stereo_out(frames);
        pool.render(&regions, &mut out, 2, 0, frames);
        let mono: Vec<f32> = out.iter().step_by(2).copied().collect();
        // Skip the (tiny) attack, analyze the steady part.
        let est = estimate_freq(&mono[1000..], RATE, 200.0, 1000.0);
        let target = 440.0 * (3.0f64 / 12.0).exp2();
        assert!(
            (est - target).abs() / target < 0.01,
            "estimated {est:.2} Hz, want {target:.2} Hz"
        );
        // And the root reproduces the source frequency.
        pool.kill_all();
        pool.note_on(&regions, 69, 127, RATE, HOLD_MANUAL);
        let mut out2 = stereo_out(frames);
        pool.render(&regions, &mut out2, 2, 0, frames);
        let mono2: Vec<f32> = out2.iter().step_by(2).copied().collect();
        let est2 = estimate_freq(&mono2[1000..], RATE, 200.0, 1000.0);
        assert!((est2 - 440.0).abs() / 440.0 < 0.01, "root {est2:.2} != 440");
    }

    #[test]
    fn loop_continuous_sustains_past_sample_end() {
        // 100-frame ramp with a loop over [50, 90).
        let data: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        let s = Arc::new(SampleData { channels: 1, rate: RATE, data });
        let mut r = region(s);
        r.looped = true;
        r.loop_start = 50;
        r.loop_end = 90;
        let regions = vec![r];
        let mut pool = VoicePool::new(1);
        pool.note_on(&regions, 69, 127, RATE, HOLD_MANUAL);
        let frames = 1000; // 10x the sample length
        let mut out = stereo_out(frames);
        pool.render(&regions, &mut out, 2, 0, frames);
        assert_eq!(pool.active_count(), 1, "looped voice keeps sounding");
        let tail = &out[(frames - 10) * 2..];
        assert!(tail.iter().any(|s| s.abs() > 0.3), "loop region audio present");
        // Without a loop the same voice dies at the sample end.
        let regions2 = vec![region(Arc::new(SampleData {
            channels: 1,
            rate: RATE,
            data: vec![0.5; 100],
        }))];
        let mut pool2 = VoicePool::new(1);
        pool2.note_on(&regions2, 69, 127, RATE, HOLD_MANUAL);
        let mut out2 = stereo_out(200);
        pool2.render(&regions2, &mut out2, 2, 0, 200);
        assert_eq!(pool2.active_count(), 0, "one-shot ends");
    }

    #[test]
    fn stereo_regions_and_pan_gains_apply() {
        let mut data = Vec::new();
        for _ in 0..64 {
            data.push(0.5); // L
            data.push(-0.25); // R
        }
        let s = Arc::new(SampleData { channels: 2, rate: RATE, data });
        let mut r = region(s);
        r.gain_l = 1.0;
        r.gain_r = 0.5;
        r.ampeg_attack = 0.0; // instant
        let regions = vec![r];
        let mut pool = VoicePool::new(1);
        pool.note_on(&regions, 69, 127, RATE, HOLD_MANUAL);
        let mut out = stereo_out(16);
        pool.render(&regions, &mut out, 2, 0, 16);
        // Frame 4 well inside instant-attack sustain.
        assert!((out[8] - 0.5).abs() < 0.01, "left channel + gain_l");
        assert!((out[9] + 0.125).abs() < 0.01, "right channel * gain_r 0.5");
    }
}
