#!/usr/bin/env bash
# Obtain/renew the broker SERVER cert via the HOST'S EXISTING certbot (T4/T9).
# DNS-01 through Cloudflare, reusing the invoice host's certbot setup. Does NOT
# install a new ACME client. Host-side, run at provisioning; not in selfcheck.
set -euo pipefail

: "${BROKER_FQDN:?set BROKER_FQDN}"
: "${CERTBOT_CLOUDFLARE_INI:?set CERTBOT_CLOUDFLARE_INI (path to existing creds)}"
hook="$(cd "$(dirname "$0")/.." && pwd)/certbot-deploy-hook.sh"

certbot certonly --non-interactive --keep-until-expiring \
  --dns-cloudflare --dns-cloudflare-credentials "$CERTBOT_CLOUDFLARE_INI" \
  -d "$BROKER_FQDN" \
  --deploy-hook "$hook"

echo "server cert ready: /etc/letsencrypt/live/${BROKER_FQDN}/ (deploy-hook reloads only mosquitto)"
