# muxa workspace tasks — invoke with `just <recipe>`.
# Run `just` (no args) for the list.

# Show available recipes when called with no args.
default:
    @just --list

# Format the workspace (writes changes).
fmt:
    cargo fmt --all

# Check formatting without writing changes (CI-friendly).
fmt-check:
    cargo fmt --all -- --check

# Run clippy across the workspace, treating warnings as errors.
lint:
    cargo clippy --workspace --all-targets --no-deps -- -D warnings

# Run the default-feature test suite.
test:
    cargo test --workspace

# Run the muxa-pgmq capability composition tests with both backends.
test-pgmq:
    cargo test -p muxa-pgmq --features sqlx,diesel-async --tests

# fmt-check + lint + tests + pgmq capability tests. What CI should run.
check: fmt-check lint test test-pgmq
    @echo "✔ check passed"

# Build the workspace in dev mode.
build:
    cargo build --workspace

# Build the workspace in release mode.
build-release:
    cargo build --workspace --release

# Run the full hello demo (otel + sqlite + web, in-memory).
hello *ARGS:
    cargo run -p muxa --example hello --features sqlite -- {{ARGS}}

# Run the minimal web-only example (no DB, no auth).
web-only *ARGS:
    cargo run -p muxa --example web_only -- {{ARGS}}

# Build + open docs in your browser.
doc:
    cargo doc --workspace --no-deps --open

# Update the lockfile.
update:
    cargo update

# Remove the target directory.
clean:
    cargo clean
