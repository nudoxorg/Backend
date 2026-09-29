#!/bin/sh
# labshot.sh <name> "<query>" [budget-ms] [w] [h]: one still of Lab.html into .local/lanes/wave6/lead/moments/lab/<name>.png
OUT=/Users/mileswirht/Downloads/backend/.local/lanes/wave6/lead/moments/lab; mkdir -p $OUT
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --window-size=${4:-1488},${5:-1400} --virtual-time-budget=${3:-6000} --screenshot=$OUT/$1.png "http://127.0.0.1:47811/v6/moments/Lab.html?$2" >/dev/null 2>&1 </dev/null
echo $OUT/$1.png
