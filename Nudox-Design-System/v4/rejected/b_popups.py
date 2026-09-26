"""Peeks (a symbol one rung up) and lenses (an aggregate broken down)."""
from nx4 import (gem, kind, caps, cap, mod, compass, compass_row, cbar, comb, fcomb, mosaic, ico, chev,
                 kbd, keys, esc, page, cursor, lang, tpl)

# ------------------------------------------------------------------ peeks

def peek_symbol(x, y, cls=""):
    return (f'<div class="peekwrap" style="left:{x}px;top:{y}px"><div class="peek {cls}">'
            '<div class="ph">' + gem("enum", 34) +
            '<div class="col" style="gap:3px;min-width:0"><span class="nm">SemanticLinkKind</span>'
            '<span class="wh">enum in <code>present::relation</code> · since <code>0.2.0</code></span></div>'
            f'<span class="acts"><span data-tip="Pin beside (Space)">{ico("pin", "s14")}</span></span></div>'
            '<div style="padding:0 14px 10px 59px">' + caps(["Clone", "Copy", "Debug", "Eq", "Hash"]) + '</div>'
            '<div class="psig"><span class="kw">pub enum</span> <span class="ty">SemanticLinkKind</span> <span class="p">{</span> '
            '<span class="va" style="color:var(--f-val)">Calls</span><span class="p">,</span> '
            '<span class="va anchor-word" style="color:var(--f-val)">MethodCall</span><span class="p">,</span> '
            '<span style="color:var(--f-val)">TypeReference</span><span class="p">,</span> <span class="more">+8</span> <span class="p">}</span></div>'
            '<div class="say">The closed vocabulary of how one symbol touches another.</div>'
            '<div class="blk">' + cbar(5, 11, 26, 0) + '</div>'
            '<div class="blk"><div class="cap-h">in your code<span class="grow"></span>'
            '<span style="font:500 11px var(--mono);font-style:normal;color:var(--ink2)">14 uses · 5 files</span></div>'
            + fcomb([(6, "hot", [9, 12, 8, 10, 7, 11]), (3, "", [8, 11, 7]), (2, "", [9, 6]), (2, "", [7, 10]), (1, "them", [7])]) + '</div>'
            '<div class="foot">' + keys("Space") + 'pin' + keys("↵") + 'open' + keys("⌥", "→") + 'follow</div>'
            '</div></div>')


def peek_child(x, y):
    return (f'<div class="peekwrap" style="left:{x}px;top:{y}px"><div class="peek child focus">'
            '<div class="crumb">SemanticLinkKind ' + chev("s12") + ' <b>MethodCall</b></div>'
            '<div class="ph" style="padding-top:8px">' + gem("variant", 26) +
            '<div class="col" style="gap:2px;min-width:0"><span class="nm" style="font-size:14px">MethodCall</span>'
            '<span class="wh">variant, no payload</span></div></div>'
            '<div class="say" style="padding-top:0">A call through a receiver: <code style="font:12px var(--mono);font-style:normal">'
            'value.method()</code>, resolved by the compiler.</div>'
            '<div class="blk"><div class="cap-h">where the engine emits it</div>'
            '<div class="col" style="gap:3px;font:400 12px/18px var(--mono);color:var(--ink1)">'
            f'<span class="row g6">{kind("function", "sm", tip=False)}rust::resolve::method_edge</span>'
            f'<span class="row g6">{kind("function", "sm", tip=False)}typescript::calls::member</span></div></div>'
            '<div class="foot">' + keys("↵") + 'open' + keys("Esc") + 'back to SemanticLinkKind</div>'
            '</div></div>')


