//! The RT gate: what a block of rendering is allowed to do to the allocator.
//!
//! `ARCHITECTURE.md` §2.1 forbids allocation on the audio thread, and until
//! now that rule has been prose — dossier 10 lists "nothing enforces the RT
//! rules mechanically" as gap 19. This file is the mechanism for this crate's
//! share of the path: a counting global allocator, and a test per claim.
//!
//! The first test is the one that makes the rest mean anything. An allocation
//! assertion in a binary that forgot to register the allocator passes
//! vacuously, and a green suite that cannot fail is worse than no suite — so
//! `the_guard_is_actually_armed` proves the counter sees a real `Box` before
//! anything else is asserted.

use aura_engine::automation::{AbsParamEvent, RampCursor};
use aura_engine::dsp::{apply_fader_into, fused_scalar, mix_post_into, multipass, Accum};
use aura_engine::jit::Kernels;
use aura_engine::metrics::{assert_no_alloc, counting_is_active, measure, AllocCounter, Telemetry};
use aura_engine::strip::{plan, PanQuad, Strip};
use aura_engine::sync::triple_buffer;

#[global_allocator]
static ALLOC: AllocCounter = AllocCounter::new();

const FRAMES: usize = 512;

/// Centre pan, equal-power — the −3 dB the app's `mixer::pan_gains(0.0)`
/// returns. Spelled as the constant rather than `0.7071` so the pan law here
/// is the same value the mixer's own tests assert.
const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;

const TRACKS: usize = 32;

fn signal() -> Vec<f32> {
    (0..FRAMES).flat_map(|i| [(i as f32 * 0.03).sin(), (i as f32 * 0.02).cos()]).collect()
}

fn fader_move() -> Vec<AbsParamEvent> {
    vec![
        AbsParamEvent { sample: 0, value: 1.0 },
        AbsParamEvent { sample: 211, value: 0.3 },
        AbsParamEvent { sample: 640, value: 0.8 },
        // Inside the 1024..1536 window every test renders, so the plans
        // under test really do split into several stretches — a no-alloc
        // claim proved only on one-segment plans would miss the loop that
        // walks them.
        AbsParamEvent { sample: 1180, value: 0.2 },
        AbsParamEvent { sample: 1390, value: 0.95 },
    ]
}

fn strips<'a>(ramp: &'a [AbsParamEvent]) -> Vec<Strip<'a>> {
    (0..TRACKS)
        .map(|n| Strip {
            gain: 0.3 + n as f32 * 0.01,
            ramp: if n % 3 == 0 { ramp } else { &[] },
            pan: if n % 2 == 0 {
                PanQuad { gl0: 0.98, gr0: 0.19, gl1: 0.19, gr1: 0.98 }
            } else {
                PanQuad { gl0: CENTRE, gr0: CENTRE, gl1: CENTRE, gr1: CENTRE }
            },
            audible: n % 8 != 7,
            pdc_delay: if n % 5 == 0 { 128 } else { 0 },
        })
        .collect()
}

#[test]
fn the_guard_is_actually_armed() {
    assert!(
        counting_is_active(),
        "AllocCounter is not the registered global allocator — every other \
         assertion in this file would pass vacuously"
    );
    // And it counts the right way round: an allocation is seen, and a
    // deallocation is not counted as one.
    let (_, stats) = measure(|| vec![0u8; 4096]);
    assert!(stats.allocs >= 1 && stats.bytes >= 4096, "{stats:?}");
}

#[test]
fn planning_a_block_does_not_allocate() {
    let ramp = fader_move();
    let all = strips(&ramp);
    // Warm: the plan is `Copy` and fixed-size, so there is nothing to warm —
    // which is the point being asserted.
    assert_no_alloc("strip::plan over 32 tracks", || {
        let mut segments = 0usize;
        let mut multi = 0usize;
        for s in &all {
            let p = plan(s, 1024, FRAMES, 0, FRAMES - 1);
            segments += p.segments().len();
            multi += usize::from(p.segments().len() > 1);
        }
        // Audible strips must all have planned something, and the automated
        // ones must have split — otherwise this measured the easy path.
        assert!(segments >= all.iter().filter(|s| s.audible).count(), "{segments} segments");
        assert!(multi > 0, "no plan split: the multi-segment path went unmeasured");
    });
}

