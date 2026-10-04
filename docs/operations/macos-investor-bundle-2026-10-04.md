# macOS investor bundle: build, package, and verify

This runbook defines a fail-closed Apple Silicon packaging protocol. The operator supplies the full commit and tree SHAs of the reviewed, clean release source; neither script embeds a release-source pin. The packager is [`tools/package/macos-investor-bundle.py`](../../tools/package/macos-investor-bundle.py), and [`tools/package/build-macos-investor-app.py`](../../tools/package/build-macos-investor-app.py) performs one locked application build and writes its binary receipt. Both preserve the exact source manifest in their evidence. The current deliverable is a packaging plan and verifier, not a verified investor release.

At preparation time this lane has no exact-revision application binaries or helper receipts, so no Mach-O closure or bundle launch has been verified. The source has no custom `.icns` app icon, and the current `Info.plist` declares no icon. The read-only host audit found zero valid signing identities; signing and notarization have not been performed. A finished investor release therefore remains pending exact builds, external helper inputs, native QA, a reviewed app icon, and an available signing/notarization path.

## Build inputs

Use a clean checkout at the full commit and tree SHAs selected for the reviewed release. The initial native target is `aarch64-apple-darwin` (Apple Silicon); the build and package scripts reject a host/target architecture mismatch. Choose the pin only after the code is frozen; do not carry forward an older commit or accept a dirty checkout.

The application build is one serial Cargo invocation through a Root-supplied, content-pinned runner that already selects the saved environment and Cargo wrapper. The packaging helper does not evaluate Nix or clear compiler/cache environment inherited from that runner. It verifies the runner hash and records hashes for its absolute interpreter, saved environment, compiler, and wrapper references without copying host paths into the app receipt. The runner's Cargo wrapper remains responsible for capacity/cache policy and produces the schema-2 invocation provenance. The build reuses the worktree-owned `.local/target` when that wrapper verifies the cache path; only the copied artifact/receipt output directory must be new. The receipt binds source manifests before and after the build and hashes the three executables without claiming that the target cache was fresh. **Do not run this workload until Root explicitly admits its build capacity.**

```sh
SOURCE=/path/to/clean/backend-at-reviewed-release
REVISION=<full reviewed release commit SHA>
TREE=<full reviewed release tree SHA>
OUT=${TMPDIR:-/tmp}/nudox-investor-inputs-$REVISION
CARGO_RUNNER=/private/tmp/nudox-current-gui-candidate-command-20261004-v2.sh
RUNNER_SHA256=3868617cf7238a5d8760c8ef1dad91d3f1b33be617e1bf559261102137baab5f
test "$(git -C "$SOURCE" rev-parse HEAD)" = "$REVISION"
test "$(git -C "$SOURCE" rev-parse HEAD^{tree})" = "$TREE"
test -z "$(git -C "$SOURCE" status --porcelain --untracked-files=all)"
python3 "$SOURCE/tools/package/build-macos-investor-app.py" \
  --source-root "$SOURCE" \
  --expected-revision "$REVISION" \
  --expected-tree "$TREE" \
  --cargo-runner "$CARGO_RUNNER" \
  --expected-runner-sha256 "$RUNNER_SHA256" \
  --target aarch64-apple-darwin \
  --output-dir "$OUT"
```

The supplied runner file is intentionally non-executable; the builder invokes its captured absolute shebang interpreter and then the script without changing its mode. Use this path/digest only if Root admits that exact runner for this build and the hash still matches. A later admitted runner requires its own reviewed path and digest. The application receipt has `schema: 1`, the exact clean source identity (`git_revision`, `git_tree`, `cargo_lock_sha256`, `working_tree: "clean"`) in `source`, `source_before`, and `source_after`, `source_unchanged: true`, `target: "aarch64-apple-darwin"`, `profile: "release"`, `locked_build: true`, the normalized interpreter/script invocation prefix plus Cargo command, and an `executables` map from each of `backend-desktop`, `backend-mcp`, and `backend-locald` to its SHA-256. It records matching before/after runner and referenced-file digests, plus the explicit runner pin, role-keyed environment/tool hashes, Cargo provenance record digest, unchanged-source/lock/tool identities, target selection, and relative output hashes. Local worktree paths are omitted from the copied receipt. The packager independently checks the source and runner claims against explicit operator pins and verifies the application binary digests against the supplied files.

