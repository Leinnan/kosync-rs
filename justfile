# kosync-rs development & deployment tasks

default:
    @just --list

# --- development ---
fmt:
    cargo fmt
fmt-check:
    cargo fmt --check
lint:
    cargo clippy --all-targets
test:
    cargo test
check: fmt-check lint test

build:
    cargo build --profile dist
run:
    cargo run --profile dist

# --- docker ---
docker-build:
    docker compose build
docker-up:
    docker compose up -d --build
docker-down:
    docker compose down
docker-logs:
    docker compose logs -f

# --- deploy (run from your workstation over SSH) ---

# Cross-compiled with zig and cargo-zigbuild from any host, Apple Silicon
# included, so a small server does not have to compile it. `glibc` is the
# oldest glibc the binary may run on (2.28: RHEL/AlmaLinux 8, Debian 10 and newer).
# Release binary for Linux x86_64 in target/dist/kosync_rs-linux-amd64
build-linux-amd64 glibc="2.28":
    @command -v zig >/dev/null || { echo "zig is required: brew install zig" >&2; exit 1; }
    @command -v cargo-zigbuild >/dev/null || { echo "cargo-zigbuild is required: cargo install --locked cargo-zigbuild" >&2; exit 1; }
    rustup target add x86_64-unknown-linux-gnu
    ulimit -Sn 10240 && cargo zigbuild --locked --profile dist --package kosync_rs --target x86_64-unknown-linux-gnu.{{glibc}}
    mkdir -p target/dist
    cp target/x86_64-unknown-linux-gnu/dist/kosync_rs target/dist/kosync_rs-linux-amd64
    file target/dist/kosync_rs-linux-amd64

# Restarts the service, so a brief downtime (see scripts/install-update.sh).
# Build here, upload over SSH and update the systemd service on `host`
deploy host: build-linux-amd64
    #!/usr/bin/env bash
    set -euo pipefail
    arch="$(ssh "{{host}}" uname -m)"
    if [ "$arch" != x86_64 ]; then
      echo "{{host}} is $arch, but this build is for x86_64" >&2
      exit 1
    fi
    upload_dir="$(ssh "{{host}}" mktemp -d /tmp/kosync-deploy.XXXXXX)"
    trap 'ssh "{{host}}" rm -rf "$upload_dir"' EXIT
    scp target/dist/kosync_rs-linux-amd64 scripts/install-update.sh "{{host}}:$upload_dir/"
    # -t gives sudo a terminal in case it asks for a password.
    ssh -t "{{host}}" bash "$upload_dir/install-update.sh" "$upload_dir/kosync_rs-linux-amd64"
