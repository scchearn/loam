#!/usr/bin/env bash
# codegraph.sh — helper for loam::ingesting-codebase and loam::syncing-code-graph
# No jq dependency. Output is minimal JSON assembled with printf.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(dirname "$SCRIPT_DIR")"
DEFAULT_EXCLUSIONS="$SKILL_DIR/references/ingestion-exclusions.md"
MAX_BYTES=$((500 * 1024))

json_escape() {
  local s="$1"
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  printf '%s' "$s"
}

stat_epoch() {
  stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null || echo 0
}

is_epoch() {
  [[ "$1" =~ ^[0-9]+$ ]]
}

stat_size() {
  stat -c %s "$1" 2>/dev/null || stat -f %z "$1" 2>/dev/null || echo 0
}

validate_wiki_root() {
  local wiki_root="$1"
  [[ -z "$wiki_root" || ! -d "$wiki_root" ]] && { echo "Error: wiki root not found: $wiki_root" >&2; exit 2; }

  if [[ -f "$wiki_root/SCHEMA.md" || -f "$wiki_root/index.md" || -f "$wiki_root/log.md" ]]; then
    return 0
  fi

  if [[ -f "$wiki_root/wiki/SCHEMA.md" || -f "$wiki_root/wiki/index.md" || -f "$wiki_root/wiki/log.md" ]]; then
    echo "Error: wiki root contract not found: $wiki_root; did you mean: $wiki_root/wiki" >&2
    exit 2
  fi

  echo "Error: wiki root contract not found: $wiki_root" >&2
  exit 2
}

usage() {
  cat <<'EOF'
Usage:
  codegraph.sh index <wiki-root> [--codebase-root <codebase-root>]
  codegraph.sh walk  <codebase-root> [--exclusions <exclusions.md>] [--summary] [--no-gitignore]
  codegraph.sh diff  <codebase-root> <wiki-root> [--exclusions <exclusions.md>] [--no-gitignore]

Exit codes: 0 ok, 1 bad args, 2 root not found, 3 exclusions file missing.
EOF
  exit 1
}

declare -a exclude_patterns=()
declare -a include_exts=()

parse_exclusions() {
  local exclusions_file="$1"
  [[ ! -f "$exclusions_file" ]] && { echo "Error: exclusions file not found: $exclusions_file" >&2; exit 3; }

  exclude_patterns=()
  include_exts=()
  local section="" in_code=false line
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" =~ ^## ]]; then :; else line="${line%%#*}"; fi
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line%"${line##*[![:space:]]}"}"
    [[ -z "$line" ]] && continue

    if [[ "$line" == '```' ]]; then
      if $in_code; then in_code=false; else in_code=true; fi
      continue
    fi
    if [[ "$line" =~ ^##[[:space:]]*(.*)$ ]]; then section="${BASH_REMATCH[1]}"; continue; fi
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
}

has_included_extension() {
  local file="$1" ext
  ext="${file##*.}"
  [[ "$ext" == "$file" ]] && return 1
  for candidate in "${include_exts[@]}"; do
    [[ "$ext" == "$candidate" ]] && return 0
  done
  return 1
}

matches_exclusion() {
  local rel_path="$1" basename pat match_pat
  basename="$(basename "$rel_path")"
  for pat in "${exclude_patterns[@]}"; do
    [[ -z "$pat" ]] && continue
    match_pat="${pat//\*\*/\*}"
    # shellcheck disable=SC2254
    case "$rel_path" in $match_pat) return 0 ;; esac
    # shellcheck disable=SC2254
    case "$basename" in $match_pat) return 0 ;; esac
  done
  return 1
}

is_gitignored() {
  local root="$1" rel_path="$2"
  git -C "$root" check-ignore --quiet -- "$rel_path" >/dev/null 2>&1
}

is_generated_header() {
  LC_ALL=C sed -n '1,5p' "$1" 2>/dev/null | grep -Eiq 'generated|auto-generated|do not edit|@generated|Code generated|This file was generated'
}

declare -a walk_paths=()
declare -A walk_mtimes=()
declare -A by_ext=()
excluded_pattern=0
excluded_gitignore=0
excluded_empty=0
excluded_large=0
excluded_generated_header=0
excluded_binary=0

