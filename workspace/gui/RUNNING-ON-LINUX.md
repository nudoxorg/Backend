# RUNNING-ON-LINUX.md — lindsey on Linux

What the Linux build needs, why each piece is needed, and how each failure
announces itself. Every symptom below has been reproduced on this tree.

lindsey is a GPUI application. On Linux GPUI draws through **wgpu** and talks to
either a **Wayland compositor** or an **X11 server**; on macOS it draws through
Metal and neither of those exists. Nearly everything Linux-specific here follows
from that one difference.

---

## The short version

From the devshell, nothing extra is required:

```sh
cd workspace/gui
cargo build
./packaging/linux/run-lindsey                  # fixtures corpus

NUDOX_CORPUS=package NUDOX_PACKAGE_ROOT=/path/to/crate \
  ./packaging/linux/run-lindsey
```

The wrapper exists for one reason — putting the host's GPU driver on the
loader's path for this process only. See "The one part that is not hermetic".
`cargo run` is fine on a host where the Vulkan driver is already reachable.

`workspace/gui` is a **standalone Cargo workspace** with its own lockfile — it
is deliberately not a member of the root workspace, which is what keeps `gpui`
out of the backend's dependency graph. Build it from its own directory; a
`-p lindsey` from the repo root will not find it.

---

## What the devshell provides, and why

Four native libraries exist in the devshell for lindsey and for nothing else
(`flake.nix`, `guiGraphicsLibraries`):

| Library | How it is reached | Used by |
|---|---|---|
| `libxcb` | `DT_NEEDED` — linked | the X11 backend (`x11rb`, clipboard) |
| `libxkbcommon` (+ `-x11`) | `DT_NEEDED` — linked | keymap handling, both backends |
| `libwayland-client` | **dlopened by soname** | the Wayland backend |
| `libvulkan` | **dlopened by soname** | wgpu's Vulkan backend |

The bottom two never appear in `patchelf --print-needed`. That matters when
diagnosing: a binary can look completely satisfied and still fail to open a
window.

### The failure this prevents

Before those libraries were in the devshell, the build did **not** fail. There
is a `pkg-config` on the host `PATH`, and it answered from `/usr/lib/pkgconfig`.
The link succeeded against the host's copies, and the result was a binary that
mixed Nix's dynamic linker with the host's glibc:


```
./target/debug/lindsey: /nix/store/…-glibc-2.42-67/lib/libm.so.6:
version `GLIBC_2.43' not found (required by ./target/debug/lindsey)
```

A build that silently links half the host is worse than one that fails, so the
devshell now supplies its own `pkg-config` as well. To confirm a build is clean:

```sh
objdump -p target/debug/lindsey | grep -o 'GLIBC_2\.[0-9]*' | sort -uV | tail -1
```

The highest version must be **at or below** the devshell's glibc
(`ldd --version`). Today that is `GLIBC_2.39` against a 2.42 loader.

### Two devshell traps that only fire on Linux

Both were found by this build failing, and both are fixed in `flake.nix`. They
are recorded here because the error messages point nowhere near the cause.

**`install` and `patch` were Nushell scripts.** The devshell's convenience
commands land ahead of coreutils on `PATH`, so a C dependency running autotools
got `The install.nu command doesn't have flag -c` followed by
`configure: error: cannot determine return type of strerror_r`. Those two
commands are now `nudox-install`, `nudox-install-force` and `nudox-patch`;
every other devshell command keeps its bare name.

**Fortification versus `-O0`.** Nix's default hardening injects
`-D_FORTIFY_SOURCE=3`; cargo's dev profile compiles C dependencies at `-O0`;
glibc emits `#warning _FORTIFY_SOURCE requires compiling with optimization`.
Harmless until an autotools probe uses `-Werror` — `tikv-jemalloc-sys` does, so
both of its `strerror_r` probes failed and configure aborted. The shell now sets
`hardeningDisable = [ "fortify" "fortify3" ]`.

This one reproduces **only in debug builds**: `--release` compiles the same C at
`-O3`, where the warning never fires. A green `cargo build --release` is not
evidence that `cargo build` works.

