"use strict";

// ---------- helpers ----------
const $ = (s, el = document) => el.querySelector(s);
const h = (tag, attrs = {}, ...kids) => {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null || v === false) continue;
    if (k === "class") el.className = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "html") el.innerHTML = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const kid of kids.flat()) if (kid != null && kid !== false) el.append(kid instanceof Node ? kid : String(kid));
  return el;
};
const esc = s => String(s ?? "").replace(/[&<>"]/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const md = text => {
  if (window.marked && window.DOMPurify) return DOMPurify.sanitize(marked.parse(text || "", { breaks: true, gfm: true }));
  return `<p style="white-space:pre-wrap">${esc(text)}</p>`;
};
const ago = t => {
  const s = Date.now() / 1000 - t;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  if (s < 86400) return `${Math.floor(s / 3600)} h`;
  return new Date(t * 1000).toLocaleDateString("en-US", { day: "numeric", month: "short" });
};
const shortPath = p => (p || "").replace(/^\/Users\/[^/]+/, "~");
const fmtTok = n => n >= 1000 ? `${(n / 1000).toFixed(n >= 100000 ? 0 : 1)}k` : String(n);
const toast = (msg, ms = 2200) => {
  const t = $("#toast"); t.textContent = msg; t.hidden = false;
  clearTimeout(toast._t); toast._t = setTimeout(() => (t.hidden = true), ms);
};
const store = {
  get(k, d) { try { return JSON.parse(localStorage.getItem(k)) ?? d; } catch { return d; } },
  set(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch {} },
};

// ---------- state ----------
const S = {
  token: "",
  sessions: new Map(),
  current: null,          // visible session ID
  es: null,               // EventSource session
  globalEs: null,
  lastSeq: 0,
  tools: new Map(),       // tool_use_id -> element
  perms: new Map(),       // request_id -> element
  draft: null,
  images: [],
  snippets: [],
  modes: ["manual", "acceptEdits", "auto", "plan", "dontAsk", "bypassPermissions"],
  efforts: ["", "low", "medium", "high", "xhigh", "max"],
};
const MODE_LABELS = {
  manual: "Manual (ask for everything)", acceptEdits: "Accept edits", auto: "Auto (classify)",
  plan: "Plan (read-only)", dontAsk: "Do not ask (deny)", bypassPermissions: "⚠ Allow everything",
};
const MODE_HINTS = {
  manual: "Each sensitive tool triggers an approval request in the browser.",
  acceptEdits: "File edits are allowed automatically; other actions require approval.",
  auto: "A classifier decides; uncertain cases are sent to you for approval.",
  plan: "Claude explores and proposes a plan without changing anything.",
  dontAsk: "Anything not pre-approved is denied without asking.",
  bypassPermissions: "⚠ No checks: Claude can execute anything on your computer.",
};

// ---------- API ----------
async function api(path, opts = {}) {
  const res = await fetch(path, {
    ...opts,
    headers: { "Authorization": `Bearer ${S.token}`, ...(opts.body ? { "Content-Type": "application/json" } : {}), ...opts.headers },
    body: opts.body && typeof opts.body !== "string" ? JSON.stringify(opts.body) : opts.body,
  });
  if (res.status === 401) { askToken(); throw new Error("Invalid token"); }
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.detail || res.statusText);
  return data;
}

function askToken() {
  const d = $("#dlg-token");
  if (d.open) return;
  d.showModal();
  $("#token-form").onsubmit = () => {
    S.token = $("#t-input").value.trim();
    store.set("cr.token", S.token);
    boot();
  };
}

// ---------- sidebar ----------
function renderSidebar() {
  const q = $("#side-search").value.toLowerCase();
  const list = [...S.sessions.values()]
    .filter(s => !q || s.name.toLowerCase().includes(q) || s.cwd.toLowerCase().includes(q))
    .sort((a, b) => (!!b.pinned - !!a.pinned) || (b.updated - a.updated));
  const nav = $("#session-list");
  nav.replaceChildren(...list.map(s => {
    const st = s.pending?.length ? "waiting" : s.status;
    return h("button", { class: `s-item ${s.id === S.current ? "active" : ""}`, onclick: () => select(s.id) },
      h("div", { class: "top" },
        h("span", { class: `dot ${st} ${s.alive && st === "idle" ? "alive" : ""}`, title: st }),
        h("span", { class: "name" }, s.name),
        h("span", { class: `pin ${s.pinned ? "on" : ""}`, title: "Pin",
          onclick: e => { e.stopPropagation(); api(`/api/sessions/${s.id}`, { method: "PATCH", body: { pinned: !s.pinned } }); } }, s.pinned ? "📌" : "📍"),
        h("span", { class: "muted", style: "font-size:11px;font-weight:400" }, ago(s.updated))),
      h("div", { class: "sub" }, s.preview || shortPath(s.cwd)));
  }));
  const total = [...S.sessions.values()].reduce((a, s) => a + (s.cost_usd || 0), 0);
  $("#usage").textContent = `≈ $${total.toFixed(2)} API equivalent`;
  const waiting = [...S.sessions.values()].filter(s => s.pending?.length).length;
  document.title = (waiting ? `(${waiting}) ` : "") + "Pocket Relay";
}

