# Continuity via pq-grams — the §6.6 subdoc (adversarially hardened)

> **Status:** design, greenfield. This is the full expansion of **`docs/GLOBAL-IR-GRAPH.md` §6.6**
> ("The unified similarity surface: pq-grams"), hardened against five independent adversarial
> reviews (soundness, IR-richness, efficiency/scope, redundancy, failure-modes). It **supersedes
> the §6.6 code snippet** wherever they disagree; §6.6 in the parent doc should be reduced to a
> one-paragraph pointer here.
>
> **Scope of the change vs. the north-star doc:** §6.1–6.5 (continuity is undecidable; nominal
> `IntroId` id + best-effort *edge*; never-merge/under-merge asymmetry) are **unchanged and
> load-bearing** — everything below serves them. What changes is the *mechanism* under §6.6.

---

## 0. Executive summary — what survived review, what did not

The adversarial passes converged on one shape. Stated bluntly so the rest of the doc can lean on it:

1. **The headline soundness claim was wrong, and the fix is a role-swap, not a redesign.**
   pq-gram distance is **not** a provable lower bound on the tree-edit-distance we actually verify
   with — it is a lower bound on *fanout-weighted* TED and only at `p=1` (Augsten TODS 2010,
   Thm 7.4; SIGMOD-Record 2013 survey). It is a **pseudo-metric** (distance 0 ⇏ same tree).
   → **pq-gram distance + MinHash/LSH move to the RETRIEVAL/RANKING role (heuristic, no
   guarantee).** The **provable prune** moves to bounds that really do lower-bound the exact TED:
   `max(|n₁−n₂|, label-hist L1/2, degree-hist L1/3, binary-branch/5)` — all computable from the
   `LabelDegreeHist` the profile *already* carries (§1, §3.4).

2. **There are TWO planes, and the plan conflated them.** (a) **Intra-package** continuity already
   *exists and works*: `workspace/ir-vcs/continuity.rs::compute_sigma` matches a package's
   `tip → staged` generations, keyed on `IntroId`, with never-merge margin logic. pq-grams must
   **feed** it, not replace it. (b) **Corpus, cross-package** continuity (fork / vendored-copy /
   renamed-crate provenance) has **no implementation** — that is where corpus-scale LSH genuinely
   earns its keep, and it must emit *provenance-DAG edges, never `σ`*.

3. **The genuine new signal is the BODY.** `compute_sigma` and `api_surface_hash` read
   `OwnedEntryPayload` (the declaration) *only*; they never touch the body. Over the *declaration*
   tree, pq-grams are ~redundant with the existing `by_shape` (api-surface) + name + `sig_key`
   indexes. Their real earn is a **body region-tree axis** that bridges "declaration surface
   churned, implementation stayed the same" — which nothing today can link.

4. **The scope-gate the request proposed is unsound as stated,** because `IntroId` already encodes
   name+parent (so a rename *mints a new id* → shows up as delete+add) while `api_surface_hash`
   *excludes* name+parent (so a pure rename leaves it unchanged). Gate continuity on the
   **`IntroId`-set delta**, and keep the api-surface-hash gate for its real job (skip
   re-embed / shape-semver). Two gates, two keys.

5. **The §6.6 code dropped the safety gates the shipping matcher already has.** A raw
   `continuity(old,new,lb,thr) → {Distinct | classify}` is *weaker* than `compute_sigma`. Port the
   discrete refusals onto the metric: kind hard-gate, `MARGIN` ambiguity refusal, R-OV, R-CHILD,
   and **High-confidence-alone writes `σ`**. Add tiny-tree/min-distinct-gram floors and a
   three-band `High / Soft / Unresolved` output.

The rest of the doc is these five, made precise and buildable.

---

## 1. Soundness: what pq-grams provably give us (and what they do not)

### 1.1 The corrected statements

| Statement in the §6.6 snippet | Reality (cited) |
|---|---|
| pq-gram profile = bag of p-ancestor×q-child grams, O(n) one pass | **Holds** (Augsten Defs 1–4; the profile is a **bag**). |
| pq-gram distance is a "TRUE METRIC" | **Pseudo-metric.** Triangle-inequality + symmetry hold after normalization, but **distance 0 does not imply isomorphic trees** — and our label projection (which drops name/span/doc/cfg/locals, §3) *widens* the distance-0 class. |
| distance is a "PROVABLE LOWER BOUND on TED" ⇒ "safe to prune, NO false negatives" | **False as used.** It lower-bounds **fanout-weighted** TED and **only at `p=1`, `c ≥ max(2q−1,2)`** (Thm 7.4). §6.6 verifies with `apted(…, &IrCost)` (a *different* cost) and wants `p≥2` (ancestor context). So it bounds neither the TED we run nor at the `p` we use. |
| MinHash sketch's "Jaccard estimate *is* the bag overlap" → LSH candidates | **Conflates set-Jaccard with multiset overlap.** MinHash sketches a **set**; the distance is a **multiset** difference. Fine as a *heuristic* shortlister; it is **not** a lossless prefilter, and it can drop a true candidate *before any bound is consulted* (a retrieval-side false negative). |
| "small changes aren't outsized … unit-TED up to a constant" | **Unproven, and useless even if true.** No such multiplicative theorem exists in the literature; and a large constant κ on a lower bound (`pq/κ ≤ TED`) shrinks the usable bound toward 0, so a sound threshold prunes ~nothing. |

Sources: Augsten, Böhlen, Gamper, *The pq-Gram Distance between Ordered Labeled Trees*, ACM TODS
35(1), 2010 (Thm 7.4; pseudo-metric); *Windowed pq-grams* (VLDB J.) — "non-identical trees may be
at distance zero"; *A Survey on Tree Edit Distance Lower Bound Estimation*, SIGMOD Record 2013 —
"lower bound to the **fanout-weighted** tree edit distance, **but not to the widely used unit cost
tree edit distance**"; proven unit-TED bounds therein (label-hist `L1/2`, degree-hist `L1/3`,
binary-branch `BDist/5`, string-edit on Euler traversal).

### 1.2 The role-swap (the whole fix in one line)

> **pq-gram distance + MinHash/LSH = _retrieve & rank_ (heuristic, probabilistic recall).
> The _prune_ decision = a `max` of bounds that provably lower-bound the exact `IrCost`/unit TED
> we actually compute.**

This keeps everything the plan wanted — one cheap decomposition, exact APTED only on survivors —
on *sound* footing, and it reuses the `hist` field the profile already carries. Nothing is thrown
away; the load-bearing bound just moves from the pq-distance term to the histogram/size terms.

### 1.3 The sound prune stack (replaces `if lb > thr { Distinct }`)

For a candidate pair `(old, new)` with declaration trees of `n₁, n₂` nodes, compute — in this
order, cheapest-first, stopping as soon as one exceeds `thr`:

```
lb = max(
    size_bound      = |n₁ − n₂|,                       // exact unit-TED LB, O(1)
    label_bound     = L1(label_hist₁,  label_hist₂) / 2,   // survey Thm 3, O(n log n)
    degree_bound    = L1(degree_hist₁, degree_hist₂) / 3,  // survey Thm 2
    branch_bound    = binary_branch_dist / 5,          // survey Thm 4 (optional, tighter)
)
if lb > thr        → Distinct           // SOUND: every term LBs the TED APTED computes
else               → run APTED(old, new, IrCost); classify by exact cost
```

- **The constants `/2, /3, /5` are mandatory.** Using raw `L1` *over*-prunes → the exact false
  negatives we forbid.
- If `IrCost ≠ unit cost`, re-scale each bound by the model's minimum per-edit cost (or run APTED
  under the genuinely fanout-weighted model if you want to keep a pq-derived bound in the `max`).
  Pin `IrCost` in the format registry (§6) — changing it silently reclassifies history.
- **Optional tightest bound:** string-edit distance on the pre-/post-order Euler traversal of the
  label sequence (`ed(pre(T₁),pre(T₂)) ≤ TED_unit`, survey §4.1) is frequently the *tightest* and
  is affordable at symbol scale (n = tens–hundreds). Use it as a second filter before APTED on the
  pairs the histograms can't separate.

### 1.4 The retrieval hole, and why never-merge absorbs it

