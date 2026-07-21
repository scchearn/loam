#!/bin/sh
# loam.sh — POSIX scope resolver, bootstrapper, and launcher for the native
# loam runtime. Every loam skill reaches the runtime through this one contract.
#
# Usage:
#   loam.sh <loam-command> [args...]   run the native runtime
#   loam.sh --loam-runtime-path        print the resolved runtime path
#   loam.sh --loam-bootstrap           install the runtime synchronously
#
# Exit codes: the runtime's own when it runs; 75 when the runtime is not yet
# available (temporary); 78 for invalid CLI_VERSION or an unsupported target.
#
# The runtime lives outside .agents/skills/ so `npx skills update` neither
# deletes it nor sees it as modified skill content.

set -u

REPO_RELEASE_BASE='https://github.com/scchearn/loam/releases/download'
SUPPORTED_TARGETS='x86_64-apple-darwin aarch64-apple-darwin x86_64-pc-windows-msvc x86_64-unknown-linux-musl aarch64-unknown-linux-musl'
MARKER_STALE_SECONDS=600

# --- logical script directory -------------------------------------------------
# $0's dirname, made absolute without resolving symlinks: scope must follow the
# logical invocation path, never the physical inode a harness symlink targets.
script_dir() {
  case "$0" in
    /*) dir=${0%/*} ;;
    */*) dir=$PWD/${0%/*} ;;
    *) dir=$PWD ;;
  esac
  # Normalise . and .. textually; `cd -P` would resolve symlinks.
  printf '%s' "$dir" | awk -F/ '{
    n = 0
    for (i = 1; i <= NF; i++) {
      if ($i == "" || $i == ".") continue
      if ($i == ".." && n > 0) { n--; continue }
      parts[++n] = $i
    }
    out = ""
    for (i = 1; i <= n; i++) out = out "/" parts[i]
    print (out == "" ? "/" : out)
  }'
}

SCRIPT_DIR=$(script_dir)

# --- scope resolution ---------------------------------------------------------
# Nearest .agents ancestor of the logical launcher path; otherwise ~/.agents.
agents_root() {
  dir=$SCRIPT_DIR
  while [ -n "$dir" ] && [ "$dir" != "/" ]; do
    case ${dir##*/} in
      .agents) printf '%s' "$dir"; return 0 ;;
    esac
    dir=${dir%/*}
  done
  printf '%s/.agents' "${HOME:-/tmp}"
}

AGENTS_ROOT=$(agents_root)
RUNTIME_ROOT="$AGENTS_ROOT/loam"
INSTALL_LOG="$RUNTIME_ROOT/install.log"

# An agent-managed tree must never dirty the user's worktree, and we must not
# edit the repository's own .gitignore to achieve that.
ensure_agents_gitignore() {
  [ -f "$AGENTS_ROOT/.gitignore" ] && return 0
  mkdir -p "$AGENTS_ROOT" 2>/dev/null || return 0
  printf '*\n' > "$AGENTS_ROOT/.gitignore" 2>/dev/null || true
}

# --- CLI_VERSION --------------------------------------------------------------
# Read and validated exactly once so an in-flight install stays pinned to the
# version it started with.
read_version() {
  file="$SCRIPT_DIR/CLI_VERSION"
  [ -f "$file" ] || return 1
  value=$(tr -d ' \t\r\n' < "$file")
  case "$value" in
    '' | *[!0-9.]* ) return 1 ;;
  esac
  # SemVer core: exactly three numeric components, none empty.
  printf '%s' "$value" | awk -F. '{ exit !(NF == 3 && $1 != "" && $2 != "" && $3 != "") }' || return 1
  printf '%s' "$value"
}

# --- target detection ---------------------------------------------------------
detect_target() {
  [ -n "${LOAM_TARGET:-}" ] && { printf '%s' "$LOAM_TARGET"; return 0; }
  os=$(uname -s 2>/dev/null || printf 'unknown')
  arch=$(uname -m 2>/dev/null || printf 'unknown')
  case "$os" in
    Darwin)
      case "$arch" in
        arm64 | aarch64) printf 'aarch64-apple-darwin' ;;
        x86_64) printf 'x86_64-apple-darwin' ;;
        *) printf '%s-unknown-darwin' "$arch" ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64) printf 'x86_64-unknown-linux-musl' ;;
        aarch64 | arm64) printf 'aarch64-unknown-linux-musl' ;;
        *) printf '%s-unknown-linux-musl' "$arch" ;;
      esac
      ;;
    MINGW* | MSYS* | CYGWIN*) printf 'x86_64-pc-windows-msvc' ;;
    *) printf '%s-unknown-%s' "$arch" "$os" ;;
  esac
}

is_supported_target() {
  for candidate in $SUPPORTED_TARGETS; do
    [ "$candidate" = "$1" ] && return 0
  done
  return 1
}

runtime_binary() {
  case "$2" in
    *windows*) printf '%s/bin/%s/%s/loam.exe' "$RUNTIME_ROOT" "$1" "$2" ;;
    *) printf '%s/bin/%s/%s/loam' "$RUNTIME_ROOT" "$1" "$2" ;;
  esac
}

# --- download helpers ---------------------------------------------------------
# file:// support exists so release fixtures can be tested without a network.
fetch() {
  url=$1 destination=$2
  case "$url" in
    file://*) cp "${url#file://}" "$destination" 2>/dev/null ;;
    *)
      if command -v curl > /dev/null 2>&1; then
        curl --fail --silent --show-error --location --max-time 120 --output "$destination" "$url"
      elif command -v wget > /dev/null 2>&1; then
        wget --quiet --timeout=120 --output-document "$destination" "$url"
      else
        echo "loam: neither curl nor wget is available" >&2
        return 1
      fi
      ;;
  esac
}

sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum > /dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    return 1
  fi
}

# manifest_field <manifest> <target> <field>
# The manifest is a small fixed-shape document; a targeted scan beats adding a
# JSON dependency to a bootstrap script that must run before anything is installed.
manifest_field() {
  awk -v target="$2" -v field="$3" '
    { gsub(/[{}\[\]]/, " ") }
    {
      n = split($0, entries, /"target"[[:space:]]*:/)
      for (i = 2; i <= n; i++) {
        if (match(entries[i], /"[^"]+"/) == 0) continue
        found = substr(entries[i], RSTART + 1, RLENGTH - 2)
        if (found != target) continue
        pattern = "\"" field "\"[[:space:]]*:[[:space:]]*\"[^\"]+\""
        if (match(entries[i], pattern) == 0) continue
        value = substr(entries[i], RSTART, RLENGTH)
        sub(/^[^:]*:[[:space:]]*"/, "", value)
        sub(/"$/, "", value)
        print value
        exit
      }
    }
  ' "$1"
}

# --- bootstrap ----------------------------------------------------------------
bootstrap() {
  version=$1 target=$2
  binary=$(runtime_binary "$version" "$target")
  [ -x "$binary" ] && return 0

  marker="$RUNTIME_ROOT/bin/$version/$target.installing"
  mkdir -p "$RUNTIME_ROOT/bin/$version" || return 1
  if [ -e "$marker" ]; then
    # Reclaim a marker left behind by an interrupted installer.
    age=$(( $(date +%s) - $(marker_mtime "$marker") ))
    [ "$age" -lt "$MARKER_STALE_SECONDS" ] && return 0
    rm -f "$marker"
  fi
  # ponytail: `set -C` makes the marker create-exclusive, which is the atomic
  # primitive we need; a lock daemon buys nothing for a once-per-version download.
  ( set -C; : > "$marker" ) 2>/dev/null || return 0
  # shellcheck disable=SC2064
  trap "rm -f '$marker'" EXIT INT TERM

  base=${LOAM_RELEASE_BASE_URL:-"$REPO_RELEASE_BASE/cli-v$version"}
  staging=$(mktemp -d "${TMPDIR:-/tmp}/loam-install.XXXXXX") || return 1
  status=1

  if fetch "$base/loam-runtime-manifest.json" "$staging/manifest.json"; then
    file=$(manifest_field "$staging/manifest.json" "$target" file)
    expected=$(manifest_field "$staging/manifest.json" "$target" sha256)
    if [ -z "$file" ] || [ -z "$expected" ]; then
      echo "loam: manifest has no runtime for target $target" >&2
    elif fetch "$base/$file" "$staging/$file"; then
      actual=$(sha256_of "$staging/$file")
      if [ "$actual" != "$expected" ]; then
        echo "loam: checksum mismatch for $file (expected $expected, got $actual)" >&2
      else
        chmod +x "$staging/$file"
        mkdir -p "${binary%/*}"
        # Publish only a fully verified executable, and never remove an
        # existing runtime first.
        if publish "$staging/$file" "$binary"; then
          status=0
        else
          echo "loam: could not publish runtime to $binary" >&2
        fi
      fi
    else
      echo "loam: download failed: $base/$file" >&2
    fi
  else
    echo "loam: runtime manifest unavailable: $base/loam-runtime-manifest.json" >&2
  fi

  rm -rf "$staging"
  rm -f "$marker"
  trap - EXIT INT TERM
  return $status
}

marker_mtime() {
  stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null || echo 0
}

# Windows hosts hold sharing locks on running executables; retry with bounded
# backoff and fail closed rather than deleting a runtime that is in use.
publish() {
  source=$1 destination=$2
  attempt=1
  while [ "$attempt" -le 5 ]; do
    if mv -f "$source" "$destination" 2>/dev/null; then
      return 0
    fi
    [ -x "$destination" ] && return 0
    sleep "$attempt"
    attempt=$((attempt + 1))
  done
  return 1
}

# Hand off to a detached installer: returns immediately, closes stdin, and keeps
# installer chatter out of hook output.
start_background_bootstrap() {
  [ -n "${LOAM_NO_BOOTSTRAP:-}" ] && return 0
  mkdir -p "$RUNTIME_ROOT" 2>/dev/null || return 0
  # Bound the log so a repeatedly failing install cannot grow without limit.
  if [ -f "$INSTALL_LOG" ] && [ "$(wc -c < "$INSTALL_LOG" 2>/dev/null || echo 0)" -gt 1048576 ]; then
    rm -f "$INSTALL_LOG"
  fi
  ( "$SCRIPT_DIR/loam.sh" --loam-bootstrap < /dev/null >> "$INSTALL_LOG" 2>&1 & ) &
}

# --- main ---------------------------------------------------------------------
ensure_agents_gitignore

version=$(read_version) || {
  echo "loam: CLI_VERSION is missing, empty, or not valid SemVer at $SCRIPT_DIR/CLI_VERSION" >&2
  exit 78
}
target=$(detect_target)

if ! is_supported_target "$target"; then
  echo "loam: unsupported platform target: $target" >&2
  exit 78
fi

binary=$(runtime_binary "$version" "$target")

case "${1:-}" in
  --loam-runtime-path)
    printf '%s\n' "$binary"
    exit 0
    ;;
  --loam-bootstrap)
    bootstrap "$version" "$target"
    exit $?
    ;;
esac

if [ -x "$binary" ]; then
  exec "$binary" "$@"
fi

start_background_bootstrap
echo "loam: native runtime $version ($target) is temporarily unavailable; retry shortly" >&2
exit 75
