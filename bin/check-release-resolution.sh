#!/usr/bin/env bash
# check-release-resolution.sh — strict verification that the CLI_VERSION every
# copied launcher requests actually resolves to a published runtime manifest.
#
# This gate is about the RUNTIME only. `CLI_VERSION` is the version the copied
# launcher requests, and runtime releases are tagged `cli-v<version>`, so that
# is the release this resolves against. Plugin releases (`v<version>`) publish
# no runtime and are deliberately out of scope here.
#
# Version agreement itself is `loam check versions --runtime`, with no python3
# dependency. This script keeps the network half, which must never become a
# pre-commit concern.
#
# Exit codes: 0 the runtime version agrees and the manifest resolves;
#             1 runtime version drift (always a hard failure);
#             2 the requested manifest is not published yet.
set -uo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
RELEASE_BASE="${LOAM_RELEASE_BASE_URL_ROOT:-https://github.com/scchearn/loam/releases/download}"
LOAM="${LOAM_NATIVE_BIN:-$ROOT/target/release/loam}"

fail() { echo "release resolution: FAIL: $1" >&2; exit 1; }

[[ -x "$LOAM" ]] || fail "native runtime not built: cargo build --release --workspace"

"$LOAM" check versions "$ROOT" --runtime || exit 1

runtime_version=$(tr -d ' \t\r\n' < "$ROOT/skills/loam-using/scripts/CLI_VERSION")

if [[ "${1:-}" == "--versions-only" ]]; then
  exit 0
fi

manifest_url="$RELEASE_BASE/cli-v$runtime_version/loam-runtime-manifest.json"
manifest=$(curl --fail --silent --show-error --location --max-time 60 "$manifest_url" 2>&1) || {
  echo "release resolution: PENDING: CLI_VERSION $runtime_version has no published runtime manifest" >&2
  echo "release resolution: expected $manifest_url" >&2
  exit 2
}

# Every supported target must be present, or the launcher would exit 75 forever
# on a platform the release claims to support.
for target in x86_64-apple-darwin aarch64-apple-darwin x86_64-pc-windows-msvc \
  x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  grep -Fq "\"$target\"" <<< "$manifest" \
    || fail "published manifest for cli-v$runtime_version is missing target $target"
done

echo "release resolution: manifest PASS ($manifest_url)"
