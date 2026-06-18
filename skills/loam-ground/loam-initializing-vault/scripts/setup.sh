#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL_DIR="$(dirname "$SCRIPT_DIR")"
ASSETS="$SKILL_DIR/assets/obsidian"

REGISTRY_URL="https://raw.githubusercontent.com/obsidianmd/obsidian-releases/master/community-plugins.json"

usage() {
  echo "Usage: setup.sh <vault-path>"
  echo ""
  echo "  vault-path   Directory to create or scaffold (will be created if absent)"
  echo ""
  echo "Example:"
  echo "  setup.sh ~/Nextcloud/MyVault"
  exit 1
}

[[ $# -lt 1 ]] && usage

TARGET="${1%/}"
TARGET="${TARGET/#\~/$HOME}"

if [[ -f "$TARGET" ]]; then
  echo "Error: $TARGET is a file, not a directory." >&2
  exit 1
fi

OBSIDIAN_DIR="$TARGET/.obsidian"

if [[ -d "$OBSIDIAN_DIR" ]]; then
  echo "Warning: $OBSIDIAN_DIR already exists. Files will be overwritten."
  read -rp "Continue? [y/N] " confirm
  [[ "$confirm" =~ ^[Yy]$ ]] || { echo "Aborted."; exit 0; }
fi

for cmd in curl jq; do
  command -v "$cmd" &>/dev/null || { echo "Error: $cmd is required but not found." >&2; exit 1; }
done

echo "Setting up vault at: $TARGET"
mkdir -p "$OBSIDIAN_DIR/plugins"

# Copy root config files
cp "$ASSETS"/*.json "$OBSIDIAN_DIR/"

# Download and install community plugins
echo ""
echo "Fetching plugin registry..."
registry=$(curl -sfL "$REGISTRY_URL") || { echo "Error: could not fetch plugin registry." >&2; exit 1; }

plugin_ids=$(jq -r '.[]' "$ASSETS/community-plugins.json")

echo "Installing plugins..."
while IFS= read -r plugin_id; do
  repo=$(echo "$registry" | jq -r --arg id "$plugin_id" '.[] | select(.id == $id) | .repo // empty')

  if [[ -z "$repo" ]]; then
    echo "  skip: $plugin_id (not found in registry)"
    continue
  fi

  # Pin to the version in our stored manifest, fall back to latest
  version=""
  if [[ -f "$ASSETS/plugins/$plugin_id/manifest.json" ]]; then
    version=$(jq -r '.version // empty' "$ASSETS/plugins/$plugin_id/manifest.json")
  fi

  if [[ -n "$version" ]]; then
    base_url="https://github.com/$repo/releases/download/$version"
  else
    base_url="https://github.com/$repo/releases/latest/download"
  fi

  plugin_dir="$OBSIDIAN_DIR/plugins/$plugin_id"
  mkdir -p "$plugin_dir"

  ok=true
  curl -sfL "$base_url/manifest.json" -o "$plugin_dir/manifest.json" || ok=false
  curl -sfL "$base_url/main.js"       -o "$plugin_dir/main.js"       || ok=false
  curl -sfL "$base_url/styles.css"    -o "$plugin_dir/styles.css" 2>/dev/null || true  # optional

  if $ok; then
    # Restore saved plugin settings
    if [[ -f "$ASSETS/plugins/$plugin_id/data.json" ]]; then
      cp "$ASSETS/plugins/$plugin_id/data.json" "$plugin_dir/data.json"
    fi
    echo "  installed: $plugin_id${version:+ @ $version}"
  else
    rm -rf "$plugin_dir"
    echo "  failed:    $plugin_id (download error — install manually)"
  fi
done <<< "$plugin_ids"

# Register vault in Obsidian's global vaults list
register_vault() {
  local vault_path="$1"
  local obsidian_cfg

  if [[ -f "$HOME/.config/obsidian/obsidian.json" ]]; then
    obsidian_cfg="$HOME/.config/obsidian/obsidian.json"
  elif [[ -f "$HOME/Library/Application Support/obsidian/obsidian.json" ]]; then
    obsidian_cfg="$HOME/Library/Application Support/obsidian/obsidian.json"
  else
    echo "  (could not find obsidian.json — add the vault manually in Obsidian)"
    return
  fi

  if jq -e --arg p "$vault_path" '.vaults[] | select(.path == $p)' "$obsidian_cfg" &>/dev/null; then
    echo "  Vault already registered in Obsidian."
    return
  fi

  local key
  while true; do
    key=$(openssl rand -hex 8 2>/dev/null || cat /proc/sys/kernel/random/uuid 2>/dev/null | tr -d '-' | head -c 16)
    jq -e --arg k "$key" '.vaults[$k]' "$obsidian_cfg" &>/dev/null || break
  done

  local ts
  ts=$(date +%s%3N)

  local tmp
  tmp="$(mktemp)"
  jq --arg k "$key" --arg p "$vault_path" --argjson ts "$ts" \
    '.vaults[$k] = {path: $p, ts: $ts}' "$obsidian_cfg" > "$tmp" && mv "$tmp" "$obsidian_cfg"

  echo "  Registered vault in Obsidian (id: $key)"
}

echo ""
register_vault "$(cd "$TARGET" && pwd)"

echo ""
echo "Done. Open Obsidian — the vault will appear in the switcher with all plugins installed."
echo ""
echo "Fonts used: SF Pro Text, Liga SFMono Nerd Font, Victor Mono, Iosevka Term SS07"
echo "Install any that are missing if you want the appearance to match exactly."
