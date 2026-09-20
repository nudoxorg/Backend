# Forge-only source and package acquisition probe

Status: probe of the current `codex/versioned-engine-v2` worktree, 2026-09-19.

This note is intentionally scoped to source acquisition and package identity. It
does not treat a source declaration fixture as evidence that a repository can be
acquired, resolved, indexed, or queried. The current local scanner is useful,
but it has no forge boundary yet.

## Result

The current path supports a checked-out directory whose source files are
already present. It does not support a project that exists only at a forge URL,
nor does it preserve the revision, repository, package manifest, dependency
graph, or acquisition provenance needed to make that support correct.

The most dangerous failure is silent success:

```text
backend index https://github.com/acme/lib
    -> PackageKey(hash(raw URL))
    -> Path::new(raw URL).is_dir() == false
    -> BuiltinIntent::add(...)
    -> empty ProductSourceRecord::Project
    -> successful Added reply
```

The caller is told that the package was added even though no source was
acquired and no declaration can be searched. This is worse than a typed
unsupported-source error because it poisons the package list and makes an
offline miss indistinguishable from a genuinely empty package.

### Measured active surface

The source scan has three focused unit tests after this probe: one grammar
smoke test, one exact `Arc` declaration-reuse test, and one ignored-tree test.
Its hard limits are 512 KiB per source file, 64 MiB per project, 100,000
source files per project, 16,384 declarations per file, and at most eight
worker threads. These limits bound local work but do not bound remote
acquisition because no remote acquisition path exists yet.

The active source relation has two record variants (`Project` and `File`), and
the Turso extension has three tables (metadata, visible rows, and projection
commits). The Tantivy extension declares no `tantivy` dependency and currently
contains a deterministic in-memory provider. The `Add` command has one
directory predicate: every non-directory coordinate, including a forge URL,
takes the empty compatibility-record branch. These are static measurements of
the active tree, not claims about a live remote benchmark.

A source-tree search found no active `git2`, `gix`, `ls-remote`,
`GitRepository`, `RepoSlug`, or Git subprocess adapter under the local-service,
engine, Turso, or Tantivy extension paths. The only `Command::new` hit in that
scope is a test-executable lease helper. This confirms that the missing forge
behavior is an absent composition plane rather than an adapter hidden behind a
different URL spelling.

The attempted runtime command was:

```text
cargo test -p backend-local-service --offline ingest::tests -- --nocapture
```

It did not reach compilation because workspace resolution could not find the
`gpui` Git dependency in offline mode. Consequently this probe reports no
fabricated latency, throughput, or network numbers; the benchmark matrix below
is the minimum experiment needed once the workspace dependency cache is
available.

## Experiments and code evidence

### 1. Local checkout scan

`crates/local-service/src/builtin/ingest.rs:70-80` canonicalizes the supplied
coordinate and requires it to be a directory. `supported_paths` recursively
walks that directory while skipping symlinks and the generated/vendor trees in
`crates/local-service/src/builtin/ingest.rs:141-182`. Supported file suffixes are
selected by the seven compiled frontends; manifest files are not read.

For a temporary checkout with `src/lib.rs`, `src/extra.py`, `.git/ignored.rs`,
`target/ignored.rs`, and `node_modules/ignored.ts`, the expected current result
is the two source files under `src/`. `.git`, `target`, and `node_modules` are
not source roots. This is a good local safety property and should remain a
contract test.

The scan is parallel and deterministic: paths are sorted before work, each
file is content-versioned, and unchanged records can be reused when the
content and frontend producer versions match (`ingest.rs:82-139` and
`ingest.rs:217-226`). The existing unit test proves declaration allocation
reuse for an unchanged file. The scan root and current working-tree bytes are
the only source facts in that contract.

### 2. Forge URLs and local `file://` URLs

`crates/local-service/src/builtin/commands.rs:116-122` branches solely on
`Path::new(&label).is_dir()`. A normal HTTPS URL, SSH URL, SCP-style Git URL,
or `file://` URL is not a local directory path. It therefore uses
`BuiltinIntent::add`, which constructs a project record with zero files in
`crates/local-service/src/builtin/profile.rs:74-110` and
`crates/engine/src/builtin/relation.rs:80-119`.

