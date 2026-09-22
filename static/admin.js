"use strict";

// ---------- utilities ----------
const $ = s => document.querySelector(s);
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
const icon = (name, cls = "i") => {
  const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  s.setAttribute("class", cls);
  const u = document.createElementNS("http://www.w3.org/2000/svg", "use");
  u.setAttribute("href", `#i-${name}`);
  s.append(u);
  return s;
};
const fmtDate = t => t ? new Date(t * 1000).toLocaleString("en-US", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }) : "—";
const fmtTime = t => new Date(t * 1000).toLocaleTimeString("en-US", { hour: "2-digit", minute: "2-digit" });
const ago = t => {
  if (!t) return "never";
  const s = Date.now() / 1000 - t;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return fmtDate(t);
};
const num = n => n >= 1e6 ? (n / 1e6).toFixed(1) + " M" : n >= 1e3 ? (n / 1e3).toFixed(1) + " k" : String(n);
function toast(msg, bad = false) {
  const t = h("div", { class: `toast ${bad ? "bad" : ""}` }, msg);
  $("#toasts").append(t);
  setTimeout(() => t.remove(), bad ? 5000 : 2600);
}
const copy = (text, what = "Copied") => navigator.clipboard.writeText(text).then(() => toast(what));

async function call(path, method = "GET", body) {
  const r = await fetch(path, {
    method, credentials: "same-origin",
    headers: { "X-Admin": "1", ...(body ? { "Content-Type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const d = await r.json().catch(() => ({}));
  if (r.status === 401 && path !== "/admin/auth/token") { showLogin(); throw new Error("Session expired"); }
  if (!r.ok) throw new Error(d.detail || r.statusText);
  return d;
}

let S = null;          // last received state
let pollTimer = null;

// ---------- theme ----------
function applyTheme(t) { if (t) document.documentElement.dataset.theme = t; }
try { applyTheme(localStorage.getItem("cr.admin.theme")); } catch {}
$("#theme").onclick = () => {
  const cur = document.documentElement.dataset.theme || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
  const next = cur === "dark" ? "light" : "dark";
  applyTheme(next);
  try { localStorage.setItem("cr.admin.theme", next); } catch {}
};

// ---------- connection ----------
async function showLogin() {
  clearInterval(pollTimer);
  $("#console").hidden = true;
  $("#login").hidden = false;
  const cfg = await fetch("/admin/auth/config").then(r => r.json()).catch(() => ({}));
  $("#oidc-btn").hidden = !cfg.oidc;
  $("#or").hidden = !cfg.oidc || cfg.token_login === false;
  $("#master").closest("label").hidden = cfg.token_login === false;
  $("#master-btn").hidden = cfg.token_login === false;
  const denied = new URLSearchParams(location.search).get("denied");
  if (denied) { $("#denied").hidden = false; $("#denied").textContent = `Access denied for ${denied}: this account is not an administrator.`; }
}
$("#oidc-btn").onclick = () => (location.href = "/admin/auth/login");
$("#master-btn").onclick = async () => {
  try { await call("/admin/auth/token", "POST", { token: $("#master").value }); $("#master").value = ""; start(); }
  catch (e) { toast(e.message, true); }
};
$("#master").onkeydown = e => { if (e.key === "Enter") $("#master-btn").click(); };
$("#logout").onclick = async () => { await call("/admin/auth/logout", "POST").catch(() => {}); showLogin(); };

// ---------- rendering ----------
function render() {
  const s = S;
  $("#who").textContent = s.user.who + (s.user.how === "token" ? " · recovery" : "");
  $("#avatar").textContent = (s.user.who || "?").trim()[0].toUpperCase();
  $("#host").textContent = location.host;
  $("#base-url").textContent = `${location.origin}/v1`;
  renderKpis();
  renderAccounts("claude");
  $("#codex-col").hidden = !s.codex_enabled;
  if (s.codex_enabled) renderAccounts("codex");
  renderKeys();
}

function renderKpis() {
  const all = [...S.accounts.claude, ...(S.codex_enabled ? S.accounts.codex : [])];
  const ready = all.filter(a => a.status === "ok" || a.status === "unknown").length;
  const activeKeys = S.keys.filter(k => !k.revoked).length;
  const kpi = (ic, lbl, val, sub) => h("div", { class: "kpi" },
    h("div", { class: "lbl" }, icon(ic), lbl), h("div", { class: "val" }, val, sub ? h("small", {}, " " + sub) : null));
  $("#kpis").replaceChildren(
    kpi("bolt", "Requests", num(S.stats.requests), S.stats.errors ? `· ${S.stats.errors} errors` : ""),
    kpi("chart", "Generated tokens", num(S.stats.output_tokens)),
    kpi("users", "Available accounts", ready, `/ ${all.length}`),
    kpi("key", "Active keys", activeKeys, `/ ${S.keys.length}`),
  );
}

const STATUS = {
  ok: ["ok", "Active"], unknown: ["unknown", "Never used"], disabled: ["disabled", "Disabled"],
  error: ["error", "Error"], paused: ["paused", "Paused"],
};

function meters(a) {
  const out = [];
  const l = a.limits || {};
  const bar = (label, pct, reset) => {
    pct = Math.max(0, Math.min(100, Math.round(pct)));
    return h("div", { class: "meter", title: reset ? `Resets: ${fmtDate(reset)}` : "" },
      h("div", { class: "row" }, h("span", {}, label), h("b", {}, `${pct} %`)),
      h("div", { class: "bar" }, h("i", { class: pct >= 90 ? "high" : pct >= 70 ? "mid" : "", style: `width:${pct}%` })));
  };
  if (a.provider === "claude") {
    const w = l.unifiedWindows || {};
    const names = { five_hour: "5 hours", seven_day: "7 days", seven_day_opus: "7 days · Opus", seven_day_sonnet: "7 days · Sonnet" };
    for (const [k, v] of Object.entries(w)) out.push(bar(names[k] || k, (v.utilization || 0) * 100, v.resetsAt));
  } else {
    for (const [k, lbl] of [["primary", "Primary"], ["secondary", "Secondary"]]) {
      const w = l[k];
      if (!w) continue;
      const d = w.windowDurationMins;
      const dur = d ? (d >= 1440 ? `${Math.round(d / 1440)} days` : `${Math.round(d / 60)} hours`) : lbl;
      out.push(bar(dur, w.usedPercent || 0, w.resetsAt));
    }
  }
  return out.length ? h("div", { class: "meters" }, out) : null;
}

function identity(a) {
  if (a.provider === "codex") {
    const id = a.identity;
    if (id) return `${id.email || "ChatGPT account"}${id.planType ? " · plan " + id.planType : ""}`;
    return a.system ? "This server's codex login session: not signed in" : "Not signed in";
  }
  return a.system ? "This server's claude login session" : `Token ${a.masked || "missing"}`;
}

function renderAccounts(provider) {
  const list = S.accounts[provider];
  const box = $(`#acc-${provider}`);
  if (!list.length) {
    box.replaceChildren(h("div", { class: "empty-accounts" }, "No accounts yet."));
    return;
  }
  box.replaceChildren(...list.map((a, i) => {
    const [cls, label] = STATUS[a.status] || ["unknown", a.status];
    const pill = h("span", { class: `pill ${cls}` }, a.status === "paused" && a.paused_until ? `${label} · resumes ${fmtTime(a.paused_until)}` : label);
    const toggle = h("label", { class: "switch", title: a.enabled ? "Disable" : "Enable" },
      h("input", { type: "checkbox", checked: a.enabled, onchange: e => act(() => call(`/admin/api/accounts/${a.id}`, "PATCH", { enabled: e.target.checked })) }),
      h("span"));
    const notes = [];
    if (a.status === "paused") notes.push(h("div", { class: "acc-note warn" }, `Paused (${a.pause_reason || "quota"}): requests use the next account.`));
    else if (a.last_error && a.enabled) notes.push(h("div", { class: "acc-note bad" }, a.last_error.message));
    const needsLogin = a.provider === "codex" && !a.identity && a.enabled;
    return h("div", { class: `acc ${a.enabled ? "" : "disabled"}` },
      h("div", { class: "acc-top" },
        h("span", { class: "prio", title: "Priority" }, i + 1),
        h("span", { class: "acc-name" }, a.label),
        pill,
        h("span", { class: "spacer" }),
        toggle),
      h("div", { class: "acc-id" }, identity(a)),
      meters(a),
      notes,
      h("div", { class: "acc-foot" },
        h("span", { class: "stat" }, `${a.requests || 0} request${(a.requests || 0) !== 1 ? "s" : ""} · ${ago(a.last_used)}`),
        needsLogin ? h("button", { class: "btn sm primary", onclick: () => codexLogin(a) }, icon("link"), "Connect") : null,
        h("button", { class: "btn sm", onclick: e => testAccount(a, e.currentTarget) }, icon("play"), "Test"),
        h("button", { class: "btn ghost icon", title: "Move up", disabled: i === 0, onclick: () => act(() => call(`/admin/api/accounts/${a.id}/move`, "POST", { delta: -1 })) }, icon("up")),
        h("button", { class: "btn ghost icon", title: "Move down", disabled: i === list.length - 1, onclick: () => act(() => call(`/admin/api/accounts/${a.id}/move`, "POST", { delta: 1 })) }, icon("down")),
        h("button", { class: "btn ghost icon", title: "More", onclick: e => accountMenu(a, e.currentTarget) }, icon("dots"))));
  }));
}

async function act(fn, okMsg) {
  try { await fn(); if (okMsg) toast(okMsg); } catch (e) { toast(e.message, true); }
  refresh();
}

async function testAccount(a, btn) {
  btn.disabled = true;
  const old = btn.lastChild.textContent;
  btn.lastChild.textContent = "Test…";
  try {
    const r = await call(`/admin/api/accounts/${a.id}/test`, "POST");
    r.ok ? toast(`✓ ${a.label}: ${r.reply} in ${r.seconds} s (${r.model})`) : toast(`✗ ${a.label} : ${r.error}`, true);
  } catch (e) { toast(e.message, true); }
  btn.disabled = false;
  btn.lastChild.textContent = old;
  refresh();
}

// menu « ⋯ »
function accountMenu(a, anchor) {
  document.querySelector(".menu")?.remove();
  const item = (ic, label, fn, cls) => h("button", { class: cls || "", onclick: () => { menu.remove(); fn(); } }, icon(ic), label);
  const items = [item("edit", "Rename", () => prompt2("Rename account", "", a.label, v => call(`/admin/api/accounts/${a.id}`, "PATCH", { label: v })))];
  if (a.provider === "claude" && !a.system) items.push(item("key", "Replace token", () =>
    prompt2("New token", "Paste the output of `claude setup-token`.", "", async v => {
      const r = await call(`/admin/api/accounts/${a.id}/token`, "PUT", { token: v });
      r.test?.ok ? toast("Token replaced and tested ✓") : toast(`Token saved, but the test failed: ${r.test?.error}`, true);
    }, true)));
  if (a.provider === "codex") items.push(item("link", a.identity ? "Switch ChatGPT account" : "Connect", () => codexLogin(a)));
  if (a.status === "paused") items.push(item("refresh", "Resume now", () => act(() => call(`/admin/api/accounts/${a.id}/resume`, "POST"), "Account resumed")));
  if (a.provider === "codex" && a.identity) items.push(item("logout", "Disconnect", () => {
    if (confirm(`Disconnect ${a.identity.email || a.label} from this server?`)) act(() => call(`/admin/api/accounts/${a.id}/logout`, "POST"), "Disconnected");
  }));
  if (!a.system) {
    items.push(h("hr"));
    items.push(item("trash", "Delete", () => {
      if (confirm(`Delete account ${a.label} and its credentials?`)) act(() => call(`/admin/api/accounts/${a.id}`, "DELETE"), "Account deleted");
    }, "danger"));
  }
  const menu = h("div", { class: "menu" }, items);
  document.body.append(menu);
  const r = anchor.getBoundingClientRect();
  menu.style.top = `${r.bottom + scrollY + 6}px`;
  menu.style.left = `${Math.max(12, Math.min(r.right + scrollX - menu.offsetWidth, innerWidth - menu.offsetWidth - 12))}px`;
  setTimeout(() => document.addEventListener("click", function off(e) {
    if (!menu.contains(e.target)) { menu.remove(); document.removeEventListener("click", off); }
  }));
}

// simple input dialog
function prompt2(title, sub, value, onOk, secret = false) {
  const d = $("#dlg-prompt");
  $("#pr-title").textContent = title;
  $("#pr-sub").textContent = sub;
  const inp = $("#pr-input");
  inp.value = value;
  inp.type = secret ? "password" : "text";
  inp.className = `input ${secret ? "mono" : ""}`;
  d.returnValue = "";
  d.showModal();
  inp.select();
  d.onclose = () => { if (d.returnValue === "ok" && inp.value.trim()) act(() => onOk(inp.value.trim())); };
}

// ---------- add account ----------
let addProvider = "claude", loginPoll = null;
document.querySelectorAll("[data-add]").forEach(b => (b.onclick = () => openAdd(b.dataset.add)));

function openAdd(provider) {
  addProvider = provider;
  clearInterval(loginPoll);
  renderAddForm();
  $("#dlg-add").showModal();
}
$("#dlg-add").addEventListener("close", () => { clearInterval(loginPoll); refresh(); });

function segmented() {
  const seg = (p, label, color) => h("button", { type: "button", class: addProvider === p ? "on" : "", onclick: () => { addProvider = p; renderAddForm(); } },
    h("span", { class: "dotc", style: `background:var(--${color})` }), label);
  return S.codex_enabled ? h("div", { class: "segmented" }, seg("claude", "Claude", "claude"), seg("codex", "ChatGPT · Codex", "codex")) : null;
}

function renderAddForm() {
  $("#add-title").textContent = "Add an account";
  $("#add-sub").textContent = "Each account has its own quotas; the order defines priority.";
  const name = h("input", { class: "input", placeholder: addProvider === "claude" ? "ex. Perso, Pro…" : "ex. ChatGPT perso", maxlength: 40 });
  const cancel = h("button", { class: "btn ghost", onclick: () => $("#dlg-add").close() }, "Cancel");
  if (addProvider === "claude") {
    const tok = h("input", { class: "input mono", type: "password", placeholder: "sk-ant-oat01-…", autocomplete: "off" });
    const out = h("div");
    const submit = h("button", { class: "btn primary", onclick: async () => {
      if (!tok.value.trim()) return tok.focus();
      submit.disabled = true; submit.textContent = "Adding and testing…";
      try {
        const r = await call("/admin/api/accounts", "POST", { provider: "claude", label: name.value, token: tok.value.trim() });
        if (r.test.ok) { success(`${r.account.label} is ready`, `Test response in ${r.test.seconds} s.`); }
        else { out.replaceChildren(h("div", { class: "result bad" }, `Account added, but the test failed: ${r.test.error}`)); submit.textContent = "Close"; submit.disabled = false; submit.onclick = () => $("#dlg-add").close(); }
      } catch (e) { out.replaceChildren(h("div", { class: "result bad" }, e.message)); submit.disabled = false; submit.textContent = "Add"; }
    } }, "Add");
    $("#add-body").replaceChildren(segmented(),
      h("ol", { class: "steps" },
        h("li", {}, "On a machine signed into this account, generate a long-lived token:",
          h("div", { class: "cmd" }, h("code", {}, "claude setup-token"), h("button", { class: "btn ghost icon", type: "button", title: "Copy", onclick: () => copy("claude setup-token") }, icon("copy")))),
        h("li", {}, "Paste the displayed token (it starts with sk-ant-):")),
      h("label", { class: "field" }, h("span", {}, "Token"), tok),
      h("label", { class: "field" }, h("span", {}, "Account name"), name),
      out);
    $("#add-foot").replaceChildren(cancel, submit);
    setTimeout(() => tok.focus(), 50);
  } else {
    const submit = h("button", { class: "btn primary", onclick: async () => {
      submit.disabled = true; submit.textContent = "Preparing…";
      try {
        const r = await call("/admin/api/accounts", "POST", { provider: "codex", label: name.value });
        deviceStep(r.account, r.login);
      } catch (e) { toast(e.message, true); submit.disabled = false; submit.textContent = "Continue"; }
    } }, "Continue");
    $("#add-body").replaceChildren(segmented(),
      h("ol", { class: "steps" },
        h("li", {}, "Give the account a name."),
        h("li", {}, "The server displays a one-time code."),
        h("li", {}, "Open the ChatGPT login page and enter this code; the server obtains its own credentials.")),
      h("label", { class: "field" }, h("span", {}, "Account name"), name));
    $("#add-foot").replaceChildren(cancel, submit);
    setTimeout(() => name.focus(), 50);
  }
}

function deviceStep(account, login) {
  const url = login.verificationUrl || login.verificationUri || "https://auth.openai.com/codex/device";
  $("#add-title").textContent = `Connect ${account.label}`;
  $("#add-sub").textContent = "Complete sign-in in your browser; this window updates automatically.";
  $("#add-body").replaceChildren(h("div", { class: "device" },
    h("a", { class: "btn primary", href: url, target: "_blank", rel: "noopener" }, icon("link"), "Open login page"),
    h("div", { class: "code" }, login.userCode || "????"),
    h("button", { class: "btn sm", onclick: () => copy(login.userCode, "Code copied") }, icon("copy"), "Copy code"),
    h("div", { class: "waiting" }, h("span", { class: "spin" }), "Waiting for approval…")));
  $("#add-foot").replaceChildren(h("button", { class: "btn ghost", onclick: () => $("#dlg-add").close() }, "Finish later"));
  const t0 = Date.now();
  clearInterval(loginPoll);
  loginPoll = setInterval(async () => {
    const s = await call("/admin/api/state").catch(() => null);
    const acc = s?.accounts.codex.find(a => a.id === account.id);
    if (acc?.identity) {
      clearInterval(loginPoll);
      S = s; render();
      success(`${acc.label} is connected`, `${acc.identity.email || ""}${acc.identity.planType ? " · forfait " + acc.identity.planType : ""}`);
    } else if (Date.now() - t0 > 15 * 60 * 1000) {
      clearInterval(loginPoll);
      $("#add-body").replaceChildren(h("div", { class: "result bad" }, "The code has expired. Restart sign-in from the account menu."));
    }
  }, 3000);
}

async function codexLogin(a) {
  addProvider = "codex";
  $("#add-body").replaceChildren(h("div", { class: "waiting" }, h("span", { class: "spin" }), "Preparing code…"));
  $("#add-foot").replaceChildren();
  $("#dlg-add").showModal();
  try { deviceStep(a, await call(`/admin/api/accounts/${a.id}/login`, "POST")); }
  catch (e) { $("#dlg-add").close(); toast(e.message, true); }
}

function success(title, sub) {
  $("#add-body").replaceChildren(h("div", { class: "success-big" },
    h("div", { class: "ring" }, icon("check")), h("h3", { style: "margin:0" }, title), h("p", { style: "color:var(--text-2);margin:6px 0 0" }, sub)));
  $("#add-foot").replaceChildren(h("button", { class: "btn primary", onclick: () => $("#dlg-add").close() }, "Done"));
  refresh();
}

// ---------- keys ----------
function renderKeys() {
  const box = $("#keys");
  if (!S.keys.length) return box.replaceChildren(h("div", { class: "keys-empty" }, "No keys yet. Create one for each client application."));
  const head = h("div", { class: "key-row head" }, h("span", {}, "Name"), h("span", {}, "Key"), h("span", {}, "Created"), h("span", {}, "Last used"), h("span", {}, "Req."), h("span"));
  box.replaceChildren(head, ...S.keys.slice().reverse().map(k => h("div", { class: `key-row ${k.revoked ? "revoked" : ""}` },
    h("span", { class: "name" }, k.name),
    h("span", { class: "mono muted" }, k.prefix),
    h("span", { class: "muted" }, fmtDate(k.created)),
    h("span", { class: "muted" }, ago(k.last_used)),
    h("span", {}, k.requests),
    h("span", { class: "actions" }, k.revoked
      ? h("button", { class: "btn ghost sm", onclick: () => act(() => call(`/admin/api/keys/${k.id}`, "DELETE")) }, "Remove")
      : h("button", { class: "btn sm danger", onclick: () => confirm(`Revoke ${k.name}? Clients using this key will be denied immediately.`) && act(() => call(`/admin/api/keys/${k.id}/revoke`, "POST"), "Key revoked") }, "Revoke")))));
}

let lastKey = null, snipTab = "python";
function snippet() {
  const base = `${location.origin}/v1`, k = lastKey?.key || "";
  return {
    python: `from openai import OpenAI\n\nclient = OpenAI(base_url="${base}", api_key="${k}")\nr = client.chat.completions.create(\n    model="sonnet",  # or gpt-5.6-sol, haiku…\n    messages=[{"role": "user", "content": "Hello!"}],\n)\nprint(r.choices[0].message.content)`,
    curl: `curl ${base}/chat/completions \\\n  -H "Authorization: Bearer ${k}" \\\n  -H "Content-Type: application/json" \\\n  -d '{"model": "sonnet", "messages": [{"role": "user", "content": "Hello!"}]}'`,
    owui: `Admin settings → Connections → OpenAI API\n  URL: ${base}\n  Key: ${k}`,
  }[snipTab];
}
$("#snip-tabs").onclick = e => {
  const t = e.target.dataset.t;
  if (!t) return;
  snipTab = t;
  document.querySelectorAll("#snip-tabs button").forEach(b => b.classList.toggle("on", b.dataset.t === t));
  $("#snip").textContent = snippet();
};
$("#key-form").onsubmit = async e => {
  e.preventDefault();
  try {
    const r = await call("/admin/api/keys", "POST", { name: $("#key-name").value });
    lastKey = { key: r.key, name: r.item.name };
    $("#key-name").value = "";
    $("#reveal").hidden = false;
    $("#reveal-name").textContent = r.item.name;
    $("#reveal-key").textContent = r.key;
    $("#snip").textContent = snippet();
    refresh();
  } catch (err) { toast(err.message, true); }
};
$("#copy-key").onclick = () => copy(lastKey?.key || "", "Key copied");

// ---------- cycle ----------
async function refresh() {
  try { S = await call("/admin/api/state"); } catch { return; }
  render();
}
async function start() {
  try { S = await call("/admin/api/state"); } catch { return showLogin(); }
  $("#login").hidden = true;
  $("#console").hidden = false;
  history.replaceState(null, "", "/admin");
  render();
  clearInterval(pollTimer);
  pollTimer = setInterval(() => { if (!document.hidden && !$("#dlg-add").open) refresh(); }, 15000);
}
start();
