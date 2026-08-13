// Does the COW chunk B-tree survive INSERT/DELETE, not just point-set?
// Leaves split at 2*LEAF; internal nodes split at 2*BRANCH. Persistent.
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
fn ev(t: u32) -> Ev {
    Ev { tick: t, kind: 1, key: (t % 88) as u8, vel: 100, _p: 0, dur: 240, val: 0.5 }
}

const LEAF: usize = 128;
const BRANCH: usize = 32;

#[derive(Clone, Copy)]
struct Sum {
    count: u32,
    max_tick: u32,
}
enum Node {
    Leaf(Vec<Ev>),
    Int(Vec<(Sum, Arc<Node>)>),
}
fn lsum(v: &[Ev]) -> Sum {
    Sum { count: v.len() as u32, max_tick: v.last().map(|e| e.tick).unwrap_or(0) }
}
fn msum(k: &[(Sum, Arc<Node>)]) -> Sum {
    Sum {
        count: k.iter().map(|(s, _)| s.count).sum(),
        max_tick: k.last().map(|(s, _)| s.max_tick).unwrap_or(0),
    }
}
type Kid = (Sum, Arc<Node>);

/// Insert `e` keeping the tree sorted by tick. Returns new root kid(s)
/// (one, or two if this level split).
fn ins(n: &Node, e: Ev) -> (Kid, Option<Kid>) {
    match n {
        Node::Leaf(v) => {
            let pos = v.partition_point(|x| x.tick <= e.tick);
            let mut v2 = Vec::with_capacity(v.len() + 1);
            v2.extend_from_slice(&v[..pos]);
            v2.push(e);
            v2.extend_from_slice(&v[pos..]);
            if v2.len() > 2 * LEAF {
                let right = v2.split_off(v2.len() / 2);
                (
                    (lsum(&v2), Arc::new(Node::Leaf(v2))),
                    Some((lsum(&right), Arc::new(Node::Leaf(right)))),
                )
            } else {
                ((lsum(&v2), Arc::new(Node::Leaf(v2))), None)
            }
        }
        Node::Int(kids) => {
            let mut i = kids.partition_point(|(s, _)| s.max_tick < e.tick);
            if i >= kids.len() {
                i = kids.len() - 1;
            }
            let (a, b) = ins(&kids[i].1, e);
            let mut k2 = kids.clone();
            k2[i] = a;
            if let Some(b) = b {
                k2.insert(i + 1, b);
            }
            if k2.len() > 2 * BRANCH {
                let right = k2.split_off(k2.len() / 2);
                (
                    (msum(&k2), Arc::new(Node::Int(k2))),
                    Some((msum(&right), Arc::new(Node::Int(right)))),
                )
            } else {
                ((msum(&k2), Arc::new(Node::Int(k2))), None)
            }
        }
    }
}

#[derive(Clone)]
struct Tree {
    root: Arc<Node>,
    sum: Sum,
}
impl Tree {
    fn build(n: usize) -> Tree {
        let mut lvl: Vec<Kid> = Vec::new();
        let mut i = 0;
        while i < n {
            let hi = (i + LEAF).min(n);
            let v: Vec<Ev> = (i..hi).map(|j| ev(j as u32 * 8)).collect();
            lvl.push((lsum(&v), Arc::new(Node::Leaf(v))));
            i = hi;
        }
        while lvl.len() > 1 {
            let mut nx = Vec::new();
            for c in lvl.chunks(BRANCH) {
                let k: Vec<Kid> = c.to_vec();
                nx.push((msum(&k), Arc::new(Node::Int(k))));
            }
            lvl = nx;
        }
        let (sum, root) = lvl.pop().unwrap();
        Tree { root, sum }
    }
    fn insert(&self, e: Ev) -> Tree {
        let (a, b) = ins(&self.root, e);
        match b {
            None => Tree { root: a.1, sum: a.0 },
            Some(b) => {
                let k = vec![a, b];
                Tree { sum: msum(&k), root: Arc::new(Node::Int(k)) }
            }
        }
    }
    fn len(&self) -> u32 {
        self.sum.count
    }
    fn sorted_scan(&self) -> (u64, bool) {
        fn go(n: &Node, last: &mut u32, ok: &mut bool, c: &mut u64) {
            match n {
                Node::Leaf(v) => {
                    for e in v {
                        if e.tick < *last {
                            *ok = false
                        }
                        *last = e.tick;
                        *c += 1;
                    }
                }
                Node::Int(k) => {
                    for (_, ch) in k {
                        go(ch, last, ok, c)
                    }
                }
            }
        }
        let (mut l, mut ok, mut c) = (0, true, 0);
        go(&self.root, &mut l, &mut ok, &mut c);
        (c, ok)
    }
}

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmRSS")).and_then(|l| {
                l.split_whitespace().nth(1).and_then(|v| v.parse().ok())
            })
        })
        .unwrap_or(0)
}

fn main() {
    const N: usize = 1_000_000;
    const INS: usize = 10_000;
    let t = Instant::now();
    let tr = Tree::build(N);
    println!("build {} events: {:.1} ms", N, t.elapsed().as_secs_f64() * 1e3);

    // baseline: Vec insert (memmove) at random positions, no history
    let mut v: Vec<Ev> = (0..N).map(|i| ev(i as u32 * 8)).collect();
    let t = Instant::now();
    for i in 0..INS {
        let tick = ((i * 7919) % N) as u32 * 8 + 1;
        let pos = v.partition_point(|x| x.tick <= tick);
        v.insert(pos, ev(tick));
    }
    println!(
        "Vec::insert x{}      {:>8.2} ms ({:.2} us/insert)  <- mutable baseline, NO history",
        INS,
        t.elapsed().as_secs_f64() * 1e3,
        t.elapsed().as_secs_f64() * 1e6 / INS as f64
    );

    let base = rss_kb();
    let t = Instant::now();
    let mut hist = Vec::with_capacity(INS);
    let mut cur = tr.clone();
    for i in 0..INS {
        let tick = ((i * 7919) % N) as u32 * 8 + 1;
        cur = cur.insert(ev(tick));
        hist.push(cur.clone());
    }
    let el = t.elapsed().as_secs_f64() * 1e3;
    let after = rss_kb();
    let (cnt, sorted) = cur.sorted_scan();
    println!(
        "COW insert x{}       {:>8.2} ms ({:.2} us/insert)  ALL {} versions retained  history={:.1} MB  per-version={:.0} B",
        INS, el, el * 1000.0 / INS as f64, INS,
        (after - base) as f64 / 1024.0,
        ((after - base) as f64 * 1024.0) / INS as f64
    );
    println!("   correctness: len={} (expect {}) scan_count={} sorted={}", cur.len(), N + INS, cnt, sorted);
    std::hint::black_box(hist.len());
}
