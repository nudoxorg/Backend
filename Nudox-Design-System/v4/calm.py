"""FACET v4, calm: the shared window frame and CSS for the restrained boards.

gui-plan.md §6.2: one thing speaks; rows at rest are mark + name; one accent
(mint = yours/current, periwinkle = focus); space, not lines; serif only for
the lede and a card's one sentence; keys only while ⌘ is held; boards show the
product, not notes about it.
"""
import re

from nx4 import tpl, gem, kind, ico, chev, keys, esc, page as _page


def titlebar(here_kind="enum", here="RelationLabel", path="present › glyph", beads=3, ask=False):
    t = tpl("titlebar")
    t = re.sub(r'<nav class="alt".*?</nav>', "", t, flags=re.S)
    if ask:
        i, j = t.index('<div class="thread"'), t.index('<button class="ibtn" aria-label="Trail map"')
        field = (f'<div class="thread"><button class="here askf">{ico("search", "s14")}'
                 '<span class="ph">Ask anything, or find a package</span></button></div>')
        return t[:i] + field + t[j:]
    t = re.sub(r'<span class="k ty sm">.*?</svg></span>', kind(here_kind, "sm", tip=False), t, count=1, flags=re.S)
    t = t.replace('<span class="nm">RelationLabel</span><span class="path">present › glyph</span>',
                  f'<span class="nm">{esc(here)}</span><span class="path">{esc(path)}</span>')
    if beads < 3:
        t = re.sub(r'<button class="bead ns older".*?<span class="strand"></span>', "", t, count=1, flags=re.S)
    return t


def shelf(book=("package", "present", "0.4.2"), rows=(), up=("backend", "")):
    k, name, ver = book
    head = (f'<div class="cbook">{gem(k, 28)}<div class="col" style="gap:1px;min-width:0">'
            f'<span class="bn">{esc(name)}</span><span class="bv">{esc(ver)}</span></div></div>') if name else ""
    items = []
    for r in rows:
        kd, nm, depth, state = (list(r) + [0, ""])[:4]
        cls = f"crow {state}".strip()
        pad = f' style="padding-left:{12 + 16 * depth}px"' if depth else ""
        mark = kind(kd, "sm", tip=False) if kd in ("module",) or True else ""
        items.append(f'<div class="{cls}"{pad}>{mark}<span class="n">{esc(nm)}</span></div>')
    upl = (f'<div class="cup">{chev("s12", "transform:rotate(180deg)")}{esc(up[0])}</div>') if up[0] else ""
    return (f'<aside class="cshelf">{upl}{head}'
            f'<div class="cfilter">{ico("filter", "s12")}<span>Filter</span></div>'
            f'<div class="crows">{"".join(items)}</div></aside>')


def kspine(kinds=("module", "module", "enum", "enum", "struct", "function"), on=2):
    return ('<nav class="ckspine">' + "".join(
        f'<span class="{"on" if i == on else ""}">{kind(k, "sm", tip=False)}</span>' for i, k in enumerate(kinds)) + '</nav>')


def status(address):
    return f'<footer class="cstatus"><span class="mono">{esc(address)}</span></footer>'


def window(width, height, titlebar_html, shelf_html, reader_html, address, overlay="", pins=""):
    ground = tpl("ground")
    return (f'<div class="win calm" style="width:{width}px;height:{height}px">{ground}{titlebar_html}'
            f'<div class="cbody">{shelf_html}<main class="reader creader">{reader_html}</main>{pins}</div>'
            f'{status(address)}{overlay}</div>')


def page(name, w, h, body, css="", cls=""):
    return _page(name, w, h, body, cls=cls, css=CSS + css)


def facts(*parts):
    """One quiet line: parts are strings (already escaped html allowed)."""
    return '<div class="cfacts">' + '<i class="sep">·</i>'.join(f'<span>{p}</span>' for p in parts) + '</div>'


def tabs(names, on=0):
    return '<nav class="ctabs">' + "".join(
        f'<span class="{"on" if i == on else ""}">{esc(n)}</span>' for i, n in enumerate(names)) + '</nav>'


