BIN := pacpurge

.PHONY: all build test check lint fmt fmt-check release static clean

all: fmt-check lint test check

build:
	cargo build --locked

test:
	cargo test --locked

## strictrs is the primary feedback oracle (JSON report on stdout)
check:
	strictrs check .

lint:
	cargo clippy --all-targets --all-features --locked -- -D warnings

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

release:
	cargo build --release --locked

## The artefact people install: one static binary, no runtime dependencies.
static:
	rustup target add x86_64-unknown-linux-musl
	cargo release-small
	@stat -c '%s bytes  %n' target/x86_64-unknown-linux-musl/release/$(BIN)

clean:
	cargo clean
