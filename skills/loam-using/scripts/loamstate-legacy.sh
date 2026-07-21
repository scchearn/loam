#!/usr/bin/env bash
# loamstate.sh — probe wiki root and qmd readiness in one shot
#
# Emits a single JSON line that every loam-memory skill consumes instead of
# doing 4-6 Glob/Read/Bash calls to discover the same state.
#
# Usage:
#   loamstate.sh <workspace-root>
#
# Output JSON (one line): wiki discovery + qmd readiness + orientation state
# (latest_checkpoint, recent_checkpoints, checkpoint_count, git_status,
# drift_count) + an advisory `hints` array. Each hint is
#   {kind, group(maintenance|workflow), severity(info|warn|action),
#    message, command(string|null), evidence{}}
# Hints are best-effort routing signals; loam::using owns their contract.
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
Usage: loamstate.sh [--fast] <workspace-root>

Probes for a wiki under <workspace-root> (checks wiki/ subdir and root itself).
Resolves qmd readiness from .wiki-metadata.json, falling back to `which qmd`
and `qmd collection list` when metadata is absent or stale.

Emits one JSON line. Always exits 0 when args are valid.

  --fast  Skip codegraph diff and datecheck (drift_count=null, code_ingest_pending
          and date_drift_pending hints omitted). Use for session-start injection
          where speed matters more than complete drift detection.
EOF
  exit 1
}

[[ $# -lt 1 ]] && usage

FAST=false
WORKSPACE=""
for _arg in "$@"; do
  case "$_arg" in
    --fast) FAST=true ;;
    --*) usage ;;
    *) [[ -z "$WORKSPACE" ]] && WORKSPACE="$_arg" ;;
  esac
done
[[ -z "$WORKSPACE" ]] && usage
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
  echo '{"wiki_root":"","exists":false,"qmd_ready":false,"latest_checkpoint":null,"recent_checkpoints":[],"checkpoint_count":0,"git_status":null,"drift_count":null,"hints":[{"kind":"memory_missing","group":"maintenance","severity":"info","message":"No memory substrate found; scaffold a wiki to begin.","command":"/loam::scaffolding-wiki <goal>","evidence":{}}]}'
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
# Skipped in --fast mode (drift_count stays null, date_drift_pending hint omitted).
DRIFT_JSON="null"
if ! $FAST; then
  DATECHECK="$SCRIPT_DIR/datecheck-legacy.sh"
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
fi

# --- Hints (advisory, best-effort) ---
# Every probe below reuses state already computed above; a failing probe appends
# no hint rather than aborting. loam::using owns the shared hint contract.

NOW_EPOCH=$(date +%s 2>/dev/null || echo 0)
epoch_of() { date -d "$1" +%s 2>/dev/null || echo ""; }  # "" on parse failure

HINTS=()
add_hint() {  # kind group severity message command|"" evidence_json
  local cmd
  if [[ -n "$5" ]]; then cmd="\"$(json_escape "$5")\""; else cmd="null"; fi
  HINTS+=("$(printf '{"kind":"%s","group":"%s","severity":"%s","message":"%s","command":%s,"evidence":%s}' \
    "$1" "$2" "$3" "$(json_escape "$4")" "$cmd" "$6")")
}

# Checkpoint age (minutes) from the latest checkpoint's Captured timestamp.
CKPT_AGE_MIN=""
if [[ $CHECKPOINT_COUNT -gt 0 ]]; then
  _cap=$(ckpt_field "${CKPTS[0]}" Captured)
  _ce=$(epoch_of "$_cap")
  [[ -n "$_ce" && "$NOW_EPOCH" -gt 0 ]] && CKPT_AGE_MIN=$(( (NOW_EPOCH - _ce) / 60 ))
fi
GIT_DIRTY=false
[[ "$GIT_STATUS_JSON" != "null" && "$GIT_STATUS_JSON" != '""' ]] && GIT_DIRTY=true

# checkpoint_stale: dirty worktree AND (no checkpoint OR latest >30min old).
if $GIT_DIRTY && { [[ $CHECKPOINT_COUNT -eq 0 ]] || [[ -n "$CKPT_AGE_MIN" && $CKPT_AGE_MIN -ge 30 ]]; }; then
  add_hint checkpoint_stale maintenance info \
    "Working tree changed; the last checkpoint is missing or 30+ min old." \
    "/loam::checkpointing" \
    "$(printf '{"git_dirty":true,"age_minutes":%s,"checkpoint_count":%s}' "${CKPT_AGE_MIN:-null}" "$CHECKPOINT_COUNT")"
