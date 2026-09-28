#!/bin/bash
# snap.sh <name> <W> <H> "<query>"  — one still of Hand.html into ../shots/hand/<name>.png
D="$(cd "$(dirname "$0")" && pwd)"; OUT="$D/../shots/hand"; mkdir -p "$OUT"
if ! curl -s -o /dev/null "http://127.0.0.1:47811/v4/hand/Hand.html"; then (cd "$D/../.." && nohup python3 -m http.server 47811 --bind 127.0.0.1 >/dev/null 2>&1 &); sleep 1; fi
perl -e 'alarm 150; exec @ARGV' "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --window-size=$2,$3 --virtual-time-budget=${WAIT:-12000} --screenshot="$OUT/$1.png" "http://127.0.0.1:47811/v4/hand/Hand.html?w=$2&h=$3&$4" >/dev/null 2>&1
echo "$OUT/$1.png"
