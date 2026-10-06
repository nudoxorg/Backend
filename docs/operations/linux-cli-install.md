# Linux x86_64 CLI, MCP and local daemon

The Linux archive contains the matched `backend-cli`, `backend-mcp` and `backend-locald` executables. The desktop GUI is excluded. The three ELF files remain siblings under `~/.local/lib/nudox/versions/<release-tag>/bin`; any non-glibc shared-library dependencies live in that release's `lib/`. Commands under `~/.local/bin` point through an atomically replaced `current` symlink, preserving `backend-runtime`'s sibling lookup for locald.

## Immutable checkpoint install

The candidate installer embeds a unique checkpoint tag and the SHA-256 digest of its exact release manifest. It fetches that manifest and the archive over HTTPS, verifies the pinned manifest bytes, then checks archive size and SHA-256 before safe extraction. This path is usable while `linux-x64` remains `legacy` in the public catalog. The checkpoint installer is not a claim that the stable public channel is available.

For the first accepted source checkpoint `1840fca247`, the one-line command is:

```sh
curl -fsSL https://github.com/nudoxorg/Backend/releases/download/checkpoint-20261006-1840fca247-linux-x64/install-linux-x64.py | python3 -
```

Publish and communicate that command only after the exact archive passes all Ubuntu native QA cases and the GitHub release assets are read-back verified. The bootstrap uses Python 3, installs by default under `~/.local`, does not invoke `sudo`, and prints a PATH line when `~/.local/bin` is not on `PATH`. It checks the detected glibc against the package's measured minimum before installing, refuses unsupported architectures, and reports HTTP, metadata, checksum or extraction failures without claiming success.

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

The packager prints the immutable release tag and SHA-256. On Ubuntu, record that exact archive SHA and source revision in `native-qa-linux-x64.json` only after `cli_version`, `mcp_help`, `locald_sibling_discovery`, `clean_environment_without_nix_paths`, and `ubuntu_glibc_floor` all pass. The QA record must identify Ubuntu and glibc versions and is bound to the same archive and source. Keep actual QA logs with the candidate.

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

The channel bootstrap refuses the current legacy catalog entry because it has no digest. Python 3 is required. Node.js and the project's installed `typescript` package are needed for TypeScript indexing; Python and Go toolchains are needed only for projects that use those languages.
