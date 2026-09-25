# PID identity

How the index names a package. Normative for new identity code. The types live
in `workspace/index/pid.rs`.

## What a PID system actually requires

Handle (RFC 3650), DOI (ISO 26324), ARK, and the RDA PID kernel agree on one
split: the **identifier** is not the **location**. A resolver binds an opaque
name to kernel metadata (type, checksum, date, current URLs). URLs rot; the
name does not. Withdrawn objects stay resolvable as tombstones. DOI and Handle
suffixes are opaque on purpose — a name that embeds a host, a version scheme,
or a path breaks when that fact changes.

Software adds two more layers those systems already use, under different names:

| Claim | PID analogue | What it identifies | Stored as |
|---|---|---|---|
| Concept | Zenodo concept DOI | The work, across versions | Opaque `PackageId` (`ConceptPid`) |
| Version | Zenodo version DOI | One release coordinate | A different opaque id (`VersionPid`) |
| Content | SWHID (ISO 18670), gitoid, registry checksum | The bytes | The digest itself (`ContentPid`) |
| Location | DOI kernel URL | Where to fetch today | Kernel field, not identity |
| Rendering | purl, `swh:`, `doi:` | An export spelling | Computed, never a key |

Package URL names a **version coordinate** (`pkg:type/namespace/name@version`).
It does not name bytes: one version string can be rebuilt. SWHID names
**bytes**, and only git SHA-1 objects. BLAKE3 and SHA-256 are content PIDs of
other algorithms; they are not SWHIDs. CPE is not used (no content binding, no
canonical C/C++ names).

`docs/GLOBAL-IR-GRAPH.md` §1 says package identity *is* the purl. That
collapses concept, version, and location into one mutable string. This
document does not. `docs/REGISTRYLESS-PLAN.md` RL-8 (purl and SWHID are
renderings) is the rule that matches PID practice.

## Laws

1. Concept, version, and content are different types. A version bump changes
   `VersionPid` only. A rebuild with the same version string changes
   `ContentPid` only.
2. Assigned ids are opaque UUIDv5 names from `package_id_from_parts`. A local
   name that is `pkg:`, `swh:`, `doi:`, `ark:`, or a URL is rejected.
3. A location change does not change the PID. Withdrawal resolves to a
   tombstone of the same PID.
4. `render_purl` and `render_swhid` are pure. `render_swhid` returns a value
   only for `ContentDigest::GitSha1`.
5. The concept PID uses the same framing as the catalog package id
   (ecosystem, then canonical stem). Version PIDs add a `"version"` domain
   part so they cannot collide with that package id.

## Sources

- Handle System, RFC 3650; DOI Handbook, ISO 26324.
- ARK Alliance, comparison of ARK / DOI / Handle / PURL (identifier vs. the
  resolver’s metadata kernel).
- RDA PID Kernel recommendation (minimal metadata bound to the identifier).
- Zenodo DOI versioning: concept DOI vs. release DOI.
- Software Heritage persistent identifiers, ISO/IEC 18670 (SWHID).
- Package URL specification (coordinate scheme, qualifiers for vcs URL and
  checksum — the checksum is not the purl).
- RSQKit “Software identifiers” (2026-03-31): semver, DOI, checksum, UUID, and
  git hash answer different questions and are used together.
