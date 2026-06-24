#!/usr/bin/env bash
# codegraph.sh — helper for loam::ingesting-codebase and loam::syncing-code-graph
#
# Subcommands:
#   index <wiki-root>           Emit JSON of code-ingested entity pages in the wiki
#   walk  <codebase-root>       Emit JSON of candidate code files under the codebase root
#
# JSON shapes:
#   index: [{"source_path":"...","slug":"...","ingested_at":"YYYY-MM-DD","mtime":"YYYY-MM-DD","exists":true}]
#   walk:  [{"path":"...","mtime":"YYYY-MM-DD"}]
#
# No jq dependency. Output is minimal JSON assembled with printf.
# Exit codes: 0 success, 1 bad args, 2 root not found, 3 exclusions file missing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(dirname "$SCRIPT_DIR")"
DEFAULT_EXCLUSIONS="$SKILL_DIR/references/ingestion-exclusions.md"

# ponytail: no jq. Hand-emit JSON with proper escaping of backslashes and quotes.
json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"   # escape backslashes first
  s="${s//\"/\\\"}"   # escape double quotes
  printf '%s' "$s"
}

# Format a Unix epoch timestamp as YYYY-MM-DD
format_date() {
  local epoch="$1"
  if [[ -z "$epoch" || "$epoch" == "0" ]]; then
    printf ''
    return
  fi
  date -d "@$epoch" '+%Y-%m-%d' 2>/dev/null || date -r "$epoch" '+%Y-%m-%d' 2>/dev/null || printf ''
}

