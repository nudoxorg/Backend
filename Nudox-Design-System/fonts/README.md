# Nudox typeface acquisition — fonts scratchpad

Research/fetch task output. Nothing in `/Users/mileswirht/Downloads/backend` was
modified. All files below are variable TTFs suitable for
`include_bytes!` + `cx.text_system().add_fonts(...)` under GPUI/CoreText
(macOS). None are woff2.

nixpkgs pin used throughout (from `backend/flake.lock` `nodes.nixpkgs.locked.rev`):
`0bb7ec54c8483066ec9d7720e780a5caa71f8612` (aarch64-darwin).

## 1. Bricolage Grotesque — `BricolageGrotesque[opsz,wdth,wght].ttf`

- **Source**: `github.com/google/fonts`, path `ofl/bricolagegrotesque/BricolageGrotesque[opsz,wdth,wght].ttf`,
  fetched at the exact commit nixpkgs pins for its `google-fonts` derivation at
  the rev above: `5174b3333331c966c38f4355d50b03ca1c1df2f9`.
  URL: `https://raw.githubusercontent.com/google/fonts/5174b3333331c966c38f4355d50b03ca1c1df2f9/ofl/bricolagegrotesque/BricolageGrotesque%5Bopsz%2Cwdth%2Cwght%5D.ttf`
- **Why a direct fetch instead of building `google-fonts.override{...}`**: that
  derivation's `src` is `fetchFromGitHub` of the *entire* `google/fonts` repo
  (a single fixed-output derivation — the `fonts` override only filters what
  gets *installed*, not what gets fetched). Its narinfo on cache.nixos.org
  reports a 1136 MiB compressed / 2810 MiB unpacked download for that one
  source tree. I started that build in the background
  (`nix build --no-link --print-out-paths --impure --expr 'let p = (builtins.getFlake "github:NixOS/nixpkgs/0bb7ec54c8483066ec9d7720e780a5caa71f8612").legacyPackages.aarch64-darwin; in p.google-fonts.override { fonts = [ "BricolageGrotesque" "Newsreader" ]; }'`)
  as a cross-check, but rather than block on a 1+ GB fetch to get two files, I
  fetched the identical bytes directly from the pinned commit and verified
  sha256 matches between that pin and current `main` (no upstream drift since
  the nixpkgs pin was cut).
- **sha256**: `413e7357809ddd12fd80a96a8a396de0e401638d4acd3cb3e37532f0472ac682`
- **Size**: 408496 bytes. `file`: TrueType Font data, 20 tables.
- **License**: `BricolageGrotesque-OFL.txt`, SIL OFL 1.1, same commit/path
  (`ofl/bricolagegrotesque/OFL.txt`). sha256:
  `4b5a7d8f37f5602621c8a8d7358a6a2e71317e6c231c661e15aef0275d3e07ba`.
- **fvar axes**: `opsz` 12–96 (default **96**), `wght` 200–800 (default
  **800**), `wdth` 75–100 (default **100**).
- **Named instances** (all pinned at `opsz=14, wdth=100`): ExtraLight(200),
  Light(300), Regular(400), Medium(500), SemiBold(600), Bold(700),
  ExtraBold(800).
