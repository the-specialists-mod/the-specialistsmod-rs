# The build image: everything `cargo test`, `cargo fmt`, `cargo xtask build|lint|dist` need, so
# a build doesn't depend on what the host has installed. Run commands in it with docker/run.sh
# (AGENTS.md "Build and test"). The in-engine tests (`install`, `test`, `play`, ...) stay on the
# host: they need the engine roots, Wine, Steam and a display.
FROM ubuntu:24.04

# The toolchain the workspace is fmt- and clippy-clean with. A newer rustfmt reformats the
# `payload!` macros and a newer clippy adds lints, so bump this deliberately, with the fixes.
ARG RUST_VERSION=1.99.1
# The FWGS commit the bindings are generated from: keep it the same as .github/workflows/release.yml.
ARG FWGS_COMMIT=51353ff8d65e58300eeb2bd014867673f1f367c6

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git build-essential gcc-multilib mingw-w64 clang libclang-dev zip binutils \
    && rm -rf /var/lib/apt/lists/*

# rustup in /usr/local (read-only for the build user); CARGO_HOME is a separate, world-writable
# directory so docker/run.sh can run as the host user and keep the registry in a volume.
ENV RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | CARGO_HOME=/usr/local/cargo sh -s -- -y --no-modify-path --profile minimal \
        --default-toolchain ${RUST_VERSION} --component rustfmt,clippy \
    && CARGO_HOME=/usr/local/cargo rustup target add \
        i686-unknown-linux-gnu x86_64-unknown-linux-gnu i686-pc-windows-gnu x86_64-pc-windows-gnu \
    && chmod -R a+rX /usr/local/rustup /usr/local/cargo
RUN mkdir -p /cargo-home && chmod 1777 /cargo-home
ENV CARGO_HOME=/cargo-home

# goldsrc-sys's headers (FWGS_DIR overrides its search for ../xash3d-fwgs).
RUN git clone --filter=blob:none https://github.com/FWGS/xash3d-fwgs /opt/xash3d-fwgs \
    && git -C /opt/xash3d-fwgs checkout ${FWGS_COMMIT} \
    && git -C /opt/xash3d-fwgs submodule update --init 3rdparty/library_suffix \
    && chmod -R a+rX /opt/xash3d-fwgs
ENV FWGS_DIR=/opt/xash3d-fwgs

# Any user (docker/run.sh passes the host's uid) needs a writable HOME.
ENV HOME=/tmp
