"""Couche compatible OpenAI : /v1/models, /v1/chat/completions, /v1/completions, /v1/responses.

Each request launches an ephemeral claude -p process as a pure chat model:
no Claude Code tools, --safe-mode (no CLAUDE.md / skills / MCP), and a system
prompt supplied by the client. The CLI accepts one user message, so
multi-turn history is rendered inside a <conversation> tag. Function
calls are prompt-emulated: the model emits blocks
<tool_call>{...}</tool_call>, converted to OpenAI tool calls.
"""

import asyncio
import base64
import json
import os
import re
import secrets
import time
import urllib.request
import uuid
from collections import OrderedDict
from contextlib import aclosing

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse, StreamingResponse

from . import accounts, antigravity_backend, codex_backend, keys, limits
from .config import CLAUDE_BIN, DATA, TOKEN
from .sessions import child_env

router = APIRouter(prefix="/v1")

DEFAULT_MODEL = os.environ.get("REMOTE_OAI_MODEL", "sonnet")
ALIASES = {"opus", "sonnet", "haiku", "fable", "default", "opusplan"}
MODELS = ["opus", "sonnet", "haiku", "fable", "claude-opus-5", "claude-sonnet-5", "claude-fable-5-1",
          "claude-haiku-4-5", "claude-haiku-4-5-20251001"]
EFFORT_MAP = {"minimal": "low", "none": "low", "low": "low", "medium": "medium", "high": "high",
              "xhigh": "xhigh", "max": "max"}
WORKDIR = DATA / "oai-cwd"
DEBUG = os.environ.get("REMOTE_OAI_DEBUG") == "1"
TOOL_OPEN = "<tool_call>"
STATS = {"requests": 0, "errors": 0, "output_tokens": 0}
TOOL_MARKERS = [TOOL_OPEN, "<function_calls>"]  # Claude sometimes uses the second native format.


# ---------- OpenAI-style errors ----------

class OAIError(Exception):
    def __init__(self, status: int, message: str, type_: str = "invalid_request_error",
                 param: str | None = None, code: str | None = None):
        super().__init__(message)
        self.status, self.message, self.type, self.param, self.code = status, message, type_, param, code

    def body(self) -> dict:
        return {"error": {"message": self.message, "type": self.type, "param": self.param, "code": self.code}}


async def oai_error_handler(request: Request, exc: OAIError):
    return JSONResponse(exc.body(), status_code=exc.status)


# Allow the master token on /v1 locally; production accepts revocable keys only.
V1_ALLOW_MASTER = os.environ.get("REMOTE_V1_ALLOW_MASTER", "1") == "1"


def check_auth(request: Request) -> dict:
    header = request.headers.get("authorization", "")
    key = header[7:].strip() if header.lower().startswith("bearer ") else request.headers.get("x-api-key", "")
    if key and V1_ALLOW_MASTER and secrets.compare_digest(key, TOKEN):
        return {"master": True}
    item = keys.verify(key) if key else None
    if not item:
        raise OAIError(401, "Incorrect API key provided.", "invalid_request_error", code="invalid_api_key")
    return {"key_id": item["id"], "key_name": item["name"]}


async def read_json(request: Request) -> dict:
    ident = check_auth(request)
    request.state.owner = limits.subject(request.headers, ident)
    try:
        limits.admit(request.headers, ident)
    except limits.LimitError as e:
        raise OAIError(e.status, e.message, "rate_limit_error", code=e.code)
    try:
        body = await request.json()
    except Exception:
        raise OAIError(400, "We could not parse the JSON body of your request.")
    if not isinstance(body, dict):
        raise OAIError(400, "Request body must be a JSON object.")
    return body


def new_id(prefix: str) -> str:
    return f"{prefix}{uuid.uuid4().hex[:24]}"


def _strip(model: str | None) -> str:
    m = (model or "").strip()
    for prefix in ("anthropic/", "claude-code/", "claude/", "openai/", "google/", "antigravity/"):
        if m.startswith(prefix):
            m = m[len(prefix):]
    return m


def is_claude(model: str) -> bool:
    return model.startswith("claude") or model.split("[")[0] in ALIASES


def is_antigravity(model: str) -> bool:
    # Route by name so missing Antigravity CLI fails explicitly.
    # Fail clearly rather than silently switching providers.
    return model.startswith(("gemini", "gemma", "gpt-oss")) or model in antigravity_backend.MODELS


def provider_of(model: str) -> str:
    return "claude" if is_claude(model) else "antigravity" if is_antigravity(model) else "codex"


def split_account(model: str) -> tuple[dict | None, str]:
    """Resolve account/model into (account, model), otherwise (None, model)."""
    head, sep, rest = model.partition("/")
    if sep:
        try:
            return accounts.get(head), rest
        except KeyError:
            pass
    return None, model


async def resolve_model(model: str | None) -> str:
    """Resolve a requested model while preserving its account prefix."""
    acc, m = split_account(_strip(model))
    real = await _resolve_plain(m)
    if acc:
        if acc["provider"] != provider_of(real):
            raise OAIError(400, f"Model {m} does not belong to account {acc['label']}.", param="model")
        return f"{acc['slug']}/{real}"
    return real


async def _resolve_plain(m: str) -> str:
    """Use Claude for aliases/claude-*; Codex for its IDs and OpenAI-style names."""
    if m and is_claude(m):
        return m
    if m and is_antigravity(m):
        return m
    if codex_backend.ENABLED:
        try:
            ids = {x["id"] for x in await codex_backend.list_models()}
        except Exception:
            ids = set()
        if m in ids:
            return m
        if m == "codex" or m.startswith(("codex/", "gpt", "o1", "o3", "o4", "chatgpt")):
            sub = m.removeprefix("codex/")
            if sub in ids:
                return sub
            if REQUIRE_ACCOUNT:   # No silent fallback to the default Codex model.
                raise OAIError(400, f"Unknown Codex model: {m}. Use a name listed by /v1/models.",
                               param="model", code="model_not_found")
            return await codex_backend.default_model()
    if REQUIRE_ACCOUNT:
        raise OAIError(400, f"Unknown model: {m}. Use a name listed by /v1/models.",
                       param="model", code="model_not_found")
    return DEFAULT_MODEL  # Unknown names use the default Claude model.