function onSessionUpdate(s) {
  const prev = S.sessions.get(s.id);
  S.sessions.set(s.id, s);
  if (prev) notifyTransition(prev, s);
  renderSidebar();
  if (s.id === S.current) renderHeader();
}

function notifyTransition(prev, s) {
  const bg = document.hidden || s.id !== S.current;
  if (!bg || !("Notification" in window) || Notification.permission !== "granted") return;
  const n = (title, body) => {
    const notif = new Notification(title, { body, tag: s.id });
    notif.onclick = () => { window.focus(); select(s.id); notif.close(); };
  };
  if ((s.pending?.length || 0) > (prev.pending?.length || 0)) n(`🔐 ${s.name}`, "Claude is waiting for approval");
  else if (prev.status === "running" && s.status === "idle") n(`✅ ${s.name}`, s.preview || "Turn complete");
  else if (s.status === "error" && prev.status !== "error") n(`❌ ${s.name}`, "The Claude process stopped");
}

function renderLimits(info) {
  const box = $("#limits");
  const wins = info?.unifiedWindows || {};
  const names = { five_hour: "5-hour window", seven_day: "Week", seven_day_opus: "Week (Opus)", seven_day_sonnet: "Week (Sonnet)" };
  const rows = Object.entries(wins).map(([k, w]) => {
    const pct = Math.round((w.utilization || 0) * 100);
    const reset = w.resetsAt ? new Date(w.resetsAt * 1000).toLocaleString("en-US", { weekday: "short", hour: "2-digit", minute: "2-digit" }) : "";
    return h("div", { class: "lim", title: `Resets: ${reset}` },
      h("div", { class: "lim-top" }, h("span", {}, names[k] || k), h("span", {}, `${pct} %  ·  ↻ ${reset}`)),
      h("div", { class: "bar" }, h("i", { class: pct >= 90 ? "high" : pct >= 70 ? "mid" : "", style: `width:${Math.min(pct, 100)}%` })));
  });
  if (info?.status && info.status !== "allowed") rows.unshift(h("div", { class: "lim", style: "color:var(--red)" }, `⚠ Rate limit reached (${info.rateLimitType || info.status})`));
  box.hidden = !rows.length;
  box.replaceChildren(...rows);
}

function connectGlobal() {
  S.globalEs?.close();
  const es = new EventSource(`/api/events?token=${encodeURIComponent(S.token)}`);
  S.globalEs = es;
  es.onmessage = e => {
    const ev = JSON.parse(e.data);
    if (ev.type === "sessions") {
      S.sessions = new Map(ev.sessions.map(s => [s.id, s]));
      renderSidebar();
      if (S.current && S.sessions.has(S.current)) renderHeader();
    } else if (ev.type === "limits") renderLimits(ev.limits);
    else if (ev.type === "session_update") onSessionUpdate(ev.session);
    else if (ev.type === "session_deleted") {
      S.sessions.delete(ev.id);
      if (S.current === ev.id) clearMain();
      renderSidebar();
    }
  };
}

// ---------- header ----------
function renderHeader() {
  const s = S.sessions.get(S.current);
  if (!s) return;
  $("#s-title").textContent = s.name;
  $("#s-cwd").textContent = shortPath(s.cwd) + (s.active_model ? `  ·  ${s.active_model}` : "");
  $("#s-controls").hidden = false;
  $("#composer").hidden = false;
  const st = s.pending?.length ? "waiting" : s.status;
  const labels = { idle: s.alive ? "ready" : "idle", running: "running…", waiting: "waiting for approval", starting: "starting…", error: "error" };
  $("#s-status").replaceChildren(h("span", { class: `dot ${st} ${s.alive && st === "idle" ? "alive" : ""}` }), labels[st] || st);
  const m = $("#s-model"); if (document.activeElement !== m) m.value = s.model || "";
  $("#s-mode").value = s.permission_mode || "manual";
  $("#s-effort").value = s.effort || "";
  $("#s-ctx").textContent = s.context_tokens ? `ctx ${fmtTok(s.context_tokens)}` : "ctx —";
  $("#s-ctx").hidden = !s.context_tokens;
  $("#s-cost").textContent = `≈ $${(s.cost_usd || 0).toFixed(3)} · ${s.turns || 0} turns`;
  const busy = st === "running" || st === "waiting" || st === "starting";
  $("#stop-btn").hidden = !busy;
  // models offered by the CLI
  const models = (s.models || []).map(x => x.value).filter(Boolean);
  const all = [...new Set(["opus", "sonnet", "haiku", "fable", ...models])];
  $("#model-list").replaceChildren(...all.map(v => h("option", { value: v })));
  const banner = $("#perm-banner");
  banner.hidden = !s.pending?.length;
  banner.textContent = `🔐 ${s.pending?.length} pending approval request(s) — click to review`;
}

