"""Admin console (/admin): Claude/Codex subscription accounts, API keys and quotas.

Use OIDC (Keycloak, etc.) when REMOTE_OIDC_ISSUER is configured, otherwise
use the master token for login or recovery. Sessions use signed cookies;
all mutations require the X-Admin header to prevent CSRF.
"""

import asyncio
import secrets
import time

from fastapi import APIRouter, HTTPException, Request
from fastapi.responses import FileResponse, RedirectResponse

from . import accounts, codex_backend, keys, openai_compat
from .config import (ADMIN_EMAILS, ADMIN_GROUPS, OIDC_CLIENT_ID, OIDC_CLIENT_SECRET, OIDC_ISSUER, PUBLIC_HOST, PUBLIC_URL,
                     STATIC, TOKEN)

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
    if email in ADMIN_EMAILS and claims.get("email_verified") is not False:  # Reject unverified email.
        return True
    groups = {g.strip("/") for g in claims.get("groups") or []}
    roles = set(claims.get("roles") or [])                                   # Flat roles mapper
    roles |= set((claims.get("realm_access") or {}).get("roles") or [])     # Keycloak realm roles
    roles |= set(((claims.get("resource_access") or {}).get(OIDC_CLIENT_ID) or {}).get("roles") or [])
    return bool((groups | roles) & ADMIN_GROUPS)


def open_session(request: Request, who: str, how: str):
    request.session["admin"] = {"who": who, "how": how, "exp": time.time() + 12 * 3600}


# ---------- pages & connection ----------

@router.get("")
@router.get("/")
async def page():
    return FileResponse(STATIC / "admin.html")


def via_public_proxy(request: Request) -> bool:
    """Identify the public VPS proxy by its public X-Forwarded-Host."""
    fwd = (request.headers.get("x-forwarded-host") or "").split(",")[0].strip().split(":")[0]
    return bool(PUBLIC_HOST) and fwd == PUBLIC_HOST


def token_login_allowed(request: Request) -> bool:
    # With SSO, master-token recovery is internal-only through NetBird.
    return not (oauth and via_public_proxy(request))


@router.get("/auth/config")
async def auth_config(request: Request):
    return {"oidc": bool(oauth), "token_login": token_login_allowed(request), "user": current_user(request)}


@router.get("/auth/login")
async def login(request: Request):
    if not oauth:
        raise HTTPException(404, "OIDC is not configured")
    if PUBLIC_URL:  # Behind the VPS, Host is internal: enforce the public URL.
        redirect = f"{PUBLIC_URL}/admin/auth/callback"
    else:
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
    if not token_login_allowed(request):
        raise HTTPException(403, "Token login is disabled from the Internet; use SSO.")
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
    codex_accs = [a for a in accounts.listing("codex") if a["enabled"]] if codex_backend.ENABLED else []
    await asyncio.gather(*(_refresh_identity(a) for a in codex_accs))
    return {
        "user": u,
        "keys": keys.list_keys(),
        "stats": openai_compat.STATS,
        "codex_enabled": codex_backend.ENABLED,
        "accounts": {p: [accounts.public(a) for a in accounts.listing(p)] for p in accounts.PROVIDERS},
    }


async def _refresh_identity(acc: dict):
    """ChatGPT account email and plan from account/read, cached in state."""
    srv = codex_backend.server_for(acc)
    try:
        res = await asyncio.wait_for(codex_backend.account(srv), 10)
        accounts.state(acc["id"])["identity"] = res.get("account")
        if srv.limits:
            accounts.record_limits(acc, srv.limits)
    except Exception as e:
        accounts.state(acc["id"])["identity_error"] = str(e)[:200]


def _account(acc_id: str) -> dict:
    try:
        return accounts.get(acc_id)
    except KeyError:
        raise HTTPException(404, "Unknown account")


def _check_claude_token(tok: str):
    if not tok.startswith("sk-ant-"):
        raise HTTPException(400, "Invalid claude setup-token token: expected the sk-ant- prefix.")


@router.post("/api/accounts")
async def add_account(request: Request):
    require_admin(request, mutating=True)
    body = await request.json()
    provider, label = body.get("provider"), str(body.get("label", "")).strip()
    if provider == "claude":
        tok = str(body.get("token", "")).strip()
        _check_claude_token(tok)
        acc = accounts.add_claude(label or "Claude account", tok)
        return {"account": accounts.public(acc), "test": await _test(acc)}
    if provider == "codex":
        if not codex_backend.ENABLED:
            raise HTTPException(400, "Codex is not installed on this server.")
        acc = accounts.add_codex(label or "ChatGPT account")
        return {"account": accounts.public(acc), "login": await _device_login(acc)}
    raise HTTPException(400, "Unknown provider")


