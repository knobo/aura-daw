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
    let ref_hz = target.unwrap_or_else(|| midi_to_hz(hz_to_midi(med).round()));
    let errs: Vec<f32> = hzs.iter().map(|h| cents_between(*h, ref_hz)).collect();
    let abs: Vec<f32> = errs.iter().map(|c| c.abs()).collect();
    let mean = errs.iter().sum::<f32>() / errs.len() as f32;
    let within = |c: f32| abs.iter().filter(|e| **e <= c).count() as f32 / abs.len() as f32;

    println!(
        "  median pitch    {:.2} Hz  ({}, {:+.1}¢)",
        med,
        note_name(hz_to_midi(med).round() as i32),
        cents_between(med, midi_to_hz(hz_to_midi(med).round()))
    );
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
    println!(
        "  clarity         median {:.2}",
        median(&voiced.iter().map(|f| f.clarity).collect::<Vec<_>>())
    );
    Ok(())
}
