#!/bin/sh
# shot.sh <state> [width] [height]: one still of Gems.html into .local/lanes/wave6/lead/gems/<state>.png
OUT=/Users/mileswirht/Downloads/backend/.local/lanes/wave6/lead/gems; mkdir -p $OUT
W=${2:-1488}; H=${3:-1060}
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
  --window-size=$W,$H --virtual-time-budget=4000 --screenshot=$OUT/$1.png "http://127.0.0.1:47811/v6/gems/Gems.html?state=$1" >/dev/null 2>&1
echo $OUT/$1.png
