# Surface smoke testing

The testing rework starts with a small Linux smoke runner. It builds the CLI, MCP
and desktop binaries for the host once, then checks each public surface:

- CLI: `--help` exits successfully and prints usage.
- MCP: stdio `initialize` and `tools/list` succeed, followed by clean EOF exit.
- GUI: a visible window paints under Xvfb/software Vulkan and exits successfully
  through the public Quit shortcut.

From the Backend checkout:

```sh
nix develop path:.#smoke -c nu --no-config-file .config/ci/t1.nu
```

The Nix shell supplies the pinned Rust toolchain and Linux graphics dependencies.
The runtime checks use temporary HOME/XDG directories and an empty project, so
startup does not index the repository or reuse your normal workspace. Correctness
of indexing, search and navigation will be tested by the forthcoming T2 corpus.

For the small driver regression suite, without compiling the product:

```sh
python3 .config/ci/test_smoke.py
```

The warm target is under one minute. A cold build is substantially longer. CI owns
its dedicated, serialized build caches; local edits can use the default target
directory. Do not point a local run at another worktree's mutable cache. Keep
build caches outside the flake source tree to avoid copying them during Nix setup.

## CI selection during rollout

Backend PRs changing only the explicit documentation paths (`docs/`, `README.md`,
`LICENSE`) receive `T0: docs-only; no build required` on the existing CI contexts.
The dispatcher verifies the complete changed-file list for the exact head first.
Mixed changes, source-to-doc renames and unknown or incomplete diffs run normal CI.
Markdown outside that explicit list does not automatically skip CI.

The full T1/T2 pipeline replacement is still being built. Existing fast and legacy
platform jobs remain in effect during rollout; the smoke command is available now.
