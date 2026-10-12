# Linux x86_64 CLI, MCP and local daemon

The Linux archive contains the matched `backend-cli`, `backend-mcp` and `backend-locald` executables. The desktop GUI is excluded. The three ELF files remain siblings under `~/.local/lib/nudox/versions/<release-tag>/bin`; any non-glibc shared-library dependencies live in that release's `lib/`. Commands under `~/.local/bin` point through an atomically replaced `current` symlink, preserving `backend-runtime`'s sibling lookup for locald.

## Current diagnostic delivery

Use the immutable R2 shell launcher `install-shell-d837c1fc8130285f.sh` in the command below. Anonymous readback on October 12 returned HTTP 200: 5,845 bytes, SHA-256 `d837c1fc8130285fe60d1dc44ef96f93fa2177f18bdb2c1db031eab7fc28a8ac`. It bounds and verifies the bootstrap download before executing saved source with an admitted CPython interpreter. The immutable launcher downloads the historical 13,426-byte R2 Python bootstrap and verifies SHA-256 `36f0ea62e3b49cadebcef0f89e735639530c0ba1686e62d4dcbfea96294174f5` before execution. This channel still selects historical binaries, independently of newer source commits. GitHub delivery remains unavailable: its releases returned to draft and anonymous asset downloads returned 404. Use this verified Forgejo URL.

Download the shell launcher to an owned temporary file before running it. This command preserves HTTP/download and launcher failures, and cleans up the saved script:

```sh
(umask 077; installer_dir=$(mktemp -d "${TMPDIR:-/tmp}/nudox-launcher.XXXXXXXX") || exit 1; trap 'rm -rf "$installer_dir"' 0; curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output "$installer_dir/install.sh" 'https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261009-r2/install-shell-d837c1fc8130285f.sh' && sh "$installer_dir/install.sh")
```

The launcher requires curl with HTTPS support and CPython 3.8+, prefers a suitable host `/usr/bin/python3` ahead of PATH shims, and executes saved Python source. `NUDOX_INSTALL_PYTHON` can select an explicit CPython executable. To select a prefix or permit a guarded downgrade, append `--prefix /absolute/path` or `--allow-downgrade` to the final `sh "$installer_dir/install.sh"` invocation. The existing library creates all six `nudox`/`backend` aliases and prints a correctly quoted PATH line when the prefix's `bin` is absent from PATH. It does not invoke `sudo` or edit shell profiles. Failed download pipelines can otherwise appear successful when an interpreter receives empty input; the observed RustPython shim also misparses Python source on stdin.

Root ran the anonymously downloaded new launcher against the existing default Mac `~/.local` installation: exit 0, identical active release marker and pointer. A separate independent audit rehashed all five installed binaries/libraries and executed CLI/MCP/locald help with exit 0. These checks installed historical Mac a659 (October 6) binaries. The same channel retains historical Linux c001 (October 8); its validation boundary remains Ubuntu 24.04+ with glibc 2.39+, while the tested NixOS loader was refused and the previous active pointer preserved. This delivery does not contain later Rust repairs or establish current Angular/index/MCP/GUI native acceptance. The new repeat evidence is recorded in `docs/audits/diagnostic-installer-mac-repeat-evidence-20261012.json`; the October 11 fresh-install evidence for the previous launcher remains in `docs/audits/diagnostic-installer-shell-publication-evidence-20261011.json`.

## Immutable checkpoint install

The candidate installer embeds a unique checkpoint tag and the SHA-256 digest of its exact release manifest. It fetches that manifest and the archive over HTTPS, verifies the pinned manifest bytes, then checks archive size and SHA-256 before safe extraction. This path is usable while `linux-x64` remains `legacy` in the public catalog. The checkpoint installer is not a claim that the stable public channel is available.

Once a real candidate has passed every Ubuntu native QA case and its GitHub release assets have been read-back verified, publish the exact command printed for its immutable tag. The command will have this form:

