# Get started with PocketRelay

Connect one of your AI accounts, create a gateway key, and use its models from an
OpenAI-compatible application. Models run at the provider; keep an Internet connection
and use an account that is eligible for the selected provider service.

Choose **one** installation. The Mac app and Docker offer the same web console but keep
separate accounts and keys. Neither requires LiteLLM or an external database.

## 1. Install and open the console

### macOS: download the app

The first downloadable package is an **Apple Silicon preview** (M-series Macs).
There is no tested Intel DMG in this release. Open Apple menu → **About This Mac** to
check your chip. See the [release notes](https://github.com/als0m3/pocketrelay/releases/tag/v0.2.1)
for the package's minimum macOS version and the system actually tested.

1. Open the [v0.2.1 release](https://github.com/als0m3/pocketrelay/releases/tag/v0.2.1).
2. Under **Assets**, download `PocketRelay-0.2.1-arm64.dmg`. The GitHub “Source code”
   archives are for developers; they are not the Mac installer.
3. Open the DMG and drag **PocketRelay** onto **Applications**.
4. Eject the disk image, then open PocketRelay from Applications.
5. Choose a username and a password of at least 12 characters. **Generate a password**
   fills both password fields with a random password. Save it in your password manager,
   then choose **Create and start**.

The app includes its server and tools. You do not need to install Docker, Rust, Python,
Homebrew or Node.js. The console is inside the app and also at
**http://localhost:8788/admin** while it is running.

#### If macOS blocks the first launch

This preview has a local **ad hoc signature**, without an Apple Developer ID or Apple
notarization. A Gatekeeper warning is expected.

After attempting to open PocketRelay from Applications:

1. Open **System Settings → Privacy & Security**.
2. Scroll to the warning about PocketRelay and click **Open Anyway**.
3. Confirm opening the app; macOS may ask for your Mac password or Touch ID.

Approve only the package you intentionally downloaded from this repository. Do not disable
Gatekeeper globally. If macOS reports malware or a damaged package, stop and verify the
download instead of bypassing that warning. Managed Macs may restrict manual approval.
See [Apple's instructions](https://support.apple.com/102445).

Optional integrity check: download `SHA256SUMS.txt` alongside the DMG, open Terminal in
the download folder and run:

```bash
shasum -a 256 -c SHA256SUMS.txt
```

The result should end in `OK`. A checksum checks that the download matches the release;
it is not an Apple signature or a security certification.

### Docker: install on Mac, Linux or Windows

Install Docker with Compose and Git first. On Windows, use a WSL terminal with Docker
integration enabled. Start Docker, then run:

```bash
git clone https://github.com/als0m3/pocketrelay.git
cd pocketrelay
./install.sh
```

The installer builds the image, asks you to create an administrator account and starts
PocketRelay. The first build can take several minutes. Open **http://localhost:8787/admin**
and sign in. On later runs, the installer preserves your existing administrator account.

## 2. Connect one provider account

In **Provider accounts**, choose the provider you want and click **Add**. Give the
account a recognizable name, such as `Personal Claude` or `Work ChatGPT`.
You only need one provider to get started.

### Claude

1. In the Mac app, click **Connect Claude on this Mac**. Terminal opens the bundled
   setup assistant; complete the provider's sign-in flow in your browser.
2. Copy the generated `sk-ant-…` subscription token and paste it into PocketRelay's
   **Token** field. Keep this token private.
3. Enter an account name and click **Add**. The console tests the account.

With Docker, the dialog gives you this command to run from the repository directory:

```bash
docker compose exec customremote claude setup-token
```

Then paste the returned token into the console as above.

### ChatGPT / Codex

1. Click **Add** under **ChatGPT · Codex**, name the account and click **Continue**.
2. Open the sign-in link displayed by the console in your browser and enter its device code.
3. Complete the provider authorization. Return to PocketRelay and wait for confirmation.
4. Click **Test** on the account card.

This connects the server's own Codex account; it does not import credentials from another
installation. If your account requires enabling device-code authentication, follow the
provider's on-screen instructions.

### Google / Antigravity

1. Click **Add** under **Google · Antigravity**, name the account and click **Continue**.
2. In the Mac app, use the button to open the connection assistant in Terminal.
   With Docker, copy and run the exact command displayed by the console.
3. Complete the browser/terminal authorization, then return to PocketRelay and click **Test**.

Google sign-in needs an interactive terminal. Do not substitute a command for another
account: each account has its own credential directory.

A successful account test makes a small real provider request and consumes a little quota.
Available models and provider access depend on your account. This project does not grant
permission to bypass provider terms or share subscriptions.

## 3. Create a key for your application

Under **API keys**, enter the application name and click **Create a key**.
Copy the `sk-cr-…` key immediately and save it securely; it is displayed only once.

These three credentials have different purposes:

| Credential | Purpose |
|---|---|
| PocketRelay username and password | Sign in to the administration console |
| Provider account or subscription token | Let PocketRelay contact that provider |
| PocketRelay `sk-cr-…` API key | Let your application call PocketRelay |

Use a separate gateway key per application so you can revoke one independently. Gateway
keys currently access all enabled accounts; they do not enforce model-specific permissions
or budgets. Never put real keys in GitHub issues, screenshots or committed example code.

## 4. Connect your application

In **Connect your application**, choose a model and copy its full identifier.
For example, it may look like `personal-claude/sonnet`; use the exact value shown in your
console rather than assuming an example exists on your account.

In your application's OpenAI-compatible provider settings, enter:

| Setting | Mac app | Docker |
|---|---|---|
| Base URL | `http://localhost:8788/v1` | `http://localhost:8787/v1` |
| API key | Your `sk-cr-…` key | Your `sk-cr-…` key |
| Model | Exact `account/model` identifier | Exact `account/model` identifier |

Send a short message such as **Hello!**. Keep PocketRelay running while using the client.
A bare model name such as `sonnet` is rejected by default. Requests do not silently switch
to another account when the selected account is unavailable.

The console also generates Python, curl and Open WebUI examples when you create a key.
The [README Python example](../README.md#your-first-python-request) is another starting point.
Python is only needed for that optional client example, not to run the Mac app.

`localhost` means the machine or container running your client. If your client runs in a
container on Docker Desktop, use `host.docker.internal` to reach a Mac-hosted app. Containers
on the PocketRelay Compose network can use `http://customremote:8787/v1`. Remote-machine
access requires a deliberately configured secure proxy; it is not exposed by default.

## Everyday use

**Mac:** closing the window leaves the API running in the menu bar. Choose **Open console**
to reopen it, **Restart service** to restart the API, or **Quit and stop the API** to stop.
**Launch at login** is optional; macOS may request approval. Your Mac must remain awake.
Change your console password through **Change password…** in the app menu.

**Docker:** run these commands from the repository directory:

```bash
docker compose stop            # Stop and keep data
docker compose up -d --wait     # Start again
docker compose logs --tail=100  # Inspect local logs
```

To update the Mac app, quit it and replace it in Applications with the newer release.
Updates are manual. Back up `~/Library/Application Support/PocketRelay` while the app is
stopped. Replacing the app preserves this directory. Docker uses a separate volume;
**do not run `docker compose down -v` unless you intend to delete its data**.

## Quick troubleshooting

| Problem | What to check |
|---|---|
| App blocked on first launch | Follow the Gatekeeper steps above for the trusted downloaded package |
| Service cannot start | Port 8788 may belong to another app; quit that app and restart PocketRelay |
| Client gets connection refused | PocketRelay is running, the Mac is awake, and the port matches your installation |
| Console password lost | Mac menu → Change password; Docker → `docker compose exec customremote customremote setup` |
| API returns 401 | Use an active `sk-cr-…` key, not the console password or provider token |
| Model rejected | Copy the complete identifier from the model selector or account model list |
| Provider test fails | Read the account error, check eligibility/credentials and reconnect if needed |
| Quota reached | Wait for the provider reset; creating a new gateway key does not increase quota |
| API key lost | Create a replacement and revoke the old key |

For many users, virtual keys or budgets, [LiteLLM is an optional front gateway](../README.md#need-many-keys-litellm-is-optional).
PocketRelay works by itself. Read [security](../SECURITY.md), [configuration](configuration.md)
and [macOS details](macos.md) before exposing the service beyond your computer.
