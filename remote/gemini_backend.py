"""Google AI Pro / Ultra subscription backend using headless Gemini CLI.

Each request launches an ephemeral gemini --output-format stream-json process,
without tools, with replaced system instructions (GEMINI_SYSTEM_MD) and
account state isolated in GEMINI_CLI_HOME, under its .gemini subdirectory.

The CLI does not support remotely controlled device-code login. OAuth
credentials (oauth_creds.json from Sign in with Google) are imported
through the console, like claude setup-token credentials.
"""

import asyncio
import json
import os
import shutil
import tempfile

from . import accounts, pdf
from .config import DATA

GEMINI_BIN = os.environ.get("GEMINI_BIN") or shutil.which("gemini") or ""
ENABLED = bool(GEMINI_BIN) and os.environ.get("REMOTE_ENABLE_GEMINI", "1") == "1"
WORKDIR = DATA / "gemini-cwd"
TIMEOUT = float(os.environ.get("REMOTE_GEMINI_TIMEOUT", "600"))

# Models advertised by /v1/models; other gemini-* names remain accepted.
MODELS = {
    "gemini-3-pro-preview": "Gemini 3 Pro",
    "gemini-3-flash-preview": "Gemini 3 Flash",
    "gemini-2.5-pro": "Gemini 2.5 Pro",
    "gemini-2.5-flash": "Gemini 2.5 Flash",
}
if env_models := os.environ.get("REMOTE_GEMINI_MODELS"):
    MODELS = {m.strip(): m.strip() for m in env_models.split(",") if m.strip()}
DEFAULT_MODEL = os.environ.get("REMOTE_GEMINI_MODEL") or next(iter(MODELS), "gemini-3-pro-preview")
FAST_MODEL = os.environ.get("REMOTE_GEMINI_FAST_MODEL") or next(
    (m for m in MODELS if "flash" in m), DEFAULT_MODEL)

# The model should only answer: remove every built-in tool.
DISABLED_TOOLS = ["run_shell_command", "glob", "grep_search", "search_file_content", "list_directory",
                  "read_file", "read_many_files", "replace", "write_file", "ask_user", "write_todos",
                  "google_web_search", "web_fetch", "save_memory", "activate_skill", "get_internal_docs",
                  "enter_plan_mode", "exit_plan_mode", "list_mcp_resources", "read_mcp_resource"]

SETTINGS = {
    "security": {"auth": {"selectedType": "oauth-personal"}},   # Google subscription, never API-key billing.
    "privacy": {"usageStatisticsEnabled": False},
    "general": {"checkForUpdates": False},
    "tools": {"core": [], "exclude": DISABLED_TOOLS},
    "mcpServers": {},
}


class GeminiError(Exception):
    def __init__(self, message: str, kind: str = "api_error"):
        super().__init__(message)
        self.message, self.kind = message, kind


def prepare_home(acc: dict) -> str | None:
    """Create account state and settings if needed; None uses host login."""
    home = accounts.gemini_home(acc)
    if not home:
        return None
    d = os.path.join(home, ".gemini")
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, "settings.json")
    payload = json.dumps(SETTINGS, ensure_ascii=False, indent=1)
    if not os.path.exists(path) or open(path).read() != payload:
        with open(path, "w") as f:
            f.write(payload)
    return home


def child_env(home: str | None, system_file: str) -> dict:
    env = dict(os.environ)
    # Remove API keys to use the subscription instead of billed API access.
    for k in ("GEMINI_API_KEY", "GOOGLE_API_KEY", "GOOGLE_GENAI_USE_VERTEXAI"):
        env.pop(k, None)
    if home:
        env["GEMINI_CLI_HOME"] = home
    env["GEMINI_SYSTEM_MD"] = system_file      # Replace CLI system instructions completely.
    env["NO_BROWSER"] = "1"                    # Never open a server-side browser.
    env["NO_COLOR"] = "1"
    env["TERM"] = "dumb"
    return env


