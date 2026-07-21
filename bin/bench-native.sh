#!/usr/bin/env bash
# bench-native.sh — release-build benchmark harness for the native runtime and
# the launcher/bootstrap path.
#
# Two warmups plus ten measured runs per case; reports fixture size, mean, and
# p95 in milliseconds. Compilation time is excluded: build first.
#
# Usage:
#   bin/bench-native.sh [<extra-codebase-root> ...]
set -uo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
LOAM="$ROOT/target/release/loam"
LAUNCHER="$ROOT/skills/loam-using/scripts/loam.sh"
WARMUPS=2
RUNS=10

[[ -x "$LOAM" ]] || { echo "build first: cargo build --release --workspace" >&2; exit 1; }

now_ms() { date +%s%3N; }

# measure <label> <fixture-size> -- <command...>
measure() {
  local label="$1" size="$2"
  shift 3
  local i
  for ((i = 0; i < WARMUPS; i++)); do "$@" > /dev/null 2>&1; done
  local -a samples=()
  for ((i = 0; i < RUNS; i++)); do
    local start end
    start=$(now_ms)
    "$@" > /dev/null 2>&1
    end=$(now_ms)
    samples+=($((end - start)))
  done
  printf '%s\n' "${samples[@]}" | sort -n | awk -v label="$label" -v size="$size" -v runs="$RUNS" '
    { values[NR] = $1; total += $1 }
    END {
      # p95 of a sorted sample: ceil(0.95 * n).
      index95 = int(0.95 * runs + 0.999999)
      if (index95 < 1) index95 = 1
      printf "| %-46s | %8s | %8.1f | %6d |\n", label, size, total / runs, values[index95]
    }
  '
}

echo "| case                                           | fixture  |  mean ms | p95 ms |"
echo "| ---------------------------------------------- | -------- | -------: | -----: |"

# --- synthetic fixtures -------------------------------------------------------
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Small: 25 source files, 25 code pages. Large: 2000 / 2000.
build_fixture() {
  local base="$1" count="$2"
  mkdir -p "$base/src" "$base/wiki/code"
  : > "$base/wiki/SCHEMA.md"
  local i
  for ((i = 0; i < count; i++)); do
    printf 'fn item_%d() { let value = %d; }\n' "$i" "$i" > "$base/src/file_$i.rs"
    printf -- '---\nsource_path: src/file_%d.rs\ningested_at: "1"\nsource_size: "%d"\ncontent_hash: "deadbeef"\n---\n\n# page\n' \
      "$i" "$(wc -c < "$base/src/file_$i.rs")" > "$base/wiki/code/src-file-$i-rs.md"
  done
}

build_fixture "$tmp/small" 25
build_fixture "$tmp/large" 2000

for scale in small large; do
  root="$tmp/$scale"
  count=$(find "$root/src" -type f | wc -l | tr -d ' ')
  measure "codegraph index ($scale)" "$count files" -- \
    "$LOAM" codegraph index "$root/wiki" --codebase-root "$root"
  measure "codegraph walk ($scale)" "$count files" -- \
    "$LOAM" codegraph walk "$root"
  measure "codegraph diff ($scale)" "$count files" -- \
    "$LOAM" codegraph diff "$root" "$root/wiki"
  measure "codegraph diff --strict ($scale)" "$count files" -- \
    "$LOAM" codegraph diff "$root" "$root/wiki" --strict
  measure "state --fast ($scale)" "$count files" -- \
    "$LOAM" state --fast "$root"
done

# --- checkpoint + version gate ------------------------------------------------
# Checkpoint notes are small by nature; the large fixture is a deliberate
# worst case (50 workstreams, 200 pointers) rather than a realistic one.
build_note() {
  local path="$1" workstreams="$2" pointers_per="$3" base="$4"
  {
    printf '# Checkpoint\n\n'
    printf -- '- Captured: 2026-07-21 09:00 +02:00\n- Reason: pause\n- Scope: benchmark fixture\n- Format: v1\n- Intended return: resume the benchmark\n\n'
    printf '## Workstreams\n\n'
    local i j
    for ((i = 0; i < workstreams; i++)); do
      printf '### Workstream %d\n- Status: active\n- Next: keep going\n- Pointers:' "$i"
      for ((j = 0; j < pointers_per; j++)); do
        [[ $j -gt 0 ]] && printf ','
        printf ' %s/src/file_%d.rs' "$base" "$j"
      done
      printf '\n'
    done
  } > "$path"
}

build_note "$tmp/note-small.md" 1 2 "$tmp/small"
build_note "$tmp/note-large.md" 50 4 "$tmp/large"

measure "checkpoint verify (small note)" "1 ws / 2 ptr" -- \
  "$LOAM" checkpoint verify "$tmp/note-small.md"
measure "checkpoint verify (large note)" "50 ws / 200 ptr" -- \
  "$LOAM" checkpoint verify "$tmp/note-large.md"