The C# authority is a real Roslyn helper, separate from app startup. Build it from the pinned `frontends/csharp/src/legacy/helper/oracle.csproj` and its checked-in `packages.lock.json` using the pinned .NET SDK, a fresh output directory, the `osx-arm64` runtime identifier, framework-dependent deployment, and locked package restore. One reproducible shape is a fresh `dotnet restore ... --runtime osx-arm64 --locked-mode` followed by `dotnet publish ... --runtime osx-arm64 --no-restore --output <fresh-output>`. The receipt must bind both project/lock hashes to the pinned source, record `locked_restore: true`, `fresh_output_dir: true`, `framework_dependent: true`, `runtime_identifier: "osx-arm64"`, both commands, hashes for every file in the publish output, and a nonempty `notices` map whose paths name Roslyn license/third-party notices included in that output. The packager refuses partial or unreceipted publish directories.

The other relocatable producers must also be real receipt-bound inputs. `--helpers-dir` contains `go/oracle` (built from every source under the pinned Go oracle directory), `python/pyrefly` (the real Pyrefly executable), `typescript/node/bin/node`, and the complete real `typescript/node_modules/typescript` package. Its receipt contains `schema: 1`, the pinned source revision/tree, target triple, a complete `files` map of relative path to SHA-256, a `tools` map with Go oracle/Pyrefly/Node executable hashes and recorded versions plus the npm TypeScript version, a complete `go_source_files` hash map, `typescript_driver_sha256`, and a `notices` map for Go helper dependencies, Pyrefly, Node, and TypeScript. The packager creates only thin launch scripts that invoke the supplied real tools. It places `main.cjs` beside the TypeScript module tree so Node resolution no longer points at `CARGO_MANIFEST_DIR` from the build host.

Supply a real macOS .NET runtime root containing `dotnet`, `host/fxr`, `shared/Microsoft.NETCore.App`, `LICENSE.txt`, and `ThirdPartyNotices.txt`. The packager copies the host and runtime (not the SDK), the Roslyn publish output, all four OFL font notices used by `facet`, and all three build receipts. It writes `Contents/Resources/build-manifest.json`, then creates `Nudox-macOS.zip` with `ditto` and a sidecar receipt containing the manifest and archive hashes. The manifest is stable for the same exact inputs; the zip itself is hashed but is not claimed to be byte-for-byte reproducible because archive metadata may vary.

Example package command after Root supplies both sets of binaries and receipts:

```sh
python3 tools/package/macos-investor-bundle.py \
  --source-root "$SOURCE" \
  --expected-revision "$REVISION" \
  --expected-tree "$TREE" \
  --expected-runner-sha256 "$RUNNER_SHA256" \
  --artifact-dir "$OUT/artifacts" \
  --build-receipt "$OUT/application-build-receipt.json" \
  --dotnet-root /path/to/real/osx-arm64/dotnet-runtime \
  --roslyn-dir /path/to/receipted/osx-arm64/oracle-publish \
  --roslyn-receipt /path/to/roslyn-build-receipt.json \
  --helpers-dir /path/to/receipted/compiler-helpers \
  --helpers-receipt /path/to/compiler-helpers-receipt.json \
  --output-dir /path/to/new/Nudox-package \
  --target aarch64-apple-darwin
```

The output directory must not already exist. The package step verifies Info.plist, every Mach-O architecture and minimum macOS version, every declared Mach-O load dependency under its process root and ordered `LC_RPATH` stack, font/runtime/helper notices, and all input receipts. It follows dyld's linked run-path order: the current loader image's `LC_RPATH` entries precede entries inherited from its loader chain. A `LC_ID_DYLIB` printed by `otool -L` is recorded as the image ID and excluded from load edges. A system dependency is accepted only when `dyld_info` confirms the requested architecture in the host dyld shared cache or at the referenced system path. It independently checks the Cargo runner receipt against the operator-supplied runner digest. It rejects a missing bundled dependency, a `/nix/store`/Homebrew dependency or run path, an unrecognized install name, a stale source checkout, or a binary whose hash differs from its receipt. Static load-command closure cannot prove libraries opened later with `dlopen`; Clang/libclang remains a separate language-tool prerequisite.

