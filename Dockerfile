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
# Latest stable Rust, matching the `stable` toolchain the rest of the release
# pipeline builds with. (Not pinned to the workspace's `rust-version = "1.83"`:
# transitive dependencies in the lockfile — e.g. moxcms via the `image` crate —
# use edition 2024, which Cargo only understands from 1.85 on.)
FROM rust:slim-bookworm AS build

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
