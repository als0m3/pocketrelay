"""Read CLI transcripts (~/.claude/projects/*/*.jsonl) for listing and resuming."""

import json
from pathlib import Path

from .config import CLAUDE_PROJECTS


def _text_of(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(b.get("text", "") for b in content if isinstance(b, dict) and b.get("type") == "text")
    return ""


def _is_human_text(text: str) -> bool:
    t = text.strip()
    return bool(t) and not t.startswith("<") and not t.startswith("Caveat:")


def list_transcripts(limit: int = 150, query: str = "") -> list[dict]:
    if not CLAUDE_PROJECTS.exists():
        return []
    files = sorted(CLAUDE_PROJECTS.glob("*/*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)
    out, q = [], query.lower().strip()
    for f in files:
        if len(out) >= limit:
            break
        info = summarize(f)
        if not info or not info["title"]:
            continue
        if q and q not in info["title"].lower() and q not in info["cwd"].lower():
            continue
        out.append(info)
    return out


def summarize(path: Path) -> dict | None:
    cwd, title, custom = "", "", ""
    try:
        with path.open(encoding="utf-8", errors="replace") as fh:
            for i, line in enumerate(fh):
                if i > 400 or (cwd and title):
                    break
                try:
                    o = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if o.get("type") in ("custom-title", "summary"):
                    custom = o.get("customTitle") or o.get("summary") or custom
                cwd = cwd or o.get("cwd", "")
                if not title and o.get("type") == "user" and not o.get("isMeta") and not o.get("isSidechain"):
                    txt = _text_of((o.get("message") or {}).get("content"))
                    if _is_human_text(txt):
                        title = txt.strip().replace("\n", " ")[:160]
    except OSError:
        return None
    st = path.stat()
    return {
        "session_id": path.stem,
        "cwd": cwd,
        "title": custom or title,
        "mtime": st.st_mtime,
        "size": st.st_size,
    }


def find_transcript(session_id: str) -> Path | None:
    matches = list(CLAUDE_PROJECTS.glob(f"*/{session_id}.jsonl"))
    return matches[0] if matches else None


def import_events(session_id: str, max_events: int = 400) -> list[dict]:
    """Convert a transcript into stream-json user/assistant events."""
    path = find_transcript(session_id)
    if not path:
        return []
    events = []
    with path.open(encoding="utf-8", errors="replace") as fh:
        for line in fh:
            try:
                o = json.loads(line)
            except json.JSONDecodeError:
                continue
            if o.get("type") not in ("user", "assistant") or o.get("isSidechain") or o.get("isMeta"):
                continue
            msg = o.get("message")
            if not msg:
                continue
            if o["type"] == "user" and isinstance(msg.get("content"), str) and not _is_human_text(msg["content"]):
                continue
            events.append({"type": o["type"], "message": msg, "imported": True})
    return events[-max_events:]
