"""filmsheet.py <out-name> <cols> <label:path>...  — an HTML contact sheet (snap it with snap.sh)."""
import sys, os
name, cols, items = sys.argv[1], int(sys.argv[2]), sys.argv[3:]
cells = "".join(f'<figure><img src="{p}"><figcaption>{l}</figcaption></figure>' for l, p in (i.split(":", 1) for i in items))
open(os.path.join(os.path.dirname(os.path.abspath(__file__)), name + ".html"), "w").write(f"""<!doctype html><html><head><meta charset=utf-8><style>
body{{margin:0;background:#02060e;font:500 13px ui-monospace,monospace;color:#74819a}}
.g{{display:grid;grid-template-columns:repeat({cols},1fr);gap:10px;padding:10px}}
figure{{margin:0}} img{{width:100%;display:block}} figcaption{{padding:5px 2px}}</style></head><body><div class=g>{cells}</div></body></html>""")
