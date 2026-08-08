# Our UI vs. docs.rs — an evidence-bound comparison

**Revision 2, 2026-08-05.** Revision 1 (CRITIC, 2026-08-04) compared against a
synthetic 23-symbol fixture and correctly refused to credit anything it could
not see. This revision re-runs the comparison against a **real crate**:
`.shots/memchr/`, captured from a rust-analyzer lowering of
`.real-crates/memchr-2.8.3` — **11,329 symbols**. docs.rs claims are from a
live fetch of `docs.rs/memchr/2.8.3/memchr/struct.Memchr.html` on 2026-08-05.

Same rule as revision 1: every claim about our UI is tied to a frame that was
opened and looked at. Nothing is credited from code or intent.

> **Caveat added after capture — the symbol count in these frames is wrong.**
> Every frame displays `11329 symbols`. That number was produced while
> `cargo metadata` was silently failing and rust-analyzer was substituting
> `--no-deps` metadata, so no dependency resolved and every Cargo feature —
> including `default` — evaluated false. With the fixtures vendored and
> resolving, memchr 2.8.3 lowers to **11,361**, and 2.7.6 to **902** (see
> `LIMITATIONS.md` L28, and `AGENTS-DOCTRINE.md` §8 on why identical counts
> across three generations should have been treated as a bug report).
>
> Unaffected: prose rendering, hyperlinks, the breadcrumb, the layout void, the
> empty table of contents, and the `TODO(views)` leak — none depend on feature
> resolution. The impl count is also safe, verified against source
> independently (none of the six impls is cfg-gated).
>
> **Possibly affected: F1.** The seven duplicate `mod memchr` rows are
> arch-gated modules (`arch/x86_64/avx2`, `arch/aarch64/neon`,
> `arch/wasm32/simd128`, `arch/all`). With features resolving, fewer may be
> active, so the *count* of duplicates could drop. The defect itself does not
> go away — rows that do appear are still undifferentiated — but re-measure
> before quoting "seven".
>
> The frames must be re-captured before they are shown as end-to-end proof.

**Frame integrity check (`.shots/memchr/`, 26 frames):** 15 distinct, **11
byte-identical duplicates** in two runs — `08`–`15` (eight frames, MD5
`16b69ff7…`) and `17`–`19` (three frames, MD5 `1928adff…`). Those are not
mislabelled captures; they are real evidence that the actions dispatched in
those steps reach no handler. `MANIFEST.md` records each as `changed px: 0`
with an honest caption naming the cause. Credited below as failures, not
skipped.

---

## 0. Scoreboard against revision 1's bar

Revision 1 closed with six things that "would make the difference undeniable".

| # | Bar | Status |
|---|---|---|
| 1 | A real checked-out crate loaded, non-trivial symbol count | ✅ `memchr-2.8.3`, 11,329 symbols, sidebar + every frame |
| 2 | A rendered symbol page to compare item-for-item | ✅ `08-symbol-opened.png` — `struct Memchr<'h>`, real prose, `Implementations 6` |
| 3 | A frame captured immediately after committing a search hit | ✅ `08` is exactly that; 5.1 M pixels changed from `07` |
| 4 | Two versions of one crate, both selectable | ⚠️ **Partial.** Three versions are on disk and a lineage test exists, but `13-version-picker.png` is byte-identical to `08` — the picker never opened. Not demonstrated in the GUI. |
| 5 | The `Semantic` tab actually selected, returning different results | ❌ **Still not done.** The tab is drawn in `03`/`04`; no frame shows it selected. Unchanged from revision 1. |
| 6 | Fix or re-caption the duplicate boot frame | ✅ `02-corpus-loaded.png` now differs from `01` by 8,331 px with a caption that says exactly what repainted |

Four of six cleared. Items 4 and 5 are the honest remaining gaps in the
"working lineage / working search" half of the goal.

---

## 1. What docs.rs does well (revision 1, re-verified 2026-08-05)

- **Per-item pages are excellent** — full signature, prose, a worked example,
  a direct source link to an exact line range, and breadcrumbs.
