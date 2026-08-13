// Isolated measurement: cost of RETAINING history.
// Run one variant per process so RSS is clean.
//   hist rope <chunk> | hist imbl | hist im | hist rpds | hist full
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy)]
#[repr(C)]
struct Ev {
    tick: u32,
    kind: u8,
    key: u8,
    vel: u8,
    _p: u8,
    dur: u32,
    val: f32,
}
fn ev(i: u32) -> Ev {
    Ev { tick: i * 7, kind: 1, key: (i % 88) as u8, vel: 100, _p: 0, dur: 240, val: 0.5 }
}

const N: usize = 1_000_000;
const EDITS: usize = 10_000;

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS"))
                .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        })
        .unwrap_or(0)
}

#[derive(Clone)]
struct Rope {
    chunks: Arc<Vec<Arc<Vec<Ev>>>>,
    csz: usize,
}
impl Rope {
    fn build(n: usize, csz: usize) -> Self {
        let mut chunks = Vec::new();
        let mut i = 0;
        while i < n {
            let hi = (i + csz).min(n);
            chunks.push(Arc::new((i..hi).map(|j| ev(j as u32)).collect::<Vec<_>>()));
            i = hi;
        }
        Rope { chunks: Arc::new(chunks), csz }
    }
    fn set(&self, i: usize, e: Ev) -> Rope {
        let ci = i / self.csz;
        let mut chunks = (*self.chunks).clone();
        let mut c = (*chunks[ci]).clone();
        c[i % self.csz] = e;
        chunks[ci] = Arc::new(c);
        Rope { chunks: Arc::new(chunks), csz: self.csz }
    }
    fn scan(&self) -> u64 {
        self.chunks.iter().flat_map(|c| c.iter()).map(|e| e.tick as u64).sum()
    }
    fn get(&self, i: usize) -> Ev {
        self.chunks[i / self.csz][i % self.csz]
    }
}

fn report(tag: &str, base: u64, build_ms: f64, edit_ms: f64, scan_us: f64, idx_ms: f64) {
    let now = rss_kb();
    println!(
        "{:<26} base={:>7} kB  after={:>7} kB  hist={:>7} kB  per-version={:>8.0} B   build={:>7.1} ms  10k-edits={:>8.2} ms  scan={:>8.1} us  1e6-idx={:>7.2} ms",
        tag,
        base,
        now,
        now.saturating_sub(base),
        (now.saturating_sub(base) as f64 * 1024.0) / EDITS as f64,
        build_ms,
        edit_ms,
        scan_us,
        idx_ms
    );
}

