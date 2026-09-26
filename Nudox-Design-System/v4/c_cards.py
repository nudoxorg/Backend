"""Calm peeks and lenses: short at rest; ⌘ shows keys; ⌥ (or a second rest) shows the rest."""
from nx4 import gem, kind, compass, cbar, fcomb, ico, keys, esc, lang, tpl
import calm


def card(inner, w=340, cls=""):
    return f'<div class="ccard {cls}" style="width:{w}px">{inner}</div>'


def head(mark, name, where, crumb=None, pin=False):
    c = f'<div class="crumb">{crumb}</div>' if crumb else ""
    p = f'<span class="cpin">{ico("pin", "s12")}</span>' if pin else ""
    return (f'{c}<div class="ch">{mark}<div class="col" style="gap:2px;min-width:0"><span class="nm">{name}</span>'
            f'<span class="wh">{where}</span></div>{p}</div>')


def sig(html):
    return f'<div class="sg">{html}</div>'


def say(text):
    return f'<p class="sy">{text}</p>'


def fact(html):
    return f'<div class="fc">{html}</div>'


def foot(*pairs):
    return '<div class="ft">' + "".join(f'<span>{keys(*k)}{w}</span>' for k, w in pairs) + '</div>'


SLK_SIG = ('<span class="kw">pub enum</span> <span class="ty">SemanticLinkKind</span> <span class="p">{</span> '
           '<span class="va">Calls</span><span class="p">,</span> <span class="va aw">MethodCall</span><span class="p">,</span> '
           '<span class="va">TypeReference</span><span class="p">, … }</span>')


def peek_symbol(state="rest"):
    body = (head(gem("enum", 28), "SemanticLinkKind", 'enum in <code>present::relation</code>', pin=state != "rest")
            + sig(SLK_SIG) + say("The closed vocabulary of how one symbol touches another.")
            + fact('<b>14</b> uses in your code'))
    if state == "xray":
        body += ('<div class="more">' + cbar(5, 11, 26, 0)
                 + '<div class="fch"><span>in your code</span><span class="mono">14 uses · 5 files</span></div>'
                 + fcomb([(6, "hot", [9, 12, 8, 10, 7, 11]), (3, "", [8, 11, 7]), (2, "", [9, 6]), (2, "", [7, 10]), (1, "them", [7])])
                 + '</div>')
    if state in ("keys", "xray"):
        body += foot((("Space",), "pin"), (("↵",), "open"), (("⌥", "→"), "follow"))
    return card(body, 360)


def peek_child():
    body = (head(gem("variant", 24), "MethodCall", "variant, no payload",
                 crumb='SemanticLinkKind <span class="gt">›</span> MethodCall')
            + say("A call through a receiver, <code>value.method()</code>, resolved by the compiler."))
    return card(body, 320, "focus")


def peek_package():
    body = (head(gem("package", 28), "serde", f'{lang("rust", 11)} <code>1.0.193</code> · crates.io')
            + say("A generic serialization and deserialization framework.")
            + fact('your code reaches <b>31</b> of its items'))
    return card(body, 340)


def peek_file():
    lines = [(137, "", ""), (138, '<span class="at">#[derive(Clone, Copy, Debug, Eq, Hash)]</span>', "hl"),
             (139, '<span class="kw">pub enum</span> <span class="ty">RelationLabel</span> <span class="p">{</span>', "hl"),
             (140, '    <span class="va">Typed</span><span class="p">(…),</span>', ""),
             (141, '    <span class="va">Neighbourhood</span><span class="p">,</span>', "")]
    ex = "".join(f'<div class="xl {c}"><span class="ln">{n}</span><span>{h}</span></div>' for n, h, c in lines)
    body = head(ico("file", "s16"), "glyph.rs", "present/src/glyph.rs · line 138") + f'<div class="ex">{ex}</div>'
    return card(body, 380)


