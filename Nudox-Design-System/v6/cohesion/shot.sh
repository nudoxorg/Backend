#!/bin/sh
# shot.sh <name> "<query>" [w] [h]: one still of Cohesion.html into .local/lanes/wave6/lead/cohesion/<name>.png
OUT=/Users/mileswirht/Downloads/backend/.local/lanes/wave6/lead/cohesion; mkdir -p $OUT
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --window-size=${3:-1488},${4:-1010} --virtual-time-budget=5000 --screenshot=$OUT/$1.png "http://127.0.0.1:47811/v6/cohesion/Cohesion.html?$2" >/dev/null 2>&1 </dev/null
echo $OUT/$1.png
