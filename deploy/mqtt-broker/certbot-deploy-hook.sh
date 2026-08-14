#!/usr/bin/env bash
# certbot deploy-hook (T2): reload ONLY the Loam broker after a mqtt.example.org renewal.
# Installed on the host into /etc/letsencrypt/renewal-hooks/deploy/.
# Never bounces any other service on the host — reload, not restart.
#
# certbot sets $RENEWED_LINEAGE for the cert that renewed; act only for our FQDN.
set -euo pipefail

: "${BROKER_FQDN:=mqtt.example.org}"

# Only react to our own lineage; ignore every other cert on this shared host.
case "${RENEWED_LINEAGE:-}" in
  */"${BROKER_FQDN}") ;;
  *) exit 0 ;;
esac

# Reload (graceful) — not restart, and only this unit.
systemctl reload loam-mosquitto.service 2>/dev/null \
  || systemctl reload-or-restart loam-mosquitto.service