def peek_package(x, y):
    ticks = []
    for i in range(40):
        if i == 22:
            ticks.append((13, "maj mint", ""))
        elif i == 24:
            ticks.append((12, "maj peri", ""))
        elif i == 39:
            ticks.append((14, "maj", ""))
        else:
            ticks.append((5 + (i * 5) % 6, "", ""))
    return (f'<div class="peekwrap" style="left:{x}px;top:{y}px"><div class="peek" style="width:360px">'
            '<div class="ph">' + gem("package", 34) +
            '<div class="col" style="gap:3px;min-width:0"><span class="nm">serde</span>'
            f'<span class="wh">{lang("rust", 12)} <code>1.0.193</code> · crates.io · MIT or Apache-2.0</span></div></div>'
            '<div class="say" style="padding-top:0">A generic serialization and deserialization framework.</div>'
            '<div class="blk"><div class="cap-h">64 releases<span class="grow"></span>'
            '<span style="font:500 11px var(--mono);font-style:normal"><span style="color:var(--mint)">pin 1.0.191</span> · '
            '<span style="color:var(--peri)">reading 1.0.193</span> · 1.0.210</span></div>' + comb(ticks, 18) + '</div>'
            '<div class="blk"><div class="cap-h">what it is made of<span class="grow"></span>'
            '<span style="font:500 11px var(--mono);font-style:normal;color:var(--ink2)">412 public</span></div>'
            '<div class="lensc" style="width:auto;clip-path:none;box-shadow:none;background:none"><div class="stack" style="margin:0">'
            '<i class="ty" style="--w:118"></i><i class="co" style="--w:64"></i><i class="ca" style="--w:190"></i><i class="va" style="--w:40"></i></div>'
            '<div class="legend" style="padding-left:0"><span class="ty" style="color:var(--f-type)"><i></i>118 types</span>'
            '<span style="color:var(--f-con)"><i></i>64 contracts</span><span style="color:var(--f-call)"><i></i>190 callables</span>'
            '<span style="color:var(--f-val)"><i></i>40 values</span></div></div></div>'
            '<div class="blk"><div class="cap-h">you reach<span class="grow"></span></div>'
            '<div style="font:400 12.5px/18px var(--ui);color:var(--ink1)"><b style="color:var(--mint);font-family:var(--mono)">31</b> items from '
            '<b style="font-family:var(--mono);font-weight:500">backend</b> and <b style="font-family:var(--mono);font-weight:500">polyglot</b></div></div>'
            '<div class="foot">' + keys("↵") + 'open' + keys("Space") + 'pin</div>'
            '</div></div>')


def peek_file(x, y):
    lines = [
        (136, '<span class="p">}</span>', ""),
        (137, "", ""),
        (138, '<span class="at">#[derive(Clone, Copy, Debug, Eq, Hash)]</span>', "hot"),
        (139, '<span class="kw">pub enum</span> <span class="ty">RelationLabel</span> <span class="p">{</span>', "hot"),
        (140, '&nbsp;&nbsp;&nbsp;&nbsp;<span style="color:var(--f-val)">Typed</span><span class="p">(</span><span class="ty">SemanticLinkKind</span><span class="p">, …),</span>', ""),
        (141, '&nbsp;&nbsp;&nbsp;&nbsp;<span style="color:var(--f-val)">Neighbourhood</span><span class="p">,</span>', "new"),
    ]
    rows = "".join(
        f'<div style="display:grid;grid-template-columns:3px 30px 1fr;gap:8px;align-items:center;{"background:var(--peri-soft)" if c == "hot" else ""}">'
        f'<i style="height:18px;background:{"var(--mint)" if c == "new" else "var(--line2)"}"></i>'
        f'<span style="text-align:right;color:var(--ink4)">{n}</span><span>{code}</span></div>'
        for n, code, c in lines)
    return (f'<div class="peekwrap" style="left:{x}px;top:{y}px"><div class="peek" style="width:380px">'
            f'<div class="ph" style="padding-bottom:8px">{ico("file", "s14")}<div class="col" style="gap:2px"><span class="nm" style="font-size:13px">glyph.rs</span>'
            '<span class="wh"><code>present/src/glyph.rs</code> · 312 lines</span></div></div>'
            f'<div class="psig" style="white-space:normal;padding:6px 8px;font-size:11.5px;line-height:18px">{rows}</div>'
            '<div class="say" style="font-size:12.5px;line-height:17px;padding-top:8px">Line 141 is the newest here: added in <code style="font:11px var(--mono);font-style:normal">0.1.3</code>. '
            'The bar is age; bright means recent.</div>'
            '<div class="foot">' + keys("S") + 'peel to source' + keys("↵") + 'open file</div>'
            '</div></div>')