def _failure_kind(e: OAIError) -> str:
    if e.status == 429:
        return "rate_limit"
    if e.status in (401, 503) or e.type == "authentication_error" or e.code == "subscription_not_configured":
        return "auth"
    return "other"


async def run_llm(system: str, blocks: list[dict], model: str, effort: str | None, schema: dict | None = None):
    """Route to a backend and account using the same text/done event stream.

    Before any text is emitted, quota/authentication failures pause
    the account and retry the next one. Account-prefixed model
    names, such as <id>/sonnet, use only that account.
    """
    pinned, model = split_account(model)
    if REQUIRE_ACCOUNT and not pinned:
        raise OAIError(400, f"Specify the account: <account>/{model}, for example from "
                            "/v1/models. No default account is selected for you.",
                       param="model", code="account_required")
    provider = provider_of(model)
    if provider == "antigravity" and not antigravity_backend.ENABLED:
        raise OAIError(503, f"Model {model} requires Antigravity CLI, which is not installed.", "api_error",
                       code="provider_unavailable")
    if pinned and not pinned["enabled"]:
        raise OAIError(503, f"Account {pinned['label']} is disabled in the console.", "api_error",
                       code="account_disabled")
    if provider == "codex" and effort in ("minimal", "none"):
        effort = "low"  # niveaux absents chez Codex
    tried: set[str] = set()
    last_error: OAIError | None = None
    while True:
        if pinned:
            acc = None if pinned["id"] in tried else pinned
        else:
            acc = accounts.pick(provider, tried)
        if acc is None:
            raise last_error or OAIError(503, f"No {provider} account available: add one in /admin.",
                                         "api_error", code="no_account_available")
        tried.add(acc["id"])
        started = False
        try:
            async with limits.slot():
                if provider == "claude":
                    gen = run_claude(system, blocks, model, EFFORT_MAP.get(effort or ""), accounts.token(acc),
                                     lambda lim, a=acc: accounts.record_limits(a, lim))
                elif provider == "antigravity":
                    gen = _antigravity(system, blocks, model, effort, schema, acc)
                else:
                    gen = _codex(system, blocks, model, effort, schema, acc)
                async with aclosing(gen) as inner:
                    async for ev in inner:
                        if ev[0] == "text":
                            started = True
                        elif ev[0] == "done":
                            ev[1]["account"] = acc["label"]
                        yield ev
                accounts.record_success(acc)
                return
        except limits.LimitError as e:
            raise OAIError(e.status, e.message, "api_error", code=e.code)
        except OAIError as e:
            last_error = e
            if not accounts.record_failure(acc, _failure_kind(e), e.message, getattr(e, "resets_at", None)) or started:
                raise


async def _antigravity(system, blocks, model, effort, schema, acc):
    try:
        async with aclosing(antigravity_backend.run_antigravity(system, blocks, model, effort, acc, schema)) as inner:
            async for ev in inner:
                if ev[0] == "done":
                    STATS["requests"] += 1
                    STATS["output_tokens"] += ev[1]["usage"].get("output_tokens") or 0
                yield ev
    except antigravity_backend.AntigravityError as e:
        STATS["errors"] += 1
        status = {"rate_limit_error": 429, "authentication_error": 503, "invalid_request_error": 400}.get(e.kind, 502)
        raise OAIError(status, e.message, e.kind)


async def _codex(system, blocks, model, effort, schema, acc):
    srv = codex_backend.server_for(acc)
    try:
        async with aclosing(codex_backend.run_codex(system, blocks, model, effort or None, schema, srv)) as inner:
            async for ev in inner:
                if ev[0] == "done":
                    STATS["requests"] += 1
                    STATS["output_tokens"] += ev[1]["usage"].get("output_tokens") or 0
                yield ev
    except codex_backend.CodexError as e:
        STATS["errors"] += 1
        status = {"rate_limit_error": 429, "authentication_error": 503, "invalid_request_error": 400}.get(e.kind, 502)
        err = OAIError(status, e.message, e.kind)
        win = (srv.limits.get("primary") or {}) if e.kind == "rate_limit_error" else {}
        err.resets_at = win.get("resetsAt")
        raise err
    finally:
        if srv.limits:
            accounts.record_limits(acc, srv.limits)


# ---------- content conversion ----------

def _check_public_url(url: str):
    """Reject internal network URLs to prevent SSRF from the pod into the cluster."""
    import ipaddress
    import socket
    from urllib.parse import urlparse
    u = urlparse(url)
    if u.scheme not in ("http", "https") or not u.hostname:
        raise OAIError(400, "Only http(s) image URLs are allowed.", param="image_url")
    try:
        infos = socket.getaddrinfo(u.hostname, u.port or (443 if u.scheme == "https" else 80))
    except socket.gaierror:
        raise OAIError(400, f"Cannot resolve {u.hostname}.", param="image_url")
    for info in infos:
        ip = ipaddress.ip_address(info[4][0])
        if not ip.is_global:
            raise OAIError(400, "Image URL points to a non-public address.", param="image_url")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        _check_public_url(newurl)  # Redirects must not bypass validation.
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def _fetch_image(url: str) -> dict:
    _check_public_url(url)
    req = urllib.request.Request(url, headers={"User-Agent": "CustomRemote"})
    with urllib.request.build_opener(_NoRedirect).open(req, timeout=20) as r:
        data = r.read(20 * 1024 * 1024 + 1)
        mt = r.headers.get_content_type()
    if len(data) > 20 * 1024 * 1024:
        raise OAIError(400, "Image too large (max 20 MB).", param="image_url")
    if not mt.startswith("image/"):
        raise OAIError(400, f"URL did not return an image ({mt}).", param="image_url")
    return {"type": "image", "source": {"type": "base64", "media_type": mt, "data": base64.b64encode(data).decode()}}