def to_prompt(blocks: list[dict]) -> str:
    """Convert Anthropic blocks into text; flatten PDFs and reject images."""
    parts = []
    for b in blocks:
        if b["type"] == "text":
            parts.append(b["text"])
        elif b["type"] == "document":
            # Flatten PDFs into text; scanned pages have no extractable content.
            try:
                pieces = pdf.convert(b["source"]["data"], b.get("title"))
            except pdf.PdfError as e:
                raise GeminiError(str(e), "invalid_request_error")
            parts += [p["text"] for p in pieces if p["type"] == "text"]
            if dropped := sum(1 for p in pieces if p["type"] == "image"):
                parts.append(f"[{dropped} page(s) without extractable text were omitted: "
                             "Gemini models in this gateway do not accept images.]")
        elif b["type"] == "image":
            raise GeminiError("Gemini models in this gateway do not accept images: "
                              "the CLI only accepts attachments as local files.",
                              "invalid_request_error")
    return "\n\n".join(p for p in parts if p).strip() or "(empty)"


def _kind(message: str) -> str:
    low = message.lower()
    if any(k in low for k in ("resource_exhausted", "rate limit", "ratelimit", "quota", "429", "too many requests")):
        return "rate_limit_error"
    if any(k in low for k in ("unauthenticated", "401", "403", "permission_denied", "credentials", "oauth",
                              "sign in", "not authenticated", "login")):
        return "authentication_error"
    if "invalid" in low and "argument" in low:
        return "invalid_request_error"
    return "api_error"


async def run_gemini(system: str, blocks: list[dict], model: str, acc: dict):
    """Yield text/done events using the same interface as run_claude."""
    if not ENABLED:
        raise GeminiError("Gemini CLI is not installed on this server.", "api_error")
    prompt = to_prompt(blocks)
    home = await asyncio.to_thread(prepare_home, acc)
    WORKDIR.mkdir(parents=True, exist_ok=True)
    tmp = tempfile.mkdtemp(prefix="gemini-", dir=WORKDIR)
    system_file = os.path.join(tmp, "system.md")
    with open(system_file, "w") as f:
        f.write(system or "You are a helpful assistant.")
    args = [GEMINI_BIN, "--output-format", "stream-json", "--model", model, "--approval-mode", "default"]
    proc = await asyncio.create_subprocess_exec(
        *args, cwd=tmp, env=child_env(home, system_file), limit=64 * 1024 * 1024,
        stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    proc.stdin.write(prompt.encode())
    await proc.stdin.drain()
    proc.stdin.close()
    info = {"model": model, "usage": {}}
    warnings: list[str] = []
    done = False
    try:
        while True:
            try:
                raw = await asyncio.wait_for(proc.stdout.readline(), TIMEOUT)
            except asyncio.TimeoutError:
                raise GeminiError("Gemini did not respond before the timeout.", "api_error")
            if not raw:
                break
            try:
                ev = json.loads(raw)
            except json.JSONDecodeError:
                continue
            t = ev.get("type")
            if t == "message" and ev.get("role") == "assistant":
                if ev.get("content"):
                    yield "text", ev["content"]
            elif t == "init":
                info["model"] = ev.get("model") or model
            elif t == "error":
                msg = ev.get("message") or "Error Gemini"
                if ev.get("severity") == "error":
                    raise GeminiError(msg, _kind(msg))
                warnings.append(msg)
            elif t == "result":
                if ev.get("status") == "error":
                    msg = (ev.get("error") or {}).get("message") or "; ".join(warnings) or "Gemini request failed"
                    raise GeminiError(msg, _kind(msg))
                stats = ev.get("stats") or {}
                cached = stats.get("cached") or 0
                info["usage"] = {"input_tokens": max(0, (stats.get("input_tokens") or 0) - cached),
                                 "cache_read_input_tokens": cached,
                                 "output_tokens": stats.get("output_tokens") or 0}
                done = True
                break
        if not done:
            err = (await proc.stderr.read()).decode(errors="replace")[-1500:].strip()
            msg = err or "; ".join(warnings) or "Gemini CLI exited without a result."
            raise GeminiError(msg, _kind(msg))
        yield "done", info
    finally:
        if proc.returncode is None:
            proc.kill()
        await proc.wait()
        shutil.rmtree(tmp, ignore_errors=True)
