.PHONY: build test clippy fmt check example

build:
	cargo build --lib --features ble,serde

test:
	cargo test --lib --features ble,serde

clippy:
	cargo clippy --lib --all-targets --features ble,serde -- -D warnings

fmt:
	cargo fmt --all

# Every feature combination must build.
check:
	cargo build --lib
	cargo build --lib --features serde
	cargo build --lib --features ble
	cargo build --lib --features ble,serde
	cargo build --lib --target x86_64-pc-windows-gnu --features ble

example:
	cargo run --example monitor --features ble -- $(club)
