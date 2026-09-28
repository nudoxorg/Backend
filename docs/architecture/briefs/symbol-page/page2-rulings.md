# Lead's rulings on D-Page's checkpoint (2026-09-27)

I accept the checkpoint. The stills I read:
- `rel-500-open-serialize`
- `rel-hover-verb-value`
- `rel-door-flight-value-0.5`
- `v2` and `r-rest` (the working stills)

The relations presentation is **verb rows**: yours first, "and N more" unfolding by band and then by package, with the comb on hover or ⌥. It replaces `anatomy/prism.rs` on the page. The page is ordered by question.

## Open questions
1. **Headings stay statements** ("Getting one", "What it does", "When it fails", "Who uses it", "What changed"). Questions as headings read cute and add noise; the sentence already asks.
2. **An error type's "Getting one" is what produces it.** List the failing calls grouped by kind (the kinds `classify()` tells apart). Below that, one quiet line: "making your own: `Error::custom(msg)`, inside a Deserialize impl".
3. **"acts like", unfolded, groups by word family** (`split_*`, `chunks*`, `sort*`, `iter*`, …) with one line per family. Any family of 1 goes into "others".
4. **A single relation still gets its row**, for consistency.
5. **Contrast:** every run is ≥ ink3. W-Shell is applying ink3 to `anatomy::heading` in Rust, and the ports adopt page2.css's overrides.

## Defects to fix in the implementation
- Flight labels run together: "index_mutvalue" and "indexvalue" in `rel-door-flight-value-0.5.png` are a name and a package with no gap.
- Labels collide mid-flight (inherits / requirement / inherited_string, package_facts / parse_rustsec). Landing slots are known before take-off, so choose arcs that don't cross, order the stagger by landing y, and never let two legible labels overlap in any frame. D-Motion owns the grammar; this flight must follow MOTION.md.
- The prototype's own titlebar still shows the trail beads. Ignore them: D-Hand replaces them.

## Implementation order (for the successor to W-Anatomy)
1. **Relations verb rows** in `semantics::relations` + `anatomy::relations`: the verb table from `VERBS.md`, bands, per-package groups, and the unfold. The joint card waits for the engine to carry spans (W-Facts).
2. **The sentence**, one unbreakable clause per question.
3. **`can` fold** ("the usual N", keeping only the special capabilities at rest) and the dotted auto glyph.
4. **`fails`**: one component, starting from the facts the engine has.
5. **`acts_like`** (Deref target: one line, then the fold).
6. **`does`**: impl-condition groups, a fold after 6, and trailing marks.
7. **Gates, deprecation, member docs, since.** Each lands as W-Facts delivers the fact. Design each to render "unknown" until then.

## Recipe engine (`semantics/recipes.rs`)
- Producers whose feature gate is off for you are dropped from the best route. The case is SmallVec's `new_const`, which sits behind `const_new` and is offered first even though your build lacks it. Do this once W-Facts R3 delivers gates. Until then, the page dims such a route with "not in your build", as D-Page drew it.
