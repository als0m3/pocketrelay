import { changed, reconcile, cachedNode } from "./admin-view.js";
"use strict";
const desktop = window.webkit?.messageHandlers?.customremote;
const nativeAction = (action, extra = {}) => desktop?.postMessage({ action, ...extra });
if (desktop) {
  document.querySelector(".login-help").replaceChildren(
    Object.assign(document.createElement("summary"), { textContent: "Forgot your password?" }),
    Object.assign(document.createElement("button"), { className: "btn", textContent: "Change password on this Mac", onclick: () => nativeAction("resetPassword") })
  );
}

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
if (desktop) {
  $("#guide-account p").textContent = "Click Add. ChatGPT provides a code to enter in your browser. For Claude and Google, the sign-in button opens the setup assistant in Terminal automatically, with no commands to type.";
  const dataHelp = $("#guide-data").querySelectorAll("p");
  dataHelp[0].textContent = "Requests are sent to the selected provider. Your accounts and keys stay on this Mac, in Library → Application Support → PocketRelay. Open this folder from the app menu.";
  dataHelp[1].textContent = "Quit the app before backing up this folder. Replacing the app during an update preserves your accounts. Docker has its own separate data.";
  $("#guide-connection").querySelectorAll("p")[1].textContent = "The API is available on this Mac while Pocket Relay is open and the Mac is awake. Copy the address shown above. The app uses port 8788; Docker uses port 8787 by default.";
  $(".guide-grid").append(h("details", {}, h("summary", {}, "Does closing the window stop the API?"),
    h("p", {}, "No. The service keeps running in the menu bar. Choose Quit and stop the API to stop it. You can also enable Launch at login from that menu.")));
}
const icon = (name, cls = "i") => {
  const s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  s.setAttribute("class", cls);
  s.setAttribute("aria-hidden", "true");
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
async function copy(text, what = "Copied") {
  try {
    if (desktop) nativeAction("copy", { text: String(text) });
    else await navigator.clipboard.writeText(text);
    toast(what);
  } catch {
    toast("Automatic copying is unavailable: select the text and copy it manually.", true);
  }
}

async function call(path, method = "GET", body) {
  const r = await fetch(path, {
    method, credentials: "same-origin",
    signal: AbortSignal.timeout(path.includes("/test") || method !== "GET" ? 620000 : 30000),
    headers: { "X-Admin": "1", ...(body ? { "Content-Type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const d = await r.json().catch(() => ({}));
  if (r.status === 401 && !path.startsWith("/admin/auth/")) { await showLogin(); }
  if (!r.ok) {
    const message = r.status === 422 ? "Check the values you entered." : (typeof d.detail === "string" ? d.detail : r.statusText);
    throw Object.assign(new Error(message), { status: r.status });
  }
  return d;
}

let S = null;          // last received state
let pollTimer = null;
let generation = 0;
const accountNodes = new Map();

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
function forgetKey() {
  lastKey = null;
  $("#reveal").hidden = true;
  $("#reveal-key").textContent = "";
  $("#snip").textContent = "";
}

async function showLogin() {
  generation++;
  clearTimeout(pollTimer);
  clearInterval(loginPoll);
  accountNodes.clear();
  forgetKey();
  S = null;
  $("#console").hidden = true;
  document.querySelectorAll("dialog[open]").forEach(d => d.close());
  $("#loading").hidden = true;
  $("#login").hidden = false;
  try {
    const cfg = await call("/admin/auth/config");
    $("#oidc-btn").hidden = !cfg.oidc;
    $("#or").hidden = !cfg.oidc || !cfg.password_login;
    $("#password-form").hidden = !cfg.password_login;
    $("#token-help").hidden = !cfg.token_login;
    $("#setup-help").hidden = !cfg.setup_needed;
    $("#denied").hidden = true;
    const denied = new URLSearchParams(location.search).get("denied");
    if (denied) loginError(`Access denied for ${denied}: this account is not an administrator.`);
    if (cfg.password_login) $("#username").focus();
  } catch { loginError("Cannot reach the server. Refresh the page to try again."); }
}
function loginError(message) {
  $("#denied").textContent = message;
  $("#denied").hidden = false;
}
$("#oidc-btn").onclick = () => (location.href = "/admin/auth/login");
$("#show-password").onchange = e => { $("#password").type = e.target.checked ? "text" : "password"; };
async function submitLogin(event, path, body, button) {
  event.preventDefault();
  button.disabled = true;
  const original = button.textContent;
  button.textContent = "Signing in…";
  $("#denied").hidden = true;
  try {
    await call(path, "POST", body);
    $("#password").value = "";
    $("#master").value = "";
    $("#show-password").checked = false;
    $("#password").type = "password";
    await start();
  } catch (e) { loginError(e.message); }
  finally { button.disabled = false; button.textContent = original; }
}
$("#password-form").onsubmit = e => submitLogin(e, "/admin/auth/password", { username: $("#username").value.trim(), password: $("#password").value }, $("#password-btn"));
$("#token-form").onsubmit = e => submitLogin(e, "/admin/auth/token", { token: $("#master").value }, $("#master-btn"));
$("#logout").onclick = async () => {
  try { await call("/admin/auth/logout", "POST"); await showLogin(); }
  catch (e) { toast("Could not sign out: " + e.message, true); }
};

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
  $("#antigravity-col").hidden = !s.antigravity_enabled;
  if (s.antigravity_enabled) renderAccounts("antigravity");
  renderKeys();
  renderOnboarding();
  renderConnection();
  $("#api-docs").hidden = !s.docs_enabled;
  $("#routing-help").textContent = s.require_account
    ? "Each model targets a specific account. Add an account, then test its connection."
    : "Models without an account use priority order. Account/model identifiers stay tied to that account.";
}

function renderKpis() {
  if (!changed($("#kpis"), [S.stats, Object.values(S.accounts).flat().map(a => [a.id, a.enabled, a.status]), S.keys.map(k => [k.id, k.revoked])])) return;
  const all = [...S.accounts.claude, ...(S.codex_enabled ? S.accounts.codex : []),
               ...(S.antigravity_enabled ? S.accounts.antigravity : [])];
  const ready = all.filter(a => a.enabled && a.status === "ok").length;
  const activeKeys = S.keys.filter(k => !k.revoked).length;
  const kpi = (ic, lbl, val, sub) => h("div", { class: "kpi" },
    h("div", { class: "lbl" }, icon(ic), lbl), h("div", { class: "val" }, val, sub ? h("small", {}, " " + sub) : null));
  $("#kpis").replaceChildren(
    kpi("bolt", "Requests", num(S.stats.requests), S.stats.errors ? `· ${S.stats.errors} errors` : ""),
    kpi("chart", "Generated tokens", num(S.stats.output_tokens)),
    kpi("users", "Accounts tested / used", ready, `/ ${all.length}`),
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
  if (a.provider === "antigravity") {
    return a.system ? "This server's antigravity session" : "Server-side Google session dedicated to this account";
  }
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
    if (!changed(box, [])) return;
    accountNodes.delete(provider);
    const hints = { claude: "Connect your subscription with a Claude token.", codex: "Enter a code in your browser to connect ChatGPT.", antigravity: desktop ? "The app guides you through connecting Google." : "Connect Google from the server terminal." };
    box.replaceChildren(h("div", { class: "empty-accounts" }, h("strong", {}, "Your first account starts here"),
      h("p", {}, hints[provider]), h("button", { class: "btn", onclick: () => openAdd(provider) }, "Connect an account")));
    return;
  }
  changed(box, list.map(a => a.id));
  let cache = accountNodes.get(provider);
  if (!cache) accountNodes.set(provider, cache = new Map());
  for (const id of cache.keys()) if (!list.some(a => a.id === id)) cache.delete(id);
  reconcile(box, list.map((a, i) => cachedNode(cache, a.id, [a, i, list.length, S.models[provider], Math.floor(Date.now() / 60000)], () => {
    const [cls, label] = STATUS[a.status] || ["unknown", a.status];
    const pill = h("span", { class: `pill ${cls}` }, a.status === "paused" && a.paused_until ? `${label} · resumes ${fmtTime(a.paused_until)}` : label);
    const toggle = h("label", { class: "switch", title: a.enabled ? "Disable" : "Enable" },
      h("input", { type: "checkbox", "aria-label": `Enable account ${a.label}`, checked: a.enabled, onchange: e => act(() => call(`/admin/api/accounts/${a.id}`, "PATCH", { enabled: e.target.checked })) }),
      h("span"));
    const notes = [];
    if (a.status === "paused") notes.push(h("div", { class: "acc-note warn" }, `Paused (${a.pause_reason || "quota"}). Try again after it resumes or choose another account.`));
    else if (a.last_error && a.enabled) notes.push(h("div", { class: "acc-note bad" }, a.last_error.message));
    const needsLogin = a.enabled && a.provider === "codex" && !a.identity;
    return h("div", { class: `acc ${a.enabled ? "" : "disabled"}` },
      h("div", { class: "acc-top" },
        h("span", { class: "prio", title: "Priority" }, i + 1),
        h("span", { class: "acc-name" }, a.label),
        a.system ? h("span", { class: "pill sys", title: "Server-created account: uses the host machine's login" }, "system") : null,
        pill,
        h("span", { class: "spacer" }),
        toggle),
      h("div", { class: "acc-id" }, identity(a)),
      meters(a),
      accModels(a),
      notes,
      h("div", { class: "acc-foot" },
        h("span", { class: "stat" }, `${a.requests || 0} request${(a.requests || 0) !== 1 ? "s" : ""} · ${ago(a.last_used)}`),
        needsLogin ? h("button", { class: "btn sm primary", onclick: () => codexLogin(a) }, icon("link"), "Connect") : null,
        h("button", { class: "btn sm", onclick: e => testAccount(a, e.currentTarget) }, icon("play"), "Test"),
        h("button", { class: "btn ghost icon", title: "Move up", disabled: i === 0, onclick: () => act(() => call(`/admin/api/accounts/${a.id}/move`, "POST", { delta: -1 })) }, icon("up")),
        h("button", { class: "btn ghost icon", title: "Move down", disabled: i === list.length - 1, onclick: () => act(() => call(`/admin/api/accounts/${a.id}/move`, "POST", { delta: 1 })) }, icon("down")),
        h("button", { class: "btn ghost icon", title: "More", onclick: e => accountMenu(a, e.currentTarget) }, icon("dots"))));
  })));
}

// Names targeting this account: <account>/<model>, prefixed with openai/ for LiteLLM.
function accModels(a) {
  const list = (S.models || {})[a.provider] || [];
  if (!list.length) return null;
  const row = m => {
    const name = `${a.slug || a.id}/${m.id}`;
    return h("div", { class: "acc-model" },
      h("span", { class: "mname" }, m.name),
      h("code", { class: "mono" }, name),
      h("button", { class: "btn ghost icon", title: `Copy model: ${name}`,
        onclick: () => copy(name, "Model name copied") }, icon("copy")));
  };
  const details = h("details", { class: "acc-models", "data-account": a.id },
    h("summary", {}, `Models for this account (${list.length})`),
    h("p", { class: "hint" }, "Copy this name into your application. For LiteLLM, add the prefix ",
      h("code", {}, "openai/"), "."),
    h("div", { class: "model-rows" }));
  details.addEventListener("toggle", () => {
    if (details.open && !details.dataset.loaded) {
      details.querySelector(".model-rows").replaceChildren(...list.map(row));
      details.dataset.loaded = "1";
    }
  });
  return details;
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
  if (a.provider === "antigravity") items.push(item("link", "Sign in: view command", () => antigravityLogin(a)));
  if (a.status === "paused") items.push(item("refresh", "Resume now", () => act(() => call(`/admin/api/accounts/${a.id}/resume`, "POST"), "Account resumed")));
  if (a.provider === "antigravity" && !a.system) items.push(item("logout", "Disconnect", () => {
    if (confirm(`Delete the Google session for ${a.label} on this server?`)) act(() => call(`/admin/api/accounts/${a.id}/logout`, "POST"), "Disconnected");
  }));
  if (a.provider === "codex" && a.identity) items.push(item("logout", "Disconnect", () => {
    if (confirm(`Disconnect ${a.identity.email || a.label} from this server?`)) act(() => call(`/admin/api/accounts/${a.id}/logout`, "POST"), "Disconnected");
  }));
  if (!a.system || S.deletable_system?.includes(a.provider)) {
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
  const tabs = [seg("claude", "Claude", "claude")];
  if (S.codex_enabled) tabs.push(seg("codex", "ChatGPT · Codex", "codex"));
  if (S.antigravity_enabled) tabs.push(seg("antigravity", "Google · Antigravity", "antigravity"));
  return tabs.length > 1 ? h("div", { class: "segmented" }, tabs) : null;
}

function renderAddForm() {
  $("#add-title").textContent = "Add an account";
  $("#add-sub").textContent = "Connect a subscription, then use the models linked to that account.";
  const name = h("input", { class: "input", placeholder: addProvider === "claude" ? "e.g. Personal, Work…" : "e.g. Personal ChatGPT", maxlength: 40 });
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
        h("li", {}, desktop ? "Open the Claude setup assistant, then sign in through your browser:" : "Generate a token from a terminal in the project directory:",
          desktop ? h("button", { class: "btn", type: "button", onclick: () => nativeAction("claudeToken") }, "Connect Claude on this Mac") :
          h("div", { class: "cmd" }, h("code", {}, "docker compose exec customremote claude setup-token"), h("button", { class: "btn ghost icon", type: "button", title: "Copy", onclick: () => copy("docker compose exec customremote claude setup-token") }, icon("copy"))),
          h("p", { class: "hint" }, desktop ? "Terminal opens automatically. Copy the generated token and paste it below." : "Without Docker, on a machine with the CLI installed: claude setup-token.")),
        h("li", {}, "Paste the displayed token (it starts with sk-ant-):")),
      h("label", { class: "field" }, h("span", {}, "Token"), tok),
      h("label", { class: "field" }, h("span", {}, "Account name"), name),
      out);
    $("#add-foot").replaceChildren(cancel, submit);
    setTimeout(() => tok.focus(), 50);
  } else if (addProvider === "antigravity") {
    const submit = h("button", { class: "btn primary", onclick: async () => {
      submit.disabled = true; submit.textContent = "Creating…";
      try {
        const r = await call("/admin/api/accounts", "POST", { provider: "antigravity", label: name.value });
        commandStep(r.account, r.login.command);
      } catch (e) { toast(e.message, true); submit.disabled = false; submit.textContent = "Continue"; }
    } }, "Continue");
    $("#add-body").replaceChildren(segmented(),
      h("ol", { class: "steps" },
        h("li", {}, "Name the account: it gets its own state directory on the server."),
        h("li", {}, desktop ? "The app will open Google setup in Terminal, with no commands to type." : "Google CLI login requires a terminal: the server will provide the command to run."),
        h("li", {}, "The CLI displays a URL. Approve sign-in in your browser, then paste the code back.")),
      h("label", { class: "field" }, h("span", {}, "Account name"), name));
    $("#add-foot").replaceChildren(cancel, submit);
    setTimeout(() => name.focus(), 50);
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
  let checking = false;
  loginPoll = setInterval(async () => {
    if (checking || document.hidden) return;
    checking = true;
    const s = await call("/admin/api/catalog?refresh=1").catch(() => null);
    checking = false;
    if (!$("#dlg-add").open || !S) return;
    const acc = s?.accounts.codex.find(a => a.id === account.id);
    if (acc?.identity) {
      clearInterval(loginPoll);
      S = s; render();
      success(`${acc.label} is connected`, `${acc.identity.email || ""}${acc.identity.planType ? " · plan " + acc.identity.planType : ""}`);
    } else if (Date.now() - t0 > 15 * 60 * 1000) {
      clearInterval(loginPoll);
      $("#add-body").replaceChildren(h("div", { class: "result bad" }, "The code has expired. Restart sign-in from the account menu."));
    }
  }, 3000);
}

function commandStep(account, command) {
  if (desktop) {
    $("#add-title").textContent = `Connect ${account.label}`;
    $("#add-sub").textContent = "Google setup opens in Terminal on your Mac.";
    $("#add-body").replaceChildren(
      h("p", {}, "Click below, follow the sign-in link, then return here. No commands to copy."),
      h("button", { class: "btn primary", onclick: () => nativeAction("googleLogin", { account: account.id }) }, "Connect Google on this Mac"),
      h("p", { class: "hint" }, "Once signed in, use Test on the account card."));
    $("#add-foot").replaceChildren(h("button", { class: "btn", onclick: () => $("#dlg-add").close() }, "Done"));
    return;
  }
  $("#add-title").textContent = `Connect ${account.label}`;
  $("#add-sub").textContent = "Run in a server terminal; the CLI displays a URL and waits for the code.";
  $("#add-body").replaceChildren(
    h("div", { class: "cmd" }, h("code", {}, command),
      h("button", { class: "btn ghost icon", type: "button", title: "Copy", onclick: () => copy(command) }, icon("copy"))),
    h("p", { class: "hint" }, "With Docker Compose, run this command from the project directory:"),
    h("div", { class: "cmd" }, h("code", {}, `docker compose exec customremote ${command}`),
      h("button", { class: "btn ghost icon", title: "Copy Docker command", onclick: () => copy(`docker compose exec customremote ${command}`) }, icon("copy"))),
    h("p", { style: "color:var(--text-2)" }, "Once signed in, return here and select Test."));
  $("#add-foot").replaceChildren(h("button", { class: "btn primary", onclick: () => $("#dlg-add").close() }, "Done"));
}

async function antigravityLogin(a) {
  try { commandStep(a, (await call(`/admin/api/accounts/${a.id}/login`, "POST")).command); $("#dlg-add").showModal(); }
  catch (e) { toast(e.message, true); }
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
  if (!changed(box, [S.keys, Math.floor(Date.now() / 60000)])) return;
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
function selectedModel() { return $("#model-select").value || "your-account/model"; }
const shellQuote = value => "'" + value.replaceAll("'", "'\\''") + "'";
function snippet() {
  const base = `${location.origin}/v1`, k = lastKey?.key || "YOUR_KEY", model = selectedModel();
  const payload = JSON.stringify({ model, messages: [{ role: "user", content: "Hello!" }] });
  return {
    python: `from openai import OpenAI\n\nclient = OpenAI(base_url=${JSON.stringify(base)}, api_key=${JSON.stringify(k)})\nr = client.chat.completions.create(\n    model=${JSON.stringify(model)},\n    messages=[{"role": "user", "content": "Hello!"}],\n)\nprint(r.choices[0].message.content)`,
    curl: `curl ${shellQuote(base + "/chat/completions")} \\\n  -H ${shellQuote("Authorization: Bearer " + k)} \\\n  -H 'Content-Type: application/json' \\\n  -d ${shellQuote(payload)}`,
    owui: `Admin settings → Connections → OpenAI API\n  URL: ${base}\n  Key: ${k}\n  Model: ${model}\n\nIn a container on the same Compose network:\n  URL: http://customremote:8787/v1`,
  }[snipTab];
}
$("#snip-tabs").onclick = e => {
  const t = e.target.dataset.t;
  if (!t) return;
  snipTab = t;
  document.querySelectorAll("#snip-tabs button").forEach(b => {
    b.classList.toggle("on", b.dataset.t === t);
    b.setAttribute("aria-pressed", String(b.dataset.t === t));
  });
  $("#snip").textContent = snippet();
};
$("#key-form").onsubmit = async e => {
  e.preventDefault();
  const button = e.currentTarget.querySelector("button");
  button.disabled = true;
  try {
    const r = await call("/admin/api/keys", "POST", { name: $("#key-name").value });
    lastKey = { key: r.key, name: r.item.name };
    $("#key-name").value = "";
    $("#reveal").hidden = false;
    $("#reveal-name").textContent = r.item.name;
    $("#reveal-key").textContent = r.key;
    $("#snip").textContent = snippet();
    $("#reveal").focus();
    refresh();
  } catch (err) { toast(err.message, true); }
  finally { button.disabled = false; }
};
$("#copy-key").onclick = () => copy(lastKey?.key || "", "Key copied");

$("#copy-snippet").onclick = () => copy(snippet(), "Example copied");
$("#hide-key").onclick = () => { forgetKey(); $("#key-name").focus(); };
$("#copy-url").onclick = () => copy($("#connection-url").value, "Address copied");
$("#copy-model").onclick = () => copy(selectedModel(), "Model copied");
$("#model-select").onchange = () => {
  $("#model-help").textContent = `Exact name: ${selectedModel()}. Test the account to check availability.`;
  if (lastKey) $("#snip").textContent = snippet();
};

function renderOnboarding() {
  if (!changed($("#onboarding"), [Object.values(S.accounts).flat().map(a => [a.enabled, a.status, Boolean(a.identity)]), S.keys.map(k => [k.revoked, k.requests > 0])])) return;
  const all = Object.values(S.accounts).flat();
  const added = all.some(a => a.enabled && (a.status === "ok" || a.identity));
  const hasAccount = all.some(a => a.enabled);
  const key = S.keys.some(k => !k.revoked);
  const used = S.keys.some(k => k.requests > 0);
  const steps = [
    { done: added, title: "Connect an account", text: "Claude, ChatGPT or Google: one is enough.", action: added ? "Manage accounts" : hasAccount ? "Test my account" : "Choose a provider", target: "accounts-section" },
    { done: key, title: "Create an API key", text: "A dedicated key for each application.", action: key ? "Manage keys" : "Create my first key", target: "keys-section" },
    { done: used, title: "Connect an application", text: "Copy the settings into your application.", action: "View settings", target: "connect-section" },
  ];
  $("#setup-progress").textContent = `${steps.filter(s => s.done).length} / 3 steps completed`;
  $("#onboarding").replaceChildren(...steps.map((s, i) => h("a", { class: `onboarding-step ${s.done ? "done" : ""}`, href: `#${s.target}` },
    h("span", { class: "step-number", "aria-label": s.done ? "Step completed" : `Step ${i + 1}` }, s.done ? icon("check") : i + 1),
    h("div", {}, h("strong", {}, s.title), h("p", {}, s.text), h("span", { class: "step-action" }, s.action + " →")))));
}
function renderConnection() {
  $("#connection-url").value = `${location.origin}/v1`;
  const select = $("#model-select"), previous = select.value;
  const choices = Object.entries(S.accounts).flatMap(([provider, list]) => list.filter(a => a.enabled).flatMap(a =>
    (S.models[provider] || []).map(m => ({ id: `${a.slug || a.id}/${m.id}`, name: `${a.label} · ${m.name}` }))));
  if (!changed(select, choices)) return;
  select.replaceChildren(...(choices.length ? choices.map(m => h("option", { value: m.id }, m.name)) : [h("option", { value: "" }, "Add an account first")]));
  if (choices.some(m => m.id === previous)) select.value = previous;
  select.disabled = !choices.length;
  $("#copy-model").disabled = !choices.length;
  $("#model-help").textContent = choices.length ? `Exact name: ${select.value}. Test the account to check availability.` : "Add an account to see its models. The ChatGPT list may take a few seconds to appear after sign-in.";
  if (lastKey) $("#snip").textContent = snippet();
}

// ---------- cycle ----------
let refreshing = false;
async function refresh() {
  if (refreshing || !S) return;
  refreshing = true;
  const current = generation;
  try {
    const state = await call("/admin/api/state");
    if (!S || current !== generation) return;
    const open = [...document.querySelectorAll(".acc-models[open]")].map(d => d.dataset.account);
    S = state;
    render();
    document.querySelectorAll(".acc-models").forEach(d => { d.open = open.includes(d.dataset.account); });
    $("#connection-error").hidden = true;
  } catch (e) {
    if (e.status !== 401) {
      $("#connection-error").textContent = "Could not refresh. Displayed data may be outdated. Retrying automatically in a few seconds.";
      $("#connection-error").hidden = false;
    }
  } finally { refreshing = false; }
}
let catalogAt = 0, catalogLoading = false;
async function refreshCatalog() {
  if (!S || catalogLoading || Date.now() - catalogAt < 60000) return;
  catalogLoading = true;
  const current = generation;
  try {
    const data = await call("/admin/api/catalog");
    if (!S || current !== generation) return;
    // Refresh promptly to avoid restoring stale counters after a slow call.
    catalogAt = Date.now();
    await refresh();
  } catch (e) { if (e.status !== 401) console.debug("Model catalog temporarily unavailable"); }
  finally { catalogLoading = false; }
}
function schedulePoll() {
  clearTimeout(pollTimer);
  if (!S || document.hidden) return;
  pollTimer = setTimeout(async () => {
    if (!$("#dlg-add").open && !$("#dlg-prompt").open && !$(".menu")) {
      await refresh();
      refreshCatalog();
    }
    schedulePoll();
  }, 15000);
}
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && S) { refresh(); refreshCatalog(); }
  schedulePoll();
});
async function start() {
  try { S = await call("/admin/api/state"); }
  catch (e) {
    if (e.status === 401) return;
    $("#loading").hidden = false;
    $("#loading p").textContent = "Cannot load the console. Check that the server is running.";
    $("#retry").hidden = false;
    $("#login").hidden = true;
    return;
  }
  $("#loading").hidden = true;
  $("#login").hidden = true;
  $("#console").hidden = false;
  history.replaceState(null, "", "/admin");
  render();
  generation++;
  catalogAt = 0;
  refreshCatalog();
  schedulePoll();
}
$("#retry").onclick = () => start();
start();
