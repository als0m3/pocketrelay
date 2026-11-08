FROM rust:1.97-slim-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
# Cache dependencies until Cargo.lock changes.
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release --locked
COPY src ./src
COPY static/api-docs.html ./static/api-docs.html
RUN touch src/main.rs && cargo build --release --locked

FROM debian:bookworm-slim

LABEL org.opencontainers.image.title="PocketRelay" \
      org.opencontainers.image.description="OpenAI-compatible API gateway (Claude, Codex, Antigravity) and account console" \
      org.opencontainers.image.source="https://github.com/als0m3/pocketrelay" \
      org.opencontainers.image.authors="Als0m3"

RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates poppler-utils \
    && rm -rf /var/lib/apt/lists/*

# Official CLIs: pinned versions and hashes, without executing a remote installer.
ENV HOME=/home/app
ARG CLAUDE_VERSION=2.1.289
ARG CLAUDE_SHA256=a186b99e4a9c88366cd49df2f7dad56c61fc306ef0140b19ee64b7c42a8d1348
RUN mkdir -p $HOME && curl -fsSL --retry 3 --retry-all-errors "https://downloads.claude.ai/claude-code-releases/${CLAUDE_VERSION}/linux-x64/claude" -o /usr/local/bin/claude \
    && echo "${CLAUDE_SHA256}  /usr/local/bin/claude" | sha256sum -c - \
    && chmod 755 /usr/local/bin/claude && claude --version

# Codex CLI (ChatGPT subscription), static musl binary
ARG CODEX_VERSION=0.155.1
ARG CODEX_SHA256=a0ef8b2debc3bf747e07b1a039354de31300ac0dcc2276498ba281470b5d9115
RUN curl -fsSL --retry 3 --retry-all-errors "https://github.com/openai/codex/releases/download/rust-v${CODEX_VERSION}/codex-x86_64-unknown-linux-musl.tar.gz" -o /tmp/codex.tar.gz \
    && echo "${CODEX_SHA256}  /tmp/codex.tar.gz" | sha256sum -c - \
    && tar xzf /tmp/codex.tar.gz -C /tmp && mv /tmp/codex-x86_64-unknown-linux-musl /usr/local/bin/codex \
    && rm /tmp/codex.tar.gz && codex --version

# Antigravity CLI (Google AI subscription): standalone Go binary replacing the Gemini CLI
ARG ANTIGRAVITY_VERSION=1.2.10
ARG ANTIGRAVITY_SHA256=77cb69251292aa35b0b662f91f704f06dd787b72f7902a62db8c6d692989203e
RUN curl -fsSL --retry 3 --retry-all-errors "https://github.com/google-antigravity/antigravity-cli/releases/download/${ANTIGRAVITY_VERSION}/agy_cli_linux_x64.tar.gz" -o /tmp/antigravity.tar.gz \
    && echo "${ANTIGRAVITY_SHA256}  /tmp/antigravity.tar.gz" | sha256sum -c - \
    && tar xzf /tmp/antigravity.tar.gz -C /usr/local/bin && rm /tmp/antigravity.tar.gz && antigravity --version

WORKDIR /app
COPY --from=build /build/target/release/customremote /usr/local/bin/customremote
COPY static ./static
COPY LICENSE /usr/share/doc/pocketrelay/LICENSE

# OpenShift: arbitrary UID in group 0; HOME and data must be group-writable
RUN mkdir -p /data && chgrp -R 0 $HOME /data && chmod -R g=u $HOME /data
ENV REMOTE_DATA=/data REMOTE_HOST=0.0.0.0 REMOTE_PORT=8787 REMOTE_ENABLE_SESSIONS=0 \
    DISABLE_AUTOUPDATER=1 CLAUDE_BIN=/usr/local/bin/claude \
    CODEX_BIN=/usr/local/bin/codex CODEX_HOME=/data/codex \
    ANTIGRAVITY_BIN=/usr/local/bin/antigravity ANTIGRAVITY_HOME=/data/antigravity
USER 1001
EXPOSE 8787
CMD ["customremote", "serve"]
