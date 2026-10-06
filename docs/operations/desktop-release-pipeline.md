# Desktop artifact release pipeline

Once a platform build is ready, the developer submits its packaged, natively tested artifact.
Concourse validates it automatically. One manual `publish-<platform>` job publishes those exact
bytes and promotes the website download. Successful Mac publication automatically updates the
stable Homebrew cask. Publishing never rebuilds the candidate or publishes every successful PR.

The release tooling is independent of the GUI/indexing recovery PRs. It can merge first;
select a product revision only after those fixes are merged and their acceptance is complete.

## Developer handoff

1. Select a canonical source revision with passing `concourse/backend-fast` and the relevant
   platform lane (`linux`, `arm64-emu`, or `windows-wine`). Mac additionally needs its real
   native acceptance; Wine/QEMU CI does not replace Windows/ARM hardware testing.
2. Build and package the complete product and helper/runtime closure on a suitable host.
   For Mac, use `macos_release.py preflight`, `prepare`, and `finalize` with a configured
   `macos-release.example.json`. It builds matched binaries, signs with Developer ID,
   notarizes through Apple, staples, and produces the final candidate directory. Follow
   [the Mac runbook](macos-release.md) for the receipted helper/toolchain inputs.
3. Upload a private draft for native acceptance (this does not release it or trigger Concourse):

```nu
python3 tools/package/release_contract.py stage --candidate /absolute/path/to/candidate --source . --rc 1
```

   `stage` checks artifact/source/CI/signing evidence but does not require native QA yet. It
   leaves the GitHub release draft and returns its private preview URL. A team member with
   repository access can download its archive through the browser for quarantine testing.
   Test the final downloaded archive on a clean supported native system. Index a real project,
   search, restart, exercise CLI/MCP and helpers, and record `native-qa.json` using the matching
   example in `tools/package/`. Bind it to the exact final archive SHA-256 and source SHA.
   Keep browser quarantine/Gatekeeper enabled on Mac. Set a case true only after observing
   it pass. For Homebrew acceptance, create a local QA tap on the clean test Mac and generate
   its cask from this exact downloaded archive:

```nu
brew tap-new nudoxorg/release-qa
let qa_cask = ((brew --repository | str trim) | path join "Library" "Taps" "nudoxorg" "homebrew-release-qa" "Casks" "nudox.rb")
python3 tools/package/release_contract.py cask --candidate /absolute/path/to/candidate --output $qa_cask --local-preview
brew install --cask nudoxorg/release-qa/nudox
```

   Test CLI/MCP indexing and uninstall/reinstall. The QA cask uses a local file URL and the
   final archive's checksum; the production cask uses the versioned public URL. Test on an
   isolated clean Mac, preserve the separate browser quarantine test, and do not alter the
   final archive to make either pass.
4. With the same dedicated GitHub candidate-upload and Forgejo read credentials in the environment,
   submit from a canonical checkout with access to that source revision:

```nu
python3 tools/package/release_contract.py submit --candidate /absolute/path/to/candidate --source . --rc 1
```

`NUDOX_RELEASE_TOKEN` needs Contents write on `nudoxorg/backend`; `FORGEJO_TOKEN` needs
Backend/status read. Obtain them through the team's secret mechanism, not command-line
arguments or committed files. `submit` checks source/version/tree/lock, artifact bytes, native
QA, and exact-revision CI. It chooses `v<version>-rc.<number>-<platform>`, uploads a draft,
reads every asset back, and only then makes the completed prerelease visible to Concourse.

Changed bytes need a new RC number. Retrying the same bytes is idempotent. Build directories,
ad-hoc signed previews, a green build without native QA, and old archive relabels are refused.

## Artifact contract

