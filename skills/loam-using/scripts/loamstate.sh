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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

json_escape() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  s="${s//$'\n'/\\n}"
  s="${s//$'\r'/\\r}"
  s="${s//$'\t'/\\t}"
  printf '%s' "$s"
}

# --- checkpoint parsing helpers ---
# Checkpoint format (loam-checkpointing): "# Checkpoint" is a constant
# header; "- Captured:" / "- Scope:" bullets under it carry the fields.
# The descriptive title is the first "### " heading (workstream name
# under ## Workstreams). Fall back to "# " if no "### " exists.

ckpt_heading() {
  local h
  h=$(awk '/^### /{ sub(/^###[[:space:]]+/,""); sub(/[[:space:]]+$/,""); print; exit }' "$1" 2>/dev/null)
  if [[ -z "$h" ]]; then
    h=$(awk '/^# /{ sub(/^#[[:space:]]+/,""); sub(/[[:space:]]+$/,""); print; exit }' "$1" 2>/dev/null)
  fi
  printf '%s' "$h"
}

ckpt_field() {
  # $1 file, $2 field name (Captured|Scope). gawk match() capture — gawk is
  # already a dep (checkpoint-verify uses it).
  awk -v f="$2" '
    /^# /{ if(seen) exit; seen=1 }
    seen && match($0, "^[[:space:]]*-[[:space:]]*" f "[[:space:]]*:(.*)$", a) {
      val=a[1]; gsub(/^[[:space:]]+|[[:space:]]+$/,"",val); print val; exit
    }
  ' "$1" 2>/dev/null
}

# ckpt_obj <file> <include_scope 0|1> — emit one checkpoint JSON object.
# Missing heading/fields degrade to null; never crashes the aggregate JSON.
ckpt_obj() {
  local f="$1" inc="$2" title cap scope tj cj sj
  title=$(ckpt_heading "$f"); cap=$(ckpt_field "$f" Captured); scope=$(ckpt_field "$f" Scope)
  if [[ -n "$title" ]]; then tj="\"$(json_escape "$title")\""; else tj="null"; fi
  if [[ -n "$cap" ]]; then cj="\"$(json_escape "$cap")\""; else cj="null"; fi
  if [[ "$inc" == "1" ]]; then
    if [[ -n "$scope" ]]; then sj="\"$(json_escape "$scope")\""; else sj="null"; fi
    printf '{"path":"%s","title":%s,"captured_at":%s,"scope":%s}' \
      "$(json_escape "$f")" "$tj" "$cj" "$sj"
  else
    printf '{"path":"%s","title":%s,"captured_at":%s}' \
      "$(json_escape "$f")" "$tj" "$cj"
  fi
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
  echo '{"wiki_root":"","exists":false,"qmd_ready":false,"latest_checkpoint":null,"recent_checkpoints":[],"checkpoint_count":0,"git_status":null,"drift_count":null}'
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

# --- Aggregate orientation state (checkpoints, git, drift) ---
# Every field degrades to null/0/[] independently; a failure here never
# turns the clean JSON into a partial object or a crash.

declare -a CKPTS=()
if [[ -d "$WIKI_ROOT/checkpoints" ]]; then
  # Filename sort is chronological (checkpoint-YYYY-MM-DD-HHMM.md); mtime is
  # unreliable on a clone-install plugin. Newest first.
  mapfile -t CKPTS < <(ls "$WIKI_ROOT/checkpoints"/checkpoint-*.md 2>/dev/null | sort -r)
fi
CHECKPOINT_COUNT=${#CKPTS[@]}

if [[ $CHECKPOINT_COUNT -gt 0 ]]; then
  LATEST_JSON=$(ckpt_obj "${CKPTS[0]}" 1)
else
  LATEST_JSON="null"
fi

RECENT_JSON="["
if [[ $CHECKPOINT_COUNT -gt 0 ]]; then
  for i in "${!CKPTS[@]}"; do
    [[ $i -ge 5 ]] && break
    [[ $i -gt 0 ]] && RECENT_JSON+=","
    RECENT_JSON+=$(ckpt_obj "${CKPTS[$i]}" 0)
  done
fi
RECENT_JSON+="]"

# git status — optional, never fatal
GIT_STATUS_JSON="null"
if command -v git >/dev/null 2>&1 && git -C "$WORKSPACE" rev-parse --git-dir >/dev/null 2>&1; then
  GIT_OUT=$(git -C "$WORKSPACE" status --porcelain 2>/dev/null || true)
  GIT_STATUS_JSON="\"$(json_escape "$GIT_OUT")\""
fi

# date drift count — optional, never fatal. datecheck exits 0 (clean) or 2
# (drift found); both are success for counting. Anything else → null.
DRIFT_JSON="null"
DATECHECK="$SCRIPT_DIR/datecheck.sh"
if [[ -f "$DATECHECK" ]]; then
  set +e
  DC_OUT=$(bash "$DATECHECK" check "$WIKI_ROOT" 2>/dev/null)
  DC_RC=$?
  set -e
  if [[ $DC_RC -eq 0 || $DC_RC -eq 2 ]]; then
    if [[ -n "$DC_OUT" ]]; then
      DRIFT_JSON=$(printf '%s\n' "$DC_OUT" | grep -c '^{' || true)
    else
      DRIFT_JSON=0
    fi
  fi
fi

# --- Emit JSON ---

printf '{"wiki_root":"%s","exists":true,"has_schema":%s,"has_index":%s,"has_log":%s,"has_overview":%s,"qmd_ready":%s,"collection":"%s","metadata_status":"%s","metadata_path":"%s","latest_checkpoint":%s,"recent_checkpoints":%s,"checkpoint_count":%s,"git_status":%s,"drift_count":%s}\n' \
  "$(json_escape "$WIKI_ROOT")" \
  "$HAS_SCHEMA" \
  "$HAS_INDEX" \
  "$HAS_LOG" \
  "$HAS_OVERVIEW" \
  "$QMD_READY" \
  "$(json_escape "$COLLECTION")" \
  "$(json_escape "$META_STATUS")" \
  "$(json_escape "$META_PATH")" \
  "$LATEST_JSON" \
  "$RECENT_JSON" \
  "$CHECKPOINT_COUNT" \
  "$GIT_STATUS_JSON" \
  "$DRIFT_JSON"