def board_peeks():
    # a page fragment as the stage
    sig_row = (
        '<div class="ledger" style="width:760px">'
        f'<div class="lr hover">{kind("variant", "sm", tip=False)}<span class="nm">Typed<span class="p">(</span>'
        '<a class="anchor-word">SemanticLinkKind</a><span class="p">, </span><a>RelationDirection</a><span class="p">)</span></span>'
        '<span class="say">A relation whose compiler kind and direction are both known.</span>'
        f'<span class="rt">{compass(0, 2, 14, 1)}<span class="uses">14</span></span></div>'
        f'<div class="lr">{kind("variant", "sm", tip=False)}<span class="nm">Neighbourhood</span>'
        '<span class="say">A bounded neighbourhood whose per-edge kind the reply did not carry.</span>'
        f'<span class="rt">{compass(0, 0, 3, 0)}<span class="uses">3</span></span></div>'
        f'<div class="lr">{kind("variant", "sm", tip=False)}<span class="nm">Related</span>'
        '<span class="say">Related, and nothing more is known.</span>'
        f'<span class="rt">{compass(0, 0, 2, 0)}<span class="uses">2</span></span></div></div>')
    stage = (
        '<div class="frame" style="width:1312px;height:640px;background:var(--g1)">'
        + '<div style="position:absolute;inset:0">' + tpl("ground") + '</div>'
        + '<div style="position:absolute;left:40px;top:34px"><div class="ledger-h"><span class="sec-h">Made of</span>'
          '<span class="cnt">three variants, one carries data</span></div>' + sig_row + '</div>'
        + peek_symbol(128, 124)
        + peek_child(534, 250)
        + cursor(206, 92) + cursor(470, 214)
        + '<aside class="pins" style="display:flex;position:absolute;right:0;top:0;bottom:0;width:300px">'
          '<div class="pins-h">' + ico("pin", "s12") + 'Pinned<span style="flex:1"></span>' + keys("⌘", "P") + '</div>'
          '<div class="peek pinrow" style="width:auto;clip-path:none"><div class="ph" style="padding:10px 12px 6px">' + gem("method", 24)
        + '<div class="col" style="gap:1px"><span class="nm" style="font-size:13px">Page::relations</span><span class="wh"><code>present::page</code></span></div></div>'
          '<div class="say" style="padding:0 12px 8px;font-size:13px;line-height:18px">Every relation group of a page, in rose order.</div>'
          '<div style="padding:0 12px 10px">' + compass_row(0, 0, 4, 6) + '</div></div>'
          '<div class="peek pinrow" style="width:auto;clip-path:none;opacity:.55"><div class="ph" style="padding:10px 12px 10px">' + gem("trait", 24)
        + '<div class="col" style="gap:1px"><span class="nm" style="font-size:13px">Display</span><span class="wh"><code>core::fmt</code></span></div></div></div>'
          '<div style="font:italic 400 12.5px/17px var(--serif);color:var(--ink3);margin-top:auto">Pins stay while you walk. '
          'On wide screens they live here; on narrow ones they stack under ⌘P.</div>'
          '</aside>'
        + '</div>')
    rules = (
        '<div class="rule-strip">'
        '<div><b>Rest 350 ms, it rises</b><span>A word you rest on climbs one rung: a tag becomes a card. Moving on dismisses it; '
        'moving <i>into</i> it keeps it.</span><span class="k2">' + keys("Space") + '<span>or rest</span></span></div>'
        '<div><b>Peeks chain</b><span>Rest on a word inside a peek and a child peek opens beside it with the path on top. '
        'Three deep, then it asks you to open.</span><span class="k2">' + keys("Esc") + '<span>steps back one</span></span></div>'
        '<div><b>Pin keeps it</b><span>A pinned peek leaves the pointer and joins the pinned column; it keeps updating as the page '
        'underneath changes.</span><span class="k2">' + keys("Space") + '<span>pin</span></span></div>'
        '<div><b>Follow without losing your place</b><span>⌥→ descends into what the peek shows; ⌘[ comes straight back to '
        'this word with the peek reopened.</span><span class="k2">' + keys("⌥", "→") + keys("⌘", "[") + '</span></div>'
        '</div>')
    others = (
        '<div class="row" style="gap:28px;align-items:flex-start;position:relative;height:430px">'
        + '<div style="position:relative;width:380px">' + label("A package, from an import") + peek_package(0, 40) + '</div>'
        + '<div style="position:relative;width:400px">' + label("A file, from a location") + peek_file(0, 40) + '</div>'
        + '<div style="position:relative;width:340px">' + label("A version, from “since”") + lens_history(0, 40) + '</div>'
        + '</div>')
    body = (
        '<div class="doc" style="padding:48px 64px;display:flex;flex-direction:column;gap:26px">'
        '<div class="col" style="gap:8px"><span class="t-micro" style="color:var(--mint)">FACET v4 · 14</span>'
        '<span class="t-display-xl">Peeks</span>'
        '<span class="lede" style="max-width:none">Everything you can point at is one rung away from its card. The card is the same '
        'shape everywhere: what it is, one sentence, its shape, where it touches your code, and the keys to go further.</span></div>'
        + stage + rules + others + '</div>')
    return body


