#!/usr/bin/env bash
# Run on the Mac after creating DNS records:
#   ./deploy/vps/install.sh [root@203.0.113.10]
# 1) HTTP-only ACME host  2) webroot certificate  3) complete virtual host.
# Validate nginx before every reload to protect other sites on configuration errors.
set -euo pipefail
cd "$(dirname "$0")"
VPS="${1:-root@203.0.113.10}"
HOSTS=(relay.example.test relay-chat.example.test relay-api.example.test)

for h in "${HOSTS[@]}"; do
  ip=$(dig +short "$h" | tail -1)
  [ "$ip" = "203.0.113.10" ] || { echo "DNS: $h → '${ip:-none}' (expected 203.0.113.10)"; exit 1; }
done

scp -q relay.conf "$VPS:/tmp/relay.conf"
ssh "$VPS" "HOSTS='${HOSTS[*]}' bash -s" <<'REMOTE'
set -euo pipefail
AV=/etc/nginx/sites-available/relay.conf; EN=/etc/nginx/sites-enabled/relay.conf
CERT=/etc/letsencrypt/live/relay.example.test/fullchain.pem
if [ ! -f "$CERT" ]; then
  # Step 1: port 80 only; other blocks refer to certificates not yet created.
  awk 'BEGIN{b=0} /^server \{/{b++} b<=1{print}' /tmp/relay.conf > "$AV"
  ln -sf "$AV" "$EN"
  nginx -t && systemctl reload nginx
fi
# Create or expand the certificate when adding hostnames such as relay-chat.
have="$(openssl x509 -noout -text -in "$CERT" 2>/dev/null || true)"; args=""
for h in $HOSTS; do
  args="$args -d $h"
  case "$have" in *"DNS:$h"*) ;; *) need=1 ;; esac
done
if [ "${need:-0}" = 1 ]; then
  certbot certonly --webroot -w /var/www/html --non-interactive --agree-tos --keep-until-expiring --expand $args
fi
cp /tmp/relay.conf "$AV"
ln -sf "$AV" "$EN"
nginx -t && systemctl reload nginx
echo "nginx reloaded with relay.conf"
REMOTE
