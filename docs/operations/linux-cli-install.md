# Linux x86_64 CLI, MCP and local daemon

The Linux archive contains the matched `backend-cli`, `backend-mcp` and `backend-locald` executables. The desktop GUI is excluded. The three ELF files remain siblings under `~/.local/lib/nudox/versions/<release-tag>/bin`; any non-glibc shared-library dependencies live in that release's `lib/`. Commands under `~/.local/bin` point through an atomically replaced `current` symlink, preserving `backend-runtime`'s sibling lookup for locald.

## Current public channel readback (diagnostic preview)

Anonymous public GitHub readback on October 17 returned HTTP 200 for `install.sh` (5,851 bytes, SHA-256 `2194af358fc71b8031973ca549a15256b08d778a5052825f724f7828c5dea61a`) and `preview-channel.json` (2,685 bytes, SHA-256 `930c7f146c45514222e434c29fef8e84687ed1f37e77eaa4b8aab475772f135e`). The channel identifies Linux source `df93af4890005c2bb748928c5a524d1f203fe04f`, tag `checkpoint-20261016-df93af4890-linux-x64`, and remains `production_ready: false`; the Mac row is still the older October 6 build. The separately read-back [bootstrap](https://github.com/nudoxorg/Backend/releases/download/nudox-preview-installer/install-bootstrap-c533dfe34b7d137a.py) (6,033 bytes, SHA-256 `c533dfe34b7d137a08f1569e217cdfd44fc733c8584f0b619c57a23b27cc0209`) and [Linux installer script](https://github.com/nudoxorg/Backend/releases/download/checkpoint-20261016-df93af4890-linux-x64/install-linux-x64.py) (58,492 bytes, SHA-256 `423adce9c0d78a106f71c41cbce980e00bd78bdf90a80c9e5c73c26e528c2cdb`) match the channel's recorded extent and digest; the channel's [Mac installer script](https://github.com/nudoxorg/Backend/releases/download/checkpoint-20261006-a659d5d181-macos-arm64/install-macos-arm64-82f71f98a05a2b13.py) also read back at 22,094 bytes with SHA-256 `82f71f98a05a2b13e05afd7185e35c5f8d252b9973ab4ce6d32ab22688794f25`. These are script and metadata readbacks: no archive bytes were downloaded, and no installer was run. They do not establish an installation result, a production-ready release, or fresh native acceptance.

The Linux channel currently points to the matched CLI/MCP/local-daemon trio from source `df93af4890005c2bb748928c5a524d1f203fe04f` at tag `checkpoint-20261016-df93af4890-linux-x64`. Its shell interface matches the repository's pinned `c533` bootstrap. To use the public preview installer, save it, verify the observed script digest, and then run it:

```sh
(umask 077; installer_dir=$(mktemp -d "${TMPDIR:-/tmp}/nudox-preview.XXXXXXXX") || exit 1; trap 'rm -rf "$installer_dir"' 0; curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output "$installer_dir/install.sh" 'https://github.com/nudoxorg/Backend/releases/download/nudox-preview-installer/install.sh' && printf '%s  %s\n' '2194af358fc71b8031973ca549a15256b08d778a5052825f724f7828c5dea61a' "$installer_dir/install.sh" | sha256sum --check --status && sh "$installer_dir/install.sh")
```

The checked hash identifies the read-back shell script, not the archive. Do not infer archive integrity or installation success from the launcher and channel-file readback; use the exact archive/manifest verification and Ubuntu QA process below before making those claims.

## Historical Forgejo diagnostic fallback: CPython 3.8 containment repair

The Forgejo fallback launcher is `install-shell-e4e3a7c70ee4d81f.sh` in Forgejo release `nudox-diagnostic-installer-20261016-py38`. Anonymous public readback on October 16 returned HTTP 200 with 5,955 bytes and SHA-256 `e4e3a7c70ee4d81f4509d19c36a7ab1c8bbbcbb5509e11535eceff1609e10ca3`. It verifies and saves the 13,428-byte bootstrap (`7558db53fd2820c3879a048a6f1c39f02f7464a4ef1c8cb88aa4d089aeda59b7`) before executing it with CPython 3.8 or newer. The bootstrap pins the 49,968-byte Linux installer (`d2c568159507fd3c6e57d11c25c91edbbe1940852b364261253327b8edc0b103`) and the unchanged Mac installer.

This Forgejo Linux installer repairs one Python-version incompatibility in package-path containment. That Forgejo channel selects the historical October 8 Linux c001 binaries and October 6 Mac a659 binaries; the original archive and manifest identities, Mac row, and known limitations are unchanged. `production_ready` remains false. The Linux validation boundary remains Ubuntu 24.04+ with glibc 2.39+, and the tested NixOS loader refusal remains recorded. This is not the current public Linux matched-trio checkpoint or fresh whole-application/native acceptance. The compatibility control was run on CPython 3.12.14 with a simulated missing `Path.is_relative_to`; a real CPython 3.8 installation has not been run. Use this verified Forgejo URL only as a historical fallback; it does not select the current GitHub `df93` channel.

Download the shell launcher to a private temporary file before running it. This preserves HTTP/download and launcher failures and removes the saved script:

```sh
(umask 077; installer_dir=$(mktemp -d "${TMPDIR:-/tmp}/nudox-launcher.XXXXXXXX") || exit 1; trap 'rm -rf "$installer_dir"' 0; curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output "$installer_dir/install.sh" 'https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261016-py38/install-shell-e4e3a7c70ee4d81f.sh' && sh "$installer_dir/install.sh")
```

The fallback launcher requires curl with HTTPS support and CPython 3.8+, prefers a suitable host `/usr/bin/python3` ahead of PATH shims, and executes saved Python source. `NUDOX_INSTALL_PYTHON` can select an explicit CPython executable. If the launcher cannot find CPython, set this in the same shell before running the command above, replacing the example with the path to a CPython 3.8+ executable:

```sh
export NUDOX_INSTALL_PYTHON=/absolute/path/to/cpython3
```

Do not pipe the Python asset directly to `python3`: that skips the saved, hash-verified launcher and invokes whichever interpreter PATH selects. A PATH shim such as RustPython may not support that mode. To select a prefix or permit a guarded downgrade, append `--prefix /absolute/path` or `--allow-downgrade` to the final `sh "$installer_dir/install.sh"` invocation. The existing library creates all six `nudox`/`backend` aliases and prints a correctly quoted PATH line when the prefix's `bin` is absent from PATH. It does not invoke `sudo` or edit shell profiles.

### Historical R2 delivery (pre-CPython-3.8 fix)

The previous R2 launcher remains immutable at `install-shell-d837c1fc8130285f.sh` (5,845 bytes, SHA-256 `d837c1fc8130285fe60d1dc44ef96f93fa2177f18bdb2c1db031eab7fc28a8ac`). Its pinned 49,954-byte Linux installer calls `Path.is_relative_to`, which is unavailable on CPython 3.8. Use the Forgejo fallback above when CPython 3.8 compatibility is needed. The R2 command is retained here only as a historical reference:

```sh
(umask 077; installer_dir=$(mktemp -d "${TMPDIR:-/tmp}/nudox-launcher.XXXXXXXX") || exit 1; trap 'rm -rf "$installer_dir"' 0; curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output "$installer_dir/install.sh" 'https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261009-r2/install-shell-d837c1fc8130285f.sh' && sh "$installer_dir/install.sh")
```

The October 12 R2 checks included a repeat run against the existing default Mac `~/.local` installation, with exit 0 and an unchanged active release marker and pointer; a separate audit rehashed the five installed binaries/libraries and ran CLI/MCP/locald help successfully. Those checks concern the historical Mac a659 binaries, not the new Python 3.8 Linux installer asset. Their receipt remains `docs/audits/diagnostic-installer-mac-repeat-evidence-20261012.json`. The October 11 R2 fresh-install evidence remains `docs/audits/diagnostic-installer-shell-publication-evidence-20261011.json`. Neither establishes current Angular/index/MCP/GUI native acceptance.

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

The default package uses linked TSZ and its embedded TypeScript declarations. It does not require or bundle Node or an npm TypeScript compiler. The compiler implementation is part of the source-bound product executables above. For an explicitly configured legacy Compiler API integration, supply both `--typescript-sdk-directory /absolute/payload` and `--typescript-sdk-receipt /absolute/sdk-receipt.json`. A single SDK flag is refused. The optional payload retains exact source, complete inventory, notice, hash, ELF relocation and Node/Compiler API version checks; it does not change the default compiler selection.

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

The channel bootstrap refuses the current legacy catalog entry because it has no digest. Its Python interpreter requirement belongs to the installer. Product Python analysis uses linked Rust Pyrefly by default, without a host `pyrefly` executable; selected host CPython, standard-library and virtual-environment inputs are captured separately for project semantics. Product TypeScript analysis uses linked TSZ by default; explicit legacy compiler overrides remain supported. These source contracts do not upgrade the historical binaries on the diagnostic channel or establish native acceptance for a new package.
