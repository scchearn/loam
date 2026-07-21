#!/bin/sh
# codegraph.sh — compatibility forwarder to `loam codegraph`.
#
# Usage:
#   codegraph.sh index <wiki-root> [--codebase-root <codebase-root>]
#   codegraph.sh walk  <codebase-root> [--exclusions <file>] [--summary] [--no-gitignore]
#   codegraph.sh diff  <codebase-root> <wiki-root> [--exclusions <file>] [--no-gitignore] [--strict]
#
# Exit codes are the native runtime's: 0 ok, 1 bad args, 2 root not found,
# 3 exclusions file missing, 75 runtime not yet available, 78 configuration.
#
# The full Bash implementation this replaced lives in codegraph-legacy.sh and is
# retained only as parity evidence until the native runtime is published.
set -u
case "$0" in
  /*) SCRIPT_DIR=${0%/*} ;;
  */*) SCRIPT_DIR=$PWD/${0%/*} ;;
  *) SCRIPT_DIR=$PWD ;;
esac

# The generic launcher lives in the loam-using base skill; both the nested and
# flat install layouts are supported.
for candidate in \
  "$SCRIPT_DIR/../../../loam-using/scripts/loam.sh" \
  "$SCRIPT_DIR/../../loam-using/scripts/loam.sh"; do
  if [ -f "$candidate" ]; then
    exec "$candidate" codegraph "$@"
  fi
done
echo "Error: loam launcher not found near: $SCRIPT_DIR" >&2
exit 1
