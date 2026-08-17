#!/usr/bin/env bash
# Measures every figure published on jodd.bbmedia.co.th/case-study.html and in
# public-mirror-prep/ENGINEERING-PRACTICE.md.
#
# The case-study page's whole asset is that its claims survive inspection, so
# no figure ships unless this script reproduces it. Run before publishing, and
# again whenever the page is updated.
#
#   bash scripts/showcase-metrics.sh            # print figures
#   bash scripts/showcase-metrics.sh --check    # diff against the baseline
set -euo pipefail
cd "$(dirname "$0")/.."

# Emitted in place of a real measurement when the input a figure depends on
# is not present in this checkout (e.g. CLAUDE.md and docs/superpowers/plans/
# are deliberately stripped by sync-to-public.sh's EXCLUDES before the public
# snapshot ships). A silent `0` would read as a real measurement — this
# document's whole premise is that every figure can be checked, so a reader
# who can't reproduce a row needs to be told that plainly, not handed a
# number that looks like one they could have gotten wrong. All-caps and
# parenthetical on purpose: it must be impossible to mistake for a count.
UNAVAILABLE_MARKER="UNAVAILABLE (input not present in this checkout)"

measure() {
  first=$(git log --reverse --format=%ad --date=short | head -1)
  last=$(git log -1 --format=%ad --date=short)

  emit() { printf '%s: %s\n' "$1" "$2"; }

  emit commits      "$(git rev-list --count HEAD)"
  emit first_commit "$first"
  emit last_commit  "$last"
  # Epoch seconds for a YYYY-MM-DD date at local midnight.
  #
  # Two independent hazards are handled here.
  #
  # 1. Portability. BSD/macOS `date` needs `-j -f <format>`; GNU/coreutils
  #    `date` has no `-j` at all and parses with `-d`. There is no syntax
  #    that works on both, so try BSD, fall back to GNU, and fail loudly if
  #    neither works — rather than emitting a silently wrong span. This
  #    script is cited by ENGINEERING-PRACTICE.md as the way to re-derive
  #    the published figures, and that invitation is to Linux readers too.
  # 2. The pinned "00:00:00". BSD `date -j -f` fills in *today's* current
  #    wall-clock time for any field the format string omits, and the two
  #    calls below run in separate subshells — if the process clock crosses
  #    a second boundary between them, the subtraction loses a second and
  #    the /86400 floor-divide rounds down a whole day. Without the pin,
  #    days_span is observed to flip between two values on different runs
  #    with no repo change in between.
  epoch_of() {
    date -j -f "%Y-%m-%d %H:%M:%S" "$1 00:00:00" +%s 2>/dev/null && return 0
    date -d "$1 00:00:00" +%s 2>/dev/null && return 0
    echo "showcase-metrics: no usable 'date' syntax (tried BSD -j -f and GNU -d)" >&2
    return 1
  }
  emit days_span    "$(( ( $(epoch_of "$last") - $(epoch_of "$first") ) / 86400 ))"
  emit version      "$(node -p "require('./package.json').version")"
  emit tags         "$(git tag | grep -c '^v')"

  emit rust_lines   "$(find src-tauri/src jodd-mcp/src -name '*.rs' -exec cat {} + | wc -l | tr -d ' ')"
  emit web_lines    "$(find src \( -name '*.svelte' -o -name '*.ts' \) -not -name '*.test.ts' -exec cat {} + | wc -l | tr -d ' ')"

  # Safe from the set -e / zero-match-grep abort described above the --check
  # loop ONLY because this whole function's body runs inside the subshell
  # created by `output=$(measure)` at the bottom of this file: bash does not
  # propagate `errexit` into a `$(...)` subshell unless `inherit_errexit` is
  # set (it is not, here), so a failing pipeline here would be absorbed
  # silently rather than aborting the script — verified empirically, not
  # assumed. Moving these two lines to the top level (outside `measure`)
  # would remove that accidental protection and reintroduce the same class
  # of bug Finding 2 fixed in the --check loop below.
  rust_tests=$(grep -rhE '^\s*#\[(tokio::)?test\]' src-tauri/src jodd-mcp/src | wc -l | tr -d ' ')
  vitest_tests=$(find src -name '*.test.ts' -exec grep -hE '^\s*(it|test)\(' {} + | wc -l | tr -d ' ')
  emit rust_tests   "$rust_tests"
  emit vitest_tests "$vitest_tests"
  emit tests_total  "$(( rust_tests + vitest_tests ))"

  emit specs     "$(git ls-files 'docs/superpowers/specs/*.md' | wc -l | tr -d ' ')"
  # docs/superpowers/plans/ is one of sync-to-public.sh's EXCLUDES, so it is
  # absent by design in the public snapshot — that's a different fact than
  # "this repo has zero plans," and collapsing the two would make the public
  # script silently under-report instead of admitting it can't see the input.
  if [ -d docs/superpowers/plans ]; then
    emit plans   "$(git ls-files 'docs/superpowers/plans/*.md' | wc -l | tr -d ' ')"
  else
    emit plans   "$UNAVAILABLE_MARKER"
  fi
  emit docs      "$(git ls-files docs/ | grep -c '\.md$')"
  # CLAUDE.md is also stripped from the public snapshot — same reasoning as
  # plans above. Checking for the file's existence (not "am I in the public
  # repo") keeps this testable by anyone: move CLAUDE.md aside locally and
  # the marker appears, put it back and the real count returns.
  if [ -f CLAUDE.md ]; then
    emit gotchas "$(awk '/^## Gotchas that still bite/,/^## Open defects/' CLAUDE.md | grep -cE '^[0-9]+\. \*\*')"
  else
    emit gotchas "$UNAVAILABLE_MARKER"
  fi
  emit workflows "$(ls .github/workflows/*.yml | wc -l | tr -d ' ')"
  # Deliberately NOT derived from a measurement — there is no single file or
  # command that enumerates "backend verticals implemented" the way there is
  # for tests or docs. This is a hand-maintained literal. If a fourth backend
  # ships (see CLAUDE.md roadmap #4/#5), this line must be updated by hand;
  # nothing here will catch a stale value.
  emit backends  3
}

