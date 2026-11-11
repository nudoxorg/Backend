# Development companion builds

A plain `cargo build --locked` builds the Rust workspace. Several native
frontends also need companion executables. A successful Cargo build alone does
not mean those frontends are available. Missing companions remain explicit
capability gaps in the owner; they do not select a syntax-parser fallback.

## C# / Roslyn

Use an installed .NET SDK that can target net8.0. Publish the pinned Roslyn
helper and its entire dependency closure into a new development directory:

```sh
python3 tools/package/build_roslyn_development.py --output target/roslyn-development
source target/roslyn-development/environment.sh
cargo build --locked --offline -p backend-locald -p backend-cli -p backend-mcp -p backend-desktop
```

The builder restores in locked mode using cached NuGet packages by default.
If the pinned packages have not been cached, explicitly pass `--allow-downloads`.
`--dotnet /absolute/path/to/dotnet` selects a specific SDK. The environment file
sets `NUDOX_DOTNET` and `NUDOX_ROSLYN_HELPER`; source it in the environment that
launches the development owner. Restart an already running development owner
before testing the new helper. Publish output includes the runtime configuration,
Roslyn assemblies, and transitive dependencies; copying just `oracle.dll` fails.
The builder isolates intermediate files from the checked-in helper sources and
refuses to replace an existing output directory.

This framework-dependent development output requires the selected .NET runtime.
It is not a release bundle or a distribution admission receipt. macOS release
packaging separately validates its managed-host/runtime closure.

## SQLite

SQLite is a separate Cargo workspace. Building it alone does not bind the owner
to its executable. To build an anchored development bundle and exercise public
SQL commands, use:

```sh
bash tools/package/run_sql_public_journey.sh target/sql-development target/sql-journey
```

The script builds the syntaqlite worker from the standalone lockfile, observes
its executable identity, compiles that identity into the owner, and stages the
worker beside the owner under the fixed `backend-sqlite-worker` name. This
journey builds the CLI, MCP server and owner. For desktop development, the desktop
must also be compiled with the same `NUDOX_BUILD_SQLITE_WORKER_ID` and use the same
worker sibling. Release packaging uses `sqlite_linux_development.py` or
`sqlite_macos_development.py` to retain the platform closure and recheck identity
after relocation/signing; a development journey is not release evidence.

## CSS / Stylo and PostgreSQL

Stylo's workspace crate builds `backend-stylo-worker`. The owner needs an exact
compiled worker identity and, on Linux, the admitted private loader/library
closure. Use the existing source-receipted worker/owner packaging stages
(`record_stylo_worker_build.py`, `record_stylo_owner_build.py`, and the macOS
companion equivalents). Merely placing a worker on PATH does not authorize it.
The worker and owner must use the same compiler.

PostgreSQL's pgrust worker is built out of tree because of its AGPL license.
There is no in-tree PostgreSQL worker workspace to add to the main Cargo build.
An owner requires the final `backend-postgres-worker` sibling and its compiled
identity. Do not copy pgrust sources into this repository or treat an arbitrary
runtime worker path as an admitted compiler.