def label(t):
    return f'<span class="t-micro" style="display:block;color:var(--ink3);margin-bottom:10px">{t}</span>'


# ------------------------------------------------------------------ lenses

def lens_history(x, y):
    return (f'<div class="peekwrap" style="left:{x}px;top:{y}px"><div class="lensc">'
            '<div class="lh"><b>RelationLabel</b><span class="when">across 24 releases</span></div>'
            '<div class="items" style="padding-top:0">'
            '<div class="it"><span class="sy plus">+</span><code>0.0.3</code><span class="q">added, two variants</span></div>'
            '<div class="it"><span class="sy chg">~</span><code>0.1.3</code><span class="q">gained Neighbourhood</span></div>'
            '<div class="it"><span class="sy" style="color:var(--mint)">◆</span><code>0.2.1</code><span class="q">you pin this</span></div>'
            '</div><div style="padding:10px 13px 0">'
            + comb([(5 + (i * 7) % 4, ("maj peri" if i in (3, 11) else "maj mint" if i == 17 else ""), "") for i in range(24)], 20)
            + '</div><div class="foot">' + keys("↵") + 'diff 0.1.3 with 0.2.1</div></div></div>')


def release_lens(x, y):
    return (f'<div class="peekwrap" style="left:{x}px;top:{y}px"><div class="lensc" style="width:356px">'
            '<div class="lh"><b>1.0.200</b><span class="when">3 weeks ago</span><span class="grow"></span>'
            f'<span data-tip="nothing breaking">{mod("inherited")}</span></div>'
            '<div class="fig"><span class="plus"><b>+3</b> added</span><span class="chg"><b>~5</b> changed</span><span class="minus"><b>−0</b> removed</span></div>'
            '<div class="stack"><i class="ca" style="--w:2"></i><i class="ty" style="--w:1"></i><i class="ca hat" style="--w:4"></i><i class="va hat" style="--w:1"></i></div>'
            '<div class="legend"><span style="color:var(--f-call)"><i></i>6 callables</span><span style="color:var(--f-type)"><i></i>1 type</span>'
            '<span style="color:var(--f-val)"><i></i>1 value</span><span style="color:var(--ink3)">hatched = changed</span></div>'
            '<div class="items">'
            f'<div class="it"><span class="sy plus">+</span>{kind("method", "sm", tip=False)}SerializeMap::serialize_key<span class="q">new</span></div>'
            f'<div class="it"><span class="sy plus">+</span>{kind("struct", "sm", tip=False)}IgnoredAny<span class="q">new</span></div>'
            f'<div class="it"><span class="sy chg">~</span>{kind("function", "sm", tip=False)}de::from_str<span class="q">bound relaxed</span></div>'
            '<div class="it" style="color:var(--ink3);font-family:var(--serif);font-style:italic">and five more</div></div>'
            '<div class="yours">' + ico("target", "s14", "color:var(--mint)") + '<span><code>from_str</code> changed, and '
            '<b style="color:var(--ink0)">backend</b> calls it in 2 places</span></div>'
            '<div class="foot">' + keys("↵") + 'diff with 1.0.193' + keys("P") + 'pin this version</div>'
            '</div></div>')


def big_comb(hover=40):
    ticks = []
    for i in range(64):
        v = f"1.0.{147 + i}"
        h = 10 + (i * 37) % 17
        cls = ""
        if i in (0, 13, 29, 45):
            cls = "maj"
            h += 6
        if i == 44:
            cls, h = "maj mint", 30
        if i == 46:
            cls, h = "maj peri", 28
        if i == 63:
            cls, h = "maj", 34
        if i in (8, 9, 51):
            cls += " hat"
        for d, n in ((0, "is-hover"), (1, "n1"), (2, "n2"), (3, "n3")):
            if i == hover - d or i == hover + d:
                cls += " " + n
        ticks.append((h, cls, ""))
    return comb(ticks, 48, style="gap:0")


