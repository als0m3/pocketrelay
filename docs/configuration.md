# Configuration and troubleshooting

## Docker installation

`./install.sh` is enough for the standard settings. `.env` is optional and must never be
committed. Compose reads the following variables:

| Variable | Initial value | Purpose |
|---|---|---|
| `PORT` | `8787` | Port accessible from your computer |
| `BIND_ADDRESS` | `127.0.0.1` | Interface published by Docker |
| `REMOTE_ALLOWED_HOSTS` | `localhost` | Allowed hostnames, comma-separated |
| `REMOTE_PUBLIC_URL` | empty | Exact external URL when using a reverse proxy |
| `REMOTE_HTTPS` | `0` | Set to `1` for Secure cookies behind an HTTPS proxy |

Other server variables must be explicitly added to `environment` in a local Compose file,
such as `compose.override.yaml`. Putting them only in `.env` does not pass them into the
container. Keep local configuration files out of Git. The Compose service is named
`customremote` for technical compatibility.

## Server settings and limits

| Server variable | Unconfigured binary default | Effect |
|---|---|---|
| `REMOTE_DATA` | `data` | Data directory; `/data` in Docker |
| `REMOTE_HOST` / `REMOTE_PORT` | `127.0.0.1` / `8787` | Server listen address |
| `REMOTE_STATIC` | `static` | Console directory |
| `REMOTE_ENABLE_SESSIONS` | `1` | Claude sessions that can act on the host |
| `REMOTE_SYSTEM_ACCOUNTS` | `claude,codex,antigravity` | Accounts inherited from the host |
| `REMOTE_V1_ALLOW_MASTER` | `1` | Accept the master token on `/v1` |
| `REMOTE_REQUIRE_ACCOUNT` | `1` | Require an explicit account in model names |
| `REMOTE_MAX_CONCURRENCY` | `6` | Concurrent calls; `0` removes the practical limit |
| `REMOTE_QUEUE_TIMEOUT` | `120` | Maximum queue wait in seconds |
| `REMOTE_REQUEST_TIMEOUT` | `600` | Maximum call duration in seconds |
| `REMOTE_MAX_OUTPUT_MB` | `16` | Maximum collected output size |
| `REMOTE_RESPONSES_MAX_MB` | `64` | Responses context memory; `0` disables it |
| `REMOTE_USER_RATE` | `200/h` | Per-identity API limit, except configured exemptions |
| `REMOTE_ENABLE_DOCS` | `1` without a public URL | Local reference routes |

**Compose, the macOS app and `./run.sh` disable sessions, inherited system accounts and
master-token access to `/v1`.** The directly executed binary retains its legacy defaults;
use the script or explicitly set these three variables.

`CLAUDE_BIN`, `CODEX_BIN` and `ANTIGRAVITY_BIN` select the executables. Install them separately
when running without Docker or the macOS package. PDF handling uses Poppler's `pdfinfo`,
`pdftotext` and `pdftoppm`; the macOS package provides a PDFKit adapter.

## Forgotten password

From the Docker project directory, rerun interactive setup:

```bash
docker compose exec customremote customremote setup
```

The new password invalidates existing login sessions. Provider accounts and API keys are
preserved. In the macOS app, use the password-change menu item.

## Troubleshooting

- **Port in use**: choose another Docker `PORT`. The app requires port 8788 to be free.
- **401**: use an active API key, not the console password or a provider token.
- **Unknown model**: copy the full identifier from `/v1/models` or the account card.
- **403 Host rejected**: add only your hostname to `REMOTE_ALLOWED_HOSTS`.
- **429 / provider unavailable**: check the account and its quotas; the project does not bypass them.
- **Another container cannot reach localhost**: use `http://customremote:8787/v1` only when
  that container shares the service's Compose network.

Before sharing logs, remove account information, local paths, prompts and secrets.

## Backups and network access

Stop the service and back up the entire Docker volume, or quit the app and copy its data
directory. Encrypt backups: they contain provider credentials. Restore into the same
installation with the same permissions, then restart. Do not run two servers against the
same data directory at the same time.

The project targets a personal workstation. Network exposure requires dedicated configuration:
HTTPS at the proxy, explicit hostnames and public URL, restricted access and protected data.
This repository does not include a production configuration ready for Internet exposure.

Optional SSO uses `REMOTE_OIDC_ISSUER`, `REMOTE_OIDC_CLIENT_ID`, optionally
`REMOTE_OIDC_CLIENT_SECRET`, and an administrator allowlist through `REMOTE_ADMIN_GROUPS` or
`REMOTE_ADMIN_EMAILS`. Register `/admin/auth/callback` at the public URL as the callback;
verify roles with your provider before exposing the service. Tests use a simulated local
OIDC provider.
