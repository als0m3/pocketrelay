# Third-party components

The repository's MIT license covers original PocketRelay code, not the services or every
tool it invokes. Provider names identify integrations and do not imply affiliation.

- Rust dependencies: exact versions in `Cargo.lock`, each with its own license.
- Frontend: marked and DOMPurify, with licenses and integrity values in [static/vendor](static/vendor/README.md).
- Codex CLI: [source and Apache-2.0 license](https://github.com/openai/codex).
- Claude Code: [legal and compliance information](https://code.claude.com/docs/en/legal-and-compliance).
- Antigravity CLI: [official repository](https://github.com/google-antigravity/antigravity-cli) and
  [service terms](https://antigravity.google/terms).
- Docker: Debian, Poppler and their dependencies retain their respective licenses.
- macOS app: Apple system frameworks; executable details in [macos/THIRD-PARTY.md](macos/THIRD-PARTY.md).

Provider executables are not committed to this repository. Docker downloads them during builds;
the Mac app downloads them on first use from pinned official sources. Building a package does not validate redistribution rights.
Before distributing an image or DMG, review each bundled component's licenses, terms, notices
and any source-distribution obligations. Account terms are separate from source-code licenses.
