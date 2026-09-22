"""Usage controls for /v1: per-user rates and concurrency limits.

Open WebUI sends user identity in an HS256 JWT signed with a shared
secret; trust it only on the dedicated Open WebUI API key.
Other clients are limited per API key.
"""

import asyncio
import os
import time
from collections import defaultdict, deque

USER_RATE = os.environ.get("REMOTE_USER_RATE", "200/h")        # Set 0 to disable.
MAX_CONCURRENCY = int(os.environ.get("REMOTE_MAX_CONCURRENCY", "6"))
QUEUE_TIMEOUT = float(os.environ.get("REMOTE_QUEUE_TIMEOUT", "120"))
FORWARDER_KEY = os.environ.get("REMOTE_FORWARDER_KEY_NAME", "open-webui")
JWT_SECRET = os.environ.get("REMOTE_FORWARD_JWT_SECRET", "")
JWT_HEADER = "x-openwebui-user-jwt"

_PERIODS = {"m": 60, "h": 3600, "d": 86400}
_hits: dict[str, deque] = defaultdict(deque)
_sem = asyncio.Semaphore(MAX_CONCURRENCY) if MAX_CONCURRENCY > 0 else None


class LimitError(Exception):
    def __init__(self, status: int, message: str, code: str):
        super().__init__(message)
        self.status, self.message, self.code = status, message, code


def _parse(rate: str) -> tuple[int, int, str] | None:
    if not rate or rate == "0":
        return None
    n, _, per = rate.partition("/")
    unit = (per or "h").strip()[0]
    return int(n), _PERIODS.get(unit, 3600), {"m": "minute", "h": "heure", "d": "jour"}.get(unit, "heure")


_RATE = _parse(USER_RATE)


def forwarded_user(headers) -> dict | None:
    """Accept Open WebUI identity only with a valid, unexpired JWT signed by our secret."""
    token = headers.get(JWT_HEADER)
    if not token or not JWT_SECRET:
        return None
    from authlib.jose import JoseError, jwt
    try:
        claims = jwt.decode(token, JWT_SECRET)
        claims.validate()  # exp / iat
    except (JoseError, ValueError):
        return None
    return {"id": claims.get("sub"), "email": claims.get("email"), "role": claims.get("role")}


def admit(headers, ident: dict):
    """Raise LimitError when the caller exceeds its allowance; exempt master/admin access."""
    if not _RATE or ident.get("master"):
        return
    subject = f"key:{ident.get('key_id')}"
    if ident.get("key_name") == FORWARDER_KEY:
        user = forwarded_user(headers)
        if user and user.get("role") == "admin":
            return
        # Without valid identity, all requests under the Open WebUI key share one allowance.
        subject = f"owui:{user['email'] or user['id']}" if user else subject
    n, secs, label = _RATE
    now = time.time()
    dq = _hits[subject]
    while dq and dq[0] < now - secs:
        dq.popleft()
    if len(dq) >= n:
        wait = int(dq[0] + secs - now) + 1
        raise LimitError(429, f"Rate limit reached: {n} requests per {label}. Retry in {wait // 60} min {wait % 60} s.",
                         "user_rate_limit")
    dq.append(now)


class slot:
    """Bound concurrent model requests, each of which launches a CLI process."""

    async def __aenter__(self):
        if _sem is None:
            return
        try:
            await asyncio.wait_for(_sem.acquire(), QUEUE_TIMEOUT)
        except asyncio.TimeoutError:
            raise LimitError(503, "Server busy: too many active requests. Try again shortly.", "server_busy")

    async def __aexit__(self, *exc):
        if _sem is not None:
            _sem.release()
