"""Admin console (/admin): API keys, subscription token and quotas.

Use OIDC (Keycloak, etc.) when REMOTE_OIDC_ISSUER is configured, otherwise
use the master token for login or recovery. Sessions use signed cookies;
all mutations require the X-Admin header to prevent CSRF.
"""

import asyncio
import secrets
import time

from fastapi import APIRouter, HTTPException, Request
from fastapi.responses import FileResponse, RedirectResponse

from . import codex_backend, keys, openai_compat
from .config import ADMIN_EMAILS, ADMIN_GROUPS, OIDC_CLIENT_ID, OIDC_CLIENT_SECRET, OIDC_ISSUER, STATIC, TOKEN

router = APIRouter(prefix="/admin")

oauth = None
if OIDC_ISSUER:
    from authlib.integrations.starlette_client import OAuth

    oauth = OAuth()
    oauth.register(
        "oidc",
        client_id=OIDC_CLIENT_ID,
        client_secret=OIDC_CLIENT_SECRET,
        server_metadata_url=OIDC_ISSUER.rstrip("/") + "/.well-known/openid-configuration",
        client_kwargs={"scope": "openid email profile", "code_challenge_method": "S256"},
    )


def current_user(request: Request) -> dict | None:
    u = request.session.get("admin")
    if u and u.get("exp", 0) > time.time():
        return u
    return None


def require_admin(request: Request, mutating: bool = False) -> dict:
    u = current_user(request)
    if not u:
        raise HTTPException(401, "Not signed in")
    if mutating and request.headers.get("x-admin") != "1":
        raise HTTPException(403, "Missing X-Admin header")
    return u


def is_allowed(claims: dict) -> bool:
    if not ADMIN_EMAILS and not ADMIN_GROUPS:
        return False  # No implicit access: require an explicit allowlist.
    email = (claims.get("email") or "").lower()
    groups = {g.strip("/") for g in claims.get("groups") or []}
    roles = set((claims.get("realm_access") or {}).get("roles") or [])
    return email in ADMIN_EMAILS or bool((groups | roles) & ADMIN_GROUPS)


def open_session(request: Request, who: str, how: str):
    request.session["admin"] = {"who": who, "how": how, "exp": time.time() + 12 * 3600}


# ---------- pages & connection ----------

@router.get("")
@router.get("/")
async def page():
    return FileResponse(STATIC / "admin.html")


@router.get("/auth/config")
async def auth_config(request: Request):
    return {"oidc": bool(oauth), "user": current_user(request)}


@router.get("/auth/login")
async def login(request: Request):
    if not oauth:
        raise HTTPException(404, "OIDC is not configured")
    redirect = str(request.url_for("oidc_callback"))
    if request.headers.get("x-forwarded-proto") == "https":
        redirect = redirect.replace("http://", "https://", 1)
    return await oauth.oidc.authorize_redirect(request, redirect)


@router.get("/auth/callback", name="oidc_callback")
async def oidc_callback(request: Request):
    tok = await oauth.oidc.authorize_access_token(request)
    claims = dict(tok.get("userinfo") or {})
    if not is_allowed(claims):
        return RedirectResponse("/admin?denied=" + (claims.get("email") or claims.get("preferred_username") or "?"))
    open_session(request, claims.get("email") or claims.get("preferred_username"), "oidc")
    return RedirectResponse("/admin")


@router.post("/auth/token")
async def token_login(request: Request):
    body = await request.json()
    if not secrets.compare_digest(str(body.get("token", "")), TOKEN):
        raise HTTPException(401, "Invalid master token")
    open_session(request, "master token", "token")
    return {"ok": True}


@router.post("/auth/logout")
async def logout(request: Request):
    request.session.clear()
    return {"ok": True}


# ---------- Console API ----------

@router.get("/api/state")
async def state(request: Request):
    u = require_admin(request)
    return {"user": u, "keys": keys.list_keys(), "claude_token": keys.claude_token_status(),
            "limits": openai_compat.LAST_LIMITS, "stats": openai_compat.STATS, "codex": await codex_state()}


async def codex_state() -> dict:
    if not codex_backend.ENABLED:
        return {"enabled": False}
    try:
        acc = await asyncio.wait_for(codex_backend.account(), 15)
    except Exception as e:
        return {"enabled": True, "error": str(e)}
    return {"enabled": True, "account": acc.get("account"), "limits": codex_backend.LAST_LIMITS}


@router.post("/api/codex/login")
async def codex_login(request: Request):
    """ChatGPT device-code login: the administrator opens the URL and enters the code."""
    require_admin(request, mutating=True)
    try:
        return await codex_backend.start_device_login()
    except codex_backend.CodexError as e:
        raise HTTPException(502, e.message)


@router.post("/api/codex/logout")
async def codex_logout(request: Request):
    require_admin(request, mutating=True)
    await codex_backend.logout()
    return {"ok": True}


@router.post("/api/codex/test")
async def codex_test(request: Request):
    require_admin(request, mutating=True)
    t0 = time.time()
    try:
        models = await codex_backend.server.list_models()
        model = next((m["id"] for m in models if "luna" in m["id"]), None) or await codex_backend.server.default_model()
        text = ""
        async for kind, val in codex_backend.run_codex("Reply with exactly: pong", [{"type": "text", "text": "ping"}], model, "low"):
            if kind == "text":
                text += val
        return {"ok": True, "reply": text.strip(), "model": model, "seconds": round(time.time() - t0, 1)}
    except codex_backend.CodexError as e:
        return {"ok": False, "error": e.message, "seconds": round(time.time() - t0, 1)}


@router.post("/api/keys")
async def create_key(request: Request):
    require_admin(request, mutating=True)
    body = await request.json()
    item, key = keys.create_key(str(body.get("name", "")))
    return {"key": key, "item": item}


@router.post("/api/keys/{key_id}/revoke")
async def revoke(key_id: str, request: Request):
    require_admin(request, mutating=True)
    if not keys.revoke_key(key_id):
        raise HTTPException(404, "Unknown key")
    return {"ok": True}


@router.delete("/api/keys/{key_id}")
async def delete(key_id: str, request: Request):
    require_admin(request, mutating=True)
    if not keys.revoke_key(key_id, delete=True):
        raise HTTPException(404, "Unknown key")
    return {"ok": True}


@router.put("/api/claude-token")
async def put_claude_token(request: Request):
    require_admin(request, mutating=True)
    tok = str((await request.json()).get("token", "")).strip()
    if tok and not tok.startswith("sk-ant-"):
        raise HTTPException(400, "Invalid claude setup-token token: expected the sk-ant- prefix")
    keys.set_claude_token(tok or None)
    return keys.claude_token_status()


@router.post("/api/test")
async def test(request: Request):
    """Make a small real request to verify the subscription token."""
    require_admin(request, mutating=True)
    t0 = time.time()
    try:
        text = ""
        async for kind, val in openai_compat.run_claude("Reply with exactly: pong", [{"type": "text", "text": "ping"}], "haiku", "low"):
            if kind == "text":
                text += val
        return {"ok": True, "reply": text.strip(), "seconds": round(time.time() - t0, 1)}
    except openai_compat.OAIError as e:
        return {"ok": False, "error": e.message, "seconds": round(time.time() - t0, 1)}
