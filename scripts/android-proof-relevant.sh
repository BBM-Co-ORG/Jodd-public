#!/usr/bin/env bash
# Does a pull request change anything the Android encryption proof proves?
#
#   scripts/android-proof-relevant.sh <base-sha> <head-sha>   → prints true|false
#
# The workflow's `paths:` filter already narrows a PR to the files the proof
# covers, but a release-prep PR touches two of them, src-tauri/Cargo.toml and
# Cargo.lock, only to change Jodd's own version line. The proof cannot fail
# differently over a version string, and the tag push that follows runs it
# anyway through release.yml (which gates publication on it), so running it on
# that PR is ~20 runner minutes spent on nothing.
#
# So: compare each watched file at the merge base and at the head, with Jodd's
# OWN package version masked out, and only that one line. A dependency bump
# also edits `version = …` lines in Cargo.lock, but under another package's
# `name =` and with a changed `checksum =`, so it still counts as relevant.
# Anything else in the watched paths (db_crypto.rs, backend/ssh/**, the
# workflow itself) is always relevant.
set -euo pipefail

base=$(git merge-base "$1" "$2")
head=$2

# Each mask rewrites exactly one line: Jodd's package version.
mask_cargo_toml='!done && /^version = /{print "version = <jodd>"; done=1; next} {print}'
mask_cargo_lock='prev == "name = \"jodd\"" && /^version = /{print "version = <jodd>"; prev=""; next} {prev=$0; print}'

masked() { # <rev> <path> <awk-program> — a file missing at <rev> reads as empty
  git show "$1:$2" 2>/dev/null | awk "$3" || true
}

changed=$(git diff --name-only "$base" "$head" -- \
  src-tauri/src/db_crypto.rs 'src-tauri/src/backend/ssh/' \
  src-tauri/Cargo.toml Cargo.lock .github/workflows/android-encryption.yml)

for f in $changed; do
  case "$f" in
    src-tauri/Cargo.toml) mask=$mask_cargo_toml ;;
    Cargo.lock)           mask=$mask_cargo_lock ;;
    *) echo true; exit 0 ;;
  esac
  if [ "$(masked "$base" "$f" "$mask")" != "$(masked "$head" "$f" "$mask")" ]; then
    echo true; exit 0
  fi
done
echo false
