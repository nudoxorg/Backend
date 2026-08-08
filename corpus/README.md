# corpus

A hash-pinned, reproducible package corpus: 20 packages per language across
seven ecosystems (`crates.io`, `go`, `maven`, `nuget`, `npm`, `pypi`, `cpp`),
used to exercise the producers against real-world code instead of hand-written
fixtures.

- `manifest.toml` — the package list: ecosystem, name, and one or more
  `{ version, hash }` entries.
- `fetch.nu` — materializes `manifest.toml` into `.real-crates/` (gitignored;
  see AGENTS-DOCTRINE.md §8 before ever running `rm -rf` in there). This is
  the environments-without-Nix path. `flake.nix`'s `buildCorpus` is the
  Nix-native fixed-output-derivation path for `crates.io` entries; it does not
  (yet) cover the other six ecosystems, so treat `fetch.nu` as the source of
  truth for the full corpus.

## Why a hash at all

Every entry is content-addressed: `hash` is a nix-base32-encoded sha256 of the
exact bytes at the download URL (the same encoding `nix-prefetch-url` prints,
and the same one Nix fixed-output derivations use). `fetch.nu` downloads,
hashes, and compares *before* extracting anything — a mismatch is a hard
failure (loud error, non-zero exit, nothing written to `.real-crates/`). This
matters more than it sounds: a fetcher that silently accepts whatever bytes a
registry hands back on a given day is not reproducible, it just looks
reproducible until the day it isn't.

## Adding a package

1. Decide the ecosystem, name, and version. Read the per-ecosystem table
   below for what "name" means in that ecosystem (it isn't always just the
   package name — maven wants `groupId:artifactId`, go wants the full module
   path, cpp wants an explicit URL).
2. Get its hash:

   ```bash
   # With Nix:
   nix-prefetch-url --type sha256 <url>

   # Without Nix (or to sanity-check the URL convention itself):
   nu corpus/fetch.nu hash-url <ecosystem> <name> <version> [--url <override>]
   # e.g.
   nu corpus/fetch.nu hash-url crates.io regex 1.10.3
   nu corpus/fetch.nu hash-url go github.com/google/uuid v1.6.0
   nu corpus/fetch.nu hash-url cpp nlohmann-json v3.11.3 \
     --url https://github.com/nlohmann/json/releases/download/v3.11.3/include.zip
   ```

   `hash-url` prints a ready-to-paste `{ version = "...", hash = "..." }`
   snippet. It does not require the package to already be in the manifest,
   and it does not write anything to `.real-crates/`.

3. Add or extend a `[[packages]]` block in `manifest.toml`. If a package with
   that `(ecosystem, name)` already exists, add the new `{ version, hash }`
   to its `versions` list instead of creating a second block — lineage
   testing (`nudox-graph` version-history queries) depends on multiple
   versions living under one package entry, not scattered across duplicates.
4. Run `nu corpus/fetch.nu` from the repo root. It only fetches what's
   missing (`SKIP: ... already present` for everything else), so re-running
   after adding one package is cheap.

Never hand-write a hash. A fabricated or guessed hash is worse than no entry
at all — see AGENTS-DOCTRINE.md §6 on fabricated fixtures.

## Per-ecosystem conventions

| ecosystem | `name` means | fetches | why |
|---|---|---|---|
| `crates.io` | crate name | `.crate` (tar.gz) from static.crates.io | crates.io only ever serves source; there's no separate binary artifact to avoid. |
| `go` | full module path, e.g. `github.com/google/uuid` | module zip from `proxy.golang.org` | The module proxy is the same thing `go mod download` uses; module zips are source by construction. Path/version are escaped per Go's module-proxy convention (every uppercase letter becomes `!`+lowercase) — see `github.com/BurntSushi/toml` in the manifest for a real example that exercises this. |
| `maven` | `groupId:artifactId` | the **sources** jar (`-sources.jar` classifier) from `repo1.maven.org` | The default jar is compiled `.class` bytecode. Maven Central separately publishes a sources classifier for exactly this reason; asking for anything else would hand the producer bytecode instead of Java. |
| `nuget` | package ID as published | the **binary** `.nupkg` from `api.nuget.org`'s v3 flat-container endpoint | Unlike the other five source ecosystems, NuGet packages do not generally embed C# source — a `.nupkg` is compiled DLLs (per-TFM under `lib/`) plus a `.nuspec`. There is no NuGet-wide "sources package" convention comparable to Maven's classifier or PyPI's sdist (some packages publish symbol packages, inconsistently, for a different purpose). This was scoped explicitly (see the task's ecosystem table) — it is a real, intentional gap in this corpus, not an oversight. If the C# producer needs to document real C# *source*, its packages will need a different source (GitHub, symbol server with embedded source, etc.); flagging it here so it isn't discovered by surprise later. |
| `npm` | package name, including scope (e.g. `@types/node`) | tarball from `registry.npmjs.org` | npm tarballs are source (usually already-transpiled JS plus, for typed packages, `.d.ts`). For scoped packages the tarball filename drops the scope (`@scope/name` → `name-version.tgz` under `@scope/name/-/`); `fetch.nu` handles that. |
| `pypi` | project name | the **sdist** (`packagetype: sdist`), not a wheel, from `pypi.org`'s JSON API | Wheels are frequently pre-built/pre-compiled and platform-specific, and don't reliably contain full source for extension modules. The sdist is what PyPI itself calls a source distribution. `fetch.nu` calls PyPI's JSON API (`/pypi/<name>/<version>/json`) to find the real sdist URL rather than guessing a filename pattern (sdist filenames aren't uniformly `name-version.tar.gz` — underscores vs. hyphens vary by project). |
| `cpp` | short slug, e.g. `nlohmann-json` | **explicit `url` per version** — no computed convention | There is no C++ package registry, so unlike every ecosystem above there's nothing to template a URL from. See below for the scheme actually used. |

