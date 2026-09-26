"""The world graph: a live prototype over the real workspace (graph/world.js, graph/app.js).

Regenerate the data with `node graph/extract.mjs && node graph/layout.mjs`, the page
with `python3 c_graph.py`. Open http://127.0.0.1:47811/v4/Graph.html (snap.sh serves it).
URL state for captures: ?cam=x,y,w  ?focus=Name  ?hover=Name  ?fly=A,B&t=0.4  ?still=1
"""
import os
import re

import json

from nx4 import FACES, ICONS, FAM, inner, tpl, kind, ico, esc
import calm

HERE = os.path.dirname(os.path.abspath(__file__))


def views(on="graph"):
    """The altimeter returns as the view switch: graph (altitude) · page · code."""
    out = []
    for v, label in (("graph", "Graph"), ("page", "Page"), ("code", "Code")):
        cls = "on" if v == on else ""
        out.append(f'<button class="{cls}" data-view="{v}"><i class="d"></i><span>{label}</span></button>')
    return '<nav class="alt views" aria-label="View">' + "".join(out) + "</nav>"


def titlebar():
    t = calm.titlebar(here_kind="enum", here="RelationLabel", path="present › glyph")
    i = t.index('<div class="thread"')
    t = t[:i] + views() + t[i:]
    return t


def build():
    body = f"""<div class="win calm gwin">{tpl("ground")}{titlebar()}
<div class="cbody">
  <main class="gview" id="gview">
    <canvas id="gc"></canvas>
    <div class="gfind" id="gfind">{ico("search", "s14")}<input id="gq" placeholder="Find a symbol" spellcheck="false" autocomplete="off"><kbd>/</kbd></div>
    <div class="gres" id="gres"></div>
    <div class="gwhere" id="gwhere"></div>
    <div class="gpeek" id="gpeek"></div>
    <div class="gfocus" id="gfocus"></div>
    <div class="ghud" id="ghud"></div>
    <div class="gpage" id="gpage"><aside class="cshelf" id="pshelf"></aside><main class="creader" id="preader"></main></div>
  </main>
</div>
<footer class="cstatus"><span class="mono" id="gaddr">nudox://graph</span></footer>
</div>"""
    doc = f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><title>World Graph</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>{FACES}</style>
<link rel="stylesheet" href="facet.css"><link rel="stylesheet" href="main-extra.css"><link rel="stylesheet" href="v4.css">
<link rel="stylesheet" href="graph/graph.css">
<style>{calm.CSS}</style></head>
<body><div class="nx v4 gnx">{body}</div>
<script src="graph/world.js"></script><script src="graph/kinds.js"></script><script src="graph/page.js"></script><script src="graph/app.js"></script></body></html>"""
    kinds = {name: inner(item["svg"]) for (group, name), item in ICONS.items() if group == "kind"}
    open(os.path.join(HERE, "graph", "kinds.js"), "w").write("window.KINDS=" + json.dumps(kinds) + ";window.KFAM=" + json.dumps(FAM) + ";\n")
    path = os.path.join(HERE, "Graph.html")
    open(path, "w").write(doc)
    return path


if __name__ == "__main__":
    print(build())
