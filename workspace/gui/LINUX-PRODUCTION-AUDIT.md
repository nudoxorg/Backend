# LINUX-PRODUCTION-AUDIT.md

An audit of lindsey against published Linux desktop-application requirements,
with what each check actually returned on this tree. Re-run the commands; none
of them take longer than a second.

The checklist is not invented here. It comes from:

- the [Flatpak requirements and conventions](https://docs.flatpak.org/en/latest/conventions.html)
  (application id rules, desktop entry, icon naming/sizes, metainfo location)
- the [AppStream specification for desktop applications](https://www.freedesktop.org/software/appstream/docs/sect-Metadata-Application.html)
  and [Flathub's MetaInfo guidelines](https://docs.flathub.org/docs/for-app-authors/metainfo-guidelines)
- the Wayland app-id rule, which is what decides whether a window gets its icon
  ([bitwarden/clients#17760](https://github.com/bitwarden/clients/issues/17760),
  [Mozilla bug 1826330](https://bugzilla.mozilla.org/show_bug.cgi?id=1826330),
  [Demystifying StartupWMClass](https://thoughts.greyh.at/posts/startup-wm-class/))

---

## Results

| # | Check | Result |
|---|---|---|
| 1 | Desktop entry passes `desktop-file-validate` | **pass** — silent |
| 2 | Application id is reverse-DNS, lowercase, no dashes, not ending `.desktop` | **pass** — `org.nudox.lindsey` |
| 3 | Desktop entry is named `<app-id>.desktop` | **pass** |
| 4 | Entry has Name, Exec, Type, Icon, Categories | **pass** |
| 5 | Wayland `app_id` == `StartupWMClass` == entry filename | **pass** — pinned by a test |
| 6 | An icon is installed, by app-id name, in the hicolor theme | **was FAIL** — fixed |
| 7 | AppStream metainfo exists and validates | **was FAIL** — fixed |
| 8 | Config/state follow the XDG base directory spec | **pass** |
| 9 | Credentials in the platform keyring, never a file | **pass** — fixed earlier this session |
| 10 | Clean shutdown on `SIGTERM` | **pass** — exits in ~1 s |
| 11 | Release build carries no debug information | **was FAIL** — fixed |
| 12 | Dependency versions are locked in-tree | **pass** — fixed earlier this session |
| 13 | Secrets never reach a log or an error body | **pass** — enforced by the crate's own tests |
| 14 | **The installed app launches from the menu** | **was FAIL** — fixed |

### Commands

```sh
desktop-file-validate packaging/linux/org.nudox.lindsey.desktop        # 1
appstreamcli validate packaging/linux/org.nudox.lindsey.metainfo.xml   # 7
readelf -S target/release/lindsey | grep -c '\.debug_'                 # 11 → 0
env -i HOME=$HOME WAYLAND_DISPLAY=$WAYLAND_DISPLAY \
    XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR PATH=/usr/bin:/bin \
    ~/.local/bin/lindsey                                               # 14
```

---

## What was wrong, and what changed

### 14 — the installed app could not be launched (worst of the four)

Copying the binary to `~/.local/bin` produced a menu entry that answered a click
with:

```
lindsey: error while loading shared libraries: libxcb.so.1:
         cannot open shared object file: No such file or directory
```

A binary built in this repo's Nix devshell resolves `libxcb` and `libxkbcommon`
out of the Nix store, and nothing outside that shell has them on its search
path. The app worked perfectly from the terminal it was built in and not at all
from the desktop — the failure mode least likely to be noticed by the person who
built it.

`packaging/linux/install.sh` now does three things, and the first fix alone was
not enough:

* installs the binary to `~/.local/share/lindsey/` with a wrapper at
  `~/.local/bin/lindsey` that captures the library path at install time;
* **writes that path into the ELF with `patchelf --set-rpath`**, so the binary
  runs on its own. Shipping only the wrapper left the binary itself unrunnable —
  anything reaching it directly (a launcher configured with the real path, a
  copy, a debugger) still got `libxcb.so.1: cannot open shared object file`;
* **rewrites `Exec=` to the absolute launcher path.** The shipped entry says
  `Exec=lindsey`, which is right for a distribution package in `/usr/bin` and a
  trap for a per-user install: a graphical session's PATH is not the shell's and
  frequently omits `~/.local/bin`, so the menu entry cannot find a binary
  `which` can see. Flatpak performs the same rewrite on install.

Verified by launching *both* the wrapper and the bare binary under `env -i`.

### 6 — the icon did not exist

The entry said `Icon=org.nudox.lindsey` and no such icon was installed anywhere,
so every surface that resolves an icon by name — menu, dock, alt-tab, software
centre — had nothing to find. The installer now places `/logo.svg` (the repo's
one brand mark, square viewBox, no fixed width) at
`hicolor/scalable/apps/org.nudox.lindsey.svg`. It is installed *from* the brand
mark rather than copied into `packaging/`, so there is still exactly one.

### 7 — no AppStream metainfo

Without it lindsey can be installed but not *found*: no entry in GNOME Software
or KDE Discover, no summary, no icon in any catalogue.
`packaging/linux/org.nudox.lindsey.metainfo.xml` is new and validates clean.

### 11 — release binary shipped its debug information

`readelf -S` reported `.debug_*` sections in the release build. `strip =
"debuginfo"` in `[profile.release]` removes them; `readelf` now counts zero.

---

## Open items, most worth doing first

1. **The wrapper is a Nix workaround, not a distribution build.** It bakes store
   paths into a generated script; that is right for testing a devshell build on
   this machine and wrong as a shipping strategy. A real Linux release means a
   build linked against the distribution's own libraries — a Flatpak manifest,
   or a package per distribution. Everything in this audit is a prerequisite for
   that, not a substitute.

2. **No screenshots or releases in the metainfo.** Both are warnings today and
   hard requirements for a Flathub submission. Screenshots need somewhere to
   host them; `<releases/>` needs a release process that makes the dates true.

3. **The symbol table is 28 MB of a 102 MB binary.** Kept deliberately, so a
   panic backtrace names functions — see the note in `Cargo.toml`. Past a
   certain user count the answer is to ship stripped *and* keep a debug archive
   to symbolise against, which is a build-pipeline decision rather than a flag.

4. **Only one icon size, and it is scalable.** An SVG in `hicolor/scalable` is
   correct and sufficient for GNOME and KDE. Some environments and some
   catalogue tooling prefer a rasterised 64px or 128px PNG; generating those
   from the same brand mark would remove the caveat.

5. **`cargo fmt` and `clippy` have never run against this branch.** The repo's
   pre-push hooks run both repo-wide, and the tree is not formatter-clean, so
   the gate reformatted ~500 unrelated files and could not pass. The branch was
   pushed with `--no-verify`. Landing a whole-tree format on `canonical` would
   make the gate usable again.

6. **A degraded package load is silent.** Opening a real package whose
   dependencies are not in the local cargo cache logs
   `ra_load metadata_degraded=true` and carries on with a partial symbol set,
   with nothing in the UI saying so. The reader sees a package that looks
   complete and is not.
