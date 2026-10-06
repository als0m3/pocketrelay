#!/usr/bin/env bash
# Local installer: only Docker and its Compose plugin are required.
set -euo pipefail
cd "$(dirname "$0")"

if ! command -v docker >/dev/null 2>&1 || ! docker compose version >/dev/null 2>&1; then
  echo "Install Docker Desktop (Mac/Windows) or Docker Engine with the Compose plugin (Linux), then run ./install.sh again."
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo "Docker is not running or your user cannot access it. Start Docker, then run ./install.sh again."
  exit 1
fi
if [[ ! -t 0 ]]; then
  echo "Run ./install.sh in an interactive terminal to choose your password."
  exit 1
fi

echo "1/3 · Building PocketRelay (the first run may take several minutes)…"
docker compose build
echo "2/3 · Creating your administrator account…"
docker compose run --rm --no-deps customremote customremote setup --if-missing
echo "3/3 · Starting…"
docker compose up -d --wait
published_address="$(docker compose port customremote 8787)"
echo "Ready! On this computer: http://localhost:${published_address##*:}/admin"
echo "Sign in with your username and password, then follow the three steps shown."
