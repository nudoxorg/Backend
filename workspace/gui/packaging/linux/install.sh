#!/bin/sh
# Install lindsey into a user's desktop session.
#
# Four files have to land in four places for a Linux desktop application to be
# a desktop application rather than a binary someone runs from a terminal:
#
#   the launcher      ~/.local/bin/lindsey            (generated wrapper)
#   the binary        ~/.local/share/lindsey/lindsey
#   the desktop entry ~/.local/share/applications/org.nudox.lindsey.desktop
#   the icon          ~/.local/share/icons/hicolor/scalable/apps/org.nudox.lindsey.svg
#   the metainfo      ~/.local/share/metainfo/org.nudox.lindsey.metainfo.xml
#
# The three ids below are all the *same string* — the application
# id. That is not stylistic: the entry's `Icon=` key is looked up by name in the
# icon theme, `appstreamcli compose` finds the entry through the metainfo's
# `<launchable/>`, and a Wayland compositor matches the window to the entry by
# the `app_id` the app sets (`app::desktop::APP_ID`). Break the agreement
# anywhere and the app loses its icon somewhere specific: the menu, the
# software centre, or the taskbar.
#
# Paths follow the XDG base directory specification, so everything is
# per-user and nothing needs root. Set XDG_DATA_HOME / PREFIX to relocate.
#
# Usage:
#   ./packaging/linux/install.sh              # installs the release build
#   BIN=target/debug/lindsey ./…/install.sh   # or an explicit binary
#   ./packaging/linux/install.sh --uninstall

set -eu

here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
gui_root="$(CDPATH= cd -- "$here/../.." && pwd)"
repo_root="$(CDPATH= cd -- "$gui_root/../.." && pwd)"

app_id="org.nudox.lindsey"

data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
bin_dir="${PREFIX:-$HOME/.local}/bin"
applications_dir="$data_home/applications"
icon_dir="$data_home/icons/hicolor/scalable/apps"
metainfo_dir="$data_home/metainfo"
# The real binary lives here; `lindsey` on PATH is a wrapper (see below).
libexec_dir="$data_home/lindsey"

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$bin_dir/lindsey" \
          "$libexec_dir/lindsey" \
          "$applications_dir/$app_id.desktop" \
          "$icon_dir/$app_id.svg" \
          "$metainfo_dir/$app_id.metainfo.xml"
    echo "removed lindsey from $data_home"
    # The caches are rebuilt below on install; refresh them here too so the
    # menu entry disappears now rather than at next login.
    command -v update-desktop-database >/dev/null 2>&1 &&
        update-desktop-database "$applications_dir" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 &&
        gtk-update-icon-cache -q -t "$data_home/icons/hicolor" 2>/dev/null || true
    exit 0
fi

# The binary. Release unless told otherwise, because an installed debug build
# is a 76 MB binary that a user will assume is how fast the app is.
bin="${BIN:-$gui_root/target/release/lindsey}"
if [ ! -x "$bin" ]; then
    echo "install.sh: no binary at $bin" >&2
    echo "  build one first:  cd $gui_root && cargo build --release" >&2
    echo "  or point BIN= at an existing binary" >&2
    exit 1
fi

# The icon is the brand mark itself, not a copy of it. `/logo.svg` is the one
# file every visual artefact in this repo is generated from (see
# packaging/generate_dmg_background.py), and its root <svg> carries a square
# viewBox and no fixed width, which is exactly what a scalable icon needs.
# Copying it into this directory would create a second brand mark to keep in
# step with the first.
icon_source="$repo_root/logo.svg"
if [ ! -f "$icon_source" ]; then
    echo "install.sh: no brand mark at $icon_source" >&2
    exit 1
fi

# `mkdir -p` + `cp` + `chmod` rather than `install -D`, deliberately.
#
# This repository's own devshell used to put a Nushell script named `install`
# ahead of coreutils on PATH, so `install -D` here failed with
# "The install.nu command doesn't have flag -D" — the same shadowing that broke
# `tikv-jemalloc-sys`'s autotools build (see flake.nix, `shadowingCommands`).
# The flake no longer does that, but an installer is the last place that should
# depend on which shell it is run from.
mkdir -p "$bin_dir" "$libexec_dir" "$applications_dir" "$icon_dir" "$metainfo_dir"

cp -f "$bin"                        "$libexec_dir/lindsey"
# The shipped entry says `Exec=lindsey`, which is right for a distribution
# package installing to /usr/bin. For a per-user install it is a trap: a
# graphical session's PATH is not the shell's, and ~/.local/bin is frequently
# absent from it, so the menu entry fails to find a binary that `which` can see.
# Rewrite it to the absolute launcher — the same rewrite Flatpak performs on
# install, for the same reason.
sed "s|^Exec=lindsey|Exec=$bin_dir/lindsey|" \
    "$here/$app_id.desktop" > "$applications_dir/$app_id.desktop"
