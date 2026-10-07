# Third-party tools

The macOS app bundles the Pocket Relay server, web console and PDFKit adapters. Provider
executables are not included in the DMG. When you add or use a provider, the app downloads
its pinned tool directly from the official source and verifies its size and SHA-256 hash.

The complete source URLs, versions and hashes are in `src/provider-tools.json`, also included
in the app under `Resources/ThirdParty/provider-tools.json`.

- Claude Code 2.1.289: https://downloads.claude.ai/claude-code-releases/2.1.289/manifest.json;
  https://code.claude.com/docs/en/legal-and-compliance. Anthropic terms apply.
- Codex CLI 0.155.1: https://github.com/openai/codex/releases/tag/rust-v0.155.1 (Apache-2.0).
  Its LICENSE and NOTICE are included under `ThirdParty/codex`.
- Antigravity CLI 1.2.10: https://github.com/google-antigravity/antigravity-cli/releases/tag/1.2.10.
  See the upstream project for its terms and notices.
- Frontend: see `static/vendor/README.md` and accompanying licenses.

PDF support uses Apple's PDFKit framework through `Sources/PDFTool.swift`; no Poppler
executable is bundled. Docker has its own separately pinned tools in the Dockerfile.
Downloading a tool does not grant permission to use a provider account or override its terms.