async def _device_login(acc: dict) -> dict:
    try:
        return await codex_backend.start_device_login(codex_backend.server_for(acc))
    except codex_backend.CodexError as e:
        raise HTTPException(502, e.message)


@router.post("/api/accounts/{acc_id}/login")
async def relogin(acc_id: str, request: Request):
    require_admin(request, mutating=True)
    acc = _account(acc_id)
    if acc["provider"] != "codex":
        raise HTTPException(400, "Device-code login is only available for Codex accounts")
    accounts.resume(acc_id)
    return await _device_login(acc)


@router.put("/api/accounts/{acc_id}/token")
async def replace_token(acc_id: str, request: Request):
    require_admin(request, mutating=True)
    acc = _account(acc_id)
    if acc["provider"] != "claude" or acc["system"]:
        raise HTTPException(400, "Only added Claude accounts have a token")
    tok = str((await request.json()).get("token", "")).strip()
    _check_claude_token(tok)
    accounts.set_token(acc_id, tok)
    return {"account": accounts.public(acc), "test": await _test(acc)}


@router.patch("/api/accounts/{acc_id}")
async def patch_account(acc_id: str, request: Request):
    require_admin(request, mutating=True)
    _account(acc_id)
    acc = accounts.update(acc_id, await request.json())
    if not acc["enabled"] and acc["provider"] == "codex":
        await codex_backend.stop_account(acc_id)  # Release process memory.
    return accounts.public(acc)


@router.post("/api/accounts/{acc_id}/move")
async def move_account(acc_id: str, request: Request):
    require_admin(request, mutating=True)
    _account(acc_id)
    accounts.move(acc_id, -1 if (await request.json()).get("delta", 1) < 0 else 1)
    return {"ok": True}


@router.post("/api/accounts/{acc_id}/resume")
async def resume_account(acc_id: str, request: Request):
    require_admin(request, mutating=True)
    _account(acc_id)
    accounts.resume(acc_id)
    return {"ok": True}


@router.post("/api/accounts/{acc_id}/logout")
async def logout_account(acc_id: str, request: Request):
    require_admin(request, mutating=True)
    acc = _account(acc_id)
    if acc["provider"] == "codex":
        await codex_backend.logout(codex_backend.server_for(acc))
        accounts.state(acc_id).pop("identity", None)
    return {"ok": True}


@router.delete("/api/accounts/{acc_id}")
async def delete_account(acc_id: str, request: Request):
    require_admin(request, mutating=True)
    _account(acc_id)
    await codex_backend.stop_account(acc_id)
    try:
        accounts.remove(acc_id)
    except ValueError as e:
        raise HTTPException(400, str(e))
    return {"ok": True}


@router.post("/api/accounts/{acc_id}/test")
async def test_account(acc_id: str, request: Request):
    require_admin(request, mutating=True)
    return await _test(_account(acc_id))


async def _test(acc: dict) -> dict:
    """Make a real ping/pong request on this account, without failover."""
    t0 = time.time()
    text, model = "", ""
    try:
        if acc["provider"] == "claude":
            model = "haiku"
            gen = openai_compat.run_claude("Reply with exactly: pong", [{"type": "text", "text": "ping"}], model, "low",
                                           accounts.token(acc), lambda lim: accounts.record_limits(acc, lim))
        else:
            models = await codex_backend.list_models()
            model = next((m["id"] for m in models if "luna" in m["id"]), None) or await codex_backend.default_model()
            gen = openai_compat._codex("Reply with exactly: pong", [{"type": "text", "text": "ping"}], model, "low", None, acc)
        async for kind, val in gen:
            if kind == "text":
                text += val
        accounts.record_success(acc)
        return {"ok": True, "reply": text.strip(), "model": model, "seconds": round(time.time() - t0, 1)}
    except openai_compat.OAIError as e:
        accounts.record_failure(acc, openai_compat._failure_kind(e), e.message, getattr(e, "resets_at", None))
        return {"ok": False, "error": e.message, "model": model, "seconds": round(time.time() - t0, 1)}


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


