# Agent CLIs, Rust, and tmux for OMAR team sandboxes. OMAR itself is not
# here: the host copies its own Linux binary in when a sandbox starts, so the
# worker always matches the runtime that drives it.
FROM rust:1.89-bookworm@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff AS rust

FROM docker.io/docker/sandbox-templates:shell@sha256:c66aa9c0212bb710089cc356501f8bde8989e02a7071bf1c6be0893be1c30adf
ARG TARGETARCH
ARG CLAUDE_VERSION=2.1.310
ARG CODEX_VERSION=0.164.0
ARG OPENCODE_VERSION=1.19.4
ARG PI_VERSION=1.4.0
ARG CURSOR_VERSION=2026.10.22-27dcf5f
USER root
RUN apt-get update && apt-get install -y --no-install-recommends tmux build-essential ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
# Rust bodies compile inside the VM, including on macOS hosts.
COPY --from=rust /usr/local/rustup /usr/local/rustup
COPY --from=rust /usr/local/cargo /usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/home/agent/.cargo PATH=/usr/local/cargo/bin:$PATH
# OMAR's cursor adapter runs `cursor agent ...`; the Linux CLI is `cursor-agent`.
RUN printf '#!/bin/sh\n[ "$1" = agent ] && shift\nexec cursor-agent "$@"\n' > /usr/local/bin/cursor \
 && chmod 0755 /usr/local/bin/cursor
USER agent
RUN set -eux; \
    case "$TARGETARCH" in amd64) claude=linux-x64 cursor=x64 ;; arm64) claude=linux-arm64 cursor=arm64 ;; *) exit 1 ;; esac; \
    mkdir -p ~/.local/bin ~/.local/share/claude/versions ~/.local/share/cursor-agent/versions/$CURSOR_VERSION; \
    curl -fsSL "https://downloads.claude.ai/claude-code-releases/$CLAUDE_VERSION/$claude/claude" \
      -o ~/.local/share/claude/versions/$CLAUDE_VERSION; \
    chmod 0755 ~/.local/share/claude/versions/$CLAUDE_VERSION; \
    ln -s ~/.local/share/claude/versions/$CLAUDE_VERSION ~/.local/bin/claude; \
    curl -fsSL "https://downloads.cursor.com/lab/$CURSOR_VERSION/linux/$cursor/agent-cli-package.tar.gz" \
      | tar -xz --strip-components=1 -C ~/.local/share/cursor-agent/versions/$CURSOR_VERSION; \
    ln -s ~/.local/share/cursor-agent/versions/$CURSOR_VERSION/cursor-agent ~/.local/bin/cursor-agent; \
    curl -fsSL https://antigravity.google/cli/install.sh | bash; \
    npm install -g --no-fund --no-audit "@openai/codex@$CODEX_VERSION" "opencode-ai@$OPENCODE_VERSION" \
      "@earendil-works/pi-coding-agent@$PI_VERSION"; \
    npm cache clean --force; \
    for cli in claude codex opencode pi cursor-agent agy; do command -v "$cli"; done
ENV IS_SANDBOX=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_AUTOUPDATER=1
LABEL org.opencontainers.image.source=https://github.com/omar-os/omar
