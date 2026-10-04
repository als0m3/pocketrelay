"""Google AI subscription backend using headless Antigravity CLI (agy).

Each request launches an ephemeral antigravity -p stream-json process.
The CLI stores state under $HOME/.gemini/, so accounts receive isolated
HOME directories, like Codex accounts receive isolated CODEX_HOME directories.

Google login requires a controlling terminal (/dev/tty), so the console
cannot drive it. It displays the command to run on the server
through oc exec -it. The former Gemini CLI, replaced for individual
accounts in June 2026, is no longer used.
"""

import asyncio
import json
import os
import re
import shutil
import sqlite3
import tempfile
from pathlib import Path

from . import accounts, pdf
from .config import DATA, clean_env

BIN = os.environ.get("ANTIGRAVITY_BIN") or shutil.which("antigravity") or shutil.which("agy") or ""
ENABLED = bool(BIN) and os.environ.get("REMOTE_ENABLE_ANTIGRAVITY", "1") == "1"
WORKDIR = DATA / "antigravity-cwd"
TIMEOUT = float(os.environ.get("REMOTE_ANTIGRAVITY_TIMEOUT", "600"))

# Subscription model slugs; run antigravity models after login for the exact list.
MODELS = {
    "gemini-3-pro": "Gemini 3 Pro",
    "gemini-3-flash": "Gemini 3 Flash",
    "gemini-2.5-flash": "Gemini 2.5 Flash",
}
if _env := os.environ.get("REMOTE_ANTIGRAVITY_MODELS"):
    MODELS = {m.strip(): m.strip() for m in _env.split(",") if m.strip()}
DEFAULT_MODEL = os.environ.get("REMOTE_ANTIGRAVITY_MODEL") or next(iter(MODELS), "gemini-3-pro")
FAST_MODEL = os.environ.get("REMOTE_ANTIGRAVITY_FAST_MODEL") or next(
    (m for m in MODELS if "flash" in m), DEFAULT_MODEL)
EFFORTS = ("low", "medium", "high")


class AntigravityError(Exception):
    def __init__(self, message: str, kind: str = "api_error"):
        super().__init__(message)
        self.message, self.kind = message, kind


def home(acc: dict) -> str:
    """Account HOME, containing CLI state and Google sessions in .gemini/."""
    return accounts.cli_home(acc, "antigravity")


def login_command(acc: dict) -> str:
    """Server-terminal command for connecting this account."""
    return f"env HOME={home(acc)} {BIN or 'antigravity'}"


def child_env(h: str) -> dict:
    # Allowlist the environment, excluding gateway secrets and provider API keys
    # to use subscription access rather than billed API access.
    env = clean_env()
    if h:
        os.makedirs(h, exist_ok=True)
        env["HOME"] = h
    env["NO_COLOR"] = "1"
    env["TERM"] = "dumb"
    return env


def purge(h: str, cid: str):
    """Delete CLI-retained conversation data because there is no nonpersistent mode.

    Remove conversation files, transcript/annotation data, its summary database
    row (including prompt-derived title and preview) and regenerated summary caches.
    """
    if not re.fullmatch(r"[0-9a-f-]{36}", cid or ""):
        return
    base = Path(h) / ".gemini" / "antigravity-cli"
    for p in base.glob(f"*/{cid}*"):
        shutil.rmtree(p, ignore_errors=True) if p.is_dir() else p.unlink(missing_ok=True)
    for name in ("jetbox_summaries_proto.pb", "cache/last_conversations.json", "cache/conversation_metadata.json"):
        (base / name).unlink(missing_ok=True)
    db = base / "conversation_summaries.db"
    if db.exists():
        try:
            with sqlite3.connect(db, timeout=2) as con:
                con.execute("DELETE FROM conversation_summaries WHERE conversation_id = ?", (cid,))
        except sqlite3.Error:
            pass


