> Archive of the previous Python version. Private information was replaced with examples during import; use the current branch to install PocketRelay.

# CustomRemote

Personal remote for Claude Code: a local server controlling the **`claude` CLI**
(using your subscription through `claude login`) and exposing an **HTTP API and web UI**.

```
browser ──HTTP/SSE──▶ FastAPI (127.0.0.1:8787) ──stdin/stdout stream-json──▶ claude -p (1 process / session)
```

## Run

```bash
./run.sh
# → prints http://localhost:8787/#token=… (open this link once to save the token)
```

Requirements: `uv` and `claude` signed into your subscription (`claude login`).
`ANTHROPIC_API_KEY` is removed from the CLI environment to use the subscription (`REMOTE_KEEP_API_KEY=1` retains it).

## Features

- **Multiple concurrent sessions**, persisted in `data/` and automatically resumed with `--resume` after process or server restarts.
- **Browser permissions**: Allow / Always (tool) / Deny with instructions, plus a banner and desktop notification when Claude is waiting.
- **Token streaming**, Markdown rendering and collapsible tools (Edit diffs, TodoWrite checklists, Bash output…).
- **Resume any CLI / VS Code session** from `~/.claude/projects`, optionally as a fork; copy a terminal command to continue a web session in the CLI.
- Change **model / permission mode** live; changing effort or system instructions restarts the process while preserving the conversation.
- **Subscription quota meters** (5-hour / weekly windows), estimated equivalent API cost and context size.
- Autocomplete installed **slash commands** (skills, plugins…), prompt history (↑), session drafts, pasted/dropped images and **snippets**.
- Shortcuts: `Enter` sends, `Shift+Enter` adds a line, `Escape` interrupts, `⌘K` searches and `⌘J` creates a session.

## API

Interactive docs: http://localhost:8787/docs. Authentication: `Authorization: Bearer $(cat data/token)` (or `?token=` for SSE).

| Method | Route | Purpose |
|---|---|---|
| GET | `/api/sessions` | list |
| POST | `/api/sessions` | create `{cwd, model?, permission_mode?, effort?, resume?, fork?}` |
| POST | `/api/sessions/{id}/messages` | send `{text, images?}` |
| GET | `/api/sessions/{id}/stream?since=N` | SSE event stream |
| POST | `/api/sessions/{id}/permissions/{req}` | `{behavior: allow\|deny, always?, message?}` |
| POST | `/api/sessions/{id}/interrupt` · `/stop` | interrupt the turn · stop the process |
| PATCH | `/api/sessions/{id}` | `{name, model, permission_mode, effort, pinned…}` |
| GET | `/api/history` | resumable CLI sessions |
| GET | `/api/usage` | quotas and costs |
| POST | `/api/run` | synchronous one-shot `{prompt, cwd, model?}` (default `dontAsk` mode) |

```bash
T=$(cat data/token)
curl -s localhost:8787/api/run -H "Authorization: Bearer $T" -H 'Content-Type: application/json' \
  -d '{"prompt":"Summarize the README","cwd":"~/projects/customremote"}' | jq -r .result
```

## OpenAI-compatible API (`/v1`)

Use any OpenAI client: base URL `http://localhost:8787/v1`, API key from `data/token`.

```python
from openai import OpenAI
client = OpenAI(base_url="http://localhost:8787/v1", api_key=open("data/token").read().strip())
client.chat.completions.create(model="sonnet", messages=[{"role": "user", "content": "Hello"}])
```

| Endpoint | Supported features |
|---|---|
| `GET /v1/models`, `/v1/models/{id}` | ✓ |
| `POST /v1/chat/completions` | multi-turn messages, system/developer, `stream` + `stream_options.include_usage`, images (data URL or HTTP), PDF (`file`), **tools / tool_choice / parallel_tool_calls** (plus legacy `functions`), `response_format` json_object / json_schema, `stop`, `n`, `reasoning_effort` |
| `POST /v1/responses` | text or item `input`, `instructions`, `previous_response_id` (server memory), function tools and `function_call_output`, `text.format`, `reasoning.effort`, streaming with official event types |
| `POST /v1/completions` | legacy API (prompt, stop, echo, stream) |
| embeddings, audio, images, files… | 404 with an OpenAI-style error |

Three providers selected by model name:

| Models | Backend | Subscription |
|---|---|---|
| `opus`, `sonnet`, `haiku`, `fable`, `claude-*` | ephemeral `claude -p` | Claude (`claude login` / `claude setup-token`) |
| `gpt-6-astra`, `gpt-5.6-sol/terra/luna`, `gpt-5.5`… (catalog from Codex) | persistent `codex app-server`, ephemeral thread per request | ChatGPT (`codex login`) |
| `gemini-3-pro`, `gemini-3-flash`, `gemini-*`, `gemma-*`, `gpt-oss-*` | ephemeral `antigravity -p --output-format stream-json` | Google AI (Antigravity CLI Google login) |

