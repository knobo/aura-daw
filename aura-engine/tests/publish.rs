//! Handing compiled kernels to the audio thread, and getting them back.
//!
//! The JIT's awkward fact is that `JITModule::free_memory` is `unsafe`:
//! nothing inside cranelift can prove no compiled code is still executing, so
//! dropping a module **leaks its executable pages**. A DAW that recompiles
//! kernels whenever the graph changes shape would leak a few KiB per rebuild,
//! forever.
//!
//! The triple buffer is what turns that into a solvable problem, and the
//! mechanism is the same one that makes it RT-safe in the first place: the
//! writer does not *give* a value away, it *swaps* — so two publishes later
//! the retired table is back in the writer's own slot, on the control thread,
//! after the reader has provably moved on. That is the one place where
//! `release`'s safety condition can be discharged.
//!
//! Neither `ArcSwap` nor a queue gets you there. `ArcSwap` puts the last
//! `Arc` release on whichever thread happens to drop it last — which can be
//! the audio one. A queue can hand the value back, and that is what
//! `audio::engine`'s retire ring does for graphs; it needs a second ring and
//! the rule that the reader may only adopt while a retire slot is free.

use aura_engine::dsp::Accum;
use aura_engine::jit::Kernels;
use aura_engine::strip::{plan, PanQuad, Strip};
use aura_engine::sync::from_slots;

/// Centre pan, equal-power — the −3 dB the app's `mixer::pan_gains(0.0)`
/// returns. Spelled as the constant rather than `0.7071` so the pan law here
/// is the same value the mixer's own tests assert.
const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn table() -> Kernels {
    Kernels::compile().expect("cranelift could not target this host")
}

fn render_one_block(k: &Kernels) -> Vec<f32> {
    let frames = 128;
    let buf: Vec<f32> = (0..frames * 2).map(|i| (i as f32 * 0.05).sin()).collect();
    let s = Strip {
        gain: 0.8,
        ramp: &[],
        pan: PanQuad { gl0: CENTRE, gr0: CENTRE, gl1: CENTRE, gr1: CENTRE },
        audible: true,
        pdc_delay: 0,
    };
    let p = plan(&s, 0, frames, 0, frames - 1);
    let mut post = vec![0.0; frames * 2];
    let mut acc = Accum::default();
    assert!(k.run(&p, &buf, &mut post, &mut acc));
    post
}

#[test]
fn a_kernel_table_survives_the_crossing_and_renders() {
    let (mut tx, mut rx) = from_slots([None, None, None]);
    assert!(rx.read().is_none(), "nothing published yet");
    tx.write(Some(table()));
    let published = rx.read().as_ref().expect("the table crossed");
    assert!(render_one_block(published).iter().any(|&x| x != 0.0));
}

#[test]
fn the_audio_thread_actually_runs_the_published_kernels() {
    // The crossing is only worth anything if the pointer is still callable on
    // the other thread: JIT pages are process-wide, but `Kernels` is `Send`
    // and not `Sync` precisely so that this is the only shape it happens in.
    let (mut tx, mut rx) = from_slots([None, None, None]);
    tx.write(Some(table()));
    let rendered = std::thread::spawn(move || {
        let k = rx.read().as_ref().expect("published before the thread started");
        render_one_block(k)
    })
    .join()
    .expect("the audio-thread stand-in panicked");
    assert!(rendered.iter().any(|&x| x != 0.0));
}

#[test]
fn a_retired_table_comes_back_to_the_writer_and_can_be_released() {
    let (mut tx, mut rx) = from_slots([None, None, None]);

    // Publish three tables, reading between each — the reader is what frees
    // the slots up again.
    for _ in 0..3 {
        tx.write(Some(table()));
        assert!(rx.read().is_some());
    }

    // By now the writer's own slot holds a table it published earlier and the
    // reader has since moved off. THIS is the moment `release`'s safety
    // condition holds: nothing can reach that table any more.
    let retired = tx.replace(None);
    let retired = retired.expect("a triple buffer recycles: the writer gets its slots back");
    // SAFETY: two publishes and two reads have happened since this table was
    // handed over, so the reader's index has moved off it twice; no kernel
    // from it is running or reachable.
    unsafe { retired.release() };

    // And the live table is untouched by its predecessor's release.
    let live = rx.read().as_ref().expect("the latest table is still published");
    assert!(render_one_block(live).iter().any(|&x| x != 0.0));
}

#[test]
fn a_reader_that_never_looks_costs_the_writer_nothing() {
    // The failure mode of the queue shape: a callback that stops popping
    // blocks the control thread's next publish. A triple buffer cannot —
    // the writer just overwrites its own slot, so a stopped audio device
    // never stalls a rebuild.
    let (mut tx, _rx) = from_slots([0u64, 0, 0]);
    for n in 1..10_000u64 {
        tx.write(n);
    }
    // Nothing to assert but the absence of a hang; the value is checked by
    // the burst test in the unit suite.
}
