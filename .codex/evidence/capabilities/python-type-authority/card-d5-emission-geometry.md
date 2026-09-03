# Card d5-emission-geometry — every corpus primary lowers completely

registered role: nudox_luna_implementer
baseline: commit 202b42aa2 (branch codex/fidelity-python)
- compiler/driver/lower.rs | 2448 LOC | e0e1831ea2b1a1dc
- compiler/driver/lower/python.rs | 2139 LOC | d2c31634f17d44f2
- compiler/driver/lower/tests.rs | 486 LOC | eb47acff6445d16d

## Owned paths (no other writer)

- compiler/driver/lower.rs
- compiler/driver/lower/python.rs
- compiler/driver/lower/tests.rs

Everything else is forbidden (including compiler/ir/**, ir-vocabulary, every other
lower/*.rs language file, every tests/*.rs driver file, manifests).

## Law (one terminal)

All twenty real-package primary modules lower COMPLETELY at one frozen, named
geometry. Today four typed capacity terminals remain (verified live by Terra at
baseline):

1. attrs 25.3.0 src/attr/_make.py — fact 258 rejects, FactFault::ChildCapacity
   (the per-fact child lane bound, 16, is exceeded);
2. pyparsing 3.2.3 pyparsing/core.py — fact 1024 rejects, FactFault::Capacity
   (the emission fact lane bound, 1024, is reached);
3. click 8.2.1 src/click/core.py — fact 105 rejects, FactFault::ChildCapacity;
4. jinja2 3.1.6 src/jinja2/environment.py — fact 107 rejects,
   FactFault::ChildCapacity.

After this card, `cargo test -p compiler-driver --test python_packages -- --nocapture`
must print "python package selected:" for ALL TWENTY primaries and ZERO
"python package typed terminal:" lines.

## Must measure first (evidence in your return; do not commit throwaway probes)

Instrument once to report the EXACT demand: total fact demand of pyparsing
core.py; the exact child demand of attrs fact 258, click fact 105, jinja2 fact
107 (which declaration each is, by name). The frozen bound must be derived from
these numbers with named headroom (e.g. next power of two or a stated policy),
never a magic number.

## Mechanism is yours, within these bounds

- Raising the named lane constants (MAX_EMISSION_FACTS, MAX_FACT_CHILDREN,
  MAX_TYPE_CHILDREN, and every derived constant: MAX_EMISSION_ATOMS,
  MAX_TYPE_ROWS, ANONYMOUS_ROW_BASE, COMPUTED_ROW_BASE) is expected; OR rerouting
  member-heavy classes through the pooled structural segment; OR both. Pick what
  the measured demand supports and say why in the return.
- The scratch-lane size law `size_of::<FactSet<'static>>() <= 64 * 1024`
  (lower.rs ~line 351) must remain TRUE at the chosen geometry, or the lane
  moves to a boxed slice with the bound renamed to a stated byte/element
  policy — never deleted.
- Wire format unchanged: fragment bytes stay schema-1; fact/row ordinals stay
  u32; no validator edits outside lower.rs.
- Typed admission stays honest: input beyond the NEW bounds must still produce
  the exact RejectedFact with ordinal + name bytes + FactFault; no silent
  truncation, no fallback to a smaller representation, no weakening of
  diagnostics.
- Other frontends (rust/go/csharp/java/clang/typescript lanes) share these
  constants; their gates must stay green. Raising a bound admits more; it must
  change no existing assertion.

## Must prove (falsifiers)

1. lower/tests.rs: a lane-level test pinning the NEW bounds — a synthetic
   source at bound-1 lowers completely, at bound lowers completely, beyond
   bound raises the exact typed fault (capacity and child-capacity classes).
2. `cargo test -p compiler-driver --test python_packages -- --nocapture` —
   20 "selected" lines, 0 "typed terminal" lines, 10/10 pass.
3. `cargo test -p compiler-driver` — every python_* test target green
   (render 6/6, lower_facts 5/5, authority 7/7, purl 8/8).
4. `cargo test -p compiler-languages-python` — 35/35.
5. `cargo test -p compiler-driver --test rust_corpus --test rust_render_golden
   --test rust_semantic_lane` and `cargo test -p compiler-application --test
   local_compiler` — adjacent-lane gates stay green.

## Bounds

- No new dependencies, no manifest edits, no unsafe.
- No edits outside the three owned files.
- Source stays rustfmt-clean; keep forbid(unsafe)/deny(unwrap,expect,panic) in
  test code.

## Commit and return

- One coherent checkpoint: `feat(driver): admit the full corpus at measured emission geometry`
- Return: commit hash; the measured-demand table (module → facts/children
  demanded, old bound, new bound, derivation); the size law outcome; exact gate
  tails (the 20-line selected/terminal check verbatim); smallest remaining red.