**Open WebUI names**: `/v1/models` prefixes display names with console accounts. “Mac · Sonnet” (`<account-id>/sonnet`) uses that account only; “Auto · Sonnet” (`sonnet`) selects the first available account with failover. IDs remain stable on rename. Disable entries with `REMOTE_MODELS_AUTO=0` or `REMOTE_MODELS_PER_ACCOUNT=0`.

`gemini-*`, `gemma-*` and `gpt-oss-*` names use Antigravity CLI and fail explicitly when it is missing. Unknown OpenAI-style names use the default Codex model (`REMOTE_CODEX_MODEL` or the advertised default); other names use `REMOTE_OAI_MODEL` (Claude, default `sonnet`). Codex uses `baseInstructions`, disabled tools, a read-only sandbox and native strict `json_schema` enforcement through `outputSchema`.

**Codex PDF inputs** are converted with `pypdfium2`: extract text per page and render scanned/image pages as PNG (≤ 2048 pixels, `detail: high`). Configure `REMOTE_PDF_MODE` (`auto`, `images` for all pages, or `text`), `REMOTE_PDF_MAX_IMAGE_PAGES` (20) and `REMOTE_PDF_MAX_TEXT_CHARS` (400,000). Prefer `gpt-5.6-sol` or above for scans because `luna` makes OCR errors. Claude reads PDFs natively.

Each request launches an ephemeral `claude -p` process **without Claude Code tools**, using `--safe-mode` (no CLAUDE.md, skills or MCP) and the client system prompt. CLI limitations:
- around 2–4 seconds of startup time per request;
- function calls are prompt-emulated: the model emits `<tool_call>` blocks converted into `tool_calls`, without native API guarantees;
- `temperature`, `top_p`, `max_tokens`, `seed` and `logprobs` are ignored;
- the CLI adds a short Claude Agent SDK preamble to the system prompt.

**Antigravity** launches an ephemeral `antigravity -p` process in an empty working directory. It maps `reasoning_effort` to `--effort` and response formats to native `--json-schema`, with account state and Google sessions isolated in `HOME`. API-key variables are removed to use the subscription. The CLI cannot replace system instructions, so client instructions are prepended in `<system_instructions>` tags. Images are unsupported; PDFs become text with scanned pages omitted. CLI tools remain available, but the working directory is temporary and headless approval requests are rejected. Configure `REMOTE_ENABLE_ANTIGRAVITY`, `REMOTE_ANTIGRAVITY_MODELS`, `REMOTE_ANTIGRAVITY_MODEL`, `REMOTE_ANTIGRAVITY_FAST_MODEL`, `ANTIGRAVITY_BIN` and system-account `ANTIGRAVITY_HOME`. Run `antigravity models` after login for exact model slugs.

`REMOTE_OAI_DEBUG=1` logs unrecognized tool output.

### With Open WebUI (Docker)

```bash
docker run -d --name open-webui-customremote -p 3000:8080 \
  -e OPENAI_API_BASE_URL=http://host.docker.internal:8787/v1 \
  -e OPENAI_API_KEY="$(cat data/token)" \
  -e ENABLE_OLLAMA_API=False -e WEBUI_AUTH=False \
  -v open-webui-customremote:/app/backend/data \
  ghcr.io/open-webui/open-webui:main
# → http://localhost:3000 (stop with: docker stop open-webui-customremote)
```

`host.docker.internal` is allowed by default.

## Config (env)

`REMOTE_PORT` (8787) · `REMOTE_HOST` (127.0.0.1) · `REMOTE_TOKEN` · `REMOTE_ALLOWED_HOSTS` (additional hosts, such as a Tailscale name) · `CLAUDE_BIN` · `REMOTE_DATA`.

## Security

The server can execute code on your machine through Claude. It binds to `127.0.0.1`, requires a token and rejects unknown `Host` headers to prevent DNS rebinding. For phone access, prefer Tailscale (`REMOTE_HOST=<tailscale-ip>` and `REMOTE_ALLOWED_HOSTS`) over public exposure.

### Antigravity accounts

Antigravity replaced Gemini CLI for individual accounts in June 2026. Google
login requires a **controlling terminal** (`/dev/tty`), so the console cannot drive it,
and keychain-backed tokens cannot simply be copied from another machine.
Create the account in `/admin`, which displays the server command to run:

```bash
oc -n custom-remote exec -it deploy/claude-api -- \
  env HOME=/data/accounts/<account-id>/antigravity antigravity
# Open the CLI URL in your browser, authorize access and paste the code back.
```

`-it` is required: login fails without a terminal. The system account uses
`ANTIGRAVITY_HOME` (`/data/antigravity`) on the volume to survive restarts.

## OKD deployment (gateway + Open WebUI + LiteLLM + SSO)

On the cluster, the private `ghcr.io/als0m3/custom-remote` image serves only `/v1` and the **`/admin` console**; Claude Code sessions are disabled with `REMOTE_ENABLE_SESSIONS=0`.