for scale in small large; do
  count=$(find "$tmp/$scale" -type f | wc -l | tr -d ' ')
  measure "checkpoint state ($scale)" "$count files" -- \
    "$LOAM" checkpoint state --window 180 "$tmp/$scale"
done

measure "check versions (repo)" "7 values" -- "$LOAM" check versions "$ROOT"

# The work-lint path is untouched by this change; measured so a regression
# would be visible rather than assumed absent.
measure "lint --only work (repo)" "$(find "$ROOT/goals" -name '*.md' | wc -l | tr -d ' ') goals" -- \
  "$LOAM" lint --only work "$ROOT"

# --- launcher / bootstrap path ------------------------------------------------
scope="$tmp/scope/.agents/skills/loam-using/scripts"
mkdir -p "$scope"
cp "$LAUNCHER" "$scope/loam.sh"
chmod +x "$scope/loam.sh"
printf '0.0.1\n' > "$scope/CLI_VERSION"
ready="$tmp/scope/.agents/loam/bin/0.0.1/x86_64-unknown-linux-musl"
mkdir -p "$ready"
cp "$LOAM" "$ready/loam"

export LOAM_TARGET=x86_64-unknown-linux-musl
measure "launcher scope resolution" "n/a" -- "$scope/loam.sh" --loam-runtime-path
measure "launcher dispatch overhead (state --fast)" "25 files" -- \
  "$scope/loam.sh" state --fast "$tmp/small"
measure "native state --fast (no launcher)" "25 files" -- \
  "$LOAM" state --fast "$tmp/small"

# Local release fixture: measures manifest parse + download + SHA-256 verify +
# atomic publish without network variance.
release="$tmp/release"
mkdir -p "$release"
cp "$LOAM" "$release/loam-x86_64-unknown-linux-musl"
digest=$(sha256sum "$release/loam-x86_64-unknown-linux-musl" | awk '{print $1}')
printf '{"version":"0.0.1","runtimes":[{"target":"x86_64-unknown-linux-musl","file":"loam-x86_64-unknown-linux-musl","sha256":"%s"}]}\n' \
  "$digest" > "$release/loam-runtime-manifest.json"
artifact_size=$(du -h "$release/loam-x86_64-unknown-linux-musl" | awk '{print $1}')

bootstrap_once() {
  local target="$tmp/bootstrap-run"
  rm -rf "$target"
  mkdir -p "$target/.agents/skills/loam-using/scripts"
  cp "$LAUNCHER" "$target/.agents/skills/loam-using/scripts/loam.sh"
  chmod +x "$target/.agents/skills/loam-using/scripts/loam.sh"
  printf '0.0.1\n' > "$target/.agents/skills/loam-using/scripts/CLI_VERSION"
  LOAM_RELEASE_BASE_URL="file://$release" "$target/.agents/skills/loam-using/scripts/loam.sh" --loam-bootstrap
}
measure "bootstrap: manifest+verify+publish" "$artifact_size" -- bootstrap_once
export -f bootstrap_once 2> /dev/null || true

# --- legacy shell comparison --------------------------------------------------
legacy_codegraph="$ROOT/skills/loam-memory/loam-ingesting-codebase/scripts/codegraph-legacy.sh"
legacy_state="$ROOT/skills/loam-using/scripts/loamstate-legacy.sh"
if [[ -n "${BENCH_LEGACY:-}" && -x "$legacy_codegraph" ]]; then
  measure "codegraph-legacy.sh index (large)" "2000 files" -- \
    bash "$legacy_codegraph" index "$tmp/large/wiki" --codebase-root "$tmp/large"
  measure "codegraph-legacy.sh diff (large)" "2000 files" -- \
    bash "$legacy_codegraph" diff "$tmp/large" "$tmp/large/wiki"
fi
if [[ -n "${BENCH_LEGACY:-}" && -x "$legacy_state" ]]; then
  measure "loamstate-legacy.sh --fast (large)" "2000 files" -- \
    bash "$legacy_state" --fast "$tmp/large"
fi

legacy_verify="$ROOT/skills/loam-work/loam-checkpointing/scripts/checkpoint-verify-legacy"
legacy_capture="$ROOT/skills/loam-work/loam-checkpointing/scripts/checkpoint-state-legacy"
if [[ -n "${BENCH_LEGACY:-}" && -f "$legacy_verify" ]]; then
  measure "checkpoint-verify-legacy (small note)" "1 ws / 2 ptr" -- \
    bash "$legacy_verify" "$tmp/note-small.md"
  measure "checkpoint-verify-legacy (large note)" "50 ws / 200 ptr" -- \
    bash "$legacy_verify" "$tmp/note-large.md"
fi
if [[ -n "${BENCH_LEGACY:-}" && -f "$legacy_capture" ]]; then
  # The legacy digest takes its root from WORKSPACE, never from an argument.
  measure "checkpoint-state-legacy (large)" "2000 files" -- \
    env WORKSPACE="$tmp/large" bash "$legacy_capture" --window 180
fi