usage() {
  cat <<'EOF'
Usage:
  codegraph.sh index <wiki-root>
  codegraph.sh walk  <codebase-root> [--exclusions <exclusions.md>]

  index  — Globs <wiki-root>/entities/*.md, parses front matter (source_path, ingested_at),
           stats each source_path for current mtime, emits JSON sorted by ingested_at desc.

  walk   — Walks <codebase-root> recursively, applies exclusion globs, lists candidate
           code files (by extension) with mtime. --exclusions defaults to the bundled
           references/ingestion-exclusions.md.

Exit codes: 0 ok, 1 bad args, 2 root not found, 3 exclusions file missing.
EOF
  exit 1
}

# --- index subcommand ---

cmd_index() {
  local wiki_root="${1:-}"
  [[ -z "$wiki_root" || ! -d "$wiki_root" ]] && {
    echo "Error: wiki root not found: $wiki_root" >&2; exit 2
  }

  local entities_dir="$wiki_root/entities"
  [[ ! -d "$entities_dir" ]] && {
    echo "[]"; exit 0
  }

  # Collect entity pages
  local entries=()
  local page source_path ingested_at slug mtime_epoch mtime_str exists

  while IFS= read -r -d '' page; do
    # Parse front matter for source_path and ingested_at
    source_path=""
    ingested_at=""

    # Read only the YAML front matter block (between first two --- lines)
    local in_fm=false
    local line
    while IFS= read -r line; do
      if [[ "$line" =~ ^---$ ]]; then
        if $in_fm; then break; else in_fm=true; continue; fi
      fi
      if $in_fm; then
        [[ "$line" =~ ^source_path:[[:space:]]*(.*)$ ]] && source_path="${BASH_REMATCH[1]//\"/}"
        [[ "$line" =~ ^ingested_at:[[:space:]]*(.*)$ ]] && ingested_at="${BASH_REMATCH[1]//\"/}"
      fi
    done < "$page"

    # Skip prose entity pages without code-graph front matter
    [[ -z "$source_path" ]] && continue
    [[ -z "$ingested_at" ]] && continue

    # Derive slug from filename
    slug="$(basename "$page")"
    slug="${slug%.md}"

    # Stat the source file (relative path stored; resolve against the wiki root's parent
    # or the codebase root — but we don't know the codebase root here. The caller knows.
    # For the index, we stat against CWD or leave exists=false if unresolvable.
    # The skill resolves source_path relative to the codebase root, not here.)
    # For now, try the path as-is (absolute or relative to CWD).
    if [[ -f "$source_path" ]]; then
      mtime_epoch=$(stat -c %Y "$source_path" 2>/dev/null || stat -f %m "$source_path" 2>/dev/null || echo 0)
      mtime_str=$(format_date "$mtime_epoch")
      exists="true"
    else
      mtime_str=""
      exists="false"
    fi

    entries+=("$(printf '{"source_path":"%s","slug":"%s","ingested_at":"%s","mtime":"%s","exists":%s}' \
      "$(json_escape "$source_path")" \
      "$(json_escape "$slug")" \
      "$(json_escape "$ingested_at")" \
      "$(json_escape "$mtime_str")" \
      "$exists")")

  done < <(find "$entities_dir" -maxdepth 1 -name '*.md' -print0 2>/dev/null)

  # Emit JSON array
  if [[ ${#entries[@]} -eq 0 ]]; then
    echo "[]"
  else
    local out="["
    local first=true
    for e in "${entries[@]}"; do
      if $first; then out+="$e"; first=false
      else out+=",$e"
      fi
    done
    echo "$out]"
  fi
}

# --- walk subcommand ---

cmd_walk() {
  local codebase_root="${1:-}"
  shift || true

  local exclusions_file="$DEFAULT_EXCLUSIONS"

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --exclusions)
        exclusions_file="$2"; shift 2 ;;
      *) echo "Error: unknown flag: $1" >&2; exit 1 ;;
    esac
  done

  [[ -z "$codebase_root" || ! -d "$codebase_root" ]] && {
    echo "Error: codebase root not found: $codebase_root" >&2; exit 2
  }
  [[ ! -f "$exclusions_file" ]] && {
    echo "Error: exclusions file not found: $exclusions_file" >&2; exit 3
  }

  # Parse exclusions file: collect exclude globs and include extensions
  local -a exclude_patterns=()
  local -a include_exts=()

  local section=""
  local in_code=false
  local line
  while IFS= read -r line || [[ -n "$line" ]]; do
    # Strip inline comments — but NOT markdown headings (## ...).
    # Only strip lines where # is preceded by non-#, non-whitespace (a real comment).
    # Simpler: check if line starts with ## — it's a heading, skip comment strip.
    if [[ "$line" =~ ^## ]]; then
      : # heading — don't strip
    else
      line="${line%%#*}"
    fi
    # Trim whitespace
    line="${line#"${line%%[![:space:]]*}"}"   # ltrim
    line="${line%"${line##*[![:space:]]}"}"   # rtrim
    [[ -z "$line" ]] && continue

    # Toggle code-block state (line is exactly three backticks)
    if [[ "$line" == '```' ]]; then
      if $in_code; then in_code=false; else in_code=true; fi
      continue
    fi

    # Section headers (## ...) are always processed (they appear outside code blocks)
    if [[ "$line" =~ ^##[[:space:]]*(.*)$ ]]; then
      section="${BASH_REMATCH[1]}"
      continue
    fi

    # Pattern and extension lines are inside code blocks
    $in_code || continue

    if [[ "$section" == *"Include"* ]]; then
      for ext in $line; do
        ext="${ext//./}"
        [[ -n "$ext" ]] && include_exts+=("$ext")
      done
    else
      exclude_patterns+=("$line")
    fi

  done < "$exclusions_file"

  # Build find command with exclusions and extension filter
  # ponytail: find -prune for excluded dirs is faster than post-filtering,
  # but handling ~40 glob patterns via -path is unwieldy. Post-filter in bash.
  # For large repos this is O(n*patterns) but simpler and correct.

  local -a find_ext_args=()
  for ext in "${include_exts[@]}"; do
    find_ext_args+=(-o -name "*.$ext")
  done

  local entries=()
  local file rel_path mtime_epoch mtime_str skip

  while IFS= read -r -d '' file; do
    rel_path="${file#"$codebase_root"/}"
    skip=false

    # Apply exclusion patterns (shell glob, ** converted to *)
    # Match against both full path and basename for flexibility
    skip=false
    local basename="$(basename "$rel_path")"
    for pat in "${exclude_patterns[@]}"; do
      [[ -z "$pat" ]] && continue
      local match_pat="${pat//\*\*/\*}"
      # shellcheck disable=SC2254
      case "$rel_path" in
        $match_pat) skip=true; break ;;
      esac
      if ! $skip; then
      # shellcheck disable=SC2254
      case "$basename" in
        $match_pat) skip=true; break ;;
      esac
      fi
    done

    $skip && continue

    mtime_epoch=$(stat -c %Y "$file" 2>/dev/null || stat -f %m "$file" 2>/dev/null || echo 0)
    mtime_str=$(format_date "$mtime_epoch")

    entries+=("$(printf '{"path":"%s","mtime":"%s"}' \
      "$(json_escape "$rel_path")" \
      "$(json_escape "$mtime_str")")")

  done < <(find "$codebase_root" -type f \( -false "${find_ext_args[@]}" \) -print0 2>/dev/null)

  if [[ ${#entries[@]} -eq 0 ]]; then
    echo "[]"
  else
    local out="["
    local first=true
    for e in "${entries[@]}"; do
      if $first; then out+="$e"; first=false
      else out+=",$e"
      fi
    done
    echo "$out]"
  fi
}

# --- main ---

main() {
  local subcmd="${1:-}"
  shift || true

  case "$subcmd" in
    index) cmd_index "$@" ;;
    walk)  cmd_walk "$@" ;;
    *)     usage ;;
  esac
}

main "$@"