There is no URL parser, forge adapter, clone/archive operation, local Git
object reader, or cache lookup on this path. The behavior is independent of
whether the URL is reachable. A nonexistent URL and a real URL are both
accepted into the same empty project representation.

### 3. Branches, tags, and commits

No active product source path calls `git ls-remote`, `git clone`, `git archive`,
`git2`, `gix`, or a forge API. A local checkout is read at its current working
tree. Its `.git` directory is deliberately skipped, so the current source
version cannot distinguish `main`, a tag, a detached commit, or uncommitted
edits that happen to have identical source bytes.

`ProductSourceRecord::Project` currently contains only `label`,
`source_version`, and a file-key frontier (`crates/engine/src/builtin/relation.rs:25-52`).
`ProductSourceRecord::File` contains project key, relative path, language,
content version, analysis version, and declarations (`relation.rs:54-78`).
There is no repository identity, resolved revision, tree ID, selector, manifest
path, dirty state, or commit provenance. A moved branch cannot produce a typed
source revision delta, and a force-pushed tag cannot be distinguished from a
normal reindex.

### 4. Subdirectories and monorepos

The coordinate is also the scan root. Supplying a package subdirectory scans
that directory and makes its user-supplied spelling the package label. There is
no repository-root discovery at ingest time, no manifest package selection,
and no relation between sibling packages. A monorepo can be indexed as one
untyped source tree, but Cargo/npm/Go/Maven/Python workspace package boundaries
and local path/workspace dependencies are absent.

### 5. Dependencies and dependents

The active seven frontend contracts extract declarations only. The source
relation has no manifest/dependency record or edge kind. `Cargo.toml`,
`package.json`, `go.mod`, `pom.xml`, `pyproject.toml`, `*.csproj`, and build
metadata are not parsed by `scan_project`, so a project whose only useful
identity is a forge repository cannot yield dependency edges. The reverse
dependent relation is consequently impossible to compute.

This also means that an empty dependency result cannot be interpreted safely:
it currently conflates “manifest has no dependencies,” “manifest format is not
implemented,” “manifest is malformed,” and “no package manifest was found.”
Those states must be distinct in the next source contract.

### 6. Turso and lexical projection

`extensions/turso/src/lib.rs:20-50` defines a projection of visible view rows,
not a package catalog. It has row identity, labels, documents, package/parent
IDs, a root fence, and a bounded commit audit. It has no repository, source
revision, manifest, dependency, forge URL, mirror, ref watermark, or
acquisition-error tables.

`extensions/tantivy/Cargo.toml` has no Tantivy dependency. The extension is a
bounded provider contract with an in-memory source, not a persistent lexical
index over acquired packages. Therefore a successful local view projection is
not evidence that discovered forge packages are searchable.

### 7. Identity and mirror behavior

`PackageKey` is derived from the raw coordinate text. Equivalent references
such as:

```text
https://github.com/Acme/lib.git
git@github.com:Acme/lib.git
https://www.github.com/acme/lib/
https://mirror.example/acme/lib
```

become unrelated package keys. There is no canonical forge repository identity,
observed URL set, mirror trust policy, or fork/lineage relation. A registry
package that declares a repository cannot be joined to a forge-only package
without external ad hoc string handling.

### 8. Offline and authentication behavior

Local indexing is naturally offline because it reads the checkout. A forge
coordinate has no acquisition mode at all: offline miss, unavailable network,
invalid URL, auth-required remote, and unsupported transport are all currently
accepted as an empty package. No secret is sent today, but that is because the
remote path does not exist, not because an acquisition boundary enforces a
policy.

Any future Git subprocess must retain the old implementation's safety rules as
an explicit adapter contract: argv-only invocation, `GIT_TERMINAL_PROMPT=0`,
neutral global/system config, refusal of `ext::` and `fd::`, bounded output,
typed failures, and no credentials in logs or content-addressed objects. The
historical implementation at
`/Users/mileswirht/Downloads/backend_1/workspace/index/ingest/git.rs` already
has focused tests for these properties and is useful as a behavior oracle.

