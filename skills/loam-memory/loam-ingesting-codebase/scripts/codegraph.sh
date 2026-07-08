#!/usr/bin/env bash
# codegraph.sh — helper for loam::ingesting-codebase and loam::syncing-code-graph
# No jq dependency. Output is minimal JSON assembled with printf.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(dirname "$SCRIPT_DIR")"
DEFAULT_EXCLUSIONS="$SKILL_DIR/references/ingestion-exclusions.md"
MAX_BYTES=$((500 * 1024))

# Shared helpers (validate_wiki_root). Single source of truth in loam-using.
LOAM_COMMON=""
for candidate in \
  "$SCRIPT_DIR/../../../loam-using/scripts/loam-common.sh" \
  "$SCRIPT_DIR/../../loam-using/scripts/loam-common.sh"; do
  if [[ -f "$candidate" ]]; then
    LOAM_COMMON="$candidate"
    break
  fi
done
[[ -n "$LOAM_COMMON" ]] || { echo "Error: loam-common.sh not found near: $SCRIPT_DIR" >&2; exit 1; }
source "$LOAM_COMMON"

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

compute_hash() {
  sha256sum "$1" 2>/dev/null | awk '{print $1}' || shasum -a 256 "$1" 2>/dev/null | awk '{print $1}' || echo ""
}

usage() {
  cat <<'EOF'
Usage:
  codegraph.sh index <wiki-root> [--codebase-root <codebase-root>]
  codegraph.sh walk  <codebase-root> [--exclusions <exclusions.md>] [--summary] [--no-gitignore]
  codegraph.sh diff  <codebase-root> <wiki-root> [--exclusions <exclusions.md>] [--no-gitignore] [--strict]

Exit codes: 0 ok, 1 bad args, 2 root not found, 3 exclusions file missing.
EOF
  exit 1
}

declare -a exclude_patterns=()
declare -a include_exts=()
declare -a prune_dirs=()