```sh
(umask 077; installer_dir=$(mktemp -d "${TMPDIR:-/tmp}/nudox-checkpoint.XXXXXXXX") || exit 1; trap 'rm -rf "$installer_dir"' 0; curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output "$installer_dir/install.py" "https://github.com/nudoxorg/Backend/releases/download/${NUDOX_LINUX_TAG}/install-linux-x64.py" && python3 "$installer_dir/install.py")
```

`NUDOX_LINUX_TAG` is a placeholder until an actual candidate passes QA and is published; this example is not an installable release URL. The bootstrap uses Python 3, installs by default under `~/.local`, does not invoke `sudo`, and prints a PATH line when `~/.local/bin` is not on `PATH`. It creates both `nudox` aliases and the `backend-cli`, `backend-mcp`, and `backend-locald` compatibility names used by current MCP setup guidance. It checks the detected glibc against the package's measured minimum before installing, refuses unsupported architectures, and reports HTTP, metadata, checksum or extraction failures without claiming success.

## Build/package and validate

The Linux build owner produces a source-bound successful build manifest on the admitted Linux builder. Run the package script against that receipt and the exact clean source checkout; it never builds Cargo. The packager inspects actual ELF headers, interpreter, symbol requirements and `ldd` closure, patches private copies to use `/lib64/ld-linux-x86-64.so.2` and origin-relative library paths, and bundles non-glibc dependencies only. It emits the archive, digest sidecar, release manifest, a tag-pinned checkpoint installer, a catalog-driven channel installer, and a QA template. It refuses to bundle glibc or accept dirty/mismatched source.

```sh
python3 tools/package/linux_release_package.py \
  --manifest /absolute/evidence/build-manifest.json \
  --source /absolute/path/to/clean/source-checkout \
  --output /absolute/path/to/new/candidate \
  --patchelf /absolute/path/to/patchelf \
  --readelf /absolute/path/to/readelf \
  --ldd /absolute/path/to/ldd
```

The packager prints the immutable release tag and SHA-256. On Ubuntu, record that exact archive SHA and source revision in `native-qa-linux-x64.json` only after `cli_version`, `project_add_search`, `mcp_help`, `mcp_session`, `locald_sibling_discovery`, `clean_environment_without_nix_paths`, and `ubuntu_glibc_floor` all pass. `project_add_search` must create a fresh project and find a known symbol in a real local source tree; `mcp_session` must complete an initialize/tools-list/tool-call exchange through the matched MCP and local daemon. The QA record must identify Ubuntu and glibc versions and is bound to the same archive and source. Keep actual QA logs with the candidate.

Before publishing, the generated checkpoint bootstrap can install those exact local candidate bytes into an isolated QA home. It rejects this option unless its embedded release-manifest SHA matches `release-manifest.json`, then rechecks archive size and SHA before extraction:

```sh
env HOME="$QA_HOME" python3 /absolute/path/to/candidate/install-linux-x64.py \
  --candidate-directory /absolute/path/to/candidate \
  --prefix "$QA_HOME/.local"
```

```sh
python3 tools/package/release_contract.py validate \
  --candidate /absolute/path/to/candidate \
  --source /absolute/path/to/clean/source-checkout
```

After review, `release_contract.py candidate --candidate-tag <printed-release-tag>` stages only the immutable platform-qualified assets on the distribution GitHub release. The release manifest and native QA assets are named `release-manifest-linux-x64.json` and `native-qa-linux-x64.json`; the checkpoint bootstrap is `install-linux-x64.py`. The packager also emits `install-linux-x64-channel.py`, which is published under `install-linux-x64.py` only with a later stable release and still requires the public catalog entry and SHA-256. Release asset uploads are read back and hash checked. The public catalog remains `legacy` until a separately accepted stable release is promoted.

The channel bootstrap refuses the current legacy catalog entry because it has no digest. Python 3 is required. Language compiler integrations are discovered separately from this CLI package; TypeScript indexing uses host Node and the project's installed `typescript` package when configured.
