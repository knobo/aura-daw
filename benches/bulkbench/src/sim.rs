//! Weighted gesture-mix simulation against a per-pattern session
//! (round 2 §10.2): 200 patterns × 5 000 events, tree per pattern,
//! 20-byte records, exact retained-bytes accounting by pointer diffing.
//!
//! Retention semantics modelled honestly:
//! - A materialized history node retains an Arc to the WHOLE session
//!   (all pattern roots), exactly like `HistoryNode.session: Arc<Session>`.
//!   Therefore a materialized node CAPTURES the surviving output of any
//!   preceding replay-only nodes — structural sharing means those bytes are
//!   charged to the next materialized node, not annihilated.
//! - A replay-only node stores only op + inverse (the op log, which every
//!   node keeps anyway per dossier §1.4); it retains no snapshot.
//! - Live memory attributable to history = union of tree nodes reachable
//!   from retained snapshots that are NOT part of the pristine base session,
//!   plus the op log. Bytes reachable only from the current (live) document
//!   are the document's own cost and are excluded.
use crate::tree::*;
use std::collections::HashMap;
use std::sync::Arc;

pub const N_PATTERNS: usize = 200;
pub const PATTERN_EVENTS: usize = 5_000;
pub const GESTURES: usize = 10_000;

/// Analytic cost of the persistent pattern map (imbl-HAMT-shaped, 200
/// entries, fanout 32): one root (7 children) + one group node per touched
/// group of 32. root = 16+32+7*24 = 216 B, group = 16+32+32*24 = 816 B.
pub const MAP_ROOT: u64 = 216;
pub const MAP_GROUP: u64 = 816;

/// Per-history-node fixed overhead (HistoryNode struct: parent, children
/// SmallVec, label, timestamps, bytes field, Arc<Session> slot).
pub const NODE_FIXED: u64 = 96;

// ---------------------------------------------------------------- RNG
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    /// uniform in [lo, hi)
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo)
    }
}

// ---------------------------------------------------------------- session
pub struct Session<E: Rec> {
    pub pats: Vec<Tree<E>>,
    /// Replaced roots are kept alive so raw pointers are never reused and
    /// pointer-set accounting stays exact across the whole run.
    pub grave: Vec<Arc<Node<E>>>,
    pub next_ev: u32,
}

impl<E: Rec> Session<E> {
    pub fn build() -> Self {
        let pats: Vec<Tree<E>> = (0..N_PATTERNS)
            .map(|p| Tree::build_from((p * PATTERN_EVENTS) as u32, PATTERN_EVENTS))
            .collect();
        Session { pats, grave: Vec::new(), next_ev: (N_PATTERNS * PATTERN_EVENTS) as u32 }
    }
    pub fn set(&mut self, p: usize, t: Tree<E>) {
        let old = std::mem::replace(&mut self.pats[p], t);
        self.grave.push(old.root);
    }
    pub fn fresh_events(&mut self, n: usize) -> Vec<E> {
        let s = self.next_ev;
        self.next_ev += n as u32;
        (0..n).map(|i| E::mk(s + i as u32)).collect()
    }
    pub fn map_bytes(touched: &[usize]) -> u64 {
        let mut groups: Vec<usize> = touched.iter().map(|i| i / 32).collect();
        groups.sort_unstable();
        groups.dedup();
        MAP_ROOT + groups.len() as u64 * MAP_GROUP
    }
}

// ---------------------------------------------------------------- gestures
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum G {
    PointEdit,
    DrawNote,
    DeleteNote,
    SmallDrag,
    PasteBar,
    QuantizeSel,
    ScatteredSel,
    QuantizeClip,
    HumanizeClip,
    TransposeClip,
    SongWide,
    Regen,
    PastePattern,
    ScatteredCond,
    MultiClip,
    InsertPhrase,
    DeleteRangeOp,
    /// Transpose-class ops routed through the PLACEMENT transpose/velocity
    /// offset field (round 2 §5) instead of rewriting leaves: the history
    /// node retains only the placement-map COW path.
    SongWideView,
    TransposeClipView,
    MultiClipView,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Point,
    Bulk,
    Scattered,
}

pub struct Applied {
    pub kind: G,
    pub class: Class,
    pub touched: Vec<usize>,
    pub oplog: u64,
}

/// Weights in per-mille. THESE ARE THE DELIVERABLE — see RESULTS.md for the
/// grounding of every line.
pub const HUMAN: &[(G, u64)] = &[
    (G::PointEdit, 420),
    (G::DrawNote, 220),
    (G::DeleteNote, 100),
    (G::SmallDrag, 100),
    (G::PasteBar, 50),
    (G::QuantizeSel, 60),
    (G::ScatteredSel, 35),
    (G::HumanizeClip, 8),
    (G::QuantizeClip, 5),
    (G::SongWide, 2),
];

