//! bulkbench — bulk-op amplification + weighted gesture-mix simulation for
//! the COW B-tree session history (docs/CORE-REDESIGN-ROUND-2.md §6, §10.2;
//! docs/research/06-time-travel-storage.md §3 Falsifier 1).
//!
//! Modes (cargo run --release -- <mode>):
//!   verify   reproduce the previous round's 16-byte single-tree numbers
//!            (expect point 3 968 B, quantize-100k 1 661 344 B)
//!   suites   single-tree suites at 16 B and 20 B + per-pattern suite at
//!            20 B (200 patterns × 5 000 events) for the same logical ops
//!   sim      10 000-gesture HUMAN and AGENT profiles × retention rules
//!   all      everything (default)
//!
//! All byte counts are EXACT: new Arc nodes found by pointer-diffing against
//! everything previously reachable, charged at true allocation size
//! (ArcInner 16 B + enum 32 B + exact heap buffer).
mod sim;
mod tree;

use sim::*;
use std::sync::Arc;
use std::time::Instant;
use tree::*;

// ------------------------------------------------------------ single-tree
/// The previous round's op suite, generic over the record. Returns
/// (point_bytes, quantize100k_bytes) for verification.
fn suite_single<E: Rec>(label: &str) -> (u64, u64) {
    const N: usize = 1_000_000;
    let base: Tree<E> = Tree::build(N);
    let mut seen: PtrSet<E> = PtrSet::default();
    collect_ptrs(&base.root, &mut seen);
    let total_nodes = seen.len();
    let total_bytes = {
        let mut s2 = PtrSet::default();
        walk_new(&base.root, &mut s2)
    };
    println!(
        "== single tree, {} ==\nbase: {} events, {} nodes, {:.1} MB ({} leaves of {})",
        label,
        N,
        total_nodes,
        total_bytes as f64 / 1e6,
        (N + LEAF - 1) / LEAF,
        LEAF
    );
    println!();
    println!(
        "{:<44} {:>12} {:>10} {:>12} {:>10}",
        "op", "retained B", "µs/op", "amp vs pt", "B/note"
    );

    let mut baseline_pt = 0u64;

    let mut run = |name: &str, notes: usize, expect: usize, op: &dyn Fn(&Tree<E>) -> Tree<E>| -> u64 {
        let mut seen = seen.clone();
        let t = Instant::now();
        let v2 = op(&base);
        let us = t.elapsed().as_secs_f64() * 1e6;
        let rb = walk_new(&v2.root, &mut seen);
        assert_eq!(v2.count(), expect, "{}", name);
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
        rb
    };

    let point = run("point edit", 1, N, &|t| t.transform(&[500_000], &|e| e.set_vel(40)));

    let idx: Vec<usize> = (400_000..500_000).collect();
    let q100k = run("quantize 100k contiguous", 100_000, N, &move |t| {
        t.transform(&idx, &|e| e.quantize_tick(120))
    });

    let idx: Vec<usize> = (400_000..410_000).collect();
    run("quantize 10k contiguous", 10_000, N, &move |t| {
        t.transform(&idx, &|e| e.quantize_tick(120))
    });

    let idx: Vec<usize> = (700_000..701_000).collect();
    run("transpose 1k contiguous", 1_000, N, &move |t| {
        t.transform(&idx, &|e| e.shift_key(2))
    });

    let mut idx: Vec<usize> =
        (0..1_000usize).map(|i| (i.wrapping_mul(2654435761) >> 2) % N).collect();
    idx.sort_unstable();
    idx.dedup();
    let n5 = idx.len();
    run("transpose 1k scattered over 1M", n5, N, &move |t| {
        t.transform(&idx, &|e| e.shift_key(2))
    });

    let mut idx: Vec<usize> = (0..10_000usize)
        .map(|i| 300_000 + (i.wrapping_mul(2654435761) >> 2) % 20_000)
        .collect();
    idx.sort_unstable();
    idx.dedup();
    let n6 = idx.len();
    run("humanize 10k scattered in 20k window", n6, N, &move |t| {
        t.transform(&idx, &|e| e.jitter_tick(0))
    });

    let mut idx: Vec<usize> =
        (0..10_000usize).map(|i| (i.wrapping_mul(2654435761) >> 2) % N).collect();
    idx.sort_unstable();
    idx.dedup();
    let n7 = idx.len();
    run("humanize 10k scattered over 1M", n7, N, &move |t| {
        t.transform(&idx, &|e| e.jitter_tick(0))
    });

    run("delete 50k contiguous", 50_000, N - 50_000, &|t| t.delete_range(600_000, 650_000));

    let paste: Vec<E> = (0..20_000).map(|i| E::mk(i as u32 + 2_000_000)).collect();
    run("paste 20k notes", 20_000, N + 20_000, &move |t| t.insert_block(250_016, &paste));

    println!();
    (point, q100k)
}

