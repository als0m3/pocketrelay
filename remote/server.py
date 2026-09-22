"""HTTP API, SSE and static UI."""

import asyncio
import json
import os
import secrets
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

from fastapi import Depends, FastAPI, HTTPException, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, JSONResponse, RedirectResponse, StreamingResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel, Field

from . import accounts, admin, codex_backend, history
from .openai_compat import OAIError, oai_error_handler, router as openai_router
from starlette.middleware.sessions import SessionMiddleware

from .config import ALLOWED_HOSTS, CLAUDE_BIN, DATA, ENABLE_DOCS, ENABLE_SESSIONS, SESSION_SECRET, STATIC, TOKEN
from .sessions import EFFORTS, PERMISSION_MODES, Manager, child_env

manager: Manager


@asynccontextmanager
async def lifespan(app: FastAPI):
    global manager
    manager = Manager()
    yield
    await manager.shutdown()
    await codex_backend.stop_all()


app = FastAPI(title="CustomRemote", version="0.1.0", lifespan=lifespan,
              docs_url="/docs" if ENABLE_DOCS else None, redoc_url=None,
              openapi_url="/openapi.json" if ENABLE_DOCS else None,
              description="Control the Claude Code subscription CLI over HTTP. Authentication: `Authorization: Bearer <token>`.")


app.add_exception_handler(OAIError, oai_error_handler)
app.include_router(openai_router)
app.include_router(admin.router)


@app.middleware("http")
async def guard(request: Request, call_next):
    # Reject unknown Hosts to prevent third-party DNS-rebinding access.
    host = (request.headers.get("host") or "").rsplit(":", 1)[0]
    if host not in ALLOWED_HOSTS and request.url.path != "/healthz":  # Kubernetes probes use the pod IP.
        return JSONResponse({"detail": f"Host rejected : {host}"}, status_code=403)
    response = await call_next(request)
    response.headers.setdefault("X-Content-Type-Options", "nosniff")
    response.headers.setdefault("X-Frame-Options", "DENY")
    response.headers.setdefault("Referrer-Policy", "same-origin")
    if request.url.path.startswith("/admin"):
        response.headers.setdefault("Content-Security-Policy",
                                    "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; "
                                    "frame-ancestors 'none'; form-action 'self' https:")
        response.headers.setdefault("Cache-Control", "no-store")
    return response


def auth(request: Request):
    if not ENABLE_SESSIONS:
        raise HTTPException(404, "Session mode is disabled on this instance")
    header = request.headers.get("authorization", "")
    tok = header[7:] if header.lower().startswith("bearer ") else request.query_params.get("token", "")
    if not secrets.compare_digest(tok, TOKEN):
        raise HTTPException(401, "Invalid token")


def get_session(sid: str):
    try:
        return manager.get(sid)
    except KeyError:
        raise HTTPException(404, "Unknown session")


# Bearer-token authentication, without cookies: CORS allows web chat clients to call /v1.
app.add_middleware(CORSMiddleware, allow_origins=["*"], allow_methods=["*"], allow_headers=["*"])
app.add_middleware(SessionMiddleware, secret_key=SESSION_SECRET, session_cookie="cr_admin",
                   same_site="lax", https_only=os.environ.get("REMOTE_HTTPS") == "1", max_age=12 * 3600)


@app.get("/healthz")
async def healthz():
    return {"ok": True}


# ---------- models ----------

class CreateSession(BaseModel):
    cwd: str
    name: str = ""
    model: str = ""
    permission_mode: str = "manual"
    effort: str = ""
    append_system_prompt: str = ""
    add_dirs: list[str] = []
    resume: str = Field("", description="CLI session ID to resume")
    fork: bool = False


class SendMessage(BaseModel):
    text: str = ""
    images: list[dict] = Field([], description="[{media_type, data(base64)}]")


class Decision(BaseModel):
    behavior: str = Field(pattern="^(allow|deny)$")
    message: str = ""
    always: bool = False
    updated_input: dict | None = None


class OneShot(BaseModel):
    prompt: str
    cwd: str = "~"
    model: str = ""
    permission_mode: str = "dontAsk"
    timeout: int = 600


