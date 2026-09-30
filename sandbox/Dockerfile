# Build from the repository root. The final image is a Docker Sandbox template,
# not a host-Docker container used as a substitute for a sandbox.
FROM rust:1.89-bookworm@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff AS build
WORKDIR /build
COPY Cargo.toml ./
COPY src ./src
COPY bridges ./bridges
COPY prompts ./prompts
COPY tests ./tests
RUN cargo build --release --bin omar

FROM docker.io/docker/sandbox-templates:shell@sha256:c66aa9c0212bb710089cc356501f8bde8989e02a7071bf1c6be0893be1c30adf
USER root
RUN apt-get update && apt-get install -y --no-install-recommends tmux libssl3 ca-certificates build-essential && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/omar /usr/local/bin/omar
# Rust bodies must compile inside the VM, including on macOS hosts.
COPY --from=build /usr/local/rustup /usr/local/rustup
COPY --from=build /usr/local/cargo /usr/local/cargo
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/home/agent/.cargo
ENV PATH=/usr/local/cargo/bin:$PATH
USER agent
# Install chosen backend CLIs in a derived template before pinning its digest.
# This base supports code reactions and OMAR's model-free stub backend.