function clearMain() {
  S.current = null;
  S.es?.close();
  $("#s-title").textContent = "No session";
  $("#s-cwd").textContent = "";
  $("#s-controls").hidden = true;
  $("#composer").hidden = true;
  $("#perm-banner").hidden = true;
  $("#log").replaceChildren(h("div", { class: "empty" }, "Create or resume a session to get started."));
  location.hash = "";
}

// ---------- event rendering ----------
const log = () => $("#log");
let stick = true;
function append(el) {
  const l = log();
  stick = l.scrollHeight - l.scrollTop - l.clientHeight < 120;
  l.append(el);
  if (stick) l.scrollTop = l.scrollHeight;
}
function keepBottom() { const l = log(); if (stick) l.scrollTop = l.scrollHeight; }

function decorateCode(root) {
  root.querySelectorAll("pre").forEach(pre => {
    const b = h("button", { class: "copy-code", type: "button", onclick: () => { navigator.clipboard.writeText(pre.innerText.replace(/Copy$/, "")); toast("Copied"); } }, "Copy");
    pre.append(b);
  });
  return root;
}

function toolArg(name, input = {}) {
  switch (name) {
    case "Bash": return input.command;
    case "Read": case "Write": case "Edit": case "MultiEdit": case "NotebookEdit": return shortPath(input.file_path || input.notebook_path);
    case "Glob": case "Grep": return `${input.pattern}${input.path ? "  in " + shortPath(input.path) : ""}`;
    case "WebFetch": return input.url;
    case "WebSearch": return input.query;
    case "Task": case "Agent": return input.description;
    case "TodoWrite": return `${input.todos?.length || 0} tasks`;
    case "Skill": return input.skill;
    default: { const s = JSON.stringify(input); return s.length > 120 ? s.slice(0, 120) + "…" : s; }
  }
}

function diffView(oldS = "", newS = "") {
  return h("div", { class: "diff" },
    ...oldS.split("\n").map(l => h("div", { class: "d" }, "- " + l)),
    ...newS.split("\n").map(l => h("div", { class: "a" }, "+ " + l)));
}

function toolInputView(name, input = {}) {
  if (name === "Edit") return diffView(input.old_string, input.new_string);
  if (name === "MultiEdit") return h("div", {}, ...(input.edits || []).map(e => diffView(e.old_string, e.new_string)));
  if (name === "Write") return h("pre", {}, (input.content || "").slice(0, 8000));
  if (name === "Bash") return h("pre", {}, input.command + (input.description ? `\n# ${input.description}` : ""));
  if (name === "TodoWrite") return h("ul", { class: "todos" }, ...(input.todos || []).map(t =>
    h("li", { class: t.status }, (t.status === "completed" ? "☑ " : t.status === "in_progress" ? "▶ " : "☐ ") + (t.content || t.activeForm))));
  return h("pre", {}, JSON.stringify(input, null, 2));
}

function resultText(content) {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map(b => b.type === "text" ? b.text : b.type === "image" ? "[image]" : JSON.stringify(b)).join("\n");
  return JSON.stringify(content, null, 2);
}

function renderToolUse(block) {
  const el = h("details", { class: "tool run" },
    h("summary", {}, h("span", { class: "tname" }, block.name), h("span", { class: "targ" }, toolArg(block.name, block.input)), h("span", { class: "tstate" })),
    h("div", { class: "tbody" }, h("div", { class: "label" }, "Input"), toolInputView(block.name, block.input)));
  if (block.name === "TodoWrite") el.open = true;
  S.tools.set(block.id, el);
  append(el);
}

function renderToolResult(block) {
  const el = S.tools.get(block.tool_use_id);
  if (!el) return;
  el.classList.remove("run");
  el.classList.add(block.is_error ? "err" : "ok");
  let txt = resultText(block.content);
  const full = txt;
  if (txt.length > 6000) txt = txt.slice(0, 6000) + `\n… (${full.length - 6000} more characters)`;
  $(".tbody", el).append(h("div", { class: "label" }, block.is_error ? "Error" : "Result"), h("pre", {}, txt));
  if (block.is_error) el.open = true;
}

function renderUser(ev) {
  const c = ev.message?.content;
  if (Array.isArray(c) && c.some(b => b.type === "tool_result")) {
    c.filter(b => b.type === "tool_result").forEach(renderToolResult);
    return;
  }
  if (ev.parent_tool_use_id) return;
  const bubble = h("div", { class: "bubble" });
  if (typeof c === "string") bubble.textContent = c;
  else (c || []).forEach(b => {
    if (b.type === "text") bubble.append(b.text);
    else if (b.type === "image" && b.source?.data) bubble.append(h("img", { src: `data:${b.source.media_type};base64,${b.source.data}` }));
  });
  append(h("div", { class: "msg user" }, bubble));
}

function renderAssistant(ev) {
  if (ev.parent_tool_use_id) return; // subagent messages
  dropDraft();
  if (S.live) setLive("Claude is working…"); // keep the indicator below the new block
  for (const b of ev.message?.content || []) {
    if (b.type === "text" && b.text.trim()) append(decorateCode(h("div", { class: "msg assistant" }, h("div", { class: "md", html: md(b.text) }))));
    else if (b.type === "thinking" && b.thinking) append(h("details", { class: "thinking" }, h("summary", {}, "Thinking"), h("div", {}, b.thinking)));
    else if (b.type === "tool_use") renderToolUse(b);
  }
}

