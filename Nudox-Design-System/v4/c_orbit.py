"""Calm Orbit: your projects at the centre, what they use on the first ring, what comes along on the second.

Names and gems only; your projects carry the one accent. One quiet line
offers to resume; nothing floats until touched.
"""
import math

from nx4 import gem, esc, ico, lang
import calm

SHELF_ROWS = [("module", "backend", 0, "cur"), ("module", "polyglot"),
              ("package", "serde"), ("package", "serde_json"), ("package", "tokio"), ("package", "anyhow"),
              ("package", "numpy"), ("package", "zod")]

RING1 = ["serde", "tokio", "anyhow", "numpy", "gpui", "tree-sitter", "rusqlite", "syn"]
RING2 = ["serde_json", "ryu", "itoa", "smallvec", "parking_lot", "hashbrown", "libc", "cc", "fmt", "zod", "gin", "jackson"]


def diamond(cx, cy, r):
    return f"M{cx} {cy - r} L{cx + r} {cy} L{cx} {cy + r} L{cx - r} {cy} Z"


def rings(W, H):
    cx, cy = W / 2, H / 2
    r1, r2 = min(W, H) * 0.27, min(W, H) * 0.44
    svg = (f'<svg width="{W}" height="{H}" style="position:absolute;inset:0">'
           f'<path d="{diamond(cx, cy, r1)}" fill="none" stroke="var(--line2)"/>'
           f'<path d="{diamond(cx, cy, r2)}" fill="none" stroke="var(--line1)"/></svg>')
    out = [f'<div class="orb" style="width:{W}px;height:{H}px">{svg}']

    def on_diamond(r, t):
        # t in [0,1): walk the diamond perimeter
        seg, f = divmod(t * 4, 1)
        pts = [(cx, cy - r), (cx + r, cy), (cx, cy + r), (cx - r, cy), (cx, cy - r)]
        (x0, y0), (x1, y1) = pts[int(seg)], pts[int(seg) + 1]
        return x0 + (x1 - x0) * f, y0 + (y1 - y0) * f

    for i, n in enumerate(RING1):
        x, y = on_diamond(r1, (i + 0.5) / len(RING1))
        out.append(f'<span class="pk r1" style="left:{x:.0f}px;top:{y:.0f}px">{gem("package", 22)}<span>{esc(n)}</span></span>')
    for i, n in enumerate(RING2):
        x, y = on_diamond(r2, (i + 0.25) / len(RING2))
        out.append(f'<span class="pk r2" style="left:{x:.0f}px;top:{y:.0f}px"><i></i><span>{esc(n)}</span></span>')
    for dx, n in ((-58, "backend"), (58, "polyglot")):
        out.append(f'<span class="me" style="left:{cx + dx:.0f}px;top:{cy:.0f}px">{gem("module", 44, "glint")}<span>{n}</span></span>')
    out.append('</div>')
    return "".join(out)


CSS = """
.orbwrap{position:absolute;inset:0;display:flex;align-items:center;justify-content:center}
.orb{position:relative}
.orb .pk{position:absolute;transform:translate(-50%,-50%);display:flex;flex-direction:column;align-items:center;gap:5px;
  font:500 11.5px var(--mono);color:var(--ink2);white-space:nowrap}
.orb .pk.r2{color:var(--ink3);font-size:11px}
.orb .pk.r2 i{width:6px;height:6px;transform:rotate(45deg);box-shadow:inset 0 0 0 1px var(--ink4)}
.orb .me{position:absolute;transform:translate(-50%,-50%);display:flex;flex-direction:column;align-items:center;gap:8px;
  font:600 13px var(--ui);color:var(--ink0)}
.orb .me .gem{color:var(--mint)}
.resume{position:absolute;top:22px;right:26px;display:flex;flex-direction:column;align-items:flex-end;gap:6px;font:400 12.5px var(--ui);color:var(--ink3)}
.resume b{font:500 12.5px var(--mono);color:var(--ink1)}
.resume .new b{color:var(--ink1)}.resume .new em{font-style:normal;color:var(--mint);font-weight:600}
.oseg{position:absolute;bottom:22px;left:50%;transform:translateX(-50%);display:flex;gap:2px;font:500 12px var(--ui);color:var(--ink3)}
.oseg span{padding:5px 12px}.oseg span.on{color:var(--ink0);background:var(--plate3);
  clip-path:polygon(6px 0,100% 0,100% calc(100% - 6px),calc(100% - 6px) 100%,0 100%,0 6px)}
"""


def build(name, w, h):
    small = w < 900
    shelf = "" if w < 640 else (calm.kspine(("module", "module", "package", "package", "package"), 0) if small
                                 else calm.shelf(book=("module", "Library", "2 projects · 48 packages"), rows=SHELF_ROWS, up=("", "")))
    tb = calm.titlebar(ask=True)
    rw = w - (0 if w < 640 else 42 if small else 264)
    ow, oh = min(rw - 40, 920), min(h - 140, 720)
    reader = (f'<div class="orbwrap">{rings(ow, oh)}</div>'
              '<div class="resume"><span>Resume <b>Relations of RelationLabel</b> · 12 min ago</span>'
              '<span class="new"><b>tokio 1.41</b> is out · <em>2</em> changes touch your code</span></div>'
              '<div class="oseg"><span class="on">Map</span><span>List</span></div>')
    body = calm.window(w, h, tb, shelf, reader, "nudox://orbit")
    return calm.page(name, w, h, body, css=CSS)


if __name__ == "__main__":
    print(build("Orbit4", 1440, 900))
    print(build("Orbit4-760", 760, 900))