def to_prompt(system: str, blocks: list[dict]) -> str:
    """Prepend system instructions because the CLI cannot replace its system prompt."""
    parts = []
    for b in blocks:
        if b["type"] == "text":
            parts.append(b["text"])
        elif b["type"] == "document":
            try:
                pieces = pdf.convert(b["source"]["data"], b.get("title"))
            except pdf.PdfError as e:
                raise AntigravityError(str(e), "invalid_request_error")
            parts += [p["text"] for p in pieces if p["type"] == "text"]
            if dropped := sum(1 for p in pieces if p["type"] == "image"):
                parts.append(f"[{dropped} page(s) without extractable text were omitted: "
                             "this provider does not accept images.]")
        elif b["type"] == "image":
            raise AntigravityError("Antigravity models in this gateway do not accept images: "
                                   "the CLI only accepts attachments as local files.",
                                   "invalid_request_error")
    user = "\n\n".join(p for p in parts if p).strip() or "(empty)"
    return f"<system_instructions>\n{system}\n</system_instructions>\n\n{user}" if system else user


def _kind(message: str) -> str:
    low = message.lower()
    if any(k in low for k in ("resource_exhausted", "rate limit", "ratelimit", "quota", "429", "too many requests")):
        return "rate_limit_error"
    if any(k in low for k in ("authentication", "unauthenticated", "sign in", "log in", "401", "403",
                              "permission_denied", "credential")):
        return "authentication_error"
    if any(k in low for k in ("unknown model", "invalid schema", "invalid argument")):
        return "invalid_request_error"
    return "api_error"


async def run_antigravity(system: str, blocks: list[dict], model: str, effort: str | None, acc: dict,
                          schema: dict | None = None):
    """Yield text/done events using the same interface as run_claude."""
    if not ENABLED:
        raise AntigravityError("Antigravity CLI is not installed on this server.", "api_error")
    prompt = to_prompt(system, blocks)
    WORKDIR.mkdir(parents=True, exist_ok=True)
    tmp = tempfile.mkdtemp(prefix="agy-", dir=WORKDIR)
    args = [BIN, "-p", prompt, "--output-format", "stream-json", "--model", model,
            "--disable-slash-commands",      # Client prompts are not CLI commands.
            "--log-file", os.path.join(tmp, "cli.log")]   # Discard logs with the working directory.
    if effort in EFFORTS:
        args += ["--effort", effort]
    if schema:
        args += ["--json-schema", json.dumps(schema)]
    h = home(acc)
    proc = await asyncio.create_subprocess_exec(
        *args, cwd=tmp, env=child_env(h), limit=64 * 1024 * 1024,
        stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    info = {"model": model, "usage": {}}
    done = streamed = False
    cid = ""
    try:
        while True:
            try:
                raw = await asyncio.wait_for(proc.stdout.readline(), TIMEOUT)
            except asyncio.TimeoutError:
                raise AntigravityError("Antigravity did not respond before the timeout.", "api_error")
            if not raw:
                break
            try:
                ev = json.loads(raw)
            except json.JSONDecodeError:
                continue
            name = ev.get("event")
            cid = cid or ev.get("conversation_id") or (ev.get("result") or {}).get("conversation_id") or ""
            # CLI versions expose details either flat or nested under the event name.
            if name == "step_update":
                if delta := (ev.get("step_update") or ev).get("text_delta"):
                    streamed = True
                    yield "text", delta
            elif name == "init":
                info["model"] = (ev.get("init") or ev).get("model") or model
            elif name == "result":
                res = ev.get("result") or ev
                status = (res.get("status") or "").upper()
                if status != "SUCCESS":
                    msg = res.get("error") or f"Antigravity request {status or 'failed'}"
                    raise AntigravityError(str(msg)[:500], _kind(str(msg)))
                if not streamed and res.get("response"):
                    yield "text", res["response"]
                u = res.get("usage") or {}
                cached = u.get("cache_read_tokens") or 0
                info["usage"] = {"input_tokens": max(0, (u.get("input_tokens") or 0) - cached),
                                 "cache_read_input_tokens": cached,
                                 "output_tokens": u.get("output_tokens") or 0,
                                 "output_tokens_details": {"thinking_tokens": u.get("thinking_tokens") or 0}}
                done = True
                break
        if not done:
            err = (await proc.stderr.read()).decode(errors="replace").strip()
            head = next((l.strip() for l in err.splitlines() if l.strip()), "")
            code = await proc.wait()
            msg = head or f"Antigravity CLI exited without a result (code {code})."
            raise AntigravityError(msg[:500], _kind(err or msg))
        yield "done", info
    finally:
        if proc.returncode is None:
            proc.kill()
        await proc.wait()
        shutil.rmtree(tmp, ignore_errors=True)
        purge(h, cid)
