// The candidate design: a persistent (COW) B-tree whose LEAVES are dense
// struct-of-values chunks, and whose internal nodes carry summaries.
// Goal: Vec-speed sequential scan + O(log N) per-version memory.
//
// usage: btree <leaf_events> <branch>
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

#[derive(Clone, Copy)]
struct Summary {
    count: u32,
    min_tick: u32,
    max_tick: u32,
    min_key: u8,
    max_key: u8,
}

enum Node {
    Leaf(Vec<Ev>),
    Internal(Vec<(Summary, Arc<Node>)>),
}

fn leaf_summary(evs: &[Ev]) -> Summary {
    let mut s = Summary { count: evs.len() as u32, min_tick: u32::MAX, max_tick: 0, min_key: 255, max_key: 0 };
    for e in evs {
        s.min_tick = s.min_tick.min(e.tick);
        s.max_tick = s.max_tick.max(e.tick);
        s.min_key = s.min_key.min(e.key);
        s.max_key = s.max_key.max(e.key);
    }
    s
}
fn merge(kids: &[(Summary, Arc<Node>)]) -> Summary {
    let mut s = Summary { count: 0, min_tick: u32::MAX, max_tick: 0, min_key: 255, max_key: 0 };
    for (k, _) in kids {
        s.count += k.count;
        s.min_tick = s.min_tick.min(k.min_tick);
        s.max_tick = s.max_tick.max(k.max_tick);
        s.min_key = s.min_key.min(k.min_key);
        s.max_key = s.max_key.max(k.max_key);
    }
    s
}

#[derive(Clone)]
struct Tree {
    root: Arc<Node>,
    sum: Summary,
    leaf: usize,
    branch: usize,
}

impl Tree {
    fn build(n: usize, leaf: usize, branch: usize) -> Tree {
        let mut level: Vec<(Summary, Arc<Node>)> = Vec::new();
        let mut i = 0;
        while i < n {
            let hi = (i + leaf).min(n);
            let evs: Vec<Ev> = (i..hi).map(|j| ev(j as u32)).collect();
            level.push((leaf_summary(&evs), Arc::new(Node::Leaf(evs))));
            i = hi;
        }
        let mut depth = 0;
        while level.len() > 1 {
            let mut next = Vec::new();
            for c in level.chunks(branch) {
                let kids: Vec<(Summary, Arc<Node>)> = c.to_vec();
                next.push((merge(&kids), Arc::new(Node::Internal(kids))));
            }
            level = next;
            depth += 1;
        }
        let (sum, root) = level.pop().unwrap();
        eprintln!("   (depth above leaves = {})", depth);
        Tree { root, sum, leaf, branch }
    }

    #[inline]
    fn get(&self, mut i: usize) -> Ev {
        let mut n: &Node = &self.root;
        loop {
            match n {
                Node::Leaf(v) => return v[i],
                Node::Internal(kids) => {
                    let mut k = 0;
                    loop {
                        let c = kids[k].0.count as usize;
                        if i < c {
                            break;
                        }
                        i -= c;
                        k += 1;
                    }
                    n = &kids[k].1;
                }
            }
        }
    }

    fn scan(&self) -> u64 {
        fn go(n: &Node, acc: &mut u64) {
            match n {
                Node::Leaf(v) => {
                    for e in v {
                        *acc += e.tick as u64
                    }
                }
                Node::Internal(k) => {
                    for (_, c) in k {
                        go(c, acc)
                    }
                }
            }
        }
        let mut a = 0;
        go(&self.root, &mut a);
        a
    }

    /// Range query using summaries: count events with tick in [lo,hi].
    /// Skips whole subtrees. This is the piano-roll viewport query.
    fn count_range(&self, lo: u32, hi: u32) -> u64 {
        fn go(n: &Node, s: &Summary, lo: u32, hi: u32, acc: &mut u64, visited: &mut u64) {
            *visited += 1;
            if s.max_tick < lo || s.min_tick > hi {
                return;
            }
            match n {
                Node::Leaf(v) => {
                    for e in v {
                        if e.tick >= lo && e.tick <= hi {
                            *acc += 1
                        }
                    }
                }
                Node::Internal(k) => {
                    for (ks, c) in k {
                        go(c, ks, lo, hi, acc, visited)
                    }
                }
            }
        }
        let mut a = 0;
        let mut vis = 0;
        go(&self.root, &self.sum, lo, hi, &mut a, &mut vis);
        a
    }