#[test]
fn the_jit_kernels_do_not_allocate_while_rendering() {
    // Compiling DOES allocate — it maps pages and runs a register allocator.
    // That is why it happens here, on the control thread, before the
    // measured region, and never inside a callback.
    let kernels = Kernels::compile().expect("cranelift could not target this host");
    let ramp = fader_move();
    let all = strips(&ramp);
    let buf = signal();
    let mut post = vec![0.0; FRAMES * 2];
    let mut master = vec![0.0; FRAMES * 2];
    let mut acc = Accum::default();

    assert_no_alloc("plan + JIT kernels, 32 tracks", || {
        for s in &all {
            let p = plan(s, 1024, FRAMES, 0, FRAMES - 1);
            assert!(kernels.run(&p, &buf, &mut post, &mut acc));
            mix_post_into(&mut master, 0, 2, &post, FRAMES);
        }
    });
    // The master, not `post`: `post` is per-strip and the last strip in this
    // set is a muted one, so it ends the loop full of the silence that strip
    // correctly wrote. A no-op cannot be RT-safe by doing nothing.
    assert!(master.iter().any(|&x| x != 0.0), "nothing reached the master");
    assert!(acc.pk_l > 0.0 && acc.pk_r > 0.0, "the meters saw nothing");
}

#[test]
fn the_scalar_paths_do_not_allocate_either() {
    // The fallback has to hold the same line as the fast path, or falling
    // back becomes its own xrun.
    let ramp = fader_move();
    let all = strips(&ramp);
    let buf = signal();
    let mut post = vec![0.0; FRAMES * 2];
    let mut acc = Accum::default();

    assert_no_alloc("mixer::apply_fader_into port", || {
        for s in &all {
            apply_fader_into(
                s,
                &buf,
                FRAMES,
                1024,
                0,
                FRAMES - 1,
                &mut RampCursor::new(),
                &mut post,
                &mut acc,
            );
        }
    });
    assert_no_alloc("fused_scalar", || {
        for s in &all {
            let p = plan(s, 1024, FRAMES, 0, FRAMES - 1);
            assert!(fused_scalar(&p, &buf, &mut post, &mut acc));
        }
    });
    assert_no_alloc("multipass", || {
        for s in &all {
            multipass(s, &buf, FRAMES, 1024, 0, FRAMES - 1, &mut post, &mut acc);
        }
    });
}

#[test]
fn publishing_and_reading_a_triple_buffer_does_not_allocate() {
    // `new` allocates once — three values in one Arc. Nothing after it does.
    let (mut tx, mut rx) = triple_buffer([0.0f32; 64]);
    assert_no_alloc("triple buffer publish/read, 1000 rounds", || {
        for n in 0..1000 {
            tx.slot()[n % 64] = n as f32;
            tx.publish();
            let v = rx.read();
            // Every value the writer ever put in slot 0 is a non-negative
            // `n as f32`. Reading anything else would mean the reader saw a
            // slot mid-write — the race this buffer exists to prevent.
            assert!(v[0] >= 0.0, "reader saw a torn slot: {}", v[0]);
        }
    });
}

#[test]
fn block_telemetry_does_not_allocate() {
    let mut t = Telemetry::new(48_000);
    assert_no_alloc("Telemetry::start/finish, 1000 blocks", || {
        for _ in 0..1000 {
            let b = t.start(FRAMES as u32);
            let _ = t.finish(b);
        }
    });
    let s = t.snapshot();
    assert_eq!(s.blocks, 1000);
    assert_eq!(s.xruns, 0, "1000 empty blocks cannot miss a 10.7 ms deadline");
}

#[test]
fn a_whole_callback_block_allocates_nothing() {
    // The integration claim, in the shape a real callback has: adopt the
    // published kernel table, plan every strip, render, publish telemetry
    // back. Everything that allocates has already happened.
    let kernels = Kernels::compile().expect("cranelift could not target this host");
    let (mut ktx, mut krx) = aura_engine::sync::from_slots([None, None, None]);
    ktx.write(Some(kernels));

    let (mut ttx, mut trx) = triple_buffer(aura_engine::metrics::Snapshot::default());
    let mut telemetry = Telemetry::new(48_000);
    let ramp = fader_move();
    let all = strips(&ramp);
    let buf = signal();
    let mut post = vec![0.0; FRAMES * 2];
    let mut master = vec![0.0; FRAMES * 2];

    let (_, stats) = measure(|| {
        for block in 0..64u64 {
            let table = krx.read().as_ref().expect("published before the first block");
            let started = telemetry.start(FRAMES as u32);
            master.fill(0.0);
            let mut acc = Accum::default();
            for s in &all {
                let p = plan(s, block * FRAMES as u64, FRAMES, 0, FRAMES - 1);
                assert!(table.run(&p, &buf, &mut post, &mut acc));
                mix_post_into(&mut master, 0, 2, &post, FRAMES);
            }
            let _late = telemetry.finish(started);
            ttx.write(telemetry.snapshot());
        }
    });
    assert_eq!(
        (stats.allocs, stats.deallocs),
        (0, 0),
        "64 blocks of 32 tracks touched the allocator: {stats:?}"
    );
    assert_eq!(trx.read().blocks, 64);
}
