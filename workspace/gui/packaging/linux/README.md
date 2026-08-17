# Linux packaging

lindsey is a standalone Cargo package (`workspace/gui/Cargo.toml` has its own
`[workspace]` table and lockfile), so all commands below are run from
`workspace/gui/`, never with `--workspace`/`--all-features`.

## Mechanism

The primary Linux packaging is **declarative Cargo metadata**, consumed by two
mainstream tools that need no per-package code:

| Tool | `cargo install` | Produces | Metadata section |
|---|---|---|---|
| [`cargo-deb`](https://github.com/kornelski/cargo-deb) | `cargo install cargo-deb` | `.deb` | `[package.metadata.deb]` |
| [`cargo-generate-rpm`](https://github.com/cat-in-136/cargo-generate-rpm) | `cargo install cargo-generate-rpm` | `.rpm` | `[package.metadata.generate-rpm]` |

The same `assets` (this `.desktop` entry + `../assets/logo.svg` installed as the
scalable icon) feed both, so the desktop integration stays in sync across
formats.

### Build

```sh
cd workspace/gui
cargo deb          # -> target/debian/lindsey_0.1.0_amd64.deb
cargo generate-rpm # -> target/generate-rpm/lindsey-0.1.0-1.x86_64.rpm
```

Both build the release binary first. `depends = "$auto"` in the deb metadata
asks cargo-deb to resolve shared-library dependencies from the built binary via
`dpkg-shlibdeps`; the RPM path relies on the same `ldd`-derived detection.

## Runtime system dependencies

gpui (the GUI toolkit lindsey renders through) needs the X11/Wayland and font
stack at runtime. These are **not** vendored into the package — they are
provided by the desktop environment and must be installed on the target:

- **Debian/Ubuntu** (the `$auto`-detected set, for reference):
  `libxkbcommon0`, `libxkbcommon-x11-0`, `libwayland-client0`,
  `libwayland-cursor0`, `libfontconfig1`, `libx11-6`, `libxcb1`,
  `libxcb-{composite,cursor,damage,dpms,glx,icccm,image,keysyms,present,randr,
  render,render-util,shape,shm,sync,xfixes,xinerama,xkb,xrandr}0`, and a GL
  provider (`libgl1` / `libglx-mesa0`).
- **Fedora/RHEL** equivalents: `libxkbcommon`, `libxkbcommon-x11`,
  `wayland`, `fontconfig`, `libX11`, `libxcb`, and `mesa-libGL`.

The explicit list is documented here rather than hard-coded into `depends`
because RPM and Debian name these differently, and `$auto`/`ldd` resolution is
the more robust source of truth.

## AppImage (optional follow-up)

An AppImage bundles the whole userland and is not covered here. If it becomes
required, the cleanest path is [`cargo-appimage`](https://github.com/StratusFearMe21/cargo-appimage)
or `linuxdeploy` against the `cargo generate-rpm`-style binary — tracked as a
separate change, since the `.deb`/`.rpm` pair already covers the two largest
distributions.