function renderPermission(ev) {
  const s = S.sessions.get(S.current);
  const pending = s?.pending?.includes(ev.request_id);
  const deny = h("input", { placeholder: "Reason for denial / instructions for Claude (optional)" });
  const decide = async (behavior, always = false) => {
    try {
      await api(`/api/sessions/${S.current}/permissions/${ev.request_id}`, { method: "POST", body: { behavior, always, message: deny.value } });
    } catch (e) { toast(e.message); }
  };
  const el = h("div", { class: `perm ${pending ? "" : "done"}` },
    h("div", { class: "ph" }, `🔐 ${ev.tool_name}`, h("span", { class: "muted", style: "font-weight:400" }, "  " + (toolArg(ev.tool_name, ev.input) || ""))),
    ev.decision_reason ? h("div", { class: "hint" }, typeof ev.decision_reason === "string" ? ev.decision_reason : JSON.stringify(ev.decision_reason)) : null,
    toolInputView(ev.tool_name, ev.input),
    h("div", { class: "actions" },
      h("button", { class: "primary", type: "button", onclick: () => decide("allow") }, "Allow"),
      h("button", { type: "button", onclick: () => decide("allow", true), title: `Allow ${ev.tool_name} without asking again in this session` }, `Always (${ev.tool_name})`),
      deny,
      h("button", { class: "danger", type: "button", onclick: () => decide("deny") }, "Deny")));
  if (!pending) el.firstChild.append(h("span", { class: "muted pstate", style: "font-weight:400" }, "  — expired"));
  S.perms.set(ev.request_id, el);
  append(el);
  if (pending) setTimeout(() => el.scrollIntoView({ block: "center" }), 50);
}

function resolvePermission(ev) {
  const el = S.perms.get(ev.request_id);
  if (!el) return;
  el.classList.add("done");
  $(".pstate", el)?.remove();
  const label = { allow: ev.always ? "✓ allowed (always)" : "✓ allowed", deny: "✗ denied", cancelled: "canceled" }[ev.behavior] || ev.behavior;
  el.firstChild.append(h("span", { class: "muted pstate", style: "font-weight:400" }, "  — " + label));
}

function dropDraft() { S.draft?.remove(); S.draft = null; }

// Activity indicator for startup, thinking and tool preparation, which would otherwise be invisible.
function setLive(text) {
  if (!S.live) S.live = h("div", { class: "live" });
  S.live.textContent = text;
  log().append(S.live); // always at the bottom of the conversation
  keepBottom();
}
function clearLive() { S.live?.remove(); S.live = null; S.liveBlock = null; }

function renderStreamEvent(ev) {
  if (ev.parent_tool_use_id) return;
  const e = ev.event || {};
  if (e.type === "message_start") setLive("Claude is thinking…");
  else if (e.type === "content_block_start") {
    const b = e.content_block || {};
    S.liveBlock = { type: b.type, name: b.name, size: 0 };
    if (b.type === "thinking") setLive("Thinking…");
    else if (b.type === "tool_use") setLive(`Preparing ${b.name}…`);
    else if (b.type === "text") clearLive();
  } else if (e.type === "content_block_delta") {
    const d = e.delta || {};
    if (d.type === "text_delta") {
      clearLive();
      if (!S.draft) {
        S.draft = h("div", { class: "msg assistant draft md" });
        S.draft._text = "";
        append(S.draft);
      }
      S.draft._text += d.text;
      const draft = S.draft;
      if (!draft._frame) draft._frame = requestAnimationFrame(() => {
        draft._frame = null;
        if (S.draft !== draft || !draft.isConnected) return;
        draft.innerHTML = md(draft._text);
        keepBottom();
      });
    } else if (d.type === "thinking_delta") {
      if (d.thinking) S.liveThinking = (S.liveThinking || 0) + d.thinking.length;
      setLive(`Thinking…${d.estimated_tokens ? `  ~${d.estimated_tokens} tokens` : ""}`);
    } else if (d.type === "input_json_delta" && S.liveBlock) {
      S.liveBlock.size += (d.partial_json || "").length;
      setLive(`Preparing ${S.liveBlock.name}…  ${(S.liveBlock.size / 1024).toFixed(1)} KiB`);
    }
  } else if (e.type === "message_stop") clearLive();
}

function renderResult(ev) {
  dropDraft(); clearLive();
  const bits = [];
  if (ev.duration_ms) bits.push(`${(ev.duration_ms / 1000).toFixed(1)} s`);
  if (ev.num_turns) bits.push(`${ev.num_turns} steps`);
  if (ev.total_cost_usd != null) bits.push(`≈ $${ev.total_cost_usd.toFixed(3)} total`);
  if (ev.is_error) bits.unshift(`⚠ ${ev.subtype}${ev.result ? " : " + String(ev.result).slice(0, 200) : ""}`);
  append(h("div", { class: `result ${ev.is_error ? "err" : ""}` }, bits.join("  ·  ")));
}