    /// COW point update: copies exactly one leaf + one node per level.
    fn set(&self, i: usize, e: Ev) -> Tree {
        fn go(n: &Node, i: usize, e: Ev) -> (Summary, Arc<Node>) {
            match n {
                Node::Leaf(v) => {
                    let mut v2 = v.clone();
                    v2[i] = e;
                    (leaf_summary(&v2), Arc::new(Node::Leaf(v2)))
                }
                Node::Internal(kids) => {
                    let mut k = 0;
                    let mut i = i;
                    loop {
                        let c = kids[k].0.count as usize;
                        if i < c {
                            break;
                        }
                        i -= c;
                        k += 1;
                    }
                    let mut k2 = kids.clone();
                    k2[k] = go(&kids[k].1, i, e);
                    (merge(&k2), Arc::new(Node::Internal(k2)))
                }
            }
        }
        let (sum, root) = go(&self.root, i, e);
        Tree { root, sum, leaf: self.leaf, branch: self.branch }
    }
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

fn main() {
    let leaf: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(128);
    let branch: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(32);

    let t = Instant::now();
    let tr = Tree::build(N, leaf, branch);
    let build = t.elapsed().as_secs_f64() * 1e3;

    let t = Instant::now();
    let mut s = 0u64;
    for _ in 0..20 {
        s += tr.scan();
    }
    let scan = t.elapsed().as_secs_f64() * 1e6 / 20.0;

    let ix: Vec<usize> = (0..1_000_000usize).map(|i| i.wrapping_mul(2654435761) % N).collect();
    let t = Instant::now();
    let g: u64 = ix.iter().map(|&i| tr.get(i).tick as u64).sum();
    let idx = t.elapsed().as_secs_f64() * 1e3;

    // viewport query: 1% of the timeline
    let span = (N as u32 * 7) / 100;
    let t = Instant::now();
    let mut q = 0u64;
    for k in 0..100u32 {
        q += tr.count_range(k * span, k * span + span);
    }
    let rq = t.elapsed().as_secs_f64() * 1e6 / 100.0;

    let base = rss_kb();
    let t = Instant::now();
    let mut keep = Vec::with_capacity(EDITS);
    let mut cur = tr.clone();
    for i in 0..EDITS {
        cur = cur.set((i * 977) % N, ev(3));
        keep.push(cur.clone());
    }
    let edits = t.elapsed().as_secs_f64() * 1e3;
    let now = rss_kb();

    println!(
        "btree leaf={:<5} branch={:<3} base={:>7} kB  after={:>7} kB  hist={:>7} kB  per-version={:>7.0} B   build={:>6.1} ms  10k-edits={:>7.2} ms ({:>5.2} us/edit)  scan={:>7.1} us  1e6-idx={:>6.2} ms  1%-range-query={:>7.1} us",
        leaf, branch, base, now, now - base,
        ((now - base) as f64 * 1024.0) / EDITS as f64,
        build, edits, edits * 1000.0 / EDITS as f64, scan, idx, rq
    );
    std::hint::black_box((keep.len(), s, g, q));

    // Drop-cascade cost: what a naive "trim history" would cost if it ran
    // anywhere latency-sensitive. Also: dropping the single newest version.
    let one = keep.pop().unwrap();
    let t = Instant::now();
    drop(one);
    let d1 = t.elapsed().as_nanos();
    let t = Instant::now();
    drop(keep);
    let dall = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    drop(cur);
    drop(tr);
    let dlast = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "   drop: newest-version={} ns   whole {}-version history={:.2} ms   final base tree={:.2} ms",
        d1, EDITS, dall, dlast
    );
}
