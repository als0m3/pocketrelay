#!/usr/bin/env bash
# Start PocketRelay (http://localhost:8787 by default)
set -euo pipefail
cd "$(dirname "$0")"
# Use the same personal profile as Compose; advanced options remain explicit.
export POCKETRELAY_ENABLE_SESSIONS="${POCKETRELAY_ENABLE_SESSIONS:-0}"
export POCKETRELAY_SYSTEM_ACCOUNTS="${POCKETRELAY_SYSTEM_ACCOUNTS-}"
export POCKETRELAY_V1_ALLOW_MASTER="${POCKETRELAY_V1_ALLOW_MASTER:-0}"
exec cargo run --release --locked -- serve
