"""The ladder of detail, x-ray, and density."""
from nx4 import (gem, kind, caps, cap, mod, compass, compass_row, cbar, comb, fcomb, mosaic, ico, chev,
                 kbd, keys, esc, page, lang)
from b_popups import peek_symbol, peek_package, release_lens


def rung_row(mark, tag, row, card, name, sub):
    return (f'<div class="cell"><div class="rn">{name}<small>{sub}</small></div></div>'
            f'<div class="cell">{mark}</div><div class="cell">{tag}</div><div class="cell">{row}</div>'
            f'<div class="cell" style="position:relative;min-height:{card[1]}px">{card[0]}</div>')


def board():
    sym = rung_row(
        gem("enum", 22),
        f'<span class="row g8" style="gap:8px">{gem("enum", 22)}<span style="font:500 13px var(--mono);color:var(--ink0)">SemanticLinkKind</span></span>',
        f'<div class="row1 cut sm">{gem("enum", 22)}<span class="nm">SemanticLinkKind</span>{mod("derived")}'
        f'<span class="sm">how one symbol touches another</span><span class="rt">{compass(5, 11, 26, 0)}'
        '<span style="font:500 11px var(--mono);color:var(--ink3)">14</span></span></div>',
        (peek_symbol(0, 0).replace('position:absolute;z-index:60', 'position:relative;z-index:1'), 420),
        "A symbol", "gem · name · row · peek")
    rel_ticks = [(5 + (i * 5) % 6, "", "") for i in range(22)] + [(13, "maj mint", "")] + [(6, "", "")] * 3
    pkg = rung_row(
        gem("package", 22),
        f'<span class="row" style="gap:8px">{lang("rust", 16)}<span style="font:500 13px var(--mono);color:var(--ink0)">serde</span>'
        '<span style="font:400 11.5px var(--mono);color:var(--ink3)">1.0.193</span></span>',
        f'<div class="row1 cut sm">{lang("rust", 16)}<span class="nm">serde</span><span style="font:400 11.5px var(--mono);color:var(--ink3)">1.0.193</span>'
        f'<span class="sm">serialization framework</span><span class="rt" style="width:120px">{comb(rel_ticks, 16)}</span></div>',
        (peek_package(0, 0).replace('position:absolute;z-index:60', 'position:relative;z-index:1'), 440),
        "A package", "logo · name · row · peek")
    rel = rung_row(
        '<span style="display:inline-block;width:2px;height:22px;background:var(--peri)"></span>',
        '<span class="row" style="gap:8px"><span style="display:inline-block;width:2px;height:18px;background:var(--peri)"></span>'
        '<span style="font:600 13px var(--mono);color:var(--ink0)">1.0.200</span></span>',
        '<div class="row1 cut sm"><span style="font:600 13px var(--mono);color:var(--ink0)">1.0.200</span>'
        '<span style="font:500 11.5px var(--mono);color:var(--mint)">+3</span><span style="font:500 11.5px var(--mono);color:var(--peri)">~5</span>'
        '<span class="sm">3 weeks ago · touches from_str</span>'
        '<span class="rt"><span style="width:80px;height:6px;display:flex;gap:2px"><i style="flex:2;background:var(--f-call)"></i><i style="flex:1;background:var(--f-type)"></i>'
        '<i style="flex:5;background:repeating-linear-gradient(135deg,var(--f-call) 0 2px,transparent 2px 4px)"></i></span></span></div>',
        (release_lens(0, 0).replace('position:absolute;z-index:60', 'position:relative;z-index:1'), 360),
        "A release", "tick · version · row · lens")
    head = ('<div class="rh"></div><div class="rh">Mark</div><div class="rh">Tag</div><div class="rh">Row</div><div class="rh">Card</div>')
    rules = (
        '<div class="rule-strip">'
        '<div><b>Width picks the rung</b><span>A list in the shelf shows tags; the same list in the reader shows rows; a comb in a '
        'margin shows marks. Nothing is ever cut off mid-word; it steps down a rung instead.</span></div>'
        '<div><b>Rest climbs one</b><span>Whatever rung something is drawn at, resting on it shows the next one up, in place or as '
        'a peek. A card is the top: it has nowhere further to climb but its page.</span></div>'
        '<div><b>Activate descends</b><span>Enter or a click goes to its page: Orbit, Package, Page, Source. The titlebar thread '
        'remembers the way back.</span><span class="k2">' + keys("↵") + keys("⌘", "[") + '</span></div>'
        '<div><b>⌥ climbs everything</b><span>Hold option and every visible thing rises one rung where there is room: marks '
        'spell their names, rows show their facts. Let go and it all settles back.</span><span class="k2">' + keys("⌥") + '</span></div>'
        '</div>')
    items = [("module", "de", "", ""), ("trait", "Deserialize", "abstract", "what your types become"),
             ("trait", "Deserializer", "", "only if you write a format"), ("trait", "Visitor", "abstract", "walks one value"),
             ("function", "from_str", "", "parse from a string"), ("struct", "IgnoredAny", "derived", "skips any value"),
             ("constant", "MAX_DEPTH", "const", "recursion limit")]
    def lst(xray):
        out = ['<div class="xr-list">']
        for k, n, m, say in items:
            mk = mod(m) if m else ""
            spell = f'<span class="spell"><b>{m}</b> · {say}</span>' if xray and (m or say) else ""
            out.append(f'<div class="li" style="{"background:var(--plate2)" if xray else ""}">{kind(k, "sm", tip=False)}'
                       f'<span class="mono" style="font-size:12.5px;color:var(--ink0)">{n}</span>{"" if xray else mk}{spell}'
                       f'<span class="meta">{compass(1 if k == "trait" else 0, 2, 4, 1) if k != "module" else "<span class=mono>214</span>"}</span></div>')
        out.append("</div>")
        return "".join(out)
    xr = ('<div class="xr-pair">'
          f'<div class="cut sm" style="padding:12px">{label("At rest")}{lst(False)}</div>'
          f'<div class="cut sm hot" style="padding:12px">{label("Holding ⌥")}{lst(True)}</div></div>')
    return ('<div class="doc" style="padding:48px 64px;display:flex;flex-direction:column;gap:26px">'
            '<div class="col" style="gap:8px"><span class="t-micro" style="color:var(--mint)">FACET v4 · 13</span>'
            '<span class="t-display-xl">The ladder of detail</span>'
            '<span class="lede" style="max-width:none">Every thing Nudox shows can be drawn at four rungs. The room it is given picks '
            'the rung; resting on it climbs one; activating it descends to its page. That one rule is how a small window stays '
            'complete and a big one stays calm.</span></div>'
            + rules
            + f'<div class="ladder">{head}{sym}{pkg}{rel}</div>'
            + '<div class="col" style="gap:10px"><span class="sec-h">X-ray</span>'
            + '<span class="lede" style="font-size:16px">Marks carry meaning quietly; holding ⌥ spells every one of them at once, '
            'so nothing is ever hidden behind a hover you did not know to make.</span></div>'
            + xr + '</div>')


def label(t):
    return f'<span class="t-micro" style="display:block;color:var(--ink3);margin-bottom:8px">{t}</span>'


if __name__ == "__main__":
    print(page("Ladder", 1440, 2000, board(), title="Ladder"))
