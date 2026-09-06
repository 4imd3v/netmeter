# NetMeter shortcuts. `make deploy` is the standard update path:
# user binary -> system binary -> daemon restart, no drift.
CARGO_BIN := $(HOME)/.cargo/bin/netmeter

install:
	cargo install --locked --path .

deploy: install
	sudo install -m755 $(CARGO_BIN) /usr/bin/netmeter
	sudo systemctl restart netmeter.service
	@echo "deployed $$(md5sum $(CARGO_BIN) | cut -d' ' -f1) -> /usr/bin/netmeter"

deb:
	cargo deb

man:
	./target/debug/netmeter manpage > man/netmeter.1

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test
