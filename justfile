# List available recipes
default:
    @just --list

# Run all tests
test:
    cargo test --workspace

# Build all crates
build:
    cargo build --workspace

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Fix formatting
fmt:
    cargo fmt --all

# Run clippy
lint:
    cargo clippy --workspace -- -D warnings

# Run all CI checks (fmt, lint, test, build)
ci: fmt-check lint test build

# Install wsctl binary
install:
    cargo install --path .

# Audit disk usage — shows space consumed by toolchains, caches, apps
audit:
    cargo run -- audit

# Interactive TUI to select and run disk cleanups
cleanup:
    cargo run -- cleanup