CSS = """
.win.calm{position:relative;display:flex;flex-direction:column;container:win / inline-size;overflow:hidden}
.calm .titlebar .chip,.calm .titlebar .av{display:none}
.calm .here.askf{min-width:min(460px,50cqi)}
.calm .here.askf .ph{font:400 13px var(--ui);color:var(--ink3)}
.cbody{flex:1;display:flex;min-height:0;position:relative}
.cshelf{flex:none;width:clamp(220px,18cqi,264px);display:flex;flex-direction:column;border-right:1px solid var(--line1);
  background:rgba(3,8,18,.55);padding:6px 0;min-height:0}
.cup{display:flex;align-items:center;gap:6px;height:30px;padding:0 14px;font:500 12px var(--ui);color:var(--ink3)}
.cbook{display:flex;align-items:center;gap:10px;padding:8px 14px 12px}
.cbook .bn{font:600 14px var(--ui);color:var(--ink0)}
.cbook .bv{font:400 11.5px var(--mono);color:var(--ink3)}
.cfilter{display:flex;align-items:center;gap:8px;margin:0 10px 8px;height:28px;padding:0 10px;font:400 12px var(--ui);color:var(--ink4);
  background:rgba(255,255,255,.02);box-shadow:inset 0 0 0 1px var(--line1)}
.crows{display:flex;flex-direction:column;overflow:hidden}
.crow{display:flex;align-items:center;gap:9px;height:calc(28px*var(--drow));padding:0 12px;font:500 calc(12.5px*var(--dtxt)) var(--mono);color:var(--ink2);position:relative;white-space:nowrap}
.crow .n{overflow:hidden;text-overflow:ellipsis}
.crow.dim{color:var(--ink3)}
.crow.cur{color:var(--ink0);background:rgba(126,242,197,.05)}
.crow.cur::before{content:"";position:absolute;left:0;top:5px;bottom:5px;width:2px;background:var(--mint)}
.crow.open{color:var(--ink1)}
.ckspine{flex:none;width:42px;display:flex;flex-direction:column;align-items:center;gap:6px;padding:12px 0;border-right:1px solid var(--line1);
  background:rgba(3,8,18,.55)}
.ckspine span{opacity:.55}.ckspine span.on{opacity:1}
.creader{flex:1;min-width:0;overflow:hidden;position:relative;container:reader / inline-size}
.cstatus{height:26px;flex:none;display:flex;align-items:center;padding:0 12px;border-top:1px solid var(--line1);font:400 11px var(--mono);color:var(--ink4)}
.cfol{max-width:880px;margin:0 auto;padding:clamp(22px,5cqi,64px) clamp(16px,4cqi,48px) 80px;display:flex;flex-direction:column;
  gap:calc(30px*var(--dsp))}
.chero{display:flex;align-items:center;gap:clamp(14px,2cqi,22px);min-width:0}
.chero .nm{font:700 clamp(30px,calc(20px + 2cqi),46px)/1.02 var(--display);letter-spacing:-.035em;color:var(--ink0);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.chero .ld{font:italic 400 clamp(15px,calc(12px + .5cqi),18px)/1.4 var(--serif);color:var(--ink2);margin-top:6px}
.cfacts{display:flex;align-items:center;flex-wrap:wrap;gap:4px 10px;font:400 12.5px var(--ui);color:var(--ink3);margin-top:-12px}
.cfacts .sep{font-style:normal;color:var(--ink4)}
.cfacts .mono{font-family:var(--mono);font-size:12px;color:var(--ink2)}
.cfacts b{font-weight:600;color:var(--mint)}
.ctabs{display:flex;gap:clamp(14px,2.4cqi,28px);border-bottom:1px solid var(--line1);font:500 13px var(--ui);color:var(--ink3)}
.ctabs span{padding:0 0 10px;position:relative;white-space:nowrap}
.ctabs span.on{color:var(--ink0)}
.ctabs span.on::after{content:"";position:absolute;left:0;right:0;bottom:-1px;height:2px;background:var(--mint)}
.csec{display:flex;flex-direction:column;gap:6px}
.csec h2{font:620 clamp(17px,calc(14px + .4cqi),20px)/1.2 var(--display);letter-spacing:-.02em;color:var(--ink0);margin:0 0 6px}
.cgrp{font:500 11px var(--ui);color:var(--ink4);padding:12px 0 2px;display:flex;align-items:center;gap:6px}
.crow2{display:grid;grid-template-columns:18px minmax(0,max-content) minmax(0,1fr) auto;align-items:baseline;gap:12px;
  padding:calc(7px*var(--drow)) 10px;margin:0 -10px;min-height:calc(34px*var(--drow))}
.crow2 .k{align-self:center}
.crow2 .nm{font:500 calc(13px*var(--dtxt)) var(--mono);color:var(--ink0);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.crow2 .nm .p{color:var(--ink3)}.crow2 .nm .t{color:var(--ink2)}
.crow2 .say{font:italic 400 calc(14px*var(--dtxt)) var(--serif);color:var(--ink3);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.crow2 .rt{display:flex;align-items:center;gap:8px;font:500 11px var(--mono);color:var(--ink3);visibility:hidden}
.crow2.hover{background:rgba(143,160,255,.06)}
.crow2.hover .rt{visibility:visible}
.ccode{padding:16px 18px;background:rgba(0,0,0,.18);font:400 calc(13px*var(--dtxt))/1.7 var(--mono);color:var(--ink1);white-space:pre;overflow:hidden}
.ccode .kw{color:#c5a3ff}.ccode .ty{color:var(--f-type)}.ccode .va{color:var(--f-val)}.ccode .co{color:var(--f-con)}
.ccode .ca{color:var(--f-call)}.ccode .p{color:var(--ink3)}.ccode .at{color:var(--ink3)}
@container reader (max-width:620px){ .crow2{grid-template-columns:18px minmax(0,1fr) auto} .crow2 .say{display:none} }
"""
