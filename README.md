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

Two providers selected by model name:

| Models | Backend | Subscription |
|---|---|---|
| `opus`, `sonnet`, `haiku`, `fable`, `claude-*` | ephemeral `claude -p` | Claude (`claude login` / `claude setup-token`) |
| `gpt-6-astra`, `gpt-5.6-sol/terra/luna`, `gpt-5.5`… (catalog from Codex) | persistent `codex app-server`, ephemeral thread per request | ChatGPT (`codex login`) |

**Open WebUI names**: `/v1/models` prefixes display names with console accounts. “Mac · Sonnet” (`<account-id>/sonnet`) uses that account only; “Auto · Sonnet” (`sonnet`) selects the first available account with failover. IDs remain stable on rename. Disable entries with `REMOTE_MODELS_AUTO=0` or `REMOTE_MODELS_PER_ACCOUNT=0`.

Unknown OpenAI-style names (`gpt-4o`, `o3`, `codex`…) use the default Codex model (`REMOTE_CODEX_MODEL` or its advertised default); other names use `REMOTE_OAI_MODEL` (Claude, default `sonnet`). Codex replaces system instructions with `baseInstructions`, disables model tools and uses a read-only sandbox. Strict `json_schema` response formats are enforced through `outputSchema`.

**Codex PDF inputs** are converted with `pypdfium2`: extract text per page and render scanned/image pages as PNG (≤ 2048 pixels, `detail: high`). Configure `REMOTE_PDF_MODE` (`auto`, `images` for all pages, or `text`), `REMOTE_PDF_MAX_IMAGE_PAGES` (20) and `REMOTE_PDF_MAX_TEXT_CHARS` (400,000). Prefer `gpt-5.6-sol` or above for scans because `luna` makes OCR errors. Claude reads PDFs natively.

Each request launches an ephemeral `claude -p` process **without Claude Code tools**, using `--safe-mode` (no CLAUDE.md, skills or MCP) and the client system prompt. CLI limitations:
- around 2–4 seconds of startup time per request;
- function calls are prompt-emulated: the model emits `<tool_call>` blocks converted into `tool_calls`, without native API guarantees;
- `temperature`, `top_p`, `max_tokens`, `seed` and `logprobs` are ignored;
- the CLI adds a short Claude Agent SDK preamble to the system prompt.

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

## OKD deployment (gateway + Open WebUI + SSO)

On the cluster, the private `ghcr.io/als0m3/custom-remote` image serves only `/v1` and the **`/admin` console**; Claude Code sessions are disabled with `REMOTE_ENABLE_SESSIONS=0`.

- **`/admin` console** (OIDC SSO, administrators allowlisted by email or Keycloak group/role; master-token recovery):
  - **Multiple accounts per provider**, ordered by priority: Claude (paste and test a `claude setup-token` token) and ChatGPT/Codex (device-code login, isolated `CODEX_HOME` and app-server per account);
  - **Automatic failover**: quota/authentication failure before the first output pauses the account (until quota reset or 15 minutes, or 10 minutes for authentication) and retries the next account;
  - per-account quota meters, testing, activation, renaming, token replacement/reconnection and deletion; system accounts use host login;
  - `sk-cr-…` API keys stored as hashes, displayed once, with Python / curl / Open WebUI examples.
- **Open WebUI**: SSO-only login, new accounts awaiting approval, background tasks using `haiku`.

```bash
GHCR_TOKEN=<PAT-write:packages> ./deploy/build-push.sh           # build amd64 and push privately
cp deploy/okd/params.env.example deploy/okd/params.env           # hosts, issuer, client, administrators
OIDC_CLIENT_SECRET=… GHCR_PULL_TOKEN=<PAT read:packages> ./deploy/okd/deploy.sh
```

Create a confidential OIDC client with the standard flow and these redirect URIs:
`https://<CHAT_HOST>/oauth/oidc/callback` and `https://<API_HOST>/admin/auth/callback`.
