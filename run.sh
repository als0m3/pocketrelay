#!/usr/bin/env bash
# Start PocketRelay (http://localhost:8787 by default)
set -euo pipefail
cd "$(dirname "$0")"
# Use the same personal profile as Compose; advanced options remain explicit.
export REMOTE_ENABLE_SESSIONS="${REMOTE_ENABLE_SESSIONS:-0}"
export REMOTE_SYSTEM_ACCOUNTS="${REMOTE_SYSTEM_ACCOUNTS-}"
export REMOTE_V1_ALLOW_MASTER="${REMOTE_V1_ALLOW_MASTER:-0}"
exec cargo run --release --locked -- serve
