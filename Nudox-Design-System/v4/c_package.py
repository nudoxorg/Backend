"""Calm package page: the hero, one release comb with one catch-up line, and the territory.

The territory is monochrome: a region per module, a stone per public item;
only the stones your code reaches are lit (mint). Family hues, counts and
breakdowns wait for a rest (the module lens) or ⌥.
"""
import random

from nx4 import gem, esc, lang
import calm


def squarify(items, x, y, w, h):
    """Squarified treemap: items = [(name, value)], returns [(name, value, x, y, w, h)]."""
    items = sorted(items, key=lambda t: -t[1])
    total = sum(v for _, v in items)
    scale = (w * h) / total
    rects = []

    def worst(row, side):
        s = sum(v for _, v in row) * scale
        return max(max(side * side * v * scale / (s * s), (s * s) / (side * side * v * scale)) for _, v in row)

    def layout(row, x, y, w, h):
        s = sum(v for _, v in row) * scale
        if w >= h:
            cw = s / h
            cy = y
            for n, v in row:
                ch = v * scale / cw
                rects.append((n, v, x, cy, cw, ch))
                cy += ch
            return x + cw, y, w - cw, h
        ch = s / w
        cx = x
        for n, v in row:
            cw = v * scale / ch
            rects.append((n, v, cx, y, cw, ch))
            cx += cw
        return x, y + ch, w, h - ch

    row = []
    rest = list(items)
    while rest:
        side = min(w, h)
        nxt = rest[0]
        if not row or worst(row + [nxt], side) <= worst(row, side):
            row.append(nxt)
            rest.pop(0)
        else:
            x, y, w, h = layout(row, x, y, w, h)
            row = []
    if row:
        layout(row, x, y, w, h)
    return rects


MODULES = [("de", 96, 17), ("ser", 72, 9), ("de::value", 58, 3), ("de::impls", 44, 0), ("ser::impls", 38, 0),
           ("de::size_hint", 9, 0), ("ser::fmt", 12, 1), ("__private", 41, 0), ("macros", 6, 2), ("de::ignored_any", 7, 1)]

SHELF_ROWS = [("module", "de"), ("module", "ser"), ("module", "de::value"), ("module", "de::impls"), ("module", "ser::impls"),
              ("module", "__private", 0, "dim"), ("module", "macros")]


def hero():
    return ('<div class="chero">' + gem("package", 64)
            + '<div class="col" style="min-width:0"><span class="nm">serde</span>'
            '<span class="ld">A generic serialization and deserialization framework.</span></div></div>'
            + calm.facts(f'{lang("rust", 12)} crates.io', '<span class="mono">1.0.193</span>', 'used by <b>3</b> of your projects'))


def releases(n=64, pin=44, reading=46):
    rng = random.Random(7)
    ticks = []
    for i in range(n):
        h = 6 + rng.randint(0, 9)
        cls = ""
        if i == pin:
            cls, h = "pin", 18
        elif i == reading:
            cls, h = "read", 18
        elif i > pin and i in (52, 58):
            cls, h = "yours", 14
        ticks.append(f'<i class="{cls}" style="height:{h}px"></i>')
    return ('<div class="crel"><div class="ticks">' + "".join(ticks) + '</div>'
            '<div class="ccap"><span class="mono">1.0.147</span><span class="mid">19 releases since your pin, '
            '<b>2</b> touch your code</span><span class="mono">1.0.210</span></div></div>')


