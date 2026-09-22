#!/usr/bin/env bash
# Deploy the gateway and Open WebUI to OKD.
#   cp deploy/okd/params.env.example deploy/okd/params.env   # Then fill in values.
#   ./deploy/okd/deploy.sh      (reads deploy/okd/secrets.env)
set -euo pipefail
cd "$(dirname "$0")"
# Local secrets (not tracked): GHCR_TOKEN, OIDC_CLIENT_SECRET
[ -f secrets.env ] && source secrets.env
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

# Generate newly introduced secrets once for existing deployments.
for k in forward-jwt-secret owui-api-key; do
  if [ -z "$(oc_ get secret custom-remote -o jsonpath="{.data.$k}")" ]; then
    v=$([ "$k" = owui-api-key ] && echo "a-generer" || openssl rand -base64 48 | tr -d '\n')
    oc_ patch secret custom-remote -p "{\"stringData\":{\"$k\":\"$v\"}}" >/dev/null
  fi
done

# Sideload the private image; no registry secret is stored in the cluster.
IMAGE="$(grep '^IMAGE=' params.env | cut -d= -f2-)"
[ "${SKIP_SIDELOAD:-0}" = "1" ] || CONTEXT="$CONTEXT" ./sideload.sh "$IMAGE"

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

# Create a dedicated Open WebUI API key, hash it on the gateway volume, and save it as a secret.
if [ "$(oc_ get secret custom-remote -o jsonpath='{.data.owui-api-key}' | base64 -d)" = "a-generer" ]; then
  key=$(oc_ exec deploy/claude-api -- python -c "from remote import keys; print(keys.create_key('open-webui')[1])")
  oc_ patch secret custom-remote -p "{\"stringData\":{\"owui-api-key\":\"$key\"}}" >/dev/null
  oc_ rollout restart deployment/open-webui >/dev/null
  echo "open-webui API key created"
fi
oc_ rollout status deployment/open-webui --timeout=600s
echo "Console : https://$API_HOST/admin   ·   Chat : https://$CHAT_HOST"