// ------------------------------------------------------------ per-pattern
/// Same logical gestures against the granular session: 200 pattern trees of
/// 5 000 events each (how AURA actually stores content). Retained bytes
/// include the pattern-map COW path (MAP_ROOT + touched groups × MAP_GROUP).
fn suite_pattern() {
    type E = Ev20;
    let base: Session<E> = Session::build();
    let mut base_seen: PtrSet<E> = PtrSet::default();
    for t in &base.pats {
        collect_ptrs(&t.root, &mut base_seen);
    }
    let base_bytes: u64 = probe_bytes(base.pats.iter().map(|t| &t.root), &PtrSet::default());
    println!(
        "== per-pattern session, Ev20 (20 B) == {} patterns x {} events = {} total, base {:.1} MB",
        N_PATTERNS,
        PATTERN_EVENTS,
        N_PATTERNS * PATTERN_EVENTS,
        base_bytes as f64 / 1e6
    );
    println!();
    println!(
        "{:<46} {:>12} {:>10} {:>12} {:>10}",
        "op", "retained B", "µs/op", "amp vs pt", "B/note"
    );

    let mut baseline_pt = 0u64;
    let mut rng = Rng(7);

    // Each op runs against a pristine clone of the base session.
    let mut run = |name: &str,
                   notes: usize,
                   op: &dyn Fn(&mut Vec<Tree<E>>, &mut Rng) -> Vec<usize>| {
        let mut pats = base.pats.clone();
        let mut seen = base_seen.clone();
        let t = Instant::now();
        let touched = op(&mut pats, &mut rng);
        let us = t.elapsed().as_secs_f64() * 1e6;
        let mut rb = Session::<E>::map_bytes(&touched);
        for &p in &touched {
            if p < pats.len() {
                rb += walk_new(&pats[p].root, &mut seen);
            }
        }
        if baseline_pt == 0 {
            baseline_pt = rb;
        }
        println!(
            "{:<46} {:>12} {:>10.1} {:>11.1}x {:>10.1}",
            name,
            rb,
            us,
            rb as f64 / baseline_pt as f64,
            rb as f64 / notes as f64
        );
    };

    run("point edit (1 note, 1 pattern)", 1, &|pats, _| {
        pats[100] = pats[100].transform(&[2500], &|e| e.set_vel(40));
        vec![100]
    });

    run("quantize 100k contiguous (20 whole patterns)", 100_000, &|pats, _| {
        for p in 40..60 {
            let idx: Vec<usize> = (0..pats[p].count()).collect();
            pats[p] = pats[p].transform(&idx, &|e| e.quantize_tick(120));
        }
        (40..60).collect()
    });

    run("quantize 10k contiguous (2 whole patterns)", 10_000, &|pats, _| {
        for p in 80..82 {
            let idx: Vec<usize> = (0..pats[p].count()).collect();
            pats[p] = pats[p].transform(&idx, &|e| e.quantize_tick(120));
        }
        (80..82).collect()
    });

    run("transpose 1k contiguous (within 1 pattern)", 1_000, &|pats, _| {
        let idx: Vec<usize> = (2000..3000).collect();
        pats[120] = pats[120].transform(&idx, &|e| e.shift_key(2));
        vec![120]
    });

    run("transpose 1k scattered over session (5/pat)", 1_000, &|pats, rng| {
        for p in 0..N_PATTERNS {
            let mut idx: Vec<usize> =
                (0..5).map(|_| rng.below(PATTERN_EVENTS as u64) as usize).collect();
            idx.sort_unstable();
            idx.dedup();
            pats[p] = pats[p].transform(&idx, &|e| e.shift_key(2));
        }
        (0..N_PATTERNS).collect()
    });

    run("humanize 10k scattered in 4-pattern window", 10_000, &|pats, rng| {
        for p in 60..64 {
            let mut idx: Vec<usize> =
                (0..2500).map(|_| rng.below(PATTERN_EVENTS as u64) as usize).collect();
            idx.sort_unstable();
            idx.dedup();
            pats[p] = pats[p].transform(&idx, &|e| e.jitter_tick(1));
        }
        (60..64).collect()
    });

    run("humanize 10k scattered over session (50/pat)", 10_000, &|pats, rng| {
        for p in 0..N_PATTERNS {
            let mut idx: Vec<usize> =
                (0..50).map(|_| rng.below(PATTERN_EVENTS as u64) as usize).collect();
            idx.sort_unstable();
            idx.dedup();
            pats[p] = pats[p].transform(&idx, &|e| e.jitter_tick(1));
        }
        (0..N_PATTERNS).collect()
    });

    run("delete 50k contiguous (drop 10 whole patterns)", 50_000, &|pats, _| {
        for p in 20..30 {
            let c = pats[p].count();
            pats[p] = pats[p].delete_range(0, c);
        }
        (20..30).collect()
    });

    run("paste 20k as NEW content (4 patterns)", 20_000, &|pats, _| {
        for (i, p) in (140..144).enumerate() {
            pats[p] = Tree::build_from(3_000_000 + (i as u32) * 5_000, PATTERN_EVENTS);
        }
        (140..144).collect()
    });

    run("paste 20k as COW duplicate (linked content)", 20_000, &|pats, _| {
        for (i, p) in (150..154).enumerate() {
            pats[p] = pats[i].clone();
        }
        (150..154).collect()
    });

    println!();
}

