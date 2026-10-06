# Development and contributions

Requirements: Rust 1.97 or newer with rustfmt/clippy, Python 3, Node.js for JavaScript syntax
checks, and OpenSSL and Poppler for tests. No third-party Python packages are required.
Tests run simulated CLIs in temporary directories, without real credentials or billable calls.
Real CLIs are only required for connected use.

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
python3 tests/integration.py
python3 tests/oidc.py
python3 tests/desktop.py
node --check static/admin.js
node --check static/admin-view.js
node --check static/app.js
```

To run the service from source:

```bash
cargo run --release --locked -- setup
./run.sh
```

The script selects the personal profile, without sessions or inherited system accounts.
Install the provider CLIs and PDF tools separately; see [configuration](docs/configuration.md).
`customremote` remains the internal crate and binary name; the product is called PocketRelay.

## Before committing

Review `git diff --check` and `git diff --cached`, verify your Git identity and use a GitHub
noreply address if you do not want to expose your personal email. Do not add private
configuration, test output or built binaries. Keep examples fictional.

With Gitleaks installed (version used for this preparation: 8.30.1):

```bash
gitleaks dir --redact=100 --no-banner .
gitleaks git --redact=100 --no-banner --log-opts='--all' .
```

Directory scans may inspect ignored files; use a clean copy for a publication review.
Investigate each finding instead of globally disabling a rule to pass the check.
Scanners can miss secrets or report fictional examples.

## Verification scope

Tests cover authentication, three simulated CLI adapters, JSON/SSE responses, tools, PDFs,
quotas, shutdown, persistence and Responses isolation. OIDC is tested with local RSA signatures.
Desktop contract tests cover stdin setup and shutdown with the parent process.
The CI workflow does not publish images, releases or signed packages.

Tests do not validate provider terms, every real model, heavy load, all macOS versions,
launch at login or Gatekeeper distribution. Native app and DMG checks are separate;
see [docs/macos.md](docs/macos.md).
