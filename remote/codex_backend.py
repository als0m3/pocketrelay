"""ChatGPT subscription backend using codex app-server JSON-RPC over stdio.

Keep one app-server process running; each /v1 request opens an ephemeral
thread with replaced baseInstructions, a read-only sandbox,
no approvals and disabled model tools. Text arrives through
`item/agentMessage/delta`.
"""

import asyncio
import json
import os
import shutil
import time

from . import accounts, pdf
from .config import DATA

CODEX_BIN = os.environ.get("CODEX_BIN") or shutil.which("codex") or ""
ENABLED = bool(CODEX_BIN) and os.environ.get("REMOTE_ENABLE_CODEX", "1") == "1"
WORKDIR = DATA / "codex-cwd"
DISABLED_FEATURES = ["shell_tool", "apps", "browser_use", "browser_use_external", "computer_use",
                     "image_generation", "in_app_browser", "memories", "goals"]



class CodexError(Exception):
    def __init__(self, message: str, kind: str = "api_error"):
        super().__init__(message)
        self.message, self.kind = message, kind


class AppServer:
    """One codex app-server process per ChatGPT account, with its own CODEX_HOME."""

    def __init__(self, codex_home: str | None):
        self.codex_home = codex_home
        self.limits: dict = {}
        self.proc: asyncio.subprocess.Process | None = None
        self.next_id = 0
        self.pending: dict[int, asyncio.Future] = {}
        self.threads: dict[str, asyncio.Queue] = {}  # threadId -> notifications
        self.lock = asyncio.Lock()
        self.models: list[dict] = []
        self.models_at = 0.0
        self.login_events: asyncio.Queue = asyncio.Queue()

    @property
    def alive(self) -> bool:
        return self.proc is not None and self.proc.returncode is None

    async def start(self):
        async with self.lock:
            if self.alive:
                return
            args = [CODEX_BIN, "app-server", "-c", 'web_search="disabled"']
            for f in DISABLED_FEATURES:
                args += ["--disable", f]
            env = dict(os.environ)
            env.pop("OPENAI_API_KEY", None)  # use the ChatGPT subscription
            if self.codex_home:
                os.makedirs(self.codex_home, exist_ok=True)
                env["CODEX_HOME"] = self.codex_home
            WORKDIR.mkdir(parents=True, exist_ok=True)
            self.proc = await asyncio.create_subprocess_exec(
                *args, cwd=WORKDIR, env=env, limit=64 * 1024 * 1024,
                stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
            asyncio.create_task(self._read(self.proc))
            asyncio.create_task(self._drain_stderr(self.proc))
            await self._call("initialize", {"clientInfo": {"name": "custom-remote", "version": "0.1.0"}}, started=True)
            self._send({"method": "initialized"})

    def _send(self, obj: dict):
        self.proc.stdin.write((json.dumps(obj, ensure_ascii=False) + "\n").encode())

    async def _call(self, method: str, params: dict, timeout: float = 60, started: bool = False):
        if not started:
            await self.start()
        self.next_id += 1
        rid = self.next_id
        fut = asyncio.get_running_loop().create_future()
        self.pending[rid] = fut
        self._send({"id": rid, "method": method, "params": params})
        await self.proc.stdin.drain()
        try:
            msg = await asyncio.wait_for(fut, timeout)
        finally:
            self.pending.pop(rid, None)
        if "error" in msg:
            raise CodexError(msg["error"].get("message", str(msg["error"])))
        return msg.get("result") or {}

    call = _call

    async def _drain_stderr(self, proc):
        async for _ in proc.stderr:
            pass

    async def _read(self, proc):
        try:
            async for raw in proc.stdout:
                try:
                    o = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                if "id" in o and ("result" in o or "error" in o):
                    fut = self.pending.get(o["id"])
                    if fut and not fut.done():
                        fut.set_result(o)
                    continue
                method, params = o.get("method"), o.get("params") or {}
                if "id" in o:  # Reject unexpected server approval requests.
                    self._send({"id": o["id"], "result": {"decision": "decline"}})
                    continue
                if method == "account/rateLimits/updated":
                    self.limits = {**(params.get("rateLimits") or {}), "seen_at": time.time()}
                elif method in ("account/login/completed", "account/updated"):
                    self.login_events.put_nowait({"method": method, **params})
                tid = params.get("threadId")
                if tid and tid in self.threads:
                    self.threads[tid].put_nowait(o)
        finally:
            for fut in self.pending.values():
                if not fut.done():
                    fut.set_result({"error": {"message": "codex app-server stopped"}})
            for q in self.threads.values():
                q.put_nowait({"method": "_exited"})

    async def list_models(self) -> list[dict]:
        if self.models and time.time() - self.models_at < 600:
            return self.models
        res = await self.call("model/list", {})
        self.models = [m for m in res.get("data") or [] if not m.get("hidden")]
        self.models_at = time.time()
        return self.models

    async def stop(self):
        if self.alive:
            self.proc.terminate()


_servers: dict[str, AppServer] = {}


def server_for(acc: dict) -> AppServer:
    srv = _servers.get(acc["id"])
    if srv is None:
        srv = _servers[acc["id"]] = AppServer(accounts.codex_home(acc))
    return srv


async def stop_account(acc_id: str):
    srv = _servers.pop(acc_id, None)
    if srv:
        await srv.stop()


async def stop_all():
    for srv in list(_servers.values()):
        await srv.stop()


async def list_models() -> list[dict]:
    """Read the shared catalog from the first responding account."""
    for acc in accounts.listing("codex"):
        if not acc["enabled"]:
            continue
        try:
            return await server_for(acc).list_models()
        except Exception:
            continue
    return []


async def default_model() -> str:
    if env := os.environ.get("REMOTE_CODEX_MODEL"):
        return env
    models = await list_models()
    return next((m["id"] for m in models if m.get("isDefault")), models[0]["id"] if models else "gpt-5.5")


def expand_documents(blocks: list[dict]) -> list[dict]:
    """Replace unsupported Codex PDF inputs with text and scanned-page images."""
    out = []
    for b in blocks:
        if b["type"] == "document":
            try:
                out += pdf.convert(b["source"]["data"], b.get("title"))
            except pdf.PdfError as e:
                raise CodexError(str(e), "invalid_request_error")
        else:
            out.append(b)
    return out


def to_inputs(blocks: list[dict]) -> list[dict]:
    """Convert Anthropic blocks from the OpenAI layer into Codex UserInput."""
    out = []
    for b in blocks:
        if b["type"] == "text":
            if out and out[-1]["type"] == "text":
                out[-1]["text"] += b["text"]
            else:
                out.append({"type": "text", "text": b["text"]})
        elif b["type"] == "image":
            src = b["source"]
            url = src.get("url") or f"data:{src['media_type']};base64,{src['data']}"
            out.append({"type": "image", "url": url, **({"detail": b["detail"]} if b.get("detail") else {})})
    return out or [{"type": "text", "text": "(empty)"}]


async def run_codex(system: str, blocks: list[dict], model: str, effort: str | None,
                    output_schema: dict | None = None, server: AppServer | None = None):
    """Same interface as run_claude: ("text", delta), then ("done", info)."""
    if server is None:
        acc = accounts.pick("codex")
        if not acc:
            raise CodexError("No Codex account available.", "authentication_error")
        server = server_for(acc)
    params = {"baseInstructions": system, "ephemeral": True, "sandbox": "read-only",
              "approvalPolicy": "never", "cwd": str(WORKDIR), "model": model}
    thread = (await server.call("thread/start", params))["thread"]
    tid = thread["id"]
    q: asyncio.Queue = asyncio.Queue()
    server.threads[tid] = q
    if any(b["type"] == "document" for b in blocks):
        blocks = await asyncio.to_thread(expand_documents, blocks)
    turn_params = {"threadId": tid, "input": to_inputs(blocks), "model": model}
    if effort:
        turn_params["effort"] = effort
    if output_schema:
        turn_params["outputSchema"] = output_schema
    info = {"model": model, "usage": {}}
    finished = False
    last_item = None
    try:
        await server.call("turn/start", turn_params)
        while True:
            o = await asyncio.wait_for(q.get(), 600)
            m, p = o.get("method"), o.get("params") or {}
            if m == "item/agentMessage/delta":
                if last_item and p.get("itemId") != last_item:
                    yield "text", "\n\n"  # New message in the same turn.
                last_item = p.get("itemId")
                yield "text", p.get("delta", "")
            elif m == "thread/tokenUsage/updated":
                last = (p.get("tokenUsage") or {}).get("last") or {}
                cached = last.get("cachedInputTokens") or 0
                info["usage"] = {"input_tokens": max(0, (last.get("inputTokens") or 0) - cached),
                                 "cache_read_input_tokens": cached,
                                 "output_tokens": last.get("outputTokens") or 0,
                                 "output_tokens_details": {"thinking_tokens": last.get("reasoningOutputTokens") or 0}}
            elif m == "error" and not p.get("willRetry"):
                err = p.get("error") or {}
                raise CodexError(err.get("message") or "Error Codex", _kind(err))
            elif m == "turn/completed":
                turn = p.get("turn") or {}
                if turn.get("status") == "failed":
                    err = turn.get("error") or {}
                    raise CodexError(err.get("message") or "Codex turn failed", _kind(err))
                finished = True
                break
            elif m == "_exited":
                raise CodexError("codex app-server stopped during the request.")
        yield "done", info
    finally:
        server.threads.pop(tid, None)
        if server.alive:
            if not finished:
                try:
                    await server.call("turn/interrupt", {"threadId": tid}, timeout=10)
                except Exception:
                    pass
            try:
                await server.call("thread/unsubscribe", {"threadId": tid}, timeout=10)
            except Exception:
                pass


def _kind(err: dict) -> str:
    text = json.dumps(err).lower()
    if "invalid_request_error" in text or "invalid_json_schema" in text:
        return "invalid_request_error"
    if ("usage" in text and "limit" in text) or "rate_limit" in text or "ratelimit" in text:
        return "rate_limit_error"
    if "auth" in text or "login" in text or "unauthorized" in text:
        return "authentication_error"
    return "api_error"


# ---------- account (console /admin) ----------

async def account(srv: AppServer) -> dict:
    return await srv.call("account/read", {})


async def start_device_login(srv: AppServer) -> dict:
    return await srv.call("account/login/start", {"type": "chatgptDeviceCode"})


async def logout(srv: AppServer):
    await srv.call("account/logout", {})
