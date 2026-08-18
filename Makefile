.PHONY: build test clippy fmt check example

build:
	cargo build --lib --features bluez,serde

test:
	cargo test --lib --features bluez,serde

clippy:
	cargo clippy --lib --all-targets --features bluez,serde -- -D warnings

fmt:
	cargo fmt --all

# Every feature combination must build.
check:
	cargo build --lib
	cargo build --lib --features serde
	cargo build --lib --features bluez
	cargo build --lib --features btleplug
	cargo build --lib --features bluez,btleplug,serde
	cargo build --lib --target x86_64-pc-windows-gnu --features btleplug

example:
	cargo run --example monitor --features bluez -- $(club)