def peek_version():
    body = (head(f'<span class="vt"></span>', "1.0.200", "3 weeks ago")
            + fact('<b>from_str</b> changed, and your code calls it in <b>2</b> places')
            + '<div class="dl"><span class="cadd">+3</span> added <span class="cchg">~5</span> changed</div>')
    return card(body, 320)


def pinned():
    rows = "".join(f'<div class="pr">{gem(k, 20)}<div class="col" style="gap:1px;min-width:0"><span class="nm">{n}</span>'
                   f'<span class="wh">{w}</span></div></div>'
                   for k, n, w in (("enum", "SemanticLinkKind", "present::relation"), ("method", "Page::relations", "present::page")))
    return f'<aside class="cpins"><div class="ph2">{ico("pin", "s12")}Pinned</div>{rows}</aside>'


# ---------------------------------------------------------------- lenses

def lens_release():
    items = [("+", "method", "SerializeMap::serialize_key"), ("+", "struct", "IgnoredAny"), ("~", "function", "de::from_str")]
    rows = "".join(f'<div class="cli"><span class="g {"cadd" if s == "+" else "cchg"}">{s}</span>{kind(k, "sm", tip=False)}'
                   f'<span class="mono">{n}</span></div>' for s, k, n in items)
    body = ('<div class="lh"><span class="mono v">1.0.200</span><span class="wh">3 weeks ago</span></div>'
            + fact('<b>from_str</b> changed — your code calls it in <b>2</b> places')
            + f'<div class="lis">{rows}<div class="andm">and five more</div></div>')
    return card(body, 330, "lens")


def lens_module():
    rows = "".join(f'<div class="cli">{kind(k, "sm", tip=False)}<span class="mono">{n}</span></div>'
                   for k, n in (("trait", "Deserialize"), ("trait", "Visitor"), ("function", "from_str")))
    body = ('<div class="lh"><span class="mono v">de</span><span class="wh">214 public items</span></div>'
            + fact('your code uses <b>17</b> of them, most often') + f'<div class="lis">{rows}<div class="andm">and fourteen more</div></div>')
    return card(body, 300, "lens")


def lens_language():
    rows = "".join(f'<div class="cli"><span class="mono" style="width:78px">{n}</span><span class="bar"><i style="width:{v}%"></i></span>'
                   f'<span class="wh">{o}</span></div>' for n, v, o in (("numpy", 92, "22"), ("requests", 52, "12"), ("pydantic", 38, "9")))
    body = (f'<div class="lh">{lang("python", 14)}<span class="mono v">python</span><span class="wh">7 packages</span></div>'
            + f'<div class="lis">{rows}<div class="andm">and four more</div></div>')
    return card(body, 300, "lens")


def comb_strip(hot=40, n=90):
    ticks = []
    for i in range(n):
        h = 8 + (i * 7) % 9
        cls = "mint" if i == 30 else ("hot" if i == hot else "")
        if i == hot:
            h = 22
        ticks.append(f'<i class="{cls}" style="height:{h}px"></i>')
    return '<div class="ccomb">' + "".join(ticks) + '</div>'