def tile(name, n, stones, you=None, hover_stone=None, head_hover=False):
    st = []
    for i, s in enumerate(stones):
        extra = " is-hover" if i == hover_stone else ""
        st.append(s + extra)
    h = ' style="box-shadow:inset 0 -1px 0 var(--peri)"' if head_hover else ""
    return (f'<div class="cut sm" style="padding:12px 12px 12px;display:flex;flex-direction:column;gap:10px;width:268px">'
            f'<div class="row g8"{h}>{kind("module", "sm", tip=False)}<span style="font:600 13px var(--mono);color:var(--ink0)">{name}</span>'
            f'<span style="flex:1"></span><span style="font:500 11px var(--mono);color:var(--ink3)">{n}</span></div>'
            + mosaic(st) + "</div>")


def stones(n, seed, fam_cycle=("ty", "ca", "ca", "va", "co")):
    out = []
    for i in range(n):
        f = fam_cycle[(i * 7 + seed) % len(fam_cycle)]
        r = (i * 13 + seed * 5) % 23
        s = f
        if r in (1, 2, 3, 4, 5, 6, 7):
            s += " y"
        if r == 11:
            s += " new"
        if r == 17 and seed % 2 == 0:
            s += " gate"
        out.append(s)
    return out


def module_lens(x, y):
    return (f'<div class="peekwrap" style="left:{x}px;top:{y}px"><div class="lensc" style="width:300px">'
            f'<div class="lh">{kind("module", "sm", tip=False)}<b>de</b><span class="when">214 public items</span></div>'
            '<div class="stack"><i class="ty" style="--w:52"></i><i class="co" style="--w:31"></i><i class="ca" style="--w:108"></i><i class="va" style="--w:23"></i></div>'
            '<div class="legend"><span style="color:var(--f-type)"><i></i>52</span><span style="color:var(--f-con)"><i></i>31</span>'
            '<span style="color:var(--f-call)"><i></i>108</span><span style="color:var(--f-val)"><i></i>23</span></div>'
            '<div class="yours">' + ico("target", "s14", "color:var(--mint)") + '<span>you reach <b style="color:var(--ink0)">17</b>: '
            '<code>Deserialize</code>, <code>Visitor</code>, <code>from_str</code> …</span></div>'
            '<div class="items"><div class="it" style="font-family:var(--serif);font-style:italic;color:var(--ink3)">Start here</div>'
            f'<div class="it">{kind("trait", "sm", tip=False)}Deserialize<span class="q">what your types become</span></div>'
            f'<div class="it">{kind("trait", "sm", tip=False)}Deserializer<span class="q">only if you write a format</span></div></div>'
            '<div class="foot">' + keys("↵") + 'open module' + keys("⌥") + 'dim all but yours</div></div></div>')


def stone_peek(x, y):
    return (f'<div class="peekwrap" style="left:{x}px;top:{y}px"><div class="peek" style="width:300px">'
            '<div class="ph" style="padding-bottom:6px">' + gem("function", 26) +
            '<div class="col" style="gap:2px"><span class="nm" style="font-size:13.5px">next_key</span>'
            '<span class="wh">method of <code>MapAccess</code></span></div></div>'
            '<div class="psig" style="font-size:11.5px"><span class="kw">fn</span> <span class="ca">next_key</span>'
            '<span class="p">&lt;K&gt;(&amp;mut self) -&gt; Result&lt;Option&lt;K&gt;, …&gt;</span></div>'
            '<div class="say" style="font-size:13px;line-height:18px">Walks a map one key at a time.</div>'
            '<div class="blk" style="padding-bottom:12px">' + compass_row(0, 0, 6, 2) + '</div></div></div>')


def lang_lens(x, y):
    pk = [("numpy", 38, "most opened"), ("requests", 22, ""), ("pydantic", 16, ""), ("bytes-py", 9, ""), ("rich", 7, ""), ("httpx", 5, ""), ("attrs", 3, "")]
    rows = "".join(f'<div class="it"><code style="width:84px">{n}</code><span style="flex:1;height:5px;background:var(--line2);position:relative">'
                   f'<i style="position:absolute;left:0;top:0;bottom:0;width:{v * 2.4}%;background:#8fc4ff"></i></span>'
                   f'<span class="q" style="width:74px;text-align:right">{q or str(v) + " opens"}</span></div>' for n, v, q in pk)
    return (f'<div class="peekwrap" style="left:{x}px;top:{y}px"><div class="lensc" style="width:320px">'
            f'<div class="lh">{lang("python", 16)}<b>python</b><span class="when">7 packages · 48 pages read</span></div>'
            f'<div class="items">{rows}</div>'
            '<div class="foot">' + keys("↵") + 'only python on the rings</div></div></div>')


