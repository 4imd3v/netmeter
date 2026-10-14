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

# `man` was a silent no-op: the target name collided with the man/ directory, so
# make saw an up-to-date "file" and never ran the recipe (the ./target/debug path
# was wrong too on shared-target-dir setups). .PHONY + cargo run fixes both.
.PHONY: install deploy deb man check

man:
	cargo run --quiet -- manpage > /tmp/netmeter.1.tmp && mv /tmp/netmeter.1.tmp man/netmeter.1

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test
