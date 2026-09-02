# Capability brief: go-authority-fidelity

## Public terminal

The Go lane delivers the full oracle Output end to end: a versioned,
checksummed authority image (wire format v5; the parent mandate's "Image v2" =
second-generation image; the lane's internal wire version steps 4 -> 5) that
carries every `go/packages` oracle fact; a projection that lowers every image
plane into the canonical fact lane at full fidelity; docs.rs-style struct
rendering through `compiler/ir` render displays with golden tests; and a
complete lifecycle journey from a PURL (`golang:module@ver`) through module
proxy location/fetch, GOPATH workspace assembly, oracle execution, fragments,
publish -> reopen -> index, with old fragments still validating.

## Non-negotiable laws

1. The image is versioned, magic-tagged, checksummed (domain-separated
   SHA-256 over header + body), and binds to one exact source digest.
2. The image carries the complete oracle `Output`: module metadata; package
   rows (import path, name, files); declarations with exact constant values,
   const groups, iota; the recursive type graph (basic/named/alias/typeparam/
   pointer/slice/array/map/chan/func/struct/interface/union/tuple);
   signatures WITH parameter name facts; methods (declared + promoted with
   origin); generic type parameters with constraints; struct fields with
   tags and embedded flags; interface explicit methods, embeddeds, and the
   complete post-embedding method set; documentation rows (declaration,
   method, member, package); resolved call references (Oracle -> Local or
   Foreign go origins); build-constraint exclusions with exported-decl
   blobs; interface-satisfaction edges.
3. The reader validates every plane law before lending any row (tiling,
   canonical sort orders, containment, reserved cells, closed tags).
4. Projection never recovers Go facts by scanning source text; two-pass
   declarations-first emission with backward references only; recursive
   self-nominals and mutual recursion project without truncation.
5. Parameter carrier facts carry the exact source parameter names (never a
   blanket `_` when the image carries the name).
6. Rendering goes through `compiler/ir` render.rs displays; golden tests pin
   the exact rendered text of Go structs/interfaces/functions.
7. The lifecycle journey runs against REAL modules fetched through the real
   module-proxy protocol (or checked-in real module zips); published
   fragments from older format generations keep reopening and validating.
8. Line/column positions are NOT carried on the wire when they are
   losslessly derivable from the source digest + byte offsets already
   present; every carried fact must have a consumer or an explicit
   image-only justification.

## Explicit negative space

- No new Rust dependencies beyond the workspace set (ureq already
  workspace-approved for the proxy client).
- No unsafe, no SIMD, no serde on the binary-image boundary (serde remains
  on the legacy JSON transcript only).
- The lane does not rename or re-semant `compiler/ir` types; additions to
  `GoFacts`/lane cells are an authority fork returned to Sol.
- The legacy JSON transcript protocol stays isolated behind the same
  boundary it occupies today.
- Line/column derivation from source offsets is a consumer-side concern,
  not an image-plane concern.

## Baseline (frozen)

Branch canonical, HEAD at freeze: 2c0b26f86 (feat(python): land pyrefly ...)
+ the Go lane's uncommitted v4-image working set, which this capability
commits as its baseline checkpoint before any v5 edit. Per-file baseline
(LOC = formatted line count, digest = SHA-256/16):

```
    20  128d3ea6b681ff9e  compiler/languages/go/Cargo.toml
    19  99db6eaa78cbb548  compiler/languages/go/lib.rs
  2072  0d7a42869aefa45b  compiler/languages/go/image.rs
   988  6ed3a81f4e9946d8  compiler/languages/go/oracle.rs
   657  8ac86c1eedf9a1ff  compiler/languages/go/oracle/main.go
   690  67f95c7b41a8d1cc  compiler/languages/go/oracle/serialize.go
   332  9f3f55bb72b4e42c  compiler/languages/go/oracle/docs.go
  1297  3c83c640d3700312  compiler/languages/go/oracle/image.go
   412  404515e3d035a179  compiler/languages/go/tests/protocol.rs
  3335  0d26e9761a8162f6  compiler/driver/lower/go.rs
   108  c1111ea0b3872294  compiler/driver/tests/go_image.rs
```

## Inherited reds recorded at freeze

- `cargo test -p compiler-driver --lib` does not compile: stale
  `#[cfg(test)]` modules in lower/{clang,typescript,python,rust}.rs (43
  errors) reference APIs moved during the checkpoint. Blocks the whole
  crate's test target; Go's own test module is compile-blocked by siblings.
  First repair card owns exactly that.
- `cargo test -p compiler-driver --test go_image` fails: the integration
  fixture still emits format v1 (`...v1` digest domain, 88-byte header)
  while the reader validates v4. Repair lands with the v5 wire card (the
  fixture must track the new format anyway).
- `go` toolchain absent on this host; oracle tests need
  `NUDOX_GO_ORACLE_BIN` or an installed toolchain. Terra installs a local
  toolchain for evidence; tests keep the typed-unavailable failure mode.

## Affected consumers

- compiler/driver (FactSet/admit, compile_ir, go_image integration test).
- compiler/ir render displays (read-only consumer for goldens);
  enrichment of `build_ir` members/docs touches compiler/driver/lower.rs
  (shared) and is gated on a consumer enumeration in its card.
- compiler/application lifecycle journey (compose-only; no edits planned).
- Sibling language lanes share compiler/driver/lower.rs; any edit there
  enumerates them and runs their focused gates.

## TESTING.md

TESTING.md does not exist in this repository (the skill reference predates
the four-boundary split). Recorded as an evidenced exclusion; the
deliver-reviewed-rust-slice test catalog applies directly and is bound to
proof-matrix rows.
