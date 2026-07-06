# loam-common.sh — shared helpers for loam bash scripts.
# Source from another script:
#   source "$(dirname "${BASH_SOURCE[0]}")/loam-common.sh"
#
# Functions return non-zero instead of exiting, so the caller decides how to
# handle failure. A shared lib that kills its caller is a footgun.

# validate_wiki_root <wiki-root>
#   0  — root holds the wiki contract (SCHEMA.md | index.md | log.md)
#   2  — not found / contract missing (message on stderr, incl. "did you mean
#        .../wiki" hint when the contract lives one level down)
validate_wiki_root() {
  local wiki_root="$1"
  [[ -z "$wiki_root" || ! -d "$wiki_root" ]] && { echo "Error: wiki root not found: $wiki_root" >&2; return 2; }

  if [[ -f "$wiki_root/SCHEMA.md" || -f "$wiki_root/index.md" || -f "$wiki_root/log.md" ]]; then
    return 0
  fi

  if [[ -f "$wiki_root/wiki/SCHEMA.md" || -f "$wiki_root/wiki/index.md" || -f "$wiki_root/wiki/log.md" ]]; then
    echo "Error: wiki root contract not found: $wiki_root; did you mean: $wiki_root/wiki" >&2
    return 2
  fi

  echo "Error: wiki root contract not found: $wiki_root" >&2
  return 2
}
