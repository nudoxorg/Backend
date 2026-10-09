This mirror is an October 9 installer delivery repair for older diagnostic
binaries. It does not promote a stable release, current native acceptance, or
fresh whole-project indexing. The existing GitHub bootstrap and both platform
libraries remain unchanged. A separate bootstrap is necessary because their
command-line download paths require GitHub. The mirror uses one bounded HTTPS
transport, verifies scripts before import, and calls the platform library's
normal `install()` API without changing its globals or verification routines.

The existing public repository is `philocalyst/Backend` on
`https://dev.nudox.org/git/`. Canonical `Nudox/Backend` is private. Anonymous
repository and release-list GETs returned 200 for the public repository; its
release list was empty during preparation. Its raw bootstrap path was absent.
No repository privacy change, new repository, static-server change, upload,
or publication was performed by this worker.

Root's publication destinations are:

* API: `https://dev.nudox.org/git/api/v1/repos/philocalyst/Backend/releases`.
  Create draft prereleases, upload multipart `attachment` assets through
  `/releases/<actual-returned-id>/assets?name=<exact-name>`, verify readback,
  then publish. Use actual returned IDs; external-URL assets are unsuitable.
* Bootstrap tag: `nudox-diagnostic-installer-20261009`. Upload this source as
  `install-forgejo-bootstrap-<first-16-SHA256>.py`, plus the exact
  `forgejo-preview-channel.json` bytes as `preview-channel.json`.
* Mac tag: `checkpoint-20261006-a659d5d181-macos-arm64`. Upload unchanged
  `install-macos-arm64-82f71f98a05a2b13.py` and
  `nudox-macos-arm64-a659d5d181.tar.gz` (70,309,994 bytes, SHA-256
  `f88b841030699ae277afe697b6c92e98dfbb09a815f40388e3d2fbee0661e110`).
* Linux tag: `checkpoint-20261008-c0016d4f4f-linux-x64`. Upload unchanged
  `install-linux-x64-8baa4a60be98ee4e.py`,
  `nudox-linux-x86_64-c0016d4f4f.tar.gz` (133,556,631 bytes, SHA-256
  `b394e10203d02898b20140f0f9f9869aa8412e6e288fb8d8e25ac01469df7a05`),
  and original `release-manifest-linux-x64.json` (698 bytes, SHA-256
  `384ab3802efd256752f39fd8e3d79ace7af9e3ccf72797c71577327cb2535b24`).
  The fixture copy of this original manifest is byte-identical. Its source,
  tree, Cargo lock, build/package receipts and GitHub-era archive identity
  are not rewritten to match the mirror transport location.

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
configuration has no override. Actual publication/downloads remain Root's gate.

After publication, the explicit stdin command is `curl -fsSL <the channel's
bootstrap URL> | python3 -`. Direct-file execution uses the same URL and
`python3 /absolute/path/install.py`. Both retain `--prefix` and
`--allow-downgrade`. Downloads stream into a private temporary directory;
the unchanged installer retains its full ownership/lease, archive/inventory,
existing-install checks, startup probes and atomic pointer publication.

Mac a659 is an October 6 diagnostic trio; October 9 Rust repairs are absent.
HTTPie reads were verified, while fresh large-project indexing is not
established, semantic embeddings are unconfigured, and history exceeds its
row limit. Linux c001 is an October 8 GNU diagnostic trio requiring glibc
2.39+; original installation/reinstallation passed but packed native runtime
control timed out and whole-application acceptance remains open. The channel
retains these exact limitations.

Validation is Python-only: the existing 24 installer controls and 13 new
transport/package controls. New actual stdin/file subprocesses use fixture
HTTPS responses and harmless shell executables, while invoking the unchanged
platform installation library normally. They are not real public download,
native binary, GUI, indexing, or production acceptance evidence.
