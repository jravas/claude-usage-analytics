#!/usr/bin/env bash
# Regenerate docs/screenshots/*.jpg from the dev harness with SYNTHETIC data
# only (ui/dev/mock.js), never your own index. Needs Google Chrome.
#
# usage: scripts/screenshots.sh [port]   (default 8731)

set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
port="${1:-8731}"
chrome="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
out="$root/docs/screenshots"
mkdir -p "$out"

python3 -m http.server "$port" --bind 127.0.0.1 --directory "$root/ui" >/dev/null 2>&1 &
server=$!
profile="$(mktemp -d)"
trap 'kill $server 2>/dev/null; rm -rf "$profile"' EXIT
sleep 1

for view in overview session-detail insights project; do
  "$chrome" --headless=new --user-data-dir="$profile/$view" --hide-scrollbars \
    --window-size=1360,880 --force-device-scale-factor=2 --virtual-time-budget=6000 \
    --screenshot="$out/$view.png" \
    "http://127.0.0.1:$port/dev/index.html?data=synthetic&theme=dark&shot=$view" >/dev/null 2>&1
  sips -s format jpeg -s formatOptions 82 "$out/$view.png" --out "$out/$view.jpg" >/dev/null
  rm "$out/$view.png"
  echo "wrote $out/$view.jpg"
done