- **Crate-root pages are rich** — license, release date, owner (BurntSushi),
  homepage/repo/crates.io/source links, doc-coverage badge, dependency list,
  API grouped by kind.
- **Version and platform handling is real and visible** — version selector with
  release date (`08 July 2026`), a target selector
  (`x86_64-unknown-linux-gnu`), and a separate feature-flags page.
- **Ubiquitous and zero-setup** — every crate, every published version, no
  local checkout or corpus step.
- **Fast, server-rendered, no client runtime** — works with JS off, over slow
  links.

Newly confirmed at the struct level: **all sections are expanded by default**,
and **every item carries a `[Source]` link** to an exact range
(`src/memchr/memchr.rs.html#288-291`).

This is a strong, mature product. Anything below claiming we beat it means on
that *specific, cited* dimension — not overall.

---

## 2. Differentiation — we repair one intra-doc link typo, deliberately

*(Was "Parity — prose fidelity, verified down to an upstream typo" through
revision 2. It is not parity: we render a link where docs.rs renders literal
text. As of 2026-08-06 that difference is owned, bounded, marked and counted,
so it is claimable — see the ruling below for the evidence that keeps it
honest.)*

memchr's own source, `src/memchr.rs:282` (recurring at 358 and 426):

```rust
/// This iterator is created by the [`memchr_iter`] or `[memrchr_iter`]
```

The backtick and bracket are transposed on the second link. **docs.rs renders
`memchr_iter` as a working link and `[memrchr_iter]` with literal brackets.**

> ### ✅ OWNED, BOUNDED AND VISIBLE. Decided and implemented 2026-08-06.
>
> This section has been through three verdicts. Revision 1 claimed parity
> "verified down to reproducing an upstream typo" — false. Revision 2 corrected
> that to a **divergence**: both `memchr_iter` and `memrchr_iter` render as
> working links, we resolve the link the author obviously intended, and *nobody
> chose it* — it fell out of the doc-link and `has_declared_links()` work (L27).
> Revision 2 refused to credit it either way and demanded a decision.
>
> The decision: **keep the repair, kill the silence.** The behaviour stays,
> because a transposed backtick in someone else's crate should not be the
> reader's problem. What changes is that it is now *typed*, *counted*, *bounded*
> and *visible* — the four properties whose absence made it unownable.
>
> **The closed set.** We repair exactly one spelling, and the set of repairs is
> the Rust enum `nudox_engine::wire::LinkRepairKind`, which is deliberately
> **not** `#[non_exhaustive]`:
>
> | Variant | Source spelling | What rustdoc/docs.rs does | What we do |
> |---|---|---|---|
> | `TransposedOpenDelimiter` | `` `[foo`] `` — backtick and bracket transposed | Renders literal text (CommonMark binds code spans tighter than link brackets) | Render the resolved link, marked |
>
> That is the whole answer to "*what else* do we silently repair?". It is one
> row, and adding a second costs **five compile errors in five files** —
> `open_shape`, `DelimiterShape::repair_kind`, `LinkRepairKind::label`, the
> `all_lists_every_link_repair_kind` pin, and `lindsey`'s `repair_legend_label`
> in the separate GUI package. A wildcard arm exists in none of them.
>
> Note what is *not* in the table: `` [`foo`] `` and `[foo]` are rustdoc's own
> documented intra-doc-link syntax, which rustdoc and docs.rs both render as
> links. Rendering them as links is fidelity, and calling it a repair would put
> a "we changed this" mark on every ordinary link on every page.
>
> **Visible.** `InlineRun::Link` carries a mandatory `origin: LinkOrigin` with
> no `Default` — a link cannot enter the wire protocol without answering
> whether its spelling was the author's or ours. A repaired link paints in the
> same accent hue as a real link (it *is* a real link) but with a wavy `warn`
> underline, and hovering it shows the engine's own sentence: *"Repaired link —
> the source reads `` `[memrchr_iter`] `` (transposed backtick and bracket); we
> linked memrchr_iter."* The mark never alters the text, so selection and copy
> are unaffected.
>
> **Counted.** `RepairTally` derives the count *from the rendered runs* — there
> is no parallel counter that could drift from the thing it counts. The audit:
>
> ```bash
> RUSTC_BOOTSTRAP=1 cargo test -p nudox-engine --test link_repair_audit \
>   -- --ignored --nocapture
> ```
>
> emits one TSV row per (package, repair kind), including the zeroes:
>
> ```text
> link-repair	memchr	2.8.3	transposed_open_delimiter	3
> ```
>
> **3, measured 2026-08-06** — 1835 entries lowered, 1835 chunked. It agrees
> with the source: `grep -noE '`\[[A-Za-z_][A-Za-z0-9_:]*`\]'
> .real-crates/memchr-2.8.3/src/` finds exactly three transposed sites, all in
> `src/memchr.rs` (line 282 on `Memchr`, 358 on `Memchr2`, 426 on `Memchr3`).
> The multiplier is one because none of the three lives under the cfg-gated
> `arch/` tree; if one did, the count would be a multiple of 3 and would need
> saying so.
>
> **Kept honest by.** `transposed_backtick_shortcut_is_recorded_as_a_repair`
> (`crates/nudox-engine/tests/hyperlink_flows.rs`) is the named guard: it
> asserts the link arrives at the GUI seam carrying
> `LinkOrigin::Repaired(TransposedOpenDelimiter)` with the author's bytes in
> `raw`, and fails with "SILENT REPAIR" if the origin comes back `Authored`.
> Verified by mutation: making `DelimiterShape::repair_kind` return `None` for
> the transposed shape turns **five** tests red across three files.
>
> **This is now creditable as a differentiation win**, with the audit number as
> its evidence — see §3.

The paragraph below is retained for its evidence; its conclusion is superseded
by the ruling above.

Under CommonMark the bracketed reading is correct, and our renderer originally
reached it independently. This was previously suspected to be a bug in our
doc-link resolution; it was then closed as upstream-faithful with the source
line as
evidence. It is also the case behind `LIMITATIONS.md` L27 — brackets are link
syntax only for a symbol that declared links, exactly as rustdoc treats them.

---

## 3. Where our UI is materially better — one row per claim, each cited

| Claim | Frame | What the frame shows |
|---|---|---|
| Real doc prose with working intra-doc hyperlinks, rendered from our own IR | `08` | `DoubleEndedIterator` as inline code; `memchr_iter` and `Memchr::new` as underlined working links |
| **We repair one class of malformed intra-doc link that docs.rs renders as literal text — bounded, marked and counted** | `08`, §2 | `` `[memrchr_iter`] `` is a working link here and dead text on docs.rs. The set of repairs is one enum variant (`LinkRepairKind`); each repaired link carries a wavy `warn` underline and a hover note naming the original spelling; `cargo test -p nudox-engine --test link_repair_audit -- --ignored` prints the corpus count (memchr 2.8.3: **3**) |
| **We repair one class of malformed intra-doc link that docs.rs renders as literal text — bounded, marked and counted** | `08`, §2 | `` `[memrchr_iter`] `` is a working link here and dead text on docs.rs. The set of repairs is one enum variant; each repaired link carries a wavy `warn` underline and a hover note naming the original spelling; `cargo test -p nudox-engine --test link_repair_audit -- --ignored` prints the corpus count (memchr 2.8.3: **3**) |
| **Impl coverage is complete, with far less noise** — verified exactly, see below | `08` vs docs.rs | We show **6**, which is *every* real impl this type has. docs.rs shows the same six, then adds ~7 auto-traits, blanket impls, and ~60 inherited `Iterator` methods |
| Multi-mode search as switchable pills in one overlay, not separate pages | `03`, `04` | `Auto` / `Name` / `Type` / `Semantic` pills above the results, `Auto` active |
| Search is modal over live shell state (cmd-K), not a page navigation | `03` | Sidebar and editor chrome stay visible behind the overlay |
| Live result count *and* latency surfaced | `04` | `21` and `0 ms` in the results header |
| Corpus scale surfaced up front | all | `memchr v2.8.3 / 11329 symbols` in the sidebar |
| Escape is staged, not destructive | `23`, `24` | First Escape clears the query only; a second closes the overlay — a deliberate guard against losing a query mid-edit |
| Tabs open behind the overlay without leaving search | `22`, `24` | alt-Enter adds a second tab; `24` reveals both with the first still active |
| Local-first | all | Entirely local, no network |