CSS = """
.ccard{background:var(--glass);position:relative;clip-path:polygon(10px 0,100% 0,100% calc(100% - 10px),calc(100% - 10px) 100%,0 100%,0 10px);
  box-shadow:inset 1px 1px 0 var(--bevel-hi),inset -1px -1px 0 var(--bevel-lo);padding:14px 16px 14px;display:flex;flex-direction:column;gap:10px}
.cwrap{filter:drop-shadow(0 18px 30px rgba(0,0,0,.6)) drop-shadow(0 2px 6px rgba(0,0,0,.35))}
.ccard.focus{box-shadow:inset 2px 2px 0 var(--peri-hi),inset -2px -2px 0 var(--peri)}
.ccard .ch{display:flex;align-items:center;gap:11px}
.ccard .nm{font:600 14.5px/18px var(--mono);color:var(--ink0);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.ccard .wh{font:400 12px/16px var(--ui);color:var(--ink3)}
.ccard .wh code,.ccard .sy code{font:400 11.5px var(--mono);color:var(--ink2)}
.ccard .cpin{margin-left:auto;color:var(--ink3)}
.ccard .crumb{font:400 11px var(--mono);color:var(--ink4);margin-bottom:-2px}.ccard .crumb .gt{margin:0 4px}
.ccard .sg{font:400 12px/18px var(--mono);color:var(--ink1);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;
  padding:7px 10px;background:rgba(0,0,0,.2)}
.ccard .sg .kw{color:#c5a3ff}.ccard .sg .ty{color:var(--f-type)}.ccard .sg .va{color:var(--f-val)}.ccard .sg .p{color:var(--ink3)}
.ccard .sg .aw{box-shadow:inset 0 -1.5px 0 var(--peri)}
.ccard .sy{margin:0;font:italic 400 14.5px/21px var(--serif);color:var(--ink1)}
.ccard .fc{font:400 12.5px/17px var(--ui);color:var(--ink3)}
.ccard .fc b{color:var(--mint);font-weight:600}
.ccard .more{display:flex;flex-direction:column;gap:10px;padding-top:4px}
.ccard .fch{display:flex;justify-content:space-between;font:italic 400 12px var(--serif);color:var(--ink3)}
.ccard .fch .mono{font:400 11px var(--mono);font-style:normal;color:var(--ink2)}
.ccard .ft{display:flex;gap:14px;padding-top:10px;border-top:1px solid var(--line1);font:400 11.5px var(--ui);color:var(--ink3)}
.ccard .ft span{display:flex;align-items:center;gap:6px}
.ccard .ex{font:400 11.5px/19px var(--mono);color:var(--ink2);background:rgba(0,0,0,.2);padding:6px 0}
.ccard .ex .xl{display:flex;gap:12px;padding:0 10px;white-space:pre}.ccard .ex .ln{color:var(--ink4);width:26px;text-align:right}
.ccard .ex .xl.hl{background:rgba(143,160,255,.07);color:var(--ink1)}
.ccard .ex .kw{color:#c5a3ff}.ccard .ex .ty{color:var(--f-type)}.ccard .ex .va{color:var(--f-val)}.ccard .ex .p{color:var(--ink3)}.ccard .ex .at{color:var(--ink3)}
.ccard .vt{width:2px;height:24px;background:var(--ink1);margin:0 6px}
.ccard .dl{font:400 12px var(--ui);color:var(--ink3)}
.cadd{color:var(--mint);font:600 12px var(--mono)}.cchg{color:var(--peri);font:600 12px var(--mono)}
.ccard.lens{gap:9px}
.ccard .lh{display:flex;align-items:baseline;gap:10px}.ccard .lh .v{font:600 14px var(--mono);color:var(--ink0)}
.ccard .lis{display:flex;flex-direction:column;gap:5px}
.ccard .cli{display:flex;align-items:center;gap:8px;font:500 12px var(--mono);color:var(--ink1)}
.ccard .cli .g{width:10px;text-align:center}
.ccard .cli .bar{flex:1;height:3px;background:var(--line1)}.ccard .cli .bar i{display:block;height:100%;background:var(--ink3)}
.ccard .andm{font:italic 400 12.5px var(--serif);color:var(--ink3)}
.cpins{width:280px;display:flex;flex-direction:column;gap:4px;padding:16px 14px;border-left:1px solid var(--line1);background:rgba(3,8,18,.45)}
.cpins .ph2{display:flex;align-items:center;gap:6px;font:500 11.5px var(--ui);color:var(--ink3);margin-bottom:6px}
.cpins .pr{display:flex;align-items:center;gap:10px;padding:8px 6px}
.cpins .nm{font:500 12.5px var(--mono);color:var(--ink1)}.cpins .wh{font:400 11px var(--mono);color:var(--ink4)}
.ccomb{display:flex;align-items:flex-end;justify-content:space-between;height:26px}
.ccomb i{display:block;width:2px;background:var(--ink4)}.ccomb i.mint{background:var(--mint)}.ccomb i.hot{background:var(--ink0)}
.cboard{position:relative;padding:44px 56px;display:flex;flex-direction:column;gap:34px}
.cboard h1{font:700 34px/1 var(--display);letter-spacing:-.03em;color:var(--ink0);margin:0}
.clab{font:500 11px var(--ui);color:var(--ink4);margin-bottom:10px}
.cst{position:relative;display:flex;gap:28px;align-items:flex-start;flex-wrap:wrap}
.cfrag{position:relative;width:640px;padding:18px 22px 22px;background:rgba(6,13,27,.55)}
"""


