"""The version comb: the shelf header's release slider (W-Controls `controls::comb`).

At rest: package, version, and a comb of its releases (tall = new minor, short =
patch, one mint tick for the version your lockfile pins). Hover a tick for its
version and age. Scrub (drag, ←/→, PgUp/PgDn a minor, Home/End) to re-scope the
page to that release; one quiet line says so and Esc comes home. Past pixel
density the comb becomes a band, and the pointer magnifies it like a fisheye so
every release stays reachable.
"""
import math

from nx4 import gem, esc
import calm

PRESENT = ["0.1.0", "0.1.1", "0.1.2", "0.1.3", "0.2.0", "0.2.1", "0.2.2", "0.2.3", "0.2.4", "0.3.0", "0.3.1", "0.3.2",
           "0.3.3", "0.3.4", "0.4.0", "0.4.1", "0.4.2"]


def many():
    out = []
    for mi in range(0, 41):
        for pa in range(0, [9, 12, 6, 14, 8, 11][mi % 6]):
            out.append(f"1.{mi}.{pa}")
    return out


def tick_h(v):
    ma, mi, pa = (int(x) for x in v.split("."))
    return 18 if mi == 0 and pa == 0 else 12 if pa == 0 else 6


def comb(versions, width, pin, view=None, hover=None, fisheye=None):
    n = len(versions)
    xs = []
    if fisheye is None:
        step = width / max(1, n - 1)
        xs = [i * step for i in range(n)]
    else:  # a focus+context lens: spacing grows near the pointer, the ends stay put
        c, r, mag = fisheye

        def warp(x):
            d = x - c
            if abs(d) > r:
                return x
            t = d / r
            return c + r * (math.copysign(1, t) * (1 - (1 - abs(t)) ** mag))
        step = width / max(1, n - 1)
        xs = [warp(i * step) for i in range(n)]
    parts = []
    for i, (v, x) in enumerate(zip(versions, xs)):
        h = tick_h(v)
        cls = "t"
        if v == pin:
            cls += " pin"; h = 20
        if v == view:
            cls += " view"; h = 22
        if v == hover:
            cls += " hov"
        parts.append(f'<i class="{cls}" style="left:{x:.2f}px;height:{h}px"></i>')
    tip = ""
    if hover:
        x = xs[versions.index(hover)]
        tip = f'<span class="ctip" style="left:{x:.1f}px"><b>{esc(hover)}</b> · 7 months ago</span>'
    return f'<div class="vcomb" style="width:{width}px">{"".join(parts)}{tip}</div>'


def header(name, ver, versions, pin, view=None, hover=None, fisheye=None, width=236, note=True):
    shown = view or ver
    line = ""
    if view and view != pin and note:
        line = (f'<div class="vline">viewing <b>{esc(view)}</b><i>·</i>you pin <span>{esc(pin)}</span>'
                f'<kbd>esc</kbd></div>')
    return (f'<div class="vhead"><div class="cbook">{gem("package", 28)}<div class="col" style="gap:1px;min-width:0">'
            f'<span class="bn">{esc(name)}</span><span class="bv{" on" if view and view != pin else ""}">{esc(shown)}</span></div></div>'
            f'{comb(versions, width, pin, view, hover, fisheye)}{line}</div>')


def board():
    pv = PRESENT
    cells = [
        ("At rest", header("present", "0.4.2", pv, "0.4.2")),
        ("Hovering a release", header("present", "0.4.2", pv, "0.4.2", hover="0.3.0")),
        ("Scrubbed to an older release", header("present", "0.4.2", pv, "0.4.2", view="0.3.0")),
        ("400 releases at rest", header("tokio", "1.40.2", many(), "1.38.1")),
        ("400 releases, pointer over 1.21", header("tokio", "1.40.2", many(), "1.38.1", hover="1.21.0",
                                                  fisheye=(118, 90, 4.2))),
    ]
    out = "".join(f'<div class="vcell"><div class="clab">{esc(t)}</div><div class="vshelf">{h}</div></div>' for t, h in cells)
    return f'<div class="cboard"><h1>The version comb</h1><div class="vgrid">{out}</div></div>'


CSS = """
.cboard{padding:40px 48px;display:flex;flex-direction:column;gap:28px}
.cboard h1{font:700 30px/1 var(--display);color:var(--ink0);margin:0}
.vgrid{display:grid;grid-template-columns:repeat(3,264px);gap:36px 48px}
.vcell .clab{font:500 12px var(--ui);color:var(--ink3);margin-bottom:10px}
.vshelf{width:264px;background:rgba(3,8,18,.55);box-shadow:inset -1px 0 0 var(--line1);padding:8px 0 14px}
.vhead .cbook{padding:6px 14px 10px}
.vhead .bv.on{color:var(--peri-hi)}
.vcomb{position:relative;height:24px;margin:0 14px}
.vcomb .t{position:absolute;bottom:0;width:1px;background:var(--ink4);transform:translateX(-.5px)}
.vcomb .t.pin{background:var(--mint);width:2px}
.vcomb .t.view{background:var(--peri);width:2px;box-shadow:0 0 0 2px rgba(147,162,250,.18)}
.vcomb .t.hov{background:var(--ink0)}
.vcomb .ctip{position:absolute;top:30px;z-index:2;transform:translateX(-50%);white-space:nowrap;font:400 11.5px var(--ui);color:var(--ink2);
  background:var(--glass);padding:5px 9px;box-shadow:inset 1px 1px 0 var(--bevel-hi,rgba(255,255,255,.08))}
.vcomb .ctip b{font:600 11.5px var(--mono);color:var(--ink0)}
.vline{margin:10px 14px 0;font:400 12px var(--ui);color:var(--ink3);display:flex;align-items:center;gap:6px;flex-wrap:wrap}
.vline b{font:600 11.5px var(--mono);color:var(--peri-hi)}.vline span{font:500 11.5px var(--mono);color:var(--mint)}
.vline i{font-style:normal;color:var(--ink4)}
.vline kbd{margin-left:auto;font:500 10.5px var(--mono);color:var(--ink3);box-shadow:inset 0 0 0 1px var(--line2);padding:0 5px}
"""

if __name__ == "__main__":
    print(calm.page("VersionComb", 1000, 560, board(), css=CSS))
