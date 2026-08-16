//! The Pitch Coach phase 1 checkpoint (spec R3), with no UI in the way.
//!
//! Opens the default capture device, runs the real live chain — the same
//! [`PitchTap`] the audio callback drives and the same [`PitchWorker`] the
//! engine spawns — and prints what it hears: detected Hz, the nearest note,
//! the error in cents, and how much of the take was voiced at all.
//!
//! ```sh
//! cargo run --example pitch_check              # 10 s, nearest note
//! cargo run --example pitch_check -- 20 A3     # 20 s against a named target
//! cargo run --example pitch_check -- 10 220    # or a target in Hz
//! ```
//!
//! What this does NOT cover: the Tauri command layer. `pitch_listen_start`,
//! `pitch_subscribe` and the 60 Hz batching in `Control` are not exercised
//! here — this is the detector against a real microphone and a real voice,
//! which is what R3 asks to see before any panel is built.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aura_lib::audio::pitch::PitchFrame;
use aura_lib::audio::pitch_thread::{pitch_channel, spawn_pitch_worker};
use aura_lib::audio::yin::{cents_between, hz_to_midi, midi_to_hz};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// MIDI number -> `A3`-style name. MIDI 60 is C4, so the octave is
/// `midi / 12 - 1`.
fn note_name(midi: i32) -> String {
    let pc = midi.rem_euclid(12) as usize;
    format!("{}{}", NOTE_NAMES[pc], midi.div_euclid(12) - 1)
}

/// `A3`, `C#4`, `Bb2` -> MIDI number.
fn parse_note(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    let step = match b.first()?.to_ascii_uppercase() {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => return None,
    };
    let mut i = 1;
    let mut accidental = 0;
    while i < b.len() && (b[i] == b'#' || b[i] == b'b') {
        accidental += if b[i] == b'#' { 1 } else { -1 };
        i += 1;
    }
    let octave: i32 = s[i..].parse().ok()?;
    Some((octave + 1) * 12 + step + accidental)
}

