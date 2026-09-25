"""FACET v4 board builder: small helpers over the v3 CSS + v4.css.

Boards are plain HTML (no runtime) so headless Chrome renders them exactly;
fonts are the local TTFs, so captures are deterministic and offline.
"""
import html
import json
import math
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
ICONS = {}
for item in json.load(open(os.path.join(ROOT, "spec", "icons.json"))):
    ICONS[(item["group"], item["name"])] = item

FACES = """
@font-face{font-family:"Bricolage Grotesque";src:url("../fonts/BricolageGrotesque[opsz,wdth,wght].ttf") format("truetype");font-weight:200 800;font-stretch:75% 100%}
@font-face{font-family:"Geist";src:url("../fonts/Geist[wght].ttf") format("truetype");font-weight:100 900}
@font-face{font-family:"Geist Mono";src:url("../fonts/GeistMono[wght].ttf") format("truetype");font-weight:100 900}
@font-face{font-family:"Newsreader";src:url("../fonts/Newsreader[opsz,wght].ttf") format("truetype");font-weight:200 800;font-style:normal}
@font-face{font-family:"Newsreader";src:url("../fonts/Newsreader-Italic[opsz,wght].ttf") format("truetype");font-weight:200 800;font-style:italic}
"""

FAM = {
    "module": "ns", "package": "ns", "import": "ns", "unknown": "ns",
    "struct": "ty", "class": "ty", "enum": "ty", "union": "ty", "type": "ty",
    "trait": "co", "interface": "co",
    "function": "ca", "method": "ca", "constructor": "ca", "macro": "ca",
    "constant": "va", "field": "va", "property": "va", "variable": "va", "variant": "va",
}

MODS = {
    "const": "evaluated at compile time",
    "async": "async: returns a future",
    "unsafe": "unsafe: you uphold the invariants",
    "generic": "generic",
    "static": "static: one for the whole program",
    "crate": "visible inside this crate only",
    "deprecated": "deprecated since 0.4",
    "abstract": "abstract: you must provide it",
    "derived": "derived, no custom logic",
    "blanket": "arrives through a blanket impl",
    "auto": "auto trait: the compiler proves it",
    "override": "overrides the inherited one",
    "inherited": "inherited, unchanged",
    "makes": "no self: makes one or stands alone",
    "reads": "borrows self: reads only",
    "changes": "borrows self mutably: changes it",
    "consumes": "takes self: the value is gone after",
}

CAPS = {
    "Clone": "Clone", "Copy": "Copy", "Eq": "Eq", "Ord": "Ord", "Hash": "Hash",
    "Debug": "Debug", "Display": "Display", "convert": "From, Into, ToString",
    "thread": "Send, Sync", "Default": "Default", "serde": "Serialize",
    "iter": "IntoIterator", "Deref": "Deref", "Error": "Error",
}

LIT = [0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66]
FACETS = ["24,2 35,13 24,10", "24,10 35,13 38,24", "35,13 46,24 38,24", "46,24 35,35 38,24",
          "38,24 35,35 24,38", "35,35 24,46 24,38", "24,46 13,35 24,38", "24,38 13,35 10,24",
          "13,35 2,24 10,24", "2,24 13,13 10,24", "10,24 13,13 24,10", "13,13 24,2 24,10"]


def esc(text):
    return html.escape(str(text), quote=True)


def inner(svg):
    return svg[svg.index(">") + 1: svg.rindex("</svg>")]


def ico(name, cls="s14", style=""):
    svg = ICONS[("ui", name)]["svg"]
    st = f' style="{style}"' if style else ""
    return f'<svg class="ico {cls}" viewBox="0 0 24 24" aria-hidden="true"{st}>{inner(svg)}</svg>'


def chev(cls="s12", style=""):
    st = f' style="{style}"' if style else ""
    return f'<svg class="ico {cls}" viewBox="0 0 24 24" aria-hidden="true"{st}><path d="m9 6 6 6-6 6"></path></svg>'


def kind(name, size="sm", tip=True):
    svg = ICONS[("kind", name)]["svg"]
    t = f' data-tip="{esc(name)}"' if tip else ""
    return f'<span class="k {FAM[name]} {size}"{t}><svg viewBox="0 0 24 24" aria-label="{name}">{inner(svg)}</svg></span>'


def gem(kind_name, px=28, cls="", lit=None, extra=""):
    fam = FAM[kind_name]
    lit = LIT if lit is None else lit
    parts = []
    for i, (pts, t) in enumerate(zip(FACETS, lit)):
        parts.append(f'<polygon class="fc" style="--t:{t};--i:{i}" points="{pts}"></polygon>')
    glyph = inner(ICONS[("kind", kind_name)]["svg"])
    return (f'<svg class="gem {fam} {cls}" width="{px}" height="{px}" viewBox="0 0 48 48" aria-label="{kind_name}"{extra}>'
            + "".join(parts)
            + '<path d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor" stroke-width="1"></path>'
            + '<path class="tb" d="M24 10 38 24 24 38 10 24z" stroke="currentColor" stroke-width=".7" stroke-opacity=".55"></path>'
            + ('<g transform="translate(17.4 17.4) scale(.55)" fill="none" stroke="currentColor" stroke-width="2.4" '
               'stroke-linecap="square" stroke-linejoin="miter">' + glyph.replace('class="f"', 'class="f" fill="currentColor" stroke="none" opacity=".5"') + "</g>" if px >= 22 else "")
            + "</svg>")


