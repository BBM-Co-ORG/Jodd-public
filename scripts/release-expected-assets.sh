#!/usr/bin/env bash
#
# release-expected-assets.sh — print, one per line, the asset names a Jodd
# release must carry. One list, read by both halves of the release pipeline,
# so they cannot disagree about it:
#   - release.yml's `stage` job (Jodd-public) checks the draft holds exactly
#     the six desktop + jodd-mcp assets;
#   - release-sign.yml (private) checks all seven, the signed APK included,
#     before it publishes.
#
# Usage: scripts/release-expected-assets.sh <version> <release-tag> [--with-apk]
#   version      0.39.0  — tauri-action names the desktop bundles with it
#   release-tag  v0.39.0 (or v0.39.0-dryrun.12) — the jodd-mcp archives and the
#                APK are named with the tag
#
# The macOS updater archive has no version in its name; that is tauri-action's
# naming, and the public download docs link to it as is.
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 || ( $# -eq 3 && "$3" != --with-apk ) ]]; then
  echo "usage: $0 <version> <release-tag> [--with-apk]" >&2
  exit 2
fi
version="$1"
tag="$2"

cat <<EOF
Jodd_${version}_aarch64.dmg
Jodd_aarch64.app.tar.gz
Jodd_${version}_x64-setup.exe
Jodd_${version}_x64_en-US.msi
jodd-mcp_${tag}_aarch64-apple-darwin.tar.gz
jodd-mcp_${tag}_x86_64-pc-windows-msvc.zip
EOF
if [[ "${3:-}" == --with-apk ]]; then
  echo "Jodd_${tag}_android-universal.apk"
fi
