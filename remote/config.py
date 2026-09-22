"""Configuration through environment variables."""

import os
import secrets
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = Path(os.environ.get("REMOTE_DATA", ROOT / "data")).expanduser()
STATIC = ROOT / "static"

HOST = os.environ.get("REMOTE_HOST", "127.0.0.1")
PORT = int(os.environ.get("REMOTE_PORT", "8787"))

# Allowed Host headers protect against DNS rebinding.
ALLOWED_HOSTS = {"localhost", "127.0.0.1", "[::1]", "::1", "host.docker.internal"} | {
    h.strip() for h in os.environ.get("REMOTE_ALLOWED_HOSTS", "").split(",") if h.strip()
}

CLAUDE_BIN = (
    os.environ.get("CLAUDE_BIN")
    or shutil.which("claude")
    or str(Path.home() / ".local/bin/claude")
)

# Remove ANTHROPIC_API_KEY by default to use the subscription.
KEEP_API_KEY = os.environ.get("REMOTE_KEEP_API_KEY") == "1"

CLAUDE_PROJECTS = Path.home() / ".claude" / "projects"

# Local Claude Code sessions and UI: useful locally, disabled on the cluster.
ENABLE_SESSIONS = os.environ.get("REMOTE_ENABLE_SESSIONS", "1") == "1"

# Optional /admin OIDC (Keycloak, etc.) with an explicit administrator allowlist.
OIDC_ISSUER = os.environ.get("REMOTE_OIDC_ISSUER", "")
OIDC_CLIENT_ID = os.environ.get("REMOTE_OIDC_CLIENT_ID", "")
OIDC_CLIENT_SECRET = os.environ.get("REMOTE_OIDC_CLIENT_SECRET", "")
ADMIN_EMAILS = {e.strip().lower() for e in os.environ.get("REMOTE_ADMIN_EMAILS", "").split(",") if e.strip()}
ADMIN_GROUPS = {g.strip().strip("/") for g in os.environ.get("REMOTE_ADMIN_GROUPS", "").split(",") if g.strip()}


def load_token() -> str:
    """API token: REMOTE_TOKEN, or generate once and persist in data/token."""
    if tok := os.environ.get("REMOTE_TOKEN"):
        return tok
    DATA.mkdir(parents=True, exist_ok=True)
    path = DATA / "token"
    if path.exists():
        return path.read_text().strip()
    tok = secrets.token_urlsafe(32)
    path.write_text(tok)
    path.chmod(0o600)
    return tok


TOKEN = load_token()


def load_session_secret() -> str:
    if sec := os.environ.get("REMOTE_SESSION_SECRET"):
        return sec
    path = DATA / "session_secret"
    if not path.exists():
        path.write_text(secrets.token_urlsafe(48))
        path.chmod(0o600)
    return path.read_text().strip()


SESSION_SECRET = load_session_secret()
