#!/bin/bash
# snap.sh <name> <W> <H> [out]  — screenshots v4/<name>.html
D="$(cd "$(dirname "$0")" && pwd)"; N=$1; W=$2; H=$3; OUT=${4:-$D/shots/$N.png}
mkdir -p "$D/shots"
if ! curl -s -o /dev/null "http://127.0.0.1:47811/v4/$N.html"; then (cd "$D/.." && nohup python3 -m http.server 47811 --bind 127.0.0.1 >/dev/null 2>&1 &); sleep 1; fi
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --window-size=$W,$H --virtual-time-budget=${WAIT:-1500} --screenshot="$OUT" "http://127.0.0.1:47811/v4/$N.html" >/dev/null 2>&1
echo "$OUT"