- **Name-table / CoreText family — IMPORTANT quirk**:
  - `name` ID 1 (legacy family) = **"Bricolage Grotesque 96pt ExtraBold"**,
    ID 2 (legacy subfamily) = "Regular", ID 4/6 follow suit
    (`BricolageGrotesque-96ptExtraBold`).
  - `name` ID 16 (typographic/preferred family) = **"Bricolage Grotesque"**,
    ID 17 (preferred subfamily) = "96pt ExtraBold".
  - This happens because the file's `fvar` **default** instance is not
    Regular — it's opsz 96 / wght 800 / wdth 100 (the "96pt ExtraBold"
    corner), so whichever tool generated the legacy name records baked that
    corner in as the nominal "Regular" style of a family literally named
    after it. A resolver that only reads ID 1 would see the font's family as
    `"Bricolage Grotesque 96pt ExtraBold"`, not `"Bricolage Grotesque"`.
  - **The string to pass as GPUI `font_family` is `"Bricolage Grotesque"`**
    (ID 16), exactly mirroring how this repo already treats Newsreader today
    (`SERIF_FAMILY = "Newsreader"` in `apps/desktop/src/theme/fonts.rs`, not
    the ID-1 legacy name `"Newsreader 16pt"`). CoreText on macOS reads the
    preferred/typographic name table entries (ID 16/17) when present, so it
    should list this face under one family, `Bricolage Grotesque`, with the
    full `wght`/`wdth`/`opsz` range addressable via variation coordinates —
    **do not rely on the default (unrequested) instance for anything**, since
    unlike every other font here, "no variation specified" on this file means
    ExtraBold at opsz 96, not Regular at opsz 14.

## 2. Geist (sans) — `Geist[wght].ttf`

- **Source**: `github.com/vercel/geist-font`, tag `v1.7.2` (current latest
  release as of 2026-09-25), path `fonts/Geist/variable/Geist[wght].ttf`.
  URL: `https://raw.githubusercontent.com/vercel/geist-font/v1.7.2/fonts/Geist/variable/Geist%5Bwght%5D.ttf`
- **Why not nixpkgs**: `github:NixOS/nixpkgs/<pin>#geist-font` only has
  **1.5.0** (built and confirmed at
  `/nix/store/2p0ggx1iwjd2a1g00ywiv8p00znmddff-geist-font-1.5.0/share/fonts/truetype/Geist[wght].ttf`),
  well behind the `GeistMono[wght].ttf` already vendored in this repo, which
  is version 1.700 (from the same upstream repo's `v1.7.2` tag — verified,
  see below). Used the same upstream repo/tag instead of nixpkgs so both
  Geist faces in the app come from one consistent release.
- **Why the git tree and not the GitHub Release zip**: the release zip's copy
  of `Geist[wght].ttf` is byte-different from the git tree's copy at the same
  tag (same size, same version string "1.800", differ from byte 210 on —
  looks like a rebuild-time table checksum/timestamp, not a content change).
  I checked which one this repo's convention actually matches by re-fetching
  `GeistMono[wght].ttf` both ways: the **git-tree** raw URL
  (`raw.githubusercontent.com/vercel/geist-font/v1.7.2/fonts/GeistMono/variable/GeistMono[wght].ttf`)
  reproduces the repo's existing `apps/desktop/resources/fonts/GeistMono[wght].ttf`
  **byte-for-byte** (sha256 `87c2aff9723544a9adaea19d92e42a33705c9723624801b6e0224c2206a6af0d`
  both sides); the release-zip copy does not. So `Geist[wght].ttf` here is
  pulled the same way, for consistency with what's already vendored.
- **sha256**: `73894e0448cae90a92b6c2f8732b7bb9acb7b94c418bff559dad4a18e1de9659`
- **Size**: 169056 bytes. `file`: TrueType Font data, 21 tables.
- **License**: `Geist-OFL.txt`, SIL OFL 1.1, same tag, repo root `OFL.txt`.
  sha256: `c683bfbcc7e087f5d37a54ef628f10387c451a83ddc459b151403a164ac46c90`
  (textually identical to the `GeistMono-OFL.txt` already in the repo, modulo
  one trailing-space and one final-newline byte).
- **fvar axes**: `wght` 100–900 (default 400).
- **Named instances**: Thin(100), ExtraLight(200), Light(300), Regular(400),
  Medium(500), SemiBold(600), Bold(700), ExtraBold(800), Black(900).