parse_exclusions() {
  local exclusions_file="$1"
  [[ ! -f "$exclusions_file" ]] && { echo "Error: exclusions file not found: $exclusions_file" >&2; exit 3; }

  exclude_patterns=()
  include_exts=()
  prune_dirs=()
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
      # ponytail: derive prune dir names from **/DIR/** and DIR/** subtree patterns so find
      # never descends into them; case-loop stays as second pass for globs find can't express.
      local dir_name
      if [[ "$line" == \*\*/*/\*\* ]]; then
        dir_name="${line#\*\*/}"
        dir_name="${dir_name%%/\*\*}"
        # Only bare dir names (no slashes) are safe to prune at find level
        [[ "$dir_name" != */* && -n "$dir_name" && " ${prune_dirs[*]} " != *" $dir_name "* ]] && prune_dirs+=("$dir_name")
      elif [[ "$line" == */\*\* ]]; then
        dir_name="${line%%/\*\*}"
        # Only bare dir names (no slashes) are safe to prune at find level
        [[ "$dir_name" != */* && -n "$dir_name" && " ${prune_dirs[*]} " != *" $dir_name "* ]] && prune_dirs+=("$dir_name")
      fi
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
  local rel_path="$1" basename pat match_pat root_pat
  basename="${rel_path##*/}"
  for pat in "${exclude_patterns[@]}"; do
    [[ -z "$pat" ]] && continue
    match_pat="${pat//\*\*/\*}"
    # shellcheck disable=SC2254
    case "$rel_path" in $match_pat) return 0 ;; esac
    # shellcheck disable=SC2254
    case "$basename" in $match_pat) return 0 ;; esac
    # ponytail: **/ prefix means "any depth incl root"; */X/* misses root-level X/*, so also test stripped
    if [[ "$pat" == \*\*/* ]]; then
      root_pat="${match_pat#*/}"
      # shellcheck disable=SC2254
      case "$rel_path" in $root_pat) return 0 ;; esac
    fi
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
declare -A walk_sizes=()
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
  walk_sizes=()
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
  # ponytail: prefer fd (faster, respects .gitignore by default, parallel); fall back to find.
  # No new hard dependency — command -v guard makes fd optional.
  local find_prune_args=()
  if [[ ${#prune_dirs[@]} -gt 0 ]]; then
    local d
    find_prune_args+=(\()
    for d in "${prune_dirs[@]}"; do
      find_prune_args+=(-name "$d" -o)
    done
    unset 'find_prune_args[-1]'
    find_prune_args+=(\) -prune -o)
  fi

  if command -v fd >/dev/null 2>&1; then
    # ponytail: --hidden so dotfiles/dot-dirs reach the walk (find shows them).
    # --no-ignore (-I) so .gitignore/.ignore/.fdignore don't silently drop files
    # before the bash is_gitignored counter runs.
    local _fd_args=(--type f --print0 --hidden --no-ignore)
    local _pd
    for _pd in "${prune_dirs[@]}"; do
      _fd_args+=(--exclude "$_pd")
    done
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
      walk_sizes["$rel_path"]="$size"
      ext="${rel_path##*.}"
      by_ext["$ext"]=$(( ${by_ext["$ext"]:-0} + 1 ))
    done < <(fd "${_fd_args[@]}" . "$codebase_root" 2>/dev/null)
  else
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
      walk_sizes["$rel_path"]="$size"
      ext="${rel_path##*.}"
      by_ext["$ext"]=$(( ${by_ext["$ext"]:-0} + 1 ))
    done < <(find "$codebase_root" "${find_prune_args[@]}" -type f -print0 2>/dev/null)
  fi
}

emit_walk_json() {
  local out="[" first=true path
  for path in "${walk_paths[@]}"; do
    if $first; then first=false; else out+=","; fi
    out+="$(printf '{"path":"%s","mtime":"%s","size":"%s"}' "$(json_escape "$path")" "$(json_escape "${walk_mtimes[$path]}")" "$(json_escape "${walk_sizes[$path]}")")"
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
declare -A index_sizes=()
declare -A index_hashes=()
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
  validate_wiki_root "$wiki_root" || exit $?

  index_sources=()
  index_slugs=()
  index_ingested=()
  index_sizes=()
  index_hashes=()
  index_mtimes=()
  index_exists=()

  local page source_path ingested_at source_size content_hash slug resolved mtime_epoch
  # Dual scan: code/ (primary) and entities/ (legacy transition for stranded source_path: pages)
  for entities_dir in "$wiki_root/code" "$wiki_root/entities"; do
    [[ ! -d "$entities_dir" ]] && continue
    while IFS= read -r -d '' page; do
      source_path=""
      ingested_at=""
      source_size=""
      content_hash=""
      local in_fm=false line
      while IFS= read -r line; do
        if [[ "$line" =~ ^---$ ]]; then
          if $in_fm; then break; else in_fm=true; continue; fi
        fi
        if $in_fm; then
          [[ "$line" =~ ^source_path:[[:space:]]*(.*)$ ]] && source_path="${BASH_REMATCH[1]//\"/}"
          [[ "$line" =~ ^ingested_at:[[:space:]]*(.*)$ ]] && ingested_at="${BASH_REMATCH[1]//\"/}"
          [[ "$line" =~ ^source_size:[[:space:]]*(.*)$ ]] && source_size="${BASH_REMATCH[1]//\"/}"
          [[ "$line" =~ ^content_hash:[[:space:]]*(.*)$ ]] && content_hash="${BASH_REMATCH[1]//\"/}"
        fi
      done < "$page"

      content_hash=$(printf '%s' "$content_hash" | tr '[:upper:]' '[:lower:]')
      [[ -z "$source_path" || -z "$ingested_at" ]] && continue
      [[ -n "${index_slugs[$source_path]:-}" ]] && continue
      slug="$(basename "$page")"
      slug="${slug%.md}"
      index_sources+=("$source_path")
      index_slugs["$source_path"]="$slug"
      index_ingested["$source_path"]="$ingested_at"
      index_sizes["$source_path"]="$source_size"
      index_hashes["$source_path"]="$content_hash"
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
  done
}

emit_index_json() {
  local out="[" first=true source
  for source in "${index_sources[@]}"; do
    if $first; then first=false; else out+=","; fi
    out+="$(printf '{"source_path":"%s","slug":"%s","ingested_at":"%s","source_size":"%s","content_hash":"%s","mtime":"%s","exists":%s}' \
      "$(json_escape "$source")" "$(json_escape "${index_slugs[$source]}")" "$(json_escape "${index_ingested[$source]}")" "$(json_escape "${index_sizes[$source]}")" "$(json_escape "${index_hashes[$source]}")" "$(json_escape "${index_mtimes[$source]}")" "${index_exists[$source]}")"
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
  local codebase_root="${1:-}" wiki_root="${2:-}" exclusions_file="$DEFAULT_EXCLUSIONS" respect_gitignore=true strict=false
  shift 2 || true
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --exclusions) exclusions_file="$2"; shift 2 ;;
      --no-gitignore) respect_gitignore=false; shift ;;
      --strict) strict=true; shift ;;
      *) echo "Error: unknown flag: $1" >&2; exit 1 ;;
    esac
  done

  collect_walk "$codebase_root" "$exclusions_file" "$respect_gitignore"
  collect_index "$wiki_root" "$codebase_root"

  local out="[" first=true path reason slug idx_size file_hash
  for path in "${walk_paths[@]}"; do
    reason=""
    slug=""
    if [[ -z "${index_ingested[$path]:-}" ]]; then
      reason="new"
    else
      slug="${index_slugs[$path]}"
      if $strict; then
        # --strict overlay: compute hash on every file regardless of mtime/size
        if [[ -n "${index_hashes[$path]:-}" ]]; then
          file_hash=$(compute_hash "$codebase_root/$path")
          if [[ -n "$file_hash" && "$file_hash" == "${index_hashes[$path]}" ]]; then
            reason=""
          else
            reason="stale"
          fi
        else
          reason="stale"
        fi
      elif ! is_epoch "${index_ingested[$path]}"; then
        reason="stale"
      elif (( ${walk_mtimes[$path]} > ${index_ingested[$path]} )); then
        # mtime newer → candidate stale
        idx_size="${index_sizes[$path]:-}"
        if [[ -n "$idx_size" && "$idx_size" =~ ^[0-9]+$ ]]; then
          if [[ "${walk_sizes[$path]}" != "$idx_size" ]]; then
            reason="stale"
          elif [[ -n "${index_hashes[$path]:-}" ]]; then
            file_hash=$(compute_hash "$codebase_root/$path")
            if [[ -n "$file_hash" && "$file_hash" == "${index_hashes[$path]}" ]]; then
              reason=""
            else
              reason="stale"
            fi
          else
            reason="stale"
          fi
        else
          reason="stale"
        fi
      fi
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
