// Bulk-op amplification measurement for the COW B-tree design in
// docs/research/06-time-travel-storage.md (leaf 128, branch 32, 16-byte events).
//
// Unlike the original pdsbench (RSS deltas), this measures retained bytes
// EXACTLY: walk the new version, count every node whose Arc pointer was not
// reachable from any previous version, and charge its true allocation size
// (ArcInner header 16 B + Node enum 32 B + heap buffer).
use std::collections::HashSet;
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

fn leaves_from(evs: &[Ev]) -> Vec<(Sum, Arc<Node>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < evs.len() {
        let hi = (i + LEAF).min(evs.len());
        let v: Vec<Ev> = evs[i..hi].to_vec();
        out.push((lsum(&v), Arc::new(Node::Leaf(v))));
        i = hi;
    }
    out
}

fn build_levels(mut lvl: Vec<(Sum, Arc<Node>)>) -> Tree {
    if lvl.is_empty() {
        lvl.push((lsum(&[]), Arc::new(Node::Leaf(Vec::new()))));
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

impl Tree {
    fn build(n: usize) -> Tree {
        let evs: Vec<Ev> = (0..n).map(|i| ev(i as u32)).collect();
        build_levels(leaves_from(&evs))
    }

    /// COW transform of a sorted set of absolute indices; untouched subtrees
    /// are reused by Arc. This is quantize/transpose/humanize.
    fn transform(&self, targets: &[usize], f: &impl Fn(&mut Ev)) -> Tree {
        fn go(
            n: &Arc<Node>,
            base: usize,
            count: usize,
            targets: &[usize],
            f: &impl Fn(&mut Ev),
        ) -> (Sum, Arc<Node>) {
            if targets.is_empty() {
                let s = match &**n {
                    Node::Leaf(v) => lsum(v),
                    Node::Int(k) => msum(k),
                };
                return (s, n.clone());
            }
            match &**n {
                Node::Leaf(v) => {
                    let mut v2 = v.clone();
                    for &t in targets {
                        f(&mut v2[t - base]);
                    }
                    (lsum(&v2), Arc::new(Node::Leaf(v2)))
                }
                Node::Int(k) => {
                    let mut k2: Vec<(Sum, Arc<Node>)> = Vec::with_capacity(k.len());
                    let mut b = base;
                    let mut ti = 0usize;
                    for (s, c) in k {
                        let cc = s.count as usize;
                        let lo = ti;
                        while ti < targets.len() && targets[ti] < b + cc {
                            ti += 1;
                        }
                        k2.push(go(c, b, cc, &targets[lo..ti], f));
                        b += cc;
                    }
                    let _ = count;
                    (msum(&k2), Arc::new(Node::Int(k2)))
                }
            }
        }
        let total = self.sum.count as usize;
        let (sum, root) = go(&self.root, 0, total, targets, f);
        Tree { root, sum }
    }

    /// COW range delete [lo, hi). Fully-covered subtrees are dropped without
    /// allocating; untouched subtrees are reused.
    fn delete_range(&self, lo: usize, hi: usize) -> Tree {
        fn go(n: &Arc<Node>, base: usize, s: Sum, lo: usize, hi: usize) -> Option<(Sum, Arc<Node>)> {
            let count = s.count as usize;
            if hi <= base || lo >= base + count {
                return Some((s, n.clone())); // untouched
            }
            if lo <= base && base + count <= hi {
                return None; // fully deleted, zero new bytes
            }
            match &**n {
                Node::Leaf(v) => {
                    let keep: Vec<Ev> = v
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| base + i < lo || base + i >= hi)
                        .map(|(_, e)| *e)
                        .collect();
                    if keep.is_empty() {
                        None
                    } else {
                        Some((lsum(&keep), Arc::new(Node::Leaf(keep))))
                    }
                }
                Node::Int(k) => {
                    let mut k2 = Vec::with_capacity(k.len());
                    let mut b = base;
                    for (cs, c) in k {
                        let cc = cs.count as usize;
                        if let Some(kid) = go(c, b, *cs, lo, hi) {
                            k2.push(kid);
                        }
                        b += cc;
                    }
                    if k2.is_empty() {
                        None
                    } else {
                        Some((msum(&k2), Arc::new(Node::Int(k2))))
                    }
                }
            }
        }
        match go(&self.root, 0, self.sum, lo, hi) {
            Some((sum, root)) => Tree { root, sum },
            None => build_levels(Vec::new()),
        }
    }

    /// COW paste: insert a contiguous block of events at position `pos`.
    fn insert_block(&self, pos: usize, evs: &[Ev]) -> Tree {
        fn go(n: &Arc<Node>, base: usize, pos: usize, evs: &[Ev]) -> Vec<(Sum, Arc<Node>)> {
            match &**n {
                Node::Leaf(v) => {
                    let cut = pos - base;
                    let mut all: Vec<Ev> = Vec::with_capacity(v.len() + evs.len());
                    all.extend_from_slice(&v[..cut]);
                    all.extend_from_slice(evs);
                    all.extend_from_slice(&v[cut..]);
                    leaves_from(&all)
                }
                Node::Int(k) => {
                    let mut k2: Vec<(Sum, Arc<Node>)> = Vec::new();
                    let mut b = base;
                    let mut done = false;
                    for (s, c) in k {
                        let cc = s.count as usize;
                        if !done && pos >= b && pos <= b + cc && !(pos == b + cc && b + cc < base_total(k)) {
                            k2.extend(go(c, b, pos, evs));
                            done = true;
                        } else {
                            k2.push((*s, c.clone()));
                        }
                        b += cc;
                    }
                    // group into branch-sized parents if oversized
                    if k2.len() > BRANCH {
                        let mut nx = Vec::new();
                        for c in k2.chunks(BRANCH) {
                            let kk: Vec<_> = c.to_vec();
                            nx.push((msum(&kk), Arc::new(Node::Int(kk))));
                        }
                        nx
                    } else {
                        vec![(msum(&k2), Arc::new(Node::Int(k2)))]
                    }
                }
            }
        }
        fn base_total(k: &[(Sum, Arc<Node>)]) -> usize {
            k.iter().map(|(s, _)| s.count as usize).sum()
        }
        let lvl = go(&self.root, 0, pos, evs);
        build_levels(lvl)
    }
}

/// Exact allocation size of one node: ArcInner header (2×usize) + Node enum
/// (tag + Vec = 32 B) + heap buffer at len (collect/to_vec allocate exact).
fn node_bytes(n: &Node) -> usize {
    match n {
        Node::Leaf(v) => 16 + 32 + v.len() * std::mem::size_of::<Ev>(),
        Node::Int(k) => 16 + 32 + k.len() * std::mem::size_of::<(Sum, Arc<Node>)>(),
    }
}

fn collect_ptrs(n: &Arc<Node>, set: &mut HashSet<*const Node>) {
    if !set.insert(Arc::as_ptr(n)) {
        return;
    }
    if let Node::Int(k) = &**n {
        for (_, c) in k {
            collect_ptrs(c, set);
        }
    }
}

/// Bytes reachable from `new` that are NOT in `seen`. Adds them to `seen`.
fn retained_bytes(new: &Arc<Node>, seen: &mut HashSet<*const Node>) -> usize {
    if seen.contains(&Arc::as_ptr(new)) {
        return 0;
    }
    seen.insert(Arc::as_ptr(new));
    let mut b = node_bytes(new);
    if let Node::Int(k) = &**new {
        for (_, c) in k {
            b += retained_bytes(c, seen);
        }
    }
    b
}

fn main() {
    const N: usize = 1_000_000;
    let base = Tree::build(N);
    let mut seen = HashSet::new();
    collect_ptrs(&base.root, &mut seen);
    let total_nodes = seen.len();
    let total_bytes: usize = {
        let mut s2 = HashSet::new();
        retained_bytes(&base.root, &mut s2)
    };
    println!(
        "base tree: {} events, {} nodes, {:.1} MB ({} leaves of {})",
        N,
        total_nodes,
        total_bytes as f64 / 1e6,
        (N + LEAF - 1) / LEAF,
        LEAF
    );
    println!();
    println!("{:<44} {:>12} {:>10} {:>12} {:>10}", "op", "retained B", "µs/op", "amp vs pt", "B/note");

    let mut baseline_pt = 0usize;

    let mut run = |name: &str, notes: usize, op: &dyn Fn(&Tree) -> Tree| {
        let mut seen = seen.clone();
        let t = Instant::now();
        let v2 = op(&base);
        let us = t.elapsed().as_secs_f64() * 1e6;
        let rb = retained_bytes(&v2.root, &mut seen);
        assert_eq!(
            v2.sum.count as usize,
            match name {
                _ if name.starts_with("delete 50k") => N - 50_000,
                _ if name.starts_with("paste 20k") => N + 20_000,
                _ => N,
            }
        );
        if baseline_pt == 0 {
            baseline_pt = rb;
        }
        println!(
            "{:<44} {:>12} {:>10.1} {:>11.1}x {:>10.1}",
            name,
            rb,
            us,
            rb as f64 / baseline_pt as f64,
            rb as f64 / notes as f64
        );
    };

    // 1. point edit — the only case the dossier measured
    run("point edit (dossier's measured case)", 1, &|t| {
        t.transform(&[500_000], &|e| e.vel = 40)
    });

    // 2. quantize contiguous 100k (the §9.4 claim: ~780 leaves ≈ 1.6 MB, ~400×)
    let idx: Vec<usize> = (400_000..500_000).collect();
    run("quantize 100k contiguous", 100_000, &move |t| {
        t.transform(&idx, &|e| e.tick = (e.tick / 120) * 120)
    });

    // 3. quantize 10k contiguous
    let idx: Vec<usize> = (400_000..410_000).collect();
    run("quantize 10k contiguous", 10_000, &move |t| {
        t.transform(&idx, &|e| e.tick = (e.tick / 120) * 120)
    });

    // 4. transpose 1k contiguous selection
    let idx: Vec<usize> = (700_000..701_000).collect();
    run("transpose 1k contiguous", 1_000, &move |t| {
        t.transform(&idx, &|e| e.key = e.key.saturating_add(2))
    });

    // 5. transpose 1k SCATTERED selection (e.g. "all C3 notes in the song")
    let mut idx: Vec<usize> = (0..1_000usize).map(|i| (i.wrapping_mul(2654435761) >> 2) % N).collect();
    idx.sort_unstable();
    idx.dedup();
    let n5 = idx.len();
    run("transpose 1k scattered over 1M", n5, &move |t| {
        t.transform(&idx, &|e| e.key = e.key.saturating_add(2))
    });

    // 6. humanize 10k scattered within one 20k-event track region
    let mut idx: Vec<usize> = (0..10_000usize)
        .map(|i| 300_000 + (i.wrapping_mul(2654435761) >> 2) % 20_000)
        .collect();
    idx.sort_unstable();
    idx.dedup();
    let n6 = idx.len();
    run("humanize 10k scattered in 20k window", n6, &move |t| {
        t.transform(&idx, &|e| e.tick = e.tick.wrapping_add((e.key as u32) % 5))
    });

    // 7. humanize 10k scattered over the whole 1M
    let mut idx: Vec<usize> = (0..10_000usize).map(|i| (i.wrapping_mul(2654435761) >> 2) % N).collect();
    idx.sort_unstable();
    idx.dedup();
    let n7 = idx.len();
    run("humanize 10k scattered over 1M", n7, &move |t| {
        t.transform(&idx, &|e| e.tick = e.tick.wrapping_add((e.key as u32) % 5))
    });

    // 8. delete a contiguous 50k block (≈ delete a 50k-note track's events
    //    if they live inside a shared tree; if each track is its own tree,
    //    deletion is a map-entry drop and retains ~0 new bytes)
    run("delete 50k contiguous", 50_000, &|t| t.delete_range(600_000, 650_000));

    // 9. paste 20k contiguous notes
    let paste: Vec<Ev> = (0..20_000).map(|i| ev(i as u32 + 2_000_000)).collect();
    run("paste 20k notes", 20_000, &move |t| t.insert_block(250_016, &paste));

    println!();
    println!("(amp = retained bytes vs the point-edit baseline; B/note = retained bytes per touched note)");
}