collect_walk() {
  local codebase_root="$1" exclusions_file="$2" respect_gitignore="$3"
  [[ -z "$codebase_root" || ! -d "$codebase_root" ]] && { echo "Error: codebase root not found: $codebase_root" >&2; exit 2; }
  parse_exclusions "$exclusions_file"

  walk_paths=()
  walk_mtimes=()
  by_ext=()
  excluded_pattern=0
  excluded_gitignore=0
  excluded_empty=0
  excluded_large=0
  excluded_generated_header=0
  excluded_binary=0

  local use_gitignore=false
  if [[ "$respect_gitignore" == "true" ]] && command -v git >/dev/null 2>&1 && git -C "$codebase_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    use_gitignore=true
  fi

  local file rel_path ext size mtime_epoch
  while IFS= read -r -d '' file; do
    rel_path="${file#"$codebase_root"/}"
    has_included_extension "$rel_path" || continue

    if matches_exclusion "$rel_path"; then excluded_pattern=$((excluded_pattern + 1)); continue; fi
    if $use_gitignore && is_gitignored "$codebase_root" "$rel_path"; then excluded_gitignore=$((excluded_gitignore + 1)); continue; fi

    size=$(stat_size "$file")
    if [[ "$size" -eq 0 ]]; then excluded_empty=$((excluded_empty + 1)); continue; fi
    if ! LC_ALL=C grep -q '[^[:space:]]' "$file" 2>/dev/null; then excluded_empty=$((excluded_empty + 1)); continue; fi
    if ! LC_ALL=C grep -Iq . "$file" 2>/dev/null; then excluded_binary=$((excluded_binary + 1)); continue; fi
    if [[ "$size" -gt "$MAX_BYTES" ]]; then excluded_large=$((excluded_large + 1)); continue; fi
    if is_generated_header "$file"; then excluded_generated_header=$((excluded_generated_header + 1)); continue; fi

    mtime_epoch=$(stat_epoch "$file")
    walk_paths+=("$rel_path")
    walk_mtimes["$rel_path"]="$mtime_epoch"
    ext="${rel_path##*.}"
    by_ext["$ext"]=$(( ${by_ext["$ext"]:-0} + 1 ))
  done < <(find "$codebase_root" -type f -print0 2>/dev/null)
}

emit_walk_json() {
  local out="[" first=true path
  for path in "${walk_paths[@]}"; do
    if $first; then first=false; else out+=","; fi
    out+="$(printf '{"path":"%s","mtime":"%s"}' "$(json_escape "$path")" "$(json_escape "${walk_mtimes[$path]}")")"
  done
  echo "$out]"
}

emit_summary_json() {
  local out by_ext_json="{" first=true ext
  for ext in "${!by_ext[@]}"; do
    if $first; then first=false; else by_ext_json+=","; fi
    by_ext_json+="$(printf '"%s":%s' "$(json_escape "$ext")" "${by_ext[$ext]}")"
  done
  by_ext_json+="}"
  out=$(printf '{"total":%s,"by_ext":%s,"excluded":{"pattern":%s,"gitignore":%s,"empty":%s,"large":%s,"generated_header":%s,"binary":%s}}' \
    "${#walk_paths[@]}" "$by_ext_json" "$excluded_pattern" "$excluded_gitignore" "$excluded_empty" "$excluded_large" "$excluded_generated_header" "$excluded_binary")
  echo "$out"
}

declare -a index_sources=()
declare -A index_slugs=()
declare -A index_ingested=()
declare -A index_mtimes=()
declare -A index_exists=()