def cap(short, state="on", tip=True):
    name = CAPS[short]
    svg = ICONS[("capability", name)]["svg"]
    t = f' data-tip="{esc(name)}"' if tip else ""
    return f'<span class="cap {state}"{t}><svg viewBox="0 0 24 24">{inner(svg)}</svg></span>'


def caps(names, via=()):
    return '<span class="caps">' + "".join(cap(n, "via" if n in via else "on") for n in names) + "</span>"


def mod(short, cls="", tip=True):
    key = MODS[short]
    svg = ICONS[("modifier", key)]["svg"]
    t = f' data-tip="{esc(key)}"' if tip else ""
    return f'<span class="mod {cls}"{t}><svg viewBox="0 0 24 24">{inner(svg)}</svg></span>'


LANG_COLOR = {"rust": "#f0a27a", "python": "#8fc4ff", "typescript": "#7fb0ff", "go": "#7fdcf0",
              "java": "#f0a27a", "csharp": "#b98cf0", "cpp": "#7fb0ff"}


def lang(name, px=14):
    item = ICONS[("language", name)]
    return (f'<svg class="logo-lang" viewBox="0 0 24 24" style="width:{px}px;height:{px}px;color:{LANG_COLOR.get(name, "#aab")}">'
            f'{inner(item["svg"])}</svg>')


def kbd(text, cls=""):
    return f'<kbd class="kbd {cls}">{esc(text)}</kbd>'


def keys(*caps_, cls=""):
    return '<span class="keys">' + "".join(kbd(c, cls) for c in caps_) + "</span>"


def arm(n, maximum=6.0):
    return 0 if n <= 0 else round(min(maximum, 1.6 + math.log2(1 + n) * 1.25), 2)


def compass(u, d, l, r, lg=False, tip=True):
    mx = 11.0 if lg else 6.0
    t = f' data-tip="is {u} · made of {d} · from {l} · to {r}"' if tip else ""
    return (f'<span class="compass{" lg" if lg else ""}"{t}><b></b>'
            f'<i class="u" style="--l:{arm(u, mx)}px"></i><i class="d" style="--l:{arm(d, mx)}px"></i>'
            f'<i class="l" style="--l:{arm(l, mx)}px"></i><i class="r" style="--l:{arm(r, mx)}px"></i></span>')


def compass_row(u, d, l, r):
    parts = []
    for v, word, col in ((u, "is", "f-con"), (d, "made of", "f-type"), (l, "from", "ink2"), (r, "to", "f-call")):
        if v:
            parts.append(f'<span class="n"><i class="dir" style="color:var(--{col})"></i>{word} <b>{v}</b></span>')
    return '<span class="compass-row">' + '<span class="dot">·</span>'.join(parts) + "</span>"


def cbar(u, d, l, r):
    mx = max(1, u, d, l, r)

    def cell(v, cls, lab, sub):
        z = " zero" if v == 0 else ""
        return (f'<div class="{cls}{z}"><span class="lab">{lab}</span><span class="num">{v}<small>{sub}</small></span>'
                f'<span class="bar"><i style="--w:{round(100 * v / mx)}%"></i></span></div>')
    return ('<div class="cbar">' + cell(u, "u", "is", "traits") + cell(d, "d", "made of", "variants")
            + cell(l, "l", "from", "uses") + cell(r, "r", "to", "calls") + "</div>")


def comb(ticks, h=40, cls="", style=""):
    out = [f'<div class="comb {cls}" style="--ch:{h}px;{style}">']
    for t in ticks:
        th, tcls, pop = t
        popx = f'<span class="pop">{pop}</span>' if pop else ""
        out.append(f'<span class="t {tcls}" style="--h:{th}px"><i></i>{popx}</span>')
    out.append("</div>")
    return "".join(out)


def mosaic(stones, cls="", style=""):
    return (f'<div class="mosaic {cls}" style="{style}">'
            + "".join(f'<span class="st {s}"><i></i></span>' for s in stones) + "</div>")


def fcomb(files):
    out = ['<span class="fcomb">']
    for count, cls, heights in files:
        out.append(f'<span class="f {cls}">' + "".join(f'<i style="--h:{h}px"></i>' for h in heights[:count]) + "</span>")
    out.append("</span>")
    return "".join(out)


def cursor(x, y):
    return (f'<svg class="cursor" style="left:{x}px;top:{y}px" viewBox="0 0 14 20"><path d="M1 1v15.5l4-3.6 2.6 5.8 2.4-1.1-2.6-5.7H13z" '
            'fill="#f5f7fb" stroke="#050b17" stroke-width="1.2" stroke-linejoin="round"/></svg>')


def label(text, sub=""):
    s = f'<span class="t-small dim" style="font:italic 400 13px/18px var(--serif);color:var(--ink3)">{sub}</span>' if sub else ""
    return f'<div class="col g2" style="gap:2px"><span class="t-micro" style="color:var(--mint)">{text}</span>{s}</div>'


def tpl(name):
    return open(os.path.join(HERE, f"tpl-{name}.html")).read()


def page(name, w, h, body, cls="", css="", title=""):
    doc = f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><title>{esc(title or name)}</title>
<style>{FACES}</style>
<link rel="stylesheet" href="facet.css"><link rel="stylesheet" href="main-extra.css"><link rel="stylesheet" href="v4.css">
<style>html,body{{margin:0;background:#030814}} {css}</style></head>
<body><div class="nx v4 {cls}" style="width:{w}px;min-height:{h}px;background:var(--g0);position:relative">{body}</div></body></html>"""
    path = os.path.join(HERE, f"{name}.html")
    open(path, "w").write(doc)
    return path