LSH is a *lossy* filter over *set* Jaccard, so a real rename can be dropped at retrieval and never
verified. Under §6.3's never-merge/under-merge bias this is the **safe** error: a dropped candidate
degrades to `delete + new` (a false *split*, re-linkable later), never a false *merge*. So the
honest posture is: **document that continuity recall is probabilistic, and stop claiming "no false
negatives."** If we want to *bound* the miss rate, additionally bucket candidates by the cheap
provable bounds (size/label-hist buckets), so a pair the bound would keep is never dropped by
MinHash luck alone. (Banding parameters → target-recall tuning is an open knob, §8.)

### 1.5 `distance == 0` is not identity

Because the distance is a pseudo-metric *and* our labels drop name/span/doc/cfg/locals, a
distance-0 (or APTED-cost-0) pair means "indistinguishable under this projection," **not** "the same
symbol." Never auto-merge on 0. This is exactly the §6.4.3 tie case and must route through the
margin refusal (§5), not a fast-path merge.

---

## 2. The two planes — where pq-grams actually plug in

The parent doc uses one word ("continuity") for two operations with different inputs, keys, and
failure costs. Separate them permanently.

| Operation | Plane | Input / key | Who does it today | pq-gram role |
|---|---|---|---|---|
| `σ`: wire-id → durable-id within one package sealing | **intra-package**, `tip` vs `staged` | one channel's `PristineIntroTable`, keyed on `IntroId` | `compute_sigma` (tested C-1…C-10) | **ADD, narrowly** — a body-axis candidate index + one scoring signal, reusing all existing gates. **Never replace.** |
| Emit `Renamed/Moved/SignatureEvolved/Deleted/Introduced` | intra-package | same | `compute_sigma → ContinuitySummary` | already done; pq-grams only widen *which pairs reach* scoring |
| "Where did this symbol come from?" across packages (fork, vendored copy, renamed crate) | **corpus**, PURL-keyed | the whole `IntroId`×PURL graph | **nobody** (aspirational §6.4/§7) | **ADD, genuinely new** — corpus-scale LSH lives here, emitting **provenance-DAG edges, never `σ`** |
| Exact same-`IntroId` continuity | both | id equality | trivial; `SignatureEvolved` already in `f1.rs` | pq-grams explicitly do **not** run here |

### 2.1 The residual on the intra-package plane (why declaration-only pq-grams don't earn their keep)

`compute_sigma` already links a rename/move when **any** of: exact `api_surface_hash` (`by_shape`,
+50), exact name (`by_kind_name`, +20), exact `(kind,parent,stem)` (+ parent-continuity +15),
exact `sig_key` (+15), or the R-OV lone-1↔1 flip (forced High even when shape+sig+doc all changed).
The set pq-grams could *newly* catch over the **declaration** is the simultaneous intersection of:

> different `IntroId` **∧** name changed **∧** stem changed **∧** `api_surface_hash` changed **∧**
> not a lone 1↔1 in its name-bucket **∧** `sig_key` changed.

That is a *rename + structural edit + name-collision at once* — real, but rare, and today it safely
degrades to delete+new. **Over the declaration tree, pq-grams are ~co-extensive with `by_shape`.**
Conclusion: **do not build declaration-plane pq-grams as "a better candidate index."** That
duplicates three existing exact indexes.

### 2.2 The one place it *does* earn its keep: the body axis

> **⚠ Blocked today — see §9.** This is the design earn, **not a current capability**: no producer
> emits a body, bodies aren't in the matcher's inputs, and the substrate here should be the
> *normalized CST* (§9.4), not the flat/contested `ControlSketch`. Read §9.1/§9.6 before building.

`score_pair` / `compute_api_surface_hash` are blind to the body. `TreesitterBody` / `BodyFacts`
exist in `workspace/ir/body.rs` but the matcher never reads them. So a symbol whose **declaration
surface churns but whose implementation is stable** (return type widened, a param added, visibility
changed — same 200-node body) has **no structural bridge today**. A pq-gram profile over the body
region-tree is the *only* proposed signal that links those. **This — not "faster candidate gen" —
is the headline earn, and the plan buried it.** It is also strictly gated (§3.3) so a missing or
low-fidelity body can never *cause* a split.

---

## 3. Constructing the profile — full use of the IR's richness

The §6.6 label `(kind_disc, type-skeleton token)` collapses ~30 semantically distinct `KEY_*`
frames into two and — worse — hashes the type-skeleton (itself a tree) into an atomic token. Both
are fixed here.

### 3.1 The governing law (alpha-normalization)

A frame goes **in the node label** iff it is (a) *edit-stable* — invariant under the edits we want
to see *through* (symbol rename, dependency rename, module move, doc/format/cfg churn) — **and**
(b) *discriminative* — its change is a real semantic change we want distance to reflect. A frame
that is a **set/sequence of sub-entities** becomes **child nodes**, never label bytes (pq-grams
already capture child cardinality + ordering; folding them into the parent double-counts and
destroys the fanout signal).

### 3.2 Per-kind label schema (declaration tree)

`Label = (kind_disc: u8, shape_class: u8, prim_payload: varbytes)`, `prim_payload` drawn only from
scalar, edit-stable, kind-defining frames. Collections become children.

| Kind | Label | In label | Children (each own label) | Excluded (why) |
|---|---|---|---|---|
| **Function** | `(3, fnsig_shape)` — `fnsig_shape` = `fnsig_flag_bytes` minus ABI string & arbitrary-receiver skeleton (just `self_kind, async, const, unsafe, variadic, defaulted`) | `KEY_FNSIG` flag bits | `KEY_IN`/`KEY_OUT` params (label = type-skeleton **subtree**), `KEY_GPARAM`, `KEY_WHERE` | name, span, src, doc, dlink, cfg, attr, **vis** (§3.5), ABI string, lifetimes, param names |
| **Record** | `(2, recform)` | `KEY_RECFORM` | Field entries (via `KEY_PARENT`), `KEY_GPARAM`, `KEY_WHERE`, `KEY_AUTO` | name/span/doc/cfg/attr/vis; **raw RECFIELD hex** (redundant — field is a child entry) |
| **Field** | `(6,)` + type-skeleton **subtree** | — | the skeleton opcode tree (0x11/0x13/0x14…) | **name** (field rename must be see-through), span, doc |
| **Enum** | `(9,)` | — | Variant children, `KEY_GPARAM`, `KEY_WHERE`, `KEY_AUTO` | name/span/doc/cfg/attr/vis |
| **Variant** | `(10, vform)` | `KEY_VFORM` | variant fields | name, **`KEY_VDISCR`** (discriminant *values renumber on insertion* → would false-split a whole enum), span, doc |
| **Trait** | `(7, tflags)` | `KEY_TFLAGS` | `KEY_SUPER` (→ §3.6 ref label), `KEY_GPARAM`, `KEY_WHERE`; methods via `KEY_PARENT` | name/span/doc/attr/vis |
| **Impl** | `(8, iflags)` + self-head opcode | `KEY_IFLAGS` | `KEY_IOF` (trait ref), `KEY_IFOR` (self-ty subtree), `KEY_GPARAM`, `KEY_WHERE`; items via `KEY_PARENT` | name (anon), span, attr |
| **TypeAlias** | `(4,)` | — | `KEY_TYPE` subtree, `KEY_GPARAM`, `KEY_WHERE`, `KEY_AUTO` | name/span/doc |
| **Const** | `(9c,)` | — | `KEY_CTY` subtree | **`KEY_CVAL`** (literal churn ≠ identity; already not S-marked), name/span/doc |
| **Static** | `(10s, mutable_bit)` | `mutable` | `KEY_CTY` subtree | `KEY_CVAL`, name/span/doc |
| **Reexport** | `(12,)` | — | `KEY_RETGT` → §3.6 ref label | name/span/doc |
| **Module** | `(1,)` | — | contained entries via `KEY_PARENT` | name/span/doc/cfg |

### 3.3 Richness win #1 — the type-skeleton stays a TREE

