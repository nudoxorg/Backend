# SEMANTIC-IR-VCS-PLAN — True semantic IR versioning, API correctness on IR only

**Date:** 2026-07-17 · **Status:** Rev 2.1 — implementation-grade spec (greenfield; supersedes Rev 1 wholesale; language- and versioning-scheme-agnostic core)
**Companions:**

- As-built prior art: `workspace/nudox-ir-vcs` (libpijul engine), `workspace/nudox-ir`, `workspace/nudox-change` — **prior art, not compatibility constraints** (see §0.2)
- `SMOLVM-PLAN.md` (streaming IR into host recording; SV-9/SV-10, GD-39)
- Producer facts source: `workspace/compiler/compile/rust/RA_IMPLEMENTER_BRIEF.md`
- Research: `.research/librarification/_master_exec_summaries.txt` (§05–§06 identity; §15 GUI diffs)
- ~~`IR-NATIVE-VCS-DESIGN.md`~~ — deleted from the tree; do not cite it. This document is the single normative source.

**Pinned engine:** `libpijul = 1.0.0-beta.11` (root `Cargo.toml` workspace pin). Every libpijul
behavior this plan relies on has been verified against that exact source and is cited by
file/function below. If the pin ever moves, re-verify §1.6 and §11 first.

**Success criteria (product):**

1. All API/semver judgment on the **product path** is a pure function of **IR generations**
   (plus policy, plus hermetic dep IR). No rustdoc-JSON sidecar, no shell-out to
   cargo-semver-checks (CSC) at classify time.
2. On Rust public-API evolution, correctness is **strictly better than cargo-semver-checks**:
   higher true-break recall on a fixed corpus **without** worse false-positive rate (Pareto, §10).
3. IR history is first-class semantic (field/op grain), not prose text diff — with libpijul
   remaining the commute/apply/unrecord engine, unmodified below the record seam.
4. The VCS state at any ref **mirrors the package's API surface** at that version: same
   symbols, same shapes, same reachability — a semantic record, not a prose transcript.
5. The classification core is **language-agnostic**: any language with an IR producer plugs
   in as a law pack (§9.0) — its rules, its exceptions, its versioning-scheme mapping — with
   zero changes to the core engine, the store format, or the hash classes. Rust is the
   reference pack and the CSC benchmark, not a special case.

---

## 0. How to read this document

### 0.1 Normative language

- **MUST / MUST NOT** — correctness depends on it; a divergence is a bug.
- **SHOULD** — the default; divergence requires a written reason in the PR.
- **frozen** — the value/encoding is part of durable identity. It can never be edited in
  place; changing it requires a new domain tag / format epoch and is a design event, not a
  refactor. Frozen items in this doc: every domain string, every numeric discriminant, the F1
  key registry (§6.2), the matcher weights (§5.4), the skeleton opcodes (as-built
  `nudox-ir/skeleton.rs`).
- Rust snippets are **normative shapes** — field names, variants, and semantics are binding;
  syntax niceties (derives, lifetimes) are the implementer's.

### 0.2 Greenfield rule

There is no production data to migrate. Existing dev channels recorded under the Rev-0
formats (`NdIrSym` blobs, `nudox.intro.v1` ids, `nudox.entry.v1` payload hashes) are
**discarded and re-recorded from source** when this plan lands. Consequences:

- No dual-read paths, no legacy projection (`ProjectionConfidence::LegacyText` from Rev 1 is
  **cut**), no v1/v2 remap tables.
- `NdIrSym` (`blob.rs`) survives only as a **debug export** behind a function, never as a
  store format.
- Domain tags bump once, cleanly: `nudox.intro.v2`, `nudox.entry.v2`, `nudox.apisurface.v1`,
  `nudox.embed.v1`, `nudox.sigkey.v1`, `nudox.config.v1`, `nudox.genmeta.v1`,
  `nudox.delta.v1`. Golden pins are regenerated at the same commit that lands each domain.
- `nudox-sync` peers speak **format epoch 2**; mixed-epoch channels are refused (no
  translation layer — greenfield).

### 0.3 The one-sentence architecture

