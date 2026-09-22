#!/usr/bin/env bash
# Install a VPS nginx catch-all and a certificate-renewal reload hook.
#   ./deploy/vps/harden.sh [root@203.0.113.10]
set -euo pipefail
cd "$(dirname "$0")"
VPS="${1:-root@203.0.113.10}"
scp -q 00-catch-all.conf reload-nginx.sh "$VPS:/tmp/"
ssh "$VPS" bash -s <<'REMOTE'
set -euo pipefail
mkdir -p /etc/nginx/certs
[ -f /etc/nginx/certs/catch-all.crt ] || openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=invalid" \
  -keyout /etc/nginx/certs/catch-all.key -out /etc/nginx/certs/catch-all.crt 2>/dev/null
chmod 600 /etc/nginx/certs/catch-all.key
install -m 755 /tmp/reload-nginx.sh /etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh
cp /tmp/00-catch-all.conf /etc/nginx/sites-available/00-catch-all.conf
ln -sf /etc/nginx/sites-available/00-catch-all.conf /etc/nginx/sites-enabled/00-catch-all.conf
if nginx -t; then systemctl reload nginx; echo "catch-all active"
else rm -f /etc/nginx/sites-enabled/00-catch-all.conf; echo "nginx validation failed: catch-all removed, configuration not reloaded"; exit 1; fi
REMOTE