function renderRemote(ev) {
  switch (ev.event) {
    case "exited":
      clearLive();
      if (ev.code && ev.code !== -15) append(h("div", { class: "note err" }, `Process exited (code ${ev.code})\n${ev.stderr || ""}`));
      else append(h("div", { class: "note" }, "Process idle — restarts automatically with the next message"));
      break;
    case "interrupted": dropDraft(); clearLive(); append(h("div", { class: "note" }, "⏹ Interrompu")); break;
    case "resumed": append(h("div", { class: "note" }, `↺ CLI session resumed${ev.fork ? " (fork)" : ""} · ${ev.claude_session_id}`)); break;
    case "config": append(h("div", { class: "note" }, "⚙ " + Object.entries(ev.changes).map(([k, v]) => `${k} → ${v || "default"}`).join(", "))); break;
    case "error": case "raw": append(h("div", { class: "note err" }, ev.text)); break;
    case "status": {
      if (ev.status === "starting") setLive("Starting Claude…");
      if (ev.status === "waiting") clearLive();
      const s = S.sessions.get(S.current);
      if (s) { s.status = ev.status; renderHeader(); }
      break;
    }
    case "snapshot": onSessionUpdate(ev.session); break;
  }
}

function renderEvent(ev) {
  if (ev.seq) S.lastSeq = Math.max(S.lastSeq, ev.seq);
  switch (ev.type) {
    case "user": return renderUser(ev);
    case "assistant": return renderAssistant(ev);
    case "stream_event": return renderStreamEvent(ev);
    case "result": return renderResult(ev);
    case "permission_request": return renderPermission(ev);
    case "permission_resolved": return resolvePermission(ev);
    case "permission_auto": return;
    case "system":
      if (ev.subtype === "init") append(h("div", { class: "note" }, `● ${ev.model} · ${ev.permissionMode || ""} · ${ev.tools?.length || 0} outils${ev.mcp_servers?.length ? ` · MCP : ${ev.mcp_servers.map(m => m.name + (m.status === "connected" ? "" : " ✗")).join(", ")}` : ""}`));
      else if (ev.subtype === "compact_boundary") append(h("div", { class: "note" }, "— context compacted —"));
      return;
    case "remote": return renderRemote(ev);
  }
}

// ---------- session selection ----------
function select(id) {
  if (!S.sessions.has(id)) return;
  S.es?.close();
  S.current = id;
  S.lastSeq = 0;
  S.tools.clear(); S.perms.clear(); S.draft = null; S.live = null;
  $("#log").replaceChildren();
  $("#app").classList.remove("side-open");
  location.hash = `s=${id}`;
  renderSidebar();
  renderHeader();
  openStream(id);
  input.value = store.get(`cr.draft.${id}`, ""); autosize();
  input.focus();
}

function openStream(id) {
  const es = new EventSource(`/api/sessions/${id}/stream?token=${encodeURIComponent(S.token)}&since=${S.lastSeq}`);
  S.es = es;
  es.onmessage = e => { if (S.current === id) renderEvent(JSON.parse(e.data)); };
  es.onerror = () => {
    // EventSource reconnects automatically but starts over; resume from lastSeq instead.
    es.close();
    if (S.current === id) setTimeout(() => S.current === id && openStream(id), 1500);
  };
}

// ---------- composer ----------
const input = $("#input");
function autosize() { input.style.height = "auto"; input.style.height = Math.min(input.scrollHeight, innerHeight * 0.4) + "px"; }

async function send() {
  const text = input.value;
  if (!text.trim() && !S.images.length) return;
  const images = S.images.map(({ media_type, data }) => ({ media_type, data }));
  input.value = ""; store.set(`cr.draft.${S.current}`, ""); S.images = []; renderAttachments(); autosize(); hideSlash();
  const hist = store.get("cr.hist", []);
  if (text.trim()) store.set("cr.hist", [text, ...hist.filter(x => x !== text)].slice(0, 50));
  histIdx = -1;
  setLive("Sending…");
  try { await api(`/api/sessions/${S.current}/messages`, { method: "POST", body: { text, images } }); }
  catch (e) { toast(e.message); input.value = text; }
}

function addImage(file) {
  if (!file.type.startsWith("image/")) return;
  if (file.size > 5 * 1024 * 1024) return toast("Image larger than 5 MiB skipped");
  const r = new FileReader();
  r.onload = () => {
    const [, data] = r.result.split(",");
    S.images.push({ media_type: file.type, data, url: r.result });
    renderAttachments();
  };
  r.readAsDataURL(file);
}
function renderAttachments() {
  $("#attachments").replaceChildren(...S.images.map((im, i) =>
    h("div", { class: "att" }, h("img", { src: im.url }), h("button", { type: "button", onclick: () => { S.images.splice(i, 1); renderAttachments(); } }, "×"))));
}

