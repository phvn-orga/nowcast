# The release build: the static `nowcast` binary for x86_64 Linux, built the
# same way by the release workflow and on a developer's machine, with the
# toolchain pinned here. From the repository root:
#
#   docker build -f build/release.Dockerfile --output dist .
#
# leaves the binary in dist/nowcast. Bump the image tag to change toolchains.

FROM rust:1.99.1-trixie AS build
# ring, under the claude-code brick's TLS, compiles C for the musl target.
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-unknown-linux-musl
ENV CC_x86_64_unknown_linux_musl=musl-gcc
WORKDIR /src
COPY . .
# The cache mounts keep downloads and builds between local runs; they are not
# part of the image, so the binary is copied out of the cached target.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked -p nowcast-cli --target x86_64-unknown-linux-musl --profile dist \
    && cp target/x86_64-unknown-linux-musl/dist/nowcast /nowcast \
    && /nowcast --version

FROM scratch
COPY --from=build /nowcast /nowcast