- **CoreText family**: `name` ID 16/17 are absent, so CoreText falls back to
  the legacy ID 1/2 pair — family **`"Geist"`**, subfamily "Regular". Pass
  `"Geist"` as `font_family`, exactly parallel to how `SPECIMEN_FAMILY` is
  `"Geist Mono"` today (also no ID 16/17 on that file).

## 3. Geist Mono — already in repo, confirmed

- `apps/desktop/resources/fonts/GeistMono[wght].ttf` — **not modified,
  not re-copied here.**
- Confirmed variable: `fvar` axis `wght` 100–900 (default 400), 9 named
  instances (Thin…Black), has a `STAT` table. `name` ID 1 = "Geist Mono", no
  ID 16/17. This matches `SPECIMEN_FAMILY = "Geist Mono"` in
  `apps/desktop/src/theme/fonts.rs` and its recorded manifest hash
  `87c2aff9723544a9adaea19d92e42a33705c9723624801b6e0224c2206a6af0d` — verified
  against the live file, matches. Also verified this exact byte content is
  reproducible from `raw.githubusercontent.com/vercel/geist-font/v1.7.2/fonts/GeistMono/variable/GeistMono%5Bwght%5D.ttf`
  (see §2 above) — i.e. the repo's existing file's provenance is now
  independently confirmed as that git tree path, not the release zip.

## 4. Newsreader — upright confirmed, italic fetched

- `apps/desktop/resources/fonts/Newsreader[opsz,wght].ttf` — **not modified,
  not re-copied here.** Confirmed: `fvar` axes `wght` 200–800 (default 400),
  `opsz` 6–72 (default 18); 7 named instances (ExtraLight…ExtraBold), all
  non-italic. `name` ID 1 = "Newsreader 16pt", ID 16 (typographic family) =
  **"Newsreader"** — this is what `SERIF_FAMILY` in `fonts.rs` already uses.
  **No italic axis and no italic named instances — confirmed it lacks
  italics**, matching the task's premise.
  Its sha256 (`8a08d13f8a6c0d51be379a60af84f945f65369a67e509ee3c3bdcc421254d7c1`)
  is reproduced exactly by fetching
  `raw.githubusercontent.com/google/fonts/5174b3333331c966c38f4355d50b03ca1c1df2f9/ofl/newsreader/Newsreader%5Bopsz%2Cwght%5D.ttf`
  — confirms this file's provenance is that exact google/fonts commit, the
  same one nixpkgs pins.
- **New: `Newsreader-Italic[opsz,wght].ttf`**
  - **Source**: same commit, path `ofl/newsreader/Newsreader-Italic[opsz,wght].ttf`.
    URL: `https://raw.githubusercontent.com/google/fonts/5174b3333331c966c38f4355d50b03ca1c1df2f9/ofl/newsreader/Newsreader-Italic%5Bopsz%2Cwght%5D.ttf`
  - **sha256**: `796668611f80b64d5adf182fde3b6f29ed83b4e7cbec7b96937e84ac01364792`
  - **Size**: 495684 bytes. `file`: TrueType Font data, digitally signed
    (has a `DSIG` table), 21 tables.
  - **License**: `Newsreader-OFL.txt` in this dir, sha256
    `fdfad38143ec470553cae82a1e45320bdd1b9ec70415d37bd0171051d8a4ded8`
    — textually identical to the repo's existing `Newsreader-OFL.txt` modulo
    the same trailing-space/final-newline non-difference seen above (i.e.
    it is the same license text already vendored; a fresh copy is included
    here anyway since the task asked for a license text alongside every font).
  - **fvar axes**: `wght` 200–800 (default 400), `opsz` 6–72 (default 18).
  - **Named instances**: ExtraLight Italic(200), Light Italic(300),
    Italic(400), Medium Italic(500), SemiBold Italic(600), Bold Italic(700),
    ExtraBold Italic(800).
  - **CoreText family**: `name` ID 1 = "Newsreader 16pt", ID 2 (subfamily) =
    "Italic"; ID 16 (typographic family) = **"Newsreader"**, ID 17 = "Italic".
    Same family string as the upright file (`"Newsreader"`); CoreText/GPUI
    distinguishes this face from the upright one by style (subfamily
    "Italic"/slant), not by a different family name — so both
    `Newsreader[opsz,wght].ttf` and `Newsreader-Italic[opsz,wght].ttf` should
    be registered with `add_fonts`, and the italic face gets selected by
    requesting italic style within family `"Newsreader"`, not by a distinct
    `font_family` string.

