#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

failures=0
versions_file="${TMPDIR:-/tmp}/loam-curation-versions.$$"
trap 'rm -f "$versions_file"' EXIT
: > "$versions_file"

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  failures=$((failures + 1))
}

readme_count="$(sed -n 's/^\([0-9][0-9]*\) skills,.*/\1/p' README.md | sed -n '1p')"
plugin_count="$(grep -c '"\./skills/' .claude-plugin/plugin.json || true)"
disk_count="$(find skills -type f -name SKILL.md | wc -l | tr -d ' ')"

if [ -z "$readme_count" ]; then
  readme_count="missing"
fi

if [ "$readme_count" != "$plugin_count" ] || [ "$plugin_count" != "$disk_count" ]; then
  fail "skill count mismatch: README.md count ($readme_count) != .claude-plugin/plugin.json count ($plugin_count) != disk SKILL.md count ($disk_count)"
fi

metadata_version() {
  awk '
    BEGIN { fm = 0; meta = 0 }
    /^---[[:space:]]*$/ { fm++; if (fm == 2) exit }
    fm == 1 && /^metadata:[[:space:]]*$/ { meta = 1; next }
    fm == 1 && meta && /^[^[:space:]]/ { meta = 0 }
    fm == 1 && meta && /^[[:space:]]+version:[[:space:]]*/ {
      v = $0
      sub(/^[[:space:]]+version:[[:space:]]*/, "", v)
      gsub(/"/, "", v)
      print v
      found = 1
      exit
    }
    END { exit found ? 0 : 1 }
  ' "$1"
}

while IFS= read -r skill; do
  if ! version="$(metadata_version "$skill")"; then
    fail "missing metadata.version: $skill"
    continue
  fi
  printf '%s\n' "$version" >> "$versions_file"
done < <(find skills -type f -name SKILL.md | sort)

awk '
  function cmp(a, b,   aa, bb, i, av, bv) {
    split(a, aa, ".")
    split(b, bb, ".")
    for (i = 1; i <= 3; i++) {
      av = aa[i] + 0
      bv = bb[i] + 0
      if (av < bv) return -1
      if (av > bv) return 1
    }
    return 0
  }
  NF {
    if (!seen) { min = $1; max = $1; seen = 1 }
    if (cmp($1, min) < 0) min = $1
    if (cmp($1, max) > 0) max = $1
  }
  END {
    if (seen) printf "INFO: metadata.version spread: min=%s max=%s range=%s..%s\n", min, max, min, max
    else print "INFO: metadata.version spread: none"
  }
' "$versions_file"

find skills -type f -name SKILL.md | sort | while IFS= read -r skill; do
  last="$(git log -1 --format=%cd --date=short -- "$skill" 2>/dev/null || true)"
  if [ -z "$last" ]; then
    last="untracked"
  fi
  printf 'INFO: git freshness: %s %s\n' "$last" "$skill"
done

if [ "$failures" -ne 0 ]; then
  exit 1
fi

printf 'OK: curation checks passed\n'
