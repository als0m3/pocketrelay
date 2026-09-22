"""API keys (/v1) and CLI subscription token, stored in data/.

Only SHA-256 hashes of keys are stored, never plaintext.
The key value is displayed only once, when created.
"""

import hashlib
import json
import secrets
import threading
import time

from .config import DATA

KEYS_FILE = DATA / "api_keys.json"
CLAUDE_TOKEN_FILE = DATA / "claude_oauth_token"
_lock = threading.Lock()


def _hash(key: str) -> str:
    return hashlib.sha256(key.encode()).hexdigest()


def _load() -> list[dict]:
    return json.loads(KEYS_FILE.read_text()) if KEYS_FILE.exists() else []


def _save(items: list[dict]):
    DATA.mkdir(parents=True, exist_ok=True)
    tmp = KEYS_FILE.with_suffix(".tmp")
    tmp.write_text(json.dumps(items, ensure_ascii=False, indent=1))
    tmp.chmod(0o600)
    tmp.replace(KEYS_FILE)


def public(item: dict) -> dict:
    return {k: v for k, v in item.items() if k != "hash"}


def list_keys() -> list[dict]:
    return [public(k) for k in _load()]


def create_key(name: str) -> tuple[dict, str]:
    key = "sk-cr-" + secrets.token_urlsafe(32)
    item = {"id": secrets.token_hex(6), "name": name.strip() or "untitled", "prefix": key[:10] + "…" + key[-4:],
            "hash": _hash(key), "created": time.time(), "last_used": None, "requests": 0, "revoked": False}
    with _lock:
        items = _load()
        items.append(item)
        _save(items)
    return public(item), key


def revoke_key(key_id: str, delete: bool = False) -> bool:
    with _lock:
        items = _load()
        found = [k for k in items if k["id"] == key_id]
        if not found:
            return False
        if delete:
            items = [k for k in items if k["id"] != key_id]
        else:
            found[0]["revoked"] = True
        _save(items)
    return True


def verify(key: str) -> dict | None:
    """Return a valid key and record its usage."""
    h = _hash(key)
    with _lock:
        items = _load()
        for k in items:
            if secrets.compare_digest(k["hash"], h) and not k["revoked"]:
                k["last_used"] = time.time()
                k["requests"] += 1
                _save(items)
                return public(k)
    return None


# ---------- subscription token (claude setup-token) ----------

def claude_token() -> str | None:
    return CLAUDE_TOKEN_FILE.read_text().strip() if CLAUDE_TOKEN_FILE.exists() else None


def set_claude_token(token: str | None):
    if not token:
        CLAUDE_TOKEN_FILE.unlink(missing_ok=True)
        return
    DATA.mkdir(parents=True, exist_ok=True)
    CLAUDE_TOKEN_FILE.write_text(token.strip())
    CLAUDE_TOKEN_FILE.chmod(0o600)


def claude_token_status() -> dict:
    tok = claude_token()
    if not tok:
        return {"set": False}
    return {"set": True, "masked": tok[:14] + "…" + tok[-4:], "updated": CLAUDE_TOKEN_FILE.stat().st_mtime}
