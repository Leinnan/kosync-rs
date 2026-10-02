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
deploy server:
    ssh {{server}} 'cd /opt/kosync-rs && docker compose up -d --build'