## URL and source matrix

| Input | Current behavior | Required behavior |
| --- | --- | --- |
| Absolute local checkout | Scans supported source files; reuses matching records | Preserve, plus repository/ref/dirty provenance when available |
| Local Git checkout at a tag or detached commit | Scans current tree; no ref or commit identity | Capture `HEAD`, resolved commit/tree, selector, and dirty status |
| `file://` checkout or bare repository | Empty compatibility package | Read local Git objects or materialize a selected tree |
| `https://github.com/...` | Empty compatibility package | Resolve repository, selector, and optional package subpath; acquire from cache/network |
| `https://gitlab.com/...` | Empty compatibility package | Same, including subgroup paths |
| Codeberg/Forgejo/SourceHut URL | Empty compatibility package | Generic Git transport plus host metadata adapter where available |
| `git@host:owner/repo.git` / `ssh://...` | Empty compatibility package | Auth-safe Git transport, with explicit auth-required failure |
| Branch selector | No selector grammar | Track moving selector and resolved commit; refresh to a new source version |
| Tag selector | No selector grammar | Pin tag observation and peeled commit; detect force movement |
| Commit selector | No selector grammar | Verify exact object and expose immutable package version |
| Forge repo with no registry identity | No package facts or source | Repository identity is the package stem; manifest/package identity is optional |
| Mirror/fork | Distinct raw package key | Canonical repo identity plus observed URL/mirror and lineage evidence |
| Offline cache hit | No cache | Read immutable tree/archive/CAS by exact source identity |
| Offline cache miss | Empty success | Typed cache miss; never invent an empty package |

## Replacement architecture

The forge plane should be a source adapter below the existing versioned engine,
not a special `Add` branch in locald. Separate user coordinates, canonical
identity, acquisition, and indexed package selection:

```text
SourceCoordinate
  ├─ LocalPath { path, package_selector }
  ├─ Git { repo, selector, package_selector }
  └─ Archive { locator, checksum, package_selector }

RepoIdentity { host, normalized_path, forge_kind }
RevisionSelector { DefaultBranch | Branch(name) | Tag(name) | Commit(oid) }
ResolvedRevision { commit_oid, tree_oid, selector_observed, parents, timestamp }
SourceSnapshot { repo, revision, subtree, manifest_digest, files_root, provenance }
PackageSelection { manifest_path, package_name, language, workspace_root }
```

Suggested invariants:

* `RepoIdentity` is normalized from a URL but never replaces the exact
  transport URL. Preserve every observed URL as provenance and choose a
  transport through an explicit trust/auth policy.
* `ResolvedRevision` is immutable and content-addressed by repository identity,
  commit/tree, selected subdirectory, and acquisition recipe. Branch and tag
  names are selectors, not versions. A moved selector creates a new resolved
  revision; it must never rewrite an old one.
* A dirty local worktree gets a separate `Worktree` origin. If `HEAD` is known,
  record it as a parent observation, but do not claim that uncommitted bytes
  came from the commit object.
* A repository, workspace package, package version, source file, manifest, and
  dependency edge have separate typed identities. A forge-only repository can
  exist without a registry package name. A single repository can contain many
  package selections and language ecosystems.
* Acquisition returns an immutable CAS tree/archive plus a typed outcome
  (`CacheHit`, `Fetched`, `AuthRequired`, `UnsupportedTransport`, `NotFound`,
  `OfflineMiss`, `Corrupt`, or `BudgetExceeded`). No outcome is represented by
  an empty source relation.
* Manifest readers return `Read(empty)`, `Unsupported`, `Malformed`, or
  `Missing` distinctly. Dependency edges retain the declaration as written,
  resolution mode, selected target (when known), and the source revision that
  justified it. Reverse dependents are an arrangement over those edges.

The first production relation extension should therefore add independently
versioned rows for `Repository`, `ObservedRemote`, `Ref`, `ResolvedRevision`,
`PackageSelection`, `Manifest`, `Dependency`, and `Acquisition`. Existing
`Project`/`File` rows can remain a compact source projection keyed by the
resolved snapshot. Their project frontier then changes only for files affected
by a new snapshot, while metadata/ref/dependency deltas propagate through
their own arrangements.

