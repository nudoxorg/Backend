"""Calm ladder: the same thing at four rungs (mark, tag, row, card), and ⌥ spelling a list in place."""
from nx4 import gem, kind, compass, esc, lang, mod
import calm
import c_cards


def rung_row(title, mark, tag, row, card):
    return (f'<div class="lr"><div class="t">{title}</div><div class="c">{mark}</div><div class="c">{tag}</div>'
            f'<div class="c">{row}</div><div class="c cw">{card}</div></div>')


def board():
    sym = rung_row("A symbol", gem("enum", 22),
                   f'<span class="tg">{gem("enum", 18)}<span>SemanticLinkKind</span></span>',
                   f'<div class="rw">{kind("enum", "sm", tip=False)}<span class="n">SemanticLinkKind</span>'
                   '<span class="s">how one symbol touches another</span></div>',
                   f'<div class="cwrap">{c_cards.peek_symbol()}</div>')
    pkg = rung_row("A package", gem("package", 22),
                   f'<span class="tg">{lang("rust", 14)}<span>serde</span></span>',
                   f'<div class="rw">{lang("rust", 14)}<span class="n">serde</span><span class="s">serialization framework</span></div>',
                   f'<div class="cwrap">{c_cards.peek_package()}</div>')
    rel = rung_row("A release", '<span class="tick"></span>',
                   '<span class="tg"><span class="tick sm"></span><span>1.0.200</span></span>',
                   '<div class="rw"><span class="n">1.0.200</span><span class="s">3 weeks ago · touches from_str</span></div>',
                   f'<div class="cwrap">{c_cards.lens_release()}</div>')
    items = [("trait", "Deserialize", "abstract", "what your types become"), ("trait", "Visitor", "abstract", "walks one value"),
             ("function", "from_str", None, "parse from a string"), ("struct", "IgnoredAny", "derived", "skips any value")]

    def lst(x):
        out = []
        for k, n, m, say in items:
            spell = f'<span class="sp">{"<b>" + m + "</b> · " if m else ""}{say}</span>' if x else ""
            out.append(f'<div class="xi">{kind(k, "sm", tip=False)}<span class="n">{n}</span>{spell}</div>')
        return "".join(out)

    return ('<div class="cboard"><h1>One thing, four rungs</h1>'
            '<div class="lad"><div class="lhd"><span></span><span>Mark</span><span>Tag</span><span>Row</span><span>Card</span></div>'
            + sym + pkg + rel + '</div>'
            '<div class="xr"><div><div class="clab">At rest</div>' + lst(False) + '</div>'
            '<div><div class="clab">Holding ⌥</div>' + lst(True) + '</div></div></div>')


CSS = c_cards.CSS + """
.cboard h1{font:700 30px/1 var(--display)}
.lad{display:flex;flex-direction:column;gap:30px}
.lad .lhd,.lad .lr{display:grid;grid-template-columns:110px 70px 200px 360px 380px;gap:24px;align-items:center}
.lad .lhd span{font:500 11px var(--ui);color:var(--ink4)}
.lad .t{font:500 12.5px var(--ui);color:var(--ink3)}
.lad .tg{display:flex;align-items:center;gap:8px;font:500 13px var(--mono);color:var(--ink0)}
.lad .rw{display:flex;align-items:baseline;gap:10px;min-width:0}
.lad .rw .n{font:500 13px var(--mono);color:var(--ink0)}
.lad .rw .s{font:italic 400 13.5px var(--serif);color:var(--ink3);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.tick{display:inline-block;width:2px;height:22px;background:var(--ink1)}.tick.sm{height:16px}
.xr{display:grid;grid-template-columns:360px 460px;gap:40px}
.xi{display:flex;align-items:baseline;gap:10px;height:30px}
.xi .n{font:500 13px var(--mono);color:var(--ink0)}
.xi .sp{font:italic 400 13.5px var(--serif);color:var(--ink3)}.xi .sp b{font:500 12px var(--ui);color:var(--ink2)}
"""

if __name__ == "__main__":
    print(calm.page("Ladder", 1440, 1040, board(), css=CSS))
