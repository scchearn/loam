#!/usr/bin/env bash
# Broker backup/restore (T6). Scope = broker-owned data ONLY: the persistence DB
# (${MOSQ_PERSIST}) and the org-CA + client-cert material (${PKI_DIR}). The certbot
# SERVER cert is NOT backed up here — certbot manages/renews it and it is in the
# host's own backups. Never touches any other service's data or the host backup regime.
#
# subcommands:
#   backup            -> ${BACKUP_DIR}/loam-mqtt-<ts>.tgz
#   restore <archive> -> restore into ${MOSQ_PERSIST} and ${PKI_DIR}
#   selfcheck
set -euo pipefail

backup() {
  : "${MOSQ_PERSIST:?}" "${PKI_DIR:?}" "${BACKUP_DIR:?}"
  mkdir -p "$BACKUP_DIR"
  local out="$BACKUP_DIR/loam-mqtt-$(date +%Y%m%d-%H%M%S).tgz"
  # NOTE: stop or checkpoint the broker first so the persistence DB is consistent:
  #   systemctl stop loam-mosquitto.service   (or send SIGUSR1 to persist)
  tar -czf "$out" -C / "${MOSQ_PERSIST#/}" "${PKI_DIR#/}"
  echo "$out"
}

restore() {
  : "${MOSQ_PERSIST:?}" "${PKI_DIR:?}"
  local archive="${1:?usage: restore <archive.tgz>}"
  [ -f "$archive" ] || { echo "no such archive: $archive" >&2; exit 1; }
  tar -xzf "$archive" -C /
  echo "restored from $archive (restart loam-mosquitto to load)"
}

selfcheck() {
  local t; t="$(mktemp -d)"; trap 'rm -rf "$t"' RETURN
  export MOSQ_PERSIST="$t/persist" PKI_DIR="$t/pki" BACKUP_DIR="$t/backups"
  mkdir -p "$MOSQ_PERSIST" "$PKI_DIR"
  # observe retained state BEFORE backup (non-empty sentinel — not an empty DB)
  echo "retained-sentinel-$RANDOM" > "$MOSQ_PERSIST/mosquitto.db"
  echo "org-ca-key-material" > "$PKI_DIR/ca.crt"
  local want; want="$(cat "$MOSQ_PERSIST/mosquitto.db")"
  local arc; arc="$(backup)"
  # lose the data
  rm -rf "$MOSQ_PERSIST" "$PKI_DIR"
  restore "$arc" >/dev/null
  [ "$(cat "$MOSQ_PERSIST/mosquitto.db")" = "$want" ] || { echo "FAIL: persistence not restored"; return 1; }
  [ -f "$PKI_DIR/ca.crt" ] || { echo "FAIL: org CA not restored"; return 1; }
  echo "T6 selfcheck PASS"
}

case "${1:-}" in
  backup)    backup ;;
  restore)   restore "$2" ;;
  selfcheck) selfcheck ;;
  *) echo "usage: $0 {backup|restore <archive>|selfcheck}" >&2; exit 2 ;;
esac
