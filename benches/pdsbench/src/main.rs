// Benchmark: candidate storage strategies for DAW session history.
// Workload model: a MIDI event list of N events, 16 bytes each.
// Operations that matter:
//   (a) sequential scan  -> playback / render / export
//   (b) random index     -> UI hit-testing
//   (c) point update     -> "nudge one note"
//   (d) clone            -> "take a history snapshot"
//   (e) memory footprint

use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Debug)]
#[repr(C)]
struct Ev {
    tick: u32,
    kind: u8,
    key: u8,
    vel: u8,
    _pad: u8,
    dur: u32,
    val: f32,
}
impl Ev {
    fn new(i: u32) -> Self {
        Ev { tick: i * 7, kind: 1, key: (i % 88) as u8, vel: 100, _pad: 0, dur: 240, val: 0.5 }
    }
}

const CHUNK: usize = 4096;

/// Copy-on-write chunk rope: Vec<Arc<[Ev; CHUNK]>>-ish.
/// This is the "structural sharing at chunk granularity" candidate.
#[derive(Clone)]
struct Rope {
    chunks: Arc<Vec<Arc<Vec<Ev>>>>,
    len: usize,
}
impl Rope {
    fn build(n: usize) -> Self {
        let mut chunks = Vec::new();
        let mut i = 0usize;
        while i < n {
            let hi = (i + CHUNK).min(n);
            chunks.push(Arc::new((i..hi).map(|j| Ev::new(j as u32)).collect::<Vec<_>>()));
            i = hi;
        }
        Rope { chunks: Arc::new(chunks), len: n }
    }
    #[inline]
    fn get(&self, i: usize) -> &Ev {
        &self.chunks[i / CHUNK][i % CHUNK]
    }
    /// Point update, returning a NEW rope sharing every untouched chunk.
    fn set(&self, i: usize, e: Ev) -> Rope {
        let ci = i / CHUNK;
        let mut chunks = (*self.chunks).clone(); // Vec of Arcs: n/CHUNK pointer copies
        let mut c = (*chunks[ci]).clone(); // deep-copy ONE chunk
        c[i % CHUNK] = e;
        chunks[ci] = Arc::new(c);
        Rope { chunks: Arc::new(chunks), len: self.len }
    }
}