`workspace/ir/skeleton.rs` already emits a recursive opcode tree: `Tuple(0x12)`→arity+elements,
`Slice(0x13)`→element, `Array(0x14)`→element, `Reference(0x08)`/`MutPointer(0x06)`→target,
`Union(0x15)`/`Intersection(0x16)`→elements. **Splice those opcodes in as real child nodes; one
opcode = one node label.** Then `fn(Vec<u8>)->Result<T,E>` vs `fn(HashMap<K,V>)->Result<T,E>` share
the `Result<_,_>` head (continuity preserved through an argument swap) and differ only in the
argument subtree (real change registered, fanout-weighted). Hashing the skeleton to a token makes
those look 100% different — a false split on the *most common* API evolution (type refinement).
**Free; the encoder already produces the tree — just don't hash it.**

### 3.4 The `hist` field is the sound-bound carrier

`PqProfile.hist: LabelDegreeHist` (label histogram + fanout/degree histogram) is exactly the input
to the *provable* bounds of §1.3 (`label L1/2`, `degree L1/3`). It is **not** an optional "extra
safe bound" as §6.6 framed it — after the role-swap it is the **primary** prune signal. Compute it
in the same O(n) traversal as the grams.

### 3.5 Visibility — exclude from the label, keep in scoring

`KEY_VIS` *is* S-marked (drives semver) but is a **liability in a continuity label**:
`pub(crate) fn foo → pub fn foo` is the *same function gaining exposure* — the archetypal thing to
see through. Excluding vis from the pq-label means a promotion doesn't read as a discontinuity;
vis still contributes via the existing `W_API_SURFACE` scoring signal. Same reasoning excludes
`KEY_DEPRECATED` and most `KEY_ATTR` (`#[inline]`/`#[must_use]` churn) from labels — keep them for
scoring. (`#[repr(...)]` is layout identity; leave it to scoring too rather than a label bit, for
safety.)

### 3.6 Richness wins #2/#3 — dependency references (the Unison lesson)

Confirmed from source: `StableRef = (PackageLineageId{ecosystem,name}, IntroId)`, and skeleton
`Same`(0x01) embeds a raw 32-byte `IntroId` that is **content-derived and stable across the
dependency's own renames**. So *hash-not-name is already the law* for same-package refs — a caller
referencing `dep::Foo` by its `IntroId` does not perturb when `Foo→Bar` inside the dependency. Two
labeling fixes remain:

- **#2 Foreign refs (0x02) embed the package name+id.** A dependency republished under a new
  lineage (fork, scope move `left-pad → @scope/left-pad`) changes those bytes and spuriously
  splits *every consumer*. Fix: in the pq **label** (not the identity skeleton hash), collapse a
  foreign ref to `(0x02, intro_id_only)` — drop ecosystem+name. The `IntroId` is *what it is*; the
  lineage is only *where to find it*.
- **#3 Same-package `IntroId` is 32 bytes of high-entropy label** → two structurally-twin functions
  that call *different* helpers look maximally dissimilar (over-discrimination at the gate). Fix: a
  **two-tier alphabet** — in the R0/R1 decl profile, replace a referenced `IntroId` with its
  *referent's* `(kind_disc, top-level shape class)` (a 2-byte bucket); reserve the full 32-byte id
  for the **APTED relabel cost** (matching the *same* referent costs 0; a *different same-kind*
  referent costs a small relabel, not a full delete+insert). Graceful degradation: rename a
  dependency symbol → same kind bucket → tiny cost → continuity preserved.

### 3.7 Body region-tree labels (the R2 deep-verify plane)

> **⚠ Superseded by §9.3/§9.4.** `ControlSketch` is a flat `Vec` (no nested tree) and is contested
> across two `body.rs` (§9.1), so it is **not** the near-term substrate. Build the body tree from the
> **normalized CST subtree** via span containment; the label ideas below still apply, but the source
> tree is the CST, not `ControlSketch`, until `.nb` ships a nested region tree.

Region tree = `ControlSketch` (If/Match/Loop) as skeleton; hang `BodyCall`/`LocalBind`/oracle
mentions off the enclosing region by `RelSpan` containment. Labels:

- **Region:** `(region_disc, arm_count)` (Match arm-count / If else-presence are edit-stable-ish).
- **LocalBind:** `(LocalKind)` — **exclude the name** (locals rename constantly; §6.5's "renamed
  local barely moves the profile" is *enforced* by omitting the name).
- **Call:** label = resolved `OracleCall.target` **StableRef by `IntroId`** when present (the
  rewrite-robust "does the same work" fingerprint is the *multiset of resolved callees*), else the
  treesitter `BodyCall.name`-hash with a `low_confidence` bit.
- **`RelSpan`: EXCLUDE from every label** — it exists so edits shift spans without perturbing
  identity; it builds the containment tree only, never label bytes.

### 3.8 Fanout weighting — children are not equal

Fold a frozen **integer** weight per child class into both (i) gram multiplicity and (ii) the APTED
`IrCost` delete/insert/relabel cost, so distance tracks *semantic* magnitude, not raw arity:

| Child class | Weight | Why |
|---|---|---|
| Function param (`KEY_IN`/`KEY_OUT`), record/variant field | **4** | arity/type change = breaking API change — the strongest signal |
| `KEY_FNSIG` flag flip, impl head (`KEY_IOF`/`KEY_IFOR`), `KEY_SUPER` | **3** | contract change |
| type subtree node (`KEY_TYPE`/`KEY_CTY`/`KEY_FIELDTY`) | **3**, ×0.5 decay per depth | a swap at the `Result<_,_>` head > a swap 4 levels deep |
| `KEY_GPARAM`, `KEY_AUTO`, `KEY_TFLAGS`/`KEY_IFLAGS` | **2** | real but often source-compatible |
| `KEY_WHERE` predicate | **1** | reorderable/equivalent; already sorted-set-normalized in F1 |
| `KEY_ATTR`/`KEY_DEPRECATED` (if admitted at all) | **0–1** | mostly noise — must not split a symbol |
| body `LocalBind` | **0.25** | a local barely moves identity |
| body region | **1** | control-flow restructure is a signal but rewrite-tolerant |

Weights are **frozen integers** (scale ×4 so all above are integral) exactly like the existing
`W_API_SURFACE=50…` and the frozen skeleton opcodes — a weight change is an identity-format change
(→ `nudox.pqprofile.v2`), else old/new profiles become silently incomparable *at the generation
boundary we care about*. Child **ordering** follows F1's canonical order verbatim (sets sorted,
seqs source-ordered) so the `q`-window is reproducible.

### 3.9 `p` and `q`

IR trees are **shallow + high-fanout** (the opposite of the deep-narrow XML Augsten tuned for).
Recommend **`p=2, q=2` for the declaration tree**, **`p=2, q=3` for the body region tree**:

- small `p`: decl depth is ~3–6; `p=2` (parent+grandparent) already places a type change *in its
  role* (param vs field); `p=3` inflates top-of-tree null-padding for no gain.
- small `q`: high fanout means `q`-grams per node = `f+q−1`; `q=2` keeps the profile O(n), captures
  positional-param ordering, and a single insertion perturbs only the two adjacent windows (graceful
  degradation) rather than all of them. Body gets `q=3` (lower fanout, call-*sequence* signal
  matters). Profile size stays ≈ `2n` grams → cheap 128-byte MinHash.
- Do **not** raise `p` to compensate for shallowness — spend the modeling budget on the **fanout
  weights** (§3.8), which is where our discrimination actually lives.

> **Precondition footnote (ties to §1):** at `p=2` the pq-distance is *not even in principle* a
> fanout-weighted-TED lower bound (Thm 7.4 needs `p=1`). That is *fine* here precisely because we
> demoted pq-distance to ranking; the sound prune is the histogram stack (§1.3), which is
> `p,q`-independent. Choosing `p=2` for better *ranking* no longer costs us any (already-absent)
> guarantee.

---

## 4. Scope, cost, storage — making it affordable

### 4.1 The gate the request proposed is unsound; here is the correct split

Structural fact: `IntroId`'s preimage includes `package ‖ kind ‖ segments ‖ name ‖ disambiguator`
(`workspace/ir/intro.rs`). So a **rename/move mints a new `IntroId`** and appears as
`Deleted{old} + Introduced{new}`. But `compute_api_surface_hash` **excludes** name+parent. Hence a
pure rename produces an *identical* per-entry api-surface hash. Therefore:

> A generation-level gate keyed on "the api-surface-hash aggregate is unchanged" **skips exactly the
> generations that contain pure renames/moves** — the modal thing continuity exists to track.
> **UNSOUND.**

