# Card W9b: merge explicit interface-member facts into the method-set cell

registered role: nudox_luna_implementer (implement-rust-checkpoint)
baseline: branch `go-fidelity/integration` at `82b5e939` in
`/private/tmp/nudox-fidelity-go`. Verify `git log --oneline -1` first.
You are the only writer.

## Owned paths

- `compiler/driver/lower/go.rs` — production `Projector::type_family` and
  `Projector::interface_methods` only, plus the one test if an assertion
  needs calibrating (it should not).

## The defect (manager-verified)

In `type_family`, explicit interface methods projected from the
interface row's member run are collected into `interface_methods` but
never merged into the `methods` list that becomes the GoFacts
`method_set` entity list, and their names are never registered in
`method_names`. Consequences, both wrong:

1. An interface's `method_set` cell omits its explicit methods.
2. Against a REAL producer image, the method-set plane also carries the
   same method (the producer emits `t.AllMethods` rows — see
   `compiler/languages/go/oracle/image.go` emitType interface case), so
   the method-set plane loop re-projects a duplicate fact for every
   explicit method instead of deduplicating against the member-sig fact.

## Required repair (semantics fixed by the failing test)

In `type_family`, after the member projection and before the method-set
plane loop:

- append every `interface_methods` ordinal to `methods` in projection
  order, and
- register every such member's name in `method_names` so the method-set
  plane loop's existing `method_names.contains` skip deduplicates
  against the richer member-sig fact (the member sig carries full
  parameter/result types; the plane row must not re-project it).

Fact-order law: fields first, then owned methods, then explicit
interface members, then any method-set plane rows not already covered.
The existing test
`struct_fields_and_interface_methods_join_the_go_extension` pins this
exact order (facts 0..9) and must pass UNMODIFIED except if its inline
comment needs no change — do not touch its assertions.

## Falsifier (the failing test is the falsifier)

`cargo test -p compiler-driver --lib
lower::go::tests::struct_fields_and_interface_methods_join_the_go_extension`
must pass with the Store extension row asserting
`method_set.raw == 3` and pooled list `vec![9]` (the explicit member
fact), and Node's assertions unchanged.

## Also prove (no new fixture needed)

Dedup: extend the SAME test fixture with one method-set plane row for
Store naming `Put` (the Fixture helper that the image fixture builder
uses for method-set rows — read `Fixture`), then assert the fragment is
UNCHANGED versus without it (same bytes) — the plane row must not
produce a second Put fact. If the Fixture cannot express a method-set
row, add the minimal builder method; that helper is in-card.

## Bounds

- No changes outside lower/go.rs. No wire, producer, reader, or ir
  changes. No new public surface.

## Exact commands

```
cargo test -p compiler-driver --lib lower::go 2>&1 | tail -4
cargo fmt --check -p compiler-driver
```

Terminal: lower::go 20/20 green, fmt clean.

## Commit protocol

One commit: `fix(go): merge explicit interface members into the method-set cell (W9b)`.
Return: commit hash, lower::go result line, and confirmation the
dedup falsifier ran.
