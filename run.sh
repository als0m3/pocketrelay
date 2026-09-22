#!/usr/bin/env bash
# Start CustomRemote (default: http://localhost:8787).
set -euo pipefail
cd "$(dirname "$0")"
exec uv run python -m remote
