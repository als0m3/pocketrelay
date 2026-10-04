"""Session management: one claude -p stream-json process per session.

The CLI uses existing host authentication from the subscription
(claude login). Communication uses JSON lines over stdin/stdout:
- stdin  : user messages + control_request (interrupt, set_model…) +
           control_response (permission request replies)
- stdout : system/assistant/user/result/stream_event + control_request
           (can_use_tool) + control_response
"""

import asyncio
import json
import os
import time
import uuid
from collections import deque
from pathlib import Path

from . import accounts, history
from .config import CLAUDE_BIN, DATA, KEEP_API_KEY, clean_env

SESSIONS_FILE = DATA / "sessions.json"
EVENTS_DIR = DATA / "events"
LIMITS_FILE = DATA / "limits.json"

PERMISSION_MODES = ["manual", "acceptEdits", "auto", "plan", "dontAsk", "bypassPermissions"]
EFFORTS = ["", "low", "medium", "high", "xhigh", "max"]

# Metadata fields editable through the API.
EDITABLE = {"name", "model", "permission_mode", "effort", "append_system_prompt", "pinned"}


def child_env(token: str | None = None, full: bool = False) -> dict:
    """Full local sessions inherit the host environment except REMOTE_* variables."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("REMOTE_")} if full else clean_env()
    drop = ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT"]
    if not KEEP_API_KEY:
        drop += ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]
    for k in drop:
        env.pop(k, None)
    # Use the selected account token, falling back to host login.
    if token:
        env["CLAUDE_CODE_OAUTH_TOKEN"] = token
    env.setdefault("DISABLE_AUTOUPDATER", "1")
    return env


class Session:
    def __init__(self, meta: dict, manager: "Manager"):
        self.meta = meta
        self.manager = manager
        self.proc: asyncio.subprocess.Process | None = None
        self.events: list[dict] | None = None  # Loaded on demand.
        self.subs: set[asyncio.Queue] = set()
        self.pending: dict[str, dict] = {}  # request_id -> can_use_tool request
        self.ctrl: dict[str, asyncio.Future] = {}
        self.stderr_tail: deque[str] = deque(maxlen=40)
        self.proc_cost = 0.0
        self.spawn_lock = asyncio.Lock()
        self.meta.setdefault("auto_allow", [])
        # No sessions remain alive after a server restart.
        if self.meta.get("status") in ("running", "waiting", "starting"):
            self.meta["status"] = "idle"

    # ---------- event persistence ----------

    @property
    def id(self) -> str:
        return self.meta["id"]

    @property
    def events_path(self) -> Path:
        return EVENTS_DIR / f"{self.id}.jsonl"

    def load_events(self) -> list[dict]:
        if self.events is None:
            self.events = []
            if self.events_path.exists():
                for line in self.events_path.read_text(encoding="utf-8").splitlines():
                    try:
                        self.events.append(json.loads(line))
                    except json.JSONDecodeError:
                        pass
        return self.events

    def emit(self, ev: dict, persist: bool = True):
        """Broadcast an event to SSE subscribers and optionally persist it."""
        if persist:
            events = self.load_events()
            ev["seq"] = (events[-1]["seq"] + 1) if events else 1
            ev.setdefault("ts", time.time())
            events.append(ev)
            EVENTS_DIR.mkdir(parents=True, exist_ok=True)
            with self.events_path.open("a", encoding="utf-8") as fh:
                fh.write(json.dumps(ev, ensure_ascii=False) + "\n")
        for q in list(self.subs):
            q.put_nowait(ev)

    def set_status(self, status: str):
        if self.meta.get("status") != status:
            self.meta["status"] = status
            self.emit({"type": "remote", "event": "status", "status": status}, persist=False)
            self.touch()

    def touch(self):
        self.meta["updated"] = time.time()
        self.manager.save()
        self.manager.broadcast({"type": "session_update", "session": self.summary()})

    def summary(self) -> dict:
        s = {k: v for k, v in self.meta.items()}
        s["alive"] = self.alive
        s["pending"] = list(self.pending.keys())
        return s

    @property
    def alive(self) -> bool:
        return self.proc is not None and self.proc.returncode is None

    # ---------- process ----------

    def _args(self) -> list[str]:
        m = self.meta
        args = [
            CLAUDE_BIN, "-p",
            "--input-format", "stream-json",
            "--output-format", "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompts", "host",
            "--permission-mode", m.get("permission_mode") or "manual",
        ]
        if m.get("permission_mode") == "bypassPermissions":
            args.append("--allow-dangerously-skip-permissions")
        if m.get("model"):
            args += ["--model", m["model"]]
        if m.get("effort"):
            args += ["--effort", m["effort"]]
        if m.get("append_system_prompt"):
            args += ["--append-system-prompt", m["append_system_prompt"]]
        for d in m.get("add_dirs") or []:
            args += ["--add-dir", d]
        if m.get("started"):
            args += ["--resume", m["claude_session_id"]]
            if m.pop("fork_on_resume", False):
                args.append("--fork-session")
        else:
            args += ["--session-id", m["claude_session_id"]]
        return args

    async def ensure_proc(self):
        async with self.spawn_lock:
            if self.alive:
                return
            self.set_status("starting")
            cwd = self.meta["cwd"]
            if not os.path.isdir(cwd):
                self.set_status("error")
                raise RuntimeError(f"Directory not found : {cwd}")
            self.proc_cost = 0.0
            self.stderr_tail.clear()
            self.proc = await asyncio.create_subprocess_exec(
                *self._args(),
                cwd=cwd,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                env=child_env(accounts.session_token(), full=True),
                limit=64 * 1024 * 1024,
            )
            proc = self.proc
            asyncio.create_task(self._read_stdout(proc))
            asyncio.create_task(self._read_stderr(proc))
            self.meta["started"] = True
            self.set_status("idle")
            asyncio.create_task(self._initialize())

    async def _initialize(self):
        resp = await self.control({"subtype": "initialize"}, timeout=60)
        if isinstance(resp, dict):
            # Slash commands and available models support UI autocomplete.
            self.meta["commands"] = resp.get("commands") or self.meta.get("commands") or []
            self.meta["models"] = resp.get("models") or self.meta.get("models") or []
            self.touch()

    async def _write(self, obj: dict):
        if not self.alive or self.proc.stdin is None or self.proc.stdin.is_closing():
            raise RuntimeError("Claude process has not started")
        self.proc.stdin.write((json.dumps(obj, ensure_ascii=False) + "\n").encode())
        await self.proc.stdin.drain()

    async def control(self, request: dict, timeout: float = 30):
        rid = "req_" + uuid.uuid4().hex[:16]
        fut = asyncio.get_running_loop().create_future()
        self.ctrl[rid] = fut
        try:
            await self._write({"type": "control_request", "request_id": rid, "request": request})
            return await asyncio.wait_for(fut, timeout)
        except (asyncio.TimeoutError, RuntimeError):
            return None
        finally:
            self.ctrl.pop(rid, None)

    async def _read_stderr(self, proc):
        async for raw in proc.stderr:
            line = raw.decode(errors="replace").rstrip()
            if line:
                self.stderr_tail.append(line)

    async def _read_stdout(self, proc):
        try:
            async for raw in proc.stdout:
                line = raw.decode(errors="replace").strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError:
                    self.emit({"type": "remote", "event": "raw", "text": line[:2000]})
                    continue
                try:
                    await self._handle(obj)
                except Exception as e:  # Never terminate the read loop.
                    self.emit({"type": "remote", "event": "error", "text": f"handler: {e!r}"})
        finally:
            code = await proc.wait()
            if proc is self.proc:
                self.proc = None
                for rid in list(self.pending):
                    self.pending.pop(rid)
                    self.emit({"type": "permission_resolved", "request_id": rid, "behavior": "cancelled"})
                ev = {"type": "remote", "event": "exited", "code": code}
                if code not in (0, None, -15):
                    ev["stderr"] = "\n".join(self.stderr_tail)
                self.emit(ev)
                self.set_status("error" if code not in (0, None, -15) else "idle")

    async def _handle(self, o: dict):
        t = o.get("type")

        if t == "control_request":
            await self._on_control_request(o)
            return
        if t == "control_response":
            resp = o.get("response") or {}
            fut = self.ctrl.get(resp.get("request_id"))
            if fut and not fut.done():
                fut.set_result(resp.get("response") if resp.get("subtype") == "success" else None)
            return
        if t == "stream_event":
            self.emit(o, persist=False)
            return
        if t == "keep_alive":
            return
        if t == "rate_limit_event":
            self.manager.set_limits(o.get("rate_limit_info") or {})
            return

        if t == "system" and o.get("subtype") == "init":
            if o.get("session_id"):
                self.meta["claude_session_id"] = o["session_id"]
            self.meta["active_model"] = o.get("model")
            self.meta["tools"] = o.get("tools") or []
            self.touch()
        elif t == "assistant":
            usage = (o.get("message") or {}).get("usage") or {}
            ctx = sum(usage.get(k) or 0 for k in ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"))
            if ctx:
                self.meta["context_tokens"] = ctx
            for block in (o.get("message") or {}).get("content") or []:
                if block.get("type") == "text" and block.get("text", "").strip():
                    self.meta["preview"] = block["text"].strip()[:140]
        elif t == "result":
            total = o.get("total_cost_usd") or 0.0
            self.meta["cost_usd"] = round(self.meta.get("cost_usd", 0.0) + max(0.0, total - self.proc_cost), 6)
            self.proc_cost = total
            self.meta["turns"] = self.meta.get("turns", 0) + 1
            self.meta["last_result"] = {
                "is_error": o.get("is_error"),
                "subtype": o.get("subtype"),
                "duration_ms": o.get("duration_ms"),
            }
            self.emit(o)
            self.set_status("waiting" if self.pending else "idle")
            self.touch()
            return

        self.emit(o)

    async def _on_control_request(self, o: dict):
        rid = o.get("request_id")
        req = o.get("request") or {}
        if req.get("subtype") != "can_use_tool":
            await self._write({"type": "control_response", "response": {
                "subtype": "error", "request_id": rid,
                "error": f"Subtype unsupported by CustomRemote: {req.get('subtype')}",
            }})
            return
        tool = req.get("tool_name", "")
        if tool in self.meta["auto_allow"]:
            await self._respond_permission(rid, req, {"behavior": "allow", "updatedInput": req.get("input") or {}})
            self.emit({"type": "permission_auto", "tool_name": tool, "input": req.get("input")})
            return
        self.pending[rid] = req
        self.emit({"type": "permission_request", "request_id": rid, "tool_name": tool,
                   "input": req.get("input"), "tool_use_id": req.get("tool_use_id"),
                   "suggestions": req.get("permission_suggestions"),
                   "decision_reason": req.get("decision_reason"),
                   "blocked_path": req.get("blocked_path")})
        self.set_status("waiting")

    async def _respond_permission(self, rid: str, req: dict, decision: dict):
        await self._write({"type": "control_response", "response": {
            "subtype": "success", "request_id": rid, "response": decision,
        }})

    # ---------- actions API ----------

    async def send(self, content):
        await self.ensure_proc()
        self.emit({"type": "user", "message": {"role": "user", "content": content}, "local": True})
        if isinstance(content, str):
            self.meta["preview"] = content[:140]
        await self._write({
            "type": "user",
            "message": {"role": "user", "content": content},
            "parent_tool_use_id": None,
            "session_id": self.meta["claude_session_id"],
        })
        self.set_status("running")

    async def decide(self, rid: str, behavior: str, message: str = "", always: bool = False,
                     updated_input: dict | None = None):
        req = self.pending.pop(rid, None)
        if req is None:
            raise KeyError(rid)
        if behavior == "allow":
            decision = {"behavior": "allow", "updatedInput": updated_input or req.get("input") or {}}
            if always and req.get("tool_name") not in self.meta["auto_allow"]:
                self.meta["auto_allow"].append(req["tool_name"])
        else:
            decision = {"behavior": "deny", "message": message or "Denied from CustomRemote"}
        await self._respond_permission(rid, req, decision)
        self.emit({"type": "permission_resolved", "request_id": rid, "behavior": behavior,
                   "always": always, "message": message})
        self.set_status("waiting" if self.pending else "running")

    async def interrupt(self):
        for rid in list(self.pending):
            try:
                await self.decide(rid, "deny", "Interrupted by the user")
            except Exception:
                pass
        if self.alive:
            await self.control({"subtype": "interrupt"}, timeout=10)
        self.emit({"type": "remote", "event": "interrupted"})

    async def update(self, patch: dict):
        changed = {k: v for k, v in patch.items() if k in EDITABLE and self.meta.get(k) != v}
        self.meta.update(changed)
        if self.alive:
            if "permission_mode" in changed:
                await self.control({"subtype": "set_permission_mode", "mode": changed["permission_mode"]})
            if "model" in changed:
                await self.control({"subtype": "set_model", "model": changed["model"] or None})
            if {"effort", "append_system_prompt"} & changed.keys():
                await self.stop()  # Apply at the next startup with --resume.
        if changed:
            self.emit({"type": "remote", "event": "config", "changes": changed})
        self.touch()

    async def stop(self):
        proc = self.proc
        if proc and proc.returncode is None:
            proc.terminate()
            try:
                await asyncio.wait_for(proc.wait(), 5)
            except asyncio.TimeoutError:
                proc.kill()


class Manager:
    def __init__(self):
        self.sessions: dict[str, Session] = {}
        self.subs: set[asyncio.Queue] = set()
        DATA.mkdir(parents=True, exist_ok=True)
        self.limits = json.loads(LIMITS_FILE.read_text()) if LIMITS_FILE.exists() else {}
        if SESSIONS_FILE.exists():
            for meta in json.loads(SESSIONS_FILE.read_text()):
                self.sessions[meta["id"]] = Session(meta, self)

    def save(self):
        tmp = SESSIONS_FILE.with_suffix(".tmp")
        tmp.write_text(json.dumps([s.meta for s in self.sessions.values()], ensure_ascii=False, indent=1))
        tmp.replace(SESSIONS_FILE)

    def set_limits(self, info: dict):
        """Subscription quotas reported by the CLI (5-hour / 7-day windows)."""
        info["seen_at"] = time.time()
        self.limits = info
        LIMITS_FILE.write_text(json.dumps(info))
        self.broadcast({"type": "limits", "limits": info})

    def broadcast(self, ev: dict):
        for q in list(self.subs):
            q.put_nowait(ev)

    def get(self, sid: str) -> Session:
        return self.sessions[sid]

    def list_all(self) -> list[dict]:
        return sorted((s.summary() for s in self.sessions.values()),
                      key=lambda m: (not m.get("pinned"), -m.get("updated", 0)))

    def create(self, cwd: str, name: str = "", model: str = "", permission_mode: str = "manual",
               effort: str = "", append_system_prompt: str = "", add_dirs: list[str] | None = None,
               resume: str = "", fork: bool = False) -> Session:
        cwd = os.path.abspath(os.path.expanduser(cwd))
        if not os.path.isdir(cwd):
            raise ValueError(f"Directory not found : {cwd}")
        if permission_mode not in PERMISSION_MODES:
            raise ValueError(f"Unknown mode : {permission_mode}")
        if resume and not fork:
            for s in self.sessions.values():
                if s.meta.get("claude_session_id") == resume:
                    return s
        now = time.time()
        meta = {
            "id": uuid.uuid4().hex[:12],
            "name": name or os.path.basename(cwd) or cwd,
            "cwd": cwd,
            "model": model,
            "permission_mode": permission_mode,
            "effort": effort,
            "append_system_prompt": append_system_prompt,
            "add_dirs": add_dirs or [],
            "claude_session_id": resume or str(uuid.uuid4()),
            "started": bool(resume),
            "fork_on_resume": bool(resume and fork),
            "created": now,
            "updated": now,
            "status": "idle",
            "cost_usd": 0.0,
            "turns": 0,
        }
        s = Session(meta, self)
        self.sessions[meta["id"]] = s
        if resume:
            for ev in history.import_events(resume):
                s.emit(ev)
            s.emit({"type": "remote", "event": "resumed", "claude_session_id": resume, "fork": fork})
        s.touch()
        return s

    async def delete(self, sid: str):
        s = self.sessions.pop(sid)
        await s.stop()
        s.events_path.unlink(missing_ok=True)
        self.save()
        self.broadcast({"type": "session_deleted", "id": sid})

    async def shutdown(self):
        await asyncio.gather(*(s.stop() for s in self.sessions.values()), return_exceptions=True)