Turso should own a root-bound materialization of those rows, with schema
version, source/workspace root, and per-feed/ref watermarks. Tantivy documents
should include the package/repository/revision IDs and exact lexical recipe;
search replies must carry that binding so a hit from a previous branch head
cannot be rendered as current. A separate graph arrangement should materialize
forward and reverse dependency edges; it should not infer dependents by scanning
all package rows at query time.

## Focused test and benchmark matrix

Before claiming forge support, add deterministic local fixtures and a fake
remote adapter. The tests need no network:

1. Create a real temporary Git checkout and bare repository with main, a second
   branch, lightweight and annotated tags, a detached head, a forced tag move,
   and an untagged head. Assert ref enumeration, peeled OIDs, exact commit/tree
   identities, and immutable historical snapshots.
2. Run the same fixture through GitHub, GitLab, Codeberg, SourceHut, and
   Forgejo-shaped URL forms by feeding scripted advertisement responses to the
   adapter. Include subgroup paths, `.git`, trailing slash, HTTPS, SSH, SCP,
   `file://`, and mirror aliases.
3. Test package selection for a monorepo with nested Cargo/npm/Go/Java/Python/C#
   manifests, workspace/path dependencies, package names different from folder
   names, and a missing or malformed manifest. Verify that unsupported and
   malformed are never read as empty dependency sets.
4. Test URL/auth hardening with `ext::`, `fd::`, option-shaped URLs, prompt
   suppression, hostile global config, malformed advertisements, oversize
   archives, traversal/symlink entries, and credentials absent from errors,
   logs, and CAS bytes.
5. Test cache behavior across restarts: exact hit performs zero network and
   zero frontend work; an offline miss is typed; a changed commit reuses
   unchanged file/manifest/dependency objects; a mirror hit preserves the
   canonical repository identity.
6. Exercise the full local daemon, CLI, MCP, Turso, lexical search, and graph
   path over the fixture. Record p50/p95/p99 and bytes/work for cold clone,
   warm cache, branch move, one-file edit, dependency-only edit, and force-push
   cases. The key delta claim is “work proportional to changed snapshot
   objects plus dependency fan-out,” with the fan-out reported rather than
   hidden.

The historical backend has a strong starting oracle for direct Git behavior in
`workspace/index/tests/ingest_enumerate.rs`: both an in-process adapter and a
subprocess adapter are exercised against real tagged/untagged repositories;
annotated tags pin peeled OIDs; untagged repositories get a pseudo-version;
monitor watermarks emit no work when the ref digest is unchanged; and hostile
transports are rejected. Those tests should be ported as behavioral contracts,
then extended to the new typed source/revision relation instead of copied as a
second catalog implementation.

For scale, that oracle currently contains 13 direct-ingest tests and 38
repository-URL normalization tests. It covers GitHub, GitLab, Codeberg, and
generic hosts, but the known-forge reduction table does not name SourceHut or
Forgejo. Generic-host parsing still preserves their full path, so it is not a
transport blocker, but forge-specific archive/blob/release URL reduction and
metadata adapters need explicit fixtures for those hosts.

## Priority

* **P0:** stop treating non-directory coordinates as successful empty adds;
  return an explicit deferred/unsupported acquisition result until the forge
  adapter lands.
* **P0:** add source/repository/revision identity before wiring remote ingest;
  raw URL hashes cannot be the package identity.
* **P0:** add a bounded acquisition/CAS boundary with offline and auth-safe
  behavior, then feed its immutable snapshots into the existing content reuse
  path.
* **P1:** parse package manifests into typed dependency edges and maintain
  reverse dependents incrementally.
* **P1:** project package/revision/provenance metadata into Turso and bind
  lexical/graph query results to the same immutable root.
* **P2:** add forge-specific metadata adapters (stars, default branch, releases,
  owners, language hints) only after Git source truth works. Forge API metadata
  must remain optional and must not gate source indexing.
