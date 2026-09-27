#!/bin/bash
# stills.sh — every still in v4/shots/hand/ (needs the v4 server on :47811)
D="$(cd "$(dirname "$0")" && pwd)"; S="$D/snap.sh"
run() { "$S" "$@"; }  # one at a time: many headless Chromes under virtual time can deadlock
run rest            1440 900  "state=rest"
run rest5           1440 900  "state=rest5"
run siblings        1440 900  "state=siblings"
run members         1440 900  "state=members"
run keys            1440 900  "state=keys"
run cmd             1440 900  "state=cmd"
run back            1440 900  "state=back"
wait
run first           1440 900  "state=first&t=200"
run first-landed    1440 900  "state=first&t=900"
run hand1           1440 900  "state=hand1"
run hand3           1440 900  "state=hand3"
run hand5           1440 900  "state=hand5"
run hover           1440 900  "state=hover"
run code            1440 900  "state=code"
run drop            1440 900  "state=drop&t=90"
wait
run rest-760        760 900   "state=rest"
run hand5-760       760 900   "state=hand5"
run hand3-760       760 900   "state=hand3"
run siblings-760    760 900   "state=siblings"
run rest-480        480 900   "state=rest"
run seam-480        480 900   "state=seam"
run hand3-480       480 900   "state=hand3"
run back-480        480 900   "state=back"
wait
run rest-200pct     1440 900  "state=rest&text=200"
run hand3-200pct    1440 900  "state=hand3&text=200"
run find            1440 900  "state=find"
run orbit           1440 900  "state=orbit"
run dock            2560 1440 "state=dock"
wait
# the jump bar replaces the trail on every board: the neighbours' boards, calmed in place
run board-symbolpage 1440 1500 "host=../SymbolPage.html"
run board-graph      1440 900  "host=../Graph.html"
run board-source     1440 900  "host=../Source4.html"
run board-source-480 480 900   "host=../Source4-480.html"
wait
