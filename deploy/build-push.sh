#!/usr/bin/env bash
# Build the amd64 image and push to ghcr.io/als0m3 as a private package.
# Use isolated Docker configuration, preserving the global Docker Desktop GHCR login.
#   GHCR_TOKEN=<PAT-with-write:packages> ./deploy/build-push.sh [tag]
set -euo pipefail
cd "$(dirname "$0")/.."
# Local secrets (not tracked): GHCR_TOKEN, GHCR_PULL_TOKEN, OIDC_CLIENT_SECRET
[ -f deploy/okd/secrets.env ] && source deploy/okd/secrets.env
# Without a PAT, use the gh token after gh auth refresh -s write:packages.
if [ -z "${GHCR_TOKEN:-}" ] && gh auth status 2>&1 | grep -q "write:packages"; then GHCR_TOKEN="$(gh auth token)"; fi
: "${GHCR_TOKEN:?Missing GHCR_TOKEN (PAT with write:packages, or gh auth refresh -s write:packages)}"
USER_NAME=als0m3
REPO="ghcr.io/${USER_NAME}/custom-remote"
TAG="${1:-$(git rev-parse --short HEAD 2>/dev/null || date +%Y%m%d%H%M)}"

# Preserve the current Docker endpoint and buildx plugins in the isolated configuration.
export DOCKER_HOST="${DOCKER_HOST:-$(docker context inspect --format '{{.Endpoints.docker.Host}}')}"
PLUGINS="${HOME}/.docker/cli-plugins"
export DOCKER_CONFIG="$(mktemp -d)"
trap 'rm -rf "$DOCKER_CONFIG"' EXIT
[ -d "$PLUGINS" ] && ln -s "$PLUGINS" "$DOCKER_CONFIG/cli-plugins"
# Without an explicit credsStore, macOS Docker writes to the system keychain
# and overwrites existing GHCR credentials; use a temporary-directory helper.
mkdir -p "$DOCKER_CONFIG/bin"
cat > "$DOCKER_CONFIG/bin/docker-credential-crtmp" <<'HELPER'
#!/usr/bin/env python3
import json, os, sys
path = os.path.join(os.environ["DOCKER_CONFIG"], "creds.json")
db = json.load(open(path)) if os.path.exists(path) else {}
cmd, data = sys.argv[1], sys.stdin.read().strip()
if cmd == "store":
    o = json.loads(data); db[o["ServerURL"]] = {"Username": o["Username"], "Secret": o["Secret"]}
    json.dump(db, open(path, "w")); os.chmod(path, 0o600)
elif cmd == "get":
    if data not in db: print("credentials not found in native keychain"); sys.exit(1)
    print(json.dumps({"ServerURL": data, **db[data]}))
elif cmd == "erase":
    db.pop(data, None); json.dump(db, open(path, "w"))
elif cmd == "list":
    print(json.dumps({k: v["Username"] for k, v in db.items()}))
HELPER
chmod +x "$DOCKER_CONFIG/bin/docker-credential-crtmp"
export PATH="$DOCKER_CONFIG/bin:$PATH"
echo '{"credsStore": "crtmp"}' > "$DOCKER_CONFIG/config.json"
echo "$GHCR_TOKEN" | docker login ghcr.io -u "$USER_NAME" --password-stdin >/dev/null

docker buildx build --platform linux/amd64 -t "$REPO:$TAG" -t "$REPO:latest" --push .
echo "$REPO:$TAG"
