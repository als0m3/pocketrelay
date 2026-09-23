FROM python:3.13-slim

LABEL org.opencontainers.image.title="custom-remote" \
      org.opencontainers.image.description="OpenAI-compatible gateway (Claude, Codex, Gemini) and token console" \
      org.opencontainers.image.source="https://github.com/als0m3/custom-remote" \
      org.opencontainers.image.authors="Als0m3"

RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Install the CLI in the image and copy its standalone binary (no runtime auto-update).
ENV HOME=/home/app
RUN mkdir -p $HOME && curl -fsSL https://claude.ai/install.sh | bash \
    && cp -L $HOME/.local/bin/claude /usr/local/bin/claude \
    && rm -rf $HOME/.local $HOME/.claude* && claude --version

# Codex CLI (ChatGPT subscription), static musl binary
ARG CODEX_VERSION=0.155.1
RUN curl -fsSL "https://github.com/openai/codex/releases/download/rust-v${CODEX_VERSION}/codex-x86_64-unknown-linux-musl.tar.gz" \
    | tar xz -C /tmp && mv /tmp/codex-x86_64-unknown-linux-musl /usr/local/bin/codex && codex --version

# Gemini CLI (Google AI Pro / Ultra): npm package requiring Node.
ARG NODE_VERSION=24.21.0
ARG GEMINI_VERSION=0.60.0
RUN curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.gz" \
    | tar xz -C /usr/local --strip-components=1 \
      --exclude=CHANGELOG.md --exclude=LICENSE --exclude=README.md \
    && npm install -g "@google/gemini-cli@${GEMINI_VERSION}" \
    && npm cache clean --force && rm -rf /usr/local/include/node \
    && gemini --version

WORKDIR /app
COPY pyproject.toml ./
RUN pip install --no-cache-dir "fastapi>=0.115" "uvicorn>=0.30" "authlib>=1.3" "httpx>=0.27" "itsdangerous>=2.2" "pypdfium2>=4.30" "pillow>=10"
COPY remote ./remote
COPY static ./static

# OpenShift: arbitrary UID in group 0; HOME and data must be group-writable
RUN mkdir -p /data && chgrp -R 0 $HOME /data && chmod -R g=u $HOME /data
ENV REMOTE_DATA=/data REMOTE_HOST=0.0.0.0 REMOTE_PORT=8787 REMOTE_ENABLE_SESSIONS=0 \
    DISABLE_AUTOUPDATER=1 CLAUDE_BIN=/usr/local/bin/claude PYTHONUNBUFFERED=1 \
    CODEX_BIN=/usr/local/bin/codex CODEX_HOME=/data/codex \
    GEMINI_BIN=/usr/local/bin/gemini GEMINI_CLI_HOME=/data/gemini
USER 1001
EXPOSE 8787
CMD ["python", "-m", "remote"]