Correct — **two independent gates, two keys:**

```
GATE-SEMVER      (skip re-embed / shape-semver):  the set of (IntroId → api_surface_hash) is
                 unchanged  ⇒  no shape-level change.        // api-surface hash — its real, V-1-tested job
GATE-CONTINUITY  (skip continuity matching):       the live IntroId SET is unchanged
                 ⇔  compute_sigma's deleted_ids = ∅ AND new_wire_ids = ∅.
```

Even stronger "zero work" gate, borrowed from §12 emission-reuse: if the `IntroId` set is unchanged
**and** every surviving id's `payload_hash` is unchanged, the generation is bit-identical at the IR
plane and continuity is the identity — no matcher run at all. Prefer this; it subsumes the "many
git refs between code changes" motivation soundly.

### 4.2 Symbol-level scope — and the one leak to close

Run pq-gram/APTED only over `new_wire_ids × deleted_ids` (add-set × delete-set) — which is already
what `compute_sigma` does. A visibility filter on top ("public only") is a reasonable cost cut, but:

- **Never scope the candidate set by "symbols whose `api_surface_hash` changed"** — a pure rename
  has the *same* hash on both sides, so that predicate excludes the very pair you're matching.
- **The priv→pub "promotion-rename" leak:** helper `foo` made `pub` as `bar` — old (private) side
  isn't in a "public-now" delete-set → the new symbol reads as `Introduced`, losing the link. Fix:
  scope the candidate **delete-set** by *"public in EITHER generation"* (union), ~2× the delete-set,
  still O(Δ). Same closes `#[doc(hidden)] pub` (case 7). All these leaks are false *splits* — the
  never-merge invariant is never violated; this is a completeness, not a safety, fix.

### 4.3 Cost model (corrected)

The target is **O(Δ `IntroId`-set)**, not "O(Δ public surface)". Honest statement:

- **Per generation:** `O(|surface|)` to recompute per-symbol `api_surface_hash` + tip sketches — or
  `O(1)` when the emission-reuse gate proves the id-set + payloads unchanged.
