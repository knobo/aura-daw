# Backlog: project provenance (which AURA build wrote this file)

**Opened 2026-08-30**, raised by the owner during Plan V — V2's closeout.
Not a claim, a recommendation for whenever schema work is next touched.

## What exists today

`project.json` already has `schemaVersion` (now 3) with real migration
machinery: each opener reads the number and runs the migrations it implies.
That machinery is sound and this note does not touch it.

## What is missing

Nothing in the saved file records **which AURA build wrote it.**
`CARGO_PKG_VERSION` is read in exactly one place in the whole engine — to
introduce AURA to a CLAP host — and nowhere near the project format. Open a
project from six months ago and there is no way to ask "what build produced
this" short of asking the person who saved it.

## Recommendation

Add an informational stamp beside `schemaVersion` — an `auraVersion` (or
similar) field carrying `CARGO_PKG_VERSION` at save time — that **migration
logic never reads**. `schemaVersion` stays the one thing a migration
branches on; the new field is provenance for a human (or a bug report)
asking "what wrote this", not a second axis of compatibility logic. Two
fields that both mean "how old is this file" is exactly the kind of second
source of truth this branch kept getting bitten by elsewhere (ledger:
Task 12's `midi::persist` sharing `project.json`, Task 9's tail-frames
funnel) — keep this one strictly read-only to logic so it cannot drift into
that role by accident.

## The related surprise, worth stating even though it is not this field's problem

The control-surface layout does **not** live in `project.json` at all — it
lives in the browser's `localStorage`, with its own independent
`SURFACE_LAYOUT_VERSION` (now 3). A project moved to another machine (or
opened in a fresh profile) opens with no decks: the racks, knob bindings and
pad layout the project's players and tracks assume are simply not there,
because they were never part of the document. Nobody chose this
deliberately — it fell out of the control surface predating players and
never being asked to travel with the project. Worth a deliberate decision
next time the surface or the project format is touched: either the deck
layout moves into `project.json` (making it travel with the document, at
the cost of a schema bump and a migration), or it stays local-machine state
and that is written down as intentional instead of discovered by a user
losing their decks.