## File manifest in this directory

| file | sha256 |
| --- | --- |
| `BricolageGrotesque[opsz,wdth,wght].ttf` | `413e7357809ddd12fd80a96a8a396de0e401638d4acd3cb3e37532f0472ac682` |
| `BricolageGrotesque-OFL.txt` | `4b5a7d8f37f5602621c8a8d7358a6a2e71317e6c231c661e15aef0275d3e07ba` |
| `Geist[wght].ttf` | `73894e0448cae90a92b6c2f8732b7bb9acb7b94c418bff559dad4a18e1de9659` |
| `Geist-OFL.txt` | `c683bfbcc7e087f5d37a54ef628f10387c451a83ddc459b151403a164ac46c90` |
| `Newsreader-Italic[opsz,wght].ttf` | `796668611f80b64d5adf182fde3b6f29ed83b4e7cbec7b96937e84ac01364792` |
| `Newsreader-OFL.txt` | `fdfad38143ec470553cae82a1e45320bdd1b9ec70415d37bd0171051d8a4ded8` |

All three fonts are licensed SIL Open Font License 1.1 (confirmed by reading
each `OFL.txt` header). No woff2 was used or converted anywhere in this task.

## Tooling used (for reproducibility)

- `nix build --no-link --print-out-paths 'github:NixOS/nixpkgs/0bb7ec54c8483066ec9d7720e780a5caa71f8612#geist-font'`
  → `/nix/store/2p0ggx1iwjd2a1g00ywiv8p00znmddff-geist-font-1.5.0` (used only
  to confirm nixpkgs' Geist is stale at 1.5.0; not the source of the final
  file).
- `nix build --no-link --print-out-paths 'github:NixOS/nixpkgs/0bb7ec54c8483066ec9d7720e780a5caa71f8612#python3Packages.fonttools'`
  → `/nix/store/m2ahq0c7hiyd23zy6p3l2q48l4lmbfhn-python3.14-fonttools-4.63.0`,
  used with
  `/nix/store/llk2h8rxqzv7zh53bi413ffibjrxskxw-python3-3.14.6/bin/python3`
  (built via `nixpkgs#python3`) to read `name`/`fvar` tables — see
  `inspect_font.py` in this directory (reads name IDs 1/2/4/5/6/16/17,
  `fvar` axes, and named instances for any TTF/OTF path given on argv).
- `nix build --no-link --print-out-paths --impure --expr 'let p = (builtins.getFlake "github:NixOS/nixpkgs/0bb7ec54c8483066ec9d7720e780a5caa71f8612").legacyPackages.aarch64-darwin; in p.google-fonts.override { fonts = [ "BricolageGrotesque" "Newsreader" ]; }'`
  — started as a cross-check (1.1 GB fixed-output fetch of the whole
  `google/fonts` monorepo just to filter two families at install time); ran in
  the background and was superseded by the direct pinned-commit fetch above
  once that fetch's sha256 was shown to match current `main`. This background
  build was **not** waited on further and ultimately timed out mid-download
  (exit 144) — it never finished and its result was not used for anything in
  this directory. Every hash quoted above comes from the direct
  `raw.githubusercontent.com` fetches at the pinned commit/tag, verified with
  `shasum -a 256` locally.
- `curl -L` of `raw.githubusercontent.com` at pinned commits/tags for the
  four fetched files plus their `OFL.txt`s.
- `shasum -a 256` for every hash quoted above.
