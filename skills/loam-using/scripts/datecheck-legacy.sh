#!/usr/bin/env bash
# datecheck.sh — scan wiki markdown for date-format drift, optionally fix
#
# Checks front matter point-in-time fields (created_at, updated_at, approved_at,
# started_at, completed_at) and checkpoint Captured: lines for missing timezone
# offsets and legacy TZ labels (SAST, GMT+N, UTC).
#
# Also checks decisions-log entries for non-em-dash separators.
#
# Usage:
#   datecheck.sh check <wiki-root>              # report drift as JSON
#   datecheck.sh fix   <wiki-root> [--offset +02:00]  # apply normalizations
#
# Output (check mode): one JSON object per finding, newline-separated:
#   {"file":"path","line":42,"field":"created_at","value":"2026-06-26 11:07","issue":"missing_offset","fix":"add +02:00"}
#
# Exit codes: 0 (no drift / fixes applied), 1 (bad args), 2 (drift found in check mode)
#
# Idempotent: fix mode skips already-canonical values.

set -euo pipefail

# --- helpers ---

json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '%s' "$s"
}

# Point-in-time front matter fields that require TZ offset
TZ_FIELDS_RE='^(created_at|updated_at|approved_at|started_at|completed_at):[[:space:]]+'

# Legacy TZ labels to normalize (must be preceded by space to avoid matching placeholders)
# Z omitted — too many false positives; bare Z is rare in loam wikis
LEGACY_TZ_RE=' (SAST|GMT[+-][0-9]+|UTC|UT)$'

usage() {
  cat <<'EOF'
Usage: datecheck.sh <mode> <wiki-root> [--offset +HH:MM]

Modes:
  check   Report date-format drift as JSON (one object per line). Exit 2 if drift found.
  fix     Apply normalizations in-place. Idempotent.

Options:
  --offset  Timezone offset to add to bare timestamps (default: system local, from `date +%z`)

Checks:
  1. Front matter point-in-time fields missing timezone offset
  2. Front matter fields with legacy TZ labels (SAST, GMT+N, UTC, Z)
  3. Checkpoint "Captured:" lines missing offset or with legacy labels
  4. Decisions-log entries with non-em-dash separators (- or : instead of —)
EOF
  exit 1
}

# --- args ---

[[ $# -lt 2 ]] && usage

MODE="$1"
WIKI_ROOT="$2"
shift 2

OFFSET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --offset) OFFSET="$2"; shift 2 ;;
    *) usage ;;
  esac
done

if [[ -z "$OFFSET" ]]; then
  OFFSET=$(date '+%z' 2>/dev/null || echo '+0000')
  # Convert +0200 → +02:00
  if [[ "$OFFSET" =~ ^([+-])([0-9]{2})([0-9]{2})$ ]]; then
    OFFSET="${BASH_REMATCH[1]}${BASH_REMATCH[2]}:${BASH_REMATCH[3]}"
  fi
fi

[[ ! -d "$WIKI_ROOT" ]] && {
  echo "{\"error\":\"wiki root not found: $WIKI_ROOT\"}"
  exit 1
}

# --- core: scan a single file ---

