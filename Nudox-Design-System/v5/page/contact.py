# contact.py: contact sheets of every still (PIL), labelled, for DESIGN.md
import os, sys
from PIL import Image, ImageDraw, ImageFont
S = "/private/tmp/claude-501/-Users-mileswirht-Downloads-backend/4821aaf6-28dc-4f75-b07b-d8fc41618656/scratchpad/wave5/page"
ST = S + "/stills"
F = os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../fonts/Geist[wght].ttf")
font = ImageFont.truetype(F, 22); small = ImageFont.truetype(F, 17)
BG = (22, 26, 34)
def sheet(name, rows, colw, title, crop=None):
    # rows: [(row label, [(label, file)])]
    cells = [[(l, Image.open(os.path.join(ST, f + ".png")).convert("RGB")) for l, f in r] for _, r in rows]
    def fit(im):
        if crop: im = im.crop((0, 0, im.width, min(im.height, crop)))
        s = colw / im.width; return im.resize((colw, int(im.height * s)), Image.LANCZOS)
    cells = [[(l, fit(im)) for l, im in r] for r in cells]
    ncol = max(len(r) for r in cells); lw = 150
    H = 70 + sum(max(im.height for _, im in r) + 50 for r in cells)
    W = lw + ncol * (colw + 20) + 20
    out = Image.new("RGB", (W, H), BG); d = ImageDraw.Draw(out)
    d.text((20, 20), title, fill=(230, 235, 245), font=font)
    y = 70
    for (rl, _), r in zip(rows, cells):
        d.text((20, y + 30), rl, fill=(170, 180, 200), font=font)
        x = lw
        for l, im in r:
            d.text((x, y), l, fill=(150, 160, 180), font=small)
            out.paste(im, (x, y + 26)); x += colw + 20
        y += max(im.height for _, im in r) + 50
    out.save(os.path.join(S, name)); print(name, out.size)
L = [("Rust", "rs"), ("TypeScript", "ts"), ("Go", "go")]
P = {"rs": ["value", "datetime", "from_str", "serialize"], "ts": ["issue", "toosmall", "parse", "collection"], "go": ["errorhandling", "flag", "parse", "value"]}
C = ["choice", "record", "callable", "contract"]
sheet("contact-specimens-fold.png", [(n, [(f"{C[i]} · {k}-{P[k][i]}", f"fold-{k}-{P[k][i]}-1440") for i in range(4)]) for n, k in L], 560, "The specimen, three languages x four concepts: the first screen at 1440x900, Abyss")
sheet("contact-full-1440.png", [(n, [(f"{C[i]} · {P[k][i]}", f"{k}-{P[k][i]}-1440") for i in range(4)]) for n, k in L], 420, "Whole pages at 1440, Abyss")
sheet("contact-full-760.png", [(n, [(f"{C[i]} · {P[k][i]}", f"{k}-{P[k][i]}-760") for i in range(4)]) for n, k in L], 300, "Whole pages at 760, Abyss")
sheet("contact-glacier.png", [("Glacier", [("rs-value", "rs-value-1440-glacier"), ("ts-issue", "ts-issue-1440-glacier"), ("ts-parse", "ts-parse-1440-glacier")]), ("", [("go-parse", "go-parse-1440-glacier"), ("go-value", "go-value-1440-glacier"), ("pkg-toml", "pkg-toml-1440-glacier")])], 560, "Glacier at 1440", crop=1500)
sheet("contact-directions.png", [(p, [("A: drawn", f"dirA-{p}"), ("B: typeset", f"dirB-{p}")]) for p in ["rs-value", "ts-issue", "rs-from_str", "go-parse", "go-value", "ts-toosmall"]], 640, "Two directions for the specimen: A drawn (recommended) vs B typeset")
sheet("contact-package.png", [("toml", [("1440", "pkg-toml-1440"), ("760", "pkg-toml-760"), ("1440 Glacier", "pkg-toml-1440-glacier")])], 560, "The package page")
sheet("contact-states.png", [("states", [("hover: the Table case (grammar at 0 ms + peek at 350 ms)", "hover-rs-value-table"), ("x-ray (option): exact spellings beside the drawing", "xray-rs-from_str")])], 700, "One hover grammar, and x-ray")
