# Bundled components

The package includes Claude Code and Codex CLI executables from the build machine, plus
Antigravity CLI 1.2.10 (or an explicitly supplied executable). Original file hashes are
recorded in `manifest.json`. Notices found in local installations or archives are copied
alongside that file. The Codex Apache-2.0 license and NOTICE from tag `rust-v0.155.1`
are also included under `ThirdParty/codex`.

- Claude Code: https://code.claude.com/docs/en/legal-and-compliance; Anthropic terms apply.
- Codex CLI: https://github.com/openai/codex (Apache-2.0).
- Antigravity: https://github.com/google-antigravity/antigravity-cli; check the bundled version's license.
- Frontend: see `static/vendor/README.md` and accompanying licenses.

PDF support uses Apple's PDFKit framework through the adapter built from `Sources/PDFTool.swift`.
No Poppler executable is bundled in the macOS app.

Collecting available notices is not a complete redistribution-rights review. Before publishing
a binary package, verify each component's license and terms and supply any required notices
or source materials. The local development package is not a redistribution approval.