fn bench<F: FnMut() -> u64>(name: &str, iters: u32, mut f: F) {
    // warmup
    let mut sink = 0u64;
    sink = sink.wrapping_add(f());
    let t = Instant::now();
    for _ in 0..iters {
        sink = sink.wrapping_add(f());
    }
    let d = t.elapsed().as_nanos() as f64 / iters as f64;
    let unit = if d > 1_000_000.0 {
        format!("{:>12.3} ms", d / 1e6)
    } else if d > 1000.0 {
        format!("{:>12.3} us", d / 1e3)
    } else {
        format!("{:>12.1} ns", d)
    };
    println!("{:<52} {}   [sink {}]", name, unit, sink & 0xff);
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    println!("N = {} events of {} bytes = {:.1} MB raw\n", n, size_of::<Ev>(), (n * size_of::<Ev>()) as f64 / 1e6);

    // ---------- build ----------
    let t = Instant::now();
    let v: Vec<Ev> = (0..n).map(|i| Ev::new(i as u32)).collect();
    println!("build Vec                 {:>10.2} ms", t.elapsed().as_secs_f64() * 1e3);

    let t = Instant::now();
    let iv: imbl::Vector<Ev> = (0..n).map(|i| Ev::new(i as u32)).collect();
    println!("build imbl::Vector        {:>10.2} ms", t.elapsed().as_secs_f64() * 1e3);

    let t = Instant::now();
    let imv: im::Vector<Ev> = (0..n).map(|i| Ev::new(i as u32)).collect();
    println!("build im::Vector          {:>10.2} ms", t.elapsed().as_secs_f64() * 1e3);

    let t = Instant::now();
    let rv: rpds::Vector<Ev> = (0..n).map(|i| Ev::new(i as u32)).collect();
    println!("build rpds::Vector        {:>10.2} ms", t.elapsed().as_secs_f64() * 1e3);

    let t = Instant::now();
    let rope = Rope::build(n);
    println!("build Rope(4096 COW)      {:>10.2} ms", t.elapsed().as_secs_f64() * 1e3);

    // maps: object store, key = u32 id
    let t = Instant::now();
    let hm: std::collections::HashMap<u32, Ev> = (0..n as u32).map(|i| (i, Ev::new(i))).collect();
    println!("build std::HashMap        {:>10.2} ms", t.elapsed().as_secs_f64() * 1e3);

    let t = Instant::now();
    let ihm: imbl::HashMap<u32, Ev> = (0..n as u32).map(|i| (i, Ev::new(i))).collect();
    println!("build imbl::HashMap       {:>10.2} ms", t.elapsed().as_secs_f64() * 1e3);

    println!("\n--- (a) full sequential scan (sum ticks) --- lower is better");
    let it = 20;
    bench("Vec: iter().sum", it, || v.iter().map(|e| e.tick as u64).sum());
    bench("Rope: nested iter", it, || {
        rope.chunks.iter().flat_map(|c| c.iter()).map(|e| e.tick as u64).sum()
    });
    bench("imbl::Vector: iter", it, || iv.iter().map(|e| e.tick as u64).sum());
    bench("im::Vector: iter", it, || imv.iter().map(|e| e.tick as u64).sum());
    bench("rpds::Vector: iter", it, || rv.iter().map(|e| e.tick as u64).sum());

    println!("\n--- (b) 1e6 random indexed reads ---");
    let idx: Vec<usize> = (0..1_000_000usize).map(|i| (i.wrapping_mul(2654435761)) % n).collect();
    let it = 5;
    bench("Vec: index", it, || idx.iter().map(|&i| v[i].tick as u64).sum());
    bench("Rope: index", it, || idx.iter().map(|&i| rope.get(i).tick as u64).sum());
    bench("imbl::Vector: index", it, || idx.iter().map(|&i| iv[i].tick as u64).sum());
    bench("im::Vector: index", it, || idx.iter().map(|&i| imv[i].tick as u64).sum());
    bench("rpds::Vector: index", it, || idx.iter().map(|&i| rv.get(i).unwrap().tick as u64).sum());

    println!("\n--- (b2) 1e6 random map lookups ---");
    let kidx: Vec<u32> = idx.iter().map(|&i| i as u32).collect();
    bench("std::HashMap: get", it, || kidx.iter().map(|k| hm[k].tick as u64).sum());
    bench("imbl::HashMap: get", it, || kidx.iter().map(|k| ihm[k].tick as u64).sum());

    println!("\n--- (c) ONE point update, producing a new version ---");
    let it = 2000;
    let mut ctr = 0usize;
    bench("Vec: clone + set  (naive full snapshot)", 50, || {
        ctr = ctr.wrapping_add(1);
        let mut c = v.clone();
        c[ctr % n] = Ev::new(1);
        c[0].tick as u64
    });
    bench("Rope: cow set (share all but 1 chunk)", it, || {
        ctr = ctr.wrapping_add(1);
        let r2 = rope.set(ctr % n, Ev::new(2));
        r2.get(0).tick as u64
    });
    bench("imbl::Vector: update", it, || {
        ctr = ctr.wrapping_add(1);
        let v2 = iv.update(ctr % n, Ev::new(2));
        v2[0].tick as u64
    });
    bench("im::Vector: update", it, || {
        ctr = ctr.wrapping_add(1);
        let v2 = imv.update(ctr % n, Ev::new(2));
        v2[0].tick as u64
    });
    bench("rpds::Vector: set", it, || {
        ctr = ctr.wrapping_add(1);
        let v2 = rv.set(ctr % n, Ev::new(2)).unwrap();
        v2.get(0).unwrap().tick as u64
    });
    bench("imbl::HashMap: update", it, || {
        ctr = ctr.wrapping_add(1);
        let m2 = ihm.update((ctr % n) as u32, Ev::new(2));
        m2[&0].tick as u64
    });

    println!("\n--- (d) snapshot = clone the whole structure ---");
    bench("Vec: clone (deep)", 50, || {
        let c = v.clone();
        c[0].tick as u64
    });
    bench("Rope: clone (Arc bump)", 200_000, || {
        let c = rope.clone();
        c.len as u64
    });
    bench("imbl::Vector: clone", 200_000, || {
        let c = iv.clone();
        c.len() as u64
    });
    bench("im::Vector: clone", 200_000, || {
        let c = imv.clone();
        c.len() as u64
    });
    bench("rpds::Vector: clone", 200_000, || {
        let c = rv.clone();
        c.len() as u64
    });
    bench("std::HashMap: clone (deep)", 20, || {
        let c = hm.clone();
        c.len() as u64
    });
    bench("imbl::HashMap: clone", 200_000, || {
        let c = ihm.clone();
        c.len() as u64
    });

    println!("\n--- (e) 10k sequential point updates, all versions retained ---");
    let t = Instant::now();
    let mut keep_rope: Vec<Rope> = Vec::with_capacity(10_000);
    let mut cur = rope.clone();
    for i in 0..10_000usize {
        cur = cur.set((i * 97) % n, Ev::new(3));
        keep_rope.push(cur.clone());
    }
    println!("Rope   10k retained versions: {:>8.2} ms   (rss below)", t.elapsed().as_secs_f64() * 1e3);
    print_rss("after rope history");

    let t = Instant::now();
    let mut keep_i: Vec<imbl::Vector<Ev>> = Vec::with_capacity(10_000);
    let mut curi = iv.clone();
    for i in 0..10_000usize {
        curi = curi.update((i * 97) % n, Ev::new(3));
        keep_i.push(curi.clone());
    }
    println!("imbl   10k retained versions: {:>8.2} ms", t.elapsed().as_secs_f64() * 1e3);
    print_rss("after imbl history");

    // keep alive
    println!("(alive: {} {} {})", keep_rope.len(), keep_i.len(), keep_rope[0].len);
    drop(keep_rope);
    drop(keep_i);
}

fn print_rss(tag: &str) {
    if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
        for l in s.lines() {
            if l.starts_with("VmRSS") || l.starts_with("VmHWM") {
                println!("   [{}] {}", tag, l.trim());
            }
        }
    }
}