| Website platform | Target | Final archive | Signing |
| --- | --- | --- | --- |
| macos | aarch64-apple-darwin | nudox-macos-arm64.zip | Developer ID + Apple notarization |
| linux-x64 | x86_64-unknown-linux-gnu | nudox-linux-x64.tar.gz | Checksummed archive + native QA |
| linux-arm64 | aarch64-unknown-linux-gnu | nudox-linux-arm64.tar.gz | Checksummed archive + native QA |
| windows | x86_64-pc-windows-gnu or x86_64-pc-windows-msvc | nudox-windows-x64.zip | Authenticode + native QA |

Each candidate directory contains the archive, `<archive>.sha256`, `release-manifest.json`,
and `native-qa.json`. The schema-1 manifest uses the same fields as the existing Mac
finalizer: version, source SHA/tree, Cargo.lock SHA-256, platform, target, archive name/hash/
size, minimum OS, embedded build-manifest SHA-256, signed, notarized. The authoritative
profiles and required QA cases are in `tools/package/release_platforms.py`.

Mac contains `Nudox.app`, its four matched binaries, `Nudox` GUI launcher, and `nudox-cli`,
`nudox-mcp`, `nudox-locald` command launchers. Linux/Windows payloads use `Nudox/bin/` for
`backend-desktop`, `backend-cli`, `backend-mcp`, `backend-locald` (with `.exe` on Windows),
and `Nudox/resources/build-manifest.json`. Embedded build evidence includes
`source.git_revision`, `source.git_tree`, and `source.cargo_lock_sha256`. Include every
required helper/runtime; a portable archive cannot depend on the developer's Nix store.
Linux staging must dereference links: release archives reject links/special files.

Linux/Windows build and installer tooling is not supplied by this publishing pipeline.
Their developers must provide portable payloads and genuine native/signing evidence meeting
this contract; native worker enrollment can automate that handoff later. Intel Mac and ARM
Windows are not part of the current website architecture contract.

## Operator promotion

After the one-time setup in MachineConfigurations `docs/backend-release.md`, inspect the
candidate's source, version and green `validate-<platform>` job in Concourse, then run:

```nu
nu scripts/release.nu publish macos
```

Use the other platform key for its lane. The job selects the newest validated candidate;
pin its Concourse resource first if intentionally releasing an older candidate. Jobs require
their own candidate and canonical checkout to have passed validation together, and recheck
current exact-source CI before publication. Promotion jobs are serialized across platforms.

Stable `v<version>` releases use platform-specific archive and metadata filenames; platforms
can arrive separately. A version must use the same source revision for every platform. The
publisher appends other platforms, verifies all bytes, and refuses replacement of any asset.
It never uses GitHub global latest. Catalog writes are atomic and compare-and-swap protected.
After promotion, both website and versioned public downloads are hashed. Failure rolls back
only the selected platform. A rollback conflict requires inspection; it is never overwritten.

Mac publication triggers `publish-homebrew` only after successful website promotion. The job
updates `nudoxorg/homebrew-tap/Casks/nudox.rb` with the same version/checksum and a public
version-specific URL. The cask installs the app and helper-aware CLI/MCP launchers. Users run:

```sh
brew install --cask nudoxorg/tap/nudox
```

If the tap update fails, the website release remains available. Repair the dedicated tap
credential and retry `nu scripts/release.nu homebrew`; retries with identical content do not
create another commit. The old `nudox-preview` formula remains a separate preview channel.
WinGet/Scoop, distro repositories, and installer-specific manifests need their own tested
installers/recipes; this change provides stable artifact identities, not those submissions.

## Durable downloads and rollback

Package-manager URLs are `/v1/downloads/<platform>/<version>/<archive-sha256>` on
`https://api.nudox.org`. Auth only exposes current or historically promoted available releases,
and resolves private GitHub assets server-side. Preserve release assets and catalog history
permanently. Later promotion/rollback changes the website's current selection, not old package
URLs. Private GitHub release URLs and moving current-platform URLs are not package-manager URLs.

The publisher requires Auth's `X-Nudox-Versioned-Downloads: 1` capability before stable
publication, so missing deployment is detected before public promotion. Runtime signing,
notarization and native QA remain mandatory even when all CI jobs are green.
