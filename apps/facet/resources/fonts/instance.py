"""Cuts the static FACET font instances from the variable masters.

GPUI on macOS registers a variable font as one face (its default instance)
and never applies `wght`/`wdth`/`opsz`, so every weight the type ladder uses
is cut here as a static TTF. The coordinates are the ones Chrome resolves for
the board CSS (`font-variation-settings:"opsz" 96` on display-size heads,
`font-optical-sizing:auto` elsewhere, `font-stretch` for `wdth`).

Run from the repository root (see README.md for the pinned interpreter):

    python3 apps/facet/resources/fonts/instance.py

The output is byte-reproducible: timestamps are not recalculated and every
name record is rewritten from the table below.
"""

from pathlib import Path

from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

HERE = Path(__file__).resolve().parent
MASTERS = HERE / "masters"
OUT = HERE / "static"

BRICOLAGE = "BricolageGrotesque[opsz,wdth,wght].ttf"
GEIST = "Geist[wght].ttf"
NEWSREADER_ITALIC = "Newsreader-Italic[opsz,wght].ttf"
# Geist Mono's variable file lives beside `masters/`: its default instance (400) is
# embedded as is, so only the heavier cuts are made here.
GEIST_MONO = "../GeistMono[wght].ttf"

# (output file, master, family, subfamily, italic, axis coordinates)
CUTS = [
    # Text-size display heads (13-22 px): titles, the altimeter, variant heads.
    ("BricolageGrotesque-520.ttf", BRICOLAGE, "Bricolage Grotesque", "Medium", False,
     {"opsz": 18, "wdth": 100, "wght": 520}),
    ("BricolageGrotesque-620.ttf", BRICOLAGE, "Bricolage Grotesque", "SemiBold", False,
     {"opsz": 18, "wdth": 100, "wght": 620}),
    ("BricolageGrotesque-650.ttf", BRICOLAGE, "Bricolage Grotesque", "SemiBold Plus", False,
     {"opsz": 18, "wdth": 100, "wght": 650}),
    ("BricolageGrotesque-720.ttf", BRICOLAGE, "Bricolage Grotesque", "Bold", False,
     {"opsz": 18, "wdth": 100, "wght": 720}),
    # Display-size heads: `.t-display-xl`, `.t-display` pin opsz 96.
    ("BricolageGrotesqueDisplay-620.ttf", BRICOLAGE, "Bricolage Grotesque Display", "SemiBold",
     False, {"opsz": 96, "wdth": 100, "wght": 620}),
    ("BricolageGrotesqueDisplay-640.ttf", BRICOLAGE, "Bricolage Grotesque Display",
     "SemiBold Plus", False, {"opsz": 96, "wdth": 100, "wght": 640}),
    # `.hero-name`: opsz 96, font-stretch 92 %.
    ("BricolageGrotesqueHero-640.ttf", BRICOLAGE, "Bricolage Grotesque Hero", "SemiBold Plus",
     False, {"opsz": 96, "wdth": 92, "wght": 640}),
    # `.book .t`: 17 px (auto opsz), font-stretch 95 %.
    ("BricolageGrotesqueBook-640.ttf", BRICOLAGE, "Bricolage Grotesque Book", "SemiBold Plus",
     False, {"opsz": 17, "wdth": 95, "wght": 640}),
    # Geist, the UI face.
    ("Geist-400.ttf", GEIST, "Geist", "Regular", False, {"wght": 400}),
    ("Geist-500.ttf", GEIST, "Geist", "Medium", False, {"wght": 500}),
    ("Geist-600.ttf", GEIST, "Geist", "SemiBold", False, {"wght": 600}),
    ("Geist-700.ttf", GEIST, "Geist", "Bold", False, {"wght": 700}),
    # Geist Mono, identifiers: names in peek heads, lens titles and tabs are heavier.
    ("GeistMono-500.ttf", GEIST_MONO, "Geist Mono", "Medium", False, {"wght": 500}),
    ("GeistMono-600.ttf", GEIST_MONO, "Geist Mono", "SemiBold", False, {"wght": 600}),
    # Newsreader italic: captions, margins, subs and hints (12.5-16 px)...
    ("Newsreader-Italic-400.ttf", NEWSREADER_ITALIC, "Newsreader", "Italic", True,
     {"opsz": 14, "wght": 400}),
    # ...and ledes (19 px), each at the opsz Chrome's auto optical sizing uses.
    ("NewsreaderLede-Italic-400.ttf", NEWSREADER_ITALIC, "Newsreader Lede", "Italic", True,
     {"opsz": 19, "wght": 400}),
]

# Name records rewritten on every cut (Windows Unicode English and Mac Roman).
DROPPED_NAME_IDS = {16, 17, 21, 22, 25}


def postscript(family: str, subfamily: str) -> str:
    return (family + "-" + subfamily).replace(" ", "")


def cut(filename, master, family, subfamily, italic, coords):
    font = TTFont(MASTERS / master, recalcTimestamp=False)
    static = instantiateVariableFont(font, coords, inplace=False, updateFontNames=False)
    static.recalcTimestamp = False
    if "DSIG" in static:
        del static["DSIG"]

    name = static["name"]
    name.names = [rec for rec in name.names if rec.nameID not in DROPPED_NAME_IDS]
    full = f"{family} {subfamily}"
    for name_id, value in (
        (1, family),
        (2, "Italic" if italic else "Regular"),
        (3, f"FACET;{postscript(family, subfamily)}"),
        (4, full),
        (6, postscript(family, subfamily)),
    ):
        name.setName(value, name_id, 3, 1, 0x409)
        name.setName(value, name_id, 1, 0, 0)

    weight = round(coords["wght"])
    os2 = static["OS/2"]
    os2.usWeightClass = weight
    os2.usWidthClass = 5
    selection = os2.fsSelection & ~(0b1 | 0b100000 | 0b1000000)
    if italic:
        selection |= 0b1
    else:
        selection |= 0b1000000
    os2.fsSelection = selection
    static["head"].macStyle = 0b10 if italic else 0

    OUT.mkdir(exist_ok=True)
    static.save(OUT / filename)
    print(f"{filename}: {family} / {subfamily} {coords}")


def main():
    for spec in CUTS:
        cut(*spec)


if __name__ == "__main__":
    main()