fi

# resume_available / resume_stale: a checkpoint exists (24h threshold).
if [[ $CHECKPOINT_COUNT -gt 0 ]]; then
  if [[ -n "$CKPT_AGE_MIN" && $CKPT_AGE_MIN -ge 1440 ]]; then
    add_hint resume_stale workflow info "Latest checkpoint is over 24h old; resume context may be outdated." \
      "/loam::resuming" "$(printf '{"age_minutes":%s}' "$CKPT_AGE_MIN")"
  else
    add_hint resume_available workflow info "A checkpoint exists; you can resume prior work." \
      "/loam::resuming" "$(printf '{"age_minutes":%s}' "${CKPT_AGE_MIN:-null}")"
  fi
fi

# date_drift_pending: datecheck already counted above.
if [[ "$DRIFT_JSON" =~ ^[0-9]+$ && "$DRIFT_JSON" -gt 0 ]]; then
  add_hint date_drift_pending maintenance info "Date/timezone drift found in memory pages." \
    "/loam::linting-memory" "$(printf '{"drift_count":%s}' "$DRIFT_JSON")"
fi

# log_rotation_due: log.md over 500 lines.
if [[ -f "$WIKI_ROOT/log.md" ]]; then
  LOG_LINES=$(wc -l < "$WIKI_ROOT/log.md" 2>/dev/null | tr -d ' ')
  if [[ "$LOG_LINES" =~ ^[0-9]+$ && "$LOG_LINES" -gt 500 ]]; then
    add_hint log_rotation_due maintenance info "log.md exceeds 500 lines; consider rotating it." \
      "/loam::linting-memory" "$(printf '{"log_lines":%s}' "$LOG_LINES")"
  fi
fi

# legacy_structure_pending: a root overview.md still exists.
if $HAS_OVERVIEW; then
  add_hint legacy_structure_pending maintenance info "Legacy overview.md present; consolidate into index.md." \
    "/loam::linting-memory" '{"has_overview":true}'
fi

# retrieval_not_ready: metadata present but retrieval is not ready.
if [[ -n "$META_STATUS" && "$META_STATUS" != "ready" ]]; then
  add_hint retrieval_not_ready maintenance info "qmd retrieval metadata is present but not ready." \
    "" "$(printf '{"metadata_status":"%s"}' "$(json_escape "$META_STATUS")")"
fi

# memory_lint_stale: newest `## [YYYY-MM-DD] lint-check` marker in log.md is
# missing or older than 7 days. Marker is written by /loam::linting-memory.
if [[ -f "$WIKI_ROOT/log.md" ]]; then
  LINT_DATE=$( { grep -oE '^## \[[0-9]{4}-[0-9]{2}-[0-9]{2}\] lint-check' "$WIKI_ROOT/log.md" 2>/dev/null || true; } \
    | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}' | sort -r | head -1 || true)
  LINT_STALE=false; LINT_AGE_DAYS=null
  if [[ -z "$LINT_DATE" ]]; then
    LINT_STALE=true
  else
    _le=$(epoch_of "$LINT_DATE 00:00 +00:00")
    if [[ -n "$_le" && "$NOW_EPOCH" -gt 0 ]]; then
      LINT_AGE_DAYS=$(( (NOW_EPOCH - _le) / 86400 ))
      [[ $LINT_AGE_DAYS -ge 7 ]] && LINT_STALE=true
    fi
  fi
  if $LINT_STALE; then
    _ld=$([[ -n "$LINT_DATE" ]] && echo "\"$LINT_DATE\"" || echo null)
    add_hint memory_lint_stale maintenance info "Memory lint is stale or was never recorded." \
      "/loam::linting-memory" "$(printf '{"last_lint":%s,"age_days":%s}' "$_ld" "$LINT_AGE_DAYS")"
  fi
fi

