"""The v4 symbol page: one markup, every width, density and text scale."""
from nx4 import (tpl, gem, kind, caps, cap, mod, compass, compass_row, comb, fcomb, ico, chev, kbd,
                 keys, esc, page, cursor, lang, cbar)

# ---------------------------------------------------------------- chrome

def titlebar():
    return tpl("titlebar")


def shelf():
    return tpl("shelf")


def kspine():
    marks = ["module", "module", "module", "enum", "enum", "struct", "struct", "function", "constant"]
    out = ['<nav class="kspine" aria-label="Shelf spine">']
    for i, m in enumerate(marks):
        on = " on" if i == 3 else ""
        out.append(f'<span class="sp{on}">{kind(m, "sm", tip=False)}</span>')
    out.append('<span style="flex:1"></span>' + f'<span class="sp">{ico("side-l", "s14")}</span></nav>')
    return "".join(out)


def status(extra=""):
    return ('<footer class="status"><span class="it mono">nudox://present/glyph/RelationLabel</span><span class="grow"></span>'
            f'{extra}<span class="it">{keys("⌥")} x-ray</span><span class="it">hold {keys("⌘")} for keys</span></footer>')


# ---------------------------------------------------------------- page parts

def hero():
    facts = (
        '<div class="facts">'
        f'<span class="fx">{kind("enum", "sm")}<span>enum in</span><span class="mono">present::glyph</span></span>'
        '<i class="sep"></i>'
        f'<span class="fx hov" data-tip="unchanged for 9 releases">{ico("history", "s12")}<span>since</span><span class="mono">0.3.0</span></span>'
        '<i class="sep"></i>'
        f'<span class="fx">{ico("file", "s12")}<span class="mono">glyph.rs:138</span></span>'
        '<i class="sep"></i>'
        f'<span class="fx">{caps(["Clone", "Copy", "Debug", "Eq", "Hash"])}<span style="width:6px"></span>{cap("Display", "via")}</span>'
        '<i class="sep"></i>'
        f'<span class="fx" data-tip="is 3 · made of 3 · from 9 · to 4">{compass(3, 3, 9, 4, lg=True, tip=False)}</span>'
        '</div>')
    main = (
        '<div class="col" style="gap:calc(12px*var(--dsp));min-width:0">'
        '<div class="row" style="gap:calc(18px*var(--dsp));align-items:center">'
        f'{gem("enum", 72, "glint")}'
        '<div class="col" style="gap:6px;min-width:0"><span class="hero-name">RelationLabel</span>'
        '<span class="lede">The readable label of one relation group.</span></div></div>'
        f'{facts}</div>')
    files = [(5, "hot", [9, 12, 8, 10, 7]), (2, "", [8, 11]), (1, "", [9]), (1, "them", [7])]
    ticks = []
    for i in range(24):
        v = f"0.{i // 8}.{i % 8}"
        if i in (3, 11, 17):
            ticks.append((12 if i != 17 else 15, "maj mint" if i == 17 else "maj peri",
                          f"<b>{v}</b>{'changed its variants' if i != 17 else 'you pin this'}"))
        else:
            ticks.append((5 + (i * 7) % 4, "", ""))
    margin = (
        '<aside class="mg ctxrail">'
        '<div class="ctx-b"><div class="ctx-h"><b>In your code</b><span>9 uses · 4 files</span></div>'
        + fcomb(files)
        + '<div class="ctx-f"><span><b>render.rs</b> 5</span><span><b>page.rs</b> 2</span><span><b>outline.rs</b> 1</span></div></div>'
        '<div class="ctx-b"><div class="ctx-h"><b>History</b><span>changed in 3 of 24</span></div>'
        + comb(ticks, 22)
        + '<div class="ctx-f"><span><b>0.1.3</b> +Neighbourhood</span><span><b>0.2.1</b> you pin</span></div></div>'
        '</aside>')
    return f'<div class="leaf">{main}{margin}</div>'


def lensbar():
    tabs = [("book", "Reference", "", True), ("rose", "Relations", "19", False), ("target", "Usage", "9", False),
            ("history", "History", "3", False), ("peel", "Source", "", False)]
    out = ['<nav class="lensbar">']
    for icon, name, n, on in tabs:
        sup = f"<sup>{n}</sup>" if n else ""
        out.append(f'<button class="{"on" if on else ""}">{ico(icon, "s14")}<span class="w">{name}</span>{sup}</button>')
    out.append('<span class="grow"></span>'
               f'<button data-tip="Density: comfortable">{ico("list", "s14")}</button>'
               f'<button data-tip="Pin this page beside">{ico("pin", "s14")}</button>'
               f'<button data-tip="Copy a link">{ico("link", "s14")}</button></nav>')
    return "".join(out)