# ---------- sessions ----------

api = [Depends(auth)]


@app.get("/api/health", dependencies=api)
async def health():
    return {"ok": True, "claude_bin": CLAUDE_BIN, "sessions": len(manager.sessions),
            "alive": sum(s.alive for s in manager.sessions.values()),
            "permission_modes": PERMISSION_MODES, "efforts": EFFORTS}


@app.get("/api/sessions", dependencies=api)
async def list_sessions():
    return manager.list_all()


@app.post("/api/sessions", dependencies=api)
async def create_session(body: CreateSession):
    try:
        s = manager.create(**body.model_dump())
    except ValueError as e:
        raise HTTPException(400, str(e))
    return s.summary()


@app.get("/api/sessions/{sid}", dependencies=api)
async def session_detail(sid: str):
    s = get_session(sid)
    return {"session": s.summary(), "events": s.load_events()}


@app.patch("/api/sessions/{sid}", dependencies=api)
async def patch_session(sid: str, patch: dict[str, Any]):
    s = get_session(sid)
    if "permission_mode" in patch and patch["permission_mode"] not in PERMISSION_MODES:
        raise HTTPException(400, "Unknown mode")
    await s.update(patch)
    return s.summary()


@app.delete("/api/sessions/{sid}", dependencies=api)
async def delete_session(sid: str):
    get_session(sid)
    await manager.delete(sid)
    return {"ok": True}


@app.post("/api/sessions/{sid}/messages", dependencies=api)
async def send_message(sid: str, body: SendMessage):
    s = get_session(sid)
    if not body.text.strip() and not body.images:
        raise HTTPException(400, "Empty message")
    if body.images:
        content = [{"type": "image", "source": {"type": "base64", "media_type": im["media_type"], "data": im["data"]}}
                   for im in body.images]
        if body.text.strip():
            content.append({"type": "text", "text": body.text})
    else:
        content = body.text
    try:
        await s.send(content)
    except RuntimeError as e:
        raise HTTPException(500, str(e))
    return {"ok": True}


@app.post("/api/sessions/{sid}/interrupt", dependencies=api)
async def interrupt(sid: str):
    await get_session(sid).interrupt()
    return {"ok": True}


@app.post("/api/sessions/{sid}/stop", dependencies=api)
async def stop(sid: str):
    await get_session(sid).stop()
    return {"ok": True}


@app.post("/api/sessions/{sid}/permissions/{rid}", dependencies=api)
async def decide(sid: str, rid: str, body: Decision):
    s = get_session(sid)
    try:
        await s.decide(rid, body.behavior, body.message, body.always, body.updated_input)
    except KeyError:
        raise HTTPException(404, "Unknown or expired permission request")
    return {"ok": True}


@app.delete("/api/sessions/{sid}/auto-allow/{tool}", dependencies=api)
async def revoke_auto_allow(sid: str, tool: str):
    s = get_session(sid)
    if tool in s.meta["auto_allow"]:
        s.meta["auto_allow"].remove(tool)
        s.touch()
    return s.summary()


# ---------- real-time event stream (SSE) ----------

def sse(obj) -> str:
    return f"data: {json.dumps(obj, ensure_ascii=False)}\n\n"


