#!/usr/bin/env bash
# bump-release.sh — set a product version across the surfaces that carry it.
#
# Loam releases two things independently from one repository:
#
#   --plugin  <version>   package.json
#                         .claude-plugin/marketplace.json  metadata.version
#                         .claude-plugin/marketplace.json  plugins[0].version
#                         .codex-plugin/plugin.json
#                         .cursor-plugin/plugin.json
#                         -> released as tag  v<version>
#
#   --runtime <version>   cli/Cargo.toml   [package] version
#                         skills/loam-using/scripts/CLI_VERSION
#                         -> released as tag  cli-v<version>   (only cli-v*
#                            triggers the dist / raw-runtime build)
#
# The two versions are deliberately NOT kept equal. A plugin-only change must
# not force a runtime release, and a runtime release must not churn the version
# every harness displays. `CLI_VERSION` is the one value that escapes the repo:
# the launcher interpolates it into the release URL and the on-disk runtime
# path, so it must name a published cli-v<version> release.
#
# A coordinated release is just both lanes run in either order.
#
# Usage:
#   bin/bump-release.sh --plugin  0.8.3
#   bin/bump-release.sh --runtime 0.9.0
#
# This is an explicit operator command, never a pre-commit side effect: a hook
# that rewrites version metadata behind the committer is how the Codex and
# Cursor manifests silently drifted to 0.1.0 in the first place. Pre-commit
# stays verification-only.
#
# Safety model:
#   - refuses to run on a dirty worktree, so the bump is the only change
#   - requires the target domain to already agree BEFORE editing, so the
#     outgoing version is unambiguous
#   - stages every edit in a temp directory and verifies the exact expected
#     number of literal replacements per file; nothing is written in place
#     until every file in the lane is known good
#   - re-runs the domain check afterwards and reports the result
#
# Exit codes: 0 bumped and verified; 1 refused or failed (no partial writes).
set -uo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
LOAM="${LOAM_NATIVE_BIN:-$ROOT/target/release/loam}"

fail() { echo "bump-release: FAIL: $1" >&2; exit 1; }

show_usage() {
  echo "Usage: bin/bump-release.sh --plugin <version>"
  echo "       bin/bump-release.sh --runtime <version>"
  echo
  echo "  --plugin   package.json, both marketplace fields, Codex, Cursor"
  echo "             released as tag v<version>"
  echo "  --runtime  cli/Cargo.toml, CLI_VERSION"
  echo "             released as tag cli-v<version> (triggers dist)"
  echo
  echo "The plugin and runtime versions are independent; neither implies the other."
}

if [[ $# -eq 0 ]]; then
  show_usage
  exit 1
fi
if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  show_usage
  exit 0
fi

DOMAIN=""
case "${1:-}" in
  --plugin)  DOMAIN="plugin" ;;
  --runtime) DOMAIN="runtime" ;;
  *) fail "first argument must be --plugin or --runtime (got '${1:-}')" ;;
esac
shift