pub const AGENT: &[(G, u64)] = &[
    (G::Regen, 180),
    (G::QuantizeClip, 140),
    (G::ScatteredCond, 120),
    (G::InsertPhrase, 120),
    (G::HumanizeClip, 80),
    (G::TransposeClip, 80),
    (G::MultiClip, 80),
    (G::PointEdit, 80),
    (G::PastePattern, 70),
    (G::DeleteRangeOp, 50),
];

/// HUMAN with the one change round 2 §5 already makes possible: song-wide
/// transpose rides the placement offset field instead of rewriting leaves.
pub const HUMAN_V: &[(G, u64)] = &[
    (G::PointEdit, 420),
    (G::DrawNote, 220),
    (G::DeleteNote, 100),
    (G::SmallDrag, 100),
    (G::PasteBar, 50),
    (G::QuantizeSel, 60),
    (G::ScatteredSel, 35),
    (G::HumanizeClip, 8),
    (G::QuantizeClip, 5),
    (G::SongWideView, 2),
];

/// AGENT with transpose-class ops as placement-offset edits. Quantize,
/// humanize, scattered-conditional and regeneration still rewrite leaves —
/// they change per-note data and cannot ride an offset field. MultiClip's
/// weight is split: half transpose-flavoured (view), half quantize-flavoured
/// (tree rewrite).
pub const AGENT_V: &[(G, u64)] = &[
    (G::Regen, 180),
    (G::QuantizeClip, 140),
    (G::ScatteredCond, 120),
    (G::InsertPhrase, 120),
    (G::HumanizeClip, 80),
    (G::TransposeClipView, 80),
    (G::MultiClip, 40),
    (G::MultiClipView, 40),
    (G::PointEdit, 80),
    (G::PastePattern, 70),
    (G::DeleteRangeOp, 50),
];

pub fn pick(table: &[(G, u64)], rng: &mut Rng) -> G {
    let total: u64 = table.iter().map(|(_, w)| w).sum();
    let mut r = rng.below(total);
    for &(g, w) in table {
        if r < w {
            return g;
        }
        r -= w;
    }
    table.last().unwrap().0
}

fn scattered_idx(rng: &mut Rng, count: usize, k: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..k).map(|_| rng.below(count as u64) as usize).collect();
    idx.sort_unstable();
    idx.dedup();
    idx
}