def _data_url_block(url: str, param: str) -> dict:
    m = re.match(r"data:([\w/+.-]+);base64,(.*)$", url, re.S)
    if not m:
        raise OAIError(400, "Invalid data URL.", param=param)
    mt, data = m.group(1), m.group(2)
    if mt == "application/pdf":
        return {"type": "document", "source": {"type": "base64", "media_type": mt, "data": data}}
    if mt.startswith("image/"):
        return {"type": "image", "source": {"type": "base64", "media_type": mt, "data": data}}
    if mt.startswith("text/") or mt in ("application/json",):
        return {"type": "text", "text": base64.b64decode(data).decode(errors="replace")}
    raise OAIError(400, f"Unsupported file type: {mt}", param=param)


async def to_blocks(content) -> list[dict]:
    """Convert OpenAI chat/response content strings or parts into Anthropic blocks."""
    if content is None:
        return []
    if isinstance(content, str):
        return [{"type": "text", "text": content}]
    if not isinstance(content, list):
        raise OAIError(400, "Invalid message content.", param="messages")
    out = []
    for part in content:
        if isinstance(part, str):
            out.append({"type": "text", "text": part})
            continue
        t = part.get("type")
        if t in ("text", "input_text", "output_text"):
            out.append({"type": "text", "text": part.get("text", "")})
        elif t == "refusal":
            out.append({"type": "text", "text": part.get("refusal", "")})
        elif t in ("image_url", "input_image"):
            iu = part.get("image_url")
            url = iu.get("url") if isinstance(iu, dict) else iu
            if not url:
                raise OAIError(400, "Missing image URL.", param="image_url")
            out.append(_data_url_block(url, "image_url") if url.startswith("data:")
                       else await asyncio.to_thread(_fetch_image, url))
        elif t in ("file", "input_file"):
            f = part.get("file", part)
            data = f.get("file_data")
            if not data:
                raise OAIError(400, "Only inline file_data is supported for files.", param="file")
            if not data.startswith("data:"):
                data = f"data:application/pdf;base64,{data}"
            block = _data_url_block(data, "file")
            if block["type"] == "document" and f.get("filename"):
                block["title"] = f["filename"]
            out.append(block)
        elif t in ("input_audio", "audio"):
            raise OAIError(400, "Audio input is not supported.", param="messages")
        else:
            out.append({"type": "text", "text": json.dumps(part, ensure_ascii=False)})
    return out


def text_of(blocks: list[dict]) -> str:
    return "\n".join(b["text"] for b in blocks if b["type"] == "text")


# ---------- prompt construction ----------

def tools_prompt(tools: list[dict], tool_choice, parallel: bool) -> str:
    fns = []
    for t in tools:
        fn = t.get("function", t)  # chat: {type, function:{…}} ; responses: {type, name, …}
        if t.get("type", "function") != "function" or not fn.get("name"):
            continue
        fns.append({"name": fn["name"], "description": fn.get("description", ""),
                    "parameters": fn.get("parameters") or {"type": "object", "properties": {}}})
    if not fns:
        return ""
    lines = [
        "# Function calling",
        "You can call the following functions. Their parameters are described with JSON Schema:",
        "<functions>", *(json.dumps(f, ensure_ascii=False) for f in fns), "</functions>",
        "",
        "To call a function, output a block in exactly this format:",
        f'{TOOL_OPEN}\n{{"name": "<function name>", "arguments": {{<arguments as a JSON object>}}}}\n</tool_call>',
        "Rules:",
        "- You may write a short sentence before the calls, but nothing after the last </tool_call>.",
        "- After your calls, stop: the results will arrive later inside <tool_result> blocks.",
        "- Only call the functions listed above, with arguments that match their schema.",
        "- If no function is needed, just answer normally without any block.",
        "- Call at most one function per message." if not parallel else "- You may emit several blocks to call functions in parallel.",
    ]
    if tool_choice == "required":
        lines.append("- You MUST call at least one function in this message.")
    elif isinstance(tool_choice, dict):
        name = (tool_choice.get("function") or {}).get("name") or tool_choice.get("name")
        if name:
            lines.append(f"- You MUST call the function `{name}` in this message.")
    return "\n".join(lines)


def format_prompt(response_format: dict | None) -> str:
    if not response_format:
        return ""
    t = response_format.get("type")
    if t == "json_object":
        return "# Output format\nRespond with a single valid JSON object only: no prose, no code fences."
    if t == "json_schema":
        js = response_format.get("json_schema", response_format)
        schema = js.get("schema", {})
        return ("# Output format\nRespond with a single valid JSON value only (no prose, no code fences) "
                f"that strictly conforms to this JSON Schema:\n{json.dumps(schema, ensure_ascii=False)}")
    return ""


def render_tool_call(name: str, arguments) -> str:
    if isinstance(arguments, str):
        try:
            arguments = json.loads(arguments or "{}")
        except json.JSONDecodeError:
            pass
    return f'{TOOL_OPEN}\n{json.dumps({"name": name, "arguments": arguments}, ensure_ascii=False)}\n</tool_call>'


async def build_prompt(turns: list[dict], system_parts: list[str], extra: list[str]) -> tuple[str, list[dict]]:
    """turns: [{role: user|assistant|tool, blocks, tool_calls?, tool_call_id?, name?}]"""
    system = "\n\n".join(p for p in system_parts if p.strip()) or "You are a helpful assistant."
    extra = [e for e in extra if e]
    if len(turns) == 1 and turns[0]["role"] == "user":
        return "\n\n".join([system, *extra]), turns[0]["blocks"] or [{"type": "text", "text": "(empty)"}]

    system += ("\n\n# Conversation format\nThe user message contains the conversation so far inside <conversation> "
               "tags. Write only the next assistant message, as yourself, without any role tag.")
    blocks: list[dict] = []

    def add_text(s: str):
        if blocks and blocks[-1]["type"] == "text":
            blocks[-1]["text"] += s
        else:
            blocks.append({"type": "text", "text": s})

    add_text("<conversation>\n")
    for t in turns:
        if t["role"] == "user":
            add_text("<user>\n")
            for b in t["blocks"]:
                add_text(b["text"]) if b["type"] == "text" else blocks.append(b)
            add_text("\n</user>\n")
        elif t["role"] == "assistant":
            body = text_of(t["blocks"])
            calls = "\n".join(render_tool_call(c["name"], c["arguments"]) for c in t.get("tool_calls") or [])
            add_text(f"<assistant>\n{body}{chr(10) if body and calls else ''}{calls}\n</assistant>\n")
        elif t["role"] == "tool":
            add_text(f'<tool_result tool_call_id="{t.get("tool_call_id", "")}" name="{t.get("name", "")}">\n'
                     f'{text_of(t["blocks"])}\n</tool_result>\n')
    add_text("</conversation>")
    return "\n\n".join([system, *extra]), blocks


