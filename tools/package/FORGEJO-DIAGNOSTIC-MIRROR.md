This mirror is an October 9 installer delivery repair for older diagnostic
binaries. It does not promote a stable release, current native acceptance, or
fresh whole-project indexing. The existing GitHub bootstrap remains unchanged;
R2 carries the reviewed Linux library repair beside its own bootstrap and
retains the original Mac library. A separate bootstrap is necessary because their
command-line download paths require GitHub. The mirror uses one bounded HTTPS
transport, verifies scripts before import, and calls the platform library's
normal `install()` API without changing its globals or verification routines.
An old immutable bootstrap first validates the channel envelope and verifies
the current bootstrap before re-executing it. Only the digest-matching current
bootstrap admits its own compiled library pins. This permits reviewed library
updates without importing a library that the executing bootstrap has not pinned.

The existing public repository is `philocalyst/Backend` on
`https://dev.nudox.org/git/`. Canonical `Nudox/Backend` is private. R2's public
entrypoint, digest-named bootstrap, channel, both platform libraries and Linux
manifest were anonymously read back on October 11 and retained their exact
published bytes. The shell launcher was added to R2 on October 11 and both
named shell URLs were independently read back anonymously at 05:44 UTC.
Each returned HTTP 200 and the integrated source's exact 4,318 bytes/SHA-256
`7e7bedf26d43119396fc62127464a2fa4af2affdad3238cbb5e8016402c46a44`.

Root's publication destinations are:

* API: `https://dev.nudox.org/git/api/v1/repos/philocalyst/Backend/releases`.
  Create draft prereleases, upload multipart `attachment` assets through
  `/releases/<actual-returned-id>/assets?name=<exact-name>`, verify readback,
  then publish. Use actual returned IDs; external-URL assets are unsuitable.
* Bootstrap tag: `nudox-diagnostic-installer-20261009-r2`. Upload this source as
  `install-forgejo-bootstrap-<first-16-SHA256>.py`, plus the exact
  `forgejo-preview-channel.json` bytes as `preview-channel.json`. The same
  bootstrap bytes are published as `install.py`. R1 remains a separate
  immutable release and its older library asset is unchanged.
* Published shell assets on R2: `install.sh` (asset 14) and
  `install-shell-7e7bedf26d431193.sh` (asset 13), both 4,318 bytes/SHA-256
  `7e7bedf26d43119396fc62127464a2fa4af2affdad3238cbb5e8016402c46a44`.
  Launcher source commit `a79ee40748190d7c4555e615886356444e161592` was
  integrated through PR 132 at `e3321a4327d8cf4cf6fee60ff0f5962d0e6d8d0a`.
  The existing Python bootstrap, channel and platform identities were retained.
* Mac tag: `checkpoint-20261006-a659d5d181-macos-arm64`. Upload unchanged
  `install-macos-arm64-82f71f98a05a2b13.py` and
  `nudox-macos-arm64-a659d5d181.tar.gz` (70,309,994 bytes, SHA-256
  `f88b841030699ae277afe697b6c92e98dfbb09a815f40388e3d2fbee0661e110`).
* Linux checkpoint tag: `checkpoint-20261008-c0016d4f4f-linux-x64`. Its unchanged
  assets are
  `nudox-linux-x86_64-c0016d4f4f.tar.gz` (133,556,631 bytes, SHA-256
  `b394e10203d02898b20140f0f9f9869aa8412e6e288fb8d8e25ac01469df7a05`),
  and original `release-manifest-linux-x64.json` (698 bytes, SHA-256
  `384ab3802efd256752f39fd8e3d79ace7af9e3ccf72797c71577327cb2535b24`).
  The fixture copy of this original manifest is byte-identical. Its source,
  tree, Cargo lock, build/package receipts and GitHub-era archive identity
  are not rewritten to match the mirror transport location.
  R2's materialized Linux library is instead
  `install-linux-x64-e11fcc271fc3a8fe.py` on the R2 bootstrap release (49,954
  bytes, SHA-256 `e11fcc271fc3a8feb676b9f8c383bbf5bdc215578ffc399774819184908ed071`).

Named public asset URLs have the form
`https://dev.nudox.org/git/philocalyst/Backend/releases/download/<tag>/<asset>`.
Only the exact role's repository/tag/asset URL is accepted initially. Redirects
may remain on that URL or use the same origin's `/git/attachments/<UUID>`.
Credentials, alternate ports/hosts/repos/assets, query/fragment suffixes,
encoded paths and unrelated release redirects are rejected. These routes were
checked against the installed Forgejo 15.0.9 Swagger schema and upstream
`routers/web/web.go`, `routers/web/repo/repo.go`, and
`models/repo/attachment.go`. Named release downloads call `ServeAttachment`
directly. The installed local attachment storage needs no external redirect.
The documented v15 attachment limit defaults to 2,048 MiB and the inspected
configuration has no override. Root performed the shell publication and
anonymous readbacks; this documentation/evidence update changes no release.
Transport uses a 120-second socket timeout per blocking read, with no overall
download deadline; slow successful reads can prolong installation. No instant
installation or bounded wall-clock download claim is made.

Use saved-file execution: a curl failure can otherwise produce empty Python
stdin and a successful final pipeline status. The observed RustPython PATH
shim also misparses source on stdin. Published `install_diagnostic_preview.sh`
is the portable shell source that downloads the exact R2 bootstrap,
verifies its size/hash and selects CPython 3.8+ before executing the saved
file. Prefer host `/usr/bin/python3` when suitable; an explicit
`NUDOX_INSTALL_PYTHON` override must pass the same implementation/stdlib probe.
The wrapper requires HTTPS for the initial request and redirects, rejects
HTTP/partial-transfer or digest failures before execution and cleans its
private temporary directory on exit/signals. It forwards `--prefix` and
`--allow-downgrade`. Downloads stream into a private temporary directory;
the unchanged installer retains its full ownership/lease, archive/inventory,
existing-install checks, startup probes and atomic pointer publication.

The public user command also downloads before execution, preserving a curl
failure instead of running an empty shell script:

```sh
(umask 077; installer_dir=$(mktemp -d "${TMPDIR:-/tmp}/nudox-launcher.XXXXXXXX") || exit 1; trap 'rm -rf "$installer_dir"' 0; curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output "$installer_dir/install.sh" 'https://dev.nudox.org/git/philocalyst/Backend/releases/download/nudox-diagnostic-installer-20261009-r2/install.sh' && sh "$installer_dir/install.sh")
```

The command preserves curl or launcher failures through its final exit status,
and its exit trap removes the downloaded shell script.

Mac a659 is an October 6 diagnostic trio; October 9 Rust repairs are absent.
HTTPie reads were verified, while fresh large-project indexing is not
established, semantic embeddings are unconfigured, and history exceeds its
row limit. Linux c001 is an October 8 GNU diagnostic trio requiring glibc
2.39+; original installation/reinstallation passed but packed native runtime
control timed out and whole-application acceptance remains open. The channel
retains these exact limitations.

The 56 installer controls and broader 201 package controls passed before
publication. Their stdin/file subprocesses use fixture
HTTPS responses and harmless shell executables, while invoking the unchanged
platform installation library normally. These fixtures are distinct from
Root's successful actual fresh/repeat public shell installs, default-prefix
update and CLI/MCP/locald help probes on the historical Mac a659 archive.
The receipt explicitly records `native_indexing_tested: false`. Current
Angular/index/MCP/GUI native acceptance remains unproven. The new audit is
`docs/audits/diagnostic-installer-shell-publication-evidence-20261011.json`;
the earlier source audit's unpublished state is preserved as historical evidence.
