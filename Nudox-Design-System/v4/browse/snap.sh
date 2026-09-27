#!/bin/bash
# snap.sh <out-name> <W> <H> [query]  — screenshots browse/Browse.html?query into ../shots/browse/<out-name>.png
D="$(cd "$(dirname "$0")" && pwd)"; N=$1; W=$2; H=$3; QS=${4:-}
OUT="$D/../shots/browse/$N.png"; mkdir -p "$D/../shots/browse"
if ! curl -s -o /dev/null "http://127.0.0.1:47811/v4/browse/Browse.html"; then (cd "$D/../.." && nohup python3 -m http.server 47811 --bind 127.0.0.1 >/dev/null 2>&1 &); sleep 1; fi
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --window-size=$W,$H --virtual-time-budget=${WAIT:-4000} --screenshot="$OUT" "http://127.0.0.1:47811/v4/browse/Browse.html?still=1&w=$W&$QS" >/dev/null 2>&1
echo "$OUT"