output=$(measure)
printf '%s\n' "$output"

if [ "${1:-}" = "--check" ]; then
  echo "--- checking against scripts/showcase-metrics.expected ---" >&2
  missing=0
  unavailable=0
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    case "$line" in \#*) continue ;; esac
    key=${line%%:*}
    want=$(printf '%s' "${line#*: }")
    # `|| true` matters: this while loop runs at the script's top level (not
    # inside the measure() subshell), so under `set -e` a zero-match `grep`
    # here would silently abort the whole script mid-loop — no DRIFT line,
    # no explanation, remaining keys never checked. See Finding 2.
    line_match=$(printf '%s\n' "$output" | grep "^$key: " || true)
    if [ -z "$line_match" ]; then
      printf 'DRIFT %s: key not found in measurement (baseline expects %s)\n' "$key" "$want" >&2
      missing=1
      continue
    fi
    got=$(printf '%s\n' "$line_match" | sed "s/^$key: //")
    # An unavailable input is not a drift — the underlying figure hasn't
    # changed, this checkout just can't re-derive it (see UNAVAILABLE_MARKER
    # above). It must not fall into the != branch below: that would print
    # "expected 13, measured UNAVAILABLE (...)" which reads exactly like the
    # figure moved, when nothing did. It also must not be skipped silently —
    # that would let "all figures match the baseline" print even though two
    # of them were never actually checked. So: its own message, its own
    # counter, and deliberately NOT added to `missing` — running --check from
    # a checkout that's missing these inputs by design (the public snapshot)
    # is an expected state, not a failure to flag with a non-zero exit.
    if [ "$got" = "$UNAVAILABLE_MARKER" ]; then
      printf 'UNAVAILABLE %s: input not present in this checkout; baseline value %s was not verified\n' "$key" "$want" >&2
      unavailable=$((unavailable + 1))
      continue
    fi
    if [ "$got" != "$want" ]; then
      printf 'DRIFT %s: expected %s, measured %s\n' "$key" "$want" "$got" >&2
      missing=1
    fi
  done < scripts/showcase-metrics.expected
  if [ "$missing" -eq 0 ] && [ "$unavailable" -eq 0 ]; then
    echo "all figures match the baseline" >&2
  elif [ "$missing" -eq 0 ]; then
    echo "$unavailable figure(s) unavailable in this checkout (not verified); remaining figures match the baseline" >&2
  fi
  exit "$missing"
fi
