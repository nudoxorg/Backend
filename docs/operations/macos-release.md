# Mac-first release and CD

This lane produces an Apple Silicon NuDox app, signs/notarizes it on a team Mac, and delivers
the exact accepted archive through the existing website download URL. Windows and Linux remain
pinned to their legacy v0.1.0 artifacts until they pass independent release acceptance.

Implementation status: release tooling and platform routing are implemented and tested. The
first product archive has not been built, signed, accepted or published. The Concourse release
pipeline is disabled until credentials and deployed routing are ready. Initial native builds
run on an existing team Mac; automated native building needs a later Mac runner.

## 1. Select and prepare the native build

Merge the release tooling to Backend canonical, wait for `concourse/backend-fast` success,
and select that exact clean commit/tree. Version 0.2.0 is the first current-product candidate.
The private GitHub repo is a distribution store; canonical source provenance is the Forgejo
SHA/tree/Cargo.lock recorded in the manifests, not GitHub's distribution tag target.

Use the input/receipt protocol in [macos-investor-bundle-2026-10-04.md](macos-investor-bundle-2026-10-04.md)
for the Cargo runner, Go oracle, Pyrefly, Node/TypeScript, Roslyn and .NET runtime. These must be
real, pinned native inputs. The release driver deliberately refuses unreceipted helpers.
Building them and their receipts is an operator prerequisite; the driver does not fabricate them.
Optional relocation plans/package roots follow that runbook's existing source and closure checks.

Build pinned `cargo-bundle` separately from the app:

```sh
nix build .#cargo-bundle --no-link --print-out-paths
```

Both flake entrypoints pin Miles's nixpkgs revision for cargo-bundle 0.12.0 without changing
workspace nixpkgs. Binary-specific Cargo bundle metadata is under
`package.metadata.bundle.bin.backend-desktop`; `--binary-path` avoids a second application build.
The app includes desktop, locald, CLI and MCP beside one another. The existing launcher binds
compiler helper paths inside the bundle. Final Mach-O checks reject builder-only dependencies.

Copy `tools/package/macos-release.example.json` outside the checkout and replace every input.
The example minimum OS 26.0 is conservative for the current development host, not a verified
support claim. Choose the advertised minimum from actual binary/helper load commands and native
testing on that OS; a plist change cannot make a newer binary compatible with an older OS.
The icon digest refers to the ICNS converted from the checked-in brand logo SVG.

Preflight requires a clean source checkout, matching revision/tree, all absolute inputs,
an available Developer ID Application identity, authenticating notarization profile and native
distribution tools. The build
volume defaults to a 40 GiB admission floor and must have room for the actual closure/build;
use bounded jobs in the admitted runner. Do not lower the floor merely to get admission.

## 2. Signing and notarization

Use the team's Developer ID Application identity with its private key in the release Mac's
keychain. Apple Development is a different identity. Confirm certificate availability locally
with `security find-identity -v -p codesigning`. Obtain the identity through the existing
Apple Developer organization/account workflow, then configure the exact identity in JSON.
Store notarization access in a keychain profile, e.g. interactively:

```sh
xcrun notarytool store-credentials nudox-notary
python3 tools/package/macos_release.py preflight --config /absolute/path/release-config.json
python3 tools/package/macos_release.py prepare --config /absolute/path/release-config.json
```

Keep account passwords, tokens, certificate private keys and notary credentials out of the
checkout and logs. The profile name is configuration; secrets remain in the keychain.
`prepare` builds receipted binaries, packages/relocates, signs nested code and the app with
hardened runtime/timestamps, submits notarization, requires Accepted, staples the ticket,
and verifies signatures/Gatekeeper before creating the final ZIP/checksum/release manifest.
Executable components receive the JIT entitlement for bundled Node/.NET; native acceptance
must verify those helpers work with this signing configuration. Signing/notarization itself
has not yet been exercised against a real product bundle.

If packaging completed but signing was interrupted, `finalize` can resume signing from the
verified packaged source. Inspect any partial candidate first; it refuses to overwrite one.
Retain the build/package receipts and notarization.json alongside the candidate.

## 3. Native acceptance of the exact final archive

Extract/install the ZIP on a clean account or second Mac without the source checkout, Nix
shell, dev environment variables or an already-running development locald. Launch from Finder;
add/index/search a real project; exercise bundled helpers, preferences, quit/relaunch and
cold restart. Verify Gatekeeper, persistence and optional-toolchain failure messages.
Test on the advertised minimum OS as well as the development OS. Confirm CLI/MCP can use the
bundled local owner where advertised. Native QA is required in addition to Linux CI evidence.

Copy `tools/package/native-qa.example.json` to `candidate/native-qa.json`. Fill the operator,
time, OS and exact final archive/source hashes. Mark each case true only after it passed.
The template deliberately starts false; no staging/publication is allowed with missing QA.

```sh
python3 tools/package/release_contract.py validate \
  --candidate /absolute/path/release/candidate --source /absolute/path/clean-canonical-backend
```

The verifier checks archive/checksum, embedded source/tree/lock evidence, required app binaries,
plist version/minimum OS and QA bound to those exact bytes. Signature/Gatekeeper acceptance
is performed natively; Linux validation checks the bound record rather than pretending to
execute Apple's distribution checks.

## 4. Stage, validate and publish

Deploy platform-aware auth and its seeded catalog first; `/v1/releases` must expose all four
platforms with the old versions. Deploy the Web labels and restricted promotion service.
Provision dedicated publisher credentials per the MachineConfigurations release runbook.
The native operator supplies `NUDOX_RELEASE_TOKEN` securely through the process environment
and stages the already-tested files as a prerelease. Uploads stay draft until all bytes pass
read-back verification; only then is the RC published for Concourse discovery:

```sh
python3 tools/package/release_contract.py candidate \
  --candidate /absolute/path/release/candidate --candidate-tag v0.2.0-rc.1
```

Concourse `nudox-backend-release/validate-candidate` then verifies the candidate against
canonical source and its latest `concourse/backend-fast` status. `publish-macos` is a manual
promotion job: it revalidates, creates a stable distribution release from the same bytes,
reads every uploaded asset back and checks hashes, then changes only the Mac catalog entry.
Neither candidate nor stable creation marks the release GitHub global latest. Existing
published assets cannot be overwritten. Draft retries resolve the tag through the paginated
release listing to avoid creating duplicate drafts. A new candidate requires a new RC tag when bytes differ.

The promoter uses pinned SSH host keys, a forced-command key and compare-and-swap. The public
Mac URL is downloaded and hashed after promotion. If verification fails, it restores the prior
Mac channel from history using compare-and-swap; concurrent changes cause an explicit failure
instead of overwriting another release. GitHub's immutable stable asset remains for inspection.
Manual channel rollback is documented in MachineConfigurations.

Success means `https://api.nudox.org/v1/downloads/macos` serves the exact accepted archive,
Web shows its version/minimum OS, and all three legacy platform selections are unchanged.
Future Windows/Linux CD extends native packaging/acceptance and independently promotes their
channels. Backend index/worker deployment and dedicated Mac CI enrollment remain separate
tracks in the NuDox root plan; this desktop lane does not deploy those servers.