// slash commands
let slashSel = 0, histIdx = -1;
function slashItems() {
  const v = input.value;
  if (!v.startsWith("/") || v.includes(" ") || v.includes("\n")) return [];
  const s = S.sessions.get(S.current);
  const cmds = (s?.commands || []).map(c => ({ name: c.name, desc: c.description, hint: c.argumentHint }));
  const base = [{ name: "compact", desc: "Compact context" }, { name: "clear", desc: "Clear context" }];
  const all = [...cmds, ...base.filter(b => !cmds.some(c => c.name === b.name))];
  const q = v.slice(1).toLowerCase();
  return all.filter(c => c.name.toLowerCase().includes(q)).slice(0, 40);
}
function renderSlash() {
  const items = slashItems(), menu = $("#slash-menu");
  if (!items.length) return hideSlash();
  slashSel = Math.min(slashSel, items.length - 1);
  menu.hidden = false;
  menu.replaceChildren(...items.map((c, i) => h("div", { class: i === slashSel ? "sel" : "", onmousedown: e => { e.preventDefault(); pickSlash(c); } },
    "/" + c.name, c.hint ? h("small", {}, c.hint) : null, c.desc ? h("small", {}, c.desc) : null)));
}
function hideSlash() { $("#slash-menu").hidden = true; slashSel = 0; }
function pickSlash(c) { input.value = `/${c.name} `; hideSlash(); input.focus(); }

input.addEventListener("input", () => { autosize(); renderSlash(); store.set(`cr.draft.${S.current}`, input.value); });
input.addEventListener("keydown", e => {
  const menuOpen = !$("#slash-menu").hidden;
  if (menuOpen && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
    e.preventDefault(); slashSel += e.key === "ArrowDown" ? 1 : -1; if (slashSel < 0) slashSel = 0; renderSlash(); return;
  }
  if (menuOpen && (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey))) {
    e.preventDefault(); const it = slashItems()[slashSel]; if (it) pickSlash(it); return;
  }
  if (e.key === "Escape") { hideSlash(); if (!$("#stop-btn").hidden) interrupt(); return; }
  // Navigate prompt history with Up/Down when the input is empty or history navigation is active.
  if ((e.key === "ArrowUp" && (input.value === "" || histIdx >= 0)) || (e.key === "ArrowDown" && histIdx >= 0)) {
    const hist = store.get("cr.hist", []);
    if (!hist.length) return;
    e.preventDefault();
    histIdx = Math.max(-1, Math.min(hist.length - 1, histIdx + (e.key === "ArrowUp" ? 1 : -1)));
    input.value = histIdx < 0 ? "" : hist[histIdx]; autosize(); return;
  }
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); }
});
input.addEventListener("paste", e => {
  for (const it of e.clipboardData?.items || []) if (it.kind === "file") { addImage(it.getAsFile()); e.preventDefault(); }
});
$("#main").addEventListener("dragover", e => e.preventDefault());
$("#main").addEventListener("drop", e => { e.preventDefault(); [...e.dataTransfer.files].forEach(addImage); });
$("#file-input").addEventListener("change", e => { [...e.target.files].forEach(addImage); e.target.value = ""; });
$("#composer").addEventListener("submit", e => { e.preventDefault(); send(); });

async function interrupt() {
  try { await api(`/api/sessions/${S.current}/interrupt`, { method: "POST" }); } catch (e) { toast(e.message); }
}
$("#stop-btn").onclick = interrupt;
$("#perm-banner").onclick = () => {
  const el = [...S.perms.values()].find(p => !p.classList.contains("done"));
  el?.scrollIntoView({ behavior: "smooth", block: "center" });
};

// header controls
const patch = body => api(`/api/sessions/${S.current}`, { method: "PATCH", body }).catch(e => toast(e.message));
$("#s-mode").onchange = e => {
  if (e.target.value === "bypassPermissions" && !confirm("This mode disables all checks: Claude can execute anything. Continue?")) return renderHeader();
  patch({ permission_mode: e.target.value });
};
$("#s-model").onchange = e => patch({ model: e.target.value.trim() });
$("#s-effort").onchange = e => {
  const s = S.sessions.get(S.current);
  if (s?.alive && !confirm("Changing effort restarts the process while preserving the conversation. Continue?")) return renderHeader();
  patch({ effort: e.target.value });
};

// ---------- new session ----------
function fillSelect(sel, values, labels = {}) {
  sel.replaceChildren(...values.map(v => h("option", { value: v }, labels[v] || v || "default")));
}

