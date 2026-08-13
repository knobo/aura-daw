# bulkbench

Exact retained-bytes measurement for the COW B-tree session history
(leaf 128 / branch 32) — the crate behind the bulk-op numbers in
`docs/research/06-time-travel-storage.md` (CORRECTIONS block) and the
weighted gesture-mix study that closes `docs/CORE-REDESIGN-ROUND-2.md`
§10.2 open question 2. Results and analysis: **[RESULTS.md](RESULTS.md)**.

## What it measures

Unlike `benches/pdsbench` (RSS deltas), every byte count here is **exact**:
new versions are pointer-diffed against everything previously reachable
(`Arc::as_ptr` sets), and each new node is charged its true allocation size
(ArcInner header 16 B + `Node` enum 32 B + exact heap buffer). Replaced
roots go to a graveyard so allocator address reuse can never corrupt the
pointer-set accounting. Byte counts are deterministic and reproduce
byte-exact across runs; µs figures are single-shot and ±40 % (see the
dossier's timing correction).

Two record types:

* `Ev16` — 16 B, the record the original dossier measured (verification).
* `Ev20` — 16 B + `note_id: u32` = 20 B, the canonical record once
  round 2 §2.1's note identity lands.

Two session shapes:

* single tree of 10⁶ events (the dossier's configuration);
* **per-pattern**: 200 trees × 5 000 events (how AURA actually stores
  content), including the COW pattern-map path in every charge.

The simulation (`sim` mode) runs 10 000-gesture weighted mixes (HUMAN and
AGENT profiles, weights in `src/sim.rs` and justified in RESULTS.md)
against the per-pattern session under seven retention configurations:
no replay-only, class-based replay-only (round 2 §6 as written) at
64 KB / 256 KB / 1 MB, and charge-based replay-only with a forced capture
every 32 nodes at the same caps. Retention is modelled honestly: a
materialized node retains an `Arc` to the *whole* session, so it captures
the surviving output of preceding replay-only nodes — see RESULTS.md §5,
this is the study's central finding.

## Running

```sh
cargo run --release -- verify   # reproduce the previous round: 3 968 B point,
                                # 1 661 344 B quantize-100k (exits 1 on mismatch)
cargo run --release -- suites   # record-size + granularity op tables
cargo run --release -- sim      # the four 10 000-gesture profile runs
cargo run --release -- all      # everything (default); ~10 s, peak RSS ~1.6 GB
```

`run-output.txt` is the captured output of `all` that RESULTS.md quotes.
