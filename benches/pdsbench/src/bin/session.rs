// Session-scale test: 500 tracks, 10^7 events total, versioned session root.
// Session = imbl::HashMap<TrackId, Arc<EventTree>>  (persistent map for identity)
//           EventTree = COW B-tree with dense chunk leaves (bulk data)
// Measures: build, one-note edit cost, per-version memory, full scan
// (= "render/export the whole song"), and materialize-a-past-state latency.
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
struct Sum {
    count: u32,
    min_tick: u32,
    max_tick: u32,
}
enum Node {
    Leaf(Vec<Ev>),
    Int(Vec<(Sum, Arc<Node>)>),
}
const LEAF: usize = 128;
const BRANCH: usize = 32;

fn lsum(v: &[Ev]) -> Sum {
    let mut s = Sum { count: v.len() as u32, min_tick: u32::MAX, max_tick: 0 };
    for e in v {
        s.min_tick = s.min_tick.min(e.tick);
        s.max_tick = s.max_tick.max(e.tick);
    }
    s
}
fn msum(k: &[(Sum, Arc<Node>)]) -> Sum {
    let mut s = Sum { count: 0, min_tick: u32::MAX, max_tick: 0 };
    for (a, _) in k {
        s.count += a.count;
        s.min_tick = s.min_tick.min(a.min_tick);
        s.max_tick = s.max_tick.max(a.max_tick);
    }
    s
}

#[derive(Clone)]
struct Tree {
    root: Arc<Node>,
    sum: Sum,
}
impl Tree {
    fn build(n: usize, off: u32) -> Tree {
        let mut lvl: Vec<(Sum, Arc<Node>)> = Vec::new();
        let mut i = 0;
        while i < n {
            let hi = (i + LEAF).min(n);
            let v: Vec<Ev> = (i..hi).map(|j| ev(j as u32 + off)).collect();
            lvl.push((lsum(&v), Arc::new(Node::Leaf(v))));
            i = hi;
        }
        while lvl.len() > 1 {
            let mut nx = Vec::new();
            for c in lvl.chunks(BRANCH) {
                let k: Vec<_> = c.to_vec();
                nx.push((msum(&k), Arc::new(Node::Int(k))));
            }
            lvl = nx;
        }
        let (sum, root) = lvl.pop().unwrap();
        Tree { root, sum }
    }
    fn set(&self, i: usize, e: Ev) -> Tree {
        fn go(n: &Node, mut i: usize, e: Ev) -> (Sum, Arc<Node>) {
            match n {
                Node::Leaf(v) => {
                    let mut v2 = v.clone();
                    v2[i] = e;
                    (lsum(&v2), Arc::new(Node::Leaf(v2)))
                }
                Node::Int(k) => {
                    let mut j = 0;
                    loop {
                        let c = k[j].0.count as usize;
                        if i < c {
                            break;
                        }
                        i -= c;
                        j += 1;
                    }
                    let mut k2 = k.clone();
                    k2[j] = go(&k[j].1, i, e);
                    (msum(&k2), Arc::new(Node::Int(k2)))
                }
            }
        }
        let (sum, root) = go(&self.root, i, e);
        Tree { root, sum }
    }
    fn scan(&self) -> u64 {
        fn go(n: &Node, a: &mut u64) {
            match n {
                Node::Leaf(v) => {
                    for e in v {
                        *a += e.tick as u64
                    }
                }
                Node::Int(k) => {
                    for (_, c) in k {
                        go(c, a)
                    }
                }
            }
        }
        let mut a = 0;
        go(&self.root, &mut a);
        a
    }
    /// Flatten to a dense Vec — this is "materialize an RT-ready buffer".
    fn flatten_into(&self, out: &mut Vec<Ev>) {
        fn go(n: &Node, o: &mut Vec<Ev>) {
            match n {
                Node::Leaf(v) => o.extend_from_slice(v),
                Node::Int(k) => {
                    for (_, c) in k {
                        go(c, o)
                    }
                }
            }
        }
        go(&self.root, out);
    }
}

type Session = imbl::HashMap<u32, Tree>;

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
    let tracks: u32 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(500);
    let per: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let versions: usize = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let total = tracks as usize * per;
    println!(
        "{} tracks x {} events = {} events ({:.1} MB raw payload)",
        tracks, per, total, (total * 16) as f64 / 1e6
    );

    let r0 = rss_kb();
    let t = Instant::now();
    let mut sess: Session = Session::new();
    for tr in 0..tracks {
        sess.insert(tr, Tree::build(per, tr * 1_000_000));
    }
    println!("build session            {:>9.1} ms   RSS {:>8} kB ({:.2} bytes/event all-in)",
        t.elapsed().as_secs_f64() * 1e3, rss_kb(),
        ((rss_kb() - r0) as f64 * 1024.0) / total as f64);

    // full-session scan = "render / export the whole song"
    let t = Instant::now();
    let s: u64 = sess.values().map(|tr| tr.scan()).sum();
    println!("full-session scan        {:>9.1} ms", t.elapsed().as_secs_f64() * 1e3);

    // materialize one track to a dense RT buffer
    let mut buf = Vec::with_capacity(per);
    let t = Instant::now();
    for tr in 0..tracks.min(64) {
        buf.clear();
        sess[&tr].flatten_into(&mut buf);
    }
    println!("flatten 64 tracks -> Vec {:>9.2} ms  ({:.1} us/track of {} events)",
        t.elapsed().as_secs_f64() * 1e3,
        t.elapsed().as_secs_f64() * 1e6 / 64.0, per);

    // ---- history: N user edits, every version retained ----
    let base = rss_kb();
    let t = Instant::now();
    let mut hist: Vec<Session> = Vec::with_capacity(versions);
    let mut cur = sess.clone();
    for i in 0..versions {
        let tr = (i as u32 * 37) % tracks;
        let idx = (i * 977) % per;
        let nt = cur[&tr].set(idx, ev(9));
        cur = cur.update(tr, nt);
        hist.push(cur.clone());
    }
    let el = t.elapsed().as_secs_f64() * 1e3;
    let after = rss_kb();
    println!(
        "{} retained versions     {:>9.1} ms ({:.2} us/edit)   history RSS = {:.1} MB   per-version = {:.0} B",
        versions, el, el * 1000.0 / versions as f64,
        (after - base) as f64 / 1024.0,
        ((after - base) as f64 * 1024.0) / versions as f64
    );

    // ---- materialize an arbitrary past state ----
    let mut acc = 0u64;
    let t = Instant::now();
    for k in 0..1000 {
        let v = &hist[(k * 7919) % versions];
        acc += v.len() as u64;
    }
    println!("select a past version    {:>9.1} ns/lookup (pure pointer)", t.elapsed().as_nanos() as f64 / 1000.0);

    // realistic: "show me that state" = scan one track of it
    let t = Instant::now();
    for k in 0..200 {
        let v = &hist[(k * 7919) % versions];
        acc += v[&((k as u32 * 13) % tracks)].scan();
    }
    println!("...and read one track    {:>9.1} us", t.elapsed().as_secs_f64() * 1e6 / 200.0);

    // ---- what a naive full-copy history would have cost ----
    println!(
        "\nnaive full-copy history would need {} x {:.1} MB = {:.1} GB",
        versions, (total * 16) as f64 / 1e6, (versions * total * 16) as f64 / 1e9
    );

    let t = Instant::now();
    drop(hist);
    println!("drop whole history       {:>9.1} ms   <-- MUST NOT happen on the audio thread", t.elapsed().as_secs_f64() * 1e3);
    std::hint::black_box((s, acc, buf.len()));
}
