.PHONY: all build dev release clean test install fmt lint docs-reference docs-check to-spec-check help

# Default target
all: build

# Build debug version
build:
	cargo build

# Build release version (optimized)
release:
	cargo build --release

# Build for a specific package
build-%:
	cargo build --package $* --release

# Install binaries to /usr/local/bin
install: release
	@echo "Installing nca to /usr/local/bin..."
	cp target/release/nca /usr/local/bin/nca
	@echo "Installed successfully!"

# Run development build
dev:
	cargo build
	@echo "Dev binaries built at target/debug/"

# Run tests
test:
	cargo test

# Run tests for a specific package
test-%:
	cargo test --package $*

# Format code
fmt:
	cargo fmt

# Lint code
lint:
	cargo clippy -- -D warnings

# Clean build artifacts
clean:
	cargo clean

# Build and show binary sizes
sizes: release
	@echo "=== Binary Sizes ==="
	@ls -lh target/release/nca 2>/dev/null || echo "Binary not found"

# Run with custom config
run-dev: dev
	./target/debug/nca

# Generate shell completions
completions:
	./target/release/nca completion bash > contrib/nca.bash
	./target/release/nca completion zsh > contrib/_nca

# Run benchmarks (if any)
bench:
	cargo bench

# Check formatting
check-fmt:
	cargo fmt -- --check

# Generate the top-level CLI reference from the current Clap help output
docs-reference:
	mkdir -p target/docs-tools
	rustc --edition=2024 tools/generate_cli_reference.rs -o target/docs-tools/generate-cli-reference
	target/docs-tools/generate-cli-reference docs/reference/cli-reference.md

# Validate documentation links, anchors, taxonomy, lifecycle metadata, compatibility pages, and CLI coverage
docs-check:
	mkdir -p target/docs-tools
	rustc --edition=2024 tools/validate_docs.rs -o target/docs-tools/validate-docs
	rustc --edition=2024 --test tools/validate_docs.rs -o target/docs-tools/validate-docs-tests
	target/docs-tools/validate-docs-tests
	target/docs-tools/validate-docs

# Validate the local to-spec skill and its filesystem/destination contract
to-spec-check:
	mkdir -p target/docs-tools
	rustc --edition=2024 tools/validate_to_spec_workflow.rs -o target/docs-tools/validate-to-spec
	rustc --edition=2024 --test tools/validate_to_spec_workflow.rs -o target/docs-tools/validate-to-spec-tests
	target/docs-tools/validate-to-spec-tests
	target/docs-tools/validate-to-spec

# Update dependencies
update:
	cargo update

# Help
help:
	@echo "Available targets:"
	@echo "  build     - Build debug version"
	@echo "  release   - Build optimized release version"
	@echo "  install   - Install binaries to /usr/local/bin"
	@echo "  dev       - Build development version"
	@echo "  test      - Run tests"
	@echo "  test-*    - Run tests for specific package"
	@echo "  fmt       - Format code"
	@echo "  lint      - Lint code"
	@echo "  clean     - Clean build artifacts"
	@echo "  sizes     - Show binary sizes"
	@echo "  completions - Generate shell completions"
	@echo "  bench     - Run benchmarks"
	@echo "  update    - Update dependencies"
	@echo "  check-fmt - Check code formatting"
	@echo "  docs-reference - Generate the top-level CLI reference"
	@echo "  docs-check - Validate documentation structure and links"
	@echo "  to-spec-check - Validate local to-spec workflow and destination safety"