The app bundle uses a small shell entrypoint to point the child compiler owner at the bundled real .NET host/Roslyn DLL, Go oracle, Pyrefly, and all four typed TypeScript hooks: Node, module root, report program, and `tsc`. The generated TypeScript wrappers use the selected `NUDOX_TYPESCRIPT_NODE` path consistently. It keeps the Rust desktop executable and its `backend-locald`/`backend-mcp` siblings together in `Contents/MacOS`. This launcher change has not yet been exercised through Launch Services; native startup verification remains a release gate.

## Launch versus language compilation

The app can start without project, index, compiler, or Nix environment variables. The existing first-run path creates per-user application state and opens the folder chooser. The embedded fonts and Metal shader library are part of the native executable; no font or shader runtime download is needed. The app also has its local service and MCP companion beside the desktop executable.

Language compilation is optional and is discovered independently. A Finder launch does not inherit an interactive shell's `PATH` or Nix development-shell variables. The host checks configured absolute paths plus fixed clean-machine locations: Homebrew (`/opt/homebrew/bin`, `/usr/local/bin`), user-local and Nix profile bins, and selected platform paths (`/usr/bin/clang`, `/usr/bin/python3`, `/usr/local/go/bin/go`, the standard Homebrew/macOS JDK roots, and `/usr/local/share/dotnet/dotnet`). Rust additionally checks rustup's Cargo bin and pairs `rustc` with `cargo`. The package launcher explicitly supplies relocatable Go, Pyrefly, Node/TypeScript, and Roslyn helper paths, but native compilers and their workspace/cache context remain prerequisites: Clang needs `clang` and compatible `libclang` through `LIBCLANG_PATH`; Python needs `python3` (Pyrefly is bundled); Go needs `go` and its module cache (the Go oracle is bundled); Java needs a JDK with `java`/`javac`; Rust needs a matched `rustc`/`cargo` pair and Cargo home/cache. TypeScript's real Node, `tsc`, module, and report driver are bundled. C#'s real `dotnet` runtime and Roslyn helper are bundled. None of these compiler capabilities is required to launch the app. Offline navigation remains local; registry/package acquisition may show unavailable content when its cache has no source.

The packager emits an unsigned or ad-hoc app status based on the bundle's observed signature, and records notarization as “not performed.” It never queries signing credentials, signs, or notarizes. A bundle that has not passed signing/notarization and the native QA below is not ready to send as a finished investor release.

## Native release checklist

Use a clean macOS user account with no project/index environment variables, no development shell, and network access disabled for offline checks. Sol owns all native input and screenshots; this packaging lane does not drive the app UI.

| Check | Required evidence |
| --- | --- |
| Package provenance | Exact source revision/tree, lock hashes, build commands, target triple, binary digests, helper digests, font/runtime notices, `otool -L` closure, `lipo -archs` results, bundle-manifest hash, and zip SHA-256 agree. |
| Cold launch | Finder opens the received `.app` without `NUDOX_*`, project, index, or Nix variables; no Gatekeeper workaround is presented as a signed release. |
| First project | Folder chooser appears; add a project, wait for indexing, close and reopen it. |
| Compilation | Exercise one supported real compiler path and the bundled real Roslyn helper; verify language-unavailable states remain honest for uninstalled toolchains. Record compiler versions and outputs. |
| Offline navigation | Disable network; open existing indexed content, navigate graph header and return to the selected symbol; no network fetch is required for local content. |
| Publication refresh | Refresh a publication and verify the visible generation/data changes from actual local evidence. |
| Source/search/keyboard/focus | Open source, search, use keyboard navigation and return focus after overlays/panels; capture any truncation or dead-end. |
| Preferences | Change preferences, quit, relaunch, and verify persistence. |
| Cold restart | Quit all app/locald processes, relaunch from Finder, verify project and index state persist. |
| Unavailable services | With connections/index unavailable, verify clear refusals and that local browsing remains usable. |
| Distribution | A reviewed app icon is present; a valid Developer ID signature, notarization and stapling are independently checked before claiming investor-ready distribution. |

No native build, app launch, compiler run, UI test, signing, or notarization has been performed in this lane. Root must admit the build workload and supply the remaining real runtime/compiler-helper inputs before packaging can be executed; Sol must then complete native QA.
