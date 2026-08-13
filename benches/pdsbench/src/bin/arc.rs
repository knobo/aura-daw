// Arc refcount traffic + drop-cascade costs.
use std::hint::black_box;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const IT: usize = 50_000_000;

fn main() {
    println!("size_of::<Arc<u64>>()   = {}", size_of::<Arc<u64>>());
    println!("Arc header (strong+weak) = {} bytes on this target", 2 * size_of::<usize>());

    // ---- uncontended Arc clone+drop ----
    let a: Arc<u64> = Arc::new(7);
    let t = Instant::now();
    for _ in 0..IT {
        black_box(a.clone());
    }
    let arc_ns = t.elapsed().as_nanos() as f64 / IT as f64;
    println!("Arc::clone + drop (uncontended): {:.3} ns/op", arc_ns);

    let r: Rc<u64> = Rc::new(7);
    let t = Instant::now();
    for _ in 0..IT {
        black_box(r.clone());
    }
    let rc_ns = t.elapsed().as_nanos() as f64 / IT as f64;
    println!("Rc::clone  + drop:               {:.3} ns/op  ({:.2}x cheaper than Arc)", rc_ns, arc_ns / rc_ns);

    // plain non-atomic counter for reference
    let c = AtomicUsize::new(0);
    let t = Instant::now();
    for _ in 0..IT {
        black_box(c.fetch_add(1, Ordering::Relaxed));
    }
    println!("bare atomic fetch_add(Relaxed):  {:.3} ns/op", t.elapsed().as_nanos() as f64 / IT as f64);

    // ---- contended: 4 threads hammering the same Arc ----
    for threads in [1usize, 2, 4, 8] {
        let a = Arc::new(0u64);
        let n = 5_000_000usize;
        let t = Instant::now();
        std::thread::scope(|s| {
            for _ in 0..threads {
                let a = a.clone();
                s.spawn(move || {
                    for _ in 0..n {
                        black_box(a.clone());
                    }
                });
            }
        });
        let total = (threads * n) as f64;
        println!(
            "Arc::clone+drop on ONE shared Arc, {} threads: {:.2} ns/op (aggregate)",
            threads,
            t.elapsed().as_nanos() as f64 / total
        );
    }

    // ---- drop cascade: how long to free a large persistent structure ----
    let n = 1_000_000usize;
    let v: imbl::Vector<u64> = (0..n as u64).collect();

    let t = Instant::now();
    let clone = v.clone();
    println!("\nclone of 1e6 imbl::Vector: {:?}", t.elapsed());
    let t = Instant::now();
    drop(clone);
    println!("drop of that clone (root refcount--): {:?}", t.elapsed());

    // build 10k versions, then time the teardown
    let mut hist: Vec<imbl::Vector<u64>> = Vec::with_capacity(10_000);
    let mut cur = v.clone();
    let t = Instant::now();
    for i in 0..10_000usize {
        cur.set((i * 97) % n, 3);
        hist.push(cur.clone());
    }
    println!("build 10k retained versions: {:?}", t.elapsed());

    let t = Instant::now();
    drop(hist);
    println!("DROP 10k retained versions:  {:?}   <-- the cascade", t.elapsed());

    let t = Instant::now();
    drop(cur);
    drop(v);
    println!("drop the last two roots (frees 1e6 elements): {:?}", t.elapsed());

    // For comparison: dropping 10k plain Vec snapshots is impossible (160 GB),
    // so drop 100 of a 1e6 Vec instead.
    let base: Vec<u64> = (0..n as u64).collect();
    let mut snaps: Vec<Vec<u64>> = (0..100).map(|_| base.clone()).collect();
    let t = Instant::now();
    snaps.clear();
    println!("drop 100 full Vec snapshots (800 MB): {:?}", t.elapsed());
    black_box(&base);
}