cp -f "$icon_source"                "$icon_dir/$app_id.svg"
cp -f "$here/$app_id.metainfo.xml"  "$metainfo_dir/$app_id.metainfo.xml"

# ── The launcher ─────────────────────────────────────────────────────────────
#
# The real binary goes to libexec and `lindsey` on PATH is a generated wrapper.
# That is not ceremony: a binary built inside this repo's Nix devshell resolves
# libxcb and libxkbcommon out of the Nix store, and nothing outside the devshell
# has those on its search path. Copying it straight to ~/.local/bin produced a
# menu entry that did exactly this when clicked:
#
#   lindsey: error while loading shared libraries: libxcb.so.1:
#            cannot open shared object file: No such file or directory
#
# — i.e. an installed application that cannot be launched the one way users
# launch applications, while working perfectly from the terminal it was built
# in. The library path is captured *at install time*, from the environment that
# has it, and baked in. Store paths are immutable, so a captured one stays valid
# until the devshell is rebuilt and the app reinstalled.
#
# A distribution build, linked against the distribution's own libraries, has an
# empty LD_LIBRARY_PATH here and gets a wrapper that only adds the GPU driver
# path — which is the other thing a bare exec would miss on a non-NixOS host.
cat > "$bin_dir/lindsey" <<WRAPPER
#!/bin/sh
# Generated by packaging/linux/install.sh — edits here are lost on reinstall.
set -eu

libs="${LD_LIBRARY_PATH:-}"

# The GPU driver has to come from the host: the Vulkan ICD names its vendor
# library by bare soname. See packaging/linux/run-lindsey for the long version.
for d in /run/opengl-driver/lib /usr/lib; do
    if [ -d "\$d" ]; then libs="\${libs:+\$libs:}\$d"; break; fi
done

[ -n "\$libs" ] && export LD_LIBRARY_PATH="\$libs"
exec "$libexec_dir/lindsey" "\$@"
WRAPPER

chmod 755 "$bin_dir/lindsey" "$libexec_dir/lindsey"

# ── Make the binary stand on its own ─────────────────────────────────────────
#
# The wrapper above is not enough, and assuming it was is what shipped a broken
# install: anything that reaches the binary directly rather than through
# `lindsey` on PATH — a launcher configured with the real path, a user who
# copied it, a debugger, `systemd-run` — got
#
#   error while loading shared libraries: libxcb.so.1
#
# because the loader consults only DT_RUNPATH and LD_LIBRARY_PATH, and a
# devshell build has neither pointing at the Nix store. Writing the path into
# the ELF fixes it for *every* caller instead of one.
#
# The order matters and matches the wrapper's: store paths first so they keep
# priority, host driver directory last so it supplies only what Nix does not.
if command -v patchelf >/dev/null 2>&1 && [ -n "${LD_LIBRARY_PATH:-}" ]; then
    rpath="$LD_LIBRARY_PATH"
    for d in /run/opengl-driver/lib /usr/lib; do
        if [ -d "$d" ]; then rpath="$rpath:$d"; break; fi
    done
    patchelf --set-rpath "$rpath" "$libexec_dir/lindsey"
    echo "  (rpath written into the binary; it runs without the wrapper)"
elif [ -n "${LD_LIBRARY_PATH:-}" ]; then
    echo "note: patchelf not found — the binary only runs through the wrapper" >&2
fi
chmod 644 "$applications_dir/$app_id.desktop" \
          "$icon_dir/$app_id.svg" \
          "$metainfo_dir/$app_id.metainfo.xml"

# Refresh the two caches that decide whether the entry and icon are visible
# *now* rather than after the next login. Both are optional: a machine without
# them picks the files up on its own schedule.
command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$applications_dir" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 &&
    gtk-update-icon-cache -q -t "$data_home/icons/hicolor" 2>/dev/null || true

echo "installed lindsey:"
echo "  $bin_dir/lindsey            (wrapper)"
echo "  $libexec_dir/lindsey  (binary)"
echo "  $applications_dir/$app_id.desktop"
echo "  $icon_dir/$app_id.svg"
echo "  $metainfo_dir/$app_id.metainfo.xml"

case ":${PATH}:" in
    *":$bin_dir:"*) ;;
    *) echo "note: $bin_dir is not on your PATH" ;;
esac

# Run this from inside the devshell when the binary was built there: the
# wrapper captures LD_LIBRARY_PATH at install time, and a shell without the
# GUI libraries on it produces a wrapper that cannot find libxcb either.
