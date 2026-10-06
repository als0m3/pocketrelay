# How it works

```text
Client application ── API key ──> Rust server ──> Provider CLI ──> Remote service
                                      │
Browser / macOS app ── login ──> Web console
                                      │
                                  Local data
```

The Axum/Tokio server serves both the static console and the API. An authenticated request
selects the account specified by the model prefix. The server prepares the content, waits
for a concurrency slot, starts or reuses the appropriate CLI, and translates provider events
into JSON or SSE. Errors and quotas do not silently redirect a request to another account.

Claude receives JSON messages over stdin. Codex uses its app-server over JSON-RPC;
Antigravity uses its CLI. These interfaces and their terms can change independently of
PocketRelay. This project is not an inference server and contains no model weights.

## Available API

| Route | Purpose |
|---|---|
| `GET /healthz` | HTTP process health; does not test provider accounts |
| `GET /v1/models` | Models exposed for available accounts |
| `POST /v1/chat/completions` | Conversations, JSON or SSE |
| `POST /v1/responses` | Responses input, JSON or SSE, context through `previous_response_id` |
| `POST /v1/completions` | Compatibility with legacy text completions |
| `/admin` | Console, login and administration |
| `/docs` and `/openapi.json` | Simplified local reference when enabled |

Tool definitions are included in the model prompt and calls are extracted from its output.
The client executes its functions; this adaptation does not offer every guarantee of a
native protocol. Built-in CLI execution tools are restricted for `/v1`; this does not prove
isolation against a compromised CLI.

Parameters such as `temperature`, `top_p`, `max_tokens` and `max_output_tokens` do not directly
control CLI generation. A value echoed in a response does not mean it was applied.
JSON schemas guide the model without exhaustive validation. Embeddings, audio, image
generation, files and batches endpoints are not implemented.

## State and data

The server keeps accounts, state and key hashes in atomically written JSON files. Passwords
use scrypt with a random salt. Random API keys are stored as hashes; provider credentials
must remain usable by the CLIs, and some are stored in plaintext. CLIs can also create their
own authentication files or caches. See [security](../SECURITY.md).

Responses context is kept in memory (up to 64 MiB and 500 entries by default), tied to the
request identity, and lost on restart or eviction. `store: false` prevents saving an entry;
`REMOTE_RESPONSES_MAX_MB=0` disables it globally. Clients sharing one key are not isolated
from one another.

Global request and token counters reset at startup. They are not a billing system.
Submitted content leaves your machine for the provider, whose retention policies still apply.

## Optional macOS app

The Swift/AppKit app displays the console in WKWebView and supervises the same Rust binary.
It listens only on localhost, provides a password setup wizard and keeps the server running
when the window closes. Quitting the app closes a pipe monitored by Rust and stops the server.
App and Docker data are independent. There is no Electron, Node server, automatic
synchronization or automatic app update system.