- **Continuity, only when the id-set changed:** candidate retrieval `O(|added| × #bands)` on
  **sketches alone**; histogram-bound prune `O(|added| × candidates)`; **APTED only on survivors**,
  bounded by `|added| × min(bucket, CANDIDATE_CAP=64)`, each `O(n³)` with `n` = symbol node count
  (tens) → sub-millisecond. "One *projection*, two *derivations*" — semver-delta shares the surface
  projection, but continuity *matching* is an irreducibly separate pass (the surface diff says *that*
  `foo` left and `bar` arrived; only APTED says they're the *same* symbol).

### 4.4 The materialization-deferral win (make it a hard requirement, not an optimization)

§6.5 admits continuity "couples to having both generations' IR materialized." Deferral breaks that:

- LSH retrieval needs only the 128-byte **sketch**; the histogram bounds need only `hist` — **neither
  needs the old body.**
- **Only APTED** needs both full ASTs, and only on survivors.

So the schedule is: (1) load new-gen IR (you're recording it); (2) retrieve candidates from LSH over
**tip sketches** — zero old-body fetch; (3) apply histogram bounds — zero old-body fetch; (4)
**materialize old bodies only for surviving `(added × candidate)` pairs.** In the remote-is-fallback
world where old bodies are often non-resident, this is **1–2 orders of magnitude fewer
materializations** (a normal patch: ~4–40 body fetches vs. fetching the whole old generation).
**Requirement:** the `KEY_SKETCH` line + `hist` live on the **`.nir` declaration artifact**, never
buried in the `.nb` body — so LSH/bounds are queryable without the body.

### 4.5 Storage — tip-public sketches only, recompute the rest

| Reading | Size (crates.io order-of-magnitude) |
|---|---|
| naïve: every symbol, every version, all history | **~1 TB** (the doc's "sketch per symbol alongside the IR") |
| public-only, all history | ~150 GB |
| **recommended: tip-public sketches only + LSH over them; recompute historical O(n) on demand** | **~15 GB** |

The continuity *edge* is the durable artifact; the sketch is a transient means, O(n)-recomputable
from content-addressed IR when a blame walk needs it. **Push back on "store a sketch per symbol
alongside the IR"** — persist tip-public (T0), recompute history.

### 4.6 APTED worst case & LSH maintenance

Worst case is generated getters/DTO accessors: thousands of near-identical shapes → one giant LSH
bucket, histogram bounds *can't* prune (shapes really are near-identical), every pair survives →
O(k²). This is §6.1's non-uniqueness biting. Mitigations, all already-present-in-spirit:

- **`CANDIDATE_CAP = 64`** (already in `continuity.rs`) deterministically bounds APTED runs at
  `|added|×64` regardless of bucket size — the pq/LSH layer inherits the same cap.
- Run the **histogram bounds *before* APTED** (they discriminate `getter(x:u8)` vs `getter(y:String)`
  at O(profile) cost even when pq-distance ties).
- **R-OV short-circuit** for lone-1↔1 buckets; **ambiguity → give up cheaply** (margin rule) rather
  than escalate — under never-merge, "give up" is the *correct* answer, not a compromise.
- **LSH partitioned by `PackageLineageId`** for continuity (a rename never crosses a package;
  per-package tip surface ≈ 300 sketches → maintenance is O(Δ) per generation). A corpus-global LSH
  exists *only* for the separate "find similar code anywhere" feature (§2) and **must not feed `σ`**.

---

## 5. Robustness — porting the never-merge gates onto the metric

The §6.6 snippet is a *binary, per-pair* decision and thereby **weaker** than the shipping matcher.
Every catastrophic path below reduces to the same root cause: **a small distance to the *wrong*
symbol, decided without a margin/tie/corroboration gate.** The fix is to run an *assignment over the
full candidate set* with `continuity.rs`'s discrete gates, where the metric replaces the *scorer*,
not the *assignment discipline*.

### 5.1 Failure catalog (merge-class tagged; false-merge = CATASTROPHIC, false-split = acceptable)

| # | Input | Naïve §6.6 behavior | Class | Mitigation |
|---|---|---|---|---|
| F1b | two unrelated tiny leaves (`fn id(&self)->u32` vs `fn len(&self)->u32`) | ~identical tiny profile → merge | **CATASTROPHIC** | `MIN_GRAMS` floor + require corroborating exact signal |
| F1c | marker traits / newtypes (`Meters(f64)` vs `Seconds(f64)`) | empty profile, MinHash meaningless → merge | **CATASTROPHIC** | below-floor ⇒ pq path inadmissible; decide by name/kind/parent exact only, else Unresolved |
| F2 | two structurally-identical helpers, one deleted one renamed | distance 0 to *both*; per-pair `classify` commits first visited | **CATASTROPHIC (silent, order-dependent)** | **the nightmare (§5.4)** — margin refusal |
| F3 | derive/generated getters (huge near-identical cluster) | LSH bucket explodes, all survive, many cross-match at 0 | **CATASTROPHIC at scale** | idf down-weighting (§5.3) + `CANDIDATE_CAP` + kind/parent/stem bucketing |
| F4 | same lib re-analyzed by a producer whose skeleton opcodes changed | whole package = delete+new, silent mass deletion | false split (safe) but **bad UX** | stamp `producer_skeleton_version`; on mismatch tag `Unresolved(producer-drift)`, don't emit confident mass-delete |
| F5a | Type-4 (`x!=0`→`bool(x)`, loop↔fold, inline/extract) | new symbol | false split (by design, §6.5) | none — the safe direction |
| F5b | a Type-4 rewrite that lands structurally near an *unrelated* symbol | distance ≤ thr, no margin → merge to wrong symbol | **CATASTROPHIC** | margin rule + require a nominal corroborator for High |
| F6 | hostile package crafts a profile colliding with a victim's, to graft blame | LSH buckets them; small APTED cost → poisoned edge | **CATASTROPHIC (targeted)** | continuity is **`PackageLineageId`-scoped** — never match across packages for `σ`; and continuity is a *derived edge*, so even a poisoned edge can't corrupt the `IntroId` key |
| F7 | borderline: renamed + one field added, distance right at `thr` | binary decision at the worst-calibrated point | either | three-band output (§5.5) |

### 5.2 Tiny-tree / min-distinct-gram floors

- **`MIN_GRAMS ≥ 8`** (with `p=2,q=2`): below this the MinHash Jaccard variance is too high to tell
  "same" from "coincidentally-shaped." Below the floor, **do not run the pq path** — fall to nominal
  exact signals (name/parent/`sig_key`); if undecided → **Unresolved**, never a structural guess.
- **`MIN_DISTINCT_GRAMS ≥ 4` after idf weighting** — two getters can share 6 identical grams but 0
  distinctive ones; require ≥4 grams whose corpus-idf exceeds a floor before a structural match can
  reach **High**.
- **APTED cost 0 on a 3-node tree is coincidence, not evidence.** High requires *either* tree-size
  ≥ `N_MIN` *or* a corroborating exact nominal signal.

### 5.3 Common-gram down-weighting (idf) — ranking/gating ONLY

Maintain a per-PURL-shard gram-frequency table; `idf(g)=log(N/df(g))`. Use idf to (i) rank
candidates and (ii) enforce `MIN_DISTINCT_GRAMS`. **Critical constraint (from §1):** idf weighting
is corpus-tuned and **breaks any bound property** — so it must **never** enter the provable prune
`max`. Unweighted histograms/size = the sound prune; idf-weighted = the don't-flood ranking + the
distinctiveness floor. (This generalizes §6.6's own "keep learned embeddings for a separate layer" —
anything corpus-tuned is ranking-only.)

### 5.4 The nightmare scenario, and the exact guard

> Two structurally-identical small helpers in one package, one deleted, one renamed → pq-distance 0
> to **both**, APTED cost 0 to **both**, and §6.6's per-pair `classify(apted(...))` commits the
> **first pair it visits** → silent, order-dependent, non-deterministic false merge that splices two
> unrelated histories irreversibly. This is §6.1's "the mapping is a choice, not a fact" made
> concrete.

**Guard (already in `continuity.rs:396-407`, absent from §6.6):** do not decide per-pair. For each
added symbol compute cost to *all* surviving candidates; take the best; **if any other candidate is
within `MARGIN` of the best (on either side of the match), refuse both** — emit `Unresolved`, leave
the added id fresh and the deleted id `Deleted`. The metric replaces the scorer; the *assignment*
stays the tested greedy+margin over the full candidate set.

### 5.5 Confidence taxonomy — three bands, `σ` written by High alone

| Band | Condition | Emitted as | Touches lineage? |
|---|---|---|---|
| **High** | dist ≤ `thr_high` ∧ (size ≥ `N_MIN` ∨ nominal corroborator) ∧ ≥ `MIN_DISTINCT_GRAMS` ∧ **no 2nd candidate within `MARGIN`** ∧ same kind ∧ R-CHILD(parent) | `Renamed`/`Moved`/`SignatureEvolved` → **committed to `σ`** | **YES (only band that does)** |
| **Soft** | `thr_high < dist ≤ thr_soft`, or High-scoring but below the floors | advisory `RenameEdge` only (`ContinuitySummary.rename_edges`) | NO (old=Deleted, new=Introduced) |
| **Unresolved** | near-tie (within `MARGIN`), F2 repeated-substructure, F4 producer-drift, below-floor tiny trees | explicit `unresolved-continuity` edge (§6.4.3) | NO (recorded as a *question*) |
| **Distinct** | dist > `thr_soft`, kind mismatch, or pruned by the provable bound | `Deleted` + `Introduced` | NO |

Invariant: **`σ` (durable-id reuse) is written by the High band only**, guarded by four discrete
refusals beyond the metric (kind hard-gate, margin, floors+corroboration, R-CHILD). Everything the
metric is *unsure* about degrades toward Distinct/Unresolved — never upward toward a merge. A missed
rename is a redundant row; a false merge is a corrupted history.

---

## 6. Determinism discipline (bit-identical across machines)

This system requires reproducible results; the profile is an on-disk `KEY_SKETCH` value and *must*
be a pure function of the tree. Hazards + fixes:

1. **MinHash permutations = pinned constants** in the format spec, never RNG/thread-local-seeded
   (`probminhash`/`gaoya` default ctors derive coefficients at runtime). Hard-code coefficients as a
   versioned `nudox.pqprofile.v1` constant.
2. **Bag→shingle canonical encoding** (weighted MinHash or `(gram, occurrence_index)` in a fixed
   preorder). Audit that `PqProfile::of` reads children in a **stable order** — any `HashMap`-backed
   field set makes the whole profile non-deterministic.
3. **No `f32` in decisions.** `distance`/`ted_lower_bound` return floats; the bounds are ratios of
   **integer** counts — compute and compare in integers (cross-multiply `num·den' vs num'·den`),
   never let a last-ULP difference flip a prune. Define any "scale" as an exact rational.
4. **APTED tie-break pinned:** min-cost edit *mappings* are non-unique (§6.1); pin a deterministic
   tie-break (lexicographically-least by preorder) so the decision *and* any recorded rename mapping
   reproduce. Pin the `IrCost` table in the registry.
5. **LSH band enumeration canonical/sorted**; `SortedBag<PqGram>` sorts by a total, content-only,
   endian-independent key over canonical (UTF-8, no interner-id leakage) label bytes.
6. **Version everything as identity format:** labels, weights, `(p,q)`, permutations, `IrCost` all
   under `nudox.pqprofile.v1` with committed golden vectors (mirroring `nudox.tyskel.v1`). Any change
   → `v2`, else profiles across the generation boundary become silently incomparable.

---

## 7. Corrected data structures & the integration seam

### 7.1 Revised `PqProfile`

```rust
/// One decomposition, three roles. `nudox.pqprofile.v1` (frozen — SEE §9.7: only the
/// DECLARATION half may freeze today; the body half stays unfrozen until a real body producer exists).
pub struct PqProfile {
    grams:   SortedBag<PqGram>,   // p=2,q=2 decl (p=2,q=3 body); labels per §3.2/§3.7
    sketch:  [u8; 128],           // MinHash of the gram SET  → RETRIEVAL/RANK only (heuristic)
    hist:    LabelDegreeHist,     // label + degree histograms → the PROVABLE prune bounds (§1.3)
    n_nodes: u32,                 // for |n₁−n₂| size bound + tiny-tree floor
    stamp:   ProfileStamp,        // comparability regime (§9.4) — replaces the single `prod_ver`
}
/// Three independent drift axes, ALL baked into the LSH band key so incomparable
/// regimes physically cannot collide in a bucket. A single `prod_ver` scalar (the
/// original §7.1 sketch) conflates them and is insufficient.
pub struct ProfileStamp {
    grammar_ver:  u16,   // arborium grammar revision (renames CST node kinds → F4). CST/body tier only.
    normmap_ver:  u16,   // version of the CstLabel normalization table (§9.4). Its change = identity-format change.
    skeleton_ver: u16,   // nudox.tyskel.v1 opcode-table version. Declaration tier; grammar-INDEPENDENT.
}
impl PqProfile {
    pub fn of(decl: &OwnedEntryPayload, body: Option<&BodyProfile>) -> Self;  // O(n), stable order; body via §9.3 abstraction
    pub fn lsh_bands(&self) -> impl Iterator<Item = BandKey>;                 // sorted; RETRIEVAL; band key includes `stamp`
    pub fn ted_lower_bound(&self, o: &Self) -> u64;   // max(size, label/2, degree/3[, branch/5]) — SOUND, integer
    pub fn pq_rank(&self, o: &Self) -> u64;           // idf-weighted pq bag distance — RANK ONLY, not a bound
}
```
> **Two mismatch rules (from `stamp`):** `grammar_ver` mismatch → CST/body-derived results are advisory
> **Soft** at most (the *declaration* R0/R1 are unaffected — the skeleton tree is grammar-independent);
> `normmap_ver` mismatch → hard **`Unresolved(map-drift)`**. The declaration profile carries only
> `skeleton_ver`; the body profile carries all three. This is the concrete F4 gate.

### 7.2 Where it hooks into `compute_sigma` (intra-package; body axis only)

Two surgical additions that **reuse** the existing greedy/margin/R-OV/R-CHILD/cross-kind machinery —
build them **only once the body is materialized in the matcher's inputs** (until then the
declaration plane is redundant with `by_shape`, §2.1):

- **Hook A — a 4th candidate index** `by_lsh_band: BTreeMap<BandKey, Vec<IntroId>>` in `TipIndexes`,
  populated in `build_indexes`, probed in `candidates_for` (respecting `CANDIDATE_CAP`). Purely
  additive candidate generation; determinism preserved (bands are a pure function of the tree).
- **Hook B — one bucketed scoring signal** in `score_pair`, from the **body** profile distance:
  ```rust
  const W_PQGRAM_NEAR: i32 = 12;  // pq body-distance very small
  const W_PQGRAM_MID:  i32 = 6;   // moderate
  // > MID → 0 (and pruned by ted_lower_bound before APTED)
  ```
  **Weight rationale (never-merge):** it sits *below* the exact `W_API_SURFACE=50` and at/below
  `W_NAME_EQ=20`, so body-similarity **alone (12) never reaches `THRESHOLD_SOFT=30`** — it can only
  *promote* a candidate another exact signal already surfaced. Never let pq be the sole High
  contributor; never give it ≥30. APTED runs *inside* `score_pair` for survivors of
  `ted_lower_bound`, converting exact cost → the bucket.

### 7.3 The corpus plane (separate, new, provenance-only)

```rust
/// Cross-package "where did this come from" — NEVER writes σ; emits provenance-DAG edges.
fn corpus_candidates(p: &PqProfile) -> impl Iterator<Item = (Purl, IntroId)>;
```
Feeds a *separate* classifier that emits `RenameEdge`-shaped **provenance claims** (fork /
vendored-copy / renamed-crate), gated by the same three-band confidence, and — because a false
cross-package merge splices two *projects'* histories — biased even harder toward Unresolved.

---

## 8. Build order & open decisions

**Phased (each independently landable, none regresses the tested matcher):**

- **P0 — soundness fix, no new subsystem.** Add the provable bound stack (`|n₁−n₂|`, label `L1/2`,
  degree `L1/3`) as `ted_lower_bound` over the *existing* `api_surface_hash` shapes; integer-only;
  golden vectors. Rewrite §6.6 prose per §1. *(Pure correctness; ships value immediately.)*
- **P1 — profile + determinism.** `PqProfile::of` over the **declaration** tree with the §3 label
  schema (skeleton-as-tree #1, dep-ref coarsening #2/#3), pinned MinHash, `nudox.pqprofile.v1`
  golden vectors. Store **tip-public** sketches on the `.nir` (§4.5).
- **P2 — gates & scope, corrected.** `GATE-CONTINUITY` on the `IntroId`-set delta (not surface
  hash); either-generation-public delete-set scoping; three-band `High/Soft/Unresolved` output;
  tiny-tree/min-distinct-gram floors; idf ranking-table (ranking-only).
- **P3-pre — unblock the body axis (plumbing; NOT continuity).** The body axis is unreachable from
  today's tree (§9.1): (i) `emit_body` has **zero producer callers** — bodies never leave the
  producer; (ii) `compute_sigma`'s `staged` tuple `(IntroId, OwnedEntryPayload, Option<IntroId>,
  Vec<LinkWire>)` has **no body field** — bodies travel a separate `StreamFrame::Bodies` channel and
  are never joined to the matcher's inputs; (iii) two conflicting `body.rs` (`workspace/ir` vs
  `crates/nudox-ir`) disagree on `ControlSketch`/`merge_body` (§9.1). **None of P3 is buildable until
  these three land.** P3-pre = a producer emits a body-profile keyed by `IntroId`, that profile
  reaches the matcher, and the `body.rs` conflict is resolved (ties to `IR-MIGRATION-PLAN`).
- **P3a — CST body axis (near-term substrate).** Once P3-pre lands: build the body `BodyProfile`
  (§9.3) from the **normalized CST subtree** (§9.4), not the flat `ControlSketch` (which has no
  nested tree). Fidelity-gated R2 tie-break via `BodyMergeNote`; Hooks A+B into `compute_sigma`. The
  body label schema stays **unfrozen / behind a flag that cannot write `KEY_SKETCH`** until pinned
  against a *real producer's* output (§9.7).
- **P3b — region-tree refinement.** When `.nb` ships a *nested* region tree, it supersedes the CST as
  the preferred `BodyProfile` producer for fidelity-matched pairs; the CST rung stays as the
  permanent `treesitter_only` fallback. Same seam, no rewrite (§9.3).
- **P4 — materialization deferral.** Wire LSH/bounds to run on sketches alone; materialize old
  bodies per survivor pair only. Make it a hard requirement (§4.4).
- **P5 — corpus plane.** `PackageLineageId`-partitioned LSH; provenance-DAG edges; never `σ`.

> **Honest status line (publish this):** *Today continuity sees the declaration only. P0–P2 tighten
> and make sound the existing declaration-shape matching; they add no body signal. The body axis is
> designed but blocked on an unshipped body producer and the `.nb` wave, and must not be described as
> working.* (§9.6)

**Open decisions (need a human call):**
1. **LSH banding parameters** vs. a *target recall* on the set-Jaccard prefilter (§1.4) — accept
   probabilistic recall (defensible under never-merge) or bound the miss rate?
2. **Include the binary-branch `/5` and string-edit-on-Euler bounds** (tighter, more compute) in
   the prune `max`, or ship with size+label+degree only?
3. **`thr_high` / `thr_soft` / `MARGIN` / `N_MIN` / `MIN_GRAMS` / `MIN_DISTINCT_GRAMS`** concrete
   values — calibrate on a labeled rename corpus before freezing (they become identity format).
4. **Body fidelity policy for R2** — exact predicate for "both generations fidelity-matched
   `Present`" and what to do on `treesitter_only` (abstain vs. low-confidence). See §9.3 ladder.
5. **Resolve the two-`body.rs` conflict** (`workspace/ir/body.rs` region-ish `ControlSketch` vs
   `crates/nudox-ir/src/body.rs` degenerate span-less `ControlSketch` + vec-concat `merge_body`) —
   the body label schema (§3.7) cannot be specified until one is canonical. Ties to `IR-MIGRATION-PLAN`.
6. **How the body profile reaches the matcher** — bodies are not in `compute_sigma`'s `staged` inputs
   today (separate `StreamFrame::Bodies` channel). Decide the join: a body-profile side-table keyed
   by `IntroId` consumed by `build_indexes`, vs. widening the `staged` tuple. (§9.1, §9.5)

---

## 9. The generation-time mechanism — leverage all of the parse, compute-efficiently, honestly

This section is the second adversarial round (three grounded reviewers: compute-mechanism,
substrate/leverage, assurances). Its job: ensure the profile is computed *throughout IR generation*
in the cheapest correct way, that it exploits the full richness of the parse, and — the load-bearing
correction — that we do not build a frozen identity format against machinery that **does not exist
yet**. Two reviewers disagreed; the reconciliation is stated in §9.2.

### 9.0 Two grounding facts that reshape §2.2 / §3.7 / P3

1. **The body plane is an unpopulated, internally-contested scaffold.** `BodyFacts`/`TreesitterBody`/
   `OracleBody`/`merge_body` are *defined* (`workspace/ir/body.rs`) but **constructed by no producer**
   (grep: zero `TreesitterBody {`/`OracleBody {` sites outside `body.rs`/`body_wire.rs`/tests). The
   wire that carries bodies — `SymbolSink::emit_body` (`ir-vcs/protocol/sink.rs` ≈151) — has **only
   test callers**. And there are **two conflicting `body.rs`**: `workspace/ir`'s region-ish
   `ControlSketch { If{cond,then,else}, Match{scrutinee,arms}, Loop{body} }`, vs `crates/nudox-ir`'s
   **span-less** `ControlSketch::If` unit variant + a vec-concat `merge_body(a,b)` (`FIXME: Rev2
   dedup`). The matcher depends on `workspace/ir`, but the two are mid-migration.
2. **The parse's structural richness is parsed, walked once, then thrown away.** `generate/cst.rs::
   resolve_file` parses each file, runs a *single* `walk_references`, keeps only reference spans, and
   **drops the C-allocated tree** ("the tree itself is transient — parsed, walked, and dropped"). The
   snippet path serializes the tree to an S-expression *string* (`treesitter/mod.rs`). Neither retains
   a profileable structured tree — yet the CST is the **richest** structural signal in the system.

Consequence: **§2.2's "body axis = the real earn" is unrealizable on today's tree**, and **§3.7's
body labels cannot target `ControlSketch`** (it is a flat `Vec`, no nested tree, and contested).
The earn is real *as a design*; it is *not* a current capability (§9.6).

### 9.1 Blocking preconditions for ANY body axis (CST or region-tree)

Three independent, unshipped preconditions gate P3. Building the body half before they land means
freezing a format against fabricated inputs (§9.7):

| # | Precondition | Today | Where it must land |
|---|---|---|---|
| B1 | A producer constructs a body profile and emits it | `emit_body` dead (test-only callers) | a `workspace/compiler` producer |
| B2 | The body reaches the matcher's inputs | `compute_sigma`'s `staged` tuple has **no body field**; bodies ride a separate `StreamFrame::Bodies` channel, never joined | widen `staged` OR a body-profile side-table keyed by `IntroId` consumed in `build_indexes` (open decision 6) |
| B3 | One canonical `ControlSketch`/`merge_body` | two conflicting `body.rs` | resolve via `IR-MIGRATION-PLAN` (open decision 5) |

### 9.2 Two profiles, two tiers, two keys — the reconciliation

The compute reviewer said "profile rides the existing parse walk for free"; the assurances reviewer
said "false — that walk is at the wrong *tier* to key the profile, and the CST→IR join is lossy."
**Both are right about different profiles.** Resolve by making the split explicit:

| | **Declaration profile** (the stored truth) | **Body profile** (transient enrichment) |
|---|---|---|
| Source tree | IR skeleton opcode tree (§3.3) + `OwnedEntryPayload` | normalized CST subtree (§9.4) now → `.nb` region tree later |
| Tier / when | **seal tier**, after the oracle mints the `IntroId` | **parse tier** (cheap, live tree) — but see keying |
| Keyed by | **`IntroId` directly** (minted here) — no join needed | built at parse keyed by `name_span`; **must be re-keyed to `IntroId`** |
| Inputs exist today? | **YES** (`OwnedEntryPayload`/skeleton are produced) → freezable | **NO** (body producer is B1) → **not freezable** (§9.7) |
| Role | R0 retrieve + **R1 sole sound prune**; grammar-independent (`skeleton_ver` only) | R2 deep-verify tie-break; scoring weight ≤12, never sole High |
| Cost | a **new O(n) pass at seal** — cheap in absolute terms, **not** a free parse-rider | ~free during the parse walk (§9.8), but transient until re-keyed |

**The keying rule (from the assurances review, decisive):** the profile that is *stored/compared*
must be keyed by the `IntroId` that is minted at seal — **not** carried up from the transient CST by
span. The oracle re-spans, drops, merges, and adds items between skeleton and seal, so a span/name
join is lossy (§9.5). Therefore: compute the body profile cheaply at parse (§9.8), carry it forward
keyed by `name_span`, and **join it to the `IntroId` at seal via the resolver's existing, tested
`name_span → NudoxPath → IntroId` occurrence attribution** (`generate/resolve.rs` 76–93) — and where
that join is ambiguous/lossy, the body profile **abstains** (contributes `Absent`, §9.5), never
mis-attributes. Abstention is safe under never-merge: a lost profile is a false *split* (cheap), never
a false *merge* (catastrophic). The **declaration** profile sidesteps all of this — it is built at
seal, keyed by `IntroId`, from grammar-independent IR.

### 9.3 The `BodyProfile` abstraction + the substrate ladder (write R2 once)

R2 must be written **once** against a `BodyProfile` trait, so the substrate can evolve without a
rewrite. `BodyMergeNote { treesitter_ran, oracle_ran }` (`body.rs` ≈184) is the ready-made switch for
which substrate is trustworthy per pair:

| Both sides' fidelity | Body substrate for R2 | Band ceiling |
|---|---|---|
| `.nb` region tree present both sides (future) | ③ nested region tree (canonical) | High eligible |
| oracle+treesitter both, no `.nb` yet | **② normalized CST** + oracle callee `StableRef`s | High eligible (calls carry `IntroId`) |
| `treesitter_only` either side | ② normalized CST; callee = `BodyCall.name`-hash + `low_confidence` | **Soft** max |
| `grammar_ver` mismatch (§9.4) | ② structure only; call-target identity suspect | **Soft** max |
| body `Absent` either side | none — decl-only bands | decl-only |
| `n_nodes < MIN_GRAMS` (tiny) | pq path **inadmissible** | Unresolved / nominal-only |

The declaration profile (①) is the **always-trustworthy spine** (R0 + the sole sound R1 prune, no
body fetch). ② and ③ are interchangeable rungs of the *same* body axis; ③ supersedes ② for
fidelity-matched pairs when `.nb` ships, and ② remains the permanent `treesitter_only` rung. **Nothing
is rebuilt when `.nb` lands.**

### 9.4 Normalization map, `OTHER`-sink, and determinism

Raw CST node kinds are grammar-version-specific — a grammar upgrade renaming `function_item →
function_definition` would perturb every profile (mass false-split = F4 made universal). The repo
*already ships* the fix: `categorize_*` / `is_function_like` (`treesitter/mod.rs` 166–427) fold ~40
grammar-specific `(kind, parent)` pairs into 7 frozen `ReferenceKind` discriminants, and already hedge
versions (`is_function_like` lists **both** `function_item` and `function_definition`; nix accepts
`apply_expression|apply|app`). Extend the same taxonomy to a frozen `CstLabel` map:
`FUNC_REGION / IF_REGION / MATCH_REGION / LOOP_REGION / CALL / FIELD_ACCESS / LOCAL_BIND / BLOCK /
IMPORT / OTHER=255`. **The `OTHER=255` sink is the keystone:** an unknown/renamed grammar node
collapses to one sentinel instead of minting a novel label → *bounded* perturbation, not mass split
(the pq analogue of `categorize_*`'s trailing `_ => None`).

**Determinism hazards (every source of a non-pure sketch — all must be closed):**
- **Grammar drift** → the `ProfileStamp.grammar_ver` in the band key (§7.1); no `KEY_SKETCH` byte may
  be written without the full `(grammar_ver, normmap_ver, skeleton_ver)` stamp — mirroring
  `nudox.tyskel.v1`'s domain-tag + golden-vector discipline, which pq has *none* of today.
- **`ERROR`/`MISSING` nodes** are grammar-recovery-dependent → filter `is_error()||is_missing()`
  before hashing; record a `had_errors` bit → gate partially-parsed bodies to low confidence.
- **`HashMap` iteration** leaks order → `SortedBag`/`BTreeMap` only. `indexing.rs`'s
  `build_reference_set_from_bodies` uses `HashMap` iteration — **a counter-example in-tree; do not
  copy it.** `continuity.rs` already mandates "no `HashMap` iteration influences decisions."
- **File visit order** (`fs::read_dir` is OS-order) → for cross-file symbols (§9.5), accumulate the
  per-symbol profile into a `BTreeMap<IntroId,…>` and fold file contributions in `(path, span)` order,
  never walk order.
- **`f32`** → integer bounds only; the `/2,/3,/5` constants applied by cross-multiplication.
- **APTED tie-break** unpinned + **`IrCost` unpinned** → pin lexicographically-least-by-preorder and
  freeze `IrCost` in the registry. **`SmolStr`** → hash canonical UTF-8 bytes, never an interner handle.

### 9.5 `body_span` attribution — failure table + the one guard

If/when a producer buckets grams to the innermost enclosing `body_span`, these mis-attribute:

| Case | Mis-attributes? | Guard |
|---|---|---|
| Macro-generated items (synthetic/overlapping/zero-length spans) | **yes, silently** (innermost is undefined on span overlap) | synthetic/zero-length span ⇒ **no body profile** (`Absent`), never a guessed bucket |
| Nested *named* fn / closure | yes under raw span-containment | attribute by the **definition parent tree** (subtract child-def spans), a frozen identity-format policy — not raw containment |
| Symbol split across files (partial class C#, multi-file `impl`, Go methods on a foreign type) | **yes** — CST profile is per-file, `IntroId` aggregates files | **accumulate by `IntroId`, merged across files in `(path, span)` order** before any comparison |
| No-body items (trait method, alias, marker) | yes → the F1b/F1c tiny-tree **catastrophe** | `Absent` ⇒ body axis contributes *nothing* (not distance 0); `Present ∧ n_nodes<MIN_GRAMS` ⇒ inadmissible → nominal-only |

**The one guard that covers all rows:** the body profile is admissible **only** when the body is
`Present`, above the gram floor, with a non-synthetic span, accumulated-by-`IntroId` in canonical
order. Any other state ⇒ **abstain** → the matcher falls to the existing exact indexes. Abstention can
only fail-to-link (safe), never link-wrongly (catastrophic).

### 9.6 Honest phased capability statement (what we can promise without lying)

| Phase | Real inputs exist? | Honest capability | The over-promise to refuse |
|---|---|---|---|
| **P0** sound bounds | yes (`api_surface_hash` shapes) | **Shippable now.** A graded provable prune (`|Δn|`, label `L1/2`, degree `L1/3`) tightening the existing binary `by_shape` hash. No pq *distance* involved. | Don't call it "pq-gram continuity." |
| **P1** decl profile | yes (`OwnedEntryPayload`/skeleton) | **Marginal + real.** Skeleton-as-tree (§3.3) makes `Result<Vec<u8>,E>→Result<HashMap,E>` a small edit not a split — genuine but *narrow* refinement of the +50 shape signal. | Don't sell it as a materially better candidate index — for *retrieval* it's ~co-extensive with `by_shape`+name+sig (§2.1). |
| **P2** gates & floors | yes (`staged`/`tip` id-sets) | **Real safety layer.** `GATE-CONTINUITY` on the id-set delta; three-band output; floors. | Don't claim it "reduces work" beyond the id-set-unchanged fast path. |
| **P3** body axis | **NO** (B1–B3 unshipped) | **Zero today.** Cannot be built/tested/golden-vectored against real data. | **The dangerous one:** claiming "body-level continuity links surface-churned/impl-stable symbols." That is the plan's headline and it is *false as of this tree*. |

### 9.7 The freeze guard (the single most important assurance)

**No `nudox.pqprofile.v1` *body* label schema may be frozen — no golden vectors committed, no
`KEY_SKETCH` byte written to disk — until a real producer calls `emit_body` and a real body flows
into `compute_sigma` (B1+B2), and the `body.rs` conflict is resolved (B3).** A format golden-pinned
against *fabricated* `BodyFacts` struct literals tests nothing about real producer output, looks done
(green tests), and then forces a `v2` bump when the real producer lands — making every previously
written profile incomparable **at exactly the generation boundary continuity depends on** (F4,
self-inflicted at design time). Mirror `skeleton.rs`'s discipline: its golden vectors pin *real
encoder output*. Therefore: **freeze the declaration label schema now** (real inputs: `OwnedEntryPayload`),
keep the **body label schema unfrozen behind a flag that cannot write `KEY_SKETCH`** until a
live-fixture test pins it against a real body producer.

### 9.8 The compute wins that stand today, independent of the body axis

Even with the body axis deferred, the review surfaced concrete, shippable efficiency wins:
- **Collapse the double parse.** `cst::extract` and `occurrences::build` **each independently
  re-parse every source file** (`cst.rs` ≈144, `occurrences.rs` ≈113) — two full tree-sitter parses
  (the dominant CPU step) + ~6 cursor walks per file (`RustSpec` alone runs 5 `walk_preorder` passes).
  Have `occurrences::build` own the single parse and let `cst`'s reference spans fall out of the same
  `Extraction`. **This is a standalone win, larger than the profiling itself.**
- **Fuse profiling into the existing span-stack walk.** `RustSpec::definitions` (`rust.rs` 73–129)
  already reconstructs the innermost-enclosing frame with a `stack: Vec<(body_end, def_idx)>`. Hang a
  gram accumulator off it (add a `LanguageSpec::definitions_profiled`, default impl keeps all 7
  languages compiling). Per node: read a fixed-size **ancestor ring** (O(1)), a q-sibling ring (O(1)),
  hash one gram, bump the histograms → **O(nodes) folded into an already-O(nodes) walk, dominated by
  the parse.** *The one O(n²) trap:* never call `node.parent()` in a loop or re-hash ancestors —
  maintain the ancestor ring incrementally on push/pop.
- **Reuse across generations for free.** The CST profile rides the already-serializable `Cst`/`CstSet`
  inside the existing `cst` CAS entry; the §12 per-symbol `skeleton_hash` reuse gate **copies the
  declaration profile forward** on unchanged symbols — a *provable* reuse (the profile is a pure
  function of the skeleton), so "a patch touching 3 functions reuses 997."

---

## Appendix — source map

- `docs/GLOBAL-IR-GRAPH.md` §6.1–6.6 (lines 448–606) — parent; §6.4 corpus framing (511–535); §6.6
  snippet (552–606); "sketch per symbol" storage note (875–893); §12 emission-reuse gate (≈858).
- `workspace/ir-vcs/continuity.rs` — the shipping intra-package matcher: `compute_sigma` (261),
  three exact indexes (56–102), frozen weights (34–47), R-OV (294–328), R-CHILD (236), margin
  refusal (396–407), High-only-`σ` (379–412), `CANDIDATE_CAP=64` (50), C-1…C-10 tests.
- `workspace/ir-vcs/f1.rs` — `ContinuityOp`/`RenameEdge`/`ContinuitySummary` (88–109); `KEY_*`
  registry (43–47); per-kind encoders (≈660–1110).
- `workspace/ir-vcs/semver/surface.rs` — `compute_api_surface_hash` (429; **excludes name key1 +
  parent key6**); V-1 doc-only-edit test (≈1034); domain tags (394–395).
- `workspace/ir/skeleton.rs` — the recursive type-skeleton opcode tree (splice, don't hash — #1);
  golden-vector discipline (364–420).
- `workspace/ir/body.rs` — `BodyFacts`/`TreesitterBody`/`OracleBody`/`ControlSketch`/`OracleCall`
  (the R2 body axis, never read by the matcher today; `ControlSketch` is a FLAT `Vec`, not a tree).
  **Conflicts with** `crates/nudox-ir/src/body.rs` (span-less `ControlSketch::If`, vec-concat
  `merge_body`) — resolve before freezing body labels (§9.1, open decision 5).
- `workspace/ir/intro.rs` — `IntroId` preimage includes name+parent (why rename ⇒ new id).
- `workspace/ir/change/ids.rs` — `StableRef`/`IntroId` (hash-not-name is already the law).
- `workspace/ir-vcs/protocol/sink.rs` (`emit_body`/`emit_bodies` ≈151) + `protocol/frame.rs`
  (`BodyWire { intro, body }` ≈43) — the wire that carries bodies keyed by `IntroId`; **only test
  callers today** (§9.1).
- `workspace/compiler/generate/cst.rs` (`resolve_file` 133–151 — transient tree, single
  `walk_references`, dropped) + `occurrences.rs` (≈113, the REDUNDANT second parse) + `treesitter/
  spec.rs` (`LanguageSpec`, `walk_preorder` 190–209, `RawDefinition.body_span` 47–60) +
  `treesitter/rust.rs` (73–129 — the existing span-stack to fuse profiling into) +
  `treesitter/mod.rs` (`categorize_*`/`is_function_like` 166–427 — the normalization taxonomy to reuse).
- `workspace/compiler/generate/resolve.rs` (76–93 `name_span→NudoxPath→IntroId`; 270–285
  `enclosing_def`) — the tested span→id attribution the CST-profile join reuses (§9.2), and the join
  Agent-3 flags as lossy under oracle re-spanning (§9.5).
- `workspace/driver/coordination/indexing.rs` (`build_reference_set_from_bodies` ≈1197) — reads only
  `oracle.calls`/`type_mentions`, never body structure; uses `HashMap` iteration (a determinism
  counter-example NOT to copy, §9.4).
- Theory: Augsten et al., *pq-Gram Distance*, ACM TODS 35(1) 2010 (Thm 7.4; pseudo-metric);
  *Windowed pq-grams*, VLDB J.; *A Survey on Tree Edit Distance Lower Bound Estimation*, SIGMOD
  Record 2013 (label `L1/2`, degree `L1/3`, binary-branch `/5`, Euler string-edit bounds).
