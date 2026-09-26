# FACET v4 targets (calm)

Rendered by the lead; implement against these pictures. Rules: `docs/architecture/gui-plan.md` §6.2 (restraint).
Regenerate with `python3 c_<name>.py && ./snap.sh <Board> <W> <H>` (shared frame and CSS: `calm.py`).

| Board | Shows |
|---|---|
| `shots/SymbolPage.png` | hero (gem, name, lede, one facts line), tabs, signature, the rose (monochrome at rest), Made of / Does rows (mark + name + one sentence; the hovered row reveals compass + uses) |
| `shots/SymbolPage-rose.png` | resting on a rose direction lights that direction only |
| `shots/flow-{2560,1440,1100,760,480}.png`, `flow-1440-200pct.png`, `flow-sheet.png` | the same page across widths and 200 % text: shelf → 42 px spine below 900, gone below 640, rose → four-line list at ≤ 560 reader width |
| `shots/density-{comfortable,compact,dense}.png` | the three densities |
| `shots/Peeks.png` | the peek at rest (mark, name, where, signature, one sentence, your uses), ⌘ held (key foot), ⌥ held (compass bar + file comb), chained child with crumb + focus bevel, pinned column, package / location / version peeks |
| `shots/Lenses.png` | release lens (headline, the line about your code first, three items, "and five more", connector to its tick), module lens, language lens |
| `shots/Ladder.png` | one thing at four rungs; ⌥ spelling a list in place |
| `shots/PackagePage.png`, `PackagePage-900.png` | hero, one facts line, the release comb with one catch-up line, tabs, "start with", the territory (monochrome; only stones your code reaches are lit) |
| `shots/Orbit4.png`, `Orbit4-760.png` | your projects at the centre, two rings of names, one resume line, one "new release" line, Map / List |
| `shots/Source4.png`, `Source4-xray.png`, `Source4-760.png`, `Source4-480.png` | the open item with neighbours folded to one line each, mint ticks on newest-release lines, one facts line, doc + callers in the margin; ⌥ spells ages and fold facts |
| `shots/Ask4.png`, `Ask4-760.png`, `Ask4-480.png` | one list, one reason per row, a short preview; narrow: signature under the selected row |
| `shots/Settings4.png`, `Settings4-480.png` | Appearance: theme, contrast, density, text size (comb slider), motion — one control per row; the window is the preview |
| `shots/VersionComb.png` | the shelf header's release slider: rest, hover (tip below), scrubbed ("viewing X · you pin Y", esc), 400 releases as a band, the pointer's fisheye lens |
| **Live** `Graph.html` (`graph/`) | the world graph and the symbol page over the real workspace (53 k symbols, 166 k relations): open `http://127.0.0.1:47811/v4/Graph.html`. Regenerate data with `node graph/extract.mjs && node graph/layout.mjs`; page with `python3 c_graph.py`; stills with `./gsnap.sh out.png W H "focus=RelationLabel"` (`cam=x,y,w`, `frame=pkg:NAME`, `hover=`, `fly=A,B&t=`, `page=`, `view=code`) |
| `shots/graph/g-*.png` | world (faceted territories, your code in mint), package and module zoom, hover peek, the focus prism (left: what it comes from, right: what it goes into; proxies tethered home) |
| `shots/graph/FlightA.png`, `FlightB.png` | van Wijk–Nuij camera flights: world → symbol; symbol → a symbol in another package (out, across, in, gather) |
| `shots/graph/p-*.png` | the symbol page: hero, anatomy (fork / holds / pipe / contract), "can" in plain words, the prism as flow, Does with look-alike folding; 760 and 480 |
| `shots/rejected/` | the first v4 set, rejected as cluttered. What not to do. |
