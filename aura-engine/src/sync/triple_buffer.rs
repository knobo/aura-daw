//! `TripleBuffer<T>` — wait-free single-producer/single-consumer publish of a
//! whole preallocated value.
//!
//! ## Why this and not the primitives the engine already has
//!
//! `src-tauri` already carries two lock-free seams, and neither one covers
//! this case:
//!
//! * **rtrb ownership transfer** (`audio::rt::GraphPtr`) hands a `Box` across
//!   and relies on the RT side pushing the old one back for the control
//!   thread to free. Correct, but it is a *queue*: the reader sees every
//!   version that was pushed, and the writer can only publish while a retire
//!   slot is free. For "the RT thread wants the LATEST value and nothing
//!   else", a queue is the wrong shape — a burst of five updates makes the
//!   callback pop and retire five graphs to reach the current one.
//! * **Atomics** (`audio::rt::ParamTable`) are per-field. A struct of several
//!   fields that must be read as one coherent snapshot cannot be a pile of
//!   `AtomicU32`s; `ParamTable::gain_seq` exists precisely because two of its
//!   fields had to be made coherent, and a seqlock makes the *reader* retry —
//!   unbounded work on the RT thread.
//!
//! A triple buffer is the third shape: latest-value, coherent, and bounded to
//! **one atomic swap on each side**. Three slots is what makes that possible —
//! the reader holds one, the writer holds one, and one is always free to be
//! the handoff point, so neither side ever waits for the other.
//!
//! ## The property that matters on the audio thread
//!
//! **No `T` is ever constructed, dropped, or reference-counted by the
//! consumer.** The writer publishes by swapping an index; the slot it gets
//! back is a *previously published buffer it now owns again* and can overwrite
//! in place. So a `T` whose destructor is expensive or forbidden on the RT
//! thread — a JIT module owning executable pages, a `Vec` of compiled ramps,
//! a plugin handle — is allocated and freed on the control thread only, with
//! no `Arc` clone in the callback (`ArcSwap` would put the *last* release of
//! a retired value on whichever thread happens to drop it last, which can be
//! the RT one).
//!
//! ## The algorithm
//!
//! One `AtomicU32` holds the index of the handoff slot in its low two bits
//! plus a "dirty" bit meaning *the writer published since the reader last
//! looked*.
//!
//! * `publish`: swap `write_idx | DIRTY` in; the old index becomes the
//!   writer's next slot.
//! * `read`: if dirty, swap the reader's index in (clean); the old index
//!   becomes the reader's slot. Otherwise keep reading the current slot.
//!
//! Both sides are one RMW on one word, so both are wait-free, and the three
//! indices are a permutation at every point: whatever one side swaps out, the
//! other swapped in. Nothing else can hold that slot.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

const IDX_MASK: u32 = 0b11;
const DIRTY: u32 = 0b100;

struct Shared<T> {
    /// Three slots. Exactly one is reachable from the writer, one from the
    /// reader, and one from `handoff` — see the module doc's permutation
    /// invariant.
    slots: [UnsafeCell<T>; 3],
    /// `index | DIRTY`.
    handoff: AtomicU32,
}

// SAFETY: the permutation invariant means the writer's `&mut` and the
// reader's `&` never name the same slot. `Send`/`Sync` on the shared half is
// what lets the two ends live on different threads; `Input`/`Output` are the
// only ways to reach a slot, and each of them is single-threaded by
// construction (`&mut self` on both sides).
unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send> Sync for Shared<T> {}

/// Writer end. Control thread.
pub struct Input<T> {
    shared: Arc<Shared<T>>,
    idx: u32,
}

/// Reader end. Audio thread.
pub struct Output<T> {
    shared: Arc<Shared<T>>,
    idx: u32,
}

/// Build a triple buffer from three values.
///
/// This is the **only** allocation in the type's whole lifetime: three `T` in
/// one `Arc`. `publish`/`read` never allocate, and never free.
///
/// Three separate values rather than three clones because the interesting
/// payloads are not `Clone`: a compiled JIT module, a plugin handle, a set of
/// preallocated buffers. `Option<T>` with two `None`s is the usual shape —
/// the slots that have not been filled yet cost nothing.
pub fn from_slots<T>(slots: [T; 3]) -> (Input<T>, Output<T>) {
    let [a, b, c] = slots;
    let shared = Arc::new(Shared {
        slots: [UnsafeCell::new(a), UnsafeCell::new(b), UnsafeCell::new(c)],
        // Slot 2 is the handoff, clean: the reader has not been given
        // anything new, so its first `read` returns its own slot 1.
        handoff: AtomicU32::new(2),
    });
    (Input { shared: Arc::clone(&shared), idx: 0 }, Output { shared, idx: 1 })
}

/// [`from_slots`] with three clones of one value.
pub fn triple_buffer<T: Clone>(initial: T) -> (Input<T>, Output<T>) {
    from_slots([initial.clone(), initial.clone(), initial])
}

impl<T> Input<T> {
    /// The slot the next `publish` will hand over — write into it in place.
    ///
    /// It holds whatever this end last had here (two publishes ago, or the
    /// initial value), so a `T` that is expensive to build can be *updated*
    /// rather than reconstructed. Assigning a fresh `T` through this
    /// reference drops the old one **on this thread**, which is the point.
    pub fn slot(&mut self) -> &mut T {
        // SAFETY: `self.idx` is this end's exclusive slot until `publish`
        // swaps it away, and `&mut self` rules out a second reference from
        // this end.
        unsafe { &mut *self.shared.slots[self.idx as usize].get() }
    }

