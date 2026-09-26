"""sheet.py OUT.html title img1:label1 img2:label2 ... -> an HTML contact sheet (screenshot it)."""
import sys, os
out, title, *items = sys.argv[1:]
cells = []
for it in items:
    path, lab = it.split(":", 1)
    cells.append(f'<figure><img src="{path}"><figcaption>{lab}</figcaption></figure>')
html = f"""<!doctype html><html><head><meta charset=utf-8><style>
body{{margin:0;background:#02050d;color:#9aa6ba;font:13px/1.4 -apple-system,sans-serif;padding:24px}}
h1{{font:600 18px/1 sans-serif;color:#f5f7fb;margin:0 0 16px}}
.g{{display:flex;flex-wrap:wrap;gap:20px;align-items:flex-start}}
figure{{margin:0;display:flex;flex-direction:column;gap:6px}} img{{display:block;border:1px solid #1c304f}}
figcaption{{font:12px/1.3 ui-monospace,monospace;color:#d2d9e5}}
</style></head><body><h1>{title}</h1><div class=g>{''.join(cells)}</div></body></html>"""
open(out, "w").write(html)
print(out)