def rose(hover_via=True, W=480):
    H, cx, cy = 318, W // 2, 168
    L, R = 72, W - 64
    spread = min(130, W // 4)
    nods = {
        "u": [("Display", "", cx - spread, 44, "trait"), ("8 derived", "quiet", cx + 8, 32, "trait"),
              ("ToString", "via", cx + spread, 44, "trait")],
        "d": [("Typed", "", cx - spread, 290, "variant"), ("Neighbourhood", "", cx + 4, 298, "variant"),
              ("Related", "", cx + spread + 10, 290, "variant")],
        "l": [("relation_label()", "", L, 118, "function"), ("From<LinkKind>", "", L - 6, 168, "trait"),
              ("Page::relations", "", L + 4, 218, "method")],
        "r": [("group_head", "", R, 118, "function"), ("typed_heading", "", R + 6, 168, "function"),
              ("as_str", "", R - 14, 218, "method")],
    }
    colors = {"u": "var(--f-con)", "d": "var(--f-type)", "l": "var(--ink3)", "r": "var(--f-call)"}
    paths = []
    for d, items in nods.items():
        for name, cls, x, y, _ in items:
            if d == "u":
                p = f"M{cx} {cy-26} C {cx} {cy-64}, {x} {y+46}, {x} {y+12}"
            elif d == "d":
                p = f"M{cx} {cy+26} C {cx} {cy+64}, {x} {y-46}, {x} {y-12}"
            elif d == "l":
                p = f"M{cx-26} {cy} C {cx-70} {cy}, {x+100} {y}, {x+58} {y}"
            else:
                p = f"M{cx+26} {cy} C {cx+70} {cy}, {x-100} {y}, {x-44} {y}"
            dash = ' stroke-dasharray="2 4"' if cls == "via" else ""
            op = ".5" if cls == "quiet" else ".75"
            hot = cls == "via" and hover_via
            paths.append(f'<path d="{p}" fill="none" stroke="{colors[d]}" stroke-width="{1.8 if hot else 1.3}" opacity="{1 if hot else op}"{dash}></path>')
    out = [f'<div class="rose4"><div style="position:relative;width:{W}px;height:{H}px;max-width:100%">',
           f'<svg style="position:absolute;inset:0;overflow:visible" width="{W}" height="{H}" viewBox="0 0 {W} {H}">{"".join(paths)}</svg>']
    for lab, x, y in [("is", cx + 14, cy - 58), ("made of", cx + 30, cy + 58), ("from", cx - 70, cy - 12), ("to", cx + 62, cy - 12)]:
        out.append(f'<span class="axis" style="left:{x}px;top:{y}px">{lab}</span>')
    out.append(f'<span class="hubring" style="left:{cx}px;top:{cy}px"></span><span class="hub" style="left:{cx}px;top:{cy}px">{gem("enum", 48)}</span>')
    for d, items in nods.items():
        for name, cls, x, y, k in items:
            hov = " is-hover" if (cls == "via" and hover_via) else ""
            mark = kind(k, "sm", tip=False) if cls != "quiet" else ""
            out.append(f'<span class="nodule {cls}{hov}" style="left:{x}px;top:{y}px">{mark}{esc(name)}</span>')
    if hover_via:
        out.append(f'<div class="explain" style="left:{cx - 40}px;top:-6px">'
                   f'{ico("spark", "s12")}arrives via <code>impl&lt;T: Display&gt; ToString for T</code></div>')
    out.append("</div></div>")
    out.append('<div class="roselist">')
    for d, word in [("u", "is"), ("d", "made of"), ("l", "from"), ("r", "to")]:
        items = "".join(f'<span class="nodule {c}" style="position:static;transform:none">{esc(n)}</span>' for n, c, *_ in nods[d])
        out.append(f'<div class="grp"><span class="gh">{word}</span><div class="gi">{items}</div></div>')
    out.append("</div>")
    return "".join(out)


def signature(explain=True):
    code = (
        '<span class="at">#[derive(</span><a class="co">Clone</a><span class="p">, </span>'
        '<a class="co" style="box-shadow:inset 0 -1.5px 0 var(--peri);background:var(--peri-soft)">Copy</a><span class="p">, </span>'
        '<a class="co">Debug</a><span class="p">, </span><a class="co">Eq</a><span class="p">, </span><a class="co">Hash</a><span class="at">)]</span><br>'
        '<span class="kw">pub enum</span> <span class="ty">RelationLabel</span> <span class="p">{</span><br>'
        '&nbsp;&nbsp;&nbsp;&nbsp;<span class="va">Typed</span><span class="p">(</span><a class="ty">SemanticLinkKind</a><span class="p">, </span>'
        '<a class="ty">RelationDirection</a><span class="p">),</span><br>'
        '&nbsp;&nbsp;&nbsp;&nbsp;<span class="va">Neighbourhood</span><span class="p">,</span><br>'
        '&nbsp;&nbsp;&nbsp;&nbsp;<span class="va">Related</span><span class="p">,</span><br>'
        '<span class="p">}</span>')
    chip = ('<div class="explain" style="left:150px;top:-26px">' + cap("Copy", "on", tip=False)
            + '<span><code>Copy</code> — passing a RelationLabel never moves it</span></div>') if explain else ""
    return f'<div style="position:relative"><div class="sigx">{code}</div>{chip}</div>'


def ledger_made_of(hover=True):
    rows = [
        ("Typed", '<span class="p">(</span><a>SemanticLinkKind</a><span class="p">, </span><a>RelationDirection</a><span class="p">)</span>',
         "A relation whose compiler kind and direction are both known.", (0, 2, 14, 1), True),
        ("Neighbourhood", "", "A bounded neighbourhood whose per-edge kind the reply did not carry.", (0, 0, 3, 0), False),
        ("Related", "", "Related, and nothing more is known.", (0, 0, 2, 0), False),
    ]
    out = ['<div class="ledger-h"><span class="sec-h">Made of</span><span class="cnt">three variants, one carries data</span></div><div class="ledger">']
    for name, tail, say, cp, hot in rows:
        h = " hover" if (hot and hover) else ""
        out.append(f'<div class="lr{h}">{kind("variant", "sm", tip=False)}<span class="nm">{name}{tail}</span>'
                   f'<span class="say">{say}</span><span class="rt">{compass(*cp)}<span class="uses">{cp[2] or ""}</span></span></div>')
    out.append("</div>")
    return "".join(out)


def ledger_does():
    groups = [
        ("reads", "reads self", [("as_str", "(self) -> &'static str", "The words this group prints.", (0, 0, 9, 2)),
                                 ("is_typed", "(&self) -> bool", "Whether the compiler proved the kind.", (0, 0, 4, 0))]),
        ("changes", "changes self", [("flip", "(&mut self)", "Turns incoming into outgoing, in place.", (0, 0, 2, 1))]),
        ("consumes", "consumes self", [("into_heading", "(self) -> Heading", "Spends the label to build the heading it names.", (0, 0, 3, 2))]),
        ("makes", "makes one", [("from_kind", "(kind: LinkKind) -> Self", "The label for a raw link kind.", (0, 0, 5, 1))]),
    ]
    out = ['<div class="ledger-h"><span class="sec-h">Does</span><span class="cnt">five methods, grouped by what they do to it</span></div><div class="ledger">']
    for m, word, rows in groups:
        out.append(f'<div class="recv">{mod(m)}<span>{word}</span></div>')
        for name, sig, say, cp in rows:
            out.append(f'<div class="lr">{kind("method", "sm", tip=False)}<span class="nm"><a class="ca">{name}</a><span class="p">{esc(sig)}</span></span>'
                       f'<span class="say">{say}</span><span class="rt">{compass(*cp)}<span class="uses">{cp[2] or ""}</span></span></div>')
    out.append("</div>")
    return "".join(out)


def strips():
    files = [(5, "hot", [9, 12, 8, 10, 7]), (2, "", [8, 11]), (1, "", [9]), (1, "them", [7])]
    use = ('<div class="sp cut sm"><div class="h"><b>In your code</b><span class="q">9 uses in 4 files</span></div>'
           + fcomb(files)
           + '<div class="files"><span><b>render.rs</b> 5</span><span><b>page.rs</b> 2</span><span><b>outline.rs</b> 1</span><span>tests/labels.rs 1</span></div></div>')
    ticks = []
    for i in range(24):
        v = f"0.{i // 8}.{i % 8}"
        if i in (3, 11, 17):
            ticks.append((13 if i != 17 else 16, "maj mint" if i == 17 else "maj peri", f"<b>{v}</b>{'changed its variants' if i != 17 else 'you pin this'}"))
        else:
            ticks.append((6 + (i * 7) % 5, "", ""))
    hist = ('<div class="sp cut sm"><div class="h"><b>History</b><span class="q">changed in 3 of 24 releases</span></div>'
            + comb(ticks, 26) + '<div class="files"><span><b>0.0.3</b> added</span><span><b>0.1.3</b> +Neighbourhood</span><span><b>0.2.1</b> pinned</span></div></div>')
    return f'<div class="strip">{use}{hist}</div>'


def pins():
    def card(k, name, where, say, cp):
        return (f'<div class="peek pinrow cut sm" style="clip-path:none;width:auto"><div class="ph" style="padding:10px 12px 6px">{gem(k, 24)}'
                f'<div class="col" style="gap:1px;min-width:0"><span class="nm" style="font-size:13px">{name}</span>'
                f'<span class="wh"><code>{where}</code></span></div><span class="acts">{ico("pin", "s12")}</span></div>'
                f'<div class="say" style="padding:0 12px 8px;font-size:13px;line-height:18px">{say}</div>'
                f'<div style="padding:0 12px 10px">{compass_row(*cp)}</div></div>')
    return ('<aside class="pins"><div class="pins-h">' + ico("pin", "s12") + 'Pinned<span style="flex:1"></span>'
            + keys("⌘", "P") + '</div>'
            + card("enum", "SemanticLinkKind", "present::relation", "The closed vocabulary of how one symbol touches another.", (5, 11, 26, 0))
            + card("method", "Page::relations", "present::page", "Every relation group of a page, in rose order.", (0, 0, 4, 6))
            + '</aside>')


def folio(hover=True):
    return ('<div class="folio"><div class="folio-in">' + hero() + lensbar()
            + f'<div class="leaf"><div class="col" style="gap:calc(18px*var(--dsp));min-width:0">{signature(hover)}{rose(hover, 700)}</div>'
            + '<aside class="mg"><b>The rose.</b> Up is what it <b>is</b>, down what it is <b>made of</b>, left where it comes '
            + '<b>from</b>, right where it <b>goes</b>. A dashed strand arrives through a blanket impl; touch it and it says which.</aside></div>'
            + f'<div class="leaf"><div class="col" style="gap:calc(26px*var(--dsp));min-width:0">{ledger_made_of(hover)}{ledger_does()}</div>'
            + '<aside class="mg"><b>Grouped by what they do to it.</b> Reads, changes, consumes, makes: the receiver is the '
            + 'first thing a caller needs, so it is the heading, not a label on every row. The compass on each row is that '
            + 'member\'s own relations; the number is how often your code uses it.</aside></div>'
            + "</div></div>")


def window(width, height, density="", hover=True, zoom=1.0):
    ground = tpl("ground")
    style = f"width:{width / zoom}px;height:{height / zoom}px"
    if zoom != 1.0:
        style += f";zoom:{zoom}"
    body = (f'<div class="win" style="{style};position:relative;display:flex;flex-direction:column">{ground}{titlebar()}'
            f'<div class="body">{shelf()}{kspine()}<div class="split" role="separator"><i></i></div>'
            f'<main class="reader"><div class="pagegrid"><div class="reader-scroll">{folio(hover)}</div>{pins()}</div></main></div>'
            f'{status()}</div>')
    return body


def build(name, width, height, density="", hover=True, zoom=1.0):
    return page(name, width, height, window(width, height, density, hover, zoom), cls=density,
                css=".v4 .reader-scroll{overflow:hidden}")


if __name__ == "__main__":
    print(build("SymbolPage", 1440, 1760))
    for w, h in [(2560, 1440), (1440, 900), (1100, 900), (760, 900), (480, 900)]:
        print(build(f"flow-{w}", w, h))
    print(build("flow-1440-200pct", 1440, 900, zoom=2.0))
    for d in ["compact", "dense"]:
        print(build(f"density-{d}", 1440, 1100, density=d, hover=False))
    print(build("density-comfortable", 1440, 1100, hover=False))
