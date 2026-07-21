#!/bin/sh
# datecheck.sh — compatibility forwarder to `loam datecheck`.
#
# Usage:
#   datecheck.sh check <wiki-root>
#   datecheck.sh fix   <wiki-root> [--offset +02:00]
#
# Exit codes are the native runtime's: 0 clean/fixed, 1 bad args, 2 drift found,
# 75 runtime not yet available, 78 invalid CLI_VERSION or unsupported target.
#
# The full Bash implementation this replaced lives in datecheck-legacy.sh and is
# retained only as parity evidence until the native runtime is published.
set -u
case "$0" in
  /*) SCRIPT_DIR=${0%/*} ;;
  */*) SCRIPT_DIR=$PWD/${0%/*} ;;
  *) SCRIPT_DIR=$PWD ;;
esac
exec "$SCRIPT_DIR/loam.sh" datecheck "$@"
