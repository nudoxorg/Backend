"""The v4 Settings › Appearance page, calm: one column, one control per row.

The window you are reading is the preview, so the page shows no previews of
its own. Theme, contrast, density and motion are rows of cut stones; text
size is the comb slider. Nothing else.
"""
from nx4 import (tpl, ico, chev, keys, esc, page)

NAV = [("eye", "Appearance", True), ("server", "Index & registries", False), ("key", "Keys", False),
       ("bell", "Notifications", False), ("info", "About", False)]


def titlebar():
    t = tpl("titlebar")
    i, j = t.index('<nav class="alt"'), t.index('<button class="ibtn" aria-label="Trail map"')
    return (t[:i] + f'<div class="thread"><button class="here">{ico("settings", "s14")}<span class="nm">Settings</span>'
            '<span class="path">Appearance</span></button></div>' + t[j:])


def shelf():
    rows = "".join(f'<a href="#" class="li{" on" if on else ""}">{ico(i, "s14")}<span class="grow" style="font:500 13px var(--ui);'
                   f'color:{"var(--ink0)" if on else "var(--ink2)"}">{esc(n)}</span></a>' for i, n, on in NAV)
    return ('<aside class="shelf" style="display:flex"><a href="#" class="shelf-up">' + chev("s12", "transform:rotate(180deg)")
            + 'Nudox</a><div class="pane-scroll col g2" style="padding-top:6px">' + rows + '</div></aside>')


def stones(items, on, swatch=None):
    out = []
    for i, t in enumerate(items):
        sw = f'<i class="sw {swatch[i]}"></i>' if swatch else ""
        out.append(f'<span class="stone{" on" if i == on else ""}">{sw}{esc(t)}</span>')
    return '<div class="stones">' + "".join(out) + '</div>'


def text_size(value=125):
    steps = [85, 90, 100, 110, 125, 150, 175, 200]
    ticks = "".join(
        f'<span class="tk{" on" if s == value else ""}{" past" if s < value else ""}" style="--h:{6 + (s - 85) * 0.1:.1f}px"><i></i></span>'
        for s in steps)
    return (f'<div class="tsize"><span class="a sm">A</span><div class="comb-s">{ticks}</div><span class="a lg">A</span>'
            f'<span class="val">{value} %</span></div>')


def row(label, control):
    return f'<div class="srow"><span class="lab">{esc(label)}</span><div class="ctl">{control}</div></div>'


def folio():
    return ('<div class="sfol"><h1>Appearance</h1>'
            + row("Theme", stones(["System", "Abyss", "Glacier"], 1, ["sys", "abyss", "glacier"]))
            + row("Contrast", stones(["Normal", "High"], 0))
            + row("Density", stones(["Comfortable", "Compact", "Dense"], 0))
            + row("Text size", text_size(125))
            + row("Motion", stones(["System", "Full", "Reduced"], 0))
            + '</div>')


CSS = """
.sfol{max-width:680px;margin:0 auto;padding:clamp(28px,6cqi,72px) clamp(16px,4cqi,40px) 60px;display:flex;flex-direction:column}
.sfol h1{font:640 clamp(26px,calc(18px + 1.2cqi),34px)/1.1 var(--display);letter-spacing:-.03em;margin:0 0 clamp(20px,3cqi,36px);color:var(--ink0)}
.srow{display:flex;align-items:center;gap:24px;min-height:calc(64px*var(--drow))}
.srow+.srow{border-top:1px solid var(--line1)}
.srow .lab{flex:none;width:clamp(96px,22cqi,150px);font:500 14px var(--ui);color:var(--ink1)}
.srow .ctl{flex:1;min-width:0;display:flex;justify-content:flex-end}
.stones{display:flex;gap:4px;flex-wrap:wrap;justify-content:flex-end}
.stone{display:flex;align-items:center;gap:8px;height:30px;padding:0 13px;font:500 12.5px var(--ui);color:var(--ink3);
  clip-path:polygon(7px 0,100% 0,100% calc(100% - 7px),calc(100% - 7px) 100%,0 100%,0 7px)}
.stone.on{background:var(--plate3);color:var(--ink0);box-shadow:inset 1px 1px 0 var(--bevel-hi),inset -1px -1px 0 var(--bevel-lo)}
.stone .sw{width:9px;height:9px;transform:rotate(45deg);flex:none}
.sw.abyss{background:#0b1426;box-shadow:inset 0 0 0 1px #4c5870}
.sw.glacier{background:#eef2f7;box-shadow:inset 0 0 0 1px #a3aec2}
.sw.sys{background:linear-gradient(135deg,#0b1426 50%,#eef2f7 50%);box-shadow:inset 0 0 0 1px #74819a}
.tsize{display:flex;align-items:center;gap:12px;width:100%;max-width:360px}
.tsize .a{font:500 12px var(--ui);color:var(--ink3)}.tsize .a.lg{font-size:19px}
.tsize .val{font:500 12px var(--mono);color:var(--ink1);min-width:40px;text-align:right}
.comb-s{flex:1;display:flex;justify-content:space-between;align-items:flex-end;height:24px;position:relative}
.comb-s .tk{display:flex;flex-direction:column;justify-content:flex-end;height:100%}
.comb-s .tk i{display:block;width:2px;height:var(--h);background:var(--ink4)}
.comb-s .tk.past i{background:var(--ink2)}
.comb-s .tk.on i{width:10px;height:10px;background:var(--mint);transform:rotate(45deg);margin:0 -4px 1px}
@container reader (max-width:560px){
  .srow{flex-direction:column;align-items:stretch;gap:10px;padding:14px 0}
  .srow .ctl{justify-content:flex-start}.stones{justify-content:flex-start}
}
"""


def window(width, height):
    ground = tpl("ground")
    return (f'<div class="win" style="width:{width}px;height:{height}px;position:relative;display:flex;flex-direction:column">'
            f'{ground}{titlebar()}<div class="body">{shelf() if width > 900 else ""}'
            f'<main class="reader" style="overflow:hidden"><div class="reader-scroll">{folio()}</div></main></div>'
            '<footer class="status"><span class="it mono">nudox://settings/appearance</span><span class="grow"></span></footer></div>')


if __name__ == "__main__":
    print(page("Settings4", 1440, 900, window(1440, 900), css=CSS))
    print(page("Settings4-480", 480, 900, window(480, 900), css=CSS))
