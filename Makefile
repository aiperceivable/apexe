.PHONY: setup check check-chars fmt-check build test test-scripts test-examples lint fmt clean

PYTHON ?= python3
CORPUS ?= $(if $(APEXE_TEST_CORPUS),$(APEXE_TEST_CORPUS),../cli-permissions/overlays)
TARGET_DIR ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)
RUNTIME_REPORT_DIR ?= $(TARGET_DIR)/runtime-validation
export APEXE_RUNTIME_REPORT_DIR := $(abspath $(RUNTIME_REPORT_DIR))

# One-time dev environment setup
setup:
	@echo "Installing apdev-rs..."
	@command -v apdev-rs >/dev/null 2>&1 || cargo install apdev-rs
	@echo "Installing git pre-commit hook..."
	@mkdir -p .git/hooks
	@cp hooks/pre-commit .git/hooks/pre-commit
	@chmod +x .git/hooks/pre-commit
	@echo "Done! Development environment is ready."

# Run all checks (same as pre-commit hook)
check: fmt-check lint check-chars build test test-scripts test-examples

build:
	cargo build --all-features --examples --locked

check-chars:
	apdev-rs check-chars src/

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --all-features --locked
	cargo test --all-features --examples --locked

test-scripts:
	$(PYTHON) -m unittest discover -s scripts/tests -v

test-examples:
	$(PYTHON) scripts/validate_examples.py --apexe "$(TARGET_DIR)/debug/apexe" --corpus "$(CORPUS)" --report "$(RUNTIME_REPORT_DIR)/examples.json"

fmt:
	cargo fmt --all

clean:
	cargo clean
