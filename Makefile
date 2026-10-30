.PHONY: build run test itest clean
build:
	cargo build --release
run: build
	./target/release/localhost config/server.conf
test:
	cargo test
itest: build
	python3 tests/integration.py
clean:
	cargo clean