### The one part that is not hermetic

The Vulkan **driver** cannot come from the Nix store on a non-NixOS host. The
ICD manifest the loader reads names its vendor library by bare soname:

```json
{ "ICD": { "library_path": "libGLX_nvidia.so.0" } }
```

— not a path, so the directory holding it has to be searchable at run time.
This is the seam `nixGL` exists to paper over, and it is why the GUI is the only
target here that depends on the host.

It is **not** on the devshell's `LD_LIBRARY_PATH`. That was tried, and it broke
the build: with `/usr/lib` searched first, the Nix JDK loaded the host's
`libnet.so` and `nudox-producer-java`'s build script died on
`undefined symbol: reuseport_available`. A host library directory visible to
every compiler and build script in the repo will keep finding new ways to do
that. Instead the devshell exports `NUDOX_GUI_DRIVER_PATH`, and
`packaging/linux/run-lindsey` applies it to the one process that needs it:

```sh
./packaging/linux/run-lindsey            # release if built, else debug
LINDSEY_BIN=… ./packaging/linux/run-lindsey
```

`cargo run` works too on a machine whose Vulkan driver is already reachable
without help — a NixOS host, or any host where the loader finds the ICD's
vendor library on its own.

Note what is deliberately **not** in that list: `libglvnd` and `fontconfig`.
Both must come from the host. A store `libGL` cannot find the host's EGL vendor
library and panics inside `khronos-egl`; a store `fontconfig` has no font
configuration and renders nothing.

---

## Failure modes, and what each one says

### No display server

```
lindsey: no display server: neither WAYLAND_DISPLAY nor DISPLAY is set.
```

lindsey checks this itself, before starting the engine (`app::desktop`). It has
to: `gpui::guess_compositor` treats a bare environment as a request for the
**headless** platform and builds a real one. Without the check the process
starts, loads the corpus, "opens" a window, renders every frame into nothing and
never reports a problem — over SSH, in a container, from a bare TTY, or from a
service manager that did not import the graphical session's environment.

Genuinely want headless (profiling)? `ZED_HEADLESS=1` — the same variable GPUI
reads.

### No usable GPU backend

```
lindsey: could not open a window on Wayland: Failed to create surface: …
```

The window is drawn through wgpu, so this means no backend was reachable. Check
`vulkaninfo --summary`. On a machine with no GPU at all, Mesa's lavapipe
software renderer is enough.

### A library missing at exec

```
error while loading shared libraries: libxcb.so.1: cannot open shared object file
```

The devshell is not active, or the binary was built outside it.

---

## Desktop integration

`packaging/linux/org.nudox.lindsey.desktop` is the desktop entry.

The window declares `app_id = "org.nudox.lindsey"` (`app::desktop::APP_ID`).
On Wayland that is *the* mechanism: a surface carries no icon and no window
class, so the compositor matches it to an installed entry by app id alone.
Without it, GNOME and KDE show the window as an unnamed placeholder in the
taskbar, the dock and alt-tab. The entry's `StartupWMClass` must equal it —
`app_id_matches_desktop_entry` fails the build if the two drift.

The window also declares a **minimum size** of 800×600. Tiling window managers
do not ask a window what size it would like; they hand it whatever the layout
has left, which can be a 200 px column. Below roughly that size the dock layout
has nowhere to put the centre pane (`LAYOUT-TRAPS.md`, Trap 1, at window scale).

To install for the current user:

```sh
install -Dm644 packaging/linux/org.nudox.lindsey.desktop \
  ~/.local/share/applications/org.nudox.lindsey.desktop
install -Dm755 target/release/lindsey ~/.local/bin/lindsey
update-desktop-database ~/.local/share/applications
```

---

## Verified on this tree

- KDE Plasma / `kwin_wayland`, Wayland session, Arch host, NVIDIA ICD, glibc 2.43
- Nix devshell glibc 2.42, rustc nightly 2026-07-11
- Window opens, renders, and holds; fixture corpus loads (23 symbols)

X11 is compiled in (`gpui_platform` features `x11`, `wayland`) and selected when
`WAYLAND_DISPLAY` is unset, but has not been exercised here.
