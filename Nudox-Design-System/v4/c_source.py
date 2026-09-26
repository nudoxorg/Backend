"""Calm Source: the item you peeled into is open; its neighbours are one quiet signature line each.

At rest: line numbers, code, a mint tick on lines changed in the newest
release, one facts line under the open item, and its doc in the margin.
Hover a folded line for its facts; ⌥ spells every line's age and every
fold's facts.
"""
import re

from nx4 import kind, compass, esc
import calm

ITEMS = [("enum", "RelationLabel"), ("module", "impl RelationLabel", 0, "open"), ("method", "is_typed", 1),
         ("method", "as_str", 1, "cur"), ("method", "flip", 1), ("method", "into_heading", 1), ("method", "from_kind", 1),
         ("module", "impl Display"), ("function", "relation_label"), ("function", "kind_glyph"),
         ("constant", "MAX_GLYPHS", 0, "dim"), ("struct", "GlyphSet"), ("trait", "Glyphs"), ("function", "glyph_for")]

I = "    "


def L(n, html, cls="", new=False):
    return f'<div class="sl {cls}"><span class="ln">{n}</span><i class="tk{" new" if new else ""}"></i><span class="tx">{html}</span></div>'


def F(n, html, cp, lines, depth=1, hover=False):
    return (f'<div class="sl fold{" hover" if hover else ""}"><span class="ln">{n}</span><i class="tk"></i>'
            f'<span class="tx">{I * depth}{html}<span class="el"> {{ … }}</span></span>'
            f'<span class="fx">{compass(*cp, tip=False)}<span>{lines} lines</span></span></div>')


def code(xray=False):
    rows = [
        F(138, '<span class="kw">pub enum</span> <span class="ty">RelationLabel</span>', (3, 3, 9, 4), 7, 0),
        L(146, '<span class="kw">impl</span> <span class="ty">RelationLabel</span> <span class="p">{</span>'),
        F(147, '<span class="kw">pub fn</span> <span class="ca">is_typed</span><span class="p">(&amp;self) -&gt; bool</span>', (0, 0, 4, 0), 3),
        L(149, I + '<span class="dc">/// The words this group prints.</span>', "cur"),
        L(150, I + '<span class="dc">/// Stable across surfaces: the desktop reader, the terminal and MCP print the same.</span>', "cur"),
        L(151, I + '<span class="at">#[must_use]</span>', "cur", True),
        L(152, I + '<span class="kw">pub const fn</span> <span class="ca">as_str</span><span class="p">(self) -&gt; &amp;</span>'
              '<span class="kw">\'static</span> <span class="ty">str</span> <span class="p">{</span>', "cur head"),
        '<div class="sfacts"><b>9</b> callers, <em>6</em> yours<i>·</i><b>2</b> tests<i>·</i>since <span class="mono">0.3.0</span></div>',
        L(153, I * 2 + '<span class="kw">match</span> <span class="kw">self</span> <span class="p">{</span>', "cur"),
        L(154, I * 3 + '<span class="kw">Self</span><span class="p">::</span><span class="va">Typed</span><span class="p">(kind, dir) =&gt; </span>'
              '<span class="ca">typed_words</span><span class="p">(kind, dir),</span>', "cur", True),
        L(155, I * 3 + '<span class="kw">Self</span><span class="p">::</span><span class="va">Neighbourhood</span><span class="p"> =&gt; </span>'
              '<span class="st">"Nearby"</span><span class="p">,</span>', "cur"),
        L(156, I * 3 + '<span class="kw">Self</span><span class="p">::</span><span class="va">Related</span><span class="p"> =&gt; </span>'
              '<span class="st">"Related"</span><span class="p">,</span>', "cur"),
        L(157, I * 2 + '<span class="p">}</span>', "cur"),
        L(158, I + '<span class="p">}</span>', "cur"),
        F(163, '<span class="kw">pub fn</span> <span class="ca">flip</span><span class="p">(&amp;mut self)</span>', (0, 0, 2, 1), 5, hover=not xray),
        F(169, '<span class="kw">pub fn</span> <span class="ca">into_heading</span><span class="p">(self) -&gt; </span><span class="ty">Heading</span>', (0, 0, 3, 2), 4),
        F(175, '<span class="kw">pub fn</span> <span class="ca">from_kind</span><span class="p">(kind: </span><span class="ty">SemanticLinkKind</span>'
               '<span class="p">) -&gt; </span><span class="kw">Self</span>', (0, 0, 5, 1), 6),
        L(183, '<span class="p">}</span>'),
        F(184, '<span class="kw">impl</span> <span class="co">Display</span> <span class="kw">for</span> <span class="ty">RelationLabel</span>', (1, 0, 0, 3), 6, 0),
        F(192, '<span class="kw">pub fn</span> <span class="ca">relation_label</span><span class="p">(link: &amp;</span><span class="ty">Link</span>'
               '<span class="p">) -&gt; </span><span class="ty">RelationLabel</span>', (0, 0, 6, 3), 14, 0),
        F(208, '<span class="kw">pub const fn</span> <span class="ca">kind_glyph</span><span class="p">(kind: </span><span class="ty">Kind</span>'
               '<span class="p">) -&gt; </span><span class="ty">Glyph</span>', (0, 0, 11, 0), 9, 0),
        '<div class="sl more"><span class="ln"></span><i class="tk"></i><span class="tx">4 more items</span></div>',
    ]
    if xray:
        ver = {138: "0.1.3", 146: "0.1.0", 147: "0.2.0", 149: "0.3.0", 150: "0.3.0", 151: "0.4.2", 152: "0.3.0", 153: "0.3.0",
               154: "0.4.2", 155: "0.3.4", 156: "0.3.4", 157: "0.3.0", 158: "0.3.0", 163: "0.2.1", 169: "0.3.0", 175: "0.1.0",
               183: "0.1.0", 184: "0.1.0", 192: "0.2.4", 208: "0.1.0"}
        rows = [re.sub(r'<span class="ln">(\d+)</span>',
                       lambda m: f'<span class="ln v{" new" if ver[int(m.group(1))] == "0.4.2" else ""}">{ver[int(m.group(1))]}</span>', r)
                for r in rows]
    return f'<div class="src{" xray" if xray else ""}">' + "".join(rows) + '</div>'