def territory(W=780):
    items = [(m, v) for m, v, _ in MODULES]
    area = sum(v for _, v in items) * 15.5 * 15.5 * 1.25
    H = max(260, area / W)
    rects = squarify(items, 0, 0, W, H)
    reach = {m: r for m, _, r in MODULES}
    out = [f'<div class="terr" style="width:{W}px;height:{H:.0f}px">']
    for name, v, x, y, w, h in rects:
        pitch = 15.5
        cols = max(1, int((w - 14) // pitch))
        lit = reach.get(name, 0)
        rng = random.Random(name)
        lit_idx = set(rng.sample(range(v), min(lit, v)))
        stones = "".join(f'<i class="{"on" if i in lit_idx else ""}"></i>' for i in range(v))
        label = (f'<span class="rl">{esc(name)}</span>' if w > 70 and h > 44 else "")
        hov = " hov" if name == "de" else ""
        out.append(f'<div class="reg{hov}" style="left:{x:.0f}px;top:{y:.0f}px;width:{w - 3:.0f}px;height:{h - 3:.0f}px">{label}'
                   f'<div class="st" style="grid-template-columns:repeat({cols},10px)">{stones}</div></div>')
    out.append('</div>')
    return "".join(out)


# Start here: the package's reading path (graph/tour.js), computed not written. docs.rs lists items
# alphabetically; the page says which few to read, in the order you meet them. T flies it in the graph.
TOUR = [("trait", "Serialize", "the idea", "486 types do it"),
        ("trait", "Deserialize", "and the other half", "412 types do it"),
        ("enum", "Error", "when it fails", "")]


def start_here(width):
    stops = "".join(
        (f'<span class="tl"></span>' if i else "")
        + f'<a class="ts{" on" if i == 0 else ""}">{gem(k, 18)}<span class="tx"><b>{esc(n)}</b><em>{esc(role)}</em></span></a>'
        for i, (k, n, role, _why) in enumerate(TOUR))
    rows = "".join(
        f'<a class="tr">{gem(k, 18)}<b>{esc(n)}</b><em>{esc(role)}</em><span>{esc(why)}</span></a>'
        for (k, n, role, why) in TOUR)
    fly = '<span class="fly"><kbd>T</kbd>fly it</span>'
    if width >= 760:
        return f'<div class="shere"><span class="sh">Start here</span><div class="strip">{stops}</div>{fly}</div>'
    return f'<div class="shere col"><div class="shh"><span class="sh">Start here</span>{fly}</div><div class="rows">{rows}</div></div>'


def folio(width):
    tw = max(300, min(800, width - 96))
    return ('<div class="cfol" style="max-width:896px">' + hero() + releases()
            + calm.tabs(["Map", "Readme", "Depends", "Used by", "Changes"])
            + start_here(width)
            + territory(tw) + '</div>')


CSS = """
.crel{display:flex;flex-direction:column;gap:8px;margin-top:-6px}
.crel .ticks{display:flex;align-items:flex-end;justify-content:space-between;height:22px}
.crel .ticks i{display:block;width:2px;background:var(--ink4);opacity:.8}
.crel .ticks i.pin{background:var(--mint);opacity:1}.crel .ticks i.read{background:var(--ink0);opacity:1}
.crel .ticks i.yours{background:var(--mint);opacity:.6}
.crel .ccap{display:flex;justify-content:space-between;align-items:baseline;font:400 12.5px var(--ui);color:var(--ink3)}
.crel .ccap .mono{font:400 11px var(--mono);color:var(--ink4)}.crel .ccap b{color:var(--mint);font-weight:600}
.shere{display:flex;align-items:flex-start;gap:22px;margin-bottom:-8px}
.shere .sh{font:400 13px var(--ui);color:var(--ink3);padding-top:1px;white-space:nowrap}
.shere .strip{display:flex;align-items:flex-start;flex-wrap:wrap;row-gap:12px;min-width:0}
.shere .ts{display:flex;align-items:flex-start;gap:9px;cursor:pointer}
.shere .ts .gem{flex:none;margin-top:-1px}
.shere .ts .tx{display:flex;flex-direction:column;gap:2px}
.shere .ts b{font:500 13px var(--mono);color:var(--ink1)}
.shere .ts.on b{color:var(--ink0);box-shadow:inset 0 -1.5px 0 var(--peri)}
.shere .ts em{font:italic 400 13px var(--serif,"Newsreader"),serif;color:var(--ink3)}
.shere .tl{flex:none;width:34px;height:1px;margin:9px 12px 0;background:var(--peri-line,rgba(147,162,250,.35))}
.shere .fly{margin-left:auto;font:400 12px var(--ui);color:var(--ink4);white-space:nowrap;padding-top:1px}
.shere kbd{font:500 10.5px var(--mono);color:var(--ink3);box-shadow:inset 0 0 0 1px var(--line2);padding:0 5px;margin-right:6px}
.shere.col{flex-direction:column;gap:10px}
.shere.col .shh{display:flex;align-items:baseline;width:100%}
.shere .rows{display:flex;flex-direction:column;gap:9px;width:100%}
.shere .tr{display:grid;grid-template-columns:18px auto auto minmax(0,1fr);align-items:baseline;column-gap:10px}
.shere .tr .gem{align-self:center}
.shere .tr b{font:500 13px var(--mono);color:var(--ink1)}
.shere .tr em{font:italic 400 13px var(--serif,"Newsreader"),serif;color:var(--ink3)}
.shere .tr span{font:400 12px var(--ui);color:var(--ink4);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.terr{position:relative}
.terr .reg{position:absolute;background:rgba(255,255,255,.018);box-shadow:inset 0 0 0 1px var(--line1);padding:8px 8px;overflow:hidden}
.terr .reg.hov{box-shadow:inset 0 0 0 1px var(--line3)}
.terr .rl{display:block;font:500 11.5px var(--mono);color:var(--ink2);margin-bottom:6px}
.terr .st{display:grid;gap:5.5px}
.terr .st i{display:block;width:10px;height:10px;background:rgba(158,176,255,.10);transform:rotate(0)}
.terr .st i.on{background:var(--mint);opacity:.85}
"""


def build(name, w, h):
    shelf = calm.kspine(("module",) * 6, 0) if w < 1100 else calm.shelf(book=("package", "serde", "1.0.193"), rows=SHELF_ROWS)
    tb = calm.titlebar(here_kind="package", here="serde", path="crates.io · 1.0.193", beads=3 if w >= 1100 else 1)
    reader_w = w - (42 if w < 1100 else 264)
    body = calm.window(w, h, tb, shelf, folio(reader_w), "nudox://crates.io/serde@1.0.193")
    return calm.page(name, w, h, body, css=CSS)


if __name__ == "__main__":
    print(build("PackagePage", 1440, 1180))
    print(build("PackagePage-900", 900, 1180))
