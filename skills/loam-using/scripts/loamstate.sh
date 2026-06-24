#!/usr/bin/env bash
# loamstate.sh — probe wiki root and qmd readiness in one shot
#
# Emits a single JSON line that every loam-memory skill consumes instead of
# doing 4-6 Glob/Read/Bash calls to discover the same state.
#
# Usage:
#   wikistate.sh <workspace-root>
#
# Output JSON:
#   {"wiki_root":"/abs/path","exists":true,"has_schema":true,"has_index":true,
#    "has_log":true,"has_overview":false,"qmd_ready":true,
#    "collection":"my-wiki","metadata_status":"ready","metadata_path":"/abs/.json"}
#
# Exit codes: 0 (wiki found or not found, JSON always emitted), 1 bad args

set -euo pipefail

json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '%s' "$s"
}

usage() {
  cat <<'EOF'
Usage: loamstate.sh <workspace-root>

Probes for a wiki under <workspace-root> (checks wiki/ subdir and root itself).
Resolves qmd readiness from .wiki-metadata.json, falling back to `which qmd`
and `qmd collection list` when metadata is absent or stale.

Emits one JSON line. Always exits 0 when args are valid.
EOF
  exit 1
}

[[ $# -lt 1 ]] && usage

WORKSPACE="$1"
[[ ! -d "$WORKSPACE" ]] && {
  echo "{\"error\":\"workspace not found: $WORKSPACE\"}"
  exit 0
}

# --- Resolve wiki root ---

WIKI_ROOT=""
for candidate in "$WORKSPACE/wiki" "$WORKSPACE"; do
  if [[ -f "$candidate/SCHEMA.md" || -f "$candidate/index.md" || -f "$candidate/log.md" ]]; then
    WIKI_ROOT="$(cd "$candidate" && pwd)"
    break
  fi
done

if [[ -z "$WIKI_ROOT" ]]; then
  echo '{"wiki_root":"","exists":false,"qmd_ready":false}'
  exit 0
fi

# --- Check contract files ---

HAS_SCHEMA=false; [[ -f "$WIKI_ROOT/SCHEMA.md" ]] && HAS_SCHEMA=true
HAS_INDEX=false;   [[ -f "$WIKI_ROOT/index.md" ]]   && HAS_INDEX=true
HAS_LOG=false;     [[ -f "$WIKI_ROOT/log.md" ]]     && HAS_LOG=true
HAS_OVERVIEW=false; [[ -f "$WIKI_ROOT/overview.md" ]] && HAS_OVERVIEW=true

# --- qmd readiness ---

QMD_READY=false
COLLECTION=""
META_STATUS=""
META_PATH=""

META_FILE="$WIKI_ROOT/.wiki-metadata.json"

if [[ -f "$META_FILE" ]]; then
  META_PATH="$META_FILE"
  # Parse JSON without jq: extract retrieval.status and retrieval.collection_name
  META_STATUS=$(grep -o '"status"[[:space:]]*:[[:space:]]*"[^"]*"' "$META_FILE" 2>/dev/null \
    | head -1 | sed 's/.*"status"[[:space:]]*:[[:space:]]*"//;s/"$//')
  COLLECTION=$(grep -o '"collection_name"[[:space:]]*:[[:space:]]*"[^"]*"' "$META_FILE" 2>/dev/null \
    | head -1 | sed 's/.*"collection_name"[[:space:]]*:[[:space:]]*"//;s/"$//')

  if [[ "$META_STATUS" == "ready" ]]; then
    QMD_READY=true
  fi
fi

# Fallback: if not ready from metadata, try `which qmd` + `qmd collection list`
if ! $QMD_READY; then
  if command -v qmd &>/dev/null; then
    # qmd exists — check if any collection points at our wiki root
    COLLECTIONS=$(qmd collection list 2>/dev/null || true)
    if [[ -n "$COLLECTIONS" ]]; then
      # Extract collection paths and match by absolute path equality
      # Output format varies; try to find a path matching WIKI_ROOT
      while IFS= read -r line; do
        # qmd collection list output varies by version; try to extract paths
        case "$line" in
          *"$WIKI_ROOT"*)
            # Found a collection pointing at our wiki root
            COLLECTION=$(echo "$line" | grep -o '"collection_name"[[:space:]]*:[[:space:]]*"[^"]*"' 2>/dev/null \
              | head -1 | sed 's/.*"collection_name"[[:space:]]*:[[:space:]]*"//;s/"$//')
            # If we couldn't parse the name, try the first token
            if [[ -z "$COLLECTION" ]]; then
              COLLECTION=$(echo "$line" | awk '{print $1}' | sed 's/[: ]//g')
            fi
            QMD_READY=true
            break
            ;;
        esac
      done <<< "$COLLECTIONS"
    fi
  fi
fi

# --- Emit JSON ---

printf '{"wiki_root":"%s","exists":true,"has_schema":%s,"has_index":%s,"has_log":%s,"has_overview":%s,"qmd_ready":%s,"collection":"%s","metadata_status":"%s","metadata_path":"%s"}\n' \
  "$(json_escape "$WIKI_ROOT")" \
  "$HAS_SCHEMA" \
  "$HAS_INDEX" \
  "$HAS_LOG" \
  "$HAS_OVERVIEW" \
  "$QMD_READY" \
  "$(json_escape "$COLLECTION")" \
  "$(json_escape "$META_STATUS")" \
  "$(json_escape "$META_PATH")"