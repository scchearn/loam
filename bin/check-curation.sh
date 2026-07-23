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

package_name="$(node -p "JSON.parse(require('fs').readFileSync('package.json', 'utf8')).name")"
[ "$package_name" = '@scchearn/loam' ] || fail "package identity must be @scchearn/loam, got $package_name"
grep -Fq 'npx @scchearn/loam setup' README.md || fail 'README.md must document the public setup command'
node setup/package-check.mjs >/dev/null || fail 'package asset check failed'

for workflow in .github/workflows/*.yml; do
  while IFS= read -r action; do
    [[ "$action" == *'uses: ./'* ]] && continue
    ref="${action##*@}"
    ref="${ref%%[[:space:]#]*}"
    [[ "$ref" =~ ^[0-9a-f]{40}$ ]] || fail "workflow action is not pinned to a commit SHA: $workflow"
  done < <(grep -h 'uses:' "$workflow" 2>/dev/null || true)
done

# Every Windows call site in skill instructions must use in-box Windows
# PowerShell 5.1 through the exact pinned invocation. A bare `.ps1` path or a
# `pwsh` command silently assumes PowerShell 7 is installed.
pinned_powershell='powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File'
while IFS= read -r doc; do
  while IFS= read -r line; do
    case "$line" in
      *"$pinned_powershell"*) continue ;;
      *pwsh*) fail "$doc invokes pwsh; use: $pinned_powershell <script>.ps1" ;;
      *.ps1*) fail "$doc has a bare .ps1 invocation; use: $pinned_powershell <script>.ps1" ;;
    esac
  done < <(grep -n 'pwsh\|\.ps1' "$doc" 2>/dev/null || true)
done < <(find skills -type f -name '*.md' | sort)

# Runtime access in installed instructions is global and must use the injected
# native runtime command. The shared Node integration remains a startup/status
# boundary only. These patterns catch old project-first launchers and paired
# fallback blocks without rejecting ordinary setup documentation.
runtime_docs=(
  "skills/loam-using/SKILL.md"
  "skills/loam-memory/loam-adding-to-memory/SKILL.md"
  "skills/loam-memory/loam-querying-memory/SKILL.md"
  "skills/loam-memory/loam-learning-from-session/SKILL.md"
  "skills/loam-memory/loam-linting-memory/SKILL.md"
  "skills/loam-memory/loam-ingesting-codebase/SKILL.md"
  "skills/loam-memory/loam-syncing-code-graph/SKILL.md"
  "skills/loam-work/loam-resuming/SKILL.md"
  "skills/loam-work/loam-checkpointing/SKILL.md"
)
state_docs=(
  "skills/loam-using/SKILL.md"
  "skills/loam-memory/loam-adding-to-memory/SKILL.md"
  "skills/loam-memory/loam-querying-memory/SKILL.md"
  "skills/loam-memory/loam-learning-from-session/SKILL.md"
  "skills/loam-memory/loam-linting-memory/SKILL.md"
  "skills/loam-memory/loam-ingesting-codebase/SKILL.md"
  "skills/loam-memory/loam-syncing-code-graph/SKILL.md"
  "skills/loam-work/loam-resuming/SKILL.md"
)

for doc in "${runtime_docs[@]}"; do
  [ -f "$doc" ] || { fail "runtime instruction file is missing: $doc"; continue; }
  grep -Fq '<native-runtime-command>' "$doc" \
    || fail "runtime instructions must use the injected native runtime command: $doc"
done

for doc in "${state_docs[@]}"; do
  grep -Fq '<native-runtime-command> state --fast' "$doc" \
    || fail "state refresh must use the direct native runtime command: $doc"
done

for retired in \
  skills/loam-using/scripts/loam.sh \
  skills/loam-using/scripts/loam.ps1 \
  skills/loam-using/scripts/loamstate.sh \
  skills/loam-using/scripts/loamstate.ps1 \
  skills/loam-using/scripts/datecheck.sh \
  skills/loam-using/scripts/datecheck.ps1 \
  skills/loam-memory/loam-ingesting-codebase/scripts/codegraph.sh \
  skills/loam-memory/loam-ingesting-codebase/scripts/codegraph.ps1 \
  skills/loam-work/loam-checkpointing/scripts/checkpoint-state \
  skills/loam-work/loam-checkpointing/scripts/checkpoint-state.ps1 \
  skills/loam-work/loam-checkpointing/scripts/checkpoint-verify \
  skills/loam-work/loam-checkpointing/scripts/checkpoint-verify.ps1; do
  [ ! -e "$retired" ] || fail "retired runtime wrapper still exists: $retired"
done

while IFS= read -r doc; do
  if grep -Eni \
    'loamstate\.(sh|ps1)|loam\.(sh|ps1)|datecheck\.(sh|ps1)|codegraph\.(sh|ps1)|checkpoint-(state|verify)(\.ps1)?|hook --harness|node[^[:cntrl:]]*loam\.mjs[^[:cntrl:]]*run --|(^|[[:space:]])run --|project-first|project-scoped (bootstrap|runtime)|nearest [^[:space:]]* \.agents|git ls-remote|startup[^[:cntrl:]]*(download|install|update|poll)|(^|[[:space:]])\|\|[[:space:]]*powershell\.exe|minimal state|synthetic state|fabricated state' \
    "$doc" >/dev/null; then
    fail "stale project-first/fallback/runtime behavior in production instructions: $doc"
  fi
done < <(find skills -type f -name '*.md' | sort)

if [ -z "$(tail -n 1 README.md)" ]; then
  fail "README.md has a trailing blank line; skill-metrics update must be idempotent"
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

ingest_skill="skills/loam-memory/loam-ingesting-codebase/SKILL.md"
sync_skill="skills/loam-memory/loam-syncing-code-graph/SKILL.md"

for skill in "$ingest_skill" "$sync_skill"; do
  if ! grep -Fq '<wiki root>/code/_index.md' "$skill"; then
    fail "code graph skill must maintain the generated code hub: $skill"
  fi
  if ! grep -Fq '[[code/_index|Code graph]]' "$skill"; then
    fail "code graph skill must link the code hub from root index.md: $skill"
  fi
  if ! grep -Fq 'Do not add individual code pages to root `index.md`.' "$skill"; then
    fail "code graph skill must forbid direct code-page entries in root index.md: $skill"
  fi
done

for reference in \
  "skills/loam-ground/loam-scaffolding-wiki/references/schema-template.md" \
  "skills/loam-ground/loam-scaffolding-wiki/references/wiki-architecture.md"; do
  if ! grep -Fq 'code/_index.md' "$reference"; then
    fail "wiki architecture must document the generated code hub: $reference"
  fi
  if ! grep -Fq '[[code/_index|Code graph]]' "$reference"; then
    fail "wiki architecture must document the root-to-code-hub link: $reference"
  fi
done

lint_skill="skills/loam-memory/loam-linting-memory/SKILL.md"
lint_hub_reference="skills/loam-memory/loam-linting-memory/references/code-hub.md"
if ! grep -Fq 'references/code-hub.md' "$lint_skill"; then
  fail "memory lint must conditionally load the code-hub reference"
fi
if [ ! -f "$lint_hub_reference" ]; then
  fail "memory lint code-hub reference is missing"
elif ! grep -Fq 'Do not add individual code pages to root `index.md`.' "$lint_hub_reference"; then
  fail "memory lint code-hub reference must forbid direct root entries"
fi

if [ "$failures" -ne 0 ]; then
  exit 1
fi

printf 'OK: curation checks passed\n'
