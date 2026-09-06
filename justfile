# Default recipe - show available commands
default:
    @just --list

# Install the glyph project workflow tool
install:
    cargo uninstall glyph || true
    cargo install --path crates/glyph-cli --bin glyph

# Install the glyph-cli compiler driver
install-cli:
    cargo install --path crates/glyph-cli --bin glyph-cli

# Install all CLI tools
install-all:
    cargo install --path crates/glyph-cli

# Build all crates in release mode
build:
    cargo build --release

# Run all tests
test:
    cargo test

# Run the demo
demo:
    ./demo.sh

# Run the release validation matrix (builds glyph-cli, exercises example programs)
release-matrix:
    ./scripts/release-matrix.sh

# Uninstall all glyph binaries
uninstall:
    cargo uninstall glyph glyph-cli
