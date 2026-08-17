# Windows packaging

lindsey is being migrated to **gpui-ce** (which renders through the **wgpu**
backend), so the Windows story is real but deliberately lighter than macOS/Linux:
there is no Nix coverage for Windows, and no `cargo-bundle` support (cargo-bundle
is macOS/Linux-only). The standard Windows tooling is **cargo-wix** (MSI via the
WiX Toolset).

## Prerequisites (all Windows-only)

- **MSVC Rust toolchain** — `rustup target add x86_64-pc-windows-msvc` plus the
  "Desktop development with C++" Visual Studio workload (for the MSVC linker and
  Windows SDK headers gpui-ce links against).
- **WiX Toolset v3** — install from https://wixtoolset.org/ and make sure
  `candle.exe` and `light.exe` are on `PATH`.
- **cargo-wix** — `cargo install cargo-wix`.

## Producing the app icon (`assets/lindsey.ico`)

There is no committed `.ico`: it must be rasterised from the single brand mark
`assets/logo.svg` (the same source the macOS `.icns` is generated from). Do not
commit the generated binary. One of:

```sh
# ImageMagick (multi-resolution .ico in one command)
magick assets/logo.svg -define icon:auto-resize=256,128,64,48,32,16 assets/lindsey.ico

# rsvg-convert + icotool (librsvg + icoutils)
for s in 16 32 48 64 128 256; do
  rsvg-convert -w $s -h $s assets/logo.svg -o /tmp/lindsey-$s.png
done
icotool -c -o assets/lindsey.ico /tmp/lindsey-*.png
```

The `.wxs` and `[package.metadata.wix]` (if you run `cargo wix init`) both point
at `assets/lindsey.ico`.

## `main.wxs`

`main.wxs` in this directory is a **reviewable template**, not the output of
`cargo wix init`. It pins the structure that matters:

- per-machine install (`InstallScope="perMachine"`),
- a fixed `UpgradeCode` with `MajorUpgrade` semantics (old versions removed on
  upgrade, downgrades refused),
- the app icon wired to the `.exe`, the Add/Remove Programs entry
  (`ARPPRODUCTICON`), and the Start Menu shortcut,
- `WixUI_InstallDir` for an install-location dialog.

Path convention: `candle`/`light` are run from the crate root
(`workspace/gui/`), so `Source`/`SourceFile` are crate-root-relative
(`target/release/lindsey.exe`, `assets/lindsey.ico`). If you invoke the WiX
toolset by hand against this file, copy it to `wix/main.wxs` (cargo-wix's
expected location) or rewrite those paths.

## Building the MSI

Preferred flow — let cargo-wix generate and build in one step (this also builds
the release binary first):

```sh
cd workspace/gui
cargo wix init   # one-time: generates wix/main.wxs wired to [package.metadata.wix]
cargo wix        # builds target/release/lindsey.exe, then candle+light -> MSI
```

Manual flow against this template (skips `cargo wix init`):

```sh
cd workspace/gui
cargo build --release
candle.exe -arch x64 -out target/wix/ packaging/windows/main.wxs
light.exe -out target/wix/lindsey-0.1.0-x64.msi target/wix/main.wixobj \
  -ext WixUIExtension
```

The output MSI installs `lindsey.exe` to `Program Files\lindsey\` and drops a
Start Menu shortcut. Signing is out of scope here (no certificate is committed);
wire it into `[package.metadata.wix]`'s signing fields or a CI secret when one
exists.

## What Windows does *not* get (yet)

- **No Nix/`flake.nix` coverage** — Nix cannot target Windows; the dev shell and
  `nix build` remain macOS/Linux-only (see docs/LIMITATIONS.md).
- **No cross-compilation from Linux/macOS** — gpui-ce/wgpu needs the MSVC
  toolchain and Windows SDK; build natively on Windows.
- **No code signing** — no certificate is present in the repo.