> Producers lower source to a typed IR; the host assigns durable identity (`IntroId`) with a
> continuity matcher; each symbol is one canonical ASCII file in a libpijul working tree, so
> pijul's line algebra **is** field algebra; a pure projector turns any two tips into a
> `PackageDelta` (what changed) and an `ApiReport` (what it means for consumers, under the
> package's language law pack), and every
> downstream consumer (embed, graph, registry, GUI, sync) gates on the hash class it actually
> depends on.

---

## 1. As-built ground truth (verified 2026-07-17)

Everything in this section was read from source, not from prior docs. Corrections to Rev 1's
claims are marked **[fix]**.

### 1.1 Engine and crate layout

| Fact | Location |
|---|---|
| libpijul `=1.0.0-beta.11` is the change engine; hand-rolled algebra retired | root `Cargo.toml`, `nudox-ir-vcs/Cargo.toml`; `nudox-change` is identity vocabulary only |
| GPL at a distance (K7): libpijul linked only by `nudox-ir-vcs` (host/server); GUI consumes API | `nudox-ir-vcs/Cargo.toml` comment |
| Working tree: **flat root**, `{intro_hex}.nir` per live symbol | `serialize.rs::symbol_path` — root-flat to dodge the memory-WC `touch(dir)` `unreachable!()` panic on unrecord re-output. **[fix]** the module doc atop `repo.rs` still says `symbols/{intro_hex}` — stale comment, root-flat is real |
| Blob = line-oriented UTF-8 text, TAB fields, `\\`/`\t`/`\n` escaping, `NdIrSym\t1` magic | `blob.rs` header |
| Links stored once on canonical owner (smaller `IntroId`; local endpoint for cross-package) | `serialize.rs`, `session.rs::stage` |
| Record path is O(delta) via mtime stat-cache + skip-identical-write + mtime floor clamp | `repo.rs::record_generation` steps 1–4; `record.rs::modified_since_last_commit` (mtime ≥ channel-ms, second-truncated) |
| `RecordingSession`: `stage(batch)* → checkpoint(msg)* → finish()` / `abandon()`; deletions only at `finish` (tip_intros − staged); abort resets WC tip → next open resyncs | `session.rs` |
| Stream driver: `Hello → Symbols/Links/SourceDigest/Occurrences/Progress → Finish/Abort`; abort is a normal outcome, protocol errors are `Err` | `stream.rs::record_stream` |
| Published version = frozen channel `version/{label}`; `VersionState` = channel-tip Merkle; serve caches key on it | `version.rs`, `checkpoint.rs` (demand-based **serve** checkpoints — naming collision with session checkpoints; §15 renames the serve one) |
| Public diff surface: `VersionDiff{added,removed,modified}` by intro + whole-payload bytes; `changes_between_refs`; `symbol_history` via `log_for_path` | `repo.rs::diff_refs/diff_indices/symbol_history_in_channel` |

### 1.2 Identity stack (the load-bearing bugs)

| Primitive | As-built behavior | Consequence |
|---|---|---|
| `IntroId` = `blake3("nudox.intro.v1" ‖ package ‖ u16le(kind) ‖ segments ‖ name ‖ disambiguator)` | `nudox-ir/intro.rs::bootstrap_intro_id` | Name or path change ⇒ new id (**I1**) |
| Functions: disambiguator = **full signature skeleton, always** | `builder.rs::seal_payloads` (every `KindWire::Function` gets `Disambiguator::Overload(skeleton)`) | Any param/return type change ⇒ new id (**I2**) |
| `seal_payloads` re-bootstraps every generation; nothing persists an assignment | `builder.rs` | "Assigned once, forever-stable" is documented (`nudox-change/hash.rs` IntroId doc, K18) but **not implemented** (**I3**) |
| `payload_hash` = `blake3("nudox.entry.v1" ‖ postcard(symbol, kind_disc, kind, flags))`; includes docs/spans | `wire.rs::compute_payload_hash` + golden pin | Not an API hash; doc edit ⇒ "modified" |
| Type skeleton opcodes frozen with golden vectors; lifetimes excluded | `skeleton.rs` | Reusable as-is for `SigKey` (§4.6) |
| `Disambiguator::Span` stores two u32s widened to u64le×2 | `intro.rs` | Harmless quirk; v2 keeps u64le×2 for uniformity |

**[fix] New as-built gap (I4):** re-export **targets are lost** in the VCS store. The blob
records only a bare `ref` line when `EntryPayloadFlags::IS_REFERENCE` is set (`blob.rs:471`);
`ReferencePayload{target}` exists in `nudox-ir/wire.rs` but is carried neither in
`OwnedEntryPayload` nor in the blob. Pack C is impossible until §6.2's `retgt` key and §8.2's
`Reexport` kind land.

### 1.3 IR kinds today vs. needed

As-built `KindDiscriminant`: `Module=1 Record=2 Field=3 Function=4 Type=5` — far too thin for
CSC parity (no traits, impls, enums, variants, consts, statics, re-exports, generics, attrs).
§8 defines the target schema. Numbers 1–5 stay frozen; new kinds take 6+.

### 1.4 libpijul interior facts (verified in the pinned source)

| Fact | Citation | Why it matters here |
|---|---|---|
| Record seam: `Recorded::record_nondeleted` → stat-cache check → `working_copy.decode_file` → `Recorded::diff` | `record.rs` (~1214–1247) | The exact injection point for Depth-1 structured record (§11) |
| Encoding detection = `chardetng` guess + **strict round-trip** (`decode` then `encode` must reproduce the input bytes) | `working_copy/mod.rs::decode_file`, `lib.rs::get_valid_encoding` | Printable-ASCII/UTF-8 content **always** lands on the text path; arbitrary binary values do not (§6.4 proof) |
| If `encoding.is_none()` ⇒ **binary path**: 8192-byte rolling-hash chunks, no lines | `diff/mod.rs::Recorded::diff`, `diff/bin.rs` (`ROLLING_SIZE = 8192`) | The "binary footgun" is real; F1 must be ASCII-armored |
| Line split = regex separator, `DEFAULT_SEPARATOR = "\n"`; a "line" is any chunk between matches | `diff/mod.rs`, `diff/split.rs` | One F1 frame per `\n` line ⇒ pijul vertices ≈ fields |
| Diff algorithms: `Myers` (default), `Patience`, `ImaraHistogram` | `diff/diff.rs::Algorithm` | Canonical F1 ordering makes Myers behave like keyed set-diff (§6.3) |
| Atoms: `Atom::{NewVertex, EdgeMap}`; hunks: `FileMove/FileDel/FileUndel/FileAdd/SolveNameConflict/UnsolveNameConflict/Edit/Replacement/SolveOrderConflict/UnsolveOrderConflict/ResurrectZombies`; `Edit/Replacement` carry `local` (path+line) and per-hunk `encoding` | `change.rs:74–124`, `change/noenc.rs::Hunk` | Depth-1 emits only `Edit`/`Replacement` with existing atoms; no new atom kinds |
| `Hashed { version: 6, header, dependencies, extra_known, **metadata: Vec<u8>**, changes, contents_hash }` + **unhashed** `Option<serde_json::Value>` per change | `change.rs:151–198` | **[fix]** Rev 1 assumed vendoring was needed to attach app metadata. Wrong: `Change::make_change(txn, channel, changes, contents, header, metadata)` already takes it (`change.rs:1363`); as-built passes `Vec::new()` (`repo.rs` step 6). GenerationMeta + delta digest land at **Depth 0** with zero forking (§7.6) |
| New-file add = one whole-content `NewVertex`; later edits split vertices at edit offsets | `record.rs::add_file` | Initial single-vertex files are fine; field grain emerges on first edit |
| `output_repository_no_pending` returns `(Vec<Conflict>, Vec<Redundant>)` | `output/output.rs:181`, `output/output.rs:19` | Conflict detection at materialize is a return value, not a marker-scan (§12.3) |
| Stat cache truncation: file re-diffed iff mtime ≥ channel-last-modified truncated to seconds | `record.rs::modified_since_last_commit` | Preserve the as-built mtime-floor clamp when porting the session (§5.7) |

### 1.5 Single-writer reality

Each package channel has exactly one writer (the host recording pipeline; `nudox-sync`
replicates to the trusted remote). Pijul's commutation is still load-bearing for branch/merge
workflows and unrecord, but v1 **asserts** the single-writer invariant per channel; concurrent
same-channel writers are a protocol violation (§12.4).

---

## 2. Comparative models (what we take, what we refuse)

### 2.1 Spectrum

```text
Text VCS          Structural view        Structural store         Content-addressed code
(git, pijul raw)  (difftastic)           (AST patches)            (Unison)
```

| System | Identity | Diff unit | Merge/commute | Multi-lang | Verdict for nudox |
|---|---|---|---|---|---|
| Git | path + blob | lines | 3-way text | yes | Wrong algebra for independent symbols |
| Pijul (stock) | path + vertex graph | line chunks | true commute | as bytes | **Keep as engine**; feed it canonical lines |
| Difftastic / SemanticDiff | n/a (viewers) | AST | none | many | UI oracles only |
| GumTree / RefactoringMiner | heuristics | AST ops | research | per-lang | Continuity-matcher eval oracles only |
| SCIP / LSIF | monikers | n/a | n/a | multi | Nav; moniker ≠ continuity |
| Unison | hash(normalized AST) | term replace | hash breakage | one lang | Steal: names-as-metadata, types-break-dependents. Refuse: single-language CAS ecosystem |
| cargo-semver-checks | rustdoc item paths → Trustfall lints | lint queries | n/a | Rust | **Correctness benchmark**, eval-only |
| TerminusDB | doc layers | semantic JSON patch | branch | schema | Hot graph tier complement (§13) |
| **nudox target** | IntroId + F1 fields | IrOps / field lines | pijul + canonical order | multi via IR | This plan |

### 2.2 cargo-semver-checks — architecture and holes

**How it works:** rustdoc JSON ×2 → Trustfall queries → discrete lints; design goal ≈0 false
positives; optional witness programs; incomplete lint set ⇒ false negatives by design.

| CSC hole | Our attack |
|---|---|
| Breaking **type** changes (param/field/return) — the "final boss" | Pack B (§9.5): typed IR compares nominal refs + structural shells directly |
| **Generics / lifetimes / bounds** | Pack B: producer-lowered `gparam`/`where` facts |
| **Feature subset** breaks (heuristic feature sets) | Pack D (§9.7): bounded config enumeration + per-config surfaces |
| **Cross-crate re-exports** (their top historical FP source, and FN source) | Pack C (§9.6): `retgt` + `StableRef` + hermetic dep surfaces |
| Behavioral breaks (same signature) | **Neither we nor CSC claim this.** Non-goal |

**CSC strengths to match before claiming anything:** item presence/visibility, `doc(hidden)`,
struct/enum/trait shape lints, sealed-trait sophistication, `#[must_use]`/`#[deprecated]`,
zero-FP culture, precise pointers, witnesses on hard lints. That is Pack A and the §10 gate.

### 2.3 Why "text-encoded pijul" (as-built) is not enough

Works today: independent symbols = independent files = commute; symbol delete = `FileDel`;
per-symbol history = `log_for_path`. Fails today: param-type change as a typed op; rename
continuity (I1); function identity under signature change (I2); any API law (Into-vs-trait
-vs-sealed); re-export semantics (I4); cross-file impact; CSC-class verdicts.

---

## 3. Normative principles

### 3.1 All product classification on IR (hard rule, K-IR-Only-Semver)

```text
source ──producer──► IR generation (VCS tip, host-owned identity)
                         │
         ┌───────────────┼───────────────────┐
         ▼               ▼                   ▼
   continuity        structural Δ         ApiReport
   (matcher, §5)     (PackageDelta, §7)   (nudox-semver, §9)
```

- Product path: `ApiReport = classify(surface(T0), surface(T1), policy, deps)` with I/O only
  to sealed IR stores.
- **Forbidden on the product path:** rustdoc JSON baselines, shelling out to CSC, re-parsing
  source, invoking rustc/RA at classify time.
- **Allowed at producer lower time only:** rustc/RA/trait-solver probes that **write facts
  into IR** (sealed lists, dyn-compat flags, auto-trait facts, sealed evidence).
- **Eval harness only:** CSC + compile witnesses to *score* the classifier (§10). Never
  required for a user-facing report.
- If a verdict cannot be decided from IR facts ⇒ emit `Uncertain(reason)` (§9.3). Never guess.

### 3.2 One IR, two projections

| Projection | Definition | Use |
|---|---|---|
| Full IR | All live entries, docs, spans, links, internals | lineage, search, graph |
| `ApiSurface(IR, ExportPolicy)` | Reachable exported items + semver-relevant fields | all lint packs |

`ApiSurface` is a pure function (§8.4) — never a second artifact pipeline.

### 3.3 Hash classes and per-consumer fan-out gating

Three hashes, three consumers. **[fix]** Rev 1 said "doc-only ⇒ no re-embed", contradicting
its own `embed_hash` definition (docs are the primary embedding signal). Resolved: each
consumer gates on exactly its own hash class.

| Hash | Domain | Preimage (canonical) | Gates |
|---|---|---|---|
| `payload_hash` | `nudox.entry.v2` | postcard of the full v2 payload tuple (§8.3) | exact identity, dedup, `VersionDiff.modified` |
| `api_surface_hash` | `nudox.apisurface.v1` | the F1 frames whose key is marked **S** in §6.2, in canonical order, **excluding** `name`/`parent` (moniker-class) | semver classify skip, graph rewrite skip, registry invalidation |
| `embed_hash` | `nudox.embed.v1` | `name` ‖ moniker path ‖ `doc` paragraphs ‖ rendered signature (`in`/`out`/`fieldty`/`type` lines) | re-embedding |

Doc-only change ⇒ `payload_hash` moves, `embed_hash` moves (re-embed **fires**),
`api_surface_hash` still (semver/graph fan-out **skipped**). Acceptance test V-1 (§16.2).

### 3.4 No dual source of truth (K-No-Dual-SoT)

Authoritative order: **files → tables → surfaces → report**. Everything else is a cache of
that chain:

1. `classify(surface(T0), surface(T1))` is the authoritative report.
2. `PackageDelta` MUST be a lossless projection: `apply_delta(surface(T0), delta) ==
   surface(T1)` (round-trip law, tested per corpus pair, §7.5).
3. The delta digest stored in change metadata (§7.6) is advisory; a `verify` mode recomputes
   from atoms and compares. Drift = bug, tables win.

### 3.5 Cost asymmetry (lineage)

Falsely merging two unrelated symbols poisons lineage, embeddings, and dependents' StableRefs
— far worse than a false split (one delete+add of noise). Auto-continuity only at **High**
evidence (§5.4); everything else is a Soft rename-edge for UI, or a clean add/delete.

### 3.6 Explicit non-goals

- Behavioral semver with identical signatures. — Neither claimed nor reported.
- Replacing pijul, or a second apply engine (Depth −1 stays rejected).
- Tree-sitter CSTs as VCS units.
- Unison-style whole-ecosystem CAS source.
- Automatic split/merge of one symbol into many (Soft edges only).
- Counting multi-language coverage toward the CSC scoreboard (§10.6).
- MSRV/edition/toolchain-bump detection (report field reserved, always `Unchecked` in v1).

---

## 4. Identity v2 (prerequisite for everything)

### 4.1 Three names, three jobs

```text
IntroId  = durable entity id. File name in the WC, lineage key, StableRef component.
Moniker  = (segments root→parent, name, kind) at a generation — display/import path. Data, not identity.
SigKey   = hash of the function-signature skeleton — overload talk, evolution detection. NOT identity.
```

### 4.2 Wire-id vs durable-id (the C1 policy, made precise)

- The **producer** computes `bootstrap_intro_id_v2` for every entry (it has no history). The
  result is the entry's **wire id**.
- The **host** owns durable identity: at `finish()` it maps wire ids to durable ids. For most
  entries wire id == durable id (determinism, §4.3). For entries whose wire id is new while a
  plausibly-same entry died, the continuity matcher (§5) may **reuse the old durable id**.
- Everything durable (file names, links, StableRefs, dependents' type refs) speaks durable
  ids only. Wire ids never escape the recording session.

### 4.3 `bootstrap_intro_id_v2` (frozen)

Domain `nudox.intro.v2`. Preimage (byte-exact, reusing `nudox-change::encode`):

```text
package.encode()                 // encode_str(ecosystem) || encode_str(name)
|| u16le(kind_disc)              // frozen KindDiscriminant (§8.2)
|| encode_segments(segments)     // u32le(count) || encode_str(seg)…   root → parent
|| encode_str(name)              // leaf declaration name
|| disambiguator_v2              // see below
```

`disambiguator_v2` (**the I2 fix — collision-scoped, not always-on**):

| Case | Bytes |
|---|---|
| `(package, kind, segments, name)` unique within this generation | empty |
| ≥2 same-key **functions** (true overloads: C#, Java, C++ producers) | `0x01` ‖ `function_signature_skeleton(inputs, outputs)` (as-built `skeleton.rs`, unchanged opcodes) |
| ≥2 same-key non-functions (anonymous types, impls at same path) | `0x02` ‖ `u64le(span.start)` ‖ `u64le(span.end)` |

Determinism: the collision check is computed from **this generation alone** (count entries
sharing the tuple), so two independent producers of the same source agree byte-for-byte. No
positional overload indices — Rev 1's "deterministic overload index" is **rejected**: deleting
an earlier overload would shift later indices and churn identity exactly where stability is
wanted.

Two direct consequences the implementer MUST internalize:

1. **Unique-named functions have signature-stable wire ids.** A param-type change on a
   non-overloaded function keeps its id with **no matcher involvement**. This alone deletes
   the bulk of Rev-0's false delete+add noise.
2. **Overload-set churn flips disambiguators.** Adding a second overload of `f` changes the
   first `f`'s disambiguator (empty → skeleton) ⇒ its wire id changes ⇒ the matcher sees a
   delete+add with *identical payload* and reunifies at High (test C-6, §16.1). Same on
   dropping back to one overload.

`TraitImpl` entries have no meaningful `name`: their name slot is the canonical string
`impl` and they always disambiguate — by skeleton of `(trait_ref, self_ty)` encoded as a
2-tuple type skeleton (`0x03` ‖ skeleton bytes). Frozen.

### 4.4 Resurrection

Bootstrap determinism means a symbol deleted in gen N and re-added identically in gen N+k
gets the **same id** automatically. The session detects `id ∉ T0 ∧ symbol_history(id) ≠ ∅`
and logs `IrOp::Resurrected`. Semver-wise a resurrection is an addition. Pijul-wise it is a
fresh `FileAdd` at the same path (fine; `log_for_path` stitches history).

### 4.5 Kind changes never merge

`kind` is in the preimage, so `struct Foo` → `enum Foo` is delete+add at the wire level. The
matcher **gates on kind equality** (§5.4) and MUST NOT High-merge across kinds; at most a
Soft edge. Semver: the moniker survived but the item was replaced ⇒ Pack A lint
`item-kind-changed` = Major.

### 4.6 SigKey (frozen)

`SigKey = blake3("nudox.sigkey.v1" ‖ function_signature_skeleton(inputs, outputs) ‖ 0xFE ‖
fnsig_flag_bytes)` where `fnsig_flag_bytes` is the canonical encoding of the `fnsig` frame
value (§6.2: self-kind, async, const, unsafe, abi, variadic, defaulted). Not stored in the
file (derived); computed by surface projection and the matcher.

---

## 5. Continuity matcher and the two-phase recording session

This section replaces `RecordingSession` semantics. It exists because of one non-obvious
cascade Rev 1 missed entirely:

> **The substitution cascade.** If the host decides staged entry `a` (wire id `w`) *is*
> deleted entry with durable id `d`, then every **reference to `w` anywhere in the staged
> generation** — parent edges, `recfield` lists, `TypeRefWire::Same` refs inside params /
> fields / type exprs / `retgt`, link endpoints — MUST be rewritten `w → d`, and every
> payload so touched MUST be **re-sealed** (its `payload_hash` covers those refs). Identity
> decisions therefore cannot be finalized until the whole generation is staged.

### 5.1 Session shape (two-phase)

```text
begin_recording(&mut repo) -> RecordingSession
  Phase A (streaming, cheap):
    .stage(batch)          // validate SV-10, accumulate into in-memory StagingTable (wire ids)
    .stage_links(from, l)  // merge into StagingTable
    .checkpoint(msg)       // OPTIONAL durability aid — see §5.6, provisional identity
  Phase B (finish, authoritative):
    .finish() ->
       1. deletions D = tip_ids − staged_ids ; additions A = staged_ids − tip_ids
       2. σ = match(D, A)                       // §5.3–§5.5, deterministic
       3. substitute σ over the StagingTable    // refs + parents + links, then re-seal payloads
       4. write WC: upsert changed files (skip-identical, mtime floor), remove D − dom(σ)
       5. record one libpijul change; metadata = GenerationMeta + delta digest (§7.6)
       6. compute PackageDelta vs T0; return FinishReport{tip, change, delta, continuity}
  .abandon()               // unchanged from as-built: reset WC tip, next open resyncs
```

`record_generation(&table)` remains as the convenience wrapper: begin → one stage of the
whole table → finish.

**Memory model.** `begin_recording` materializes the tip once into `tip_table:
PristineIntroTable` (the matcher needs T0 payloads, not just `tip_intros` — Rev 1's "already
needed for deletions" undersold this). The StagingTable holds the full new generation.
Budget: ~1 KB/symbol ⇒ ~100 MB for a 100k-symbol package; acceptable v1, documented. If a
package ever busts it, spill the StagingTable to a temp file keyed by wire id — an
implementation detail behind the same API, do not redesign for it now.

### 5.2 Matcher inputs

```text
D: BTreeMap<IntroId, &OwnedEntryPayload>   // deleted at tip, sorted by id bytes
A: BTreeMap<IntroId, &StagedEntry>          // added wire ids, sorted by id bytes
ctx: parent chains on both sides; symbol_history access for resurrection tagging
```

All iteration in this section is over **sorted** maps. No `HashMap` iteration order may
influence any decision (determinism is a correctness property: replicas re-deriving a
generation must produce identical σ, or ids diverge across replicas).

### 5.3 Candidate generation (bounded)

For each `a ∈ A` (ascending id), collect candidates `d ∈ D` from three indexes, then dedupe
and sort by `d` id:

1. same `(kind, name)`;
2. same `api_surface_hash` (§3.3 — name/parent excluded, so rename and move candidates
   surface here);
3. same `(kind, resolved-parent, name-stem)` where name-stem is the exact name (no fuzzy
   string distance in v1 — determinism and FP discipline beat cleverness).

Cap: 64 candidates per `a`, keeping the lowest `d` ids beyond the cap (deterministic prune;
in practice buckets are tiny).

### 5.4 Scoring (weights frozen, integer-only)

Hard gate: `kind_disc(a) == kind_disc(d)`, else the pair does not exist.

| Feature | Points | Notes |
|---|---|---|
| `api_surface_hash` equal | +50 | shape identical (name/parent excluded by construction) |
| name equal | +20 | the move case |
| parent continuity | +15 | parent of `a` maps (via σ-so-far or identity) to parent of `d` — see ordering rule below |
| `SigKey` equal | +15 | functions only |
| doc bytes equal and nonempty | +10 | |
| `src` path equal | +5 | |
| alias overlap ≥1 | +5 | |

Thresholds (frozen): **High ≥ 60**, **Soft ≥ 30**.

Special rule **R-OV (overload flip)**: if within bucket `(kind, parent, name)` exactly one
`d` and exactly one `a` exist, the pair is High regardless of score. (Covers signature
evolution of an overloaded function, where shape+sig+doc may all have changed but the flip is
unambiguous.)

Guard **R-CHILD**: for kinds `Field` and `Variant`, High additionally REQUIRES parent
continuity (a same-named same-typed field must not migrate between two structs — cost
asymmetry). Functions/records/types may High-match across parents (module moves are real).

**Ordering rule for parent continuity:** process matching in ascending **tree depth** of `a`
(roots first), so a parent's σ entry exists before its children are scored. Depth =
length of the staged parent chain.

### 5.5 Assignment (deterministic greedy)

```text
pairs = all (d, a, score) with score ≥ Soft, sorted by (score DESC, d id ASC, a id ASC)
for (d, a, s) in pairs:
    if d unmatched and a unmatched and s ≥ High:
        runner-up rule: if another unmatched candidate for the same a (or same d)
        also scores ≥ High and within 15 points → skip both into Soft (ambiguous)
        else: σ[a.wire_id] = d ; emit continuity ops for the pair
remaining pairs with Soft ≤ s < High (or ambiguous) → RenameEdge{d, a, score} advisory,
    persisted in FinishReport only (GUI hints). Never σ.
unmatched a → Introduced (or Resurrected, §4.4); unmatched d → Deleted
```

Continuity ops emitted per matched pair by comparing the two payloads:
`Renamed{old_name, new_name}` if names differ; `Moved{old_parent, new_parent}` if parents
differ; `SignatureEvolved{old_sigkey, new_sigkey}` if SigKeys differ. Any combination can
co-occur.

### 5.6 Checkpoints are provisional

`checkpoint(msg)` records the current WC state under **wire ids** with **no deletions and no
σ** — purely a crash-durability aid for long producer runs (SMOLVM SV-9). Consequences,
stated honestly:

- A later `finish()` may re-key entries that a checkpoint already wrote ⇒ within the
  generation's history the provisional path shows a brief add(+delete). Lineage queries by
  durable id are unaffected (different path). Accepted noise, bounded to renamed/re-signed
  symbols.
- Deltas computed against a checkpoint state are labeled `PartialDelta{deletions_valid:
  false, identity_final: false}`. Publishing an `ApiReport` from a checkpoint state is
  forbidden (acceptance V-4). Versions/tags may only be created from finish tips.

### 5.7 Preserve the as-built O(delta) mechanics

Port unchanged into Phase-B WC write: skip-identical-content writes (preserves mtime ⇒ stat
cache skips re-diff), mtime floor `max(now, channel_last_modified)` (backwards-clock guard),
root-flat paths, no manual directory inodes. These carry the size_tests / O(delta) guarantees
(`repo.rs` tests: `record_generation_write_path_is_o_delta`,
`record_generation_survives_backwards_clock_step`, `record_generation_stale_wc_resyncs_once`).

### 5.8 What the matcher explicitly does not do

No cross-package matching. No split/merge (one-to-one only). No fuzzy name distance. No
learning/embedding features (the embedding plane may *suggest* Soft edges in the GUI later;
it never feeds σ).

---

## 6. `NdIrF1` — the canonical working-copy format

### 6.1 Design constraints (all load-bearing, all verified §1.4)

1. One `\n`-terminated line per field frame ⇒ pijul's separator split aligns vertices to
   fields.
2. Bytes MUST always take libpijul's **text** path ⇒ printable UTF-8 only; the escaping
   below guarantees it (proof §6.4). **[fix]** Rev 1's "postcard value bytes" would have
   produced `\n` and non-UTF-8 inside frames — binary path, format dead. Rejected.
3. Deterministic canonical serialization: same table ⇒ same bytes, byte-for-byte, on every
   producer/replica (change-hash reproducibility is the multi-producer verification story).
4. Minimal diffs: a single field edit touches exactly one line; a param insert inserts
   exactly one line. **[fix]** Rev 1's positional aux indices (`in_param(i)`) are rejected —
   inserting a param would rewrite every following param line. Sequences carry **no index**;
   order is line order.
5. Debuggable by `cat`.

### 6.2 Frozen key registry

File layout: magic line, then frames grouped by key **in exactly this table order**; within a
set-class key, lines sorted ascending by raw line bytes; within a sequence-class key,
declaration order. A frame is `key\tvalue…` (TAB-separated fields inside the value part);
flag-like frames may be bare (`key` alone). Escaping per §6.3.

Class: **1** scalar (0..1 line) · **set** (0..n, sorted, unordered semantics) · **seq**
(0..n, ordered semantics). S = in `api_surface_hash`; E = in `embed_hash`.

| # | Key | Class | Value grammar (after the key TAB) | S | E | Applies to |
|---|---|---|---|---|---|---|
| 0 | `NdIrF1` | magic | `1` (format version) | – | – | first line, mandatory |
| 1 | `name` | 1 | esc-str | – | E | all |
| 2 | `vis` | 1 | `public\|private\|protected\|internal\|package\|crate` | S | – | all |
| 3 | `kind` | 1 | kind token (§8.2) | S | – | all |
| 4 | `span` | 1 | `u32<TAB>u32` | – | – | all |
| 5 | `src` | 1 | esc-str | – | – | optional |
| 6 | `parent` | 1 | 64-hex durable IntroId | – | – | non-roots (moniker-class: excluded from S by §3.3/§5.4 rationale) |
| 7 | `cfg` | 1 | predicate expr (§8.6) | S | – | optional |
| 8 | `attr` | set | `token[<TAB>esc-str]` — `non_exhaustive`, `must_use`, `doc_hidden`, `repr<TAB>C`, `no_mangle`, `track_caller`, … (§8.5) | S | – | optional |
| 9 | `deprecated` | 1 | `esc-str<TAB>esc-str` (since, note; empty allowed) | S | – | optional |
| 10 | `alias` | set | esc-str | – | E | optional |
| 11 | `doc` | seq | esc-str — **one paragraph per line** (split on blank-line boundaries at producer; rejoin with `\n\n` on read) | – | E | optional |
| 12 | `dlink` | set | stable-ref`<TAB>`esc-str (target, label) | – | – | optional |
| 13 | `retgt` | 1 | stable-ref | S | – | `reexport` kind (mandatory there) — **fixes I4** |
| 14 | `fnsig` | 1 | tokens in fixed order, TAB-separated: `self:<none\|value\|ref\|refmut\|arb:typeref>` then optional `async` `const` `unsafe` `abi:<esc-str>` `variadic` `defaulted` | S | – | functions (mandatory there) |
| 15 | `gparam` | seq | `life:<esc-str>` \| `type:<esc-str>[<TAB>bounds:<bound-list>][<TAB>default:<typeexpr>]` \| `const:<esc-str><TAB><typeref>[<TAB>default:<esc-str>]` | S | – | functions, records, enums, traits, impls, type aliases |
| 16 | `where` | set | `<typeexpr><TAB><bound-list>` | S | – | same |
| 17 | `in` | seq | `<esc-str-or-empty><TAB><typeref>` (param name may be empty) | S | E | functions |
| 18 | `out` | seq | `<esc-str-or-empty><TAB><typeref>` | S | E | functions |
| 19 | `fieldty` | 1 | typeref | S | E | fields |
| 20 | `recform` | 1 | `struct\|tuple\|unit\|union` | S | – | records |
| 21 | `recfield` | seq | 64-hex durable IntroId | S | – | records (field order) |
| 22 | `vform` | 1 | `unit\|tuple\|struct` | S | – | variants |
| 23 | `vdiscr` | 1 | esc-str (literal text) | S | – | variants, optional |
| 24 | `super` | set | typeref | S | – | traits |
| 25 | `tflags` | 1 | tokens: `auto?` `unsafe?` `dyn:<yes\|no\|unk>` `sealed:<none\|pubapi\|full>` | S | – | traits (mandatory there) |
| 26 | `iof` | 1 | typeref (implemented trait) | S | – | trait impls |
| 27 | `ifor` | 1 | typeexpr (self type) | S | – | impls (mandatory) |
| 28 | `iflags` | 1 | tokens: `negative?` `blanket?` | S | – | impls |
| 29 | `cty` | 1 | typeref | S | – | consts/statics (mandatory there) |
| 30 | `cval` | 1 | esc-str (rendered value text) | – | – | consts, optional |
| 31 | `auto` | set | `<trait-token>:<yes\|no\|cond>` — `send\|sync\|unpin\|unwindsafe\|refunwindsafe` | S | – | records/enums/types (§8.7 phasing) |
| 32 | `type` | 1 | typeexpr | S | E | type aliases |
| 33 | `link` | set | `<eco><TAB><pkg><TAB><64-hex><TAB><kind_self><TAB><kind_other>` | – | – | canonical owner only (§1.1 rule unchanged) |
| 34 | `lfact` | set | `<token>[<TAB>esc-str]` — **ecosystem-scoped** language facts not (yet) worth a core key (e.g. `readonly`, `final`, `slots`, `checked`) | S | – | optional |

`typeref`, `typeexpr`, `stable-ref` reuse the as-built ASCII encodings from `blob.rs`
(`S:<hex>` / `F:<eco>/<pkg>#<hex>`, `prim:int:s:32`, `tuple:…`) — they are already
ASCII-pure and frozen. `bound-list` = `+`-joined typerefs/paths, canonically sorted by raw
bytes. New keys get new numbers; keys are never renamed or renumbered.

**Ecosystem-scoped token namespaces.** The registry itself (keys, classes, S/E columns) is
language-neutral and frozen; the *token vocabularies* inside `attr`, `fnsig` (`abi:` …),
`tflags`, `auto`, `cfg`, and `lfact` are per-ecosystem registries owned by that language's
producer + law pack. The channel's `PackageLineageId.ecosystem` scopes them — a channel is
always single-ecosystem, so tokens never collide across languages. The vocabularies listed
in this table are the **Rust reference instances**. A pack MUST freeze its tokens under the
same discipline (never rename/reuse); tokens are opaque strings to the store, so an unknown
token parses fine, hashes fine (`lfact` is S — false cache invalidation is cheap, false
preservation is wrong), and merely makes pack-less verdicts Uncertain (§9.0).

Notes:

- **No `hash` line.** **[fix]** As-built stores `payload_hash` in the blob; F1 drops it —
  it is derivable, and storing it doubles every single-field diff (field line + hash line).
  Materialize recomputes and the golden pins keep the derivation honest.
- **No `ref` bare flag.** Replaced by `kind reexport` + `retgt` (the flag without a target
  was I4).
- `doc` as paragraph sequence means a one-paragraph doc edit is a one-line diff even for
  huge docs.

### 6.3 Escaping (frozen; superset of as-built)

Inside any `esc-str`: `\` → `\\`, TAB → `\t`, LF → `\n`, CR → `\r`, and any other C0 control
byte → `\xNN` (two lowercase hex digits). Everything else is raw UTF-8. Decode rejects
unknown escapes and a trailing `\` (as-built `unescape` behavior, extended with `\r`/`\xNN`).

### 6.4 Why this can never hit the binary path (proof sketch, cite §1.4)

The encoded file is valid UTF-8 whose only control byte is `\n`. `decode_file` feeds
chardetng and accepts the guess only if decode→re-encode reproduces the bytes exactly. For
valid UTF-8 the UTF-8 guess round-trips; for pure-ASCII content any single-byte guess
(windows-1252 etc.) also round-trips. ISO-2022-style guesses require ESC (0x1B) which §6.3
forbids raw. Hence `encoding` is always `Some(_)` and `Recorded::diff` takes the line path.
Depth 0.5 therefore needs **no fork** — for real this time. (Depth 1 additionally pins
`Some(UTF_8)` for `.nir` paths as belt-and-suspenders, §11.3.)

### 6.5 Canonicalization algorithm (serializer contract)

```text
serialize(payload, parent, links):
 1. magic line "NdIrF1\t1\n"
 2. for key in registry order 1..33:
      scalar → 0..1 line, exactly the canonical rendering (no alternate spellings:
               lowercase hex, no leading zeros trimmed, fixed token order in fnsig/tflags/iflags)
      set    → render each element, sort lines ascending by raw bytes, emit
      seq    → emit in semantic order (declaration order; doc paragraph order)
 3. every line ends with "\n"; file ends with "\n" (no trailing partial line)
parse: strict inverse; unknown key ⇒ error (greenfield: no lenient skip);
       wrong section order / unsorted set ⇒ error (canonical form is the only valid form)
```

Strictness is deliberate: a canonical-form violation can only come from a bug or a corrupted
store, and silent tolerance would fork the "same table ⇒ same bytes" invariant that change-
hash reproducibility rests on.

### 6.6 Diff shape you get for free (design intuition, tested in §16.2)

With canonical ordering + Myers-on-lines: a scalar edit = 1-line replace; a set add/remove =
1-line insert/delete at its sort position; a param insert = 1-line insert (later params
untouched); a param reorder = small move (delete+insert) that the delta projector folds into
`ParamsReordered` by multiset equality. Cross-key misalignment is impossible in practice
because every line begins with its key token; two lines of different keys are never equal, so
Myers cannot pair them.

---

## 7. Structural delta: `IrOp` / `PackageDelta` (crate `nudox-ir-diff`, MIT)

### 7.1 Position in the pipeline

Diff-time is **matcher-free**: continuity was already decided at record time and is baked
into ids. `diff_tables(T0, T1)` compares by durable id only. (Rev 1 was ambiguous here.)

### 7.2 Types (normative shape)

```rust
pub struct PackageDelta {
    pub from: ChangeSetFingerprint,
    pub to:   ChangeSetFingerprint,
    pub ops:  BTreeMap<IntroId, Vec<IrOp>>,   // sorted; ops within an id in the §7.3 order
    pub partial: Option<PartialDelta>,        // §5.6; None for finish-to-finish deltas
}
pub struct PartialDelta { pub deletions_valid: bool, pub identity_final: bool }

pub enum IrOp {
    // lifecycle
    Introduced, Deleted, Resurrected,
    // continuity (recorded at finish; replayed here from payload comparison)
    Renamed { old: SmolStr, new: SmolStr },
    Moved { old_parent: Option<IntroId>, new_parent: Option<IntroId> },
    SignatureEvolved { old: SigKey, new: SigKey },
    // meta
    VisChanged { old: Visibility, new: Visibility },
    DocChanged,                         // content in the file; op is the signal
    DeprecationChanged { added: bool },
    AliasesChanged { added: Vec<SmolStr>, removed: Vec<SmolStr> },
    SpanChanged, SourcePathChanged,
    CfgChanged { old: Option<CfgExpr>, new: Option<CfgExpr> },
    AttrsChanged { added: Vec<AttrTok>, removed: Vec<AttrTok> },
    // functions
    ParamAdded { index: u16 }, ParamRemoved { index: u16 },
    ParamRenamed { index: u16 }, ParamTypeChanged { index: u16, old: TypeRefWire, new: TypeRefWire },
    ParamsReordered, ReturnChanged { old: Vec<TypeRefWire>, new: Vec<TypeRefWire> },
    FnSigFlagsChanged { old: FnSigFlags, new: FnSigFlags },
    GenericsChanged { detail: GenericsDelta },   // added/removed/bounds-tightened/loosened/default-added…
    WhereChanged { added: Vec<WherePred>, removed: Vec<WherePred> },
    // records / enums
    FieldTypeChanged { old: Option<TypeRefWire>, new: Option<TypeRefWire> },
    RecFormChanged, FieldsReordered,
    ChildAdded { child: IntroId }, ChildRemoved { child: IntroId },  // recfield/variant order lists
    VariantFormChanged, VariantDiscrChanged,
    // traits / impls
    SupertraitsChanged { added: Vec<TypeRefWire>, removed: Vec<TypeRefWire> },
    TraitFlagsChanged { old: TraitFlags, new: TraitFlags },     // auto/unsafe/dyn/sealed
    ImplHeaderChanged,
    // consts / statics / aliases
    ConstTypeChanged, ConstValueChanged, TypeExprChanged { /* old/new skeleton digests */ },
    // reexports
    ReexportRetargeted { old: StableRef, new: StableRef },
    // links (graph tier)
    LinkAdded { key: LinkDomainKey }, LinkRemoved { key: LinkDomainKey },
    // probes
    AutoTraitsChanged { changed: Vec<(AutoTrait, TriState, TriState)> },
}
```

### 7.3 Projection algorithm

```text
diff_tables(T0, T1):
  for id in sorted(T0.ids ∪ T1.ids):
    (None, Some) → Introduced  (Resurrected if history says so — flag passed in from session
                                for live deltas; historical replays may skip resurrection tagging)
    (Some, None) → Deleted
    (Some(p0), Some(p1)) where p0.payload_hash == p1.payload_hash and links equal → skip
    else → field-compare p0 vs p1 key by key (registry §6.2), emitting the ops above:
      scalars: != ⇒ the corresponding op
      sets: sorted-merge ⇒ added/removed
      seqs (params, recfield): LCS on element identity —
        params: identity = (name, typeref); equal multiset but different order ⇒ *Reordered;
        emit Added/Removed/TypeChanged/Renamed by pairing positions via LCS alignment
  ops sorted: lifecycle < continuity < meta < kind-specific < links (stable render order)
```

### 7.4 Cheap tier stays

`VersionDiff{added, removed, modified}` (intro sets + payload-hash compare) remains as the
O(compare) summary; `diff_refs_structured` is the new full projector. Server exposes both;
GUI uses the structured one.

### 7.5 Round-trip law (CI golden, K-No-Dual-SoT)

For every corpus pair and every VCS acceptance fixture:
`apply_delta(surface(T0), delta_surface_subset) == surface(T1)` and
`classify_from_ops(delta) == classify(surface(T0), surface(T1))` for the lint subset that is
ops-derivable. Any divergence fails CI; surfaces win.

### 7.6 Change metadata (lands at Depth 0 — no fork)

`make_change(…, metadata)` (§1.4) carries, postcard-encoded under domain `nudox.genmeta.v1`:

```rust
pub struct GenerationMeta {
    pub schema_epoch: u16,            // 2
    pub config: ConfigId,             // §9.7
    pub producer: ProducerId,         // from the stream Hello
    pub job: JobKey,                  // sealed job identity
    pub delta_digest: ContentBlake3,  // blake3("nudox.delta.v1" ‖ canonical PackageDelta bytes)
    pub partial: bool,                // checkpoint (true) vs finish (false)
}
```

Metadata is inside the change hash (field of `Hashed`) — good: peers verify it for free.
It is advisory relative to atoms (§3.4); `verify` recomputes.

---

## 8. IR completeness: the ApiSurface schema

The schema below is language-neutral in its bones (kinds, visibility, parent/child shape,
type refs) and Rust-complete in its facts — Rust is the **reference instance**, not the
boundary. Other languages reuse the kinds via their lowering contracts (§8.10), fill the
fact frames they can, and extend through ecosystem-scoped tokens (§6.2) — never through
core-schema forks.

### 8.1 What must be representable (reference target — the Rust instance)

Module tree + effective public reachability through re-export chains; structs/enums/unions
with field visibility and `non_exhaustive`; functions/methods with self-kind, async/const/
unsafe/abi/variadic, generics, where-clauses, return types (RPIT phased, §8.8); traits with
items, defaults, supertraits, auto/unsafe, dyn-compat facts, sealed evidence; impls (inherent
/trait/negative/blanket); type aliases; consts/statics; re-exports as first-class entries
with resolved `StableRef`; attrs (`must_use`, `deprecated`, `doc(hidden)`, `repr`, cfg
predicates); feature/target dimensions; auto-trait facts.

### 8.2 Kind discriminants (frozen; 1–5 as-built, 6+ new)

| Disc | Kind token | Wire body |
|---|---|---|
| 1 | `module` | `ModuleWire {}` |
| 2 | `record` | `RecordWire { form, fields: Box<[IntroId]>, generics, wheres }` |
| 3 | `field` | `FieldWire { ty: Option<TypeRefWire> }` |
| 4 | `function` | `FunctionWire { inputs, outputs, sig: FnSigFlags, generics, wheres }` |
| 5 | `type` | `TypeAliasWire { ty: TypeWire, generics, wheres }` |
| 6 | `trait` | `TraitWire { supers: Box<[TypeRefWire]>, flags: TraitFlags, generics, wheres }` — items are child entries |
| 7 | `impl` | `ImplWire { of: Option<TypeRefWire>, self_ty: TypeWire, flags: ImplFlags, generics, wheres }` — assoc items are children |
| 8 | `enum` | `EnumWire { variants: Box<[IntroId]>, generics, wheres }` |
| 9 | `variant` | `VariantWire { form: VariantForm, discr: Option<String>, fields: Box<[IntroId]> }` |
| 10 | `const` | `ConstWire { ty: TypeRefWire, value: Option<String> }` |
| 11 | `static` | `StaticWire { ty: TypeRefWire, mutable: bool }` |
| 12 | `reexport` | `ReexportWire { target: StableRef }` |

Unions are `record` with `recform union`. Trait aliases: v1 lowers as `type` with a marker
attr, and every lint touching them emits Uncertain (rare, honest).

### 8.3 Payload seal v2

`OwnedEntryPayload` gains the new kind bodies and symbol attrs; `payload_hash` domain bumps
to `nudox.entry.v2` (postcard tuple `(symbol, kind_disc, kind, flags)` unchanged in shape).
New golden pin test replaces the v1 pin. `EntryPayloadFlags::IS_REFERENCE` is retired (kind
12 subsumes it); the bit is never reused.

### 8.4 ExportPolicy and reachability (pure, beginner-implementable)

```rust
pub struct ExportPolicy {
    pub include_doc_hidden: bool,      // default false
    pub visibility_floor: Visibility,  // default Public
    pub reexport_depth_limit: u8,      // default 32, cycle-checked
}
```

```text
surface(table, policy) -> ApiSurface:
  1. roots = entries with parent=None (module roots)
  2. BFS: an entry is *exported* iff some path root→entry exists where every hop is
     (a) a child edge whose parent is exported and child.vis ≥ floor and !doc_hidden(child)   or
     (b) a reexport entry that is itself exported, whose retgt resolves to the child
         (same-package: by IntroId; foreign: recorded as a boundary item)
  3. per exported entry collect the moniker set (canonical path + every reexport path)
  4. ApiSurface {
       config: ConfigId,
       items: BTreeMap<IntroId, ApiItem>,       // exported only; ApiItem = kind body view
                                                //  + attrs + vis + sigs + surface_hash
       monikers: BTreeMap<MonikerPath, IntroId>,// import-path → item (reexports included)
       boundary: BTreeMap<MonikerPath, StableRef>, // foreign reexport targets (Pack C food)
     }
```

`doc(hidden)` items are excluded by default but remembered: a `doc_hidden` **toggle** on an
exported item is lint A-11. Trait-impl entries are surfaced when their self-ty and trait are
exported (blanket impls: when the trait is).

### 8.5 Producer probe duties (lower-time only, K-IR-Only-Semver — Rust reference instance)

| Fact | How the producer computes it (RA path) | Stored as |
|---|---|---|
| dyn-compatibility | RA's dyn-compat analysis; reasons discarded, verdict kept | `tflags dyn:yes\|no` (`unk` if analysis unavailable) |
| sealed evidence | §8.9 algorithm | `tflags sealed:none\|pubapi\|full` |
| auto traits | §8.7 phasing | `auto` set |
| cfg predicate | the item's `#[cfg]`/`#[cfg_attr]` condition, normalized (§8.6) | `cfg` |
| attrs | normalized allow-list (`non_exhaustive`, `must_use[=msg]`, `doc_hidden`, `repr <kind>`, `no_mangle`, `track_caller`) — everything else dropped | `attr` set |
| reexport target | name resolution to the defining item; foreign targets via the dep's lockfile identity | `retgt` |

### 8.6 Cfg predicate grammar (normalized, frozen)

`all(…)`, `any(…)`, `not(…)`, `feature=<esc-str>`, `target_os=<esc-str>`,
`target_arch=<esc-str>`, `other:<esc-str>` (opaque, compares by string equality only).
Canonical form: operators lowercase, operands sorted within `all`/`any`, no redundant
nesting. Anything unparseable lowers as `other:` and makes affected Pack-D verdicts
Uncertain rather than wrong.

### 8.7 Auto-trait facts, phased honestly

- **v1:** for **non-generic** exported types the producer solves
  Send/Sync/Unpin/UnwindSafe/RefUnwindSafe concretely → `auto send:yes` etc. For generic
  types it emits `auto send:cond` (conditional-unknown). Lint B-12 fires only on
  `yes → no` transitions; anything involving `cond` is Uncertain. This makes auto-trait
  regressions on concrete types (the common painful case: adding an `Rc` field) a **certain
  Major** while never lying about generics.
- **v2 (deferred):** conditional predicates (`Send iff T: Send`) once the producer can emit
  solver-derived clauses. Do not fake it earlier.

### 8.8 RPIT / async returns, phased

`out` records `impl`-trait returns as `TypeWire::Intersection` of bound typerefs plus an
opaque capture marker (`attr rpit_captures unknown` until producers lower Rust-2024 capture
sets). Return-type lints compare bound sets; capture-sensitive comparisons emit Uncertain
while the marker is `unknown`. `async fn` ⇄ `fn -> impl Future` flips are Uncertain in v1
(auto-trait capture differences are real).

### 8.9 Sealed-trait detection (producer algorithm, frozen semantics)

```text
sealed(trait T):
  full   ⇐ some supertrait S of T is not nameable outside the crate
           (no exported moniker path under the *permissive* policy incl. doc_hidden)
        ∨  some required (non-defaulted) item of T mentions a type not nameable outside
  pubapi ⇐ not full, but every path to S (or to the blocking type) is doc(hidden)
  none   ⇐ otherwise
```

`full` ⇒ downstream impls impossible ⇒ trait-shape changes judged by call sites only.
`pubapi` ⇒ CSC's "public-API sealed": impls possible but off-contract; lints use the
policy's stance (default: treat as sealed, report the nuance in the finding detail).

### 8.10 Per-language lowering contracts

The kind vocabulary (§8.2) is deliberately language-neutral; each producer ships a
**lowering contract** (a doc in the mold of `RA_IMPLEMENTER_BRIEF.md`) that fixes, per
language: (a) the mapping of native declarations onto kinds, (b) which fact frames it fills,
and (c) which facts its law pack requires. Reference mappings:

| Language | Lowers as |
|---|---|
| Rust | this section (reference instance) |
| Java / C# | interface → `trait`; class → `record` + method children; C# property → `field` + accessor `function`s (the contract fixes the choice, both producers must agree with their pack); enum → `enum`; annotations/attributes → `attr`/`lfact`; `final`/`sealed` classes → `lfact` |
| TypeScript | type alias → `type`; interface → `trait` (structural — its pack's laws differ, §9.8 note); namespace → `module`; `readonly` → `lfact` |
| Go | interface → `trait`; struct → `record`; methods → `function` children with the receiver in `fnsig self:`; build tags → `cfg` |
| Python | class → `record` + method children; module attrs → `const`/`field`; extras / interpreter version → Pack-D axes (§9.0 `config_axes`); `__slots__` / dunder protocols → `lfact` |

A lowering contract is normative for its producer: two producers of the same ecosystem MUST
agree byte-for-byte (the determinism invariant §6.1.3 spans producers). Facts a language
cannot supply are simply absent — the ambiguity protocol (§9.3) turns the lints that need
them Uncertain instead of wrong.

---

## 9. `nudox-semver` (MIT): surfaces in, `ApiReport` out

### 9.1 Types (normative shape)

```rust
pub enum BreakClass { Major, Minor, Patch, None }
pub enum Certainty { Certain, Uncertain(UncertainReason) }
pub enum UncertainReason {
    MissingFact(&'static str),      // e.g. "rpit_captures", "auto:cond"
    DepSurfaceUnavailable(PackageLineageId),
    UnsupportedConstruct(&'static str),
    ConflictedState,
    PartialGeneration,
}
pub struct Finding {
    pub lint: LintId,                     // "A-1", "B-3", …
    pub item: Option<IntroId>,
    pub moniker: MonikerPath,
    pub class: BreakClass,
    pub certainty: Certainty,
    pub when: Option<CfgExpr>,            // Pack D attribution
    pub detail: FindingDetail,            // typed old/new payload for rendering + witnesses
}
pub struct ApiReport {
    pub old_state: ChangeSetFingerprint,
    pub new_state: ChangeSetFingerprint,
    pub config: ConfigId,
    pub required_bump: BreakClass,        // max over *Certain* findings only
    pub findings: Vec<Finding>,           // sorted: class desc, lint id, moniker
    pub uncertain: Vec<Finding>,          // Certainty::Uncertain, never raise the bump
    pub dep_closure: DepClosureStatus,    // Complete | Missing(Vec<PackageLineageId>)
    pub toolchain_dimension: Unchecked,   // reserved (§3.6)
}

pub trait DepSurfaceProvider {
    /// Hermetic: resolved from the job's lockfile; same JobKey ⇒ same surfaces.
    fn surface(&self, pkg: &PackageLineageId, pin: &DepPin) -> Result<Arc<ApiSurface>, DepMissing>;
}

pub fn classify(
    old: &ApiSurface, new: &ApiSurface,
    policy: &SemverPolicy, deps: &dyn DepSurfaceProvider,
) -> ApiReport;
```

`SemverPolicy` knobs (all defaulted, all documented): `strict_uncertain: bool` (Uncertain
raises bump — default **false**, CSC-style zero-FP discipline), `transitive_api: bool`
(Pack C4), `treat_pubapi_sealed_as_sealed: bool` (default true).

### 9.2 Engine

An ordered list of pure lint functions over `(old, new)` item pairs joined three ways:
by moniker (presence lints), by IntroId (evolution lints — continuity makes this join
meaningful), and by boundary entries (Pack C). Each lint declares its pack, its class
mapping, and its certainty conditions. No I/O.

### 9.3 Ambiguity protocol (FP discipline)

```text
if structural facts decide it        → emit Certain finding
elif facts exist but law needs care  → apply the §9.8 law table (preconditions!)
elif facts missing                   → emit Uncertain(MissingFact/DepSurfaceUnavailable/…)
never: guess, downgrade silently, or skip without an Uncertain record
```

### 9.4 Pack A — CSC parity (the gate)

Verdicts follow the Cargo book SemVer chapter's classification; each lint's doc-comment
cites the rule name it mirrors.

| Lint | Trigger (old → new, exported items) | Class |
|---|---|---|
| A-1 item-missing | moniker present → absent (all reexport paths gone) | Major |
| A-2 item-kind-changed | same moniker, kind differs (§4.5) | Major |
| A-3 vis-lowered | Public → lower | Major |
| A-4 struct-pub-field-missing | exported field removed/privatized | Major |
| A-5 enum-variant-missing | variant removed | Major |
| A-6 enum-variant-added (exhaustive) | variant added, enum not `non_exhaustive` | Major |
| A-7 enum-variant-added (non_exhaustive) | variant added under `non_exhaustive` | Minor |
| A-8 trait-item-added (required, unsealed) | non-defaulted item added, `sealed:none` | Major |
| A-9 trait-item-added (defaulted or sealed) | otherwise | Minor |
| A-10 trait-item-missing | trait item removed | Major |
| A-11 doc-hidden-toggle | item enters/leaves `doc(hidden)` | leave=Minor, enter=Major |
| A-12 must-use-added | `must_use` appears | Minor |
| A-13 deprecated-toggle | deprecation added/removed | Minor |
| A-14 non-exhaustive-added | `non_exhaustive` appears on struct/enum/variant | Major |
| A-15 fn-const-removed / unsafe-added / abi-changed / self-kind-changed | `fnsig` flag deltas | Major (const-**added**, unsafe-**removed** = Minor) |
| A-16 static-mut-toggle / const-static-swap | kind 10↔11 or `mutable` flip | Major |
| A-17 repr-changed | `repr` attr removed/changed on exported type | Major |
| A-18 struct-all-pub-gains-field | §9.8 law L-5 | Major |
| A-19 dyn-compat-lost | `tflags dyn: yes → no` | Major (gained = Minor) |

Gate (frozen): on corpus C1∪C2, precision ≥ CSC and recall ≥ CSC − 2 findings absolute
(noise allowance for corpus-mapping slop) before any Pack B/C claims are published.

### 9.5 Pack B — types and generics (the recall win)

Type comparison rule (frozen): a type position compares by **nominal identity + structural
shell** — `TypeRefWire::Same(id)` equal iff ids equal (the referenced type's own changes are
its own findings, not this position's); `Foreign(sref)` by StableRef equality; structural
`TypeWire`s recursively. Bound sets compare as canonical sorted sets.

| Lint | Trigger | Class / law |
|---|---|---|
| B-1 fn-param-type-changed | `ParamTypeChanged` on exported fn | matrix: free/inherent → Major; trait method → Major unless sealed → L-1 |
| B-2 fn-return-changed | `ReturnChanged` | Major (RPIT nuances → §8.8 Uncertain paths) |
| B-3 field-type-changed | exported field `fieldty` differs | Major |
| B-4 bound-tightened | generic/where bound set grew (per param, set-superset test) | Major |
| B-5 bound-loosened | bound set shrank | Minor (inference breakage is allowed-minor; note in detail) |
| B-6 generic-param-added | fn: Minor + turbofish-hazard note; type/trait with default: Minor; without default: Major | L-2 |
| B-7 generic-param-removed | any | Major |
| B-8 into-generalization | `T` → `impl Into<T>`/`AsRef`-family at param position | L-3 — the FP trap; see preconditions |
| B-9 lifetime-bound-changed | any lifetime bound delta | Uncertain in v1 (until lifetimes lower fully) |
| B-10 rpit-capture-changed | return `impl` bound sets equal, capture marker differs/unknown | Uncertain until §8.8 v2 |
| B-11 where-clause-added/removed | on exported item | tightened→Major / loosened→Minor (same as B-4/5) |
| B-12 auto-trait-lost | `auto X: yes → no` | Major; `cond` involved → Uncertain (§8.7) |

Every Pack-B lint ships ≥3 positive and ≥3 negative witnesses across the
free-fn/trait-method/sealed matrix (§10.4) **before** it may emit Certain.

### 9.6 Pack C — cross-crate re-exports

Resolution: `retgt` chains followed through `DepSurfaceProvider` surfaces, depth-limited,
cycle-checked. **Disabled** (all findings Uncertain-`DepSurfaceUnavailable`) unless the dep
closure is hermetically pinned.

| Lint | Trigger | Class |
|---|---|---|
| C-1 reexport-drop | boundary moniker present → absent | Major (CSC FN class) |
| C-2 move-behind-reexport | item moved to dep, old moniker re-exported to same shape | **None** (kills the CSC FP class) |
| C-3 reexport-retargeted-shape-changed | same moniker, target surface hash differs | judged by the shape delta (recurse packs A/B on the resolved items) |
| C-4 dep-major-bubble (opt-in `transitive_api`) | re-exported dep item had Major delta between pinned dep versions | Major |

### 9.7 Pack D — feature/target lattice (decision, ending Rev 1's "either/or")

- The **channel** records one canonical config: `all-features` on the host target; if that
  cannot build (mutually-exclusive features), fall back `default`, recorded in
  `GenerationMeta.config`. VCS lineage is single-config — channels never multiply.
- Pack D reads **per-config ApiSurface snapshots**: sealed, content-addressed artifacts
  (`ConfigId = blake3("nudox.config.v1" ‖ sorted features ‖ target triple)`) emitted by the
  producer for the bounded set `{no-default, default, all} ∪ {default+f | f ∈ declared
  features}`. Snapshots are projections, not channels — they live in the archive plane
  keyed by `(package, version, ConfigId)`.
- `classify_lattice` runs pairwise per config and attributes: a finding present under config
  c but not under default gets `when: Some(cfg-diff(c, default))`.
- The powerset is **refused**. |configs| = F + 3. Cfg-predicate implication (symbolic, one
  all-features IR) is the v2 upgrade path once `cfg` frames are trustworthy.

### 9.8 Language-law table (the subtle ones, with preconditions — frozen)

| Law | Statement | Verdict |
|---|---|---|
| **L-1 sealed trait shape change** | `sealed:full` ⇒ no downstream impls ⇒ trait-method sig changes judged by call-compat only | call-compatible → Minor; else Major |
| **L-2 fn generic added** | Adding any generic param to a fn that **had explicit generics** breaks existing turbofish (`E0107`/arity); adding an APIT (`impl Trait` arg) to such a fn forbids turbofish entirely (`E0632`) | Minor per Cargo-reference policy (turbofish breakage is allowed-minor) **with `turbofish-hazard` note in detail**; policy flag can escalate |
| **L-3 Into-generalization** | `fn f(x: T)` → `fn f(x: impl Into<T>)`: **compatible only if** the fn previously had **zero** type generics (then: call sites fine since `T: Into<T>`; fn-pointer coercion still instantiates; turbofish was impossible before). If prior generics existed → L-2's E0632 applies. Trait method → breaks implementors → Major unless `sealed:full` (→ L-1). | no-generics free fn → **None** (must-not-FP); with-generics → Minor+hazard; trait → Major/sealed-Minor |
| **L-4 bound loosen/tighten** | Tighten = callers may no longer satisfy → Major. Loosen = more impls apply; inference ambiguity downstream is allowed-minor (RFC 1105 policy) | Major / Minor |
| **L-5 private field added** | Breaks iff the struct was **literal-constructible** downstream: previously all fields public **and** not `non_exhaustive`. Then any field addition (pub or private) breaks literals & FRU → Major. Otherwise private-field add → Minor (Patch if truly invisible: no `Default`-ish surface change claimed — keep Minor, simpler) | conditional Major/Minor |
| **L-6 pub field added to all-pub struct** | also breaks exhaustive patterns without `..` → Major (same precondition as L-5) | Major |
| **L-7 trait default method added** | may collide with existing inherent methods downstream — allowed-minor | Minor |
| **L-8 dyn-compat** | losing dyn-compat breaks `dyn Trait` users → Major; §8.9 facts make this decidable without rustc | Major / gained Minor |
| **L-9 reexport moniker stable, target changed** | not a delete; judge the resolved shapes (C-3) | recurse |
| **L-10 auto-trait removal** | `yes → no` on exported type → Major; any `cond` → Uncertain | see B-12 |
| **L-11 variant/enum discriminant change** | with `repr` + explicit discriminants (FFI/casting surface) → Major; without repr → Minor | conditional |
| **L-12 const value change** | not an API break (values aren't typed surface) | Patch, detail-noted |

If a lint's implementation cannot establish a law's precondition from IR facts, it MUST take
the Uncertain branch, not the lenient one.

---

## 10. Evaluation harness (`nudox-semver-eval`, harness-only deps allowed)

### 10.1 Corpora

| Corpus | Contents | Purpose |
|---|---|---|
| C1 | CSC's own test crates / lint fixtures, vendored+pinned | Pack A parity |
| C2 | Cargo SemVer reference micro-crates (one rule per pair) | ground-truth law behavior |
| C3 | crates.io historical adjacent-version pairs (curated list, pinned) | realism / FP hunting |
| C4 | hand-built adversarial: Into-generalization, sealed matrices, feature-gated items, turbofish | Pack B negatives |
| C5 | cross-crate re-export workspaces | Pack C |

### 10.2 Canonical break identity

`BreakId = (moniker path, class-slug)` where class-slugs are a frozen taxonomy aligned to
Cargo-book rule names (`item-remove`, `fn-param-type`, `trait-item-add-required`, …). Two
mapping tables (maintained in the harness, versioned with CSC releases): CSC lint name →
slug; nudox `LintId` → slug. Scoring never compares tool-native ids directly.

### 10.3 Scoring and the gate (frozen)

Per pair p: oracle set `O(p)`, tool set `T(p)` (Certain findings only; Uncertain excluded
from both TP and FP). `TP=|T∩O|`, `FP=|T∖O|`, `FN=|O∖T|`; precision/recall micro-averaged
over the corpus.

```text
Win ⇔ Recall_nudox > Recall_csc ∧ Precision_nudox ≥ Precision_csc      (Pareto)
```

Oracle order: (1) witness compiles against old, fails against new (or inverse for
minor-detection); (2) Cargo SemVer reference text; (3) recorded human adjudication (a
committed YAML, reviewed like code).

### 10.4 Witness protocol

`eval/witness/<lint>/<case>/{old/, new/, consumer/}` — pinned toolchain in
`rust-toolchain.toml`; harness builds consumer against old (MUST pass) and new (MUST fail)
for positives; both-pass for negatives. Witness runs are the only place the semver plane may
invoke rustc, and only inside the harness binary.

### 10.5 Runner

`nudox-semver-eval run --corpus C1..C5`: for each pair, build IR both sides via the real
producer (cage path), classify, run pinned CSC on the same pair, map both to BreakIds,
score, emit a scoreboard artifact (JSON + markdown). CI job pins the CSC version and fails
on gate regression ("living scoreboard", K-CSC-Scoreboard). CSC version bumps are PRs that
re-baseline consciously.

### 10.6 Multi-language

Separate scoreboards per ecosystem; only Rust feeds the CSC gate. Other languages get law
packs when their producers emit the §8 facts; their verdicts never dilute the Rust claim.

---

## 11. Depth ladder and the vendoring plan

Classifier correctness never depends on store depth — depths upgrade **grain and
guarantees**, not meaning.

### Depth 0 — ship first (no fork)

F1 canonical files + stock libpijul record (Myers on canonical lines) + `nudox-ir-diff`
projection + `nudox-semver` + change metadata (§7.6). This is the product MVP. Note what §6.6
buys: Myers on canonical unique-prefixed lines already behaves like keyed field diff.

### Depth 0.5 — encoding hardening (no fork)

Already implied by F1 (§6.4). Add the paranoia test: a generated corpus of pathological
payloads (huge docs, every escape, non-ASCII names, 10k params) MUST all take the text path
(assert `has_binary_files == false` after record — the field exists on `Recorded`,
`record.rs:1010`).

### Depth 1 — vendored structured record (fork; trigger criteria below)

**Trigger:** adopt only if measured, not speculatively: (a) Myers misalignment observed
producing cross-field hunks in real history, or (b) conflict UX needs field-name-level
reporting, or (c) record CPU on canonical files becomes a bottleneck. Until then the fork is
parked — the seam is fully specified so it can be executed cold.

1. Vendor as `libpijul_nudox` (GPL boundary unchanged — linked only by `nudox-ir-vcs`/sync/
   server; same K7/R12 discipline as snix/libkrunfw).
2. Add to the fork a hook trait, threaded through `Builder::record`:

```rust
pub trait StructuredRecord: Send + Sync {
    fn matches(&self, path: &str, new_bytes: &[u8]) -> bool;      // magic sniff: b"NdIrF1\t"
    fn frame_delta(&self, old: &[u8], new: &[u8]) -> Option<FrameDelta>; // None ⇒ fall back to Myers
}
```

3. Seam (verified, §1.4): in `Recorded::record_nondeleted`, after `decode_file`, if the hook
   matches: force `encoding = Some(UTF_8)`, build the same `vertex_buffer::Diff` the stock
   path builds (old bytes + line offsets from the graph), and lower `FrameDelta` line-ops to
   `Hunk::Edit`/`Hunk::Replacement` with existing `Atom::{NewVertex, EdgeMap}` — the
   lowering lives **inside the fork** as `diff/structured.rs` because it reuses the private
   context-construction machinery of `diff/delete.rs`/`diff/replace.rs`. No new atom kinds,
   no `apply`/`unrecord`/sanakirja edits (K-Fork-Depth).
4. **License split [fix of Rev 1]:** Rev 1 put "the implementation" in MIT `nudox-ir-diff`
   while emitting libpijul atoms — impossible without GPL-linking the MIT crate. Corrected
   boundary: `nudox-ir-diff` (MIT) produces the engine-neutral `FrameDelta` (pure line ops
   over F1 key classes: scalar replace / set insert-delete at sort position / seq LCS ops);
   `libpijul_nudox` (GPL) consumes it. GPL code depends on MIT code; never the reverse.
5. Conflict grain: with per-field lines, pijul's order conflicts land between adjacent
   frames; `SolveOrderConflict` records normalize set-class order per §12.2.

### Depth 2 — first-class semantic hunk variants: **deferred indefinitely**

Only if lifecycle ops provably cannot lower to Edit/Replacement+metadata. Requires an ADR.
(Depth −1, a second apply engine, stays rejected.)

---

## 12. Commutation, conflicts, and multi-writer posture

### 12.1 Commutation laws (what commutes by construction)

| Independent things | Mechanism |
|---|---|
| Different symbols | different files |
| Doc edit ∥ signature edit (same symbol, different branches) | different F1 lines ⇒ disjoint vertices |
| Two set adds with different sort positions | insertions anchor to different neighbor vertices |
| Param edit ∥ attr edit | different key sections |

### 12.2 Conflicts (what conflicts, and the reading)

| Concurrent pair | Pijul manifestation | Handling |
|---|---|---|
| Same scalar line edited both sides | vertex conflict | surfaced as `ConflictOnField{intro, key}` |
| Set adds landing between the same neighbors | order conflict (zombie order) | **auto-normalizable**: set-class order is non-semantic; materializer sorts and a `SolveOrderConflict` change may be recorded to clean the graph |
| Sequence inserts at same position | order conflict | genuine: `ConflictOnField{key}` — order is semantic; resolution = re-record from source (producer truth wins) |
| Delete symbol ∥ edit symbol | pijul missing-context / zombie | `ConflictOnSymbol{intro}` |

### 12.3 Conflict detection at materialize (mandatory)

`output_repository_no_pending` returns `Vec<Conflict>` (§1.4). The materializer MUST check
it **before parsing any blob** — conflict markers inside an F1 file are not valid F1 and
must never reach the parser. Nonempty conflicts ⇒ `VcsError::ConflictedState{paths}`;
classify/serve/seal refuse with `UncertainReason::ConflictedState`.

### 12.4 Single-writer invariant (v1)

One writer per channel (§1.5). The commute/conflict machinery above matters for branch
workflows (fork → record → merge via apply), unrecord, and future multi-writer channels; v1
treats a detected concurrent write as a protocol violation, not a merge opportunity.

### 12.5 SMOLVM partials

Checkpoint states: `PartialDelta` label (§5.6); no ApiReport, no version tags, no fan-out.
`record_stream` maps: `Finish → finish()`, `Abort → abandon()` (unchanged, `stream.rs`).

---

## 13. Integration matrix

| Plane | Action |
|---|---|
| libpijul engine | Keep; own the IR→line canonicalization (Depth 0) and optionally the record seam (Depth 1). Never apply/unrecord |
| SMOLVM (SV-9/10, GD-39) | Guest emits wire entries only; host owns identity (matcher), recording, classify at finish. Checkpoint cadence via `StreamPolicy` unchanged |
| ir-stream (K27) | Wire format unchanged except payload v2 bodies; version-gate the stream (`Hello` schema_epoch=2) |
| O(delta) record | Preserved verbatim (§5.7) |
| Fan-out | Per-hash gating (§3.3): embed on `embed_hash`, graph/registry/semver on `api_surface_hash`, dedup on `payload_hash` |
| iroh sync (`nudox-sync`) | Change files are content-addressed and verify as today; epoch 2 handshake; `GenerationMeta` rides inside the hashed change |
| GUI (lindsey) | Consumes `PackageDelta` + `ApiReport` + RenameEdges over the server API; no GPL link (K7) |
| Terminus hot tier | `Renamed`/`Moved`/`SignatureEvolved` ops project to LINEAGE edges; delta digest keys idempotent ingestion |
| Registry `/v1/compiled/lookup` | Serve keyed by `VersionState`; invalidation on `api_surface_hash` change only |
| CSC | Eval harness only (K-CSC-Scoreboard) |

### Design-rule ledger (final)

| ID | Rule |
|---|---|
| **K-IR-Only-Semver** | Product ApiReport is pure over IR (+ policy + hermetic dep IR) |
| **K-Intro-v2** | IntroId preimage = path identity + collision-scoped disambiguator; host matcher owns continuity; wire ids never escape the session |
| **K-Subst-Cascade** | Any id reuse rewrites all in-generation references and re-seals payloads before WC write |
| **K-Semantic-Diff** | PackageDelta is meaning; pijul atoms are the commute substrate; diff-time is matcher-free |
| **K-No-Dual-SoT** | files → tables → surfaces → report; round-trip law on CI; metadata advisory |
| **K-IR-Encoding** | WC files are canonical F1: printable UTF-8, one frame per line, canonical order. Binary record path unreachable |
| **K-Hash-Classes** | Three hashes, three consumers; no consumer gates on a foreign hash class |
| **K-Fork-Depth** | Vendor only for the structured-record seam; no new atom kinds, no second apply engine without ADR |
| **K-CSC-Scoreboard** | "Beats CSC" claims only from the §10 Pareto gate, on the pinned scoreboard |
| **K-Single-Writer** | One writer per channel in v1; concurrent write = protocol violation |

---

## 14. Emergent benefits (each with its unlock phase)

1. **Free changelogs** — render `PackageDelta` + `ApiReport` per version pair; the report's
   `detail` fields are the changelog entries. (P5)
2. **Semantic PR review** — GUI shows "param 2 of `parse` changed `&str → impl AsRef<str>`,
   Compatible (L-3)" instead of text hunks. (P5 + GUI wiring in P10)
3. **Fan-out thrift** — doc sweep across 10k symbols re-embeds but never touches graph,
   registry cache, or semver plane; a signature fix re-lints one item. (P3)
4. **Dependents' refs survive refactors** — continuity keeps `StableRef`s valid across
   renames/moves, so cross-package links never rot from a rename. (P2)
5. **Reverse impact scan** — "who breaks if this ships": walk inbound StableRefs/links to
   the items with Major findings. (P5 + graph tier)
6. **API archaeology** — `symbol_history(intro)` + per-change deltas answer "when did this
   param change and what shipped it" natively. (P3)
7. **Version-diff endpoint** — registry serves `diff(v1, v9)` as ops for any pair, O(state)
   worst-case via the serve cache. (P3)
8. **Pre-publish gate / editor hint** — run classify against the last published tip before
   `publish`; surface "this is a Major change" at edit time. (P5)
9. **Reproducibility proof** — deterministic canonical bytes ⇒ two independent producers of
   the same source produce the same change hash; a mismatch is a producer bug or tampering
   signal. (P1)
10. **Storage thrift** — unchanged symbols are stored once across all versions (pijul graph
    sharing) and F1's one-line-per-field keeps change contents proportional to semantic
    deltas; zstd change files absorb the hex/ASCII overhead. (P1, guarded by size_tests)
11. **Semver-frozen surfaces as artifacts** — per-config `ApiSurface` snapshots double as
    the registry's compiled-lookup source. (P9)
12. **Teaching corpus** — every adjudicated eval pair becomes a regression fixture; the lint
    suite compounds. (P6)

---

## 15. Roadmap (greenfield order)

Dependencies: P1 → P2 → P3 → {P4, P5} → P6 → P8 → P9; P7 (producer facts) starts after P1
and must land before P6 can score Pack A fully. P10/P11 trail. Critical path for "beats
CSC": P1–P8. Critical path for "semantic store": P1 alone (F1 is day-one).

| Phase | Deliverable | Key work items (crate-level) | Exit criteria |
|---|---|---|---|
| **P0 Spec freeze** | This doc; Intro-v2 ADR; delete stale doc references | — | Decisions locked; Rev 2 merged |
| **P1 Store core (F1 + wire v2)** | Canonical store end-to-end | `nudox-ir`: kinds 6–12, wire bodies, attrs, `entry.v2` seal + new golden pin. `nudox-ir-vcs`: rewrite `blob.rs` → `f1.rs` (serializer/parser per §6.5, strict), `NdIrSym` demoted to debug export; materializer conflict check (§12.3); metadata in `make_change` (§7.6); mtime/O-delta mechanics preserved; size_tests updated | determinism goldens (same table ⇒ same bytes ⇒ same change hash); paranoia corpus never binary (`has_binary_files == false`); doc-paragraph one-line diffs; O(delta) tests green |
| **P2 Identity + continuity** | Intro v2 + matcher + two-phase session | `nudox-ir`: `bootstrap_intro_id_v2` (+ collision scoping in `seal_payloads`). `nudox-ir-vcs`: `continuity.rs` (§5.3–5.5), StagingTable, σ substitution + re-seal, finish rework, `FinishReport{delta, continuity}`; checkpoint provisional labeling | §16.1 C-tests: rename, move, sig-change, overload flip, resurrection, kind-change-no-merge, replica determinism |
| **P3 Delta projection** | `nudox-ir-diff` (MIT) + structured diff API | `diff_tables` (§7.3); `IrRepository::diff_refs_structured`; delta digest into metadata; server endpoint | round-trip law on fixtures; `VersionDiff` still cheap; V-1 fan-out gating test |
| **P4 ApiSurface** | Projection + policy | `nudox-semver::surface` (§8.4 reachability, monikers, boundary); `api_surface_hash`/`embed_hash` emitters | projection unit tests incl. reexport chains, doc_hidden, cycles |
| **P5 Pack A + report** | `nudox-semver::classify` v1 | lint engine, A-1…A-19, report types, policy knobs | golden reports on C2 subset; `classify` runs with network off and no rustc on PATH (§16.1 S-5) |
| **P6 Harness** | `nudox-semver-eval` | corpora vendoring, BreakId maps, witness runner, scoreboard CI | reproducible scoreboard; Pack A parity gate evaluated |
| **P7 Producer completeness** | RA facts per §8.5 | compiler RA path: generics/where/traits/impls/attrs/cfg/sealed/dyn/auto-v1/`retgt` | surface projection over real crates matches rustdoc-JSON-derived oracle on C2 |
| **P8 Pack B** | Type/generic lints + witnesses | B-1…B-12, law table, C4 negatives | recall win on C3/C4 without precision loss (the §10.3 gate) |
| **P9 Pack C + D** | Cross-crate + lattice | `DepSurfaceProvider` over registry archives; per-config snapshots; `classify_lattice` | C5 green (re-export move ≠ delete; drop caught); feature-attributed findings |
| **P10 Product wiring** | Fan-out gates, GUI lineage, sync epoch | outbox gating per §3.3; lindsey delta/report views; `nudox-sync` epoch 2; rename serve-`checkpoint.rs` → `serve_seal.rs` (naming collision, §1.1) | V-suite green end-to-end |
| **P11 Depth 1 (conditional)** | Vendored structured record | §11 items 1–5, behind the trigger criteria | no-Myers-on-IR-path test; commute tests unchanged; hunk-quality comparison recorded |

---

## 16. Acceptance suites (concrete)

### 16.1 Identity & continuity (C) and semver (S)

| Test | Scenario | Assert |
|---|---|---|
| C-1 rename | rename exported fn, nothing else | same IntroId; ops = [Renamed]; report: A-1 Major on old moniker path absent **unless** a reexport covers it; RenameEdge absent (High consumed it) |
| C-2 move | move type across modules | same id; ops=[Moved]; dependents' refs unchanged |
| C-3 sig change, unique name | param `i64 → &str` | id stable **without matcher** (v2 preimage); ops=[SignatureEvolved, ParamTypeChanged]; B-1 Major |
| C-4 overload flip | 2 overloads (C# producer), one changes sig | R-OV High; same id; SignatureEvolved |
| C-5 kind change | struct→enum same name | delete+add ids; A-2 Major; no High merge |
| C-6 overload-set churn | 1 fn → add 2nd overload | first fn's wire id flips, matcher reunifies at High (identical payload) |
| C-7 resurrection | delete in gen2, restore in gen4 | same id; Resurrected op; history stitched |
| C-8 replica determinism | two hosts record same source over same tip | identical σ, identical change hash |
| C-9 ambiguity refusal | two identical-shape same-name candidates | no High (margin rule); both Soft; delete+add |
| C-10 cascade | rename a type used in 50 signatures | σ rewrites all 50 payload refs; re-sealed hashes; files byte-identical to a from-scratch record of the final state |
| S-1 parity | Pack A on C1∪C2 | precision ≥ CSC, recall ≥ CSC − 2 |
| S-2 type wins | B-lints on C4 positives CSC misses | caught with witnesses; negatives (L-3 no-generics) emit **no** finding |
| S-3 cross-crate | C5: move-behind-reexport / drop-reexport | C-2 None / C-1 Major |
| S-4 partial refusal | checkpoint-only state | classify returns Uncertain(PartialGeneration); no report published |
| S-5 hermetic | classify with network namespace off + rustc removed from PATH | identical report |

### 16.2 VCS (V)

| Test | Scenario | Assert |
|---|---|---|
| V-1 doc-only | edit one doc paragraph | Compatible(Patch); `embed_hash` changed (re-embed fires); `api_surface_hash` unchanged (graph/semver skipped); pijul change touches exactly 1 content line |
| V-2 commute | two symbols edited on parallel branches | both apply orders converge; no conflict |
| V-3 field conflict | same `fieldty` edited on both branches | ConflictOnField; materializer refuses before parse |
| V-4 abort/resume | producer aborts mid-stream | tip unchanged; next record succeeds; no partial report escaped |
| V-5 sync round-trip | full change sync via nudox-sync | pijul verify passes; remote materialize == local; GenerationMeta intact |
| V-6 debug export | NdIrSym export of any table | parses back equal (debug path only) |
| V-7 O(delta) | 10k symbols, edit 3 | ≤ 3 content files re-diffed (stat-cache counters); change size ∝ delta |
| V-8 binary-path paranoia | pathological payload corpus | `has_binary_files == false` on every record |

---

## 17. Worked examples (definite verdicts)

| Event | Identity / ops | Report |
|---|---|---|
| Doc paragraph edited | same id; DocChanged | Patch; re-embed only |
| Exported fn param `i64 → &str` | same id (unique name); ParamTypeChanged | B-1 Major |
| Free fn `i64 → impl Into<i64>`, no prior generics | same id; ParamTypeChanged | **None** (L-3 negative — must not FP) |
| Same generalization, fn had `<T: Display>` | same id | Minor + turbofish-hazard (L-2/L-3) |
| Same change on unsealed trait method | same id | Major (implementors break) |
| … on `sealed:full` trait method | same id | Minor (L-1) |
| Rename exported fn, no reexport left behind | same id; Renamed | A-1 Major (old path gone) + continuity note |
| Rename + `pub use old_name` deprecated alias | same id; Renamed + reexport added | Minor (deprecation A-13) |
| Move type to dep crate, reexport at old path | same id locally replaced by reexport entry; boundary retgt | C-2 **None** |
| Drop a reexport | ReexportRetargeted/absent | C-1 Major |
| Add private field to all-pub struct | ChildAdded | L-5 Major |
| Add private field to struct with existing private field | ChildAdded | Minor |
| Add variant under `non_exhaustive` | ChildAdded | A-7 Minor |
| `non_exhaustive` added | AttrsChanged | A-14 Major |
| Concrete type gains `Rc` field | AutoTraitsChanged send yes→no | B-12 Major |
| Feature-gated item removed under `feature=extra` only | per-config surfaces differ | Major `when: feature=extra` |
| Internal helper deleted | Deleted (non-exported) | None (not on surface) |

---

## 18. Risks (ranked, with owners in the phase plan)

1. **Producer facts lag lints** (P7 slips ⇒ Pack A can't reach parity). Mitigation: S-gate
   simply doesn't pass; no claims. Lints emit Uncertain on missing facts by construction.
2. **Matcher false merges** — the one unrecoverable pollution. Mitigation: High-only σ,
   kind gate, R-CHILD, margin rule, C-9/C-10 tests, cost-asymmetry defaults.
3. **Canonical-bytes drift** across producer versions (accidental re-render of unchanged
   symbols). Mitigation: determinism goldens + skip-identical-write makes drift visible as
   spurious "updated" counts in `StageReport`.
4. **Over-eager type-equality FPs** (L-3 class). Mitigation: precondition-checked laws,
   negative witnesses required per lint, Uncertain channel.
5. **Memory of two-phase staging** on giant packages. Mitigation: documented budget;
   spill-behind-API escape hatch (§5.1).
6. **CSC improves** (their type lints are on the roadmap). Mitigation: living scoreboard;
   the bet is on IR breadth (cross-crate + features + auto-traits), not a single lint race.
7. **Feature lattice combinatorics** creeping past F+3 configs. Mitigation: K-rule refusal;
   symbolic implication is the sanctioned v2.
8. **Fork bitrot** if Depth 1 triggers. Mitigation: seam is 2 functions + 1 new module;
   trigger criteria keep it parked until evidence.
9. **Checkpoint identity noise** confusing lineage readers. Mitigation: PartialDelta labels,
   provisional paths documented, publishers gated to finish tips.

---

## 19. Rev 1 → Rev 2 resolution log (adversarial findings)

| # | Rev 1 hole | Resolution |
|---|---|---|
| F1 | Depth 0.5 "postcard frames, no fork" self-contradictory: raw postcard bytes contain `\n`/non-UTF-8 ⇒ verified binary path (8 KB rolling chunks) kills field grain | F1 is ASCII-armored text with strict escaping; proof against verified `decode_file`/`get_valid_encoding` mechanics (§6.4) |
| F2 | Continuity matcher had no algorithm, no determinism story | Full spec: buckets, frozen integer weights, thresholds, R-OV/R-CHILD, greedy assignment, sorted-iteration determinism (§5.3–5.5) |
| F3 | **Substitution cascade missed**: reusing an id must rewrite every in-generation reference and re-seal payloads | K-Subst-Cascade; two-phase session; C-10 test (§5) |
| F4 | "Deterministic overload index" disambiguator: positional indices churn identity on overload deletion | Collision-scoped skeleton disambiguator; unique names get empty bytes (§4.3) |
| F5 | Positional `in_param(i)` field keys: mid-list insert rewrites all following lines | Sequences carry no index; order = line order (§6.1 #4, §6.2) |
| F6 | Doc-only acceptance ("no re-embed") contradicted `embed_hash` containing docs | Per-consumer hash gating; doc-only re-embeds, skips semver/graph (§3.3, V-1) |
| F7 | Feature dimension left as "either/or" | Canonical config on the channel + bounded per-config surface snapshots; powerset refused (§9.7) |
| F8 | `Into<T>` law stated as bare "often compatible" — FP/FN trap | L-3 with exact preconditions (zero prior generics / E0632 / trait / sealed) + witness matrix (§9.8) |
| F9 | Vendoring assumed needed for change metadata | `make_change(…, metadata)` exists in the pinned source; metadata lands at Depth 0 (§1.4, §7.6) |
| F10 | Rev 1's Depth-1 license split (MIT crate emitting GPL atoms) impossible | FrameDelta (MIT) / atom lowering inside the fork (GPL); dependency direction fixed (§11.4) |
| F11 | Re-export targets silently lost in the store (bare `ref` flag, no target) — Pack C dead on arrival | `reexport` kind + `retgt` key + boundary map (I4; §6.2, §8.2) |
| F12 | `api_surface_hash` preimage undefined; with parent/name included, rename/move continuity would self-sabotage | Moniker-class (`name`, `parent`) excluded; S-column freeze; collision caveat handled by matcher corroboration (§3.3, §5.4) |
| F13 | Matcher memory claim ("tip_intros suffices") wrong — needs T0 payloads | Session materializes `tip_table` at begin; budget documented (§5.1) |
| F14 | Checkpoint identity semantics unspecified | Provisional wire-id states; PartialDelta; publisher gating (§5.6) |
| F15 | Conflict reading unspecified (markers would corrupt blob parse) | `output_*` returns `Vec<Conflict>`; check before parse; ConflictedState refusal (§12.3) |
| F16 | Sets-vs-sequences commute hand-waved ("set vertices") | Canonical sort positions for sets + auto-normalizable order conflicts; sequences conflict genuinely (§12.2) |
| F17 | No ApiSurface/ApiReport/op type definitions, no lint tables, no law preconditions | §7.2, §8, §9 in full |
| F18 | Eval scoring undefined (what counts as one break?) | Canonical BreakId taxonomy + mapping tables + micro-averaged Pareto gate (§10.2–10.3) |
| F19 | Migration/dual-read machinery contradicted the greenfield mandate | §0.2: discard and re-record; legacy paths cut |
| F20 | Rev 1 as-built table had drifted (flat-vs-`symbols/` layout; deleted design doc; `hash` line cost; auto-trait feasibility unphased) | §1 re-verified from source with citations; `hash` line dropped from F1; §8.7 phases auto-traits |

---

## 20. Document control

| Rev | Date | Notes |
|---|---|---|
| 1 | 2026-07-17 | Initial plan (superseded) |
| 2 | 2026-07-17 | Adversarial rewrite: verified as-built + libpijul citations; identity v2 with collision-scoped disambiguators; two-phase session + substitution cascade; F1 armored format with frozen key registry; full matcher algorithm; typed delta/surface/report schemas; law tables with preconditions + witnesses; harness scoring spec; corrected vendoring seam + license split; greenfield reset; emergent-benefits and resolution log |

**Implementation status:** Plan (normative target). As-built remains `NdIrSym` + Intro v1 +
coarse `VersionDiff` until P1–P2 land.

**Where this lives:** `.research/ir-vcs/design/SEMANTIC-IR-VCS-PLAN.md`
