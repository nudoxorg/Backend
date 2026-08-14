# IR-Native VCS Research

**Status:** Draft Rev 3.2 — Greenfield arena IR + universal Change; no dual-write; no conflicted pristine

## Authoritative design

| Document | Role |
|---|---|
| [`design/IR-NATIVE-VCS-DESIGN.md`](./design/IR-NATIVE-VCS-DESIGN.md) | Normative design (Rev 3.2) |

## Briefs

Briefs are **research context, not normative**. Each carries a status banner naming which of its recommendations Rev 3.2 superseded (conflict-as-state in 01/05, dual-write in 02, `NudoxPath`-keyed deltas in 04). Where a brief and the design disagree, the design's Key Decisions table (K1–K28) wins.

| Brief | Topic |
|---|---|
| [`briefs/01-pijul-primitives.md`](./briefs/01-pijul-primitives.md) | Pijul / libpijul / patch theory |
| [`briefs/02-zero-copy-ir.md`](./briefs/02-zero-copy-ir.md) | rkyv / POD / mmap IR storage |
| [`briefs/03-existing-system.md`](./briefs/03-existing-system.md) | Live registry ground truth (context) |
| [`briefs/04-diff-and-search.md`](./briefs/04-diff-and-search.md) | Structural diff + type-directed search |
| [`briefs/05-prior-art-papers.md`](./briefs/05-prior-art-papers.md) | Prior art |
| [`briefs/06-new-ir-rewrite.md`](./briefs/06-new-ir-rewrite.md) | Target IR rewrite (authoritative model, POC snapshot) |

## Implementation entrypoint

Implement in PR order (design § "PR Plan") — each PR names its gate; the design sections that back it are listed here:

| PR | Builds | Normative design sections |
|---|---|---|
| PR-0 | `nudox-change` + stamp hygiene | Core newtypes · Universal Change Algebra · Appendix A/F |
| PR-1 | `nudox-ir` core | Entry model · Symbol parity · KindDiscriminant/KindWire · IrAtom + atom order · IntroId bootstrap · Appendix G wire twins |
| PR-2 | PackageArchive + yoke | POD headers · PackageArchive Layout · type skeleton + opcode table · deterministic seal · Yoke API |
| PR-3 | Manifest + generation identity | BlobManifest v3 · GenerationStamp identity_bytes v3 · Outbox |
| PR-4 | InMemory ChannelStore | Linear single-writer apply · normative laws · errors |
| PR-5 | LibpijulChannelStore | libpijul boundary (Issue 9) · ChannelStore trait |
| PR-6 | Resolvers + ensure_package | RegistryResolver + Sync |
| PR-7 | IR sync + RequiredClosure | Generation Ready (Issue 4) · ChangeSetFingerprint · RPC mapping |
| PR-8 | IrToGraph + cold GraphStore | Terminus Projector (Issue 15) · GraphAtom |
| PR-9+ | Language frontends | Kinds · Diff & Record Path · Appendix D publish |

## Product shape

Package IR seals into an **IR-only PackageArchive** (yoke/mmap, no source text). Lineage is a **single-writer channel** of `Change<IrAtom>` keyed by `IntroId`, with optional **libpijul** durable log. `RegistryResolver` loads local archives and fetches remote IR. Terminus stays hot graph; `IrToGraph` projects the same change envelope. Universal apply lives in `nudox-change`.

## Non-negotiables

- Yoke never wraps source files.
- Strong newtypes (`IntroId`, `ChangeId`, `CasKey` ≠ `GenerationStamp`, …).
- GPL OK — libpijul behind typed `ChannelStore`.
- **No dual-write / Index fallback.** Migration later = versioned formats + optional offline importer only.
- **No conflicted pristine.** Single-writer linear apply; races fail closed.
