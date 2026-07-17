# Binary-only image for nessemble, built as a fully-static musl executable on
# `scratch`. Its purpose is distribution, not a runnable dev shell: other
# projects lift the single binary out of it without building this workspace,
# e.g.
#
#     COPY --from=ghcr.io/kevinselwyn/nessemble-rs:latest /nessemble /usr/local/bin/nessemble
#
# There is no shell or libc in the final image — only the binary — so `docker
# cp`/`COPY --from` are the intended ways to consume it.

# --- build stage -----------------------------------------------------------
# Pinned to the workspace's minimum supported Rust (rust-version = "1.83").
FROM rust:1.83-slim-bookworm AS build

# musl-tools provides musl-gcc, the linker for the fully-static target.
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add x86_64-unknown-linux-musl

WORKDIR /src
COPY . .

# The musl target links crt-static by default, yielding a standalone binary
# with no runtime dependencies — exactly what `scratch` needs.
RUN cargo build --release --bin nessemble --target x86_64-unknown-linux-musl \
    && cp target/x86_64-unknown-linux-musl/release/nessemble /nessemble

# --- final stage -----------------------------------------------------------
FROM scratch
COPY --from=build /nessemble /nessemble
ENTRYPOINT ["/nessemble"]
