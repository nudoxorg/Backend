"""Orbit v4: derived from the v3 Orbit board — no team panels; a live indexing plate; a package peek on a planet."""
import re
from nx4 import page, gem, kind, ico, keys, esc
from b_popups import peek_package

s = open("orbit-body.html").read()
s = s[s.index(">") + 1: s.rindex("</div>")]  # strip the outer .nx wrapper (page() adds its own)
s = re.sub(r'<button class="chip" aria-label="Switch space">.*?</button>\s*<span class="av ">MW</span>', "", s, flags=re.S)
indexing = ('<div class="cut col g10 run" style="--a:130deg"><div class="row g8"><span class="t-micro grow">Indexing</span>'
            '<span class="t-small faint">about 40 s</span></div>'
            f'<div class="row g10">{gem("package", 30, "working")}<div class="col" style="gap:2px;min-width:0">'
            '<span class="hi mono" style="font-size:12.5px">tokio 1.41.0</span>'
            '<span class="t-small dim" style="font-family:var(--serif);font-style:italic;font-size:13px">resolving, stage 4 of 5</span></div></div>'
            '<div class="seam"><i class="done" style="--w:1"></i><i class="done" style="--w:1"></i><i class="done" style="--w:2"></i>'
            '<i class="now" style="--w:3"></i><i style="--w:1"></i></div></div>')
s = re.sub(r'<div class="cut col g8"><span class="t-micro">Platform, right now</span>.*?</div></div>\s*</aside>', indexing + "</aside>", s, flags=re.S)
s = s.replace("Anika left a note where you were reading.", "You read it 3 times this week; it gained a variant.")
s = s.replace('<span class="hi mono" style="font-size:12.5px">RelationLabel</span>', '<span class="hi mono" style="font-size:12.5px">RelationLabel</span>')
s = s.replace('style="left:488px;top:289px"', 'style="left:488px;top:289px" data-hover="1"', 1)
peek = peek_package(540, 250).replace('position:absolute;z-index:60', 'position:absolute;z-index:60')
i = s.find('<div class="float"')
s = s[:i] + peek + s[i:]
css = open("orbit-extra.css").read() + """
.seam{display:flex;gap:3px;height:3px}.seam i{flex:var(--w,1) 1 0;background:var(--line2)}
.seam i.done{background:var(--mint)}.seam i.now{background:repeating-linear-gradient(90deg,var(--mint) 0 3px,transparent 3px 6px)}
.v4>.win{height:900px}
"""
print(page("Orbit4", 1440, 900, s, css=css, title="Orbit v4"))
