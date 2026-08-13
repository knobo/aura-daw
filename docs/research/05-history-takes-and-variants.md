# AURA — History, Takes and Variants (research dossier)

> **Status: research input, not a specification.** Nothing here binds code.
> The design that does will be written separately; this is the evidence it
> rests on.
> **Date:** 2026-08-13.
> **Provenance:** eight parallel research agents against primary sources —
> vendor manuals (Pro Tools Reference Guide 2024.10, Logic Pro User Guide,
> REAPER User Guide 7.78, Bitwig Studio User Guide, Ardour manual,
> ableton.com/manual, steinberg.help), source code read through the GitHub
> API (Ardour, Zrythm 1.x/2.x, openDAW, JUCE, Blender, Krita, Automerge,
> Yjs, basedrop), Adobe's UXP developer reference, engineering blogs
> (Figma, Ink & Switch, Tonsky), ITU-R recommendations, and forum/Reddit
> threads for user-side evidence.
> **Marking convention:** `[V]` = verified this session against the cited
> URL or source file. `[J]` = engineering judgment, not sourced. `[U]` =
> could not verify; do not repeat as fact.

> **Origin.** The feature this document exists to serve — *pick a point in
> edit history, extract it to a track, and A/B the two versions* — is the
> project owner's idea, not a copy of anything in the market. §2 exists to
> establish exactly how close the market got and where it stopped.

---

## Why this document exists

Every DAW ships undo. Almost every DAW ships some way to keep an
alternative version of a track. **No DAW lets you compare them.** That is
the finding this dossier is organised around, and it survived a
nine-product sweep of primary documentation.

The gap is not one feature. It is four capabilities that no product has
assembled at once:

1. History that is **browsable** — durable, labelled, navigable, and
   auditionable — rather than a stack you can only pop.
2. **Extraction** of a scoped piece of a past state into the present,
   beside the current material rather than replacing it.
3. **Comparison** that is instant, level-matched, position-preserving and
   click-free, so that what the user hears is the difference and nothing
   else.
4. A **diff** that says what changed, in musical terms, so the user knows
   what to listen for.

Photoshop has (2) for pixels and nothing else. REAPER has most of (1) and
none of (2) or (3). Logic, Cubase, Ardour and Pro Tools have variants
without history. Bitwig has the only glitch-free switch in the market and
applies it only to device chains. Nobody has (4) at all.

The architectural cost of all four is low **if taken from the start** and
close to unpayable later, for the same reason PDC must precede sends:
every one of them is a property of the identity and addressing model, and
identity is the least reversible decision in the codebase.

---

## 1. History as a first-class, browsable object — prior art

### 1.1 Photoshop: the exact architectural statement of the idea

Adobe's UXP developer reference is more revealing than the user-facing
help, because it names the objects.

