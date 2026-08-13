//! Generic COW B-tree (leaf 128 / branch 32) with EXACT retained-bytes
//! accounting by Arc-pointer diffing. Ported from the original bulkbench
//! main.rs, made generic over the record type so the same code measures the
//! 16-byte legacy record and the 20-byte canonical record (note_id: u32).
use std::collections::HashSet;
use std::sync::Arc;

pub const LEAF: usize = 128;
pub const BRANCH: usize = 32;

pub trait Rec: Copy + 'static {
    fn mk(i: u32) -> Self;
    fn shift_key(&mut self, d: u8);
    fn quantize_tick(&mut self, grid: u32);
    fn jitter_tick(&mut self, salt: u32);
    fn set_vel(&mut self, v: u8);
}

/// The record the previous round measured: 16 bytes.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Ev16 {
    pub tick: u32,
    pub kind: u8,
    pub key: u8,
    pub vel: u8,
    pub _p: u8,
    pub dur: u32,
    pub val: f32,
}
const _: () = assert!(std::mem::size_of::<Ev16>() == 16);

impl Rec for Ev16 {
    fn mk(i: u32) -> Self {
        Ev16 { tick: i * 7, kind: 1, key: (i % 88) as u8, vel: 100, _p: 0, dur: 240, val: 0.5 }
    }
    fn shift_key(&mut self, d: u8) {
        self.key = self.key.saturating_add(d);
    }
    fn quantize_tick(&mut self, g: u32) {
        self.tick = (self.tick / g) * g;
    }
    fn jitter_tick(&mut self, s: u32) {
        self.tick = self.tick.wrapping_add((self.key as u32 ^ s) % 5);
    }
    fn set_vel(&mut self, v: u8) {
        self.vel = v;
    }
}

/// The canonical record once §2.1's note identity lands: 16 B + note_id: u32.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Ev20 {
    pub tick: u32,
    pub note_id: u32,
    pub dur: u32,
    pub val: f32,
    pub kind: u8,
    pub key: u8,
    pub vel: u8,
    pub _p: u8,
}
const _: () = assert!(std::mem::size_of::<Ev20>() == 20);

impl Rec for Ev20 {
    fn mk(i: u32) -> Self {
        Ev20 {
            tick: i * 7,
            note_id: i,
            dur: 240,
            val: 0.5,
            kind: 1,
            key: (i % 88) as u8,
            vel: 100,
            _p: 0,
        }
    }
    fn shift_key(&mut self, d: u8) {
        self.key = self.key.saturating_add(d);
    }
    fn quantize_tick(&mut self, g: u32) {
        self.tick = (self.tick / g) * g;
    }
    fn jitter_tick(&mut self, s: u32) {
        self.tick = self.tick.wrapping_add((self.key as u32 ^ s) % 5);
    }
    fn set_vel(&mut self, v: u8) {
        self.vel = v;
    }
}

#[derive(Clone, Copy)]
pub struct Sum {
    pub count: u32,
    pub min_tick: u32,
    pub max_tick: u32,
}

pub enum Node<E: Rec> {
    Leaf(Vec<E>),
    Int(Vec<(Sum, Arc<Node<E>>)>),
}

fn lsum<E: Rec>(v: &[E]) -> Sum {
    // min/max tick tracking kept for parity with the original crate; the
    // byte accounting never reads it, so a generic placeholder suffices.
    Sum { count: v.len() as u32, min_tick: 0, max_tick: 0 }
}
fn msum<E: Rec>(k: &[(Sum, Arc<Node<E>>)]) -> Sum {
    let mut s = Sum { count: 0, min_tick: u32::MAX, max_tick: 0 };
    for (a, _) in k {
        s.count += a.count;
        s.min_tick = s.min_tick.min(a.min_tick);
        s.max_tick = s.max_tick.max(a.max_tick);
    }
    s
}

#[derive(Clone)]
pub struct Tree<E: Rec> {
    pub root: Arc<Node<E>>,
    pub sum: Sum,
}

fn leaves_from<E: Rec>(evs: &[E]) -> Vec<(Sum, Arc<Node<E>>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < evs.len() {
        let hi = (i + LEAF).min(evs.len());
        let v: Vec<E> = evs[i..hi].to_vec();
        out.push((lsum(&v), Arc::new(Node::Leaf(v))));
        i = hi;
    }
    out
}

