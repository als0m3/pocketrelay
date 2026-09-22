#!/usr/bin/env bash
# Build the amd64 image and push to ghcr.io/als0m3 as a private package.
# Use isolated Docker configuration, preserving the global Docker Desktop GHCR login.
#   GHCR_TOKEN=<PAT-with-write:packages> ./deploy/build-push.sh [tag]
set -euo pipefail
cd "$(dirname "$0")/.."
# Local secrets (not tracked): GHCR_TOKEN, GHCR_PULL_TOKEN, OIDC_CLIENT_SECRET
[ -f deploy/okd/secrets.env ] && source deploy/okd/secrets.env
: "${GHCR_TOKEN:?Missing GHCR_TOKEN (PAT with write:packages scope)}"
USER_NAME=als0m3
REPO="ghcr.io/${USER_NAME}/custom-remote"
TAG="${1:-$(git rev-parse --short HEAD 2>/dev/null || date +%Y%m%d%H%M)}"

export DOCKER_CONFIG="$(mktemp -d)"
trap 'rm -rf "$DOCKER_CONFIG"' EXIT
echo "$GHCR_TOKEN" | docker login ghcr.io -u "$USER_NAME" --password-stdin >/dev/null

docker buildx build --platform linux/amd64 -t "$REPO:$TAG" -t "$REPO:latest" --push .
echo "$REPO:$TAG"