### Proof that `Implementations 6` is complete, not truncated

Counted from `.real-crates/memchr-2.8.3/src/memchr.rs` directly:

| # | Impl | Source |
|---|---|---|
| 1 | `Clone` | `#[derive(Clone, Debug)]`, line 287 |
| 2 | `Debug` | same derive |
| 3 | inherent `impl<'h> Memchr<'h>` (holds `new`) | line 293 |
| 4 | `Iterator` | line 308 |
| 5 | `DoubleEndedIterator` | line 341 |
| 6 | `core::iter::FusedIterator` | line 351 |

Six real implementations; we report six. We resolve `#[derive]`s into impls,
which is the part most likely to have been missed. docs.rs surfaces the same
six and then inlines the auto-trait, blanket, and inherited-method boilerplate
around them.

So the signal-to-noise claim is **not** that we show less — it is that we show
the same information with the boilerplate removed. That is the stronger claim,
and it is now measured rather than asserted.

## 4. Where docs.rs is still better — this is the work queue

| | docs.rs | ours | Ticket |
|---|---|---|---|
| **Section expansion** | All expanded; impls and methods visible on arrival | `Implementations` / `References` / `Source` collapsed at the bottom under ~600 px of empty background | F2 |
| **Per-item source links** | `[Source]` per item → exact line range | One collapsed whole-symbol `Source`; behaviour unverified because the disclosure never opened in any frame | **F6** |
| ~~**Trait implementations**~~ | — | **Resolved in our favour, see §3** | ~~F7~~ |
| **Table of contents** | Full section nav | `ON THIS PAGE` has exactly one entry, `Documentation` | F3 |
| **Result disambiguation** | n/a | Seven consecutive rows all read `mod memchr`, identically | F1 |
| **Provenance** | Owner, deps, repo/homepage, license, release date, coverage badge | Package name, version, symbol count | **F8** |
| **Platform / features** | Target selector + feature-flags page | cfg only partially resolved | L3 |
| **Breadcrumb** | `memchr::Memchr` | `memchr › memchr › memchr` | F4 |
| **Version selector** | Works | Dispatches, never opens | F-lineage / L16 |

## 5. Still not demonstrated — carried forward, uncredited

- **Semantic search.** The tab is drawn; no frame shows it selected or
  returning a result set distinct from `Name`/`Type`. Existing ≠ working.
- **Version lineage in the GUI.** Three memchr generations are on disk and
  `real_lineage.rs` tests them, but both tests are `#[ignore]`d and the GUI
  picker never opens. The lineage claim is currently backed by neither a green
  test run nor a pixel.
- **Streaming.** Nothing distinguishes "streamed from a local engine" from a
  static render in any frame.
- **Source jumping.** Asked for explicitly in the brief; no frame shows a jump.

---

## 6. What would make the difference undeniable — revised bar

1. Open the version picker and capture three memchr generations selectable —
   closes bar item 4 and the lineage half of the goal.
2. Run `real_lineage.rs` un-ignored and attach the output.
3. Select `Semantic` and capture a result set that differs from `Name` — bar
   item 5, open since revision 1.
4. Expand `Implementations` and show the six entries, so the signal-to-noise
   win is visible rather than asserted (F2/F7).
5. Capture a source jump landing on a line range (F6).
6. Make the seven `mod memchr` rows distinguishable (F1) — the one row where we
   are not merely behind docs.rs but actively unusable.