/// Apply one gesture. `pat` is the sticky target pattern. Returns what was
/// touched plus the op-log (op + inverse) byte model for the node.
pub fn apply<E: Rec>(sess: &mut Session<E>, g: G, pat: usize, rng: &mut Rng) -> Applied {
    let cnt = sess.pats[pat].count();
    // A pattern emptied by earlier deletes gets regenerated instead
    // (matches real behaviour: nobody keeps editing an empty clip).
    let g = if cnt < 64
        && !matches!(
            g,
            G::Regen
                | G::PastePattern
                | G::SongWide
                | G::MultiClip
                | G::SongWideView
                | G::TransposeClipView
                | G::MultiClipView
        ) {
        G::Regen
    } else {
        g
    };
    let cnt = sess.pats[pat].count();
    match g {
        G::PointEdit => {
            let i = rng.below(cnt as u64) as usize;
            let t = sess.pats[pat].transform(&[i], &|e| e.set_vel(90));
            sess.set(pat, t);
            Applied { kind: g, class: Class::Point, touched: vec![pat], oplog: 64 }
        }
        G::DrawNote => {
            let pos = rng.below(cnt as u64 + 1) as usize;
            let evs = sess.fresh_events(1);
            let t = sess.pats[pat].insert_block(pos, &evs);
            sess.set(pat, t);
            Applied { kind: g, class: Class::Point, touched: vec![pat], oplog: 64 }
        }
        G::DeleteNote => {
            let i = rng.below(cnt as u64) as usize;
            let t = sess.pats[pat].delete_range(i, i + 1);
            sess.set(pat, t);
            Applied { kind: g, class: Class::Point, touched: vec![pat], oplog: 56 }
        }
        G::SmallDrag => {
            let k = rng.range(8, 33) as usize;
            let k = k.min(cnt);
            let lo = rng.below((cnt - k + 1) as u64) as usize;
            let idx: Vec<usize> = (lo..lo + k).collect();
            let t = sess.pats[pat].transform(&idx, &|e| e.jitter_tick(3));
            sess.set(pat, t);
            Applied { kind: g, class: Class::Bulk, touched: vec![pat], oplog: 32 + 4 * k as u64 }
        }
        G::PasteBar | G::InsertPhrase => {
            let k = if g == G::PasteBar { rng.range(32, 257) } else { rng.range(50, 301) } as usize;
            let pos = rng.below(cnt as u64 + 1) as usize;
            let evs = sess.fresh_events(k);
            let t = sess.pats[pat].insert_block(pos, &evs);
            sess.set(pat, t);
            // op payload carries the notes themselves (20 B each); inverse
            // is a delete-by-range, O(1).
            Applied { kind: g, class: Class::Bulk, touched: vec![pat], oplog: 32 + 20 * k as u64 }
        }
        G::QuantizeSel => {
            let k = rng.range(50, 501) as usize;
            let k = k.min(cnt);
            let lo = rng.below((cnt - k + 1) as u64) as usize;
            let idx: Vec<usize> = (lo..lo + k).collect();
            let t = sess.pats[pat].transform(&idx, &|e| e.quantize_tick(120));
            sess.set(pat, t);
            // inverse: 4 B tick delta + 4 B id per note
            Applied { kind: g, class: Class::Bulk, touched: vec![pat], oplog: 32 + 8 * k as u64 }
        }
        G::ScatteredSel => {
            let k = rng.range(20, 201) as usize;
            let idx = scattered_idx(rng, cnt, k);
            let n = idx.len();
            let t = sess.pats[pat].transform(&idx, &|e| e.shift_key(2));
            sess.set(pat, t);
            // transpose inverse is O(1) + selection ids
            Applied { kind: g, class: Class::Scattered, touched: vec![pat], oplog: 32 + 4 * n as u64 }
        }
        G::ScatteredCond => {
            let k = (cnt as u64 * rng.range(30, 51) / 100) as usize;
            let idx = scattered_idx(rng, cnt, k);
            let n = idx.len();
            let t = sess.pats[pat].transform(&idx, &|e| e.set_vel(70));
            sess.set(pat, t);
            // inverse: old vel (1 B) + id (4 B) per note, padded
            Applied { kind: g, class: Class::Scattered, touched: vec![pat], oplog: 32 + 8 * n as u64 }
        }
        G::QuantizeClip => {
            let idx: Vec<usize> = (0..cnt).collect();
            let t = sess.pats[pat].transform(&idx, &|e| e.quantize_tick(120));
            sess.set(pat, t);
            // whole-clip selection is implicit; inverse = 4 B delta per note
            Applied { kind: g, class: Class::Bulk, touched: vec![pat], oplog: 32 + 4 * cnt as u64 }
        }
        G::HumanizeClip => {
            let salt = rng.next() as u32;
            let idx: Vec<usize> = (0..cnt).collect();
            let t = sess.pats[pat].transform(&idx, &|e| e.jitter_tick(salt));
            sess.set(pat, t);
            // seeded PRNG: inverse is O(1) (the §6 constraint)
            Applied { kind: g, class: Class::Bulk, touched: vec![pat], oplog: 40 }
        }
        G::TransposeClip => {
            let idx: Vec<usize> = (0..cnt).collect();
            let t = sess.pats[pat].transform(&idx, &|e| e.shift_key(2));
            sess.set(pat, t);
            Applied { kind: g, class: Class::Bulk, touched: vec![pat], oplog: 40 }
        }
        G::SongWide => {
            let mut touched = Vec::with_capacity(N_PATTERNS);
            for p in 0..N_PATTERNS {
                let c = sess.pats[p].count();
                if c == 0 {
                    continue;
                }
                let idx: Vec<usize> = (0..c).collect();
                let t = sess.pats[p].transform(&idx, &|e| e.shift_key(2));
                sess.set(p, t);
                touched.push(p);
            }
            Applied { kind: g, class: Class::Bulk, touched, oplog: 40 }
        }
        G::MultiClip => {
            let m = rng.range(4, 17) as usize;
            let s = rng.below((N_PATTERNS - m) as u64) as usize;
            let mut touched = Vec::with_capacity(m);
            for p in s..s + m {
                let c = sess.pats[p].count();
                if c == 0 {
                    continue;
                }
                let idx: Vec<usize> = (0..c).collect();
                let t = sess.pats[p].transform(&idx, &|e| e.shift_key(1));
                sess.set(p, t);
                touched.push(p);
            }
            Applied { kind: g, class: Class::Bulk, touched, oplog: 32 + 8 * m as u64 }
        }
        G::Regen => {
            let evs = sess.fresh_events(PATTERN_EVENTS);
            let t = Tree::build(0).insert_block(0, &evs); // build fresh leaves
            sess.set(pat, t);
            // generated content is blob-backed (content-addressed store, on
            // disk): the op log holds ids + blob refs only.
            Applied { kind: g, class: Class::Bulk, touched: vec![pat], oplog: 160 }
        }
        G::PastePattern => {
            // COW duplicate: the new placement SHARES the source content
            // tree by Arc (round 2 §5: linked placements share ContentId by
            // construction) — retained bytes ≈ map path only.
            let src = rng.below(N_PATTERNS as u64) as usize;
            let t = sess.pats[src].clone();
            sess.set(pat, t);
            Applied { kind: g, class: Class::Bulk, touched: vec![pat], oplog: 96 }
        }
        G::SongWideView => {
            // placement offset on every placement: map rows rewritten, no
            // leaf touched. Retained = COW map path only.
            Applied { kind: g, class: Class::Point, touched: (0..N_PATTERNS).collect(), oplog: 48 }
        }
        G::TransposeClipView => {
            Applied { kind: g, class: Class::Point, touched: vec![pat], oplog: 40 }
        }
        G::MultiClipView => {
            let m = rng.range(4, 17) as usize;
            let s = rng.below((N_PATTERNS - m) as u64) as usize;
            Applied {
                kind: g,
                class: Class::Point,
                touched: (s..s + m).collect(),
                oplog: 32 + 8 * m as u64,
            }
        }
        G::DeleteRangeOp => {
            let k = rng.range(100, 1001) as usize;
            let k = k.min(cnt);
            let lo = rng.below((cnt - k + 1) as u64) as usize;
            let t = sess.pats[pat].delete_range(lo, lo + k);
            sess.set(pat, t);
            // inverse must carry the deleted notes: 24 B/note in-RAM
            Applied { kind: g, class: Class::Bulk, touched: vec![pat], oplog: 32 + 24 * k as u64 }
        }
    }
}

