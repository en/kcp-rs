default: release

.PHONY: all

all: build test

build:
	cargo build

release:
	cargo build --release

test:
	cargo test -- --nocapture

clean:
	cargo clean
