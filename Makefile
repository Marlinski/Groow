# Groow.
#
# Two languages and a container, so there are a few things to build. Nothing here does work of
# its own: each target runs the thing you would have run by hand, so you can always drop the
# make and do it directly.
#
#   make            what you can do
#   make build      the core, the harness, the window
#   make body       the container Groow lives in
#   make test       everything that can be checked without a GPU
#   make start      wake it
#
# Nothing here ever touches state/ or home/. Those are the creature, not the build.

CARGO    := cargo
MANIFEST := rust/Cargo.toml
PYTHON   := .venv/bin/python
BIN      := rust/target/release/groow

.DEFAULT_GOAL := help
.PHONY: help build body proto python install all test test-rust test-judge check fmt \
        start stop ui logs shell status clean distclean

help: ## what you can do
	@echo "Groow"
	@echo
	@grep -hE '^[a-z-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | \
	  awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[1m%-12s\033[0m %s\n", $$1, $$2}'
	@echo
	@echo "state/ and home/ are the creature and are never touched by any of these."

# ---------------------------------------------------------------- building
build: ## the core, the harness and the window, in release
	$(CARGO) build --release --manifest-path $(MANIFEST)

$(BIN): build

proto: ## regenerate the Python side of the protocol from nervous_system/proto
	./nervous_system/generate.sh

python: ## install the neuro package: the brain and the learning passes
	@test -x $(PYTHON) || (echo "no .venv here. uv venv --python 3.12 .venv" && exit 1)
	uv pip install --python $(PYTHON) -e .

body: ## build the container, which carries both sides
	docker compose build groow

all: build python body ## everything

install: build ## put groow on your PATH
	$(CARGO) install --path rust/crates/cli

# ---------------------------------------------------------------- checking
test: test-rust test-judge ## everything that can be checked without a GPU

test-rust: ## the Rust tests
	$(CARGO) test --manifest-path $(MANIFEST)

test-judge: ## whether the judge still reads an exchange the way a person would
	@test -x $(PYTHON) || (echo "no .venv here; make python first" && exit 1)
	$(PYTHON) -m neuro.limbic.calibrate

check: ## compiler warnings and lints
	$(CARGO) clippy --manifest-path $(MANIFEST) --all-targets -- -D warnings

fmt: ## format the Rust
	$(CARGO) fmt --manifest-path $(MANIFEST)

# ---------------------------------------------------------------- living with it
start: $(BIN) ## wake it, in its sandbox
	./groow start

stop: ## put it back to sleep
	./groow stop

ui: $(BIN) ## the window
	./groow ui

logs: ## follow what its body is doing
	./groow logs

shell: ## a shell in its home, as the mind
	./groow shell

status: $(BIN) ## what it is doing
	./groow status

# ---------------------------------------------------------------- tidying
clean: ## build output only
	$(CARGO) clean --manifest-path $(MANIFEST)

distclean: clean ## build output and the container image; still not the creature
	-docker image rm groow:latest