@app.get("/api/sessions/{sid}/stream", dependencies=api)
async def stream(sid: str, request: Request, since: int = 0):
    s = get_session(sid)

    async def gen():
        q: asyncio.Queue = asyncio.Queue()
        s.subs.add(q)
        try:
            last = since
            for ev in list(s.load_events()):
                if ev["seq"] > last:
                    yield sse(ev)
                    last = ev["seq"]
            yield sse({"type": "remote", "event": "snapshot", "session": s.summary()})
            while not await request.is_disconnected():
                try:
                    ev = await asyncio.wait_for(q.get(), 15)
                except asyncio.TimeoutError:
                    yield ": ping\n\n"
                    continue
                if ev.get("seq") and ev["seq"] <= last:
                    continue
                last = ev.get("seq") or last
                yield sse(ev)
        finally:
            s.subs.discard(q)

    return StreamingResponse(gen(), media_type="text/event-stream",
                             headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"})


@app.get("/api/events", dependencies=api)
async def global_events(request: Request):
    """Stream all session updates (status, cost, etc.) to the sidebar."""
    async def gen():
        q: asyncio.Queue = asyncio.Queue()
        manager.subs.add(q)
        try:
            yield sse({"type": "sessions", "sessions": manager.list_all()})
            if manager.limits:
                yield sse({"type": "limits", "limits": manager.limits})
            while not await request.is_disconnected():
                try:
                    yield sse(await asyncio.wait_for(q.get(), 15))
                except asyncio.TimeoutError:
                    yield ": ping\n\n"
        finally:
            manager.subs.discard(q)

    return StreamingResponse(gen(), media_type="text/event-stream", headers={"Cache-Control": "no-cache"})


# ---------- utilities ----------

@app.get("/api/history", dependencies=api)
async def cli_history(q: str = "", limit: int = 150):
    """List existing CLI sessions (terminal, VS Code, etc.) available to resume."""
    return await asyncio.to_thread(history.list_transcripts, limit, q)


@app.get("/api/fs", dependencies=api)
async def browse(path: str = "~"):
    home = Path.home().resolve()
    p = Path(os.path.expanduser(path)).resolve()
    if p != home and home not in p.parents:
        raise HTTPException(403, "Outside the home directory")
    if not p.is_dir():
        raise HTTPException(404, "Not a directory")
    try:
        dirs = sorted((c.name for c in p.iterdir() if c.is_dir() and not c.name.startswith(".")), key=str.lower)
    except PermissionError:
        dirs = []
    return {"path": str(p), "parent": str(p.parent) if p != home else None,
            "is_git": (p / ".git").exists(), "dirs": dirs}


@app.get("/api/recent-dirs", dependencies=api)
async def recent_dirs():
    seen, out = set(), []
    for m in sorted(manager.list_all(), key=lambda m: -m.get("updated", 0)):
        if m["cwd"] not in seen:
            seen.add(m["cwd"])
            out.append(m["cwd"])
    return out[:15]


SNIPPETS = DATA / "snippets.json"


@app.get("/api/snippets", dependencies=api)
async def get_snippets():
    return json.loads(SNIPPETS.read_text()) if SNIPPETS.exists() else []


@app.put("/api/snippets", dependencies=api)
async def put_snippets(items: list[dict]):
    SNIPPETS.write_text(json.dumps(items, ensure_ascii=False, indent=1))
    return items


@app.get("/api/usage", dependencies=api)
async def usage():
    ms = manager.list_all()
    return {"limits": manager.limits, "cost_usd": round(sum(m.get("cost_usd", 0) for m in ms), 4),
            "turns": sum(m.get("turns", 0) for m in ms),
            "by_session": [{"id": m["id"], "name": m["name"], "cost_usd": m.get("cost_usd", 0),
                            "turns": m.get("turns", 0)} for m in ms]}


@app.post("/api/run", dependencies=api)
async def run_once(body: OneShot):
    """One-off request without a session, suitable for curl, scripts and shortcuts."""
    if body.permission_mode not in PERMISSION_MODES:
        raise HTTPException(400, "Unknown mode")
    cwd = os.path.expanduser(body.cwd)
    args = [CLAUDE_BIN, "-p", body.prompt, "--output-format", "json",
            "--permission-mode", body.permission_mode, "--permission-prompts", "none",
            "--no-session-persistence"]
    if body.model:
        args += ["--model", body.model]
    proc = await asyncio.create_subprocess_exec(*args, cwd=cwd, env=child_env(accounts.session_token()),
                                                stdin=asyncio.subprocess.DEVNULL,
                                                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    try:
        out, err = await asyncio.wait_for(proc.communicate(), body.timeout)
    except asyncio.TimeoutError:
        proc.kill()
        raise HTTPException(504, "Timed out")
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        raise HTTPException(500, (err or out).decode(errors="replace")[-2000:])


# ---------- UI ----------

@app.get("/")
async def index():
    if not ENABLE_SESSIONS:
        return RedirectResponse("/admin")
    return FileResponse(STATIC / "index.html")


app.mount("/static", StaticFiles(directory=STATIC), name="static")