From the `Document` class
([developer.adobe.com](https://developer.adobe.com/photoshop/uxp/2022/ps_reference/classes/document/)) `[V]`:

| Property | Type | Access |
|---|---|---|
| `historyStates` | `HistoryStates` | R |
| `activeHistoryState` | `HistoryState` | **R/W** |
| `activeHistoryBrushSource` | `HistoryState` | **R/W** |

From the `HistoryState` class
([developer.adobe.com](https://developer.adobe.com/photoshop/uxp/2022/ps_reference/classes/historystate/)) `[V]`, verbatim:

| Property | Description |
|---|---|
| `docId` | "The ID of the document of this history state." |
| `id` | "For use with batchPlay operations. This history ID, along with its document ID can be used to represent this history state **for the lifetime of this document**." |
| `name` | "The name of this history state as it appears on history panel." |
| `snapshot` | "Whether this history state is a snapshot or an automatically generated history state." |

Four consequences fall out, and they are the whole lesson:

- **A history state is a first-class addressable object with a stable
  ID** — not an index into a stack. That is the minimum requirement for
  history to be browsable rather than merely traversable.
- **There are exactly two cursors into history, and both are writable:**
  where the document *is* (`activeHistoryState`), and where the brush
  *reads from* (`activeHistoryBrushSource`). **The decoupling of those two
  cursors is the entire History Brush feature.** Everything else — the
  brush, the mask, the blend mode, the opacity — is ordinary compositing.
  The architectural move is: *the present has a cursor into the past that
  is independent of the document's own cursor.*
- **`snapshot` is a boolean flag on an ordinary state**, not a separate
  type. Snapshots are pinned states. That is the right factoring.
- **"for the lifetime of this document"** — Photoshop's history is not
  persisted in the PSD. `[V]` for the quote; `[J]` for the inference that
  history therefore dies on close, though it matches long-standing product
  behaviour.

The older ExtendScript reference frames a history state as "a version of
the document stored automatically… which preserves the document state each
time the document is changed"
([mirror](https://theiviaxx.github.io/photoshop-docs/Photoshop/HistoryState.html)) `[V]`.

`[U]` — Adobe's help pages (helpx.adobe.com) were unreachable across
repeated attempts, and archive.org was blocked. The widely-repeated
integers (default 50 history states, configurable to 1000, spilled to the
scratch disk, discarded on close) were **not** read from a primary source.
The structural claims are safe; the numbers are not.

#### Why it refuses across a canvas resize, and what that implies

The History Brush is a per-pixel read from the source state at the *same
coordinate* as the pixel being painted:

```
dst[x,y] = blend(dst[x,y], src_state[x,y], brush_alpha[x,y])
```

It is a compositing operation between two rasters assumed to be
**address-compatible**: pixel `(x,y)` denotes the same place in both. Crop,
Canvas Size, Image Size and rotation all change what `(x,y)` means, so the
correspondence is gone and the operation is undefined. Photoshop disables
the source. `[V]` for the mechanism (it follows necessarily from a raster
model and from the source being a plain `HistoryState` carrying no
transform); `[U]` for the exact user-facing symptom and error text.

Note what Photoshop **did not** do: it did not resample, letterbox or
align the old state into the new canvas. It could have. It refuses.
`[J]` That refusal is correct product design — an approximate cross-time
extraction is worse than none, because the user cannot see that it is
approximate.

**The generalisation, and it is the single most transferable idea in this
document:**

> Cross-time extraction requires a **stable addressing scheme shared
> between the two states**. Where the scheme is stable, you can extract
> arbitrary partial state from the past into the present, with soft edges
> and blending. Where it is not, extraction is undefined and must be
> refused or explicitly re-mapped.

Photoshop's address space is `(layer, x, y)` — cheap, dense, and stable
under almost every operation except the handful that resize the canvas.

**For a DAW the address space is `(object identity, time)`, and both
halves are more fragile than Photoshop's** `[J]`:

- *Object identity*: if track/clip/note/lane/plugin IDs are stable and
  never reused, "restore the mixer state of track 7 from 40 minutes ago"
  is well-defined. If IDs are array indices, it is meaningless the moment
  a track is deleted.
- *Time*: **editing the tempo map is the DAW's "Canvas Size" operation.**
  It silently invalidates every seconds-addressed correspondence with the
  past. So does the time-signature map, and so does any global time warp.

Recommendation `[J]`: address in musical time, version the tempo map as a
first-class object, and when the source state's map differs from the
present, take Photoshop's choice — **refuse by default, offer re-map as a
labelled, visible operation.** A silently time-warped historical
extraction is an unfindable bug in the user's music.

#### What Photoshop got right and wrong

Right: two independent cursors; snapshots as pinned ordinary states with
user names; extraction that is **spatially masked and blendable** rather
than all-or-nothing (users don't want "revert this object", they want
"revert this object, 60%, here"); and refusing rather than approximating.

Wrong, and instructive `[J]`:

- **History is ephemeral.** It dies when the document closes. This single
  decision keeps Photoshop's history a safety net rather than a browsable
  object. Nobody plans around a panel that will be empty tomorrow.
- **Non-linear history is off by default and effectively undiscoverable.**
  Photoshop shipped tree undo in the 1990s and hid it in a checkbox.
- **Fixed capacity with silent FIFO eviction.** The state you want is
  exactly the one that just fell off.
- **Extraction works on pixels only.** You cannot History-Brush a layer's
  blend mode or an adjustment's parameters from the past. A DAW has an
  advantage here: automation curves, mixer state and note data *all* have
  natural address spaces, so the analogue can be much broader.

### 1.2 Blender: the cautionary scaling case

Read from source, because blender.org returned 403 throughout.

`UndoStack` / `UndoStep` / `UndoType` in
[`BKE_undo_system.hh`](https://raw.githubusercontent.com/blender/blender/main/source/blender/blenkernel/BKE_undo_system.hh) `[V]`.
`UndoType` is a vtable — `step_encode_init`, `step_encode`, `step_decode`,
`step_free`, `step_foreach_ID_ref` — so Blender has a **pluggable,
polymorphic undo system** with multiple co-existing implementations on one
stack. Limits are enforced by count *and* memory
(`BKE_undosys_stack_limit_steps_and_memory_defaults()` reads `U.undosteps`
and `U.undomemory`; defaults 32 steps, range 0–256, memory 0 = unlimited).

`UndoRefID` exists because "pointers are not stable and may have changed
when restoring the undo-step" `[V]` — restoring reallocates the world, so
every cross-reference must be re-resolved.

**What a "memfile" actually is:** `MemFile` is a linked list of
`MemFileChunk`, and an undo step is **a serialised .blend file held in
RAM**
([`undofile.cc`](https://raw.githubusercontent.com/blender/blender/main/source/blender/blenloader/intern/undofile.cc)) `[V]`.
Dedup is positional `memcmp` with buffer sharing:

```c
if (compchunk->size == curchunk->size && memcmp(compchunk->buf, buf, size) == 0) {
  curchunk->buf = compchunk->buf;       /* SHARE the pointer */
  curchunk->is_identical = true;
  compchunk->is_identical_future = true;
}
```

Two identity flags, because "the stack is relative and traversed in both
directions" `[V]`.

**The measurement that matters.** Bastien Montagne, 2019, on a Spring
production scene `[V]`:

> "avoiding reading of unchanged IDs saves about 30% of the read process
> time… around 100ms (130 ms with current master code, 90ms with code in
> the branch), **when the actual undo step takes about 4 seconds from a
> user PoV**. So main optimization is clearly to be sought into the scene
> update/rebuild happening after undo 'memfile' has been read."

~2.5% of undo time is the data; ~97% is rebuilding everything downstream
of invalidated pointers. **Optimise identity stability, not diff format.**

User reports from
[#60695](https://projects.blender.org/blender/blender/issues/60695) `[V]`:
45 s undo on a "more or less big scene" (2.79); "minutes" at 10–50 M polys;
"at least a couple of minutes" on imported CAD; and the adoption cost —
*"It's not just that we need to wait 5-10 seconds for the Undo, it's also
all the times you avoid Undoing because you know it's going to lock your
computer for a while"*, *"I've switched to maya just because of this."*
T60695 has been open since 2019-01-21 and step 4 ("write only changed
datablocks") is still not done.

`[J]` **Blender's undo push is O(scene size), not O(edit size)** — the
encode side must still walk and byte-compare the whole serialised database
to discover which chunks are identical. That is the whole bug. If the cost
of recording a history step is proportional to project size, history dies
as the project grows — exactly when the user needs it most.

**Why Blender did not use a command pattern**, from Brecht Van Lommel `[V]`:

> "Nearly all operators will do an undo push after making changes, but not
> all. Dependency graph evaluation may flush back some data to the original,
> and this happens after undo push. Python app handlers may modify the scene
> in arbitrary ways."

and the decisive asymmetry:

> "If it is not done correctly, then instead of a missing refresh there is a
> **more serious bug of not undoing all changes**."

`[J]` Blender chose whole-database snapshots because it has an **open,
unbounded mutation surface** (C operators + Python addons + depsgraph
writeback + modal tools) over a globally shared mutable database with raw
cross-datablock pointers. A command log requires a *closed* mutation set.
Blender doesn't have one and, given its extensibility model, can never
have one. **AURA can. That is the decisive difference, and it is why AURA
should not copy Blender.**

Worth stealing anyway: the `UndoType` vtable, and especially
**`step_foreach_ID_ref`** — a callback enumerating every external identity
a step depends on. That is what makes cross-domain GC and integrity
checking possible.

And Brecht's own retrospective, which points the opposite way from what
you'd expect `[V]`:

> "I also think that ideally **everything should be stored in the memfile
> undo stack, rather than having a single stack but still separate storage
> that continues to cause problems**."

The *two-tier split* is the source of the pain, not snapshots per se.

Eviction details worth copying from
`BKE_undosys_stack_limit_steps_and_memory()` `[V]`: it walks newest →
oldest accumulating `data_size` and cuts when the budget blows, which
means **the newest step is always kept even if it alone exceeds the
budget** (the limit applies *after* the push, so a push never fails for
budget reasons), with a hard floor of two steps. And a scar: `undosteps
== 1` is silently coerced to 2, source comment *"Do not allow 1 undo
steps, useless and breaks undo/redo process (see #42531)."*

Blender's architecture doc also names the axis this dossier turns on:
**Relative vs Absolute steps.** *"Currently, Blender undo stack is fully
relative"* — reaching a step means replaying everything between. The
design in §4/§6 is **Absolute**: every retained node is directly loadable.
That is precisely why it materialises in nanoseconds and Blender's does
not.

### 1.3 Figma: history as a durable, named, shared object

Version creation, from
[help.figma.com](https://help.figma.com/hc/en-us/articles/360038006754) `[V]`:

> "Figma saves your work by adding checkpoints to the file's version
> history. Figma records a new checkpoint every 30 minutes and keeps the
> current version up to date with your file changes."

Named versions via ⌘⌥S, title **capped at 25 characters** plus a
description. **You can name an existing autosaved version retroactively** —
the most important UX detail in this section. Retention is plan-gated:
30 days on free tiers, entire history on paid. Each entry shows name,
description, time/date, and the main contributor's name and avatar;
**autosaved checkpoints are grouped and expandable.**

Preview is a live navigable canvas, not a thumbnail: pan with the Hand
tool, select/copy/export with the Move tool.

**Restore creates two checkpoints** `[V]`:

> "In the right sidebar, click next to the version and select **Restore
> this version**. Figma will add two autosave checkpoints to the file's
> version history" — one capturing the current state **before** restoration,
> one at the restored timestamp.

`[J]` Restore is forward-moving. The future is never deleted, it is pushed
down into history as a restorable checkpoint. **History only grows.** This
is the single most transferable mechanic in the dossier, and Figma is the
citable source for it.

Only created/named and library-published versions can be deleted;
**autosave checkpoints cannot be removed.**

**Branching is a full copy**, from
[help.figma.com](https://help.figma.com/hc/en-us/articles/360063144053) `[V]`:
Organization/Enterprise plans only, Full seat required; *"Figma will create
a new branch that is an **exact replica of the main file in its current
state**"*; and the stated limitation *"It's not possible to pick and choose
which changes you want to apply."*

`[J]` History is cheap for Figma because the document is already an op
stream. Branching is expensive because it is a copy — and the UX has the
tell of an expensive feature: gated to the top two plans, all-or-nothing
merge. A cheap structural-sharing branch would have no reason to be
Enterprise-only. Note also that even Figma, with a structured op-based
model, did not ship cherry-pick. Assume you can't either, and design so
users never need it.

**The undo principle**, from
[figma.com/blog](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/),
Evan Wallace, 2019-10-16 `[V]`, verbatim:

> "Undo history has a natural definition for single-player mode, but undo
> in a multiplayer environment is inherently confusing. If other people
> have edited the same objects that you edited and then undo, what should
> happen? Should your earlier edits be applied over their later edits?
> What about redo?
>
> We had a lot of trouble until we settled on a principle to help guide us:
> **if you undo a lot, copy something, and redo back to the present (a
> common operation), the document should not change.** This may seem
> obvious but the single-player implementation of redo means 'put back what
> I did' which may end up overwriting what other people did next if you're
> not careful. This is why in Figma an undo operation modifies redo history
> at the time of the undo, and likewise a redo operation modifies undo
> history at the time of the redo."

`[J]` This applies even to a single-user DAW. **Undo is not "restore a past
state". Undo is "apply an inverse operation, rebased onto the present."**
Figma is forced into it by concurrency; a DAW has the same problem from
background mutation — a recording landing on another track, a render
completing, an automation-follow write, an MCP agent editing. Snapshot-restore
undo silently destroys all of it. Design undo as inverse-plus-rebase from
day one; retrofitting it is a rewrite.

Also `[V]`: the document is `Map<ObjectID, Map<Property, Value>>` with
**last-writer-wins per property**; not a true CRDT because the server is
authoritative; **children store a link to the parent** so identity survives
reparenting, with parent and fractional position "stored as a single
property so they update atomically"; child order by **fractional
indexing**; and **deleted objects' properties are stored in the undo buffer
of the client that performed the delete**, not on the server, "keeping
documents from growing indefinitely."

`[J]` That last point is Figma independently arriving at *the undo entry
owns the deleted object* — the same conclusion Zrythm 2 reached from a
refcounting direction (§4.3).

### 1.4 Non-linear / tree undo, and why it stayed niche

**Vim** ([vimhelp.org/undo.txt](https://vimhelp.org/undo.txt.html)) `[V]`
is the most complete tree-undo in wide deployment. Branches form
naturally: "This happens when you undo a few changes and then make a new
change. The undone changes become a branch." The admission that matters:

> "Note that using 'u' and CTRL-R will not get you to all possible text
> states while repeating 'g-' and 'g+' does."

It ships **four navigation idioms over one structure**: branch-local (`u`),
chronological (`g-`/`g+`), by change number (`:undo N`), and by
wall-clock/save-count (`:earlier 10m`, `:earlier 1f`). That multiplicity is
the tell — one structure, and no single metaphor sufficed. Persistence via
`'undofile'`, with the warning "**undo files are never deleted by Vim. You
need to delete them yourself.**" Default `undolevels` is **100, or 1000 on
Unix/VMS/Win32**; Neovim defaults to 1000 everywhere.

**undo-tree** for Emacs
([elpa.gnu.org](https://elpa.gnu.org/packages/undo-tree.html)) `[V]`
documents native Emacs undo's failure mode — moving point at the wrong
moment "breaks the undo chain" — and provides a visualiser with
timestamps and diffs. `undo-tree-auto-save-history` is **disabled by
default**.

Its successor **vundo** ([github.com/casouri/vundo](https://github.com/casouri/vundo)) `[V]`
states the positioning that is the whole story in four words: *"Vundo
doesn't need to be turned on all the time nor replace the undo commands
like undo-tree does."* `[J]` undo-tree's failure was not the model; it was
being a resident system owning your undo keys and your storage format.
vundo is a **viewer over the existing history**, which is the correct
factoring.

**IntelliJ Local History**
([jetbrains.com/help/idea](https://www.jetbrains.com/help/idea/local-history.html)) `[V]`
is the most successful browsable-history feature in this survey and it is
**not a tree**: automatic recording; **semantic auto-labels** ("some
revisions are automatically marked with labels based on predefined events:
running tests, deploying apps, committing changes"); manual labels;
retention of "the last 5 working days"; and granularity down to
**specific code fragments** with **"Revert Selected Changes"**.

`[J]` IntelliJ ships the History Brush for code: pick a past revision, pick
a fragment, pull *that piece* forward. Linear timeline, semantic labels,
partial extraction. **That combination — not tree navigation — is what
actually gets used.**

**Why tree undo stayed niche** `[J]`, built on the facts above:

1. **The tree is invisible until it's too late.** A branch is created by an
   action the user does not experience as branching. By the time you want
   the lost branch, you don't know it exists.
2. **Nodes have no names, so the tree has no meaning.** A tree of "state
   47, state 48" is not navigable by a human. The label is the feature;
   the graph is plumbing.
3. **The user's question is temporal, not topological.** People ask "what
   did it sound like before lunch?" — never "the third node on the left
   branch." Vim built `:earlier 10m` and `:earlier 1f` precisely because
   change-number navigation didn't answer the real question. Note `1f` is
   *event*-based: the best coordinate isn't even clock time.
4. **Always-on is a tax most users never recoup.** Cost daily, benefit
   twice a year.
5. **Branch identity is unstable.** Trees need rebasing to stay
   comprehensible, and nobody has shipped that.

**The academic backing** `[V]`, with a correction worth carrying:

- Berlage 1994, *"A selective undo mechanism for graphical user interfaces
  based on command objects"*, ACM TOCHI 1(3):269–294, **DOI
  10.1145/196699.196721**. ⚠ The DOI **10.1145/174630.174632** circulates
  for this paper in secondary sources and is wrong — it resolves to Sears &
  Shneiderman's "Split menus."
- Prakash & Knister 1994, *"A framework for undoing actions in
  collaborative systems"*, ACM TOCHI, DOI 10.1145/198425.198427.
- Sun 2002, *"Undo as concurrent inverse in group editors"*, ACM TOCHI
  9(4):309–361, DOI 10.1145/586081.586085 — introduces the inverse
  properties IP1/IP2/IP3 required for correct undo under OT.
- Ressel & Gunzenhäuser, *"Reducing the problems of group undo"*, GROUP
  '99 — the title alone is citable.
- Weiss/Urso/Molli, *"Logoot-Undo"*, IEEE TPDS 21(8), 2010 — that CRDT
  undo needed its own paper on top of Logoot is itself the argument.

`[J]` **The decision for AURA: ship the tree, do not ship selective undo.**
Selective undo requires a commutation analysis, and in a DAW — where
commands touch overlapping time ranges on shared tracks — most interesting
pairs do not commute. The tree is nearly free (§6 measurements: branching
costs ~6 KB/node). Selective undo is a trap. "Undo the agent's changes"
is implemented as a **new forward batch carrying the inverse**, not as a
hole in history (§4.7).

### 1.5 Version-control-shaped creative tools: what creatives adopt vs reject

**Perforce** ([perforce.com](https://www.perforce.com/solutions/game-development)) `[V]`
dominates game art for one reason: *"exclusive file locking with global
lock visibility"*, *"File locking that shows your art teams what characters,
materials and textures are currently being worked on."* `[J]` Merge is
impossible for binary assets, so the system's job is to *prevent*
concurrent edits, not reconcile them. Locking is a coordination protocol,
not a version-control feature.

Perforce's `+S<n>` filetype modifier `[V]` is exactly the per-asset
bounded-history primitive: *"Only the most recent n revisions are stored.
Older revisions are purged from the depot upon submission of more than n
new revisions, **or if you change an existing +Sn file's n to a number less
than its current value**."* And `p4 obliterate` explicitly **preserves**
archive files still referenced by lazy copies — a refcount by another name.

**Git LFS** ([docs.github.com](https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-git-large-file-storage)) `[V]`:
a pointer file "which acts as a reference to the actual file (which is
stored somewhere else)" carrying version, `oid` and size. `[J]` The
pointer-file model is exactly right; the surrounding workflow is exactly
wrong. Content-addressed blobs in a side store with the document holding
only hashes is what you want for audio. What artists reject is everything
else: staging, branching, merge conflicts on unmergeable files, and the
requirement to describe your work in an imperative-mood sentence before it
is saved.

Why git chokes on binaries, from
[bup's DESIGN](https://github.com/bup/bup/blob/master/DESIGN) `[V]`:
*"The primary reason git can't handle huge files is that it runs them
through xdelta, which generally means it tries to load the entire contents
of a file into memory at once… xdelta works great for small files and gets
amazingly slow and memory-hungry for large files."* (`core.bigFileThreshold`
defaults to 512 MiB, above which files are stored deflated without delta
compression.)

**Anchorpoint** ([anchorpoint.app/blog](https://www.anchorpoint.app/blog)) `[V]`
is Git-based and artist-targeted, and leads with *"version history, **file
locking**, and review workflows"* while — on the pages fetched —
**not discussing branching for artists at all**. `[J]` That absence is
itself the finding.

**Diversion** ([diversion.dev](https://www.diversion.dev/)) `[V]` pitches
entirely on scale and speed: "100s of TB", "50M+ files", "1,000+
commits/min", "5M files cloned in under 5 min", files up to 2.4 TB. `[J]`
Not better merge, not smarter history — *fast and invisible*. That is the
market speaking.

**Plastic SCM / Unity Version Control — Gluon** `[U]`. Docs unreachable.
The recollection — a separate GUI offering a **partial workspace** with
**no branching or merging exposed**, plus locking — could not be verified.
If it holds, it is the strongest data point in this section `[J]`: the
vendor with the best merge technology in the space concluded that the
right thing to offer artists is *a client with merge removed*.

**Splice Studio** `[U]` — automatic DAW project versioning with a revision
timeline, shipped, got real users, and was **discontinued**. Worth a
dedicated post-mortem before committing to a design; both support URLs
404'd.

**"Commit" when there is no natural stopping point.** The field has
converged without saying so `[J]`:

| System | Commit trigger | Human's role |
|---|---|---|
| Figma | every 30 min, automatic `[V]` | name a version, retroactively `[V]` |
| IntelliJ Local History | continuous + on domain events `[V]` | add a label `[V]` |
| Vim `undofile` | on file write `[V]` | nothing |
| Photoshop | every operation `[V-partial]` | take a snapshot |
| Google Docs | continuous | name a version (≤40/doc) `[V]` |

**Nobody successful asks the creative to decide when to commit.** The
system commits on a timer or an event; the human's only job is to *name*,
and naming is offered retroactively so it never interrupts.

The failure mode is `character_final_final_v2.fbx` `[V, Anchorpoint]` —
worth reading generously: it is a **user-authored, semantically-named,
immutable version chain** built by people with no tooling. Artists are not
version-averse. They are *ceremony*-averse.

`[J]` A DAW's natural commit boundaries are unusually rich and free:
transport stop after a recording, loop-record take boundary,
render/bounce/export, project open and close, plugin instantiated/removed,
and the long pause.

### 1.6 Automerge / Yjs: history cost is an encoding problem

Automerge's Rust API
([docs.rs/automerge](https://docs.rs/automerge/latest/automerge/struct.Automerge.html)) `[V]`
is the most complete time-travel surface found anywhere:

- Positions: `get_heads() -> Vec<ChangeHash>`, `get_changes(have_deps)`,
  `get_changes_meta` (metadata without full changes).
- **Every read has an `_at(heads)` variant**: `get_at`, `keys_at`,
  `values_at`, `length_at`, `list_range_at`, `text_at`, `parents_at`,
  `marks_at`, plus `get_cursor_position(obj, cursor, at)`.
- Branching: `fork_at(heads)`, `transaction_at(patch_log, heads)`.
- Diffing: `diff(before_heads, after_heads) -> Vec<Patch>`,
  `diff_obj(obj, before, after, recursive)`.
- Persistence: `save()`, `save_after(heads)`.

`[J]` Four properties make this the right shape:

1. **A position in history is a value** (`Vec<ChangeHash>`), not an index —
   content-addressed, stable, storable in a bookmark, a comment, an undo
   entry.
2. **Every read is parameterisable by time.** There is no separate
   "historical document" object. `text_at(obj, heads)` is the History Brush
   primitive generalised to a structured document.
3. `diff(before, after)` gives the change-log UI for free, between *any*
   two points, not just adjacent ones.
4. `fork_at` makes branching O(1)-ish rather than a full copy — exactly
   what Figma cannot do.

**The cost numbers**, from
[automerge.org/blog/automerge-2](https://automerge.org/blog/automerge-2/) `[V]`,
on a ~260 k-operation editing trace:

| Metric | Value |
|---|---|
| Plain text on disk | 107,121 bytes |
| Automerge 2.0 **with full history** | 129,062 bytes — "only 30% overhead" |
| Automerge 0.14 | 146,406,415 bytes |
| Load time | 593 ms → 438 ms |
| Apply-trace | 1,816 ms → 661 ms (Yjs: 1,074 ms) |
| Peak memory | 44.5 MB → 23.0 MB |

`[J]` **~1.2× plaintext for complete keystroke-level history.** But note
the 0.14 → 2.0 delta: 146 MB → 129 KB, a factor of 1,134. The data model
didn't change; the *encoding* did. **History cost is dominated by encoding
quality, not by the decision to retain history.** That reframes the whole
storage question.

**Yjs is the counterexample from the other side** `[V]`: *"Set `doc.gc =
false` in order to disable gc **and be able to restore old content**"*, and
`createDocFromSnapshot` throws on a GC-enabled doc because "some of the
restored items might have their content deleted." `[J]` **Yjs's default
configuration deliberately destroys history to stay fast.** A CRDT is not
automatically a history store; merge and history are separate concerns.

Steal from `Y.UndoManager` `[V]`: `scope` (which subtree this stack owns),
**`trackedOrigins`** (only changes tagged with these origins are undoable),
`deleteFilter`, and `captureTimeout` (default **500 ms**) with
`stopCapturing()`. `[J]` `trackedOrigins` is exactly how "my edits are
undoable, but the arriving recording / the automation-follow write / the
agent's edit is not" becomes a one-line policy instead of a pile of special
cases.

**Automerge as the instructive negative example** `[V]`: its binary format
spec states *"Automerge stores the full history of changes to the document:
this is a large amount of data but in practice it is very repetitive and
amenable to compression"* — and contains **no provision for pruning at
all**. `[J]` "Never delete, just compress" works for text ops and fails
immediately for 200 MB of audio. This is the cleanest argument for keeping
the op-log DAG and the blob store as **two separately-GC'd layers**.

The confound, from
[inkandswitch.com/essay/local-first](https://www.inkandswitch.com/essay/local-first/) `[V]`:

> "CRDTs accumulate a large change history, which creates performance
> problems."

and the reason you can't truncate: it's "impossible to know when someone
might reconnect to your shared document after six months away." `[J]` That
is a **collaboration** constraint, not a history constraint. A single-user
DAW is free to compact aggressively because there is no absent peer.

**Verdict** `[J]`: a CRDT is not the right substrate for cheap historical
materialisation, and materialisation is a separate concern. What you need
is an **immutable, content-addressed, append-only change log** with
**columnar/delta encoding**, **periodic materialised snapshots as an
index**, and **`read_at(position)` on every accessor**. Automerge provides
all four because it is an op-log with good encoding — *not* because it is a
CRDT. If you don't need concurrent multi-writer merge on day one, build the
log and skip the CRDT. You can add CRDT semantics to an op-log later. You
cannot add history to a snapshot-based system later — ask Blender.

One further caution `[J]`: CRDT history is *machine* history. 260,000
keystroke operations is a perfect audit trail and a useless browsing
experience. Human-meaningful history requires a **second, coarser layer** —
named checkpoints, semantic events, coalesced gestures. Budget for it.

### 1.7 The notebook parallel: reverting the document doesn't revert the world

**nbdime** ([nbdime.readthedocs.io](https://nbdime.readthedocs.io/en/latest/)) `[V]`:
"primitive line-based diff and merge tools do not handle well the logical
structure of notebook documents"; it provides content-aware diffing,
"eliding base64-encoded images", "rendering image diffs in a web view", and
**"auto-resolving conflicts on generated values such as execution
counters."**

`[J]` Two transferable ideas:

- **Partition every field into `authored` vs `derived` at the schema
  level.** A notebook's execution counter is derived state and must never
  produce a diff. A DAW is full of these: render caches, waveform peaks,
  plugin-reported latency, last playhead position, window layout, meter
  states. Derived fields are excluded from history entirely — recomputed,
  not versioned. Get this wrong and every history entry is noise.
- **Diff must be rendered in the medium.** nbdime renders image diffs as
  images. A DAW's diff must be *audible* and visible as waveform/piano
  roll, never as a property list. "Track 3 gain: −6.0 → −4.5" is a
  checksum, not a diff.

**marimo's FAQ** ([docs.marimo.io/faq](https://docs.marimo.io/faq/)) `[V]`:

> "In Jupyter notebooks, the code you see doesn't necessarily match the
> outputs on the page or the program state. If you delete a cell, its
> variables stay in memory, which other cells may still reference; users
> can execute cells in arbitrary order."

and (marimo's citation of a study, underlying paper `[U]`): "One study
analyzed 10 million Jupyter notebooks and found that **36% of them weren't
reproducible**."

**This is the DAW's central history problem in another domain** `[J]`:

| Jupyter | DAW |
|---|---|
| kernel variables | plugin internal state (filter memory, sampler RAM, learned MIDI) |
| stale cell outputs | rendered/frozen/bounced audio, waveform caches |
| execution order ≠ cell order | signal-flow order ≠ arrangement order |
| files written by cells | recorded audio, exported stems, destructive edits |

marimo's answer — make the dependency graph explicit so the system knows
what is stale — is right for a DAW, and **a DAW is in a better position
than a notebook because the graph is not inferred from static analysis; it
is the signal graph, and we already have it** `[J]`.

Concretely: every rendered artifact (freeze, bounce, offline process,
waveform cache) is keyed by a **content hash of its inputs** — source audio
hash + chain state hash + parameter hash + time range. Then navigating to a
past state makes every cache entry either valid (hash matches, reuse
instantly) or stale (recompute), with no bookkeeping. **This is what decides
whether history browsing is a feature or a progress bar.**

**Verdant** ([github.com/mkery/Verdant](https://github.com/mkery/Verdant)) `[V]`
records history at cell/output granularity into an `.ipyhistory` file,
"designed to complement" Git rather than replace it. Its CHI 2019 paper is
framed as *"Towards Effective Foraging by Data Scientists to Find Past
Analysis Choices."*

`[J]` **Foraging** is the right frame. The user has an outcome in mind
("the mix where the vocal sat right") and is searching a large unstructured
history for the state that produced it. Foraging is a *search* problem —
you need scent, cues, and cheap sampling — not a *navigation* problem.
Implications: **cheap preview is everything** (hover a history entry and
hear it, instantly, without committing); provide scent (waveform
thumbnails, rendered change summaries, semantic labels); and
**artifact-level history** ("show me the history of *this* track") is the
single most-requested capability that almost nobody ships.

---

## 2. DAW variant features, product by product

### 2.1 Ardour playlists

Ardour's manual is unusually explicit about the data model
([manual.ardour.org](https://manual.ardour.org/working-with-playlists/understanding-playlists/)) `[V]`:
*"a track **has** a playlist"* — a track is not a playlist. A playlist is
"a list of regions ordered in time"; the track turns it into audio and
pushes it through the signal chain. **Playlists are cheap** — the manual
says so explicitly, contrasting with audio files (disk), tracks (CPU +
memory) and plugins (CPU). Ardour is telling you the design intent: make
new playlists freely.

Operations, from the `p` button
([playlist-operations](https://manual.ardour.org/working-with-playlists/playlist-operations/)) `[V]`:

| Command | Behaviour |
|---|---|
| New Playlist | Creates an **empty** playlist, switches to it |
| Copy Playlist | **Independent copy** of the current playlist |
| Select | Switch between existing playlists (with creation timestamps); can operate on one track, **all rec-armed tracks**, or **all tracks** |
| Rename | Renames the active playlist |
| **Share with** | Uses another track's playlist — *the same object*. "Edits to the playlist made in one track will magically appear in the other" |
| **Steal from** | Adopts a playlist from another track and removes it from that track's local list |

**Shared by reference is a first-class, documented workflow** — the
manual's first listed use case is parallel processing: duplicate a track,
share the playlist, apply two different non-linear processes to the same
audio without bus latency. `[J]` This is the only product in the survey
where "two tracks, one content, by reference" is intentional.

**The copy constructor mints new region IDs while sharing sources** `[V,
source]`: `Playlist`'s copy path calls `other->copy_regions(tmp)`, and
`copy_regions` does `newlist.push_back(RegionFactory::create(r, true,
true))`. Copied regions are **new region objects with new IDs**; the
underlying `Source` (the audio file) is shared
([`libs/ardour/playlist.cc`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/playlist.cc)).

**What is shared vs duplicated:** shared and untouched by a playlist
switch — the track, its processor/plugin chain, fader, routing, name, and
the source files. Duplicated by Copy Playlist — only the region list.

**Automation does NOT follow the playlist** `[V, forum]`. An Ardour forum
thread ([discourse.ardour.org/t/105868](https://discourse.ardour.org/t/105868))
describes the failure exactly: a user makes a playlist copy as a backup,
consolidates a region, then finds that deleting the now-redundant fader
automation removes it from *all* playlist copies. The requested fix is
literally "allow users to unlink automation so it operates independently
across playlist copies."

`[J]` Ardour got *sharing* right and *automation* wrong, and the automation
gap is a direct consequence of automation living on the Route (mix side)
rather than in the playlist (content side).

Switching during playback: no manual statement, no forum report either
way `[U]`. `[J]` Plausibly a discontinuity at the splice point rather than
a chain rebuild, since the processor chain is untouched.

### 2.2 Pro Tools playlists

Page references are to the **Pro Tools Reference Guide 2024.10**
([resources.avid.com](https://resources.avid.com/SupportFiles/PT/Pro_Tools_Reference_Guide_2024.10.pdf)) `[V]`.

Two kinds (p.11–12): **edit playlists** ("a sequence of clips arranged on
an audio, MIDI, or video track"; on MIDI/Instrument tracks they "can store
multiple MIDI sequences (or performances)") and **automation playlists**
("Each audio, Auxiliary Input, Instrument, Master Fader, and VCA track also
has **a single set** of automation playlists").

And the decisive line, p.1415:

> **"All edit playlists on a single audio track share the same automation
> data."**

with the exception that "MIDI controller data on Instrument and MIDI tracks
is **always** included as part of the track playlist" — audio automation is
track-global, MIDI CC is playlist-local. `[J]` A documented split-brain,
and a warning for anyone designing this.

Operations (p.870–872) `[V]`: New (`Ctrl+\`) and Duplicate (`Cmd+Ctrl+\`);
auto-naming `Kick.01`, `Kick.02`; **playlists are movable but exclusive**
("When a playlist is reassigned to another track… it is **unavailable to
other tracks** including the track on which it originated") — the opposite
of Ardour's share-by-reference; a playlist carries its own **timebase**;
**renaming a playlist renames the track** (`[J]` a genuine identity
confusion — keep variant identity separate from track identity);
`Shift+Down`/`Shift+Up` cycles; the selector is grey with one playlist and
blue with more. Creating a new playlist on a grouped track **auto-increments
the suffix on every track in the edit group** — the multitrack-drum answer,
implemented by *name convention*, not a shared variant ID.

Playlists view / comping (p.876–878) `[V]`: alternates appear as lanes
below the main playlist; the bottom lane is always empty; **"edits that are
applied to range selections are applied to all *shown* alternate
playlists"** (`[J]` visibility as semantics is a data-loss generator);
clip groups unsupported in this view; comping commands Copy Selection To
Target / New / Duplicate Playlist; clips carry a 1–5 star **Rating**
(p.845).

**Auditioning — the A/B mechanism** `[V]`:

> "Only the main playlist plays back through the track audio output path.
> To hear an alternate playlist, audition the Playlist lane. The auditioned
> lane then plays through the track audio output path instead of the main
> playlist."

`Shift+S` auditions the lane containing the Edit cursor. `[J]` This is a
**source swap upstream of an identical processing chain** — not a second
voice, not a crossfade.

**`Matches > Expand Alternates To New Tracks`** (p.889) `[V]` is the closest
existing shipping analogue of "extract this alternative and make it a real
track so I can hear them against each other." `[J]` Worth studying its
ergonomics closely — and note it is all-or-nothing and creates real,
permanent tracks.

### 2.3 Cubase Track Versions

From the Cubase manual
([archive.steinberg.help v12](https://archive.steinberg.help/cubase_pro/v12/en/cubase_nuendo/topics/track_handling/track_handling_trackversions_c.html),
restated for [15.0](https://www.steinberg.help/r/cubase-pro/15.0/en/cubase_nuendo/topics/track_handling/track_handling_trackversions_c.html)) `[V]`:

> "Track versions allow you to create and manage multiple versions of
> events and parts on the same track."

Supported on **audio, MIDI, instrument, chord, signature and tempo
tracks**. Two explicit notes:

> "Track versions are **not available for automation tracks**."
> "Track versions are included in track archives and project backups."

`[J]` The tempo/signature/chord support is notable and, as far as this
survey found, unique: Cubase lets you version the *global* musical
scaffolding, not just track content.

Feature surface (15.0 subtopic list) `[V]`: New / Duplicate / Delete /
Activate; **Activating Track Versions on Multiple Tracks**; **Track Version
IDs**, **Selecting Tracks by Track Version ID**, **Assigning a Common ID**;
Rename; copy/paste selection ranges and selected events *between* versions;
**Track Versions vs. Lanes**, and conversion both ways.

**The Version ID is the interesting primitive** `[J]`: it is the mechanism
by which "version 2 of the kick" and "version 2 of the snare" are known to
be the same variant, so one gesture flips a whole drum kit. Pro Tools
solves the same problem with name suffixes inside an edit group; Logic with
group membership. **Cubase gave it an identity, and Cubase got this right
while everyone else got it wrong.**

**Automation is global to the track** `[V, forum]` —
[forums.steinberg.net/t/665458](https://forums.steinberg.net/t/does-track-version-not-include-the-track-automation/665458):
"automation is not included in a new Track Version; it remains 'global' to
the track", "would be ten times more useful if it included the automation
as well". A decade of open requests:
[130915](https://forums.steinberg.net/t/130915/4) (users debating
*per-parameter* versions vs *per-track-version* automation vs a hybrid),
[96575](https://forums.steinberg.net/t/96575/13),
[803611](https://forums.steinberg.net/t/803611/1),
[804319](https://forums.steinberg.net/t/804319/1),
[881326](https://forums.steinberg.net/t/881326/1) (asks for group tracks,
mixer settings, track automation and a **global recall button**).

**A known bug that is a design warning** `[V]`:
[863494](https://forums.steinberg.net/t/863494/3) — "Export Audio Mixdown
exports wrong Track Version", with the user's diagnosis that items on a
track with Track Versions appear to be **muted** when not on the active
version. `[J]` If variants are implemented as mute state, export paths
desync from the UI. **Do not implement variants as mute state.**

### 2.4 Logic Pro: Track Alternatives and Project Alternatives

**Track Alternatives** — the model most worth copying
([support.apple.com](https://support.apple.com/guide/logicpro/use-track-alternatives-lgcp002c4e63/mac),
[Logic Pro User Guide PDF](https://help.apple.com/pdf/logicpromac/en_US/logic-pro-mac-user-guide.pdf)) `[V]`:

> "Each alternative can contain different regions or arrangements, **while
> sharing the same channel strip and plug-ins**. Track alternatives are like
> 'playlists' for individual tracks that can be used to try out different
> ideas or archive tracks at different stages of development."
> "One track alternative is always active and plays when you play the
> project."

Commands: New (empty — "Any regions on the track when you create a new
alternative are saved to the previous alternative"), Duplicate, Rename,
Rename by Region, Show Inactive, Delete Inactive. Auto-named **A, B, C…**

**The audition/commit separation — the key UX detail** `[V]`:

- **Hear an inactive alternative:** "click the On/Off button in the track
  header for the inactive alternative. The alternative will be audible when
  you play the project, in place of the active alternative."
- **Make it active:** click the upward-pointing arrow — "The alternative
  will be exchanged with the active alternative (which becomes inactive)."

`[J]` Auditioning is a mute/unmute-grade operation — exactly the kind that
can be made glitch-free — and it is cleanly separated from promotion. **This
separation is the model to copy.**

Inactive alternatives, when shown, "can be edited like normal tracks", with
key commands **Move/Copy Selected Regions to Selected Track** for comping
material into the active alternative. Grouping requires the **Track
Alternatives checkbox in Group Settings**. Logic can auto-create
alternatives for overlapping recordings.

Automation: never mentioned in the Track Alternatives documentation.
`[J, strong, corroborated]` — Sound on Sound
([soundonsound.com](https://www.soundonsound.com/techniques/logic-pro-track-alternatives)) `[V]`
states it directly: "all Track Alternatives share the same plug-in chain
and automation framework — only the audio/MIDI content differs." SOS also
notes the feature is "easy to miss" because it is hidden by default.

**Project Alternatives** (User Guide p.105–106) `[V]`:

> "Project alternatives let you save **snapshots of a project in different
> states**, including different cuts or mixes. They're **saved as part of the
> project and share the same assets**."

Switching prompts a Save dialog if there are unsaved changes — `[J]` a
document load, not a live swap, and therefore unusable as a fast comparison
loop. Users confirm:
[r/LogicPro](https://old.reddit.com/r/LogicPro/comments/1ul0dph/best_workflow_for_trying_alternate_song/) `[V]`
— *"switching between alternatives is slow enough that it's hard to really
A/B versions."*

Backups: each ⌘S saves a version, "up to **ten backups per alternative**",
listed newest-first under File > Revert to. **Clean Up understands the
sharing**: "Delete Unused Audio Files: Audio files not used **in any project
alternative**". `[J]` That is the correct GC root set, stated by a shipping
product.

`[J]` **Logic is the only product with a coherent three-tier story** —
region-level (Take Folders) → track-level (Track Alternatives) →
project-level (Project Alternatives) + time-travel (Backups). Its weakness
is switching cost at the top tier.

### 2.5 Studio One Scratch Pads — the right idea, the wrong boundary

*(Note: as of this session `s1manual.presonus.com` 301-redirects to
`fenderstudiopromanual.fender.com`; the description below is from Sound on
Sound plus user reports.)*

From [SOS](https://www.soundonsound.com/techniques/studio-one-using-scratch-pad) `[V]`:

> "A Scratch Pad is an alternative arrangement page within Studio One's
> Arrange view. It splits off to the right of the main Arrange View and is
> in the same format, following the same tracks and using the same editing
> tools."

The mixer console, all tracks, plugins, mixer settings, tempo and markers
are **shared**. You can have as many pads as you like but view one at a
time, and "switch between them via menu selection while playback
continues."

**What can't cross the boundary** `[V, user-sourced]`:

- **You cannot swap a whole pad into the main arrangement.**
  [r/StudioOne](https://old.reddit.com/r/StudioOne/comments/1tyyn9j/switching_between_scratch_pads/):
  *"You can only copy from/to scratch pads and you can't 'switch' them as a
  whole."* The OP wanted DP-style Chunks and concluded "well that sucks."
- **A pad cannot have its own tracks or channels.**
  [r/StudioOne](https://old.reddit.com/r/StudioOne/comments/17v9r30/does_anyone_actually_use_the_scratch_pad/):
  *"I love the concept but I wish it had its own independent tracks and
  channels. Like a .song file within a .song."* — with the workaround's
  failure mode spelled out: "unless I forget and then change all the plugins
  without realising 🤣".
- **The Arranger track is global**, so a variant needing a longer intro
  breaks:
  [r/StudioOne](https://old.reddit.com/r/StudioOne/comments/1u3h3rt/what_would_be_a_good_workflow_for_creating_song_versionsvariations/)
  — *"a varying number of tracks renders Scratch Pad useless."*
- **Shared-copy links are lost across the boundary**: "when you transfer to
  and from it, events lose their 'shared duplicate' link."

What users love it for `[V]`: indecisive clients; parking a cut verse;
making 10/30/60-second edits of the same song; tracking a part at 85% tempo
then pasting to the main timeline where it auto-timestretches.

What they hate: the `V` key opens *another* pad rather than toggling; it
eats horizontal screen space; several say "it's just cleaner and simpler to
save as a copy."

`[J]` Scratch Pads have the right *idea* (a second timeline sharing the
mixer) and the wrong *boundary*. Because a pad can't own tracks and can't be
promoted wholesale, it degrades into a clipboard with a view. **Either the
variant is content-on-existing-tracks — say so and keep it fast — or it is a
full arrangement and needs its own track set.**

### 2.6 REAPER — fixed lanes, and an undo system that is nearly the whole feature

From the **REAPER User Guide 7.78**
([reaper.fm](https://www.reaper.fm/userguide/ReaperUserGuide778.pdf)) and the
[changelog](https://www.reaper.fm/whatsnew.txt) `[V]`.

**Fixed item lanes** (v7, §8.12): *"They are effectively **tracks within
tracks**, up to **256 lanes** within a single track."* Lane context menu:
Play only (selected) lane / Toggle playing lane / Play all lanes. Mouse
modifiers on the lane header (§8.12.8): default click = *Play only this
lane*; `Ctrl` = *Toggle*; **`Alt` = "Play only this lane while mouse button
is pressed"** — a momentary solo. Comp areas are first-class objects.
`Explode takes to fixed lanes` converts the legacy take model.

And the changelog entry that names the intent outright `[V]`:

> "Fixed lanes: add action to play only most recently playing lane
> **(for A/B comparison)**"

**Take-level A/B** (§8.11): "Pressing **T** will cycle through the takes for
auditioning", plus **ranked take markers** (up-rank yellow, up to five;
down-rank red) with bulk "Delete takes that are down-ranked" / "not
up-ranked". `[J]` The only rating-driven *reduction* workflow found in any
DAW.

**The undo history as a version system — and this is the closest thing
shipping to the idea this dossier serves** (§2.28, §22.13) `[V]`:

- Undo History window (`Ctrl+Alt+Z`); **double-click any entry to load that
  state.**
- Preferences: cap **undo memory in megabytes**; choose whether
  item/track/envelope/time-selection/cursor changes create undo points;
  **"Ensure that if the allocated undo area becomes full, the most recent
  actions will be retained"**; **Save undo history with project files**;
  **Allow load of undo history**; **Store multiple redo paths**.
- Branching, verbatim: *"Store multiple undo/redo paths. You can even store
  **alternate sequences of commands and actions, then switch between
  them!**"* and *"whenever you go back to an earlier point any actions you
  take from that point on will be stored as an alternate set: REAPER will
  remember both paths independently… every time you return to that point,
  another new undo path will be created."*
- **The UI answer to "how do you display a DAG": don't.** REAPER shows a
  **linear list** and annotates the branch point with **`(*2)`**;
  right-click to choose the path. `[J]` Cheaper and better than a tree
  visualisation, and it keeps the version-control mental model away from the
  user.
- Persistence: a separate **`.RPP-UNDO`** file beside the `.RPP`. The guide:
  *"Even at some later date, you will still be able to revert the project to
  an earlier state if you wish."*

**Cost, from the changelog** `[V]` — REAPER stores *full project states*,
and Cockos repeatedly optimised around that:

> "Undo: improved memory use, **scan for common blocks in history when
> adding states**"
> "Undo: **incrementally updated RPP-UNDO files**, can make for much faster
> save of undo history"
> "Undo system: greatly reduced memory use when loading undo history from
> file"

`[J]` **That Cockos had to build block-level dedup and incremental
serialisation is the single most useful engineering datum in this dossier:
full-project snapshots are viable only with content-addressed sharing.**

Also `[V]`: per-plugin VST/VST3 compatibility settings include *"Save
minimal undo state"* and *"Avoid loading undo states where possible"* —
an explicit escape hatch for plugin-blob bloat, **per-plugin, because the
problem is per-plugin**.

**No native mixer snapshots.** SWS Snapshots
([sws-extension.org](https://www.sws-extension.org/snapshots.php)) `[V]`
fills the gap: mute, solo, pan, volume, sends, FX, visibility, selection,
with per-parameter and selected-tracks-only filters, unlimited slots,
recall actions, stored in the project — **mixer-side only, no arrangement
content.**

`[J]` **REAPER is one UI away from the feature.** It already stores full
project states, supports multiple undo branches with a picker, dedups common
blocks, and persists it all. What it does not do is let you *name* a branch,
*diff* two states, or **extract one state's version of one track into the
live session**. That last one is the idea.

### 2.7 Ableton Live

**Take lanes / comping** (Live 11+)
([ableton.com](https://www.ableton.com/en/manual/arrangement-view/),
[recording-new-clips](https://www.ableton.com/en/manual/recording-new-clips/)) `[V]`:
take lanes under a track's main lane; comp by pulling material into the
main lane; **Audition Mode** for take lanes; **linked-track editing**
propagates comp decisions across multiple tracks and supports "enabling and
disabling Audition Mode on take lanes" across linked tracks.

**Session View as a variant auditioner** — the only product in the survey
that solves the *timing* of the switch
([launching-clips](https://www.ableton.com/en/manual/launching-clips/)) `[V]`:

- **Launch quantization**: per-clip, `None` / `Global` / an explicit note
  value.
- **Launch modes**: Trigger, Gate, Toggle, Repeat.
- **Legato Mode**: the new clip *"take[s] over the play position from
  whatever clip was played in that track before"* — swap variants mid-phrase
  without losing musical position. The manual notes possible dropouts unless
  Clip RAM Mode is engaged.
- **Scenes** launch a row together; **Back to Arrangement** returns control
  to the timeline.

Session and Arrangement are **mutually exclusive per track**
([session-view](https://www.ableton.com/en/manual/session-view/)) `[V]`:
"The Session clips and the Arrangement clips in one track are mutually
exclusive: Only one can play at a time." Arrangement playback "does not
resume until you explicitly tell Live to resume", and the Back to
Arrangement button "lights up to remind you that what you hear differs from
the Arrangement."

And the round trip: "When the Arrangement Record button is on, **Live logs
all of your actions into the Arrangement: The clips launched; Changes of
those clips' properties; Changes of the mixer and the devices' controls.**"
`[J]` That is a **general performance recorder over the parameter system**,
not a clip-copying operation.

**Capture MIDI** `[V]`: retroactive recording from a rolling input buffer.

**No arrangement alternatives at all** `[V]`. The community workaround is
duplicating sets; the demand is real enough that people build tools for it.

### 2.8 Bitwig — comping inside the clip, and the only glitch-free switch

From the [Bitwig Studio User Guide](https://www.bitwig.com/media/bitwig_userguide/pdf/Bitwig_Studio_User_Guide_English_XfuP7Nz.pdf) `[V]`.

**There is no "clip variants" feature.** Bitwig has *comping* and it has
*selector containers*.

**Comping** (§10.1.4): *"Comping in Bitwig Studio is based on the idea of
defining **comp regions**, and then selecting which of the available **take
lanes** (if any) is played within that region."* Comping is an *expression
view* on an audio clip — it lives inside the clip's data model, not as extra
tracks. Comp regions support slide, per-region gain, border adjust and
one-sided border adjust. The auditioning ergonomics are the best in the
survey:

> "To point a comp region to a different take lane: click on any inactive
> portion of a take lane. Or when a comp region is already selected, press
> **[UP ARROW]** and **[DOWN ARROW]** to activate one of the nearest take
> lanes. **[LEFT ARROW]** and **[RIGHT ARROW]** move selection to the
> previous or next comp region. So once your comp regions are defined, a lot
> of auditioning and editing can be done with just the arrow keys."

Bitwig 4.0 release notes `[V]`: *"**Since comping lives within the clip,
comping clips can be freely dragged between the Launcher and Arranger**"*.
And the underlying model is itself unusual — SOS: *"**A Bitwig audio clip is
not a single piece of audio: it's actually a sequence of consecutive audio
'events'**"* — audio clips and note clips are both event containers, which
is why **Operators** could be added to both at once.

**The Selector devices — the only documented crossfaded A/B in the survey**
(§19.4.3 / §19.4.5 / §19.4.10) `[V]`:

> **FX Selector:** "A container that houses multiple audio chains. Only one
> audio chain at a time receives the incoming audio, but **any chain that
> was previously receiving audio remains active until its output is
> silent**. When a different chain is triggered, the previously active chain
> will transition to silence for the set **Fade Out** time. If incoming
> audio was being received before the transition, the new chain will **Fade
> In** over the set time. But if there was no incoming audio before the
> chain switch, the fade in will be skipped."

**Instrument Selector** adds per-note continuation ("each sounding note
continues until its output is silent") and voice modes (Manual,
Round-robin, Free-robin, Free Voice, Random, Random Other) — where Manual
is settable "by user, controller, modulator **or automation**".

`[J]` **Bitwig is the only vendor that treats "switch between alternatives"
as a *signal-flow* problem with a declared fade time and explicit tail
handling, rather than as a *UI state* problem.** Everyone else does
mute-swap, which clicks. That difference is the answer to A/B-without-a-click
— and Bitwig applies it only to device chains, never to clip content.

### 2.9 Mixer snapshots — the untyped-state-bag warning

**Pro Tools Snapshot automation** (Reference Guide p.1469–1471,
Ultimate/Studio only) `[V]` is *not* a scene system — it writes automation
breakpoints: "To a Selection" ("Anchor breakpoints are placed just before
and after the selection"), "To a Cursor Location", via `Edit > Automation >
Write to Current`. Documented gotcha: on an empty automation playlist a
Write command writes to the **entire** playlist. `[J]` The console-heritage
answer — the timeline is the only state store — and why Pro Tools has no
"compare two mixes" feature.

**Cubase/Nuendo MixConsole Snapshots** `[V, existence; forum for behaviour]`:
saved from a camera icon, listed in a Snapshots tab, with `Recall Snapshot
1…10` key commands and a Recall Settings dialog including "Selected Channels
Only". What users report:

- **Automation is the fault line.** Cubase warns on recall: *"Recalling a
  MixConsole snapshot will delete any Insert Automation. Do you want to
  continue?"* ([133370](https://forums.steinberg.net/t/133370/4)) — and
  *"snapshot without automation included is almost useless!"*
  ([116614](https://forums.steinberg.net/t/116614/12)).
- **They don't capture everything.** Sidechain routing is lost on recall
  ([807834](https://forums.steinberg.net/t/807834/1)).
- **They're expensive.** *"I found that by deleting all my MixConsole
  Snapshots, the problem went away… they all perform normal now."*
  ([1024140](https://forums.steinberg.net/t/1024140/1)).
- Long tail: snapshots not recalling
  ([972446](https://forums.steinberg.net/t/972446/1)), rename broken
  ([679099](https://forums.steinberg.net/t/679099/10)), duplicate auto-names
  ([1036186](https://forums.steinberg.net/t/1036186/1)), notes not persisting
  ([877074](https://forums.steinberg.net/t/877074/1)).

`[J]` **Mixer snapshots are the most bug-ridden variant feature in any DAW,
and the reason is structural: a snapshot is an *untyped bag of state* with
no declared schema, so every new mixer feature silently falls outside it
and nobody notices until a user loses work.** Every field must be declared,
versioned, and covered by a round-trip test.

### 2.10 What each product chose, and what it cost

| Product | Unit of variation | What it bought | What it cost |
|---|---|---|---|
| Ardour | track content (region list) | cheap, shareable across tracks, first-class parallel processing | automation stranded at track level; no cross-track variant identity |
| Pro Tools | track content (edit playlist) | mature comping, expand-to-tracks, group-wide increment | playlist name == track name; playlists exclusive; audio automation shared but MIDI CC not |
| Cubase | track content + **cross-track Version ID** | the only real multi-track variant identity; extends to tempo/chord/signature | automation excluded; decade of open requests |
| Logic | three tiers: take / track content / whole project | cleanest ladder; alternatives share assets | project switching too slow to A/B; no automation |
| Studio One | a parallel timeline | right instinct: share the mixer, vary the arrangement | can't own tracks; can't be promoted wholesale; global arranger track |
| REAPER | lane (content) + **whole-project undo state** | 256 lanes/track; persistent branching project history with a picker | no named track versions; snapshots are a third-party mixer-only bolt-on |
| Ableton | clip (Session) + take lane | best switching *timing* in the industry | no track or project variants |
| Bitwig | comp region (inside a clip) + device chain | best switching *audio*; keyboard-fast auditioning | no arrangement- or track-level variants |
| Mixer snapshots | whole console state | recall a mix | untyped state bag → chronically incomplete and buggy |

---

## 3. The universal gap: no variant carries its automation

This is the clearest single finding of the product sweep, and it holds
across every product examined.

| Product | Statement |
|---|---|
| Pro Tools | "All edit playlists on a single audio track share the same automation data." (p.1415) `[V]` |
| Cubase | "Track versions are **not available for automation tracks**." `[V]` — automation "remains 'global' to the track" `[V, forum]` |
| Ardour | Automation lives on the Route; deleting it affects all playlist copies `[V, forum]` |
| Logic | Never mentioned in the documentation; SOS states alternatives "share the same plug-in chain and automation framework" `[V]` |

**Every product punted, and every product has an open complaint thread
about it.** The Cubase requests span at least
[96575](https://forums.steinberg.net/t/96575/13),
[130915](https://forums.steinberg.net/t/130915/4),
[803611](https://forums.steinberg.net/t/803611/1),
[804319](https://forums.steinberg.net/t/804319/1) and
[881326](https://forums.steinberg.net/t/881326/1).

But the requests are not unanimous about *what* they want, and that is the
design insight. Reading [130915](https://forums.steinberg.net/t/130915/4)
carefully `[V]`, users debate three models:

(a) each **parameter** gets its own version history;
(b) each **track version** owns a full automation snapshot;
(c) a **hybrid** where parameter-versions can be *linked* to track-versions.

**The diagnosis** `[J]`: (c) is right, and the reason nobody shipped it is
that **they all modelled automation as a property of the channel rather
than as timeline content.** Automation *is* timeline content — a
region-shaped thing that happens to target a parameter. If the data model
puts automation curves in the same container as audio and MIDI events (a
content layer bound to a track over a time range), then variants get
automation **for free**, and the linking question becomes a per-lane opt-in
rather than an architectural rewrite.

**This is the single biggest opportunity identified in the entire survey.**
Every competitor has a decade-old feature request they cannot cheaply
satisfy, and the reason is a data-model decision made before the feature
existed.

Two adjacent gaps from the same root:

- **No variant carries its mixer state.** Users who want "chorus B is also
  2 dB louder with a different reverb send" must choose between Track
  Versions (content only) and MixConsole Snapshots (mixer only, buggy) with
  nothing joining them. Cubase's own users asked for exactly the union
  ([881326](https://forums.steinberg.net/t/881326/1)) `[V]`.
- **No time-ranged variant.** Every product's unit is "whole track for all
  time" or "whole project". Nobody offers "bars 33–48 of these six tracks,
  take B" as a nameable, switchable object. The Cubase request for
  range-editing across versions without switching
  ([1018628](https://forums.steinberg.net/t/1018628/2)) `[V]` is this gap
  surfacing.

---

## 4. The extract-to-take design

`[J]` throughout except where cited. This section is design analysis, not
research.

### 4.1 Scope, and the default

A revision is whole-session state; extraction needs a *scope*.

| Scope | What it is | Problem |
|---|---|---|
| S1 whole session at rev R | a snapshot | doesn't answer the gesture |
| S2 one track, everything (content + mixer + instrument + plugin state) | "the track as it was" | confounds content and mix in the A/B |
| S3 **one track's content only** | Ardour playlist semantics | needs a container concept |
| S4 a time range on one track | "the chorus as it was" | ambiguous edges |
| S5 a time range across N tracks | "the section as it was" | needs group switching |
| S6 the objects touched by the selected ops | "whatever that edit changed" | scope is derived, can be scattered |
| S7 explicit user selection resolved at rev R | precise | user must already know |

**Default: S3, content-only, one track.**

The stated purpose is A/B by alternating which is active. That only works
if both things sit at the same point in the signal chain. Extract the mixer
state too and you compare content **and** mix simultaneously — a confound,
and you forfeit level matching (§4.8). **Hold the chain constant, vary the
content.**

Prior art converges hard here: Ardour ("a track *has* a playlist"; the
channel strip is not part of it), Cubase (events and parts, not the channel
strip), Logic ("while sharing the same channel strip and plug-ins").

The content set of track T at rev R is: audio clips on T, MIDI clips on T
plus their event chunks, and automation lanes whose target is T's
*instrument*.

**Automation is the contentious member**, and §3 says why. Split by target,
not by convention: a lane travels with content iff its target is the
track's instrument node; lanes targeting the mixer strip stay with the
track. In AURA this is currently unambiguous and free — every existing lane
targets an instrument (`"track:<id>"` means the track's built-in live
instrument, and `GainAutomatedNode` wraps the instrument, not the fader).
**v1: all automation lanes travel with content**, under a rule that
survives the arrival of mixer automation.

**Making the ambiguity visible.** Pressing extract **never mutates**. It
computes an `ExtractionPlan` and shows it:

1. **The scope drawn in place** — the exact clips and notes highlighted in
   the timeline where they live. Not a list; the actual objects.
2. **A scope chip row**, pre-selected from the diff ranking (§4.2):
   `Content` (default) · `Content + instrument` · `This range only` ·
   `Everything that edit touched`.
3. **The carry manifest** (§4.5) — shared / copied / dropped / unresolved,
   with reasons. Shared items are labelled, because that is where surprise
   lives.
4. **Warnings and refusals** (§4.6, §4.10) before the button, not after.

Three rules keep it unsurprising:

- **Never silently widen.** If the ops at rev R touched three tracks and the
  cursor is on one, propose the one and offer *"2 more tracks changed here
  — include?"* as a chip.
- **Name the result from the diff, not a counter.** `Lead — before "rewrote
  bars 9–16"` beats `Lead (2)`.
- **Rank the alternatives from the diff.** Ops confined to one track →
  propose that track. Ops confined to a contiguous span → propose the range
  chip. Ops across N tracks → propose a **take group** (Cubase's Common
  Version ID, validated in the market).

### 4.2 Semantic diff

It operates on **state**, annotated by **ops**. Diffing the op log directly
produces exactly the failure mode to avoid: four moves of one clip is one
move; add-then-delete is nothing; a knob drag is 1400 sets. **The facts come
from a state diff** (idempotent, cancellation-correct); **the verbs come
from the op log** (intent: "quantized" vs "moved 40 notes"; and the actor).

Four stages:

**(a) Canonicalise.** Materialise both revisions. Bulk payloads compare
**by chunk hash first**. AURA already rewrites AMEV chunks under a new UUID
on every change — a free "did this change" oracle that makes diffing a
million-note pattern O(1) in the common case. Descend only where hashes
differ.

**(b) Element diff with matching, not set difference.** Match notes by
`(key, tick)`, then by `(key, nearest tick within one grid unit)`. A moved
note must read as *moved*, never as *deleted + added* — this single choice
is the difference between "34 notes edited" and "68 notes changed". Clips
match by id, which survives moves.

**(c) Classify into musical facts.**

```
NotesAdded/Removed/Moved { track, clip, count, span, dominant }
ClipsMoved / ClipTrimmed / ClipsAdded
MixChanged { track, param, from, to }
PluginParamChanged { instance, param, from, to }
PluginStateReplaced { instance }        // opaque, no detail available
AutomationEdited { target, param, points, span }
TempoChanged { at, from, to }
TrackAdded / Removed / Renamed
```

The `dominant` field earns its keep: if every matched note moved by exactly
+12, emit **"Transposed up an octave"**, not "Moved 40 notes". If every
onset landed on a grid line with pre-move offsets under a 32nd, emit
**"Quantized"**. These are cheap tests over the match set and they are most
of what makes the output legible.

**(d) Group, rank, budget.** Group by `(track, bar-bucket, fact kind)`,
merging adjacent buckets. Render as `<verb> <object> in <track>, <where>` →
*"Rewrote 34 notes in 'Lead', bars 9–16"*.

**Hard budget: the top level is never more than 7 entries.** Past that,
collapse by dropping the least-significant grouping dimension — first time
range, then fact kind, then track — yielding *"Edited notes across 5 tracks,
bars 1–64"* with a disclosure triangle.

**Rank by audible consequence, never by element count.** Count is the trap
that produces "1400 param sets". Structural (track/clip add/remove) >
content (notes) > continuous (params). For params, rank by perceptual delta:
dB for gains, semitones for pitch, fraction-of-range where the unit is
unknown. A change on a muted track ranks below the same change on an audible
one; a change outside every clip ranks lower still.

**Failure modes, addressed:**

- *"1400 param sets"* → transient gesture ops are never journaled (the draft
  envelope already has `transient: true`, latest-wins), and the diff is
  state-based anyway. Renders as `Reverb mix 12% → 34%`.
- *"Modified 40,000 notes"* (a transpose) → dominant-transform detection.
- *"Plugin state changed"* (opaque blob) → we genuinely cannot diff it.
  **Never fabricate detail.** Emit `Serum: state changed (contents not
  comparable)`, plus whatever param-level facts we do have, separately.
- *Edits that cancel* → the state diff yields nothing; render `No net change
  (12 edits cancelled out)`, which is itself informative.
- *Rename churn* → lowest-ranked fact kind, always collapsed to one entry.

**The part with no code-diff analogue: the audible diff.** Every entry
offers **"hear it"**: `offline::build_graph` + `offline::render` over the
scoped span at both revisions into two short buffers, A/B'd. That capability
already exists in `audio/offline.rs`.

This is not a nice-to-have. **In a temporal medium the audible diff is the
primary artifact and the text list is the index into it.** Reading `Notes
moved in "Lead" bars 9–16` is not the same act as consuming the change,
whereas reading a text diff *is* the same act as consuming the code. Design
the list as navigation, not as review.

### 4.3 Object identity across time

**Invariant first:**

> **I1 — One live identity.** At any instant, an ID names at most one object
> that can be rendered. Two objects that can sound simultaneously never
> share an ID.

This is not cosmetic in AURA. Violate it and three things break **silently**:

- `Store.slots: HashMap<String, usize>` — two tracks, one key, one of them
  gets no RT slot and is inaudible with no error.
- The decoded-clip cache in `ensure_loaded` is **keyed by clip id** — the
  extracted clip serves the other clip's decoded audio. **Wrong audio, no
  diagnostic.** The nastiest one.
- Undo's inverse ops (`SetTrackGain{track: X, …}`) resolve to whichever X
  the linear scan finds first. Non-deterministic undo is worse than no undo.

**But fresh IDs break every inbound reference. So: two-level identity.**

```rust
struct ObjectId(Uuid);    // instance identity — unique among live objects
struct LineageId(Uuid);   // "this is the same thing, across time and takes"
```

Every object gains a `lineage`, seeded at creation from its own `ObjectId`.
**On extraction, every extracted object gets a fresh `ObjectId` and keeps its
`LineageId`.** I1 holds; "same thing" remains expressible (cross-take
diffing, "apply this to every take of the lead", group switching); and
history stays valid because ops in the log target the `ObjectId` that existed
at that rev and are never retargeted.

**The remapping rule:**

```rust
struct Remap(HashMap<ObjectId, ObjectId>);  // old -> new, ONLY for in-scope objects
```

> **R.** For every reference field in every extracted object: if the referent
> is in `Remap`, rewrite it; otherwise leave it pointing at the live object
> (a **share**) and record it in the carry manifest as shared.

Decidable, because it only needs "is the referent in scope".

**The structural move that eliminates most remapping.** For content-only
takes, do not re-parent objects by rewriting `track_id`. Adopt Ardour's
shape: the take is a container the track points at.

```rust
struct TrackState { …, takes: Vec<TakeId>, active_take: TakeId }
struct Clip       { id, take: TakeId, … }   // replaces track_id
```

Now **`track_id` never needs rewriting, because it no longer exists on
content rows.** This is a real schema change and it is the correct one; it
must happen in v1 (§6.3) because retrofitting it touches everything.

For `Scope::TrackFull` (a genuinely new track), the track *is* in scope:
fresh track `ObjectId`, and lanes with `target_node == "track:<old>"` are
rewritten. Plugin instances: rewritten iff copied; if shared, not rewritten
— and then the A/B is confounded, which the manifest must say.

> **I2 — Reference closure, no silent substitution.** After extraction every
> reference resolves to a live object, or to an explicit `Unresolved { was,
> kind, name }` placeholder that the UI renders and the engine treats as
> silence. No dangling ids. No implicit fallbacks (deleted bus → master,
> missing plugin → bypass, missing file → skip).

**Prior-art check.** Ardour mints new region IDs on copy while sharing
sources (§2.1) `[V, source]`. Cubase's Track Versions share the channel
strip and provide a Common Version ID (`[V]` for the feature list; the
finer behaviour `[U]` since Steinberg's docs redirect to an unreadable PDF).
Convergent lesson: **fresh instance identity, shared heavy resources, mixer
stays put, and a way to switch several tracks together.**

### 4.4 Tempo and time

**Default: musical position wins.** Extracted MIDI and automation keep their
tick positions and are re-timed by the *current* tempo map.

The tempo map is a property of the song, not of the material. If the user
slowed 128 → 120, they want the old lead at 120 — otherwise the A/B compares
"old part" against "new part at a different tempo". More decisively: keeping
wall-clock position would make the extracted part drift against the grid and
against every other track, so it could not be *swapped in place* at all —
and swappable-in-place is the whole feature.

AURA makes this the free path: notes and automation are already stored in
ticks, and `TempoMap` is the only bijection. Re-timing costs nothing,
because the ticks *are* the storage.

**When wall-clock is right:** when the purpose is forensic rather than
musical — *"what did this sound like then"*. That is a different feature:
**bounce from here**, using `offline::render` with the *revision's* tempo map
into a fixed audio clip.

> **Takes are musical. Bounces are forensic.** A revision preview that plays
> at the old tempo is correct. A *take* that plays at the old tempo is a bug.

**Time signature:** same rule — session-global, never carried into a take.
But there is a trap: bar numbers label the diff, and if the meter changed,
bar 9 *then* is not bar 9 *now*. **Every diff entry stores its range in
ticks and renders bar labels through the meter map of the revision it
describes.** When the two disagree, show the current label with the old in
parentheses: `bars 9–16 (was 12–20)`. Never emit a bar number without
knowing which map produced it. AURA has no meter map today
(`time_signature` is hardcoded `(4,4)`), so v1 is degenerate — but the field
and the tick-based storage must exist now, or every stored label silently
rots when the meter map lands.

**Audio clips (sample-locked content):**

- *Tempo unchanged* → verbatim samples.
- *Tempo changed, no source-tempo metadata* → anchor the clip's **start** to
  its musical position (old sample → tick via the *old* map → sample via the
  *new* map); leave **length in samples untouched**. Badge the clip: `⚠ tempo
  changed — audio not stretched`. It will end in the wrong place under a
  large tempo change — honest and visible, and the user can then choose.
- *Known source tempo* → *offer* stretch; never do it silently.

> **Extraction never performs a lossy transformation implicitly.** Re-timing
> MIDI by ticks is lossless and automatic. Time-stretching audio is lossy,
> latency-adding and quality-costing — it is a prompt.

**The one case a take cannot reproduce the past:** tempo events *inside* the
extracted span were changed or deleted (a ritard that existed at R and is
now gone). Detect it (`tempo map differs within the take's tick span`) and
say so: *"Tempo changed within this range — this take will not sound as it
did at that revision."* `[Restore tempo map] [Continue]`

### 4.5 The carry manifest — what comes along, and what does not

| Resource | Decision | Rationale / cost |
|---|---|---|
| **Audio files** | **Shared**, always | Copying a 2 GB take to A/B a clip edit is indefensible. Deletion later → `Unresolved` per I2, renders silence + badge. |
| **Decoded sample cache / waveform pyramids** | **Shared** | See the required fix in §7. |
| **AMEV note chunks** | **Shared by reference** | Immutable, rewritten under a new uuid on edit. Free COW. **But** the GC root set must include takes and retained revisions — see §7. |
| **Automation lanes** | **Copied** (record), chunk shared | Small, and part of what's being compared. |
| **Sampler instruments** | **Shared** | Already copied into the project; immutable in practice. A second `SamplerNode` costs a 64-voice pool over shared sample data. |
| **Track mixer state** | **Not extracted** | Deliberately the constant in the comparison. |
| **Tempo / time-signature map** | **Never copied** | Session-global; two live tempo maps is incoherent (what does the ruler show?). |
| **Transport / loop region** | Dropped | Session-global UI state. |
| **`cache/`, freeze renders** | Dropped | Regenerable. |

**Plugin instances — where it costs real money.** Three options:

- **Shared** (both takes use the one instance). Zero cost; the plugin's
  state is not part of the A/B. Correct for content-only takes. **Default.**
- **Copied** — a real second instantiation. **RAM** is the usual killer (a
  3 GB orchestral library twice); **CPU** (a second `process()` per block —
  and even if only one renders, the other must be `prepare`d and resident to
  switch without a gap); **latency** (if the copy reports different latency,
  PDC changes and the A/B is no longer sample-aligned — silently converting
  a content comparison into a phase comparison); **licensing** (per-machine
  instance caps); **load time** (sample-scanning instruments take seconds).
- **State-only copy** (one instance, two blobs, switch = `setState`). Cheap
  in RAM, but `setState` is not RT-safe, not gapless, resets voices in most
  plugins, and can take milliseconds to seconds. Fine for "recall a preset",
  wrong for A/B.

**Rule:** copy an instance only when it is itself in scope (`TrackFull`, or
the user explicitly ticked "compare the instrument too"), and then
**instantiate and `prepare` at extraction time, never at switch time.** If
instantiation would exceed budget (default: total plugin RAM > 50% of free,
or instantiation > 500 ms), refuse the copy and offer **bounce this take to
audio from that revision** — which `offline::render` already supports.

**Deleted bus.** AURA has no buses today (`kind: "bus"` is reserved and
rejected), so v1 is trivial. Designing forward: **do not silently reroute to
master.** That changes level, EQ and any bus processing, so the A/B compares
content *plus a missing chain* — and the user will blame the content. Per
I2: produce `Unresolved { bus }`, refuse to activate the take, and offer
three explicit repairs: (1) **recreate the bus from the revision** — we have
its state, and it is only possible *because* we keep revisions; (2) map to
an existing bus; (3) route to master and accept the difference, with a
banner that stays visible the whole time the take is active. The banner is
not decoration: an unlabelled level difference is precisely how people make
bad mix decisions.

**The carry manifest is a first-class stored artifact on the take.** It is
what the UI shows before creation and what "why does this sound different?"
resolves against three weeks later.

### 4.6 A/B switching

**Genuine swap, not "both audible with one muted".** Two audible-but-muted
takes means both must be **rendered** (you have to render to keep tails and
voice state coherent) — double CPU per take, forever. At FL-scale ambition
that is fatal. And mute-based A/B tempts implementations into post-fader
muting, which leaves sends and bus feeds live — the classic "why is the
reverb still playing the old part". (§2.3's Cubase export bug is the same
disease.)

But a hard graph swap mid-note **kills the outgoing take's voices and plugin
tails**, which clicks and — worse — makes the comparison unfair: whichever
take you switch *away* from always sounds truncated.

**The switch is three things:**

**1. Structural swap, control-side.** `TakeSetActive` is a normal `Store`
mutation followed by one `ControlMsg::Rebuild`. The new `RtGraph` carries
the new take's clips and event list. This is exactly the `loopjam.rs`
pattern — prepare off-thread, make the visible change a store mutation plus
one rebuild — so it reuses a proven path.

**2. Tails.** The outgoing take's live node is **not dropped at the swap**.
The new graph carries a transient extra `RtTrack` holding the outgoing
`LiveNodeCell` with an **empty event list**, marked `releasing`. It gets
`all_notes_off()` — release, not kill, which AURA already does on
discontinuities — and keeps rendering into the same slot's sum until quiet.
The control thread drops it at the next rebuild after a silence detector
fires (or a 4 s cap). Cost is bounded: at most one releasing node per track,
only during the switch. For audio clips, the same via a 5–10 ms equal-power
crossfade. Without this you click on *every* A/B press, and A/B is a gesture
people hit dozens of times a minute.

**3. The RT thread does nothing new.** It still just pops a graph when the
retire ring has room. No decision-making in the callback. This is
ARCHITECTURE §2.6's principle ("the engine notices, the control plane
decides") applied as *"the control plane decides when, the engine only
swaps."*

**Should it wait for a bar line? Mostly no — and this is the counterintuitive
one.**

- **Stopped** → switch immediately.
- **Playing** → default **instant with crossfade**, not bar-quantized.

A/B comparison runs on echoic memory, which decays over roughly **3–4
seconds** (§5.1). Waiting up to two bars (4 s at 120 bpm) destroys the
comparison — you end up comparing against a memory that has already faded.
**Immediate switching is what makes A/B work at all.**

Bar-quantized switching is right for a *different* gesture: **arrangement
auditioning** ("play this whole section with take B"), where the material is
phrase-length and switching mid-phrase is musically incoherent.

So offer `Instant | At next bar | At loop wrap`, default **Instant**, and
auto-select **At loop wrap** when the loop region is active and its length
matches the take's span — because then the user is auditioning a phrase, and
`loopjam` already provides that seam.

**Position preservation.** Ableton's Legato Mode (§2.7) is the reference:
the incoming material takes over the play position rather than restarting.
Without this, an "A/B" is two auditions, not a comparison.

**Toggling.** One key (`V`, or Tab-style cycling) on the focused track, plus
a click target in the track header showing A/B state. The requirement that
shapes it: **reachable without stopping playback and without hitting a
specific pixel**, because this is a rapid, repeated, often eyes-closed
gesture. Plus an MCP tool, so an agent can drive comparisons.

**Determinism.** The switch must be deterministic under offline render:
bounce reproduces whichever take was active. Obvious, and it breaks the
moment "active take" lives in UI state. Keeping `active_take` in the `Store`
gets this, persistence, and op-log coverage for free.

### 4.7 The AI dimension

**Attribution.** The draft op envelope's `origin` is for echo suppression
("was this my window's batch?"). **Do not overload it.** Add a distinct,
persisted, non-optional actor:

```rust
enum Actor {
    Human  { session: SessionId },
    Agent  { agent: String, run: RunId },
    System { reason: &'static str },   // migration, crash recovery
}
```

Set it at the `ControlPlane` boundary. AURA has a structural advantage:
ARCHITECTURE §11 already mandates that **both** front doors go through
`ControlPlane`, so attribution becomes a property of the seam — it cannot be
forged by a caller or forgotten by a new command. Today attribution is
inferred frontend-side from an event bus with a 400 ms adoption grace window
(`GenJob.origin: "ui" | "agent"`); that is a heuristic and should be
*replaced* by the real field, not extended.

**`RunId` matters more than the batch.** A run is one agentic task, possibly
dozens of batches. Every user-facing operation ("undo the agent's changes",
"review what it did") is scoped to a run.

**Per-actor undo scoping.** The honest answer: **selective undo is not always
possible, and pretending otherwise produces corruption.**

- The undo stack is **not** per-actor. There is one log. What is per-actor
  is the *selection of what to invert*.
- "Undo the agent's run" = compute the run's inverse and apply it as a **new
  forward batch**. `git revert`, not `git reset`. Never pop history (I3).
- **Dependency check before offering it.** With `W` the run's write set, and
  later ops with write set `W'` and read set `R'`: the revert is clean iff
  `W ∩ (W' ∪ R') = ∅`.
- If not clean, **never silently partial-revert**. Show the conflicting
  objects: *"3 of the agent's 14 changes were edited by you afterwards.
  Revert the other 11 and leave those? [Revert 11] [Revert all, discarding
  my edits] [Cancel]"*.
- Cheap path covering most real cases: **a run is bracketed by revision
  markers**, so when nothing followed it, "undo the run" = materialise the
  pre-run revision for the objects in the run's write set. **That is exactly
  the extraction machinery from §4.1.**

> **Selective revert and extraction are the same operation with different
> targets** — one replaces in place, one adds beside. Build the core once.

**Is there a code-review diff for a music session? Partly.**

*What transfers* from Cursor / Claude Code / Zed: a **staged, reviewable
unit** (the run) with accept/reject at hunk granularity, where a hunk is a
§4.2 diff entry; **inline, in-place presentation** (show the change in the
timeline where the object lives, old state ghosted behind new — musicians
navigate by position, and a side-panel diff throws position away);
accept-all / reject-and-re-prompt; **an intent statement from the agent**
(the commit-message analogue — the agent should be *required* to label its
run); and a checkpoint to return to.

*What does NOT transfer:*

- **You cannot read a diff of music.** Text diff works because reading text
  is the same act as consuming it. The primary review artifact must be
  **audible**.
- **Review costs real time.** A 200-line code diff is reviewed in a minute;
  sixteen bars takes 30 s *per listen* and needs at least two. Therefore
  **agentic edits in a DAW must be far fewer and far coarser than agentic
  code edits**, or review cost swamps the benefit. An agent making 40 small
  edits is an anti-pattern here even though it is fine in code.
- **There is no test suite.** No oracle for "did the music get better". The
  closest analogues are weak and must be presented as *lints*, never
  verdicts: clipping, DC offset, loudness jumps, notes outside key/range,
  voice-count explosions, phase cancellation with existing material.
- **Partial acceptance is often musically incoherent.** Accepting the new
  bass line while rejecting the drum pattern it was written against can be
  worse than either. The agent must declare **cohesion groups** —
  co-dependent changes that accept/reject together. No code-review analogue
  exists, and it is the thing most likely to be got wrong.
- **Non-determinism.** "Reject and regenerate" yields something different
  each time. So **rejected takes are retained**, not discarded — inverting
  the coding-tool convention where rejected diffs vanish. Cheap here
  precisely because takes exist.
- **Feedback latency.** The user must *play*. The review UI should auto-arm
  and loop the affected range.

**Therefore the model is apply-then-review with a cheap revert**, not
propose-then-apply. You cannot review music without hearing it, and you
cannot hear it without it being in the graph. That is the deepest divergence
from coding tools, and it makes the cheap-revert path load-bearing rather
than a convenience.

**One MCP policy consequence.** The current gate is `confirmDestructive`
with a 60 s timeout **per call**. A 40-op run would prompt 40 times — and
per-call confirmation is exactly what trains people to click through. The
run concept fixes it: **confirm the run's scope once** — *"this agent may
edit MIDI on 'Lead' between bars 1–32 for the next 10 minutes"* — a
capability grant rather than a per-call interrupt.

### 4.8 Level matching

**Not optional.** Louder reliably reads as "better", and the bias operates
well below a decibel. If take B is 0.8 dB hotter because it has one more
layer, the user picks B and believes the choice was musical. **A feature
whose purpose is honest comparison cannot ship without addressing this.**

- On extraction, compute **LUFS-I (ITU-R BS.1770)** of the *scoped region*
  at both revisions via `offline::render`. Store `loudness_lufs` per take.
- **Level-matched A/B on by default**: per-take trim = `target − take_lufs`,
  where target is **the quieter of the two**. Never boost — boosting risks
  clipping and changes the behaviour of anything level-dependent downstream.
- **Show the trim.** `B: −0.8 dB (matched)`. A hidden trim is its own
  dishonesty.
- If the two takes have **different plugin latency** (only possible when
  plugins were copied and differ), refuse level-matched A/B and say why — an
  unaligned A/B compares phase, not content.
- Be honest about the metric: LUFS-I over a short region is noisy. Under
  ~3 s, fall back to short-term or peak-normalised matching and label it
  approximate.

Note §5.3: shipped implementations are deliberately cheaper than this, and
that is a legitimate starting point.

### 4.9 Naming and the mental model

Reject the VCS vocabulary. "Branch", "commit", "revert", "merge", "HEAD"
impose a model where the user must reason about a graph of states —
precisely the load to avoid. Musicians already have better words.

| Concept | Name | Why |
|---|---|---|
| a point in the edit history | **a point in History** | History is a place you *scrub to*. Reuse the transport metaphor: a second ruler. Never "commit" or "revision" in the UI. |
| internal id | `RevId` | fine in code, never surfaced |
| an alternative content set on one track | **Take** | see below |
| several tracks' takes switching together | **Take group** | natural extension |
| whole session at a point | **Snapshot** | reserve strictly for the whole-session sense |
| the action | **"Bring back as a take"** | verb-first, no nouns to learn |
| audio render of a past state | **"Bounce from here"** | existing DAW word |
| undoing an agent run | **"Undo the agent's changes"** | plain language, never "revert" |

**Why "Take":** every musician knows it — an alternative performance of the
same part, on the same track, through the same chain. That is exactly the
semantics of §4.1. Ardour says "playlist" (jargon), Cubase "Track Version"
(jargon), Logic "Alternative" (vague).

The apparent collision is the strongest argument for it: AURA already uses
"take" for one recording pass, and D-09 plans take lanes and comping.
**Unify them.** Recorded takes and history-extracted takes should be the same
object in the same list. A user who recorded four takes and then brought
back the pre-edit version of take 2 has five takes. That is coherent.

**The mental model, in one sentence:**

> Every track has a stack of takes. History is a tape you can rewind to hear
> what a track used to be, and "bring it back as a take" puts that old
> performance on top of the stack so you can flip between them.

No graph. No branching. No merge. History *is* linear (it is an op log), it
is a tape, and the only thing you can do with the past is bring something
forward as a take. Three nouns — **History, take, bounce** — and everything
else is an elaboration inside them.

Two rules keep the model honest:

- **Visiting or extracting from the past never changes history.** It is an
  ordinary edit, appended. This is the single largest source of
  version-control anxiety and it is avoidable by construction.
- **The past is read-only.** You can look, and you can bring things forward.
  You cannot edit *at* a past point. Refusing to make history a tree is what
  keeps the VCS model off the user — even though the tree exists underneath
  (§1.4).

### 4.10 When the feature must refuse

Principle: **refuse loudly rather than produce something that plays but is
wrong.** Wrong-but-playing costs the user an hour of confused mixing.

1. **The object didn't exist at that revision.** → *"'Lead' didn't exist at
   this point — it was created at [rev]. [Go there]"*. Navigation, not a
   silent empty take.
2. **Nothing in scope changed.** → *"Nothing to extract — 'Lead' is
   unchanged since here."* An identical take isn't harmful, but the user
   will A/B two identical things and conclude the feature is broken.
3. **Required media is gone.** Allow creation with `Unresolved`
   placeholders, **refuse activation**, offer relink.
4. **Plugin unavailable.** Refuse activation; offer bounce-from-that-revision
   if still possible; otherwise *"the sound of this revision cannot be
   reproduced: plugin X is not installed."*
5. **Plugin rejected the saved state** (version mismatch — plugins are
   allowed to do this). Never swallow it: *"Serum rejected the saved state
   from this revision. The take will use the current state."* The A/B is then
   not what it claims and must say so.
6. **Resource budget exceeded** (§4.5). Refuse the copy, offer the bounce.
7. **No track slot.** `MAX_TRACKS = 64` with dense allocation; `TrackFull`
   needs a slot. → *"No free track slots (64/64)"* — **and offer content-only
   as the fix**, since a content-only take needs no slot.
8. **The revision predates a migration whose result isn't reconstructible.**
   AURA's own stated policy is "never best-effort-parse someone's session".
   Same principle: refuse, don't guess.
9. **Tempo map changed inside the span** — not a refusal, a mandatory
   warning (§4.4).
10. **Beyond the history horizon.** The revision list must display its own
    horizon: *"History before [date] has been compacted."* Never show a
    reachable-looking point that cannot be materialised.
11. **Recording in progress.** → *"Can't extract while recording."*
    (`loopjam` already gates on transport state.)
12. **A destructive external edit sits in the span** (a stem-separation or
    generation job replaced a source in place). Refuse with the specific
    reason.

Every refusal has the same shape: **what can't be done, why, at which
revision, and one concrete action.** Never "extraction failed."

---

## 5. A/B methodology and history-browser UX

### 5.1 The number that justifies instant switching

**Echoic memory lasts 3–4 seconds.**
[Wikipedia: Echoic memory](https://en.wikipedia.org/wiki/Echoic_memory) `[V]`:
"typically 3 to 4 seconds", with the spread in the literature noted —
"Guttman and Julesz suggested that it may last approximately one second or
less", while "Eriksen and Johnson suggested that it can take up to 10
seconds."

`[J]` This is the sourced backbone for §4.6. Switching slower than echoic
decay means comparing against a memory that has already faded — which is not
a comparison.

**ABX statistics**
([Wikipedia: ABX test](https://en.wikipedia.org/wiki/ABX_test)) `[V]`: known
A and B, then X drawn at random; "The subject is then required to identify X
as either A or B." 95% confidence is standard — **20 trials → 15 correct**;
**25 trials → 18 correct**; pooled significance at correct responses
exceeding `N/2 + √N`. Trial counts (attributed to QSC): min 10 per round,
max 25, to avoid fatigue. And the framing that matters here: the method
"relies on short-term memory rather than long-term recall, making
comparisons immediate and direct."

**foobar2000's ABX comparator**
([foobar2000.org](https://www.foobar2000.org/components/view/foo_abx)) `[V]`
— `foo_abx` 2.2.3, 2025-09-19, "Performs a double-blind listening test
between two tracks." The component page carries **no** detail on level
matching, switching accuracy, or logging `[U]`.

**MUSHRA / ITU-R BS.1534**
([itu.int](https://www.itu.int/rec/R-REC-BS.1534/en); method detail from
[Wikipedia](https://en.wikipedia.org/wiki/MUSHRA)) `[V]`: BS.1534-3,
approved October 2015. Anchors — "a low-range and a mid-range anchor should
be included… typically a **7 kHz and a 3.5 kHz low-pass** version of the
reference" — to "calibrate the scale so that minor artifacts are not unduly
penalized." Hidden reference included, should be rated 100. Scale 0–100.
Post-screening: disqualify "all listeners who rate the hidden reference
repeat below **90** MUSHRA points for more than **15%** of all test items."

**ITU-R BS.1116-3**, February 2015
([itu.int](https://www.itu.int/rec/R-REC-BS.1116/en)) `[V]` — catalog page
only; method body not fetchable.

⚠ **`[U]` — no numeric level-matching tolerance was verified.** The
often-quoted ~0.1 dB figure is not sourced here; neither ITU page exposes
recommendation text, and the MUSHRA article contains no statement about
level matching at all.

⚠ **`[U]` — the psychoacoustic "louder is perceived as better" claim.**
[Loudness war](https://en.wikipedia.org/wiki/Loudness_war) `[V]` records
that "the industry believed that customers preferred louder-sounding CDs,
**even though that may not have been true**", and that analysis found "no
connection between sales and loudness, and that people prefer more dynamic
music." The *engineering* case for gain matching is well attested in plugin
manuals (§5.3); the psychoacoustic claim is not verified by anything fetched
here. §4.8 should be argued from the engineering side.

### 5.2 The plugin A/B convention

**FabFilter** (house-wide — the same page exists at
`/help/<plugin>/using/undoredo` for Pro-Q, Pro-C, Pro-L, Pro-MB, titled
"Undo, redo, A/B switch")
([fabfilter.com](https://www.fabfilter.com/help/pro-q/using/undoredo)) `[V]`:

- **Exactly two slots.** "The A/B button switches from A to B and back… if
  you click this button twice, you are back at the first state."
- **Copy = active → inactive.** "The Copy button copies the active state to
  the inactive state. This marks the current state of the plug-in and allows
  you to go back to it easily with the A/B button."
- It sits **alongside a full undo history**, not with the preset browser:
  "Every change to the plug-in (such as dragging a knob or selecting a new
  preset) creates a new state in the undo history."
- Relevant limitation: "If the plug-in parameters are changed without using
  the plug-in interface, for example with MIDI or automation, no new undo
  states are recorded."

**TDR Nova** ([docs.tokyodawn.net](https://docs.tokyodawn.net/nova-manual/)) `[V]`:
"A/B allows comparison of two alternative control settings. A>B and B<A
copies one state over the other." `[J]` The *directional* copy pair is more
explicit than FabFilter's single Copy button.

`[U]` — iZotope (support.izotope.com returns 403), Waves, UAD. Whether
slots are per-preset or per-session is **unverified everywhere**.

`[J]` **Model that emerges:** two slots, momentary toggle, one-way copy to
seed the other slot, grouped with undo/redo rather than with presets.

### 5.3 Level matching in shipped products is deliberately cheap

- **FabFilter Pro-Q Auto Gain**
  ([fabfilter.com](https://www.fabfilter.com/help/pro-q/using/output)) `[V]`:
  "Pro-Q automatically compensates for increase or loss of gain after
  EQing" — and explicitly **not measured**: it "is _not_ a dynamic process
  based on actually measured levels" but "an educated guess based on the
  current EQ settings."
- **Pro-L 2 Unity Gain**
  ([fabfilter.com](https://www.fabfilter.com/help/pro-l/using/outputoptions)) `[V]`:
  "Automatically sets the Output Level to the inverse of the current Gain, so
  you can listen to the effect of limiting in relation to the input signal."
  Pure arithmetic. The UI signals the temporary state, and **metering still
  reflects target loudness even though actual output is adjusted** — a
  deliberate split between what you hear and what you measure.
- **TDR Nova equal loudness** `[V]`: implemented as a *readout*, not an
  override — "A circular meter indicates the required knob position to attain
  an equal perceived loudness for both the input and output of NOVA."

`[J]` **None do measured adaptive loudness matching in the audio path.**
Strong precedent for starting simple; §4.8's LUFS-I approach can be phase
two.

### 5.4 Delta monitoring, and a naming trap

**Delta = input − output.** FabFilter Pro-L 2 "Audition Limiting"
([fabfilter.com](https://www.fabfilter.com/help/pro-l/using/outputoptions)) `[V]`,
the cleanest definition found: "**Subtracts the processed output from the
input audio to audition the 'delta' signal: the actual gain reduction that
is being applied.**"

**TDR Nova "GR DELTA"** `[V]`: "allows to 'solo' the changes all dynamics
processors are currently providing" — scoped to *dynamics only*, greyed out
when no dynamics are active.

⚠ **Naming trap:** FabFilter **Pro-MB's "Audition" is NOT a delta**
([fabfilter.com](https://www.fabfilter.com/help/pro-mb/using/expertbandcontrols)) `[V]`:
"The **Audition** button lets you listen to the filtered and stereo-linked
signal that will be used to trigger dynamics processing for this band." It
auditions the *sidechain* path. `[J]` Do not use the word "Audition" for a
delta.

**Hold-to-audition dominates.** Pro-MB: "click-and-hold the button to
temporarily audition the trigger signal"; solo/mute: "Hold down the solo or
mute button to solo or mute a band only temporarily, as long as the mouse
button is pressed" `[V]`. REAPER's `Alt`-click lane momentary solo (§2.6) is
the same idiom. `[J]` Momentary, not latching, is the dominant convention —
and the fastest possible A/B gesture.

### 5.5 History-browser UX

**Granularity.** Raw ops are far too fine; whole sessions too coarse. Time-based
coalescing is the shipped convention — Yjs `captureTimeout` 500 ms `[V]`,
Tracktion a ~350 ms transaction timer gated on no mouse button being held
`[V]` — but `[J]` **gesture-boundary coalescing beats timeout coalescing when
the UI knows when the mouse went down and up**, which it does. Keep the
timeout only as the fallback for actors that cannot bracket (an agent
hammering `set_param` in a loop), and **key the merge on `(op_kind,
target_id, actor)`**, never on op kind alone — Zrythm 2's
`ChangeParameterValueCommand` uses a single constant `id()` for all
parameters and does not compare targets, so dragging fader A then fader B
within its 1 s window merges B into A `[V, source]`. That is the bug to not
copy.

**Krita ships the tiering primitive** `[V]`: `KisCumulativeUndoData` with
`excludeFromMerge = 10` — *the most recent 10 commands are never merged*,
only older history gets coalesced (plus `mergeTimeout 5000ms`,
`maxGroupSeparation 1000ms`, `maxGroupDuration 5000ms`; default off). `[J]`
Adopt `excludeFromMerge` directly.

**VS Code's coalescing** `[V]`: consecutive typing appends into one element
labelled "Typing", and — the detail worth stealing — `close()` **serialises
the element to a compact binary form** the moment it stops being appendable.
An in-memory compaction step triggered exactly when a node becomes
immutable. Barriers are explicit (`pushStackElement`/`popStackElement`).

**Naming pins against coalescing.** Google Docs
([support.google.com](https://support.google.com/docs/answer/190843)) `[V]`:
named versions exist to "make sure your versions aren't merged"; **40 named
versions per document**, 15 per spreadsheet; an "Only show named versions"
filter; grouped versions expandable ("Google Docs groups versions into
arbitrary blocks of time, but you can click the down arrow beside any
version… down to the minute" — [Zapier](https://zapier.com/blog/google-docs-revision-history/)).
`[J]` Auto-versions are coalesced by default and **naming is the pin**. A
clean two-tier retention policy: coalesce aggressively; naming exempts.

Also from Docs `[V]`: per-author colour coding, toggled by "Show changes";
and for Sheets, **unmodified rows are hidden by default** with a "Show
unmodified rows" control. `[J]` Strong precedent for timeline previews:
suppress unchanged material so the diff draws the eye.

⚠ `[U]` — the claim that Docs' "Restore this version" saves a copy of the
current state first is **not confirmed** by Google's help page or any
fetchable secondary source. **Cite Figma instead** (§1.3), whose equivalent
behaviour *is* documented.

**Eviction warning — adopt Pro Tools'.** Reference Guide `[V]`:

> "When the oldest operation is one operation away from being pushed out of
> the queue, **it is shown in red**."

`[J]` Pre-emptive, in-place, non-modal, zero chrome. Better than any
shaded-region scheme. Emacs is the only other product that warns at all,
and only for the catastrophic per-command case (`undo-outer-limit` →
"discards the info and displays a warning") `[V]`.

**Emacs' three-tier budget** `[V]` is a cleaner model than two tiers:
`undo-limit` **160,000 bytes** (soft — the group that exceeds it is the
*last one kept*), `undo-strong-limit` **240,000 bytes** (hard — the offending
group is *discarded itself*, with everything older), and `undo-outer-limit`
(per-**single-command**, the only one that warns, with an optional
`undo-ask-before-discard`). `[J]` A per-op outer limit with a warning is the
right defence against a single bulk operation blowing the budget.

**Displaying the DAG.** Do not draw it. REAPER's `(*2)` annotation on a
linear list (§2.6) is the cheap, legible answer. Supporting evidence that
the graph metaphor is contested:

- [pvigier](https://pvigier.github.io/2019/05/06/commit-graph-drawing-algorithms.html) `[V]`
  surveys Git Cola, Git Extensions, gitk, GitKraken, SmartGit and SourceTree
  and concludes "there is no standard way to draw the commit graph"; prefers
  **straight** lanes; contains **no** discussion of colour. Also the
  ordering lesson: **temporal topological sort** ("from newest committer date
  to oldest" while guaranteeing valid topological order) — naive date sort
  fails because rebase produces commits with older author dates as children
  of newer ones.
- [Tonsky](https://tonsky.me/blog/reinventing-git-interface/) `[V]`, 2014:
  branches occupy **vertical columns without bending**; **colour commits by
  author, not by branch** — "you usually either look for commits by specific
  teammate"; and merge commits get "a different, much subtler look, because
  they are not an effort per se, but a place where two other efforts join."
  `[J]` That third point is the sharpest transferable idea:
  **de-emphasise structural nodes, emphasise nodes representing human work.**
- [opensource.com](https://opensource.com/article/22/11/git-concepts) `[V]`
  goes further: "**A common mental model most people have about what
  branches even are adds to the confusion**… **thinking of Git as a series
  of numbered dots on a graph can muddy the waters**." The standard
  swim-lane picture is argued to *cause* misunderstanding.
- Dissent, for balance — [apenwarr](https://apenwarr.ca/log/20090310) `[V]`:
  "I don't know anyone who has been confused by branching and merging in
  Git… But the DAG concept, mind blowingly confusing? No." He locates the
  confusion in the CLI.

**GitLens' minimap** ([help.gitkraken.com](https://help.gitkraken.com/gitlens/gl-commit-graph/)) `[V]`
is directly reusable for a long timeline: "a high-level overview of
repository activity" with **green lines: HEAD** and **yellow lines: search
results**, letting you "quickly jump to points of interest." Also a
**Changes** column showing added lines green / deleted red, and a **Compact
Graph Column Layout** where "columns that become too narrow automatically
switch to icons to preserve information."

**Undo depth in git GUIs is honest about its limits.** GitKraken
([help.gitkraken.com](https://help.gitkraken.com/gitkraken-desktop/undo-and-redo/)) `[V]`:
"**Undo scope: The most recent supported action only**"; "**Redo scope: Only
actions that were just undone**"; and the guidance "Click Undo to revert
local actions before they are pushed" — for pushed history, "create a revert
commit." `[J]` Depth-1 undo is a notable retreat from desktop norms, and the
honest framing is *local, pre-publication* undo.

**Sublime Merge is the most architecturally honest**
([sublimemerge.com](https://www.sublimemerge.com/docs/getting_started)) `[V]`:
undo is **reflog-based** — it exposes git's durable log of ref movements as
a navigable undo/redo axis rather than inventing a private stack — and
crucially, "**Any changes undone will be shown in the staged files
section**": reverted content is *materialised*, not discarded. `[J]` Both
ideas transfer. Its known gaps are predictable: "Ability to undo 'discard'"
([#1364](https://github.com/sublimehq/sublime_merge/issues/1364) — "There is
no way to undo that action") — reflog covers *ref movements*, not
*working-tree destruction*, which was never recorded.

**Cross-cutting patterns** `[J]`:

1. **History only grows.** Figma restore *adds two checkpoints* rather than
   truncating. Restore is an edit, not a rollback.
2. **Two-tier lists beat flat logs.** Machine-generated entries collapse into
   groups; human-named entries are the landmarks.
3. **Undo and history are separate systems.** Figma: client-side buffers vs
   server-side checkpoints. Git GUIs: undo is local and pre-publication.
   Conflating them produces the "undo my teammate's work" bug class.
4. **Suppress the unchanged.** Sheets hides unmodified rows; GitLens
   collapses narrow columns; GitHub caps at 100 branches. All trade
   completeness for legibility, deliberately.

---

## 6. Data model, invariants, and the minimal v1

### 6.1 Data-model sketch

```rust
// ---------- identity ----------
#[derive(Copy, Clone, PartialEq, Eq, Hash)] pub struct ObjectId(Uuid);   // instance
#[derive(Copy, Clone, PartialEq, Eq, Hash)] pub struct LineageId(Uuid);  // same thing over time
pub struct TrackId(ObjectId); pub struct ClipId(ObjectId);
pub struct TakeId(ObjectId);  pub struct PluginInstanceId(ObjectId);
#[derive(Copy, Clone, PartialOrd, Ord, PartialEq, Eq)] pub struct RevId(u64);
pub struct RunId(Uuid); pub struct SessionId(Uuid);

// ---------- time ----------
#[derive(Copy, Clone, PartialOrd, Ord, PartialEq, Eq)] pub struct Ticks(u64);
#[derive(Copy, Clone, PartialOrd, Ord, PartialEq, Eq)] pub struct Samples(u64);
// INVARIANT: conversion only via TempoMap. Deliberately no From/Into between them.

// ---------- history ----------
pub struct CommittedBatch {
    pub rev: RevId,
    pub parent: RevId,            // linear: parent == rev - 1, always. No tree in the UI.
    pub ops: Vec<Op>,
    pub inverse: Vec<Op>,         // materialized at commit -> revert planning is O(1)
    pub actor: Actor,             // NON-optional, stamped at the ControlPlane seam
    pub run: Option<RunId>,       // agentic runs group batches
    pub origin: Option<ClientId>, // echo suppression ONLY - never attribution
    pub label: String,            // <= 128 chars, human gesture label
    pub at: SystemTime,
    pub roots: RootSet,           // chunk/source/blob refs this rev keeps alive (GC roots)
}

pub enum Actor {
    Human  { session: SessionId },
    Agent  { agent: String, run: RunId },
    System { reason: &'static str },
}

pub struct RootSet { pub chunks: Vec<ChunkRef>, pub sources: Vec<SourceId>, pub blobs: Vec<BlobRef> }

/// A materialized past state. Never held for a whole session: reconstructed on
/// demand by replaying inverses back from HEAD, then LRU-cached.
pub struct SessionState {
    pub rev: RevId,
    pub tracks: Vec<TrackState>,
    pub takes: Vec<Take>,
    pub audio_clips: Vec<AudioClip>,
    pub midi_clips: Vec<MidiClip>,     // notes by ChunkRef, never inline
    pub lanes: Vec<AutomationLane>,
    pub plugins: Vec<PluginInstance>,  // params + BlobRef, never the blob
    pub tempo: TempoMap,
    pub meter: MeterMap,               // degenerate 4/4 in v1, but present
}

// ---------- takes: the extracted unit ----------
pub struct Take {
    pub id: TakeId,
    pub lineage: LineageId,            // shared with the take it came from
    pub track: TrackId,                // a take belongs to exactly one track
    pub name: String,                  // from the diff: "before 'rewrote bars 9-16'"
    pub provenance: Provenance,
    pub content: TakeContent,
    pub manifest: CarryManifest,
    pub loudness_lufs: Option<f32>,    // level-matched A/B
    pub group: Option<TakeGroupId>,    // switches with peers (cf. Cubase common version ID)
}

pub enum Provenance {
    Recorded      { at: SystemTime },
    ExtractedFrom { rev: RevId, scope: Scope, by: Actor },
    Generated     { run: RunId, job: JobId },
}

pub struct TakeContent {
    pub audio_clips: Vec<AudioClip>,          // clip.take == this take (replaces track_id)
    pub midi_clips:  Vec<MidiClip>,
    pub lanes:       Vec<AutomationLane>,     // instrument automation only
    pub instrument:  Option<InstrumentBinding>, // Some only when scope included it
}

pub struct TrackState {
    pub id: TrackId, pub name: String, pub kind: TrackKind,
    pub mix: MixState,          // gain/pan/mute/solo/arm - NEVER part of a take
    pub takes: Vec<TakeId>,
    pub active_take: TakeId,    // in the Store => persisted, op-logged, offline-render-correct
    pub slot: SlotIndex,        // dense 0..MAX_TRACKS, one per TRACK not per take
}

// ---------- extraction ----------
pub enum Scope {
    TrackContent      { track: TrackId },                    // DEFAULT
    TrackContentRange { track: TrackId, span: Range<Ticks> },
    TrackFull         { track: TrackId },                    // + instrument => new track
    TouchedByOps      { from: RevId, to: RevId },
    Explicit          { objects: Vec<ObjectId> },
}

/// Computed, shown, THEN applied. Never implicit.
pub struct ExtractionPlan {
    pub from_rev: RevId,
    pub scope: Scope,
    pub alternatives: Vec<Scope>,   // ranked from the semantic diff
    pub remap: Remap,               // in-scope objects only
    pub manifest: CarryManifest,
    pub warnings: Vec<Warning>,     // tempo changed in span, plugin state rejected, ...
    pub refusals: Vec<Refusal>,     // non-empty => cannot apply
    pub cost: Cost,                 // added RAM, plugin instantiations, disk
}

pub struct Remap(HashMap<ObjectId, ObjectId>);

pub enum Carry {
    Shared     { kind: ResKind, id: ObjectId },
    Copied     { kind: ResKind, from: ObjectId, to: ObjectId, bytes: u64 },
    Dropped    { kind: ResKind, id: ObjectId, why: DropReason },
    Unresolved { kind: ResKind, was: ObjectId, name: String }, // silent + badged
}
pub struct CarryManifest(Vec<Carry>);

// ---------- the ops ----------
pub enum Op {
    // ... existing kinds ...
    TakeExtract   { plan: ExtractionPlanId, into: TakeId },
    TakeSetActive { track: TrackId, take: TakeId, when: SwitchWhen },
    TakeDelete    { take: TakeId },
}
pub enum SwitchWhen { Instant, NextBar, LoopWrap }

// ---------- diff ----------
pub struct Diff { pub from: RevId, pub to: RevId, pub entries: Vec<DiffEntry> } // top level <= 7
pub struct DiffEntry {
    pub fact: Fact,
    pub track: Option<TrackId>,
    pub span: Option<Range<Ticks>>,    // ticks, NOT bar numbers
    pub significance: f32,             // audible consequence, not element count
    pub actor: Actor,
    pub children: Vec<DiffEntry>,      // disclosure
    pub audible: Option<AuditionRange>,// offline::render both sides
}
```

### 6.2 The three invariants that must never be violated

> **I1 — One live identity.** At any instant an `ObjectId` names at most one
> renderable object. Extraction always mints fresh `ObjectId`s and preserves
> `LineageId`.
>
> *Violated, AURA breaks in three silent ways:* `Store.slots` loses a track's
> RT slot (inaudible, no error); the clip-id-keyed decode cache serves the
> wrong audio; undo's inverse ops resolve non-deterministically. All silent —
> the worst failure class in a DAW.

> **I2 — Reference closure, no silent substitution.** Every reference in every
> produced object resolves to a live object or to an explicit `Unresolved`
> placeholder that is visible in the UI and silent in the engine. No dangling
> ids, and no implicit fallbacks: deleted bus never becomes master, missing
> plugin never becomes bypass, missing file never becomes a skipped clip.
>
> *Violated,* the user compares two things that differ for a reason the DAW
> knew and didn't say — and then mixes against that difference.

> **I3 — History is append-only; the past is read-only.** Extraction, revert
> and take switching all append new batches. Nothing rewrites or removes a
> committed batch, and no edit can be made *at* a past revision. Retained
> revisions are GC roots for their chunks.
>
> *Violated,* you have a tree in the UI — which forces the version-control
> mental model on the user (§4.9), makes revert ambiguous, and makes the chunk
> GC unsound (AURA's `save_into_project` already deletes unreferenced AMEV
> chunks on every save).

### 6.3 Minimal v1

Since there is no op log at all today, v1 is *the smallest history layer that
can support extraction* plus *the smallest extraction*.

**Ships:**

1. **The op log, for real** (pays D-03): `ops_apply` / `ops_subscribe` per
   the existing draft envelope, plus `rev`, `parent`, materialised `inverse`,
   non-optional `actor`, and `run`. Existing mutations become single-op
   batches through `control::ops`, which is already the designated home and
   already batch-atomic. **Do this even if extraction slips** — it is the
   load-bearing half.
2. **Content-only takes on MIDI tracks** — `Scope::TrackContent` only. MIDI
   first because notes already live in ticks inside immutable AMEV chunks:
   extraction is "keep a chunk ref", the §4.4 tempo question collapses to a
   no-op, and there is no plugin-instantiation cost.
3. **Schema v3: `Clip.track_id` → `clip.take: TakeId`**, and `TrackState {
   takes, active_take }`. The one migration that must land up front;
   retrofitting it later touches everything.
4. **`ObjectId` + `LineageId` on every object.** Cheap now; impossible to add
   retroactively, because lineage cannot be inferred after the fact.
5. **A/B switch**: `TakeSetActive` as a `Store` mutation + one `Rebuild`,
   `SwitchWhen::Instant` only, with the releasing-node tail handling and a
   5 ms declick. One keyboard toggle.
6. **Level-matched A/B** via `offline::render` + LUFS-I, displayed. Small,
   and without it the feature actively misleads.
7. **Enough diff to name takes**: state diff over tracks/clips/notes with the
   chunk-hash shortcut, dominant-transform detection (transpose, quantize),
   ≤7 entries, bar labels rendered through the right map. No plugin or blob
   diffing.
8. **Refusals 1, 2, 7, 10, 11** (object absent, nothing changed, no slot,
   beyond horizon, recording) — cheap, and each otherwise produces silent
   nonsense.
9. **Chunk GC root set extended** to retained revisions and takes.
   Non-negotiable: without it, `midi::persist::save_into_project` deletes take
   payloads on the next save and the bug looks like data loss on reopen.
10. **Source-id asset naming** (`audio/<sourceId>.wav`, decode cache keyed by
    source) — prerequisite for I1, and currently wrong.

**Defers, and why it's safe:**

- `Scope::TrackFull`, plugin copying, plugin-state A/B — needs the cost model
  and budget refusals. `Scope` is an enum; variants are additive.
- Audio-clip takes and tempo re-anchoring — additive once
  `TakeContent.audio_clips` exists.
- `TakeGroupId` / multi-track switching — field present from day one, unused.
- `SwitchWhen::NextBar | LoopWrap` — enum present; `loopjam.rs` already
  proves the loop-wrap seam.
- Buses and the deleted-bus problem — AURA has none. But I2 must be *written
  down now* so the eventual bus code cannot take the "reroute to master"
  shortcut.
- Meter map — `MeterMap` in the type, degenerate 4/4 implementation, diff
  spans stored in ticks so stored labels don't rot when it lands.
- Agent-run review UI, capability-scoped MCP grants, selective revert — but
  `actor` and `run` must be in the log from batch #1. **This is the single
  most important thing not to defer**, because attribution cannot be
  reconstructed after the fact.
- Cross-session persisted history — v1 can keep the log in-session plus the
  journal for crash recovery; refusal 10's horizon message makes the boundary
  honest rather than hidden.

**The one thing v1 must not do:** let history become a tree *in the UI*. A
linear log plus takes is a complete product, and linearity is what keeps the
version-control mental model away from the user (§4.9). The branch DAG may
exist underneath (§1.4 — it is nearly free), surfaced with REAPER's `(*2)`
annotation at most. Adding user-facing branching later is a product decision.
Removing it later is not possible.

---

## 7. What this means for AURA

### 7.1 The strategic position

The market gap is real, verified across nine products against primary
documentation, and it is not one feature but the assembly of four (§Why this
document exists). **REAPER is one UI away** from the closest approximation
and has been for years without closing it. Bitwig has the only glitch-free
switch and applies it only to device chains. Nobody has a diff.

The pitch this survey supports, in one sentence: *every DAW lets you keep
alternatives; none of them lets you compare them.* Comparison — same
position, same instant, no click, with automation carried along, and a diff
telling you what to listen for — is unclaimed ground.

### 7.2 The one design decision that unlocks the biggest gap

**Model automation as timeline content, not as a property of the channel.**
Put automation curves in the same container as audio and MIDI events, bound
to a track over a time range, and takes carry their automation *for free*.
Every competitor has a decade-old feature request they cannot cheaply satisfy
because they made the opposite choice before the feature existed (§3).

### 7.3 Two bugs this work found in our code

**1. `audio/<clipId>.wav` couples asset naming to instance identity.** We
name recorded and imported WAVs by clip id, and the decoded-sample cache in
`ensure_loaded` is **keyed by clip id**. The moment two clip instances share
a source — which is exactly what an extracted take does — this either orphans
the audio or forces a copy, and **the clip-id-keyed cache serves the wrong
audio with no diagnostic.** That is an I1 violation with a silent failure
mode.

*Fix:* rename to a content id (`audio/<sourceId>.wav`), have
`Clip.source_path` reference it, and key the decode cache by source id.
Prerequisite work, not optional.

**2. AMEV chunk GC would delete take payloads on the next save.**
`midi::persist::save_into_project` garbage-collects unreferenced event
chunks, and the root set is the current in-memory MIDI store. Takes and
retained revisions are not in it. So extraction would work perfectly until
the next save, after which the notes are gone — and the bug presents as data
loss on reopen, far from its cause.

*Fix:* extend the GC root set to `{current project} ∪ {all takes} ∪ {all
retained revisions}`. Note also Ableton's documented reachability bug as the
warning about scoping this too narrowly: Live "considers any file in the
Project which is not referenced by its Sets, clips, or presets as unused —
**even if the file is actively used in other Projects**" `[V]`.

### 7.4 The seams we already have, and what they buy

| Existing seam | What it buys this feature |
|---|---|
| `control::ops` as the single mutation implementation, with both front doors through `ControlPlane` (ARCHITECTURE §11) | `Actor` attribution becomes a property of the seam — unforgeable by a caller, unforgettable by a new command (§4.7) |
| `op-envelope.schema.json` (DRAFT) with `rev`, `baseRev`, `origin`, `label`, `transient` | The op log's wire format is already specified; §6.3 item 1 is implementing a design, not inventing one |
| RCU snapshot swap with control-thread deallocation (§2.3) | The A/B switch is a graph swap on a proven path, with no new RT contract |
| `LiveNodeCell` — node state survives snapshot swaps | The releasing-node tail handling in §4.6 has a home |
| `set_block_context(base_pos, discontinuity)` — sample-accurate ramps across seeks and loop wraps | Automation stays position-true through a take switch |
| `audio/offline.rs` — deterministic headless render of an arbitrary `(Store, MidiStore, SamplerBank)` | The audible diff (§4.2), LUFS-I level matching (§4.8) and "bounce from here" (§4.4) all already have their engine |
| `control/loopjam.rs` — prepare content off-thread → `Store` mutation → one `Rebuild`, swapped at the loop wrap | The proven A/B landing pattern, including `SwitchWhen::LoopWrap` |
| AMEV `columnMask` — "old readers skip unknown columns, never break" | Adding a note id costs one bit now (§7.5) |
| ARCHITECTURE §2.6 — "the callback reports, the control plane decides" | Better factored than Ardour's or Zrythm's equivalents; §4.6 extends it rather than fighting it |

### 7.5 The costs that are near-zero now and unpayable later

Ordered by irreversibility:

1. **`ObjectId` + `LineageId` on every object.** Lineage cannot be inferred
   after the fact. Free now.
2. **A note id in the AMEV record.** One `columnMask` bit today; a migration
   of every event chunk ever written after projects exist. (Also required by
   per-voice modulation and MPE, independently.)
3. **`actor` and `run` on every committed batch.** Attribution cannot be
   reconstructed retroactively.
4. **`Clip.take: TakeId` replacing `track_id`.** The container move that
   eliminates most remapping (§4.3).
5. **Diff spans stored in ticks, with a `MeterMap` field present but
   degenerate.** Otherwise every stored bar label silently rots when the
   meter map lands.
6. **The GC root set including takes and retained revisions** (§7.3).

### 7.6 What we should not build

- **Selective undo.** Requires commutation analysis; in a DAW most
  interesting op pairs do not commute (§1.4). "Undo the agent's changes" is a
  new forward batch carrying the inverse.
- **Merge.** Perforce dominates game art by *preventing* concurrent edits;
  Anchorpoint leads with history and locking and omits branching for artists;
  even Figma, with a structured op-based model, cannot cherry-pick (§1.5,
  §1.3). Building merge for a DAW project is a multi-year investment in a
  feature the market has repeatedly rejected.
- **A user-facing branch tree.** Store the DAG (it is nearly free); present a
  timeline with event labels and REAPER's `(*2)` annotation (§2.6, §5.5).
- **An untyped mixer-state bag.** Every field declared, versioned, and
  round-trip tested, or we reproduce the Cubase MixConsole Snapshot bug list
  line by line (§2.9).
- **Variants as mute state.** Cubase's wrong-version-on-export bug is what
  that costs (§2.3).

### 7.7 Open questions for the design phase

1. **Where do takes live in the project format?** A `takes[]` top-level array
   is a fifth writer against `project.json`, and the save-overlay rule in
   `audio/project.rs` only preserves unknown keys when the file is already
   `schemaVersion >= 2`. Must go through `update_project_v2` or be dropped by
   the next typed v1-path save.
2. **Storage strategy for retained revisions.** Structurally-shared
   materialised snapshots vs op-log replay from the nearest ancestor is a
   measured trade-off; a separate research thread produced numbers and
   falsifying benchmarks that belong in the storage design, not here.
3. **The history horizon in v1.** In-session only, or persisted from the
   start? Refusal 10 makes either honest, but the choice affects the journal
   format.
4. **Does the diff run on demand or incrementally?** On-demand is simpler;
   the ≤50 ms hover budget for a change summary may force a cached summary
   per batch.

---

## Sources

**Photoshop / history as an object** — [UXP Document class](https://developer.adobe.com/photoshop/uxp/2022/ps_reference/classes/document/) · [UXP HistoryState](https://developer.adobe.com/photoshop/uxp/2022/ps_reference/classes/historystate/) · [ExtendScript HistoryState mirror](https://theiviaxx.github.io/photoshop-docs/Photoshop/HistoryState.html)

**Blender** — [BKE_undo_system.hh](https://raw.githubusercontent.com/blender/blender/main/source/blender/blenkernel/BKE_undo_system.hh) · [undo_system.cc](https://raw.githubusercontent.com/blender/blender/main/source/blender/blenkernel/intern/undo_system.cc) · [undofile.cc](https://raw.githubusercontent.com/blender/blender/main/source/blender/blenloader/intern/undofile.cc) · [memfile_undo.cc](https://raw.githubusercontent.com/blender/blender/main/source/blender/editors/undo/memfile_undo.cc) · [#60695](https://projects.blender.org/blender/blender/issues/60695) · [#56163](https://projects.blender.org/blender/blender/issues/56163)

**Figma** — [Version history](https://help.figma.com/hc/en-us/articles/360038006754) · [Branches and merging](https://help.figma.com/hc/en-us/articles/360063144053) · [How Figma's multiplayer technology works](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/)

**Tree undo / history UI** — [Vim undo.txt](https://vimhelp.org/undo.txt.html) · [undo-tree](https://elpa.gnu.org/packages/undo-tree.html) · [vundo](https://github.com/casouri/vundo) · [IntelliJ Local History](https://www.jetbrains.com/help/idea/local-history.html) · [Google Docs version history](https://support.google.com/docs/answer/190843) · [Zapier on Docs revision history](https://zapier.com/blog/google-docs-revision-history/) · [pvigier: commit graph drawing](https://pvigier.github.io/2019/05/06/commit-graph-drawing-algorithms.html) · [Tonsky: reinventing git interface](https://tonsky.me/blog/reinventing-git-interface/) · [opensource.com: git concepts](https://opensource.com/article/22/11/git-concepts) · [apenwarr](https://apenwarr.ca/log/20090310) · [GitKraken undo/redo](https://help.gitkraken.com/gitkraken-desktop/undo-and-redo/) · [GitLens Commit Graph](https://help.gitkraken.com/gitlens/gl-commit-graph/) · [Sublime Merge](https://www.sublimemerge.com/docs/getting_started) · [Tower undo](https://www.git-tower.com/features/undo)

**Creative VCS** — [Perforce game dev](https://www.perforce.com/solutions/game-development) · [Git LFS](https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-git-large-file-storage) · [bup DESIGN](https://github.com/bup/bup/blob/master/DESIGN) · [Anchorpoint blog](https://www.anchorpoint.app/blog) · [Diversion](https://www.diversion.dev/)

**CRDT / time travel** — [Automerge Rust API](https://docs.rs/automerge/latest/automerge/struct.Automerge.html) · [Automerge 2.0](https://automerge.org/blog/automerge-2/) · [Yjs README](https://raw.githubusercontent.com/yjs/yjs/main/README.md) · [Yjs UndoManager](https://docs.yjs.dev/api/undo-manager) · [Ink & Switch: Local-first software](https://www.inkandswitch.com/essay/local-first/) · [Upwelling](https://www.inkandswitch.com/upwelling/)

**Notebooks** — [nbdime](https://nbdime.readthedocs.io/en/latest/) · [marimo FAQ](https://docs.marimo.io/faq/) · [Verdant](https://github.com/mkery/Verdant)

**DAW manuals** — [Ardour: Understanding Playlists](https://manual.ardour.org/working-with-playlists/understanding-playlists/) · [Ardour: Playlist Operations](https://manual.ardour.org/working-with-playlists/playlist-operations/) · [Ardour: Playlist Use Cases](https://manual.ardour.org/working-with-playlists/playlist_usecases/) · [Ardour playlist.cc](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/playlist.cc) · [Pro Tools Reference Guide 2024.10](https://resources.avid.com/SupportFiles/PT/Pro_Tools_Reference_Guide_2024.10.pdf) · [Cubase Track Versions (v12)](https://archive.steinberg.help/cubase_pro/v12/en/cubase_nuendo/topics/track_handling/track_handling_trackversions_c.html) · [Cubase Pro 15 Track Versions](https://www.steinberg.help/r/cubase-pro/15.0/en/cubase_nuendo/topics/track_handling/track_handling_trackversions_c.html) · [Logic: Track Alternatives](https://support.apple.com/guide/logicpro/use-track-alternatives-lgcp002c4e63/mac) · [Logic: Project Alternatives](https://support.apple.com/guide/logicpro/use-project-alternatives-and-backups-lgcpa158ef77/mac) · [Logic Pro User Guide PDF](https://help.apple.com/pdf/logicpromac/en_US/logic-pro-mac-user-guide.pdf) · [REAPER User Guide 7.78](https://www.reaper.fm/userguide/ReaperUserGuide778.pdf) · [REAPER changelog](https://www.reaper.fm/whatsnew.txt) · [SWS Snapshots](https://www.sws-extension.org/snapshots.php) · [Ableton: Session View](https://www.ableton.com/en/manual/session-view/) · [Ableton: Launching Clips](https://www.ableton.com/en/manual/launching-clips/) · [Ableton: Recording New Clips](https://www.ableton.com/en/manual/recording-new-clips/) · [Ableton: Arrangement View](https://www.ableton.com/en/manual/arrangement-view/) · [Bitwig Studio User Guide](https://www.bitwig.com/media/bitwig_userguide/pdf/Bitwig_Studio_User_Guide_English_XfuP7Nz.pdf)

**Secondary / forums** — [SOS: Studio One Scratch Pad](https://www.soundonsound.com/techniques/studio-one-using-scratch-pad) · [SOS: Logic Track Alternatives](https://www.soundonsound.com/techniques/logic-pro-track-alternatives) · [Ardour: automation & playlist copies](https://discourse.ardour.org/t/105868) · [Steinberg: Track Version & automation](https://forums.steinberg.net/t/does-track-version-not-include-the-track-automation/665458) · [Steinberg: Track Versions for Automation Tracks](https://forums.steinberg.net/t/130915/4) · [Steinberg: export wrong Track Version](https://forums.steinberg.net/t/863494/3) · [Steinberg: MixConsole snapshot threads](https://forums.steinberg.net/t/133370/4) · [r/StudioOne: switching scratch pads](https://old.reddit.com/r/StudioOne/comments/1tyyn9j/switching_between_scratch_pads/) · [r/StudioOne: does anyone use scratch pad](https://old.reddit.com/r/StudioOne/comments/17v9r30/does_anyone_actually_use_the_scratch_pad/) · [r/LogicPro: alternate arrangements](https://old.reddit.com/r/LogicPro/comments/1ul0dph/best_workflow_for_trying_alternate_song/)

**A/B methodology** — [FabFilter Pro-Q undo/redo/AB](https://www.fabfilter.com/help/pro-q/using/undoredo) · [FabFilter Pro-Q output](https://www.fabfilter.com/help/pro-q/using/output) · [FabFilter Pro-L output options](https://www.fabfilter.com/help/pro-l/using/outputoptions) · [FabFilter Pro-MB expert band controls](https://www.fabfilter.com/help/pro-mb/using/expertbandcontrols) · [TDR Nova manual](https://docs.tokyodawn.net/nova-manual/) · [foobar2000 ABX](https://www.foobar2000.org/components/view/foo_abx) · [ABX test](https://en.wikipedia.org/wiki/ABX_test) · [Echoic memory](https://en.wikipedia.org/wiki/Echoic_memory) · [MUSHRA](https://en.wikipedia.org/wiki/MUSHRA) · [ITU-R BS.1534](https://www.itu.int/rec/R-REC-BS.1534/en) · [ITU-R BS.1116](https://www.itu.int/rec/R-REC-BS.1116/en) · [Loudness war](https://en.wikipedia.org/wiki/Loudness_war)

**Academic** — Berlage 1994, ACM TOCHI 1(3):269–294, DOI 10.1145/196699.196721 · Prakash & Knister 1994, ACM TOCHI, DOI 10.1145/198425.198427 · Sun 2002, ACM TOCHI 9(4):309–361, DOI 10.1145/586081.586085 · Ressel & Gunzenhäuser, GROUP '99, DOI 10.1145/320297.320312 · Weiss/Urso/Molli 2010, IEEE TPDS 21(8), DOI 10.1109/TPDS.2009.173

**Unverified — do not cite as fact:** Photoshop's history-state integers (50/1000); Plastic SCM Gluon's partial-workspace model; Splice Studio's shutdown details; Cubase Track Version finer behaviour; iZotope/Waves/UAD A/B implementations; whether plugin A/B slots are per-preset or per-session; the ITU level-matching tolerance figure; the psychoacoustic loudness-bias claim; Google Docs' restore-copies-first behaviour; foo_abx switching/level-matching internals; Ardour playlist-switch click behaviour during playback.