fn idxs() -> Vec<usize> {
    (0..1_000_000usize).map(|i| i.wrapping_mul(2654435761) % N).collect()
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "rope".into());
    let arg: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(512);
    let ix = idxs();

    match mode.as_str() {
        "rope" => {
            let t = Instant::now();
            let r = Rope::build(N, arg);
            let b = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let s: u64 = (0..20).map(|_| r.scan()).sum();
            let sc = t.elapsed().as_secs_f64() * 1e6 / 20.0;
            let t = Instant::now();
            let g: u64 = ix.iter().map(|&i| r.get(i).tick as u64).sum();
            let idxms = t.elapsed().as_secs_f64() * 1e3;
            let base = rss_kb();
            let t = Instant::now();
            let mut keep = Vec::with_capacity(EDITS);
            let mut cur = r.clone();
            for i in 0..EDITS {
                cur = cur.set((i * 977) % N, ev(3));
                keep.push(cur.clone());
            }
            let e = t.elapsed().as_secs_f64() * 1e3;
            report(&format!("rope chunk={}", arg), base, b, e, sc, idxms);
            std::hint::black_box((keep.len(), s, g));
        }
        "imbl" => {
            let t = Instant::now();
            let v: imbl::Vector<Ev> = (0..N).map(|i| ev(i as u32)).collect();
            let b = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let s: u64 = (0..20).map(|_| v.iter().map(|e| e.tick as u64).sum::<u64>()).sum();
            let sc = t.elapsed().as_secs_f64() * 1e6 / 20.0;
            let t = Instant::now();
            let g: u64 = ix.iter().map(|&i| v[i].tick as u64).sum();
            let idxms = t.elapsed().as_secs_f64() * 1e3;
            let base = rss_kb();
            let t = Instant::now();
            let mut keep = Vec::with_capacity(EDITS);
            let mut cur = v.clone();
            for i in 0..EDITS {
                cur = cur.update((i * 977) % N, ev(3));
                keep.push(cur.clone());
            }
            let e = t.elapsed().as_secs_f64() * 1e3;
            report("imbl::Vector", base, b, e, sc, idxms);
            std::hint::black_box((keep.len(), s, g));
        }
        "im" => {
            let t = Instant::now();
            let v: im::Vector<Ev> = (0..N).map(|i| ev(i as u32)).collect();
            let b = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let s: u64 = (0..20).map(|_| v.iter().map(|e| e.tick as u64).sum::<u64>()).sum();
            let sc = t.elapsed().as_secs_f64() * 1e6 / 20.0;
            let t = Instant::now();
            let g: u64 = ix.iter().map(|&i| v[i].tick as u64).sum();
            let idxms = t.elapsed().as_secs_f64() * 1e3;
            let base = rss_kb();
            let t = Instant::now();
            let mut keep = Vec::with_capacity(EDITS);
            let mut cur = v.clone();
            for i in 0..EDITS {
                cur = cur.update((i * 977) % N, ev(3));
                keep.push(cur.clone());
            }
            let e = t.elapsed().as_secs_f64() * 1e3;
            report("im::Vector", base, b, e, sc, idxms);
            std::hint::black_box((keep.len(), s, g));
        }
        "rpds" => {
            let t = Instant::now();
            let v: rpds::Vector<Ev> = (0..N).map(|i| ev(i as u32)).collect();
            let b = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let s: u64 = (0..20).map(|_| v.iter().map(|e| e.tick as u64).sum::<u64>()).sum();
            let sc = t.elapsed().as_secs_f64() * 1e6 / 20.0;
            let t = Instant::now();
            let g: u64 = ix.iter().map(|&i| v.get(i).unwrap().tick as u64).sum();
            let idxms = t.elapsed().as_secs_f64() * 1e3;
            let base = rss_kb();
            let t = Instant::now();
            let mut keep = Vec::with_capacity(EDITS);
            let mut cur = v.clone();
            for i in 0..EDITS {
                cur = cur.set((i * 977) % N, ev(3)).unwrap();
                keep.push(cur.clone());
            }
            let e = t.elapsed().as_secs_f64() * 1e3;
            report("rpds::Vector", base, b, e, sc, idxms);
            std::hint::black_box((keep.len(), s, g));
        }
        "full" => {
            // Naive: keep a full deep Vec copy per version. Expect ~16 GB -> cap at 200.
            let t = Instant::now();
            let v: Vec<Ev> = (0..N).map(|i| ev(i as u32)).collect();
            let b = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let s: u64 = (0..20).map(|_| v.iter().map(|e| e.tick as u64).sum::<u64>()).sum();
            let sc = t.elapsed().as_secs_f64() * 1e6 / 20.0;
            let t = Instant::now();
            let g: u64 = ix.iter().map(|&i| v[i].tick as u64).sum();
            let idxms = t.elapsed().as_secs_f64() * 1e3;
            let base = rss_kb();
            let n = 200;
            let t = Instant::now();
            let mut keep = Vec::with_capacity(n);
            let mut cur = v.clone();
            for i in 0..n {
                let mut c = cur.clone();
                c[(i * 977) % N] = ev(3);
                cur = c;
                keep.push(cur.clone());
            }
            let e = t.elapsed().as_secs_f64() * 1e3 / n as f64 * EDITS as f64;
            let now = rss_kb();
            println!(
                "{:<26} base={:>7} kB  after={:>7} kB  hist={:>7} kB  per-version={:>8.0} B   build={:>7.1} ms  10k-edits={:>8.2} ms (extrapolated from {})  scan={:>8.1} us  1e6-idx={:>7.2} ms",
                "Vec deep copy/version", base, now, now - base,
                ((now - base) as f64 * 1024.0) / n as f64, b, e, n, sc, idxms
            );
            std::hint::black_box((keep.len(), s, g));
        }
        _ => eprintln!("unknown"),
    }
}
