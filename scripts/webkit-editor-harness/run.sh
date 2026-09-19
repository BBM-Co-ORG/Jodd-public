#!/bin/sh
# Runs every keys/*.txt scenario in real WebKit and diffs the output against
# expected/. macOS only (needs swiftc + WebKit); not part of CI — see README.md.
#
#   sh scripts/webkit-editor-harness/run.sh            # check
#   sh scripts/webkit-editor-harness/run.sh --update   # re-record expected/
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT
swiftc -O "$HERE/harness.swift" -o "$OUT/harness" 2>&1 | grep -v warning || true
[ -x "$OUT/harness" ] || { echo "swiftc failed"; exit 1; }
sh "$HERE/page.sh" "$OUT"
# WebKit suspends requestAnimationFrame in a window it considers hidden, and
# the harness window is off-screen — so that is every run while the display
# sleeps or the screen is locked. The markdown triggers apply in rAF: they
# would silently not fire, and --update would record that as expected.
printf '%s\n' "js requestAnimationFrame(() => { ed.textContent = 'RAF-RAN'; }); 0" "wait 1000" "snap raf" > "$OUT/raf.txt"
if ! "$OUT/harness" "$OUT/page.html" "$OUT/raf.txt" 2>&1 | grep -q 'RAF-RAN'; then
  echo "WebKit is not rendering (requestAnimationFrame never ran) — wake the display or unlock the screen, then re-run."
  exit 2
fi
fail=0
for keys in "$HERE"/keys/*.txt; do
  name=$(basename "$keys" .txt)
  "$OUT/harness" "$OUT/page.html" "$keys" > "$OUT/$name.out" 2>&1 || true
  if [ "$1" = "--update" ]; then
    cp "$OUT/$name.out" "$HERE/expected/$name.txt"
    echo "recorded $name"
  elif diff -u "$HERE/expected/$name.txt" "$OUT/$name.out"; then
    echo "ok       $name"
  else
    echo "CHANGED  $name"; fail=1
  fi
done
exit $fail
