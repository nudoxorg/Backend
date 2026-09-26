"""Calm Ask (⌘K): one list, one reason per row, a short preview of the selected result."""
from nx4 import gem, kind, ico, esc, lang
import calm
import c_symbol

RESULTS = [
    ("function", "from_str", "serde_json::de", "turns JSON text into your type", "from_str"),
    ("trait", "Deserialize", "serde", "implement this to be parsed", ""),
    ("method", "PageDto::from_json", "present · yours", "you already do this here", "from_json"),
    ("method", "from_str", "core::str::FromStr", "same name, a different job", "from_str"),
    ("package", "serde_json", "crates.io", "the crate that does this", ""),
    ("function", "to_string", "serde_json::ser", "the way back", ""),
]


def hl(name, q):
    if not q or q not in name:
        return esc(name)
    i = name.index(q)
    return esc(name[:i]) + f'<u>{esc(q)}</u>' + esc(name[i + len(q):])


def rows():
    out = []
    for i, (k, name, where, why, q) in enumerate(RESULTS):
        sel = " sel" if i == 0 else ""
        yours = " yours" if "yours" in where else ""
        out.append(f'<div class="ar{sel}{yours}">{kind(k if k != "package" else "module", "sm", tip=False)}'
                   f'<span class="n">{hl(name, q)}</span><span class="w">{esc(where)}</span><span class="y">{esc(why)}</span></div>')
        if i == 0:
            out.append('<div class="asum"><span class="kw">pub fn</span> <span class="ca">from_str</span><span class="p">&lt;T&gt;(s: &amp;str) -&gt; </span>'
                       '<span class="ty">Result</span><span class="p">&lt;T&gt;</span></div>')
    return '<div class="al">' + "".join(out) + '</div>'


def preview():
    return ('<div class="apv">' + f'<div class="ph">{gem("function", 30)}<div class="col" style="gap:2px">'
            '<span class="nm">from_str</span><span class="wh">function in <code>serde_json::de</code></span></div></div>'
            '<div class="sg"><span class="kw">pub fn</span> <span class="ca">from_str</span><span class="p">&lt;\'a, T&gt;(s: &amp;\'a </span>'
            '<span class="ty">str</span><span class="p">) -&gt; </span><span class="ty">Result</span><span class="p">&lt;T&gt;</span>\n'
            '<span class="kw">where</span> <span class="ty">T</span><span class="p">: </span><span class="co">Deserialize</span><span class="p">&lt;\'a&gt;</span></div>'
            '<p class="sy">Deserialize an instance of type <code>T</code> from a string of JSON text.</p>'
            '<div class="fc">not in your code yet · <b>3</b> of your dependencies call it</div></div>')


CSS = """
.ascrim{position:absolute;inset:46px 0 26px;background:rgba(2,6,14,.66);z-index:50}
.ask{position:absolute;z-index:55;left:50%;transform:translateX(-50%);top:clamp(36px,8cqh,90px);width:min(960px,calc(100% - 32px));
  background:var(--glass);clip-path:polygon(12px 0,100% 0,100% calc(100% - 12px),calc(100% - 12px) 100%,0 100%,0 12px);
  box-shadow:inset 1px 1px 0 var(--bevel-hi),inset -1px -1px 0 var(--bevel-lo);container:ask / inline-size}
.af{display:flex;align-items:center;gap:12px;padding:16px 20px;border-bottom:1px solid var(--line1);color:var(--ink3)}
.af .q{font:500 calc(17px*var(--dtxt)) var(--mono);color:var(--ink0)}
.af .caret{width:2px;height:20px;background:var(--mint);margin-left:-9px}
.af .cnt{margin-left:auto;font:400 12px var(--ui);color:var(--ink4)}
.ab{display:grid;grid-template-columns:minmax(0,1.3fr) minmax(0,1fr)}
.al{padding:8px;display:flex;flex-direction:column;min-width:0}
.ar{display:grid;grid-template-columns:18px auto minmax(0,1fr) auto;align-items:baseline;gap:10px;padding:9px 12px;min-width:0}
.ar .k{align-self:center}
.ar .n{font:500 13px var(--mono);color:var(--ink0);white-space:nowrap}
.ar .n u{text-decoration:none;box-shadow:inset 0 -1.5px 0 var(--mint)}
.ar .w{font:400 11.5px var(--mono);color:var(--ink4);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.ar .y{font:italic 400 13.5px var(--serif);color:var(--ink3);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;text-align:right}
.ar.sel{background:var(--plate2);box-shadow:inset 2px 2px 0 var(--peri-hi),inset -2px -2px 0 var(--peri);
  clip-path:polygon(7px 0,100% 0,100% calc(100% - 7px),calc(100% - 7px) 100%,0 100%,0 7px)}
.ar.yours .w{color:var(--mint)}
.asum{display:none;margin:2px 12px 8px 40px;font:400 12px var(--mono);color:var(--ink2);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.asum .kw,.apv .kw{color:#c5a3ff}.asum .ca,.apv .ca{color:var(--f-call)}.asum .ty,.apv .ty{color:var(--f-type)}
.asum .p,.apv .p{color:var(--ink3)}.apv .co{color:var(--f-con)}
.apv{padding:18px 20px;border-left:1px solid var(--line1);display:flex;flex-direction:column;gap:12px;min-width:0}
.apv .ph{display:flex;align-items:center;gap:11px}
.apv .nm{font:600 14.5px var(--mono);color:var(--ink0)}.apv .wh{font:400 12px var(--ui);color:var(--ink3)}
.apv .wh code,.apv .sy code{font:400 11.5px var(--mono);color:var(--ink2)}
.apv .sg{font:400 12px/19px var(--mono);color:var(--ink1);white-space:pre-wrap;padding:8px 10px;background:rgba(0,0,0,.2)}
.apv .sy{margin:0;font:italic 400 14.5px/21px var(--serif);color:var(--ink1)}
.apv .fc{font:400 12.5px var(--ui);color:var(--ink3)}.apv .fc b{color:var(--mint)}
@container ask (max-width:760px){ .ab{grid-template-columns:minmax(0,1fr)} .apv{display:none} .asum{display:block} }
@container ask (max-width:480px){ .ar{grid-template-columns:18px minmax(0,1fr)} .ar .y{display:none} .ar .w{grid-column:2} }
"""


def build(name, w, h):
    under = c_symbol.build.__wrapped__(w, h) if hasattr(c_symbol.build, "__wrapped__") else None
    shelf = "" if w < 640 else (calm.kspine() if w < 900 else calm.shelf(rows=c_symbol.SHELF_ROWS))
    tb = calm.titlebar(beads=3 if w >= 1100 else 1)
    reader = c_symbol.folio(w - (0 if w < 640 else 42 if w < 900 else 264), hover=False)
    ask = (f'<div class="ascrim"></div><div class="ask"><div class="af">{ico("search", "s16")}<span class="q">parse json into a struct</span>'
           f'<i class="caret"></i><span class="cnt">6 results</span></div><div class="ab">{rows()}{preview()}</div></div>')
    body = calm.window(w, h, tb, shelf, reader, "nudox://present/glyph/RelationLabel", overlay=ask)
    return calm.page(name, w, h, body, css=c_symbol.CSS + CSS)


if __name__ == "__main__":
    print(build("Ask4", 1440, 900))
    print(build("Ask4-760", 760, 900))
    print(build("Ask4-480", 480, 900))