fn build_levels<E: Rec>(mut lvl: Vec<(Sum, Arc<Node<E>>)>) -> Tree<E> {
    if lvl.is_empty() {
        lvl.push((lsum::<E>(&[]), Arc::new(Node::Leaf(Vec::new()))));
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

impl<E: Rec> Tree<E> {
    pub fn build(n: usize) -> Tree<E> {
        Self::build_from(0, n)
    }
    /// Build n events with ids start..start+n (distinct content per pattern /
    /// per regeneration; accounting is by pointer so ids only aid debugging).
    pub fn build_from(start: u32, n: usize) -> Tree<E> {
        let evs: Vec<E> = (0..n).map(|i| E::mk(start + i as u32)).collect();
        build_levels(leaves_from(&evs))
    }
    pub fn count(&self) -> usize {
        self.sum.count as usize
    }

    /// COW transform of a sorted set of absolute indices; untouched subtrees
    /// are reused by Arc. This is quantize/transpose/humanize.
    pub fn transform(&self, targets: &[usize], f: &impl Fn(&mut E)) -> Tree<E> {
        fn go<E: Rec>(
            n: &Arc<Node<E>>,
            base: usize,
            targets: &[usize],
            f: &impl Fn(&mut E),
        ) -> (Sum, Arc<Node<E>>) {
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
                    let mut k2: Vec<(Sum, Arc<Node<E>>)> = Vec::with_capacity(k.len());
                    let mut b = base;
                    let mut ti = 0usize;
                    for (s, c) in k {
                        let cc = s.count as usize;
                        let lo = ti;
                        while ti < targets.len() && targets[ti] < b + cc {
                            ti += 1;
                        }
                        k2.push(go(c, b, &targets[lo..ti], f));
                        b += cc;
                    }
                    (msum(&k2), Arc::new(Node::Int(k2)))
                }
            }
        }
        let (sum, root) = go(&self.root, 0, targets, f);
        Tree { root, sum }
    }

    /// COW range delete [lo, hi). Fully-covered subtrees are dropped without
    /// allocating; untouched subtrees are reused.
    pub fn delete_range(&self, lo: usize, hi: usize) -> Tree<E> {
        fn go<E: Rec>(
            n: &Arc<Node<E>>,
            base: usize,
            s: Sum,
            lo: usize,
            hi: usize,
        ) -> Option<(Sum, Arc<Node<E>>)> {
            let count = s.count as usize;
            if hi <= base || lo >= base + count {
                return Some((s, n.clone())); // untouched
            }
            if lo <= base && base + count <= hi {
                return None; // fully deleted, zero new bytes
            }
            match &**n {
                Node::Leaf(v) => {
                    let keep: Vec<E> = v
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
    pub fn insert_block(&self, pos: usize, evs: &[E]) -> Tree<E> {
        fn base_total<E: Rec>(k: &[(Sum, Arc<Node<E>>)]) -> usize {
            k.iter().map(|(s, _)| s.count as usize).sum()
        }
        fn go<E: Rec>(
            n: &Arc<Node<E>>,
            base: usize,
            pos: usize,
            evs: &[E],
        ) -> Vec<(Sum, Arc<Node<E>>)> {
            match &**n {
                Node::Leaf(v) => {
                    let cut = pos - base;
                    let mut all: Vec<E> = Vec::with_capacity(v.len() + evs.len());
                    all.extend_from_slice(&v[..cut]);
                    all.extend_from_slice(evs);
                    all.extend_from_slice(&v[cut..]);
                    leaves_from(&all)
                }
                Node::Int(k) => {
                    let mut k2: Vec<(Sum, Arc<Node<E>>)> = Vec::new();
                    let mut b = base;
                    let mut done = false;
                    for (s, c) in k {
                        let cc = s.count as usize;
                        if !done
                            && pos >= b
                            && pos <= b + cc
                            && !(pos == b + cc && b + cc < base_total(k))
                        {
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
        let lvl = go(&self.root, 0, pos, evs);
        build_levels(lvl)
    }
}

/// Exact allocation size of one node: ArcInner header (2×usize) + Node enum
/// (tag + Vec = 32 B) + heap buffer at len (collect/to_vec allocate exact).
pub fn node_bytes<E: Rec>(n: &Node<E>) -> usize {
    match n {
        Node::Leaf(v) => 16 + 32 + v.len() * std::mem::size_of::<E>(),
        Node::Int(k) => 16 + 32 + k.len() * std::mem::size_of::<(Sum, Arc<Node<E>>)>(),
    }
}

pub type PtrSet<E> = HashSet<*const Node<E>>;

/// Insert every node reachable from `n` into `set`.
pub fn collect_ptrs<E: Rec>(n: &Arc<Node<E>>, set: &mut PtrSet<E>) {
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
/// A node already in `seen` has its whole subtree in `seen` (nodes are
/// immutable and always inserted recursively), so recursion prunes there.
pub fn walk_new<E: Rec>(new: &Arc<Node<E>>, seen: &mut PtrSet<E>) -> u64 {
    if seen.contains(&Arc::as_ptr(new)) {
        return 0;
    }
    seen.insert(Arc::as_ptr(new));
    let mut b = node_bytes(&**new) as u64;
    if let Node::Int(k) = &**new {
        for (_, c) in k {
            b += walk_new(c, seen);
        }
    }
    b
}

/// Non-destructive variant of `walk_new`: bytes reachable from `roots` that
/// are not in `seen`, without inserting. `vis` dedups shared subtrees across
/// roots (patterns can share subtrees after a COW paste).
pub fn probe_bytes<'a, E: Rec>(
    roots: impl Iterator<Item = &'a Arc<Node<E>>>,
    seen: &PtrSet<E>,
) -> u64 {
    fn go<E: Rec>(n: &Arc<Node<E>>, seen: &PtrSet<E>, vis: &mut PtrSet<E>) -> u64 {
        let p = Arc::as_ptr(n);
        if seen.contains(&p) || !vis.insert(p) {
            return 0;
        }
        let mut b = node_bytes(&**n) as u64;
        if let Node::Int(k) = &**n {
            for (_, c) in k {
                b += go(c, seen, vis);
            }
        }
        b
    }
    let mut vis = PtrSet::default();
    let mut b = 0;
    for r in roots {
        b += go(r, seen, &mut vis);
    }
    b
}