resolve_source_path() {
  local source_path="$1" codebase_root="$2"
  if [[ "$source_path" = /* ]]; then
    printf '%s' "$source_path"
  elif [[ -n "$codebase_root" ]]; then
    printf '%s/%s' "$codebase_root" "$source_path"
  else
    printf '%s' "$source_path"
  fi
}

collect_index() {
  local wiki_root="$1" codebase_root="${2:-}"
  validate_wiki_root "$wiki_root"

  index_sources=()
  index_slugs=()
  index_ingested=()
  index_mtimes=()
  index_exists=()

  local entities_dir="$wiki_root/entities"
  [[ ! -d "$entities_dir" ]] && return 0

  local page source_path ingested_at slug resolved mtime_epoch
  while IFS= read -r -d '' page; do
    source_path=""
    ingested_at=""
    local in_fm=false line
    while IFS= read -r line; do
      if [[ "$line" =~ ^---$ ]]; then
        if $in_fm; then break; else in_fm=true; continue; fi
      fi
      if $in_fm; then
        [[ "$line" =~ ^source_path:[[:space:]]*(.*)$ ]] && source_path="${BASH_REMATCH[1]//\"/}"
        [[ "$line" =~ ^ingested_at:[[:space:]]*(.*)$ ]] && ingested_at="${BASH_REMATCH[1]//\"/}"
      fi
    done < "$page"

    [[ -z "$source_path" || -z "$ingested_at" ]] && continue
    slug="$(basename "$page")"
    slug="${slug%.md}"
    index_sources+=("$source_path")
    index_slugs["$source_path"]="$slug"
    index_ingested["$source_path"]="$ingested_at"
    resolved="$(resolve_source_path "$source_path" "$codebase_root")"
    if [[ -f "$resolved" ]]; then
      mtime_epoch=$(stat_epoch "$resolved")
      index_mtimes["$source_path"]="$mtime_epoch"
      index_exists["$source_path"]="true"
    else
      index_mtimes["$source_path"]=""
      index_exists["$source_path"]="false"
    fi
  done < <(find "$entities_dir" -maxdepth 1 -name '*.md' -print0 2>/dev/null)
}

emit_index_json() {
  local out="[" first=true source
  for source in "${index_sources[@]}"; do
    if $first; then first=false; else out+=","; fi
    out+="$(printf '{"source_path":"%s","slug":"%s","ingested_at":"%s","mtime":"%s","exists":%s}' \
      "$(json_escape "$source")" "$(json_escape "${index_slugs[$source]}")" "$(json_escape "${index_ingested[$source]}")" "$(json_escape "${index_mtimes[$source]}")" "${index_exists[$source]}")"
  done
  echo "$out]"
}

cmd_index() {
  local wiki_root="${1:-}" codebase_root=""
  shift || true
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --codebase-root) codebase_root="$2"; shift 2 ;;
      *) echo "Error: unknown flag: $1" >&2; exit 1 ;;
    esac
  done
  collect_index "$wiki_root" "$codebase_root"
  emit_index_json
}

cmd_walk() {
  local codebase_root="${1:-}" exclusions_file="$DEFAULT_EXCLUSIONS" summary=false respect_gitignore=true
  shift || true
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --exclusions) exclusions_file="$2"; shift 2 ;;
      --summary) summary=true; shift ;;
      --no-gitignore) respect_gitignore=false; shift ;;
      *) echo "Error: unknown flag: $1" >&2; exit 1 ;;
    esac
  done
  collect_walk "$codebase_root" "$exclusions_file" "$respect_gitignore"
  if $summary; then emit_summary_json; else emit_walk_json; fi
}

cmd_diff() {
  local codebase_root="${1:-}" wiki_root="${2:-}" exclusions_file="$DEFAULT_EXCLUSIONS" respect_gitignore=true
  shift 2 || true
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --exclusions) exclusions_file="$2"; shift 2 ;;
      --no-gitignore) respect_gitignore=false; shift ;;
      *) echo "Error: unknown flag: $1" >&2; exit 1 ;;
    esac
  done

  collect_walk "$codebase_root" "$exclusions_file" "$respect_gitignore"
  collect_index "$wiki_root" "$codebase_root"

  local out="[" first=true path reason slug
  for path in "${walk_paths[@]}"; do
    reason=""
    slug=""
    if [[ -z "${index_ingested[$path]:-}" ]]; then
      reason="new"
    elif ! is_epoch "${index_ingested[$path]}" || (( ${walk_mtimes[$path]} > ${index_ingested[$path]} )); then
      reason="stale"
      slug="${index_slugs[$path]}"
    fi
    [[ -z "$reason" ]] && continue
    if $first; then first=false; else out+=","; fi
    if [[ -n "$slug" ]]; then
      out+="$(printf '{"path":"%s","mtime":"%s","reason":"%s","slug":"%s"}' "$(json_escape "$path")" "$(json_escape "${walk_mtimes[$path]}")" "$reason" "$(json_escape "$slug")")"
    else
      out+="$(printf '{"path":"%s","mtime":"%s","reason":"%s"}' "$(json_escape "$path")" "$(json_escape "${walk_mtimes[$path]}")" "$reason")"
    fi
  done
  echo "$out]"
}

main() {
  local subcmd="${1:-}"
  shift || true
  case "$subcmd" in
    index) cmd_index "$@" ;;
    walk)  cmd_walk "$@" ;;
    diff)  cmd_diff "$@" ;;
    *)     usage ;;
  esac
}

main "$@"
