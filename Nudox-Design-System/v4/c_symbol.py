"""Calm symbol page: the hero, the signature, the rose, what it is made of and what it does."""
from nx4 import gem, kind, mod, compass, esc, ico
import calm

SHELF_ROWS = [("module", "identity"), ("module", "page"), ("module", "glyph", 0, "open"),
              ("enum", "RelationLabel", 1, "cur"), ("enum", "RelationDirection", 1), ("struct", "KindGlyph", 1),
              ("trait", "GlyphSet", 1, "dim"), ("function", "relation_label", 1), ("constant", "MAX_GLYPHS", 1, "dim"),
              ("module", "fault"), ("module", "outline")]


def hero():
    return ('<div class="chero">' + gem("enum", 64)
            + '<div class="col" style="min-width:0"><span class="nm">RelationLabel</span>'
            '<span class="ld">The readable label of one relation group.</span></div></div>'
            + calm.facts('enum in <span class="mono">present::glyph</span>', 'since <span class="mono">0.3.0</span>',
                         '<b>9</b> uses in your code'))


def signature():
    return ('<div class="ccode"><span class="at">#[derive(Clone, Copy, Debug, Eq, Hash)]</span>\n'
            '<span class="kw">pub enum</span> <span class="ty">RelationLabel</span> <span class="p">{</span>\n'
            '    <span class="va">Typed</span><span class="p">(</span><span class="ty">SemanticLinkKind</span><span class="p">, </span>'
            '<span class="ty">RelationDirection</span><span class="p">),</span>\n'
            '    <span class="va">Neighbourhood</span><span class="p">,</span>\n'
            '    <span class="va">Related</span><span class="p">,</span>\n<span class="p">}</span></div>')