scan_file() {
  local file="$1"
  local rel="${file#$WIKI_ROOT/}"
  local in_frontmatter=false
  local found_drift=false
  local line_num=0

  while IFS= read -r line || [[ -n "$line" ]]; do
    ((line_num++))

    # Track front matter block (--- delimited)
    if [[ "$line_num" -eq 1 && "$line" == "---" ]]; then
      in_frontmatter=true
      continue
    fi
    if $in_frontmatter && [[ "$line" == "---" ]]; then
      in_frontmatter=false
      continue
    fi

    # Check front matter TZ fields
    if $in_frontmatter; then
      if [[ "$line" =~ $TZ_FIELDS_RE ]]; then
        local field="${line%%:*}"
        local value="${line#*:}"
        value="${value#"${value%%[![:space:]]*}"}" # ltrim

        # Skip null values
        [[ "$value" == "null" || -z "$value" ]] && continue

        # Check for legacy TZ label
        if [[ "$value" =~ $LEGACY_TZ_RE ]]; then
          printf '{"file":"%s","line":%d,"field":"%s","value":"%s","issue":"legacy_tz","fix":"replace with %s"}\n' \
            "$(json_escape "$rel")" "$line_num" "$field" "$(json_escape "$value")" "$OFFSET"
          found_drift=true
        # Check for missing offset (has date-time but no +HH:MM)
        elif [[ "$value" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}[[:space:]]+[0-9]{2}:[0-9]{2}$ ]]; then
          printf '{"file":"%s","line":%d,"field":"%s","value":"%s","issue":"missing_offset","fix":"add %s"}\n' \
            "$(json_escape "$rel")" "$line_num" "$field" "$(json_escape "$value")" "$OFFSET"
          found_drift=true
        fi
      fi
    fi

    # Check Captured: lines (checkpoint body, not front matter)
    if [[ "$line" =~ ^-[[:space:]]*Captured:[[:space:]]+(.*) ]]; then
      local value="${BASH_REMATCH[1]}"
      # Legacy TZ label
      if [[ "$value" =~ $LEGACY_TZ_RE ]]; then
        printf '{"file":"%s","line":%d,"field":"Captured","value":"%s","issue":"legacy_tz","fix":"replace with %s"}\n' \
          "$(json_escape "$rel")" "$line_num" "$(json_escape "$value")" "$OFFSET"
        found_drift=true
      # Missing offset
      elif [[ "$value" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}[[:space:]]+[0-9]{2}:[0-9]{2}$ ]]; then
        printf '{"file":"%s","line":%d,"field":"Captured","value":"%s","issue":"missing_offset","fix":"add %s"}\n' \
          "$(json_escape "$rel")" "$line_num" "$(json_escape "$value")" "$OFFSET"
        found_drift=true
      fi
    fi

    # Check decisions-log entries: should use em-dash (—), not hyphen-minus (-) or colon (:)
    if [[ "$line" =~ ^-[[:space:]]+[0-9]{4}-[0-9]{2}-[0-9]{2}([-:[:space:]]+[[:alnum:]]) ]]; then
      if [[ ! "$line" =~ ^-[[:space:]]+[0-9]{4}-[0-9]{2}-[0-9]{2}[[:space:]]*$'\xe2\x80\x94' ]]; then
        if [[ "$line" =~ ^-[[:space:]]+[0-9]{4}-[0-9]{2}-[0-9]{2}([[:space:]]*[-:][[:space:]]*) ]]; then
          local sep="${BASH_REMATCH[1]}"
          printf '{"file":"%s","line":%d,"field":"decisions_log","value":"%s","issue":"wrong_separator","fix":"use em-dash —"}\n' \
            "$(json_escape "$rel")" "$line_num" "$(json_escape "$sep")"
          found_drift=true
        fi
      fi
    fi

  done < "$file"

  if $found_drift; then
    return 1
  fi
  return 0
}

# fix_file <file> — applies normalizations in-place using sed -i
# Returns 0 if file was changed, 1 if no changes needed
fix_file() {
  local file="$1"
  local rel="${file#$WIKI_ROOT/}"

  # Snapshot before
  local before
  before=$(cat "$file" 2>/dev/null || printf '')

  # Apply all normalizations with sed -i
  # 1. Add offset to bare date-times in front matter fields
  sed -i -E \
    "s/^(created_at|updated_at|approved_at|started_at|completed_at): ([0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2})$/\1: \2 ${OFFSET}/" \
    "$file" 2>/dev/null || true

  # 2. Replace legacy TZ labels in front matter fields
  sed -i -E \
    "s/^(created_at|updated_at|approved_at|started_at|completed_at): ([0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}) (SAST|GMT[+-][0-9]+|UTC|UT|Z)$/\1: \2 ${OFFSET}/" \
    "$file" 2>/dev/null || true

  # 3. Add offset to bare Captured: timestamps
  sed -i -E \
    "s/^(- Captured: [0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2})$/\1 ${OFFSET}/" \
    "$file" 2>/dev/null || true

  # 4. Replace legacy TZ labels in Captured: lines
  sed -i -E \
    "s/^(- Captured: [0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}) (SAST|GMT[+-][0-9]+|UTC|UT|Z)$/\1 ${OFFSET}/" \
    "$file" 2>/dev/null || true

  # 5. Fix decisions-log separators: hyphen-minus → em-dash
  sed -i -E \
    "s/^(- [0-9]{4}-[0-9]{2}-[0-9]{2}) - /\1 — /" \
    "$file" 2>/dev/null || true

  # 6. Fix decisions-log separators: colon → em-dash
  sed -i -E \
    "s/^(- [0-9]{4}-[0-9]{2}-[0-9]{2}): /\1 — /" \
    "$file" 2>/dev/null || true

  # Check if file changed
  local after
  after=$(cat "$file" 2>/dev/null || printf '')

  if [[ "$before" != "$after" ]]; then
    echo "$(json_escape "$rel")"
    return 0
  fi
  return 1
}

# --- main ---

DRIFT_FOUND=false

# Find all markdown files
mapfile -t FILES < <(find "$WIKI_ROOT" -name '*.md' -type f 2>/dev/null | sort)

case "$MODE" in
  check)
    for file in "${FILES[@]}"; do
      if ! scan_file "$file"; then
        DRIFT_FOUND=true
      fi
    done
    if $DRIFT_FOUND; then
      exit 2
    fi
    exit 0
    ;;

  fix)
    FIXED_COUNT=0
    for file in "${FILES[@]}"; do
      # Check first, only fix if drift exists (suppress scan output)
      if ! scan_file "$file" >/dev/null 2>&1; then
        if fix_file "$file"; then
          FIXED_COUNT=$((FIXED_COUNT + 1))
        fi
      fi
    done
    printf '{"mode":"fix","offset":"%s","files_fixed":%d}\n' "$OFFSET" "$FIXED_COUNT"
    exit 0
    ;;

  *)
    usage
    ;;
esac