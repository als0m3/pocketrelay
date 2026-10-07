# Security

PocketRelay is an experimental personal tool. Automated tests and secret scans are neither
an independent audit nor a guarantee that the software is free of vulnerabilities.

## Trust boundaries

- The administrator controls all accounts and keys. An API key can use all active accounts;
  there is no tenant separation or per-model access control.
- Claude sessions can read files or execute commands according to their permissions. They
  are disabled in Compose, the app and `run.sh`. Do not enable them for third parties.
- Provider CLIs are external executables. Environment filtering and their restriction flags
  are not a substitute for operating system isolation.
- Remote content fetching filters private destinations and redirects; this is not a guarantee
  against every form of SSRF.

## Secrets and storage

The administrator password is derived with scrypt and a random salt. API keys are stored as
SHA-256 hashes. The master token and some provider credentials remain in plaintext on disk;
the macOS Keychain is not used as a general credential vault. Atomically written JSON files
are private on Unix; the app creates its data directory with mode 0700. CLI caches and backups
also need protection by their owner.

Web login uses a signed HttpOnly cookie. For an external HTTPS URL, configure
`REMOTE_PUBLIC_URL` and `REMOTE_HTTPS=1`. Administrative mutations check the origin and a
dedicated header. `/v1` allows CORS and requires a key: any website that knows a key can use
it; CORS is not an access-control mechanism for this API.

In-memory Responses history is tied to an identity. Do not share one key between users who
need isolation, or disable storage with `REMOTE_RESPONSES_MAX_MB=0`. Providers may retain
content according to their own policies.

## Dependencies and publication

`Cargo.lock` is committed. The three CLIs in the Dockerfile are downloaded at fixed versions
and verified against fixed SHA-256 hashes. Base images and Debian packages are not pinned
to a point in time, so builds are not fully reproducible. The macOS app downloads tools on
first use from the official HTTPS URLs pinned in `src/provider-tools.json`, verifies size
and SHA-256 before installation, and retains them in its private data directory. Provider
executables are not bundled in the DMG. Tool versions are updated through app releases.

CI checks locked Rust dependency versions against OSV advisories. JWT validation rejects
malformed standard claims and checks expiration and optional not-before dates. These checks
cannot detect every vulnerability in application code or in external provider tools.

The source repository does not include real credentials, data directories or built packages.
Never commit `.env`, backups, tokens, private certificates, account screenshots or unsanitized
logs. `.gitignore` cannot prevent forced additions or secrets inside source files.
Also run the scan described in [CONTRIBUTING.md](CONTRIBUTING.md).

## Reporting a problem

Do not open a public issue containing an exploitable vulnerability or credentials.
Use GitHub **Security → Report a vulnerability** if private reporting is enabled. If it is
unavailable, ask for a private channel without including sensitive details. No guaranteed
remediation timeline is offered. Revoke accidentally shared keys immediately.
