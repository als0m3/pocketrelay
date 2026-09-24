"""Subscription accounts (Claude, Codex and Antigravity), ordered by provider priority.

Requests select the first active, available account. Account-related
failures before any output (quota exhaustion or lost authentication)
pause that account and retry with the next one.

Each provider system account uses authentication from
default host credentials (`claude login`, `~/.codex` or $CODEX_HOME, Antigravity `~/.gemini`).
"""

import json
import os
import secrets
import shutil
import threading
import time
from pathlib import Path

from .config import DATA

FILE = DATA / "accounts.json"
STATE_FILE = DATA / "accounts_state.json"
DIR = DATA / "accounts"
PROVIDERS = ("claude", "codex", "antigravity")
# Maintained host-login providers: empty for none on a cluster without host
# credentials; an absent variable retains all three for development machines.
_env = os.environ.get("REMOTE_SYSTEM_ACCOUNTS")
SYSTEM_ACCOUNTS = tuple(PROVIDERS) if _env is None else tuple(
    p for p in PROVIDERS if p in {x.strip() for x in _env.split(",")})
RATE_LIMIT_PAUSE = 15 * 60      # Fallback when no quota reset time is provided.
AUTH_PAUSE = 10 * 60            # Avoid retrying signed-out accounts on every request.

_lock = threading.RLock()
_accounts: list[dict] = []
_state: dict[str, dict] = {}    # id -> runtime state (quotas, pauses, counters)


def _save():
    DATA.mkdir(parents=True, exist_ok=True)
    for path, data in ((FILE, _accounts), (STATE_FILE, _state)):
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps(data, ensure_ascii=False, indent=1))
        tmp.chmod(0o600)
        tmp.replace(path)


def _load():
    global _accounts, _state
    if FILE.exists():
        _accounts = json.loads(FILE.read_text())
        _state = json.loads(STATE_FILE.read_text()) if STATE_FILE.exists() else {}
        # Add the Gemini system account to existing installations.
        # Remove system accounts for providers replaced by Antigravity.
        orphans = [a for a in _accounts if a["system"] and a["provider"] not in PROVIDERS]
        for a in orphans:
            _accounts.remove(a)
            _state.pop(a["id"], None)
        missing = [p for p in SYSTEM_ACCOUNTS if not any(a["system"] and a["provider"] == p for a in _accounts)]
        if missing or orphans:
            _accounts += [{"id": f"system-{p}", "provider": p, "label": "Host login",
                           "enabled": True, "system": True, "created": time.time()} for p in missing]
            _save()
        return
    now = time.time()
    _accounts = [{"id": f"system-{p}", "provider": p, "label": "Host login", "enabled": True,
                  "system": True, "created": now} for p in SYSTEM_ACCOUNTS]
    # Migrate the former single token into a full account with highest priority.
    legacy = DATA / "claude_oauth_token"
    if legacy.exists():
        acc = _new("claude", "Primary")
        _write_token(acc["id"], legacy.read_text().strip())
        legacy.unlink()
        _accounts.remove(acc)
        _accounts.insert(0, acc)
    _save()


def _new(provider: str, label: str) -> dict:
    acc = {"id": secrets.token_hex(5), "provider": provider, "label": label.strip() or provider,
           "enabled": True, "system": False, "created": time.time()}
    _accounts.append(acc)
    return acc


def _write_token(acc_id: str, token: str):
    d = DIR / acc_id
    d.mkdir(parents=True, exist_ok=True)
    (d / "token").write_text(token)
    (d / "token").chmod(0o600)


with _lock:
    _load()


# ---------- lecture ----------

def get(acc_id: str) -> dict:
    for a in _accounts:
        if a["id"] == acc_id:
            return a
    raise KeyError(acc_id)


def listing(provider: str | None = None) -> list[dict]:
    return [a for a in _accounts if provider is None or a["provider"] == provider]


def state(acc_id: str) -> dict:
    return _state.setdefault(acc_id, {"requests": 0, "errors": 0})


def token(acc: dict) -> str | None:
    """Account claude setup-token token, or None for the system account."""
    p = DIR / acc["id"] / "token"
    return p.read_text().strip() if p.exists() else None


def codex_home(acc: dict) -> str | None:
    if acc["system"]:
        return os.environ.get("CODEX_HOME") or None
    return str(DIR / acc["id"] / "codex")


def cli_home(acc: dict, name: str) -> str:
    """Dedicated account HOME for CLIs storing state under ~.

    System accounts also write to the persistent volume rather than the
    ephemeral process HOME. Override with $<NAME>_HOME, for example to use
    the actual HOME on a development machine.
    """
    if acc["system"]:
        return os.environ.get(f"{name.upper()}_HOME") or str(DATA / name)
    return str(DIR / acc["id"] / name)