def rose(W=760, hover=None):
    """Monochrome at rest: strands in ink4, names in ink2; the hovered direction lights in its hue."""
    H, cx, cy = 300, W // 2, 150
    spread = min(150, W // 4)
    L, R = 70, W - 70
    nods = {
        "u": [("trait", "Display", cx - spread, 34, ""), ("trait", "ToString", cx + spread, 34, "via")],
        "d": [("variant", "Typed", cx - spread, 272, ""), ("variant", "Neighbourhood", cx, 284, ""),
              ("variant", "Related", cx + spread, 272, "")],
        "l": [("function", "relation_label", L, 104, ""), ("trait", "From<LinkKind>", L - 4, 150, ""),
              ("method", "Page::relations", L + 6, 196, "")],
        "r": [("function", "group_head", R, 104, ""), ("function", "typed_heading", R + 4, 150, ""),
              ("method", "as_str", R - 10, 196, "")],
    }
    hue = {"u": "var(--f-con)", "d": "var(--f-type)", "l": "var(--ink2)", "r": "var(--f-call)"}
    paths = []
    for d, items in nods.items():
        lit = d == hover
        for _, name, x, y, cls in items:
            if d == "u":
                p = f"M{cx} {cy-24} C {cx} {cy-70}, {x} {y+50}, {x} {y+12}"
            elif d == "d":
                p = f"M{cx} {cy+24} C {cx} {cy+70}, {x} {y-50}, {x} {y-12}"
            elif d == "l":
                p = f"M{cx-24} {cy} C {cx-80} {cy}, {x+110} {y}, {x+64} {y}"
            else:
                p = f"M{cx+24} {cy} C {cx+80} {cy}, {x-110} {y}, {x-64} {y}"
            dash = ' stroke-dasharray="2 4"' if cls == "via" else ""
            col = hue[d] if lit else "var(--ink4)"
            paths.append(f'<path d="{p}" fill="none" stroke="{col}" stroke-width="{1.4 if lit else 1}" opacity="{1 if lit else .7}"{dash}/>')
    out = [f'<div class="crose" style="height:{H}px"><div style="position:relative;width:{W}px;height:{H}px;margin:0 auto">'
           f'<svg style="position:absolute;inset:0;overflow:visible" width="{W}" height="{H}">{"".join(paths)}</svg>'
           f'<span class="hub" style="left:{cx}px;top:{cy}px">{gem("enum", 40)}</span>']
    for d, items in nods.items():
        for k, name, x, y, cls in items:
            lit = " lit" if d == hover else ""
            out.append(f'<span class="nod{lit} {d}" style="left:{x}px;top:{y}px">{kind(k, "sm", tip=False)}{esc(name)}</span>')
    if hover:
        word = {"u": "is", "d": "made of", "l": "from", "r": "to"}[hover]
        out.append(f'<span class="axis-w" style="left:{cx + (90 if hover == "r" else -90 if hover == "l" else 0)}px;'
                   f'top:{cy + (-60 if hover == "u" else 60 if hover == "d" else -22)}px">{word}</span>')
    out.append('</div></div>')
    groups = [("is", nods["u"]), ("made of", nods["d"]), ("from", nods["l"]), ("to", nods["r"])]
    out.append('<div class="crlist">' + "".join(
        f'<div class="g"><span class="w">{w}</span><span class="ns">' + ", ".join(esc(n) for _, n, *_ in items) + '</span></div>'
        for w, items in groups) + '</div>')
    return "".join(out)


def made_of(hover=True):
    rows = [("Typed", '<span class="p">(</span><span class="t">SemanticLinkKind, RelationDirection</span><span class="p">)</span>',
             "A relation whose compiler kind and direction are both known.", (0, 2, 14, 1), 14),
            ("Neighbourhood", "", "A bounded neighbourhood whose per-edge kind the reply did not carry.", (0, 0, 3, 0), 3),
            ("Related", "", "Related, and nothing more is known.", (0, 0, 2, 0), 2)]
    out = ['<section class="csec"><h2>Made of</h2>']
    for i, (n, tail, say, cp, uses) in enumerate(rows):
        h = " hover" if hover and i == 0 else ""
        out.append(f'<div class="crow2{h}">{kind("variant", "sm", tip=False)}<span class="nm">{n}{tail}</span>'
                   f'<span class="say">{say}</span><span class="rt">{compass(*cp, tip=False)}{uses}</span></div>')
    out.append('</section>')
    return "".join(out)


def does():
    groups = [("reads", "reads", [("as_str", "(self) → &'static str", "The words this group prints.")]),
              ("reads", None, [("is_typed", "(&self) → bool", "Whether the compiler proved the kind.")]),
              ("changes", "changes", [("flip", "(&mut self)", "Turns incoming into outgoing, in place.")]),
              ("consumes", "consumes", [("into_heading", "(self) → Heading", "Spends the label to build the heading it names.")]),
              ("makes", "makes", [("from_kind", "(kind: LinkKind) → Self", "The label for a raw link kind.")])]
    out = ['<section class="csec"><h2>Does</h2>']
    for m, word, rows in groups:
        if word:
            out.append(f'<div class="cgrp">{mod(m, tip=False)}{word}</div>')
        for n, sig, say in rows:
            out.append(f'<div class="crow2">{kind("method", "sm", tip=False)}<span class="nm">{n}<span class="p">{esc(sig)}</span></span>'
                       f'<span class="say">{say}</span><span class="rt"></span></div>')
    out.append('</section>')
    return "".join(out)


def folio(width=1440, hover=True, rose_hover=None):
    rw = max(300, min(760, width - 340))
    return ('<div class="cfol">' + hero() + calm.tabs(["Reference", "Relations", "Usage", "History", "Source"])
            + signature() + rose(rw, rose_hover) + made_of(hover) + does() + '</div>')


CSS = """
.crose{position:relative;overflow:visible}
.crose .hub{position:absolute;transform:translate(-50%,-50%)}
.crose .nod{position:absolute;transform:translate(-50%,-50%);display:flex;align-items:center;gap:6px;white-space:nowrap;
  font:500 12px var(--mono);color:var(--ink2)}
.crose .nod .k{opacity:.7}
.crose .nod.lit{color:var(--ink0)}.crose .nod.lit .k{opacity:1}
.crose .axis-w{position:absolute;transform:translate(-50%,-50%);font:italic 400 12.5px var(--serif);color:var(--ink3)}
.crlist{display:none;flex-direction:column;gap:8px}
.crlist .g{display:grid;grid-template-columns:64px minmax(0,1fr);gap:10px;align-items:baseline}
.crlist .w{font:italic 400 13.5px var(--serif);color:var(--ink3)}
.crlist .ns{font:500 12.5px/1.6 var(--mono);color:var(--ink1)}
@container reader (max-width:560px){ .crose{display:none} .crlist{display:flex} }
"""


def build(name, w, h, cls="", zoom=1.0, hover=True, rose_hover=None):
    vw, vh = w / zoom, h / zoom
    narrow_shelf = vw < 900
    shelf = "" if vw < 640 else (calm.kspine() if narrow_shelf else calm.shelf(rows=SHELF_ROWS))
    tb = calm.titlebar(beads=3 if vw >= 1100 else 1)
    body = calm.window(int(vw), int(vh), tb, shelf, folio(int(vw) - (0 if vw < 640 else 42 if narrow_shelf else 264), hover, rose_hover),
                       "nudox://present/glyph/RelationLabel")
    if zoom != 1.0:
        body = f'<div style="zoom:{zoom}">{body}</div>'
    return calm.page(name, w, h, body, css=CSS, cls=cls)


if __name__ == "__main__":
    print(build("SymbolPage", 1440, 1640))
    print(build("SymbolPage-rose", 1440, 1000, rose_hover="r"))
    for w, h in [(2560, 1440), (1440, 900), (1100, 900), (760, 900), (480, 900)]:
        print(build(f"flow-{w}", w, h))
    print(build("flow-1440-200pct", 1440, 900, zoom=2.0))
    for d in ["comfortable", "compact", "dense"]:
        print(build(f"density-{d}", 1440, 1100, cls="" if d == "comfortable" else d, hover=False))
