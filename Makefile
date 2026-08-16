.PHONY: all check test lint fmt fmt-check docs audit package

all: check test

check:
	cargo check --workspace --all-targets --locked

test:
	cargo test --workspace --all-targets --locked

lint:
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

docs:
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

audit:
	cargo deny check

package:
	cargo build --release --locked --bin omarec --bin omarecd