- **`/admin` console** (OIDC SSO, administrators allowlisted by email or Keycloak group/role; master-token recovery):
  - **Multiple accounts per provider**, ordered by priority: tested Claude setup tokens, Codex device-code login with isolated `CODEX_HOME`/app-server, and Antigravity server-terminal login with isolated `HOME`;
  - **Automatic failover**: quota/authentication failure before the first output pauses the account (until quota reset or 15 minutes, or 10 minutes for authentication) and retries the next account;
  - per-account quotas, testing, activation, renaming, token replacement/reconnection and deletion. System accounts use host login; `REMOTE_SYSTEM_ACCOUNTS` (`SYSTEM_ACCOUNTS`) selects maintained providers, empty for none, defaulting to `antigravity` on the cluster. Excluded accounts are not recreated and can be deleted;
  - `sk-cr-…` API keys stored as hashes, displayed once, with Python / curl / Open WebUI examples.
- **Open WebUI**: SSO-only login, new accounts awaiting approval, background tasks using `haiku`.
- **LiteLLM** runs alongside Open WebUI as another `/v1` client, with its own revocable `sk-cr` key and the same usage limits. It serves `opus` / `sonnet` / `haiku` / `fable` with virtual keys and team budgets, and can connect external providers such as OpenAI, Gemini or Mistral.

| Public hostname | Service | Internal route |
|---|---|---|
| `relay.example.test` | LiteLLM (API + SSO console) | `relay-llm.apps.cluster.example.test` |
| `relay-chat.example.test` | Open WebUI | `relay.apps.cluster.example.test` |
| `relay-api.example.test` | console `/admin` + `/v1` | `relay-api.apps.cluster.example.test` |

Internal names remain unchanged because changing an OKD route hostname requires recreation. Map public names in `deploy/vps/relay.conf`.

```bash
GHCR_TOKEN=<PAT-write:packages> ./deploy/build-push.sh           # build amd64 and push privately
cp deploy/okd/params.env.example deploy/okd/params.env           # hosts, issuer, client, administrators
OIDC_CLIENT_SECRET=… GHCR_PULL_TOKEN=<PAT read:packages> ./deploy/okd/deploy.sh
```

Then configure the VPS reverse proxy after pointing all three DNS names to it;
the certificate is expanded when required:

```bash
./deploy/vps/install.sh root@203.0.113.10
```

Create a confidential OIDC client with the standard flow and these redirect URIs:
`https://<PUBLIC_CHAT_HOST>/oauth/oidc/callback`, `https://<PUBLIC_API_HOST>/admin/auth/callback`
and `https://<PUBLIC_LITELLM_HOST>/sso/callback`.

### LiteLLM

`https://relay.example.test` serves the LiteLLM API with virtual keys and its console
on `/ui`, **sign in through Keycloak SSO**, using the same client as `/admin` and Open WebUI.
`ADMIN_ROLE` → `proxy_admin`, `ACCESS_ROLE` → `internal_user`; no matching role grants read-only access.
Register this redirect URI: `https://<PUBLIC_LITELLM_HOST>/sso/callback`.

The Keycloak client requires PKCE: `GENERIC_CLIENT_USE_PKCE=true`. Without Redis,
LiteLLM keeps `code_verifier` in pod memory; use one replica with `Recreate`.
A restart during login invalidates that attempt; simply start again.

LiteLLM SSO is free for **up to five database users**. A sixth account requires
an enterprise license to sign in. This is an administration console,
while chat has no corresponding account-count limit.

The username/password form is disabled: `disable_password_login_when_sso_enabled`
makes `/login` return 403 and sends `/ui` directly to Keycloak (`AUTO_REDIRECT_UI_LOGIN_TO_SSO`).
`relay.conf` additionally returns 404 for public /login requests.

If Keycloak is unavailable, console access also fails. The master key remains
a valid **API key** for managing keys, budgets and models:

```bash
KEY=$(oc -n custom-remote get secret custom-remote -o jsonpath='{.data.litellm-master-key}' | base64 -d)
curl -s https://relay.example.test/v1/models -H "Authorization: Bearer $KEY"
```

To reopen the console during a prolonged SSO outage, remove `disable_password_login_when_sso_enabled`
from the `litellm-config` ConfigMap and restart `deploy/litellm`. This is an explicit
configuration change visible in the repository.

The `litellm-config` ConfigMap declares no models. Add them through the UI
(*Models* tab), with database persistence (`STORE_MODEL_IN_DB`) and no redeployment.
For gateway models, reference pod environment variables instead of pasting the key:
*Provider* `OpenAI-Compatible`, *Model* `openai/<sonnet|opus|haiku|fable|gpt-…>`,
*API Base* `os.environ/CLAUDE_API_BASE`, *API Key* `os.environ/CLAUDE_API_KEY`. LiteLLM encrypts
the stored value and resolves the reference at request time, keeping `sk-cr` out of configuration. State lives in PostgreSQL
(`litellm-db`, 2 Gi PVC); `litellm-salt-key` encrypts provider keys in the database
and must never change.

To offer external models in chat, add an OpenAI connection in Open WebUI using
semicolon-separated `OPENAI_API_BASE_URLS` / `OPENAI_API_KEYS`, pointing to `http://litellm:4000/v1`.
Disable duplicate gateway models in LiteLLM so they do not appear twice.