    /// Hand the current slot to the reader and take ownership of the one it
    /// leaves behind. One `swap`; no allocation, no drop, no spin.
    pub fn publish(&mut self) {
        let previous = self.shared.handoff.swap(self.idx | DIRTY, Ordering::AcqRel);
        self.idx = previous & IDX_MASK;
    }

    /// Convenience: overwrite the slot and publish it. The value that was in
    /// the slot is dropped **here**, on the writer's thread.
    pub fn write(&mut self, value: T) {
        *self.slot() = value;
        self.publish();
    }

    /// Swap a fresh value into the slot and hand back what was there, without
    /// publishing.
    ///
    /// This is how a payload whose destructor may not run on the audio thread
    /// gets reclaimed: two publishes after a value was handed over, the writer
    /// owns it again and can dispose of it deliberately. See
    /// `jit::Kernels::release`.
    pub fn replace(&mut self, value: T) -> T {
        std::mem::replace(self.slot(), value)
    }
}

impl<T> Output<T> {
    /// Whether a `read` would move to a newer value. One relaxed-ish load;
    /// safe to call from the audio thread.
    pub fn updated(&self) -> bool {
        self.shared.handoff.load(Ordering::Acquire) & DIRTY != 0
    }

    /// The latest published value, or the previous one when nothing new was
    /// published. Wait-free: at most one `swap`, and the same borrow is
    /// returned repeatedly when the writer is idle.
    pub fn read(&mut self) -> &T {
        if self.updated() {
            // Swapping in a CLEAN index is what marks the update consumed.
            let previous = self.shared.handoff.swap(self.idx, Ordering::AcqRel);
            self.idx = previous & IDX_MASK;
        }
        // SAFETY: `self.idx` is this end's exclusive slot — see the module
        // doc. The returned borrow keeps `&mut self` locked out, so the next
        // `read` cannot move the index while it is alive.
        unsafe { &*self.shared.slots[self.idx as usize].get() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    #[test]
    fn read_before_any_publish_sees_the_initial_value() {
        let (_i, mut o) = triple_buffer(7u32);
        assert_eq!(*o.read(), 7);
        assert!(!o.updated());
    }

    #[test]
    fn a_publish_is_visible_and_consumed_once() {
        let (mut i, mut o) = triple_buffer(0u32);
        i.write(1);
        assert!(o.updated());
        assert_eq!(*o.read(), 1);
        assert!(!o.updated(), "the swap in `read` is what clears dirty");
        assert_eq!(*o.read(), 1, "an idle writer means the same value again");
    }

    #[test]
    fn only_the_latest_of_a_burst_is_delivered() {
        let (mut i, mut o) = triple_buffer(0u32);
        for v in 1..=5 {
            i.write(v);
        }
        // The queue shape would make the reader pop five times to catch up;
        // this is why the primitive exists.
        assert_eq!(*o.read(), 5);
    }

    #[test]
    fn the_writer_gets_its_buffers_back_and_never_allocates_again() {
        // Every `T` here is tagged with the slot it was born in; after two
        // publishes the writer must be handed a slot it has already used,
        // which is the recycling property the RT contract needs.
        let (mut i, mut o) = triple_buffer(String::from("init"));
        let mut seen = Vec::new();
        for n in 0..6 {
            seen.push(i.slot().as_ptr());
            i.slot().clear();
            i.slot().push_str(&n.to_string());
            i.publish();
            let _ = o.read();
        }
        let distinct: std::collections::HashSet<_> = seen.iter().collect();
        assert!(
            distinct.len() <= 3,
            "six publishes touched {} buffers; a triple buffer has three",
            distinct.len()
        );
    }

    #[test]
    fn the_three_indices_stay_a_permutation() {
        let (mut i, mut o) = triple_buffer(0u32);
        // Interleave in every order that matters: publish-publish-read,
        // read-read-publish, and the alternating case.
        for step in 0..200u32 {
            if step % 3 != 2 {
                i.write(step);
            }
            if step % 4 != 3 {
                let _ = o.read();
            }
            let handoff = i.shared.handoff.load(Ordering::Acquire) & IDX_MASK;
            let mut idx = [i.idx, o.idx, handoff];
            idx.sort_unstable();
            assert_eq!(idx, [0, 1, 2], "step {step} lost the permutation");
        }
    }

    #[test]
    fn concurrent_reader_never_sees_a_torn_value() {
        // A two-field value where the fields must agree. A per-field atomic
        // pair would tear here; the whole point of the primitive is that
        // this cannot.
        #[derive(Clone, Copy, PartialEq, Debug)]
        struct Pair {
            a: u64,
            b: u64,
        }
        let (mut i, mut o) = triple_buffer(Pair { a: 0, b: 0 });
        let stop = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicUsize::new(0));
        let (stop_r, reads_r) = (Arc::clone(&stop), Arc::clone(&reads));
        let reader = std::thread::spawn(move || {
            while !stop_r.load(Ordering::Relaxed) {
                let v = *o.read();
                assert_eq!(v.a * 2, v.b, "torn read: {v:?}");
                reads_r.fetch_add(1, Ordering::Relaxed);
            }
        });
        for n in 1..50_000u64 {
            i.write(Pair { a: n, b: n * 2 });
        }
        stop.store(true, Ordering::Relaxed);
        reader.join().expect("reader panicked");
        assert!(reads.load(Ordering::Relaxed) > 0);
    }
}