def board_peeks():
    frag = ('<div class="cfrag"><div class="csec"><h2>Made of</h2>'
            f'<div class="crow2 hover">{kind("variant", "sm", tip=False)}<span class="nm">Typed<span class="p">(</span>'
            '<span class="t" style="box-shadow:inset 0 -1.5px 0 var(--peri);color:var(--ink0)">SemanticLinkKind</span>'
            '<span class="t">, RelationDirection</span><span class="p">)</span></span><span class="say"></span><span class="rt"></span></div>'
            f'<div class="crow2">{kind("variant", "sm", tip=False)}<span class="nm">Neighbourhood</span><span class="say">A bounded neighbourhood.</span><span class="rt"></span></div>'
            f'<div class="crow2">{kind("variant", "sm", tip=False)}<span class="nm">Related</span><span class="say">Related, and nothing more.</span><span class="rt"></span></div>'
            '</div>'
            f'<div class="cwrap" style="position:absolute;left:120px;top:92px">{peek_symbol()}</div>'
            f'<div class="cwrap" style="position:absolute;left:500px;top:150px">{peek_child()}</div></div>')
    return ('<div class="cboard">' + tpl("ground").replace('class="', 'style="position:absolute;inset:0" class="', 1)
            + '<h1>Peeks</h1>'
            + f'<div><div class="clab">Resting on a word</div><div class="cst" style="height:330px">{frag}</div></div>'
            + '<div class="cst">'
            + f'<div><div class="clab">⌘ held</div><div class="cwrap">{peek_symbol("keys")}</div></div>'
            + f'<div><div class="clab">⌥ held, or rest again</div><div class="cwrap">{peek_symbol("xray")}</div></div>'
            + f'<div><div class="clab">Pinned, at 1 900 px and wider</div>{pinned()}</div></div>'
            + '<div class="cst">'
            + f'<div><div class="clab">A package</div><div class="cwrap">{peek_package()}</div></div>'
            + f'<div><div class="clab">A location</div><div class="cwrap">{peek_file()}</div></div>'
            + f'<div><div class="clab">A version</div><div class="cwrap">{peek_version()}</div></div></div></div>')


def board_lenses():
    return ('<div class="cboard">' + tpl("ground").replace('class="', 'style="position:absolute;inset:0" class="', 1)
            + '<h1>Lenses</h1>'
            + '<div class="cst" style="flex-direction:column;gap:12px"><div class="clab">Resting on a release tick</div>'
            + f'<div style="position:relative;width:640px;height:250px"><div class="cwrap" style="position:absolute;left:124px;top:0">{lens_release()}</div>'
            + f'<i style="position:absolute;left:288px;top:158px;width:1px;height:68px;background:var(--line3)"></i>'
            + f'<div style="position:absolute;left:0;right:0;bottom:0">{comb_strip()}</div></div></div>'
            + '<div class="cst">'
            + f'<div><div class="clab">Resting on a module region</div><div class="cwrap">{lens_module()}</div></div>'
            + f'<div><div class="clab">Resting on a language</div><div class="cwrap">{lens_language()}</div></div></div></div>')


if __name__ == "__main__":
    print(calm.page("Peeks", 1440, 1160, board_peeks(), css=CSS))
    print(calm.page("Lenses", 1440, 760, board_lenses(), css=CSS))