### cpp in more detail

Every `cpp` manifest entry sets `url` explicitly rather than relying on a
computed convention. Two sub-cases, both present in the manifest:

- **Maintainer-uploaded GitHub Release asset** (preferred): a file the
  project owner attached to a Release, e.g.
  `github.com/nlohmann/json/releases/download/v3.11.3/include.zip`. These are
  stored blobs — GitHub never regenerates them — so they're as stable as any
  other content-addressed download. 12 of the 20 cpp entries use this.
- **GitHub tag source archive** (`/archive/refs/tags/<tag>.tar.gz`), used for
  the 8 projects that don't publish a release asset at all (`spdlog`,
  `cxxopts`, `tomlplusplus`, `date`, `cereal`, `range-v3`, `cpp-httplib`,
  `argparse`). These are auto-generated on the fly by `codeload.github.com`
  from the tag, and GitHub has in the past changed the exact bytes it
  produces for a given ref (tar/gzip implementation changes on their end,
  not any change to the tagged content). This is a real, known reproducibility
  risk that the "pick GitHub release tarballs" brief accepted implicitly.
  The mitigation is the hash check: if the bytes ever drift, `fetch.nu` fails
  loudly instead of silently absorbing a different corpus, which is the
  correct failure mode for something outside our control.

Two entries (`simdjson`, `catch2`) are raw single-file release assets — a
`.h`/`.hpp`, not an archive. `fetch.nu` detects this (no recognized archive
extension) and places the file itself into the package directory instead of
trying to `tar`/`unzip` it.

## The `[workspace]` trap — and why it's Rust-only here

AGENTS-DOCTRINE.md §8 explains the underlying issue: Cargo auto-promotes
in-tree path dependencies into the root workspace, so every `.real-crates/*`
checkout that has a `Cargo.toml` needs an empty `[workspace]` table appended,
or `cargo metadata` (and rust-analyzer) fails claiming the package "believes
it's in a workspace when it's not." `fetch.nu` does this automatically for
every fixture that has a `Cargo.toml` (in practice, only `crates.io` ones).

This repo has no analogous trap for the other six ecosystems: there is no
root `package.json` (npm/yarn workspaces), no root `pyproject.toml` ([tool.*]
workspace globs), no root `.sln` (MSBuild), and no root `go.work` (Go
workspaces) — checked directly before writing this. A `go.mod`, `pom.xml`,
`.csproj`, or `package.json` sitting under `.real-crates/` is inert as far as
this repo's own build tooling is concerned; nothing here scans for one. If a
root manifest of one of those kinds is ever added to the repo for an
unrelated reason, this note is the place to come back and re-check.

## Multiple versions / lineage

At least two packages per ecosystem carry more than one version, for
`nudox-graph`'s version-lineage queries:

| ecosystem | packages with >1 version |
|---|---|
| crates.io | `log` (2), `memchr` (3) — pre-existing |
| go | `github.com/pkg/errors` (2), `github.com/stretchr/testify` (2) |
| npm | `lodash` (2), `zod` (2) |
| pypi | `click` (2), `pydantic` (2 — spans the 1.x→2.x rewrite, a real breaking-change lineage case) |
| maven | `com.google.guava:guava` (2), `com.fasterxml.jackson.core:jackson-databind` (2) |
| nuget | `Newtonsoft.Json` (2), `Serilog` (2) |
| cpp | `nlohmann-json` (2) |

## `fetch.nu` usage

```bash
# Materialize everything missing from .real-crates/ (idempotent — already-
# present packages are skipped, not re-downloaded or re-verified).
nu corpus/fetch.nu
nu corpus/fetch.nu --output-dir /some/other/path --threads 8

# Hash a candidate package without adding it to the manifest first.
nu corpus/fetch.nu hash-url <ecosystem> <name> <version> [--url <override>]
```

A hash mismatch on any single package does not stop the others (they run
concurrently and independently), but the run exits non-zero and prints every
mismatch, so it can't be missed in CI output.