def margin():
    return ('<aside class="smg"><div class="nm">as_str</div>'
            '<p>The words this group prints. Stable across surfaces: the desktop reader, the terminal and MCP print the same.</p>'
            '<div class="cb"><div class="h">Called by</div>'
            + "".join(f'<div class="c{" y" if y else ""}">{esc(n)}</div>' for n, y in
                      (("render::heading", True), ("Page::relations", True), ("outline::label_for", True), ("mcp::print_group", False)))
            + '<div class="c m">and five more</div></div></aside>')


CSS = """
.sfol{display:grid;grid-template-columns:minmax(0,1fr) clamp(200px,20cqi,260px);gap:0 clamp(24px,3cqi,44px);
  padding:clamp(18px,3cqi,34px) clamp(14px,3cqi,40px) 60px;max-width:1240px;margin:0 auto}
.scr{grid-column:1/-1;font:500 12.5px var(--mono);color:var(--ink3);margin-bottom:18px}
.scr b{color:var(--ink0);font-weight:600}
.src{font:400 calc(13px*var(--dtxt))/calc(22px*var(--drow)) var(--mono);color:var(--ink1);min-width:0;overflow:hidden}
.src .sl{display:grid;grid-template-columns:38px 2px minmax(0,1fr) auto;column-gap:12px;align-items:center;white-space:pre;min-height:calc(22px*var(--drow))}
.src .ln{text-align:right;color:var(--ink4);font-size:11.5px;width:auto;margin:0;padding:0}
.src .ln.v{color:var(--ink3);font-size:10.5px}.src .ln.v.new{color:var(--mint)}
.src.xray .sl{grid-template-columns:44px 2px minmax(0,1fr) auto}
.src .tk{align-self:stretch}.src .tk.new{background:var(--mint)}
.src .tx{overflow:hidden;text-overflow:ellipsis}
.src .fold .tx{color:var(--ink2)}.src .fold .el{color:var(--ink4)}
.src .fx{display:flex;align-items:center;gap:8px;font:400 11px var(--mono);color:var(--ink3);visibility:hidden;padding-right:8px}
.src .fold.hover{background:rgba(143,160,255,.06)}.src .fold.hover .fx,.src.xray .fx{visibility:visible}
.src .more .tx{font:italic 400 13px var(--serif);color:var(--ink3);white-space:normal}
.src .cur .tx{color:var(--ink1)}
.src .dc{font:italic 400 calc(14px*var(--dtxt)) var(--serif);color:var(--ink3)}
.src .kw{color:#c5a3ff}.src .ty{color:var(--f-type)}.src .ca{color:var(--f-call)}.src .va{color:var(--f-val)}
.src .co{color:var(--f-con)}.src .at,.src .p{color:var(--ink3)}.src .st{color:var(--amber)}
.sfacts{margin:2px 0 6px 52px;font:400 12px var(--ui);color:var(--ink3);white-space:normal}
.sfacts b{color:var(--ink1);font-weight:600}.sfacts em{font-style:normal;color:var(--mint);font-weight:600}
.sfacts i{font-style:normal;margin:0 8px;color:var(--ink4)}.sfacts .mono{font:400 11.5px var(--mono);color:var(--ink2)}
.smg{display:flex;flex-direction:column;gap:12px;padding-top:4px}
.smg .nm{font:600 13px var(--mono);color:var(--ink0)}
.smg p{margin:0;font:italic 400 14px/21px var(--serif);color:var(--ink2)}
.smg .cb{display:flex;flex-direction:column;gap:6px;margin-top:10px}
.smg .h{font:500 11.5px var(--ui);color:var(--ink3);margin-bottom:2px}
.smg .c{font:500 12px var(--mono);color:var(--ink2)}.smg .c.y{color:var(--ink1)}
.smg .c.m{font:italic 400 12.5px var(--serif);color:var(--ink4)}
@container reader (max-width:980px){ .sfol{grid-template-columns:minmax(0,1fr)} .smg{padding-top:26px;max-width:560px} }
@container reader (max-width:520px){ .src .sl{grid-template-columns:0 2px minmax(0,1fr) auto;column-gap:8px} .src .ln{visibility:hidden}
  .sfacts{margin-left:10px} }
"""


def build(name, w, h, xray=False):
    shelf = "" if w < 640 else (calm.kspine(("enum", "module", "method", "method", "function"), 3) if w < 900
                                 else calm.shelf(book=("module", "glyph.rs", "present · 312 lines"), rows=ITEMS, up=("RelationLabel", "")))
    tb = calm.titlebar(here_kind="method", here="as_str", path="present › glyph › RelationLabel", beads=3 if w >= 1100 else 1)
    reader = (f'<div class="sfol"><div class="scr">present › glyph.rs › <b>as_str</b></div>{code(xray)}{margin()}</div>')
    body = calm.window(w, h, tb, shelf, reader, "nudox://present/glyph/RelationLabel/as_str#L152")
    return calm.page(name, w, h, body, css=CSS)


if __name__ == "__main__":
    print(build("Source4", 1440, 900))
    print(build("Source4-xray", 1440, 900, xray=True))
    print(build("Source4-760", 760, 900))
    print(build("Source4-480", 480, 900))
