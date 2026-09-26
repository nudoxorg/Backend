"""The v4 package page: the package as one territory map."""
from nx4 import (gem, kind, caps, cap, mod, compass, compass_row, cbar, comb, fcomb, mosaic, ico, chev,
                 kbd, keys, esc, page, cursor, lang, tpl)
from b_popups import release_lens, big_comb, module_lens, stone_peek


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
FAMS = ["ty", "co", "ca", "ca", "ca", "va"]


def territory(W=780, H=None, hover_mod="de", hover_stone=("ser", 14)):
    pitch = 16
    total = sum(v for _, v, _ in MODULES)
    if H is None:
        H = int(total * pitch * pitch * 1.32 / W) + 60
    rects = squarify([(n, v) for n, v, _ in MODULES], 0, 0, W, H)
    reach = {n: r for n, _, r in MODULES}
    out = [f'<div class="terr" style="position:relative;width:{W}px;height:{H}px">']
    for n, v, x, y, w, h in rects:
        pad = 6
        gx, gy, gw, gh = x + 3, y + 3, w - 6, h - 6
        private = n.startswith("__")
        hov = n == hover_mod
        out.append(f'<div class="treg{" hov" if hov else ""}{" priv" if private else ""}" style="left:{gx:.0f}px;top:{gy:.0f}px;width:{gw:.0f}px;height:{gh:.0f}px">')
        show = gw > 64 and gh > 44
        if show:
            out.append(f'<span class="tlab"><b>{esc(n)}</b><span>{v}</span></span>')
        cols = max(1, int((gw - pad * 2 + 3) // pitch))
        top = 24 if show else pad
        rows = max(1, int((gh - top - pad + 3) // pitch))
        cap_n = min(v, cols * rows)
        st = []
        lit = reach[n]
        for i in range(cap_n):
            f = FAMS[(i * 7 + len(n)) % len(FAMS)]
            cls = f
            if private:
                cls += " gate"
            elif i < lit:
                cls += " yours"
            elif (i * 13 + len(n)) % 5 == 0:
                cls += " y"
            if hover_stone and n == hover_stone[0] and i == hover_stone[1]:
                cls += " is-hover"
            st.append(cls)
        out.append(f'<div class="mosaic" style="position:absolute;left:{pad}px;right:{pad}px;top:{top}px;bottom:{pad}px">'
                   + "".join(f'<span class="st {c}"><i></i></span>' for c in st) + "</div>")
        if cap_n < v:
            out.append(f'<span class="tmore">+{v - cap_n}</span>')
        out.append("</div>")
    out.append("</div>")
    return "".join(out)


def hero():
    facts = (
        '<div class="facts">'
        f'<span class="fx">{lang("rust", 14)}<span>crates.io</span><span class="mono">1.0.193</span></span><i class="sep"></i>'
        '<span class="fx"><span>MIT or Apache-2.0</span></span><i class="sep"></i>'
        f'<span class="fx hov">{ico("users", "s12")}<span><b style="color:var(--ink1)">41,208</b> depend on it</span></span><i class="sep"></i>'
        f'<span class="fx">{ico("target", "s12", "color:var(--mint)")}<span><b style="color:var(--mint)">3</b> of your projects</span></span><i class="sep"></i>'
        '<span class="fx"><span class="famline"><i class="ty" style="flex:118"></i><i class="co" style="flex:64"></i><i class="ca" style="flex:190"></i><i class="va" style="flex:40"></i></span>'
        '<span class="mono">412</span><span>public</span></span>'
        '</div>')
    main = (
        '<div class="col" style="gap:calc(12px*var(--dsp));min-width:0">'
        '<div class="row" style="gap:calc(18px*var(--dsp));align-items:center">' + gem("package", 72) +
        '<div class="col" style="gap:6px;min-width:0"><span class="hero-name">serde</span>'
        '<span class="lede">A generic serialization and deserialization framework.</span></div></div>' + facts + '</div>')
    catch = (
        '<aside class="mg ctxrail"><div class="catch cut sm hot">'
        '<div class="col" style="gap:2px"><b class="ch">Since your pin</b><span class="cv">1.0.191 → 1.0.210</span></div>'
        '<div class="catch-n"><span><b>19</b> releases</span><span><b style="color:var(--mint)">2</b> touch your code</span>'
        '<span><b style="color:var(--coral)">0</b> breaking</span></div>'
        f'<div class="catch-i">{kind("function", "sm", tip=False)}<div class="col"><code>de::from_str</code><span>bound relaxed · 1.0.200</span></div></div>'
        f'<div class="catch-i">{kind("method", "sm", tip=False)}<div class="col"><code>Serializer::collect_str</code><span>faster · 1.0.204</span></div></div>'
        f'<div class="catch-k">{keys("↵")}review both{keys("U")}move pin</div></div></aside>')
    return f'<div class="leaf">{main}{catch}</div>'


def lensbar():
    tabs = [("mosaic", "Map", "", True), ("book", "Readme", "", False), ("layers", "Depends", "3", False),
            ("users", "Used by", "41k", False), ("history", "Changes", "64", False)]
    out = ['<nav class="lensbar">']
    for icon, name, n, on in tabs:
        sup = f"<sup>{n}</sup>" if n else ""
        out.append(f'<button class="{"on" if on else ""}">{ico(icon, "s14")}<span class="w">{name}</span>{sup}</button>')
    out.append('<span class="grow"></span>'
               f'<button data-tip="Only what you reach (⌥)">{ico("target", "s14")}</button>'
               f'<button data-tip="Density">{ico("list", "s14")}</button></nav>')
    return "".join(out)


def start_here():
    rows = [("trait", "Deserialize", "what your types become", "31k dependents enter here"),
            ("macro", "derive", "how they get there", "28k"),
            ("trait", "Deserializer", "only if you write a format", "1.2k")]
    out = ['<div class="starth">']
    for i, (k, n, why, n2) in enumerate(rows):
        out.append(f'<div class="sh cut sm" data-tip="{n2}">{gem(k, 26)}<div class="col" style="gap:2px;min-width:0"><span class="nm">{n}</span>'
                   f'<span class="why">{why}</span></div></div>')
        if i < len(rows) - 1:
            out.append(f'<span class="arrow">{chev("s14")}</span>')
    out.append("</div>")
    return "".join(out)


def features():
    fs = [("std", True, "default"), ("derive", True, "you enable it"), ("alloc", False, "implied by std"),
          ("rc", False, ""), ("unstable", False, "nightly")]
    out = ['<div class="feats"><span class="fl">Features</span>']
    for n, on, why in fs:
        out.append(f'<span class="ft{" on" if on else ""}" data-tip="{why}"><i></i>{n}</span>')
    out.append("</div>")
    return out[0] + "".join(out[1:])


def folio():
    ticks = big_comb(53)
    return ('<div class="folio"><div class="folio-in">' + hero()
            + '<div class="relrow" style="position:relative">' + ticks
            + '<div class="rel-l"><span>1.0.147</span><span><span style="color:var(--mint)">you pin 1.0.191</span> · '
              '<span style="color:var(--peri)">reading 1.0.193</span></span><span>1.0.210</span></div></div>'
            + lensbar()
            + '<div class="leaf"><div class="col" style="gap:calc(16px*var(--dsp));min-width:0">'
            + '<div class="row" style="gap:18px;align-items:center;flex-wrap:wrap">' + start_here() + '</div>'
            + features()
            + '<div style="position:relative">' + territory() + '</div>'
            + '<div class="legend4"><span><i class="ty"></i>types</span><span><i class="co"></i>contracts</span><span><i class="ca"></i>callables</span>'
              '<span><i class="va"></i>values</span><span><i class="yours"></i>you reach</span><span><i class="gate"></i>private or gated</span></div>'
            + '</div>'
            + '<aside class="mg"><b>One stone per public item.</b> Regions are modules, sized by what they hold; hue is family; mint '
              'is what your code reaches. Rest on a region for its breakdown, on a stone for its peek; hold ⌥ to dim everything '
              'that is not yours.</aside></div>'
            + '</div></div>')


CSS = """
.terr .treg{position:absolute;background:rgba(11,21,38,.55);box-shadow:inset 1px 1px 0 var(--bevel-hi),inset -1px -1px 0 var(--bevel-lo);
  clip-path:polygon(7px 0,100% 0,100% calc(100% - 7px),calc(100% - 7px) 100%,0 100%,0 7px)}
.terr .treg.hov{box-shadow:inset 2px 2px 0 var(--peri-hi),inset -2px -2px 0 var(--peri)}
.terr .treg.priv{opacity:.55}
.terr .tlab{position:absolute;left:8px;top:6px;right:8px;display:flex;gap:6px;align-items:baseline;font:400 11px/14px var(--mono);color:var(--ink3);white-space:nowrap;overflow:hidden}
.terr .tlab b{font-weight:600;color:var(--ink1)}
.terr .tmore{position:absolute;right:7px;bottom:5px;font:500 10px var(--mono);color:var(--ink3)}
.terr .mosaic{overflow:hidden;align-content:flex-start}
.terr .mosaic>.st{width:13px;height:13px}.terr .mosaic>.st>i{clip-path:polygon(3.5px 0,100% 0,100% calc(100% - 3.5px),calc(100% - 3.5px) 100%,0 100%,0 3.5px)}
.mosaic>.st.yours>i{opacity:1;background:var(--mint)}
.mosaic>.st.ty{color:var(--f-type)}.mosaic>.st.co{color:var(--f-con)}.mosaic>.st.ca{color:var(--f-call)}.mosaic>.st.va{color:var(--f-val)}
.mosaic>.st.gate>i{background:repeating-linear-gradient(90deg,currentColor 0 1.5px,transparent 1.5px 3.5px);opacity:.5}
.mosaic>.st.is-hover{transform:translateY(-3px) scale(1.55);z-index:5}
.legend4{display:flex;flex-wrap:wrap;gap:6px 16px;font:italic 400 12.5px/16px var(--serif);color:var(--ink3)}
.legend4 span{display:inline-flex;align-items:center;gap:6px}
.legend4 i{width:9px;height:9px;display:inline-block;clip-path:polygon(2.5px 0,100% 0,100% calc(100% - 2.5px),calc(100% - 2.5px) 100%,0 100%,0 2.5px)}
.legend4 i.ty{background:var(--f-type)}.legend4 i.co{background:var(--f-con)}.legend4 i.ca{background:var(--f-call)}.legend4 i.va{background:var(--f-val)}
.legend4 i.yours{background:var(--mint)}.legend4 i.gate{background:repeating-linear-gradient(90deg,var(--ink3) 0 1.5px,transparent 1.5px 3.5px)}
.famline{display:inline-flex;width:60px;height:6px;gap:1px}.famline i{display:block;height:100%}
.famline .ty{background:var(--f-type)}.famline .co{background:var(--f-con)}.famline .ca{background:var(--f-call)}.famline .va{background:var(--f-val)}
.relrow{padding-top:6px}
.rel-l{display:flex;justify-content:space-between;margin-top:8px;font:500 11px var(--mono);color:var(--ink3)}
.catch{padding:12px 13px;display:flex;flex-direction:column;gap:8px}
.catch-n{display:flex;gap:12px;flex-wrap:wrap;font:italic 400 12.5px/16px var(--serif);color:var(--ink3);font-style:normal}
.catch-n span{font-family:var(--serif);font-style:italic}.catch-n b{font:600 13px var(--mono);font-style:normal;color:var(--ink0)}
.catch-i{display:flex;align-items:center;gap:7px;font:400 12px/17px var(--mono);color:var(--ink1);font-style:normal}
.catch-i code{color:var(--ink0)}.catch-i{align-items:flex-start}.catch-i .col{gap:0;min-width:0}.catch-i code{white-space:nowrap;overflow:hidden;text-overflow:ellipsis}.catch-i span{font:italic 400 11.5px/15px var(--serif);color:var(--ink3)}
.catch .ch{font:600 13px/16px var(--ui);color:var(--ink0);font-style:normal}.catch .cv{font:500 11px var(--mono);color:var(--ink3);font-style:normal}
.catch-k{display:flex;align-items:center;gap:6px;font:400 11.5px var(--ui);color:var(--ink3);font-style:normal;margin-top:2px}
.starth{display:flex;align-items:center;gap:10px;flex-wrap:wrap}
.starth .sh{display:flex;align-items:center;gap:10px;padding:8px 12px 8px 9px}
.starth .nm{font:600 13px/16px var(--mono);color:var(--ink0)}
.starth .why{font:italic 400 12.5px/15px var(--serif);color:var(--ink3)}
.starth .n2{font:500 10.5px var(--mono);color:var(--ink3);margin-left:8px}
.starth .arrow{color:var(--ink4)}
.feats{display:flex;align-items:center;gap:14px;flex-wrap:wrap;font:400 12.5px var(--mono);color:var(--ink2)}
.feats .fl{font:italic 400 13px var(--serif);color:var(--ink3)}
.feats .ft{display:inline-flex;align-items:center;gap:6px}
.feats .ft i{width:9px;height:9px;transform:rotate(45deg);border:1.3px solid var(--ink3)}
.feats .ft.on{color:var(--ink0)}.feats .ft.on i{background:var(--mint);border-color:var(--mint)}
"""


def window(width, height):
    tb = tpl("titlebar").replace('<button class="on"><i class="d"></i>Page</button>', '<button class=""><i class="d"></i></button>') \
        .replace('<nav class="alt" aria-label="Depth"><button class=""><i class="d"></i></button><button class="">',
                 '<nav class="alt" aria-label="Depth"><button class=""><i class="d"></i></button><button class="on"><i class="d"></i>Package</button><button class="" style="display:none">')
    tb = tb.replace('<span class="nm">RelationLabel</span><span class="path">present › glyph</span>',
                    '<span class="nm">serde</span><span class="path">crates.io · 1.0.193</span>')
    from b_symbol import kspine, status
    return (f'<div class="win" style="width:{width}px;height:{height}px;position:relative;display:flex;flex-direction:column">{tpl("ground")}{tb}'
            f'<div class="body">{tpl("shelf")}{kspine()}<div class="split" role="separator"><i></i></div>'
            f'<main class="reader"><div class="pagegrid"><div class="reader-scroll">{folio()}</div></div></main></div>'
            f'{status()}</div>')


if __name__ == "__main__":
    print(page("PackagePage", 1440, 1180, window(1440, 1180), css=CSS + ".v4 .reader-scroll{overflow:hidden}"))
    print(page("PackagePage-900", 900, 1180, window(900, 1180), css=CSS + ".v4 .reader-scroll{overflow:hidden}"))