async function openNew(prefill = {}) {
  const d = $("#dlg-new");
  $("#n-cwd").value = prefill.cwd || store.get("cr.lastCwd", "~");
  $("#n-name").value = ""; $("#n-sys").value = ""; $("#n-dirs").value = "";
  $("#n-model").value = store.get("cr.lastModel", "");
  $("#n-mode").value = store.get("cr.lastMode", "manual");
  $("#n-effort").value = "";
  $("#n-browser").hidden = true;
  modeHint();
  const recents = await api("/api/recent-dirs").catch(() => []);
  $("#n-recents").replaceChildren(...recents.map(p => h("button", { type: "button", onclick: () => ($("#n-cwd").value = p) }, shortPath(p))));
  d.showModal();
  $("#n-cwd").focus();
}
function modeHint() {
  const m = $("#n-mode").value, el = $("#n-mode-hint");
  el.textContent = MODE_HINTS[m] || ""; el.className = `hint ${m === "bypassPermissions" ? "warn" : ""}`;
}
$("#n-mode").onchange = modeHint;
$("#dlg-new").addEventListener("close", async () => {
  if ($("#dlg-new").returnValue !== "ok") return;
  const body = {
    cwd: $("#n-cwd").value.trim(), name: $("#n-name").value.trim(), model: $("#n-model").value.trim(),
    permission_mode: $("#n-mode").value, effort: $("#n-effort").value, append_system_prompt: $("#n-sys").value.trim(),
    add_dirs: $("#n-dirs").value.split(",").map(s => s.trim()).filter(Boolean),
  };
  store.set("cr.lastCwd", body.cwd); store.set("cr.lastModel", body.model); store.set("cr.lastMode", body.permission_mode);
  try {
    const s = await api("/api/sessions", { method: "POST", body });
    S.sessions.set(s.id, s);
    select(s.id);
  } catch (e) { toast(e.message, 4000); }
});
$("#new-btn").onclick = () => openNew();

async function browse(path) {
  const box = $("#n-browser");
  try {
    const r = await api(`/api/fs?path=${encodeURIComponent(path)}`);
    box.hidden = false;
    box.replaceChildren(
      h("div", { class: "cur" }, h("span", {}, shortPath(r.path) + (r.is_git ? "  (git)" : "")),
        h("button", { type: "button", onclick: () => { $("#n-cwd").value = r.path; box.hidden = true; } }, "Choose")),
      r.parent ? h("div", { onclick: () => browse(r.parent) }, "⬑ ..") : null,
      ...r.dirs.map(d => h("div", { onclick: () => browse(`${r.path}/${d}`) }, "📁 " + d)));
    $("#n-cwd").value = r.path;
  } catch (e) { toast(e.message); }
}
$("#n-browse").onclick = () => browse($("#n-cwd").value || "~");

// ---------- resume ----------
let resumeTimer;
async function loadHistory() {
  const q = $("#r-search").value;
  const list = $("#r-list");
  list.replaceChildren(h("div", { class: "hint" }, "Loading…"));
  const items = await api(`/api/history?q=${encodeURIComponent(q)}`).catch(e => (toast(e.message), []));
  list.replaceChildren(...items.map(it => h("div", { class: "item", onclick: () => resume(it) },
    h("div", { class: "t" }, it.title),
    h("div", { class: "m" }, `${shortPath(it.cwd)}  ·  ${ago(it.mtime)}  ·  ${Math.round(it.size / 1024)} ko`))));
  if (!items.length) list.replaceChildren(h("div", { class: "hint" }, "No sessions found."));
}
async function resume(it) {
  $("#dlg-resume").close();
  try {
    const s = await api("/api/sessions", { method: "POST", body: {
      cwd: it.cwd, resume: it.session_id, fork: $("#r-fork").checked, name: it.title.slice(0, 48),
      permission_mode: store.get("cr.lastMode", "manual"),
    } });
    S.sessions.set(s.id, s);
    select(s.id);
  } catch (e) { toast(e.message, 4000); }
}
$("#resume-btn").onclick = () => { $("#dlg-resume").showModal(); loadHistory(); };
$("#r-search").oninput = () => { clearTimeout(resumeTimer); resumeTimer = setTimeout(loadHistory, 250); };

// ---------- session menu ----------
$("#s-more").onclick = () => {
  const s = S.sessions.get(S.current);
  if (!s) return;
  $("#m-title").textContent = s.name;
  const kv = { "Local ID": s.id, "CLI session": s.claude_session_id, "Directory": s.cwd, "Active model": s.active_model || "—",
    "Process": s.alive ? "active" : "idle", "Estimated cost": `$${(s.cost_usd || 0).toFixed(4)}`, "Created": new Date(s.created * 1000).toLocaleString("en-US") };
  $("#m-info").replaceChildren(...Object.entries(kv).flatMap(([k, v]) => [h("dt", {}, k), h("dd", {}, v)]));
  $("#m-allow").replaceChildren(s.auto_allow?.length
    ? h("label", {}, "Always-allowed tools (click to remove)", h("div", { class: "chips" }, ...s.auto_allow.map(t =>
        h("button", { type: "button", onclick: async () => { await api(`/api/sessions/${s.id}/auto-allow/${encodeURIComponent(t)}`, { method: "DELETE" }); $("#dlg-more").close(); } }, t + " ×"))))
    : "");
  $("#m-sys").value = s.append_system_prompt || "";
  $("#dlg-more").showModal();
};
$("#m-rename").onclick = () => {
  const s = S.sessions.get(S.current);
  const name = prompt("New name", s.name);
  if (name) { patch({ name }); $("#dlg-more").close(); }
};
$("#m-copy").onclick = () => {
  const s = S.sessions.get(S.current);
  navigator.clipboard.writeText(`cd ${JSON.stringify(s.cwd)} && claude --resume ${s.claude_session_id}`);
  toast("Command copied: resume the session in your terminal");
};
$("#m-stop").onclick = async () => { await api(`/api/sessions/${S.current}/stop`, { method: "POST" }); $("#dlg-more").close(); };
$("#m-delete").onclick = async () => {
  if (!confirm("Delete this session from Pocket Relay? (The CLI transcript stays in ~/.claude.)")) return;
  await api(`/api/sessions/${S.current}`, { method: "DELETE" });
  $("#dlg-more").close();
};
$("#m-save").onclick = () => {
  const s = S.sessions.get(S.current);
  const v = $("#m-sys").value.trim();
  if (v !== (s.append_system_prompt || "")) patch({ append_system_prompt: v });
  $("#dlg-more").close();
};