/// Middle value of a slice, without sorting the caller's data.
fn median(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[s.len() / 2]
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let secs: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(10);
    // A target is optional: with none, every frame is scored against
    // whichever note it is nearest, which is what you want when checking
    // "does it track my voice" rather than "am I in tune".
    let target: Option<f32> = args.get(1).and_then(|s| {
        parse_note(s)
            .map(|m| midi_to_hz(m as f32))
            .or_else(|| s.parse::<f32>().ok())
    });
    if args.len() > 1 && target.is_none() {
        return Err(format!(
            "could not read a note or a frequency from {:?}",
            args[1]
        ));
    }

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("no default input device")?;
    let name = device.name().unwrap_or_else(|_| "<unnamed>".into());
    let cfg = device.default_input_config().map_err(|e| e.to_string())?;
    if cfg.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!(
            "unsupported input sample format {:?} (the engine is f32-only too)",
            cfg.sample_format()
        ));
    }
    let rate = cfg.sample_rate().0;
    let channels = cfg.channels().max(1) as usize;

    let active = Arc::new(AtomicBool::new(true));
    let (mut tap, worker, mut frames_rx) = pitch_channel(rate, active);
    let _worker = spawn_pitch_worker(worker).map_err(|e| e.to_string())?;

    // The callback owns the tap, exactly as `InputCb` does. `pos` stands in
    // for the transport position the engine passes.
    let mut pos = 0u64;
    let stream = device
        .build_input_stream(
            &cfg.into(),
            move |data: &[f32], _| {
                tap.process(data, channels, pos);
                pos += (data.len() / channels) as u64;
            },
            |e| eprintln!("input stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;

    println!("device : {name}");
    println!("format : {rate} Hz, {channels} ch");
    match target {
        Some(hz) => println!(
            "target : {:.2} Hz ({})",
            hz,
            note_name(hz_to_midi(hz).round() as i32)
        ),
        None => println!("target : nearest note to whatever you sing"),
    }
    println!("\nSing or play a steady note for {secs} s.\n");
    println!("   time   voiced   detected      note    error   clarity");
    println!("  ─────────────────────────────────────────────────────");

    let start = Instant::now();
    let mut all: Vec<PitchFrame> = Vec::new();
    let mut window: Vec<PitchFrame> = Vec::new();
    let mut last_line = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        while let Ok(f) = frames_rx.pop() {
            all.push(f);
            window.push(f);
        }
        if last_line.elapsed() >= Duration::from_millis(500) {
            last_line = Instant::now();
            let voiced: Vec<f32> = window.iter().filter(|f| f.voiced).map(|f| f.hz).collect();
            let frac = match window.is_empty() {
                true => 0.0,
                false => voiced.len() as f32 / window.len() as f32,
            };
            if voiced.is_empty() {
                println!(
                    "  {:5.1}s   {:5.0}%          —         —        —         —",
                    start.elapsed().as_secs_f32(),
                    frac * 100.0
                );
            } else {
                let hz = median(&voiced);
                let midi = hz_to_midi(hz);
                let near = midi.round() as i32;
                let cents = cents_between(hz, midi_to_hz(near as f32));
                let clarity = median(
                    &window
                        .iter()
                        .filter(|f| f.voiced)
                        .map(|f| f.clarity)
                        .collect::<Vec<_>>(),
                );
                println!(
                    "  {:5.1}s   {:5.0}%   {:8.2} Hz   {:>5}   {:+6.1}¢    {:5.2}",
                    start.elapsed().as_secs_f32(),
                    frac * 100.0,
                    hz,
                    note_name(near),
                    cents,
                    clarity
                );
            }
            window.clear();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(stream);

    let voiced: Vec<&PitchFrame> = all.iter().filter(|f| f.voiced).collect();
    println!("\n  ── summary ──────────────────────────────────────────");
    println!(
        "  frames          {} ({:.0} Hz)",
        all.len(),
        all.len() as f32 / secs as f32
    );
    if all.is_empty() {
        println!("\n  Nothing arrived. The device opened but produced no frames.");
        return Ok(());
    }
    println!(
        "  voiced          {} ({:.1}%)",
        voiced.len(),
        100.0 * voiced.len() as f32 / all.len() as f32
    );
    if voiced.is_empty() {
        println!("\n  Nothing was voiced. Check the input level and that the");
        println!("  right device is selected — the RMS gate is relative.");
        return Ok(());
    }

    let hzs: Vec<f32> = voiced.iter().map(|f| f.hz).collect();
    let med = median(&hzs);

    // How far the voiced pitch ranged. A held note stays inside a couple of
    // hundred cents; a glissando does not, and against a glissando any
    // statistic relative to ONE target note measures "how much of the time
    // were you elsewhere" — a real number that answers no question anyone
    // asked. So the fixed-target block below is printed only when the run
    // was actually steady, and the nearest-note block always is.
    let midis: Vec<f32> = voiced.iter().map(|f| f.midi).collect();
    let lo = midis.iter().cloned().fold(f32::MAX, f32::min);
    let hi = midis.iter().cloned().fold(f32::MIN, f32::max);
    let span_cents = (hi - lo) * 100.0;
    let steady = span_cents <= 150.0;

    // Time from the first voiced frame to the last: the part of the run
    // where you were actually making a sound. Leading silence while you
    // reach for the microphone should not read as a detector failure.
    let first = all.iter().position(|f| f.voiced).unwrap_or(0);
    let last = all.iter().rposition(|f| f.voiced).unwrap_or(0);
    let singing = &all[first..=last];
    let voiced_singing = singing.iter().filter(|f| f.voiced).count();

    println!(
        "  voiced (singing) {} of {} ({:.1}%) — from first sound to last",
        voiced_singing,
        singing.len(),
        100.0 * voiced_singing as f32 / singing.len() as f32
    );
    println!(
        "  range           {:.2}..{:.2} Hz  ({} .. {}, {:.0}¢ wide)",
        hzs.iter().cloned().fold(f32::MAX, f32::min),
        hzs.iter().cloned().fold(f32::MIN, f32::max),
        note_name(lo.round() as i32),
        note_name(hi.round() as i32),
        span_cents
    );
    println!(
        "  median pitch    {:.2} Hz  ({}, {:+.1}¢)",
        med,
        note_name(hz_to_midi(med).round() as i32),
        cents_between(med, midi_to_hz(hz_to_midi(med).round()))
    );

    // Tuning accuracy that does not care WHAT you sang: how far each frame
    // sits from the nearest semitone. Meaningful for a slide as well as a
    // held note, and it is what says whether the detector is landing on
    // notes at all.
    let near: Vec<f32> = voiced
        .iter()
        .map(|f| cents_between(f.hz, midi_to_hz(f.midi.round())))
        .collect();
    let near_abs: Vec<f32> = near.iter().map(|c| c.abs()).collect();
    println!(
        "  to nearest note median {:.1}¢, worst {:.1}¢",
        median(&near_abs),
        near_abs.iter().cloned().fold(0.0, f32::max)
    );

    match (steady, target) {
        (true, _) => {
            let ref_hz = target.unwrap_or_else(|| midi_to_hz(hz_to_midi(med).round()));
            let errs: Vec<f32> = hzs.iter().map(|h| cents_between(*h, ref_hz)).collect();
            let abs: Vec<f32> = errs.iter().map(|c| c.abs()).collect();
            let mean = errs.iter().sum::<f32>() / errs.len() as f32;
            let within = |c: f32| abs.iter().filter(|e| **e <= c).count() as f32 / abs.len() as f32;
            println!(
                "  reference       {:.2} Hz  ({})",
                ref_hz,
                note_name(hz_to_midi(ref_hz).round() as i32)
            );
            println!(
                "  error           mean {mean:+.1}¢, median {:+.1}¢",
                median(&errs)
            );
            println!(
                "  |error|         median {:.1}¢, worst {:.1}¢",
                median(&abs),
                abs.iter().cloned().fold(0.0, f32::max)
            );
            println!(
                "  within          {:.0}% ≤ 25¢, {:.0}% ≤ 50¢",
                within(25.0) * 100.0,
                within(50.0) * 100.0
            );
        }
        (false, Some(_)) => {
            println!("  reference       skipped — the pitch moved {span_cents:.0}¢, so error");
            println!("                  against one target note would only measure how");
            println!("                  long you spent away from it. Hold a note to get it.");
        }
        (false, None) => {}
    }
    println!(
        "  clarity         median {:.2}",
        median(&voiced.iter().map(|f| f.clarity).collect::<Vec<_>>())
    );
    Ok(())
}
