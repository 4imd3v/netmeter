# NetMeter shortcuts. `make deploy` is the standard update path:
# build -> /usr/bin/netmeter (the ONLY binary) -> daemon restart.
# The cargo copy is deleted after copying so it cannot shadow /usr/bin in PATH:
# that shadow silently kept the shell and the GNOME extension on an older
# build while `daemon run` used the new one.
CARGO_BIN := $(HOME)/.cargo/bin/netmeter

install:
	cargo install --locked --path .

deploy: install
	sudo install -m755 $(CARGO_BIN) /usr/bin/netmeter
	rm -f $(CARGO_BIN)
	sudo systemctl restart netmeter.service
	@echo "deployed $$(md5sum /usr/bin/netmeter | cut -d' ' -f1) -> /usr/bin/netmeter"

deb:
	cargo deb

man:
	./target/debug/netmeter manpage > man/netmeter.1

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test