# code_ingest_pending: only when a code graph already exists (wiki/code). Gated
# to avoid nagging prose-only wikis and to skip the tree walk otherwise.
# Skipped in --fast mode (codegraph diff is the dominant cost — 9.6s on 51K files).
# Resolve codegraph.sh in both repo layout (grouped under loam-memory/) and
# flat installed layout (sibling of loam-using/), mirroring loam-common.
CODEGRAPH=""
if ! $FAST; then
  for _cg in \
    "$SCRIPT_DIR/../../loam-memory/loam-ingesting-codebase/scripts/codegraph-legacy.sh" \
    "$SCRIPT_DIR/../../loam-ingesting-codebase/scripts/codegraph-legacy.sh"; do
    [[ -f "$_cg" ]] && { CODEGRAPH="$_cg"; break; }
  done
  if [[ -d "$WIKI_ROOT/code" && -n "$CODEGRAPH" ]]; then
    set +e
    CG_OUT=$(bash "$CODEGRAPH" diff "$WORKSPACE" "$WIKI_ROOT" 2>/dev/null)
    CG_RC=$?
    set -e
    if [[ $CG_RC -eq 0 && -n "$CG_OUT" ]]; then
      CG_COUNT=$( { printf '%s' "$CG_OUT" | grep -o '"reason"' || true; } | wc -l | tr -d ' ')
      if [[ "$CG_COUNT" =~ ^[0-9]+$ && "$CG_COUNT" -gt 0 ]]; then
        add_hint code_ingest_pending maintenance info "$CG_COUNT source file(s) new or changed since last ingest." \
          "/loam::ingesting-codebase <workspace-root>" "$(printf '{"pending_count":%s}' "$CG_COUNT")"
      fi
    fi
  fi
fi

# Workflow: specs/ and plans/ at the workspace root (may be gitignored, so use
# direct filesystem globs, never Glob). spec_ready_for_plan / plan_* by status.
shopt -s nullglob
for _spec in "$WORKSPACE"/specs/*.md; do
  _base=$(basename "$_spec"); [[ "$_base" == "INDEX.md" ]] && continue
  _slug="${_base%.md}"
  _st=$(sed -n 's/^status:[[:space:]]*//p' "$_spec" 2>/dev/null | head -1 | tr -d '"')
  _appr=$(sed -n 's/^approved_at:[[:space:]]*//p' "$_spec" 2>/dev/null | head -1 | tr -d '"')
  if [[ "$_st" == "approved" || ( -n "$_appr" && "$_appr" != "null" ) ]] && [[ ! -f "$WORKSPACE/plans/$_slug.md" ]]; then
    add_hint spec_ready_for_plan workflow info "Approved spec has no plan yet." \
      "/loam::planning specs/$_base" "$(printf '{"spec":"specs/%s"}' "$_base")"
  fi
done
for _plan in "$WORKSPACE"/plans/*.md; do
  _base=$(basename "$_plan"); [[ "$_base" == "INDEX.md" ]] && continue
  _st=$(sed -n 's/^status:[[:space:]]*//p' "$_plan" 2>/dev/null | head -1 | tr -d '"')
  case "$_st" in
    pending)     add_hint plan_ready_to_start workflow info "A plan is ready to start." \
                   "/loam::starting plans/$_base" "$(printf '{"plan":"plans/%s"}' "$_base")" ;;
    in-progress) add_hint plan_in_progress workflow info "A plan is in progress." \
                   "/loam::starting plans/$_base" "$(printf '{"plan":"plans/%s"}' "$_base")" ;;
  esac
done
shopt -u nullglob

HINTS_JSON="["
if [[ ${#HINTS[@]} -gt 0 ]]; then
  for _i in "${!HINTS[@]}"; do
    [[ $_i -gt 0 ]] && HINTS_JSON+=","
    HINTS_JSON+="${HINTS[$_i]}"
  done
fi
HINTS_JSON+="]"

# --- Emit JSON ---

printf '{"wiki_root":"%s","exists":true,"has_schema":%s,"has_index":%s,"has_log":%s,"has_overview":%s,"qmd_ready":%s,"collection":"%s","metadata_status":"%s","metadata_path":"%s","latest_checkpoint":%s,"recent_checkpoints":%s,"checkpoint_count":%s,"git_status":%s,"drift_count":%s,"hints":%s}\n' \
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
  "$DRIFT_JSON" \
  "$HINTS_JSON"