// ---------------------------------------------------------------- retention
#[derive(Clone, Copy, PartialEq)]
pub enum Rule {
    /// Every node retains a snapshot (no replay-only mechanism).
    AllMaterialized,
    /// Round 2 §6 as written: replay-only iff the op class is
    /// bulk-or-scattered AND the op's own created bytes exceed the cap.
    ClassCreated(u64),
    /// Charge-based: replay-only iff the node's actual incremental retained
    /// charge (including bytes inherited from preceding replay-only nodes)
    /// exceeds the cap — with a forced capture every 32 nodes so replay
    /// chains stay hover-safe.
    ChargeCadence(u64),
}

pub struct CfgState<E: Rec> {
    pub rule: Rule,
    pub name: String,
    pub seen: PtrSet<E>,
    pub node_bytes: Vec<u64>, // per-node retained (snapshot charge + oplog + fixed)
    pub snap_total: u64,
    pub oplog_total: u64,
    pub replay: usize,
    pub chain: usize,
    pub max_chain: usize,
    pub forced: usize,
}

pub struct ProfileResult {
    pub cfgs: Vec<CfgSummary>,
    pub per_kind: Vec<(String, usize, u64, u64, u64)>, // kind, n, mean, p99, max created
}

pub struct CfgSummary {
    pub name: String,
    pub mean: u64,
    pub p99: u64,
    pub max: u64,
    pub snap_total: u64,
    pub oplog_total: u64,
    pub total: u64,
    pub steps_512: u64,
    pub replay_pct: f64,
    pub max_chain: usize,
    pub forced: usize,
}

fn p99(sorted: &[u64]) -> u64 {
    sorted[(sorted.len() * 99) / 100 - 1]
}