def board_lenses():
    head = (
        '<div class="frame cut" style="width:1312px;height:440px;background:var(--g1);padding:0">'
        + '<div style="position:absolute;inset:0">' + tpl("ground") + '</div>'
        + '<div style="position:absolute;left:44px;top:34px;display:flex;gap:18px;align-items:center">' + gem("package", 64)
        + '<div class="col" style="gap:4px"><span class="hero-name" style="font-size:40px">serde</span>'
          '<span class="lede" style="font-size:17px">A generic serialization and deserialization framework.</span></div></div>'
        + '<div style="position:absolute;left:44px;right:44px;top:340px">' + big_comb(40)
        + '<div class="row" style="justify-content:space-between;margin-top:8px;font:500 11px var(--mono);color:var(--ink3)">'
          '<span>1.0.147</span><span><span style="color:var(--mint)">you pin 1.0.191</span> · <span style="color:var(--peri)">reading 1.0.193</span></span><span>1.0.210</span></div></div>'
        + '<i style="position:absolute;left:822px;top:286px;width:1px;height:58px;background:var(--peri);opacity:.7"></i>'
        + release_lens(612, 18)
        + cursor(815, 348)
        + '<div style="position:absolute;left:44px;top:150px;width:520px;font:italic 400 14px/20px var(--serif);color:var(--ink2)">'
          '<b style="font:620 16px/20px var(--display);font-style:normal;color:var(--ink0);display:block;margin-bottom:6px">Every release is a tick.</b>'
          'Height is how much changed; hatched ticks were not indexed. The wave follows your pointer and the lens follows the wave: '
          'what was added, what changed, which family, and whether any of it touches code you call.</div>'
        + '</div>')
    tiles = (
        '<div class="frame" style="width:1312px;height:470px;background:var(--g1)">'
        + '<div style="position:absolute;inset:0">' + tpl("ground") + '</div>'
        + '<div style="position:absolute;left:44px;top:34px;display:flex;gap:16px;flex-wrap:wrap;width:600px">'
        + tile("de", 214, stones(80, 3), head_hover=True) + tile("ser", 167, stones(72, 4), hover_stone=26)
        + '</div>'
        + module_lens(52, 72) + stone_peek(470, 168)
        + cursor(110, 58) + cursor(456, 136)
        + '<div style="position:absolute;left:760px;top:34px;width:520px">' + label("Orbit · the language comb")
        + comb([(8 + (i * 11) % 22, "is-hover" if i == 12 else ("n1" if i in (11, 13) else ("n2" if i in (10, 14) else "")), "") for i in range(48)], 44, style="color:#8fc4ff")
        + '</div>' + lang_lens(900, 120)
        + '</div>')
    rules = (
        '<div class="rule-strip">'
        '<div><b>Every aggregate has a lens</b><span>A tick, a tile, a count, a comb: rest on it and it breaks down into its parts, '
        'by family, by change, by who uses it.</span></div>'
        '<div><b>Lenses are made of marks</b><span>The parts inside a lens are the same marks as outside, so you can rest on them '
        'again and go one more rung down.</span></div>'
        '<div><b>Yours comes first</b><span>If any part touches code you own, the lens says so in one mint line, before '
        'anything else.</span></div>'
        '<div><b>Keys, not buttons</b><span>A lens never has buttons. Its foot says the two keys that go further, and they work '
        'the moment it opens.</span></div></div>')
    return ('<div class="doc" style="padding:48px 64px;display:flex;flex-direction:column;gap:26px">'
            '<div class="col" style="gap:8px"><span class="t-micro" style="color:var(--mint)">FACET v4 · 15</span>'
            '<span class="t-display-xl">Lenses</span>'
            '<span class="lede" style="max-width:none">Numbers never stand alone. Every count, tick and tile opens into a breakdown '
            'when you rest on it, drawn with the same marks one level down.</span></div>'
            + head + tiles + rules + '</div>')


if __name__ == "__main__":
    print(page("Peeks", 1440, 1640, board_peeks(), title="Peeks"))
    print(page("Lenses", 1440, 1380, board_lenses(), title="Lenses"))