# ---------- CLI execution ----------

async def run_claude(system: str, blocks: list[dict], model: str, effort: str | None,
                     token: str | None = None, on_limits=None):
    """Yield text/done events; token is the selected account setup-token."""
    WORKDIR.mkdir(parents=True, exist_ok=True)
    args = [CLAUDE_BIN, "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
            "--include-partial-messages", "--tools", "", "--system-prompt", system,
            "--no-session-persistence", "--safe-mode", "--permission-mode", "dontAsk", "--model", model]
    if effort:
        args += ["--effort", effort]
    proc = await asyncio.create_subprocess_exec(
        *args, cwd=WORKDIR, env=child_env(token), limit=64 * 1024 * 1024,
        stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    msg = {"type": "user", "message": {"role": "user", "content": blocks}, "parent_tool_use_id": None, "session_id": ""}
    proc.stdin.write((json.dumps(msg, ensure_ascii=False) + "\n").encode())
    await proc.stdin.drain()
    proc.stdin.close()
    info = {"model": model, "usage": {}}
    streamed = False
    limits: dict = {}
    try:
        async for raw in proc.stdout:
            try:
                o = json.loads(raw)
            except json.JSONDecodeError:
                continue
            t = o.get("type")
            if t == "stream_event":
                ev = o.get("event") or {}
                if ev.get("type") == "content_block_delta" and (ev.get("delta") or {}).get("type") == "text_delta":
                    streamed = True
                    yield "text", ev["delta"]["text"]
            elif t == "system" and o.get("subtype") == "init":
                info["model"] = o.get("model") or model
            elif t == "rate_limit_event":
                limits = o.get("rate_limit_info") or {}
                if on_limits:
                    on_limits(limits)
            elif t == "result":
                if o.get("is_error"):
                    STATS["errors"] += 1
                    text = str(o.get("result") or o.get("subtype") or "error")
                    low = text.lower()
                    if any(k in low for k in ("not logged in", "/login", "authenticat", "oauth", "401", "token is invalid",
                                              "token has expired", "invalid api key", "unauthorized")):
                        raise OAIError(503, "Claude account signed out: claude setup-token token missing or expired.",
                                       "api_error", code="subscription_not_configured")
                    if "limit" in low or "rate" in low:
                        err = OAIError(429, text, "rate_limit_error", code="rate_limit_exceeded")
                        err.resets_at = limits.get("resetsAt")
                        raise err
                    raise OAIError(502, text, "api_error")
                if not streamed and o.get("result"):
                    yield "text", o["result"]
                info["usage"] = o.get("usage") or {}
                STATS["requests"] += 1
                STATS["output_tokens"] += info["usage"].get("output_tokens") or 0
                break
        else:
            err = (await proc.stderr.read()).decode(errors="replace")[-1500:]
            raise OAIError(502, f"Claude CLI exited without a result. {err}".strip(), "api_error")
        yield "done", info
    finally:
        if proc.returncode is None:
            proc.kill()
        await proc.wait()


def usage_counts(u: dict) -> tuple[int, int, int, int]:
    cached = u.get("cache_read_input_tokens") or 0
    prompt = (u.get("input_tokens") or 0) + cached + (u.get("cache_creation_input_tokens") or 0)
    completion = u.get("output_tokens") or 0
    reasoning = (u.get("output_tokens_details") or {}).get("thinking_tokens") or 0
    return prompt, completion, cached, reasoning


class Filter:
    """Split visible text, tool-call starts and stop sequences without leaking markers."""

    def __init__(self, stops: list[str], tools: bool):
        self.stops = [s for s in stops if s]
        self.markers = self.stops + (TOOL_MARKERS if tools else [])
        self.buf = self.raw = ""
        self.closed = False
        self.stopped = False  # A stop sequence was encountered.

    def feed(self, s: str) -> str:
        if self.stopped:
            return ""
        self.raw += s  # Retain raw output for tool-call parsing.
        if self.closed:
            return ""
        self.buf += s
        hits = [(self.buf.find(m), m) for m in self.markers if m in self.buf]
        if hits:
            i, m = min(hits)
            self.closed = True
            self.stopped = m not in TOOL_MARKERS
            if self.stopped:
                self.raw = self.raw[: len(self.raw) - len(self.buf) + i]
            out, self.buf = self.buf[:i], ""
            return out
        keep = 0
        for m in self.markers:
            for k in range(min(len(m) - 1, len(self.buf)), 0, -1):
                if self.buf.endswith(m[:k]):
                    keep = max(keep, k)
                    break
        out, self.buf = self.buf[: len(self.buf) - keep], self.buf[len(self.buf) - keep:]
        return out

    def flush(self) -> str:
        out, self.buf = ("" if self.closed else self.buf), ""
        return out


TOOL_RE = re.compile(r"<tool_call>\s*(.*?)\s*</tool_call>", re.S)
FCALLS_RE = re.compile(r"<function_calls>\s*(.*?)\s*</function_calls>", re.S)
INVOKE_RE = re.compile(r'<invoke name="([^"]+)">(.*?)</invoke>', re.S)
PARAM_RE = re.compile(r'<parameter name="([^"]+)">(.*?)</parameter>', re.S)


def _json_calls(body: str) -> list[dict]:
    """A block may contain an object, a list or consecutive objects."""
    body = body.strip().removeprefix("```json").removeprefix("```").removesuffix("```").strip()
    dec, i, out = json.JSONDecoder(), 0, []
    while i < len(body):
        while i < len(body) and body[i] in " \t\r\n,":
            i += 1
        if i >= len(body):
            break
        try:
            obj, i = dec.raw_decode(body, i)
        except json.JSONDecodeError:
            break
        out += obj if isinstance(obj, list) else [obj]
    return [o for o in out if isinstance(o, dict) and o.get("name")]


def parse_tool_calls(raw: str) -> tuple[str, list[dict]]:
    found = []
    for m in TOOL_RE.finditer(raw):
        found += _json_calls(m.group(1))
    for m in FCALLS_RE.finditer(raw):
        inner = m.group(1)
        invokes = INVOKE_RE.findall(inner)
        if invokes:
            for name, params in invokes:
                args = {}
                for k, v in PARAM_RE.findall(params):
                    try:
                        args[k] = json.loads(v)
                    except json.JSONDecodeError:
                        args[k] = v.strip()
                found.append({"name": name, "arguments": args})
        else:
            found += _json_calls(inner)
    calls = []
    for o in found:
        args = o.get("arguments", o.get("parameters", {}))
        calls.append({"id": new_id("call_"), "name": o["name"],
                      "arguments": args if isinstance(args, str) else json.dumps(args, ensure_ascii=False)})
    if not calls:
        if DEBUG and any(mk in raw for mk in TOOL_MARKERS):
            print(f"[oai] unrecognized tool call:\n{raw}", flush=True)
        return raw, []
    cut = min((raw.find(mk) for mk in TOOL_MARKERS if mk in raw), default=len(raw))
    return raw[:cut].strip(), calls


def clean_json(text: str) -> str:
    t = text.strip()
    m = re.match(r"^```(?:json)?\s*(.*?)\s*```$", t, re.S)
    return m.group(1) if m else t


def as_stop_list(stop) -> list[str]:
    if not stop:
        return []
    return [stop] if isinstance(stop, str) else [s for s in stop if isinstance(s, str)][:4]


def sse(obj) -> str:
    return f"data: {json.dumps(obj, ensure_ascii=False)}\n\n"


# ---------- /v1/models ----------

CLAUDE_NAMES = {"opus": "Opus", "sonnet": "Sonnet", "haiku": "Haiku", "fable": "Fable"}
# Require account/model names without Auto, failover or default model routing.
# Set REMOTE_REQUIRE_ACCOUNT=0 to restore the previous behavior.
REQUIRE_ACCOUNT = os.environ.get("REMOTE_REQUIRE_ACCOUNT", "1") == "1"
LIST_AUTO = os.environ.get("REMOTE_MODELS_AUTO", "1") == "1" and not REQUIRE_ACCOUNT  # « Auto · … »
LIST_PER_ACCOUNT = os.environ.get("REMOTE_MODELS_PER_ACCOUNT", "1") == "1"  # Account-prefixed entries pin the account.


@router.get("/models")
async def list_models(request: Request):
    check_auth(request)
    now = int(time.time())

    def entry(mid, name, owner):
        return {"id": mid, "name": name, "object": "model", "created": now, "owned_by": owner}

    codex_models = []
    if codex_backend.ENABLED:
        try:
            codex_models = await codex_backend.list_models()
        except Exception:
            pass  # Continue serving Claude when Codex is unavailable or signed out.
    data = []
    # As with Claude, an active account is sufficient; do not probe login status.
    agy_ready = antigravity_backend.ENABLED and any(a["enabled"] for a in accounts.listing("antigravity"))
    agy_models = antigravity_backend.MODELS if agy_ready else {}
    if LIST_AUTO:
        data += [entry(m, f"Auto · {CLAUDE_NAMES[m]}", "anthropic") for m in CLAUDE_NAMES]
        data += [entry(m["id"], f"Auto · {m.get('displayName') or m['id']}", "openai") for m in codex_models]
        data += [entry(m, f"Auto · {n}", "google") for m, n in agy_models.items()]
    if LIST_PER_ACCOUNT:
        for acc in accounts.listing("claude"):
            if acc["enabled"]:
                data += [entry(f"{acc['slug']}/{m}", f"{acc['label']} · {n}", "anthropic") for m, n in CLAUDE_NAMES.items()]
        for acc in accounts.listing("codex") if codex_models else []:
            if acc["enabled"]:
                data += [entry(f"{acc['slug']}/{m['id']}", f"{acc['label']} · {m.get('displayName') or m['id']}", "openai")
                         for m in codex_models]
        for acc in accounts.listing("antigravity") if agy_models else []:
            if acc["enabled"]:
                data += [entry(f"{acc['slug']}/{m}", f"{acc['label']} · {n}", "google") for m, n in agy_models.items()]
    # Full model IDs remain accepted even when omitted from the catalog.
    return {"object": "list", "data": data}


@router.get("/models/{model_id:path}")
async def get_model(model_id: str, request: Request):
    check_auth(request)
    return {"id": model_id, "object": "model", "created": int(time.time()), "owned_by": "anthropic"}


# ---------- /v1/chat/completions ----------

async def chat_turns(messages: list) -> tuple[list[str], list[dict]]:
    if not isinstance(messages, list) or not messages:
        raise OAIError(400, "'messages' must be a non-empty array.", param="messages")
    system, turns = [], []
    for i, m in enumerate(messages):
        role = m.get("role")
        if role in ("system", "developer"):
            system.append(text_of(await to_blocks(m.get("content"))))
        elif role == "user":
            turns.append({"role": "user", "blocks": await to_blocks(m.get("content"))})
        elif role == "assistant":
            calls = [{"name": c["function"]["name"], "arguments": c["function"].get("arguments", "{}")}
                     for c in m.get("tool_calls") or [] if c.get("function")]
            if m.get("function_call"):
                calls.append({"name": m["function_call"]["name"], "arguments": m["function_call"].get("arguments", "{}")})
            turns.append({"role": "assistant", "blocks": await to_blocks(m.get("content")), "tool_calls": calls})
        elif role in ("tool", "function"):
            turns.append({"role": "tool", "blocks": await to_blocks(m.get("content")),
                          "tool_call_id": m.get("tool_call_id", ""), "name": m.get("name", "")})
        else:
            raise OAIError(400, f"Invalid role '{role}' in messages[{i}].", param=f"messages[{i}].role")
    if not turns:
        turns.append({"role": "user", "blocks": [{"type": "text", "text": "Hello."}]})
    return system, turns


def legacy_functions(body: dict) -> tuple[list, object]:
    tools, choice = body.get("tools") or [], body.get("tool_choice", "auto")
    if not tools and body.get("functions"):
        tools = [{"type": "function", "function": f} for f in body["functions"]]
        fc = body.get("function_call", "auto")
        choice = {"type": "function", "function": {"name": fc["name"]}} if isinstance(fc, dict) else fc
    return tools, choice


@router.post("/chat/completions")
async def chat_completions(request: Request):
    body = await read_json(request)
    model = await resolve_model(body.get("model"))
    system, turns = await chat_turns(body.get("messages"))
    tools, choice = legacy_functions(body)
    use_tools = bool(tools) and choice != "none"
    extra = [tools_prompt(tools, choice, body.get("parallel_tool_calls", True)) if use_tools else "",
             format_prompt(body.get("response_format"))]
    sys_prompt, blocks = await build_prompt(turns, system, extra)
    effort = body.get("reasoning_effort")
    rf = body.get("response_format") or {}
    js = rf.get("json_schema") or {}
    # Match the OpenAI API: enforce schemas natively only in strict mode.
    schema = js.get("schema") if rf.get("type") == "json_schema" and js.get("strict") else None
    stops = as_stop_list(body.get("stop"))
    n = int(body.get("n") or 1)
    json_mode = (body.get("response_format") or {}).get("type") in ("json_object", "json_schema")
    cid, created = new_id("chatcmpl-"), int(time.time())

    if body.get("stream"):
        if n != 1:
            raise OAIError(400, "n > 1 is not supported with stream=true.", param="n")
        include_usage = bool((body.get("stream_options") or {}).get("include_usage"))
        return StreamingResponse(stream_chat(cid, created, model, sys_prompt, blocks, effort, stops, use_tools, include_usage, schema),
                                 media_type="text/event-stream", headers={"Cache-Control": "no-cache"})

    async def one(index: int):
        f = Filter(stops, use_tools)
        out, info = [], {}
        async with aclosing(run_llm(sys_prompt, blocks, model, effort, schema)) as stream_:
            async for kind, val in stream_:
                if kind == "text":
                    out.append(f.feed(val))
                else:
                    info = val
        out.append(f.flush())
        content, calls = parse_tool_calls(f.raw) if use_tools else ("".join(out), [])
        if json_mode and not calls:
            content = clean_json(content)
        msg = {"role": "assistant", "content": content if (content or not calls) else None, "refusal": None,
               "annotations": []}
        if calls:
            msg["tool_calls"] = [{"id": c["id"], "type": "function",
                                  "function": {"name": c["name"], "arguments": c["arguments"]}} for c in calls]
        finish = "tool_calls" if calls else "stop"
        return {"index": index, "message": msg, "logprobs": None, "finish_reason": finish}, info

    results = await asyncio.gather(*(one(i) for i in range(max(1, min(n, 8)))))
    p = c = cached = reasoning = 0
    for _, info in results:
        a, b, ca, r = usage_counts(info.get("usage") or {})
        p, c, cached, reasoning = max(p, a), c + b, max(cached, ca), reasoning + r
    return {
        "id": cid, "object": "chat.completion", "created": created,
        "model": results[0][1].get("model", model), "system_fingerprint": None, "service_tier": "default",
        "choices": [r[0] for r in results],
        "usage": {"prompt_tokens": p, "completion_tokens": c, "total_tokens": p + c,
                  "prompt_tokens_details": {"cached_tokens": cached, "audio_tokens": 0},
                  "completion_tokens_details": {"reasoning_tokens": reasoning, "audio_tokens": 0,
                                                "accepted_prediction_tokens": 0, "rejected_prediction_tokens": 0}},
    }


async def stream_chat(cid, created, model, sys_prompt, blocks, effort, stops, use_tools, include_usage, schema=None):
    def chunk(delta: dict, finish=None, usage=None, choices=True):
        o = {"id": cid, "object": "chat.completion.chunk", "created": created, "model": model,
             "system_fingerprint": None,
             "choices": [{"index": 0, "delta": delta, "logprobs": None, "finish_reason": finish}] if choices else []}
        if include_usage:
            o["usage"] = usage
        return sse(o)

    yield chunk({"role": "assistant", "content": "", "refusal": None})
    f = Filter(stops, use_tools)
    info = {}
    try:
        async with aclosing(run_llm(sys_prompt, blocks, model, effort, schema)) as stream_:
            async for kind, val in stream_:
                if kind == "text":
                    if out := f.feed(val):
                        yield chunk({"content": out})
                    if f.stopped:
                        break  # Stop sequence: no need to wait for completion.
                else:
                    info = val
        if out := f.flush():
            yield chunk({"content": out})
        calls = parse_tool_calls(f.raw)[1] if use_tools else []
        for i, c in enumerate(calls):
            yield chunk({"tool_calls": [{"index": i, "id": c["id"], "type": "function",
                                         "function": {"name": c["name"], "arguments": ""}}]})
            yield chunk({"tool_calls": [{"index": i, "function": {"arguments": c["arguments"]}}]})
        yield chunk({}, "tool_calls" if calls else "stop")
        if include_usage:
            p, c, cached, reasoning = usage_counts(info.get("usage") or {})
            yield chunk({}, usage={"prompt_tokens": p, "completion_tokens": c, "total_tokens": p + c,
                                   "prompt_tokens_details": {"cached_tokens": cached},
                                   "completion_tokens_details": {"reasoning_tokens": reasoning}}, choices=False)
    except OAIError as e:
        yield sse(e.body())
    yield "data: [DONE]\n\n"


# ---------- /v1/completions (ancienne API) ----------

@router.post("/completions")
async def completions(request: Request):
    body = await read_json(request)
    model = await resolve_model(body.get("model"))
    prompt = body.get("prompt", "")
    if isinstance(prompt, list):
        prompt = prompt[0] if prompt and isinstance(prompt[0], str) else ""
    sys_prompt = ("You are a raw text completion engine. Output only the direct continuation of the user's text, "
                  "with no preamble, quotes or commentary.")
    blocks = [{"type": "text", "text": prompt or " "}]
    stops = as_stop_list(body.get("stop"))
    cid, created = new_id("cmpl-"), int(time.time())
    echo = prompt if body.get("echo") else ""

    if body.get("stream"):
        async def gen():
            def chunk(text, finish=None):
                return sse({"id": cid, "object": "text_completion", "created": created, "model": model,
                            "choices": [{"text": text, "index": 0, "logprobs": None, "finish_reason": finish}]})
            f = Filter(stops, False)
            try:
                if echo:
                    yield chunk(echo)
                async with aclosing(run_llm(sys_prompt, blocks, model, None)) as stream_:
                    async for kind, val in stream_:
                        if kind == "text" and (out := f.feed(val)):
                            yield chunk(out)
                        if f.stopped:
                            break
                if out := f.flush():
                    yield chunk(out)
                yield chunk("", "stop")
            except OAIError as e:
                yield sse(e.body())
            yield "data: [DONE]\n\n"
        return StreamingResponse(gen(), media_type="text/event-stream")

    f, parts, info = Filter(stops, False), [], {}
    async with aclosing(run_llm(sys_prompt, blocks, model, None)) as stream_:
        async for kind, val in stream_:
            if kind == "text":
                parts.append(f.feed(val))
            else:
                info = val
    parts.append(f.flush())
    p, c, _, _ = usage_counts(info.get("usage") or {})
    return {"id": cid, "object": "text_completion", "created": created, "model": info.get("model", model),
            "choices": [{"text": echo + "".join(parts), "index": 0, "logprobs": None, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": p, "completion_tokens": c, "total_tokens": p + c}}


# ---------- /v1/responses ----------

RESPONSES: "OrderedDict[str, dict]" = OrderedDict()  # In-memory previous_response_id storage.
RESPONSES_MAX_BYTES = int(os.environ.get("REMOTE_RESPONSES_MAX_MB", "64")) * 1024 * 1024


def _remember(rid: str, owner: str, turns: list[dict]):
    """Store responses for their owner within a memory budget including attachments."""
    size = sum(len(b.get("text") or "") + len((b.get("source") or {}).get("data") or "")
               for t in turns for b in t["blocks"])
    if not RESPONSES_MAX_BYTES or size > RESPONSES_MAX_BYTES:  # Zero disables storage.
        return
    RESPONSES[rid] = {"turns": turns, "owner": owner, "size": size}
    while len(RESPONSES) > 500 or sum(r["size"] for r in RESPONSES.values()) > RESPONSES_MAX_BYTES:
        RESPONSES.popitem(last=False)


async def responses_turns(inp) -> tuple[list[str], list[dict]]:
    if isinstance(inp, str):
        return [], [{"role": "user", "blocks": [{"type": "text", "text": inp}]}]
    if not isinstance(inp, list):
        raise OAIError(400, "'input' must be a string or an array.", param="input")
    system, turns = [], []
    for item in inp:
        t = item.get("type", "message")
        if t == "message":
            role = item.get("role", "user")
            blocks = await to_blocks(item.get("content"))
            if role in ("system", "developer"):
                system.append(text_of(blocks))
            elif role == "assistant":
                if turns and turns[-1]["role"] == "assistant" and not turns[-1]["tool_calls"]:
                    turns[-1]["blocks"] += blocks
                else:
                    turns.append({"role": "assistant", "blocks": blocks, "tool_calls": []})
            else:
                turns.append({"role": "user", "blocks": blocks})
        elif t == "function_call":
            call = {"name": item.get("name"), "arguments": item.get("arguments", "{}"), "id": item.get("call_id")}
            if turns and turns[-1]["role"] == "assistant":
                turns[-1]["tool_calls"].append(call)
            else:
                turns.append({"role": "assistant", "blocks": [], "tool_calls": [call]})
        elif t == "function_call_output":
            out = item.get("output")
            turns.append({"role": "tool", "tool_call_id": item.get("call_id", ""), "name": "",
                          "blocks": await to_blocks(out) if isinstance(out, list) else [{"type": "text", "text": str(out)}]})
        # Ignore reasoning, item_reference and similar items.
    return system, turns


@router.post("/responses")
async def responses(request: Request):
    body = await read_json(request)
    owner = request.state.owner
    model = await resolve_model(body.get("model"))
    system, turns = await responses_turns(body.get("input", ""))
    prev_id = body.get("previous_response_id")
    if prev_id:
        prev = RESPONSES.get(prev_id) if isinstance(prev_id, str) else None
        if not prev or prev["owner"] != owner:  # Hide responses belonging to other callers.
            raise OAIError(404, f"Previous response with id '{prev_id}' not found.", param="previous_response_id")
        turns = prev["turns"] + turns
    if body.get("instructions"):
        system.insert(0, body["instructions"])
    tools = [t for t in body.get("tools") or [] if t.get("type") == "function"]
    choice = body.get("tool_choice", "auto")
    use_tools = bool(tools) and choice != "none"
    fmt = (body.get("text") or {}).get("format")
    extra = [tools_prompt(tools, choice, body.get("parallel_tool_calls", True)) if use_tools else "",
             format_prompt(fmt if fmt and fmt.get("type") != "text" else None)]
    sys_prompt, blocks = await build_prompt(turns, system, extra)
    effort = (body.get("reasoning") or {}).get("effort")
    schema = fmt.get("schema") if fmt and fmt.get("type") == "json_schema" and fmt.get("strict") else None
    json_mode = bool(fmt and fmt.get("type") in ("json_object", "json_schema"))
    rid, created = new_id("resp_"), int(time.time())

    base = {"id": rid, "object": "response", "created_at": created, "status": "in_progress", "model": model,
            "output": [], "error": None, "incomplete_details": None, "instructions": body.get("instructions"),
            "max_output_tokens": body.get("max_output_tokens"), "parallel_tool_calls": body.get("parallel_tool_calls", True),
            "previous_response_id": prev_id, "reasoning": body.get("reasoning") or {"effort": None, "summary": None},
            "store": body.get("store", True), "temperature": body.get("temperature", 1.0),
            "text": body.get("text") or {"format": {"type": "text"}}, "tool_choice": choice,
            "tools": body.get("tools") or [], "top_p": body.get("top_p", 1.0), "truncation": "disabled",
            "usage": None, "user": body.get("user"), "metadata": body.get("metadata") or {}}

    def finalize(text: str, calls: list[dict], info: dict) -> dict:
        out = []
        if text or not calls:
            out.append({"type": "message", "id": new_id("msg_"), "status": "completed", "role": "assistant",
                        "content": [{"type": "output_text", "text": text, "annotations": [], "logprobs": []}]})
        for c in calls:
            out.append({"type": "function_call", "id": new_id("fc_"), "call_id": c["id"], "name": c["name"],
                        "arguments": c["arguments"], "status": "completed"})
        p, c_, cached, reasoning = usage_counts(info.get("usage") or {})
        resp = {**base, "status": "completed", "model": info.get("model", model), "output": out,
                "usage": {"input_tokens": p, "input_tokens_details": {"cached_tokens": cached},
                          "output_tokens": c_, "output_tokens_details": {"reasoning_tokens": reasoning},
                          "total_tokens": p + c_}}
        if resp["store"]:
            history = turns + [{"role": "assistant", "blocks": [{"type": "text", "text": text}] if text else [],
                                "tool_calls": [{"name": c["name"], "arguments": c["arguments"]} for c in calls]}]
            _remember(rid, owner, history)
        return resp

    if not body.get("stream"):
        f, info = Filter([], use_tools), {}
        parts = []
        async with aclosing(run_llm(sys_prompt, blocks, model, effort, schema)) as stream_:
            async for kind, val in stream_:
                if kind == "text":
                    parts.append(f.feed(val))
                else:
                    info = val
        parts.append(f.flush())
        text, calls = parse_tool_calls(f.raw) if use_tools else ("".join(parts), [])
        return finalize(clean_json(text) if json_mode and not calls else text, calls, info)

    async def gen():
        seq = 0

        def ev(type_: str, **data):
            nonlocal seq
            seq += 1
            return f"event: {type_}\ndata: {json.dumps({'type': type_, 'sequence_number': seq, **data}, ensure_ascii=False)}\n\n"

        yield ev("response.created", response=base)
        yield ev("response.in_progress", response=base)
        f, info, text = Filter([], use_tools), {}, ""
        msg_id, started = new_id("msg_"), False

        def start_msg():
            item = {"type": "message", "id": msg_id, "status": "in_progress", "role": "assistant", "content": []}
            part = {"type": "output_text", "text": "", "annotations": [], "logprobs": []}
            return (ev("response.output_item.added", output_index=0, item=item)
                    + ev("response.content_part.added", item_id=msg_id, output_index=0, content_index=0, part=part))

        try:
            async with aclosing(run_llm(sys_prompt, blocks, model, effort, schema)) as stream_:
                async for kind, val in stream_:
                    if kind == "done":
                        info = val
                        continue
                    if out := f.feed(val):
                        if not started:
                            started = True
                            yield start_msg()
                        text += out
                        yield ev("response.output_text.delta", item_id=msg_id, output_index=0, content_index=0,
                                 delta=out, logprobs=[])
            if out := f.flush():
                if not started:
                    started = True
                    yield start_msg()
                text += out
                yield ev("response.output_text.delta", item_id=msg_id, output_index=0, content_index=0, delta=out, logprobs=[])
            calls = parse_tool_calls(f.raw)[1] if use_tools else []
            if calls:
                text = text.strip()
            if not started and not calls:
                started = True
                yield start_msg()
            final = finalize(text, calls, info)
            idx = 0
            if started:
                part = {"type": "output_text", "text": text, "annotations": [], "logprobs": []}
                yield ev("response.output_text.done", item_id=msg_id, output_index=0, content_index=0, text=text, logprobs=[])
                yield ev("response.content_part.done", item_id=msg_id, output_index=0, content_index=0, part=part)
                msg_item = {"type": "message", "id": msg_id, "status": "completed", "role": "assistant", "content": [part]}
                yield ev("response.output_item.done", output_index=0, item=msg_item)
                if final["output"] and final["output"][0]["type"] == "message":
                    final["output"][0] = msg_item
                idx = 1
            for item in (o for o in final["output"] if o["type"] == "function_call"):
                pending = {**item, "arguments": "", "status": "in_progress"}
                yield ev("response.output_item.added", output_index=idx, item=pending)
                yield ev("response.function_call_arguments.delta", item_id=item["id"], output_index=idx, delta=item["arguments"])
                yield ev("response.function_call_arguments.done", item_id=item["id"], output_index=idx, arguments=item["arguments"])
                yield ev("response.output_item.done", output_index=idx, item=item)
                idx += 1
            yield ev("response.completed", response=final)
        except OAIError as e:
            yield ev("response.failed", response={**base, "status": "failed",
                                                  "error": {"code": e.code or e.type, "message": e.message}})

    return StreamingResponse(gen(), media_type="text/event-stream", headers={"Cache-Control": "no-cache"})


@router.get("/responses/{rid}")
async def get_response(rid: str, request: Request):
    check_auth(request)
    raise OAIError(404, "Retrieving stored responses is not supported; use previous_response_id.", param="response_id")


# ---------- unsupported ----------

@router.api_route("/{path:path}", methods=["GET", "POST", "PUT", "DELETE", "PATCH"])
async def unsupported(path: str, request: Request):
    check_auth(request)
    raise OAIError(404, f"The endpoint /v1/{path} is not supported by CustomRemote (Claude has no {path.split('/')[0]} API here).",
                   code="unknown_url")