pub fn run_profile<E: Rec>(
    label: &str,
    table: &[(G, u64)],
    sticky_permille: u64,
    seed: u64,
) -> ProfileResult {
    let mut sess: Session<E> = Session::build();
    let mut ever: PtrSet<E> = PtrSet::default();
    for t in &sess.pats {
        collect_ptrs(&t.root, &mut ever);
    }
    let base_seen = ever.clone();

    let caps = [64 * 1024u64, 256 * 1024, 1024 * 1024];
    let mut cfgs: Vec<CfgState<E>> = Vec::new();
    cfgs.push(CfgState {
        rule: Rule::AllMaterialized,
        name: "no replay-only".into(),
        seen: base_seen.clone(),
        node_bytes: Vec::new(),
        snap_total: 0,
        oplog_total: 0,
        replay: 0,
        chain: 0,
        max_chain: 0,
        forced: 0,
    });
    for &c in &caps {
        cfgs.push(CfgState {
            rule: Rule::ClassCreated(c),
            name: format!("A class>{:>4}K", c / 1024),
            seen: base_seen.clone(),
            node_bytes: Vec::new(),
            snap_total: 0,
            oplog_total: 0,
            replay: 0,
            chain: 0,
            max_chain: 0,
            forced: 0,
        });
    }
    for &c in &caps {
        cfgs.push(CfgState {
            rule: Rule::ChargeCadence(c),
            name: format!("B charge>{:>4}K", c / 1024),
            seen: base_seen.clone(),
            node_bytes: Vec::new(),
            snap_total: 0,
            oplog_total: 0,
            replay: 0,
            chain: 0,
            max_chain: 0,
            forced: 0,
        });
    }

    let mut rng = Rng(seed);
    let mut sticky_pat = 0usize;
    let mut created_by_kind: HashMap<G, Vec<u64>> = HashMap::new();

    for step in 0..GESTURES {
        let g = pick(table, &mut rng);
        if rng.below(1000) >= sticky_permille {
            sticky_pat = rng.below(N_PATTERNS as u64) as usize;
        }
        let ap = apply(&mut sess, g, sticky_pat, &mut rng);
        let map_b = Session::<E>::map_bytes(&ap.touched);
        // Bytes freshly allocated by THIS op (vs everything ever created).
        let mut created = map_b;
        for &p in &ap.touched {
            created += walk_new(&sess.pats[p].root, &mut ever);
        }
        created_by_kind.entry(ap.kind).or_default().push(created);

        for cfg in cfgs.iter_mut() {
            let (replay, forced) = match cfg.rule {
                Rule::AllMaterialized => (false, false),
                Rule::ClassCreated(cap) => (ap.class != Class::Point && created > cap, false),
                Rule::ChargeCadence(cap) => {
                    let pb = probe_bytes(sess.pats.iter().map(|t| &t.root), &cfg.seen) + map_b;
                    if pb > cap {
                        if cfg.chain >= 32 {
                            (false, true) // forced capture: bound replay distance
                        } else {
                            (true, false)
                        }
                    } else {
                        (false, false)
                    }
                }
            };
            let charge = if replay {
                cfg.replay += 1;
                cfg.chain += 1;
                cfg.max_chain = cfg.max_chain.max(cfg.chain);
                0
            } else {
                if forced {
                    cfg.forced += 1;
                }
                cfg.chain = 0;
                // A snapshot retains the WHOLE session: charge every byte
                // reachable from the current roots not yet retained —
                // including surviving output of preceding replay-only nodes.
                let mut c = map_b;
                for t in &sess.pats {
                    c += walk_new(&t.root, &mut cfg.seen);
                }
                c
            };
            cfg.snap_total += charge;
            cfg.oplog_total += ap.oplog + NODE_FIXED;
            cfg.node_bytes.push(charge + ap.oplog + NODE_FIXED);
        }
        let _ = step;
    }

    // summaries
    let budget = 512u64 * 1024 * 1024;
    let mut out = Vec::new();
    for cfg in cfgs.iter_mut() {
        let total = cfg.snap_total + cfg.oplog_total;
        let mut sorted = cfg.node_bytes.clone();
        sorted.sort_unstable();
        out.push(CfgSummary {
            name: cfg.name.clone(),
            mean: total / GESTURES as u64,
            p99: p99(&sorted),
            max: *sorted.last().unwrap(),
            snap_total: cfg.snap_total,
            oplog_total: cfg.oplog_total,
            total,
            steps_512: (budget as f64 / (total as f64 / GESTURES as f64)) as u64,
            replay_pct: 100.0 * cfg.replay as f64 / GESTURES as f64,
            max_chain: cfg.max_chain,
            forced: cfg.forced,
        });
    }

    // per-kind created stats (op-class cap evidence)
    let mut per_kind: Vec<(String, usize, u64, u64, u64)> = created_by_kind
        .into_iter()
        .map(|(k, mut v)| {
            v.sort_unstable();
            let n = v.len();
            let mean = v.iter().sum::<u64>() / n as u64;
            let p = v[(n * 99 / 100).saturating_sub(1).min(n - 1)];
            (format!("{:?}", k), n, mean, p, *v.last().unwrap())
        })
        .collect();
    per_kind.sort_by(|a, b| b.4.cmp(&a.4));

    let _ = label;
    ProfileResult { cfgs: out, per_kind }
}
