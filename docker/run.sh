#!/usr/bin/env bash
# Runs a command in the build image (docker/Dockerfile), building the image first when the
# Dockerfile has changed:
#
#   docker/run.sh cargo xtask build
#   docker/run.sh cargo test
#
# The checkout is mounted at the same path as on the host, so `target/` (which xtask hard-codes)
# and the paths build scripts record are the same inside and out. The command runs as the host
# user, so what it writes stays the host user's. The cargo registry lives in the
# `tsrs-cargo-home` volume.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dockerfile="$repo/docker/Dockerfile"
tag="tsrs-build:$(sha256sum "$dockerfile" | cut -c1-12)"

if ! docker image inspect "$tag" >/dev/null 2>&1; then
    docker build -t "$tag" -f "$dockerfile" "$repo/docker" >&2
fi

[ $# -gt 0 ] || set -- bash
tty=()
[ -t 0 ] && [ -t 1 ] && tty=(-it)

# Run from the same directory when it's inside the checkout, else from the checkout's root.
case "$PWD/" in
    "$repo"/*) workdir="$PWD" ;;
    *) workdir="$repo" ;;
esac

exec docker run --rm "${tty[@]}" \
    -u "$(id -u):$(id -g)" \
    -v "$repo:$repo" \
    -v tsrs-cargo-home:/cargo-home \
    -w "$workdir" \
    "$tag" "$@"
