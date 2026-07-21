#!/bin/sh
# loamstate.sh — compatibility entry point for workspace state.
#
# Delegates to the native runtime through loam.sh. When the runtime is absent,
# installing, unsupported, or unavailable it emits a minimal but valid state
# document and exits 0, so session startup never blocks on a download.
#
# Usage: loamstate.sh [--fast] <workspace-root>
# Exit codes: 0 always for probe/environment failures; 1 for bad arguments.
#
# The full Bash implementation this replaced lives in loamstate-legacy.sh and
# is retained only as parity evidence until the native runtime is published.

set -u

case "$0" in
  /*) SCRIPT_DIR=${0%/*} ;;
  */*) SCRIPT_DIR=$PWD/${0%/*} ;;
  *) SCRIPT_DIR=$PWD ;;
esac

fast=0
workspace=""
for argument in "$@"; do
  case "$argument" in
    --fast) fast=1 ;;
    -*)
      echo "Usage: loamstate.sh [--fast] <workspace-root>" >&2
      exit 1
      ;;
    *) workspace=$argument ;;
  esac
done

if [ -z "$workspace" ]; then
  echo "Usage: loamstate.sh [--fast] <workspace-root>" >&2
  exit 1
fi

# Minimal valid state: every field the consumers read, degraded to neutral
# values, plus the canonical runtime_unavailable maintenance hint.
minimal_state() {
  reason=$1 version=$2 target=$3
  printf '{"wiki_root":"","exists":false,"qmd_ready":false,"latest_checkpoint":null,'
  printf '"recent_checkpoints":[],"checkpoint_count":0,"git_status":null,"drift_count":null,'
  printf '"hints":[{"kind":"runtime_unavailable","group":"maintenance","severity":"info",'
  printf '"message":"Native loam runtime is unavailable; state is minimal until it installs.",'
  printf '"command":null,"evidence":{"reason":"%s","target":"%s","version":"%s"}}]}\n' \
    "$reason" "$target" "$version"
}

runtime_version() {
  [ -f "$SCRIPT_DIR/CLI_VERSION" ] || return 1
  tr -d ' \t\r\n' < "$SCRIPT_DIR/CLI_VERSION"
}

if [ "$fast" -eq 1 ]; then
  set -- state --fast "$workspace"
else
  set -- state "$workspace"
fi

output=$("$SCRIPT_DIR/loam.sh" "$@" 2>/dev/null)
status=$?

if [ "$status" -eq 0 ] && [ -n "$output" ]; then
  printf '%s\n' "$output"
  exit 0
fi

version=$(runtime_version) || version=""
target=${LOAM_TARGET:-$(uname -m 2>/dev/null)-$(uname -s 2>/dev/null)}
case "$status" in
  78) reason="configuration" ;;
  75) reason="installing" ;;
  *) reason="unavailable" ;;
esac
minimal_state "$reason" "$version" "$target"
exit 0