// ---------- snippets ----------
const DEFAULT_SNIPPETS = [
  { title: "Review diff", text: "Critically review my uncommitted changes (git diff): bugs, edge cases and simplifications." },
  { title: "Explain", text: "Explain in detail how this works: {{sel}}" },
  { title: "Tests", text: "Run the test suite, analyze failures and suggest fixes." },
  { title: "Commit", text: "Prepare a clean commit of the current state following repository conventions." },
];
async function loadSnippets() {
  S.snippets = await api("/api/snippets").catch(() => []);
  if (!S.snippets.length) S.snippets = DEFAULT_SNIPPETS;
}
function renderSnippetEditor() {
  $("#snip-list").replaceChildren(...S.snippets.map((sn, i) => h("div", { class: "snip" },
    h("input", { value: sn.title, placeholder: "Title", oninput: e => (sn.title = e.target.value) }),
    h("textarea", { rows: 2, oninput: e => (sn.text = e.target.value) }, sn.text),
    h("button", { type: "button", class: "danger", onclick: () => { S.snippets.splice(i, 1); renderSnippetEditor(); } }, "×"))));
}
$("#snippets-btn").onclick = () => { renderSnippetEditor(); $("#dlg-snippets").showModal(); };
$("#snip-add").onclick = () => { S.snippets.push({ title: "", text: "" }); renderSnippetEditor(); };
$("#snip-save").onclick = async () => {
  S.snippets = S.snippets.filter(s => s.text.trim());
  await api("/api/snippets", { method: "PUT", body: S.snippets });
  $("#dlg-snippets").close(); toast("Snippets saved");
};
$("#snip-insert").onclick = () => {
  let menu = $("#snip-pop");
  if (menu) return menu.remove();
  menu = h("div", { id: "snip-pop", class: "popmenu" }, ...S.snippets.map(sn => h("div", { onmousedown: e => {
    e.preventDefault();
    const sel = input.value;
    input.value = sn.text.includes("{{sel}}") ? sn.text.replaceAll("{{sel}}", sel) : (sel ? sel + "\n\n" : "") + sn.text;
    menu.remove(); autosize(); input.focus();
  } }, sn.title || sn.text.slice(0, 40), h("small", {}, sn.text.slice(0, 60)))));
  $("#composer").append(menu);
  setTimeout(() => document.addEventListener("click", () => menu.remove(), { once: true }));
};

// ---------- miscellaneous ----------
$("#side-search").oninput = renderSidebar;
$("#menu-btn").onclick = () => $("#app").classList.toggle("side-open");
$("#theme-btn").onclick = () => {
  const cur = document.documentElement.dataset.theme || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  const next = cur === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next; store.set("cr.theme", next);
};
document.addEventListener("keydown", e => {
  if ((e.metaKey || e.ctrlKey) && e.key === "k") { e.preventDefault(); $("#app").classList.add("side-open"); $("#side-search").focus(); }
  if ((e.metaKey || e.ctrlKey) && e.key === "j") { e.preventDefault(); openNew(); }
});
log().addEventListener("scroll", () => { const l = log(); stick = l.scrollHeight - l.scrollTop - l.clientHeight < 120; });
setInterval(renderSidebar, 60000);

// ---------- startup ----------
async function boot() {
  const theme = store.get("cr.theme", null);
  if (theme) document.documentElement.dataset.theme = theme;
  const m = location.hash.match(/token=([^&]+)/);
  if (m) { store.set("cr.token", decodeURIComponent(m[1])); history.replaceState(null, "", "/"); }
  S.token = store.get("cr.token", "");
  if (!S.token) return askToken();
  try {
    const hinfo = await api("/api/health");
    S.modes = hinfo.permission_modes; S.efforts = hinfo.efforts;
  } catch { return; }
  fillSelect($("#s-mode"), S.modes, MODE_LABELS); fillSelect($("#n-mode"), S.modes, MODE_LABELS);
  fillSelect($("#s-effort"), S.efforts); fillSelect($("#n-effort"), S.efforts);
  const list = await api("/api/sessions");
  S.sessions = new Map(list.map(s => [s.id, s]));
  renderSidebar();
  connectGlobal();
  loadSnippets();
  const want = location.hash.match(/s=([\w]+)/)?.[1] || store.get("cr.current", null);
  if (want && S.sessions.has(want)) select(want);
  if ("Notification" in window && Notification.permission === "default") {
    document.addEventListener("click", () => Notification.requestPermission(), { once: true });
  }
}
addEventListener("beforeunload", () => store.set("cr.current", S.current));
boot();
