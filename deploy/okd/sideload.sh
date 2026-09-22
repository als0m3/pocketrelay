#!/usr/bin/env bash
# Load the image directly on the single cluster node without registry credentials:
#   docker save → node debug pod → podman load (shared CRI-O storage).
#   ./deploy/okd/sideload.sh ghcr.io/als0m3/custom-remote:<tag>
set -euo pipefail
IMG="${1:?image required}"
CONTEXT="${CONTEXT:-default/api-cluster-example-test:6443/kube:admin}"
NODE="${NODE:-cluster}"
K() { oc --context "$CONTEXT" "$@"; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"; [ -n "${POD:-}" ] && K -n "$NS" delete pod "$POD" --wait=false >/dev/null 2>&1 || true' EXIT

docker image inspect "$IMG" >/dev/null 2>&1 || docker pull --platform linux/amd64 "$IMG"
docker save "$IMG" | gzip -1 > "$TMP/img.tar.gz"
echo "archive: $(du -h "$TMP/img.tar.gz" | cut -f1)"

NS="$(K get ns -o name | grep -q 'namespace/custom-remote$' && echo custom-remote || echo default)"
K -n "$NS" debug "node/$NODE" --quiet -- sleep 900 >/dev/null 2>&1 &
for _ in $(seq 60); do
  POD="$(K -n "$NS" get pods -o name 2>/dev/null | grep -m1 "${NODE}-debug" | cut -d/ -f2 || true)"
  [ -n "$POD" ] && K -n "$NS" get pod "$POD" -o jsonpath='{.status.phase}' 2>/dev/null | grep -q Running && break
  sleep 2
done
[ -n "${POD:-}" ] || { echo "debug pod not found"; exit 1; }
K -n "$NS" cp "$TMP/img.tar.gz" "$POD:/host/var/tmp/custom-remote.tar.gz"
K -n "$NS" exec "$POD" -- chroot /host sh -c 'podman load -q -i /var/tmp/custom-remote.tar.gz && rm -f /var/tmp/custom-remote.tar.gz'
K -n "$NS" exec "$POD" -- chroot /host crictl images | grep custom-remote
