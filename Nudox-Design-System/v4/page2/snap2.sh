#!/bin/bash
# snap2.sh <out.png> <W> <H> [query]  — screenshots page2/Page2.html?query
D="$(cd "$(dirname "$0")" && pwd)"; OUT=$1; W=$2; H=$3; QS=${4:-}
if ! curl -s -o /dev/null "http://127.0.0.1:47811/v4/page2/Page2.html"; then (cd "$D/../.." && nohup python3 -m http.server 47811 --bind 127.0.0.1 >/dev/null 2>&1 &); sleep 1; fi
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --window-size=$W,$H --virtual-time-budget=${WAIT:-4000} --screenshot="$OUT" "http://127.0.0.1:47811/v4/page2/Page2.html?$QS" >/dev/null 2>&1
echo "$OUT"