def available(acc: dict) -> bool:
    return acc["enabled"] and state(acc["id"]).get("paused_until", 0) <= time.time()


def pick(provider: str, exclude: set[str] = frozenset()) -> dict | None:
    """First active, unpaused account in priority order."""
    with _lock:
        for a in listing(provider):
            if a["id"] not in exclude and available(a):
                return a
    return None


def session_token() -> str | None:
    """Token for local Claude sessions: first available Claude account."""
    a = pick("claude")
    return token(a) if a else None


# ---------- writes ----------

def add_claude(label: str, tok: str) -> dict:
    with _lock:
        acc = _new("claude", label)
        _write_token(acc["id"], tok)
        _save()
    return acc


def add_codex(label: str) -> dict:
    with _lock:
        acc = _new("codex", label)
        Path(codex_home(acc)).mkdir(parents=True, exist_ok=True)
        _save()
    return acc


def add_antigravity(label: str) -> dict:
    with _lock:
        acc = _new("antigravity", label)
        Path(cli_home(acc, "antigravity")).mkdir(parents=True, exist_ok=True)
        _save()
    return acc


def clear_cli_home(acc_id: str, name: str):
    """Delete CLI state and credentials for this account."""
    with _lock:
        h = cli_home(get(acc_id), name)
        if h:
            shutil.rmtree(h, ignore_errors=True)
            Path(h).mkdir(parents=True, exist_ok=True)


def set_token(acc_id: str, tok: str):
    with _lock:
        _write_token(get(acc_id)["id"], tok)
        resume(acc_id)


def update(acc_id: str, patch: dict) -> dict:
    with _lock:
        acc = get(acc_id)
        if "label" in patch:
            acc["label"] = str(patch["label"]).strip() or acc["label"]
        if "enabled" in patch:
            acc["enabled"] = bool(patch["enabled"])
        _save()
    return acc


def move(acc_id: str, delta: int):
    """Move an account up/down within its provider."""
    with _lock:
        acc = get(acc_id)
        same = [i for i, a in enumerate(_accounts) if a["provider"] == acc["provider"]]
        pos = same.index(_accounts.index(acc))
        target = pos + delta
        if 0 <= target < len(same):
            i, j = same[pos], same[target]
            _accounts[i], _accounts[j] = _accounts[j], _accounts[i]
            _save()


def remove(acc_id: str):
    with _lock:
        acc = get(acc_id)
        if acc["system"] and acc["provider"] in SYSTEM_ACCOUNTS:
            raise ValueError(f"System account {acc['provider']} is recreated at startup: "
                             "remove its provider from REMOTE_SYSTEM_ACCOUNTS or disable the account.")
        _accounts.remove(acc)
        _state.pop(acc_id, None)
        shutil.rmtree(DIR / acc_id, ignore_errors=True)
        _save()


def resume(acc_id: str):
    with _lock:
        st = state(acc_id)
        st.pop("paused_until", None)
        st.pop("pause_reason", None)
        _save()


# ---------- runtime tracking ----------

def record_success(acc: dict):
    with _lock:
        st = state(acc["id"])
        st["requests"] += 1
        st["last_used"] = time.time()
        st.pop("last_error", None)
        st.pop("paused_until", None)
        st.pop("pause_reason", None)
        _save()


def record_limits(acc: dict, limits: dict):
    with _lock:
        state(acc["id"])["limits"] = {**limits, "seen_at": time.time()}


def record_failure(acc: dict, kind: str, message: str, resets_at: float | None = None) -> bool:
    """Record failure and return whether switching accounts is appropriate."""
    with _lock:
        st = state(acc["id"])
        st["errors"] += 1
        st["last_error"] = {"at": time.time(), "kind": kind, "message": message[:300]}
        failover = kind in ("rate_limit", "auth")
        if failover:
            pause = AUTH_PAUSE if kind == "auth" else RATE_LIMIT_PAUSE
            st["paused_until"] = resets_at if resets_at and resets_at > time.time() else time.time() + pause
            st["pause_reason"] = "quota reached" if kind == "rate_limit" else "not signed in"
        _save()
    return failover


def public(acc: dict) -> dict:
    st = state(acc["id"])
    out = {**acc, **{k: v for k, v in st.items()}}
    paused = st.get("paused_until", 0) > time.time()
    out["status"] = ("disabled" if not acc["enabled"] else "paused" if paused
                     else "error" if st.get("last_error") else "ok" if st.get("last_used") else "unknown")
    if acc["provider"] == "claude" and not acc["system"]:
        tok = token(acc) or ""
        out["masked"] = tok[:14] + "…" + tok[-4:] if tok else None
    return out