NEW="${1:-}"
[[ -n "$NEW" ]] || fail "--$DOMAIN requires a version"
shift
[[ $# -eq 0 ]] || fail "unexpected argument: $1"

# Strict SemVer core only. Prerelease and build metadata are rejected on
# purpose: the release lane derives the tag, the runtime manifest URL, and the
# scope-derived runtime directory from this string.
if [[ ! "$NEW" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  fail "not a strict SemVer version: '$NEW' (expected MAJOR.MINOR.PATCH, no prerelease or build metadata, no leading zeros)"
fi

git -C "$ROOT" rev-parse --git-dir > /dev/null 2>&1 || fail "not a git repository: $ROOT"
if [[ -n "$(git -C "$ROOT" status --porcelain)" ]]; then
  fail "worktree is dirty; commit or stash first so the bump is the only change"
fi

[[ -x "$LOAM" ]] || fail "native runtime not built: cargo build --release --workspace"

# Agreement within the target domain before the edit means the outgoing version
# is a single known value rather than whichever file we happened to read.
if ! "$LOAM" check versions "$ROOT" "--$DOMAIN" > /dev/null 2>&1; then
  echo "bump-release: refusing to bump while the $DOMAIN version disagrees:" >&2
  "$LOAM" check versions "$ROOT" "--$DOMAIN" >&2 || true
  exit 1
fi

CLI_VERSION_FILE="skills/loam-using/scripts/CLI_VERSION"

read_current() {
  if [[ "$DOMAIN" == "runtime" ]]; then
    tr -d ' \t\r\n' < "$ROOT/$CLI_VERSION_FILE"
  else
    # package.json is the plugin reference; the domain check above already
    # proved the other four agree with it.
    sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$ROOT/package.json" | head -1
  fi
}

OLD="$(read_current)"
[[ -n "$OLD" ]] || fail "cannot read the current $DOMAIN version"
[[ "$NEW" != "$OLD" ]] || fail "$DOMAIN is already at $OLD; nothing to bump"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# Literal (non-regex) replace-and-count. Version strings contain dots, which a
# regex would treat as wildcards.
stage_literal() {
  local file="$1" expected="$2" find="$3" repl="$4"
  local src="$ROOT/$file"
  [[ -f "$src" ]] || fail "missing $file"

  local found
  found="$(grep -F -o -- "$find" "$src" | wc -l | tr -d ' ')"
  [[ "$found" == "$expected" ]] \
    || fail "$file: expected $expected occurrence(s) of '$find', found $found"

  awk -v find="$find" -v repl="$repl" '
    {
      line = $0
      out = ""
      while ((p = index(line, find)) > 0) {
        out = out substr(line, 1, p - 1) repl
        line = substr(line, p + length(find))
      }
      print out line
    }
  ' "$src" > "$STAGE/$(printf '%s' "$file" | tr '/' '_')" || fail "$file: rewrite failed"
}

# Cargo.toml needs a whole-line match: a dependency pin such as
# `chrono = { version = "0.4" }` would otherwise collide with a runtime
# version of 0.4.
stage_cargo_line() {
  local file="cli/Cargo.toml"
  local src="$ROOT/$file"
  [[ -f "$src" ]] || fail "missing $file"

  local found
  found="$(awk -v want="version = \"$OLD\"" '$0 == want { n++ } END { print n + 0 }' "$src")"
  [[ "$found" == "1" ]] \
    || fail "$file: expected exactly 1 line 'version = \"$OLD\"', found $found"

  awk -v want="version = \"$OLD\"" -v repl="version = \"$NEW\"" '
    $0 == want { print repl; next }
    { print }
  ' "$src" > "$STAGE/cli_Cargo.toml" || fail "$file: rewrite failed"
}

if [[ "$DOMAIN" == "plugin" ]]; then
  stage_literal "package.json"                    1 "\"version\": \"$OLD\"" "\"version\": \"$NEW\""
  stage_literal ".claude-plugin/marketplace.json" 2 "\"version\": \"$OLD\"" "\"version\": \"$NEW\""
  stage_literal ".codex-plugin/plugin.json"       1 "\"version\": \"$OLD\"" "\"version\": \"$NEW\""
  stage_literal ".cursor-plugin/plugin.json"      1 "\"version\": \"$OLD\"" "\"version\": \"$NEW\""
else
  stage_cargo_line
  printf '%s\n' "$NEW" > "$STAGE/CLI_VERSION" || fail "cannot stage $CLI_VERSION_FILE"
fi

# Every staged file must be non-empty, or an edit silently truncated something.
for staged in "$STAGE"/*; do
  [[ -s "$staged" ]] || fail "staged file is empty: $(basename "$staged")"
done

# Point of no return: everything above validated, now publish.
publish() {
  local staged="$1" target="$2"
  cat "$staged" > "$ROOT/$target" || fail "cannot write $target"
}

if [[ "$DOMAIN" == "plugin" ]]; then
  publish "$STAGE/package.json"                    "package.json"
  publish "$STAGE/.claude-plugin_marketplace.json" ".claude-plugin/marketplace.json"
  publish "$STAGE/.codex-plugin_plugin.json"       ".codex-plugin/plugin.json"
  publish "$STAGE/.cursor-plugin_plugin.json"      ".cursor-plugin/plugin.json"
  TAG="v$NEW"
else
  publish "$STAGE/cli_Cargo.toml"  "cli/Cargo.toml"
  publish "$STAGE/CLI_VERSION"     "$CLI_VERSION_FILE"
  TAG="cli-v$NEW"
fi

echo "bump-release: $DOMAIN $OLD -> $NEW"

if ! "$LOAM" check versions "$ROOT" "--$DOMAIN"; then
  fail "post-bump verification failed; inspect the working tree before committing"
fi

if [[ "$DOMAIN" == "runtime" ]]; then
  cat <<NEXT

Next:
  1. cargo build --release --workspace   # Cargo.lock records the new version
  2. review: git diff
  3. commit, then tag $TAG at the release commit and push the tag
     (cli-v* is what triggers the dist / raw-runtime build)
  4. bin/check-release-resolution.sh     # requires the published manifest
NEXT
else
  cat <<NEXT

Next:
  1. review: git diff
  2. commit, then tag $TAG at the release commit and push the tag
     (v* is a plugin release and does not build a runtime)
  3. CLI_VERSION is unchanged and still points at its published runtime;
     bump it separately with --runtime only if the runtime actually changed
NEXT
fi
