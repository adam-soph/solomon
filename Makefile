# Makefile for the solomon HolyC compiler/interpreter.
#
# Wraps Cargo to build release binaries for several OS/architecture targets.
#
# Quick start:
#   make                        # build for the host machine (native)
#   make targets                # install the rustup std for every target
#   make all                    # build every target in TARGETS
#   make dist                   # build all + collect binaries into dist/
#   make aarch64-apple-darwin   # build one specific target
#   make macos-universal        # arm64 + x86_64 fat binary (macOS host)
#
# Building for an OS other than the host needs a cross linker/toolchain. The
# simplest way is the `cross` tool (Docker-based, https://github.com/cross-rs/cross):
#   cargo install cross
#   make all CARGO=cross
# A native macOS host can build both Apple targets directly after `make targets`.

BIN         := solomon
CARGO       ?= cargo
CARGO_FLAGS ?= --release --locked
PROFILE_DIR := release
DIST        := dist

# OS/arch targets to build. Override on the command line, e.g.
#   make all TARGETS="x86_64-unknown-linux-gnu aarch64-apple-darwin"
TARGETS ?= \
	aarch64-apple-darwin \
	x86_64-apple-darwin \
	x86_64-unknown-linux-gnu \
	aarch64-unknown-linux-gnu \
	x86_64-unknown-linux-musl \
	x86_64-pc-windows-gnu \
	i686-pc-windows-gnu

# Apple targets used to build the universal (fat) binary.
MACOS_TARGETS := aarch64-apple-darwin x86_64-apple-darwin

.PHONY: all native targets dist macos-universal test fmt clean help $(TARGETS)

.DEFAULT_GOAL := native

# Build for the host machine.
native:
	$(CARGO) build $(CARGO_FLAGS)

# Build every configured target.
all: $(TARGETS)

# One phony rule per triple, e.g. `make x86_64-pc-windows-gnu`.
$(TARGETS):
	$(CARGO) build $(CARGO_FLAGS) --target $@

# Install the rustup standard library for every target (run once per machine).
targets:
	rustup target add $(TARGETS)

# Collect built binaries into dist/, named per target (.exe on Windows).
# Targets that haven't been built yet are skipped with a note.
dist: all
	@mkdir -p $(DIST)
	@for t in $(TARGETS); do \
		ext=""; case $$t in *windows*) ext=".exe";; esac; \
		src="target/$$t/$(PROFILE_DIR)/$(BIN)$$ext"; \
		if [ -f "$$src" ]; then \
			cp "$$src" "$(DIST)/$(BIN)-$$t$$ext"; \
			echo "  packaged $(DIST)/$(BIN)-$$t$$ext"; \
		else \
			echo "  SKIP $$t (not built: $$src missing)"; \
		fi; \
	done

# macOS universal binary (arm64 + x86_64) via lipo. macOS host only.
macos-universal: $(MACOS_TARGETS)
	@mkdir -p $(DIST)
	lipo -create -output $(DIST)/$(BIN)-macos-universal \
		target/aarch64-apple-darwin/$(PROFILE_DIR)/$(BIN) \
		target/x86_64-apple-darwin/$(PROFILE_DIR)/$(BIN)
	@echo "  created $(DIST)/$(BIN)-macos-universal"

# Run the test suite on the host.
test:
	$(CARGO) test

# Format the source.
fmt:
	$(CARGO) fmt

clean:
	$(CARGO) clean
	rm -rf $(DIST)

help:
	@echo "solomon build targets:"
	@echo "  make / make native     build for the host machine"
	@echo "  make targets           rustup target add every triple"
	@echo "  make all               build every target in TARGETS"
	@echo "  make <triple>          build one target (e.g. make aarch64-apple-darwin)"
	@echo "  make dist              build all + collect binaries into $(DIST)/"
	@echo "  make macos-universal   lipo arm64 + x86_64 into one macOS binary"
	@echo "  make test              run the test suite"
	@echo "  make clean             cargo clean + remove $(DIST)/"
	@echo ""
	@echo "  CARGO=cross            cross-compile via the 'cross' tool (Docker)"
	@echo "  TARGETS=\"...\"          override the target list"
	@echo ""
	@echo "Configured targets:"
	@for t in $(TARGETS); do echo "    $$t"; done
