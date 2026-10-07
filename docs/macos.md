# Optional macOS app

The app is another way to use PocketRelay, with a native window and menu bar icon. It bundles
the Rust server and reuses the existing web console. Docker remains a separate option.
An Apple Silicon preview is available in the
[v0.3.0 release](https://github.com/als0m3/pocketrelay/releases/tag/v0.3.0), with a checksum.
The minimum is macOS 14.0; this preview was tested on macOS 27.0.1 (arm64).
Start with the [installation and usage tutorial](getting-started.md). There is no Intel
package, Apple Developer ID signature, notarization or App Store version in this release.

## Install the release or a locally built package

1. Open the DMG for your architecture and drag PocketRelay to Applications.
2. Open the app; if macOS blocks it, use System Settings → Privacy & Security → Open Anyway
   as described in the [tutorial](getting-started.md#if-macos-blocks-the-first-launch).
   Choose your username and password.
3. **Generate a password** fills both fields with 24 random characters. Copy it into your
   password manager. This option is also available when changing the password.
4. Connect a provider account, create a key and copy settings from the built-in guide.

The end user's Mac does not need Docker, Rust, Python, Homebrew or Node.js. Internet access
and provider accounts are still required. Models do not run locally. Provider restrictions
still apply.

A local package is **ad hoc-signed**, without an Apple developer identity, and is not notarized.
macOS may refuse to open it. This does not mean Apple has approved the package.
Do not disable Gatekeeper globally. CI does not perform Developer ID signing or notarization.
Download release assets from this repository, not an unrelated mirror.

## Everyday use

- Closing the window keeps the API running in the menu bar.
- **Quit and stop the API** stops the server; data remains on disk.
- **Launch at login** is optional and disabled by default.
- The menu can copy the URL, restart the service, change the password and open data or logs.
- Provider setup opens in your browser or Terminal, depending on the provider.
- The Mac must stay awake and the app must remain open for the API to be reachable.

Console: `http://localhost:8788/admin`; API: `http://localhost:8788/v1`.
Data: `~/Library/Application Support/PocketRelay`. Docker uses port 8787 and its own volume.
Accounts and keys are not shared automatically. Port 8788 must be free; the app does not
take control of an unknown server already using it. An older app with another name may use
the same port: quit it before starting PocketRelay. Its data is not migrated automatically.

Quit the app before backing up its data directory. Replacing the app does not delete data.
Removing it from Applications does not remove that directory either. Manually deleting the
directory is irreversible and removes the accounts and keys for that installation.

## Build for yourself

Build-machine requirements: macOS 14+, Xcode command-line tools, Rust 1.97+, Python 3 and
no provider executables. The build does not download Claude, Codex or Antigravity.

```bash
./macos/build.sh
```

The script creates `dist/macos/PocketRelay.app` and `PocketRelay-arm64.dmg` or
`PocketRelay-x86_64.dmg`. Signing defaults to ad hoc, with no personal certificate or upload
to Apple. Do not use `MACOS_SIGN_IDENTITY` or `macos/notarize.sh` for this local mode: these
options are for a separate, explicit distribution process with your own rights and credentials.

`MACOS_SKIP_DMG=1` skips the DMG. The build matches the host Mac's architecture; the Intel
variant needs separate testing. PDFKit supplies PDF support without Poppler. Compiler source
paths are remapped and the bundle is checked for private paths and credentials before packaging.

Provider tools download on first use into `~/Library/Application Support/PocketRelay/tools`.
`src/provider-tools.json` pins official HTTPS URLs, versions, archive members, sizes and SHA-256
hashes for both Mac architectures. Downloads are streamed to temporary files, checked before
installation, and atomically replaced. Only the expected regular archive member is extracted.
An interrupted or failed download is retried from the beginning, never executed partially.
Existing accounts and API keys remain in place. Updates to pinned tools are delivered through
app releases; provider self-updaters are disabled. No credentials are sent with downloads.

The authenticated console exposes installation progress and retry controls, including when
opened in an external browser. API calls also install a missing tool before starting a provider.
Simply launching the app or refreshing the model catalog does not download unused tools.
Docker and standalone servers keep their current tool management unless explicitly configured.

For additional private terms, use `macos/scripts/check-privacy.py --private-terms-file`
with a local JSON array kept outside the repository.

See [third-party tools](../macos/THIRD-PARTY.md) for sources and applicable notices.

## Verify a build

```bash
python3 tests/desktop.py
# Optional online test: downloads all three tools into temporary data, without signing in
python3 tests/tools_download.py target/release/customremote
python3 macos/scripts/verify.py dist/macos/PocketRelay.app
dist/macos/PocketRelay.app/Contents/MacOS/PocketRelay --smoke-test
python3 macos/scripts/test-dmg.py dist/macos/PocketRelay-arm64.dmg
```

The desktop test checks password delivery over stdin, file permissions, account preservation
and Rust lifecycle behavior. The smoke test needs a graphical session and port 18789 free;
it checks generated passwords and WebKit login with disposable data. It makes no real
provider calls.

The DMG test mounts the volume read-only, copies the app to a path containing spaces, ejects
the volume and tests the copy. It reports functional results separately from Gatekeeper:
functional success does not imply macOS acceptance. The report stays at
`/tmp/customremote-dmg-test.json` for script compatibility.

Release notes record the exact package, tested operating system and functional checks.
The initial Apple Silicon preview is tested on the release machine; a deployment target
is not evidence of testing every supported macOS version. Provider protocol tests use
fixtures, and launch-at-login behavior and all real provider sign-in flows are not certified.
App Store distribution, automatic updates and installation synchronization are not implemented.
