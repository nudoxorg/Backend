#!/bin/bash
# stills.sh — every still in v4/shots/page2/ (needs the v4 server on :47811)
D="$(cd "$(dirname "$0")" && pwd)"; OUT="$D/../shots/page2"; mkdir -p "$OUT"
C="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
U="http://127.0.0.1:47811/v4/page2/Page2.html"
tall() { # name W query  → a still exactly as tall as the page
  local H; H=$("$C" --headless=new --disable-gpu --hide-scrollbars --window-size=$2,1200 --virtual-time-budget=4000 --dump-dom "$U?$3&tall=1" 2>/dev/null | grep -o 'data-h="[0-9]*"' | grep -o '[0-9]*')
  "$C" --headless=new --disable-gpu --hide-scrollbars --window-size=$2,${H:-3000} --virtual-time-budget=4000 --screenshot="$OUT/$1.png" "$U?$3&tall=1" >/dev/null 2>&1; echo "$1 ${2}x${H}"
}
shot() { "$C" --headless=new --disable-gpu --hide-scrollbars --window-size=$2,$3 --virtual-time-budget=4000 --screenshot="$OUT/$1.png" "$U?$4" >/dev/null 2>&1; echo "$1 ${2}x$3"; }
for p in ${PAGES:-value from_str serialize smallvec error}; do
  tall "$p-1440" 1440 "page=$p"
  tall "$p-760" 760 "page=$p"
  tall "$p-480" 480 "page=$p"
  tall "$p-200pct" 1440 "page=$p&t=2"
done
[ -n "$PAGES" ] && exit 0
# the relations presentation: at rest, hovered, at 500, the door
shot rel-rest-value 1440 900 "page=value&scroll=q-who"
shot rel-hover-joint-value 1440 900 "page=value&scroll=q-who&hover=ne:held%20by:0"
shot rel-hover-verb-value 1440 900 "page=value&scroll=q-who&hover=vb:taken%20by"
shot rel-500-rest-serialize 1440 900 "page=serialize&scroll=q-who"
shot rel-500-open-serialize 1440 1000 "page=serialize&scroll=q-who&open=done%20by"
shot rel-500-xray-serialize 1440 900 "page=serialize&scroll=q-who&x=1"
shot rel-door-hover-value 1440 900 "page=value&scroll=q-who&hover=door"
shot rel-door-flight-value-0.5 1440 900 "page=value&scroll=q-who&fly=0.5"
shot rel-door-flight-value-1 1440 900 "page=value&scroll=q-who&fly=1"
shot rel-480-value 480 1000 "page=value&scroll=q-who"
shot rel-200pct-serialize 1440 1000 "page=serialize&scroll=q-who&t=2&open=done%20by"
shot rel-B-scales-serialize 1440 900 "page=serialize&scroll=q-who&rel=b"
shot rel-C-combs-serialize 1440 900 "page=serialize&scroll=q-who&rel=c"
# docs.rs parity states
shot decl-bound-hover-from_str 1440 900 "page=from_str&hover=bd:0"
shot gate-off-smallvec 1440 900 "page=smallvec&scroll=q-do&hover=gate:const_new"
shot gate-unindexed-smallvec 1440 900 "page=smallvec&scroll=q-do&hover=gate:const_new&gatestate=unindexed"
shot caps-usual-value 1440 900 "page=value&scroll=q-do&hover=cap:usual"
shot acts-like-open-smallvec 1440 1100 "page=smallvec&scroll=q-do&open=acts"
shot flip-decl-smallvec 1440 900 "page=smallvec&flip=decl"
shot flip-fail-smallvec 1440 900 "page=smallvec&scroll=q-fail&flip=q-fail"
shot xray-smallvec 1440 1100 "page=smallvec&scroll=q-do&x=1"
shot kinds-hover-error 1440 900 "page=error&scroll=q-fail&hover=kind:Syntax"
shot deprecated-smallvec 1440 900 "page=smallvec&scroll=q-do&hover=cap:ExtendFromSlice"
shot doing-it-serialize 1440 1000 "page=serialize&scroll=q-get"
shot since-hover-value 1440 900 "page=value&hover=since"
shot member-docs-open-smallvec 1440 1000 "page=smallvec&scroll=q-do&pad=-380&open=row:drain,row:with_capacity"
shot usual-open-value 1440 900 "page=value&scroll=q-do&open=usual"
shot who-tree-serialize 1440 1000 "page=serialize&scroll=q-who"
