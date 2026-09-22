#!/usr/bin/env bash
# Deploy the gateway and Open WebUI to OKD.
#   cp deploy/okd/params.env.example deploy/okd/params.env   # Then fill in values.
#   OIDC_CLIENT_SECRET=… GHCR_PULL_TOKEN=… ./deploy/okd/deploy.sh
set -euo pipefail
cd "$(dirname "$0")"
CONTEXT="${CONTEXT:-default/api-cluster-example-test:6443/kube:admin}"
NS="${NAMESPACE:-custom-remote}"
oc_() { oc --context "$CONTEXT" -n "$NS" "$@"; }

oc --context "$CONTEXT" get namespace "$NS" >/dev/null 2>&1 || oc --context "$CONTEXT" new-project "$NS" >/dev/null

# Generate secrets once; redeployment never regenerates them.
if ! oc_ get secret custom-remote >/dev/null 2>&1; then
  oc_ create secret generic custom-remote \
    --from-literal=remote-token="$(openssl rand -base64 36 | tr -d '/+=')" \
    --from-literal=session-secret="$(openssl rand -base64 48)" \
    --from-literal=webui-secret-key="$(openssl rand -base64 48)" \
    --from-literal=oidc-client-secret="${OIDC_CLIENT_SECRET:-fill-in}"
elif [ -n "${OIDC_CLIENT_SECRET:-}" ]; then
  oc_ patch secret custom-remote -p "{\"stringData\":{\"oidc-client-secret\":\"${OIDC_CLIENT_SECRET}\"}}"
fi

# Pull secret for the private image (read-only PAT with read:packages scope).
if [ -n "${GHCR_PULL_TOKEN:-}" ]; then
  oc_ create secret docker-registry ghcr-als0m3 --docker-server=ghcr.io \
    --docker-username=als0m3 --docker-password="$GHCR_PULL_TOKEN" --dry-run=client -o yaml | oc_ apply -f -
fi

oc process --local -f template.yaml --param-file params.env --ignore-unknown-parameters | oc_ apply -f -

# Routes: NetBird label and/or Let’s Encrypt certificate from params.env.
source params.env
for r in claude-api open-webui; do
  [ "${NETBIRD_ROUTE:-false}" = "true" ] && oc_ label route "$r" netbird-route=true --overwrite >/dev/null
  if [ -n "${CERT_ISSUER:-}" ]; then
    oc_ annotate route "$r" --overwrite cert-manager.io/issuer-kind=ClusterIssuer \
      cert-manager.io/issuer-name="$CERT_ISSUER" >/dev/null
  fi
done
oc_ rollout restart deployment/claude-api deployment/open-webui >/dev/null
oc_ rollout status deployment/claude-api --timeout=180s
oc_ rollout status deployment/open-webui --timeout=600s
echo "Console : https://$API_HOST/admin   ·   Chat : https://$CHAT_HOST"