// ------------------------------------------------------------ simulation
fn print_profile(name: &str, res: &ProfileResult) {
    println!("== profile {} — 10 000 gestures, per-pattern session, Ev20 ==", name);
    println!();
    println!("per-op-class created bytes (evidence for the caps):");
    println!(
        "  {:<16} {:>6} {:>14} {:>14} {:>14}",
        "gesture", "n", "mean B", "p99 B", "max B"
    );
    for (k, n, mean, p, max) in &res.per_kind {
        println!("  {:<16} {:>6} {:>14} {:>14} {:>14}", k, n, mean, p, max);
    }
    println!();
    println!(
        "  {:<16} {:>12} {:>12} {:>13} {:>13} {:>12} {:>13} {:>12} {:>7} {:>6} {:>7}",
        "retention",
        "mean B/node",
        "p99 B/node",
        "max B/node",
        "snapshots B",
        "op-log B",
        "total B",
        "steps@512M",
        "replay%",
        "chain",
        "forced"
    );
    for c in &res.cfgs {
        println!(
            "  {:<16} {:>12} {:>12} {:>13} {:>13} {:>12} {:>13} {:>12} {:>6.1}% {:>6} {:>7}",
            c.name,
            c.mean,
            c.p99,
            c.max,
            c.snap_total,
            c.oplog_total,
            c.total,
            c.steps_512,
            c.replay_pct,
            c.max_chain,
            c.forced
        );
    }
    println!();
}

fn run_sim() {
    let t = Instant::now();
    let human = run_profile::<Ev20>("HUMAN", HUMAN, 800, 0xAA01);
    println!("(HUMAN simulated in {:.1} s)\n", t.elapsed().as_secs_f64());
    print_profile("HUMAN", &human);

    let t = Instant::now();
    let agent = run_profile::<Ev20>("AGENT", AGENT, 650, 0xBB02);
    println!("(AGENT simulated in {:.1} s)\n", t.elapsed().as_secs_f64());
    print_profile("AGENT", &agent);

    let t = Instant::now();
    let human_v = run_profile::<Ev20>("HUMAN_V", HUMAN_V, 800, 0xAA01);
    println!("(HUMAN_V simulated in {:.1} s)\n", t.elapsed().as_secs_f64());
    print_profile("HUMAN + placement-offset transpose (round 2 §5 field)", &human_v);

    let t = Instant::now();
    let agent_v = run_profile::<Ev20>("AGENT_V", AGENT_V, 650, 0xBB02);
    println!("(AGENT_V simulated in {:.1} s)\n", t.elapsed().as_secs_f64());
    print_profile("AGENT + placement-offset transpose (round 2 §5 field)", &agent_v);
}

fn verify() {
    println!("== verification against the previous round (16 B record, single tree) ==\n");
    let (point, q100k) = suite_single::<Ev16>("Ev16 (16 B)");
    let ok1 = point == 3_968;
    let ok2 = q100k == 1_661_344;
    println!(
        "point edit      = {:>9} B  (expected 3 968)      {}",
        point,
        if ok1 { "PASS" } else { "FAIL" }
    );
    println!(
        "quantize 100k   = {:>9} B  (expected 1 661 344)  {}",
        q100k,
        if ok2 { "PASS" } else { "FAIL" }
    );
    if !(ok1 && ok2) {
        std::process::exit(1);
    }
    println!();
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    match mode.as_str() {
        "verify" => verify(),
        "suites" => {
            suite_single::<Ev20>("Ev20 (20 B, canonical: +note_id u32)");
            suite_pattern();
        }
        "sim" => run_sim(),
        "all" => {
            verify();
            suite_single::<Ev20>("Ev20 (20 B, canonical: +note_id u32)");
            suite_pattern();
            run_sim();
        }
        m => {
            eprintln!("unknown mode {m}; use verify|suites|sim|all");
            std::process::exit(2);
        }
    }
    // keep Arc in scope for typed drops (silences unused import on some paths)
    let _: Option<Arc<()>> = None;
}
