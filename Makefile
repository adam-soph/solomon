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
# Building for an OS other than the host needs a cross linker/toolchain. This
# Makefile uses the `cross` tool (Docker-based, https://github.com/cross-rs/cross)
# automatically for foreign-OS targets and plain `cargo` for host-OS targets, so
# `make all` does the right thing per triple. Install cross from git — the 0.2.5
# release predates rustup 1.28 and ships no Apple-silicon images:
#   cargo install cross --git https://github.com/cross-rs/cross
#   make all
# A native macOS host builds both Apple targets with cargo (after `make targets`)
# and Linux/Windows targets with cross.

BINS        := hcc hci
CARGO       ?= cargo
CROSS       ?= cross
CARGO_FLAGS ?= --release --locked
PROFILE_DIR := release
DIST        := dist

# Publishing release binaries to GitHub is done by the `Release` GitHub Actions
# workflow (.github/workflows/release.yml) — push a `v*` tag or run it from the
# Actions tab. It builds every target on a matching native runner (no local Docker),
# so there is no `make release` target. The targets below are for local builds.

# Host OS (Darwin/Linux), used to decide native cargo vs Docker-based cross.
HOST_OS := $(shell uname -s)

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

# Pick the build tool for a triple: native cargo when the target OS matches the
# host (e.g. both Apple targets on macOS), Docker-based cross otherwise.
#   $(call build_target,<triple>)
define build_target
	case "$(1)" in \
		*-apple-darwin) tgt_os=darwin;; \
		*-linux-*)      tgt_os=linux;;  \
		*-windows-*)    tgt_os=windows;; \
		*)              tgt_os=unknown;; \
	esac; \
	case "$(HOST_OS)" in Darwin) host_os=darwin;; Linux) host_os=linux;; *) host_os=unknown;; esac; \
	if [ "$$tgt_os" = "$$host_os" ]; then tool="$(CARGO)"; else tool="$(CROSS)"; fi; \
	echo "  building $(1) with $$tool"; \
	$$tool build $(CARGO_FLAGS) --target $(1)
endef

# Build for the host machine.
native:
	$(CARGO) build $(CARGO_FLAGS)

# Build every configured target.
all: $(TARGETS)

# One phony rule per triple, e.g. `make x86_64-pc-windows-gnu`.
$(TARGETS):
	@$(call build_target,$@)

# Install the rustup standard library for every target (run once per machine).
targets:
	rustup target add $(TARGETS)

# Collect built binaries into dist/, named per target (.exe on Windows).
# Targets that haven't been built yet are skipped with a note. The standard library
# is embedded in each binary at build time (`include_str!`), so the binaries are
# self-contained — there is no `lib/` to ship alongside them.
dist: all
	@mkdir -p $(DIST)
	@for t in $(TARGETS); do \
		ext=""; case $$t in *windows*) ext=".exe";; esac; \
		for b in $(BINS); do \
			src="target/$$t/$(PROFILE_DIR)/$$b$$ext"; \
			if [ -f "$$src" ]; then \
				cp "$$src" "$(DIST)/$$b-$$t$$ext"; \
				echo "  packaged $(DIST)/$$b-$$t$$ext"; \
			else \
				echo "  SKIP $$b for $$t (not built: $$src missing)"; \
			fi; \
		done; \
	done

# macOS universal binaries (arm64 + x86_64) via lipo. macOS host only.
macos-universal: $(MACOS_TARGETS)
	@mkdir -p $(DIST)
	@for b in $(BINS); do \
		lipo -create -output $(DIST)/$$b-macos-universal \
			target/aarch64-apple-darwin/$(PROFILE_DIR)/$$b \
			target/x86_64-apple-darwin/$(PROFILE_DIR)/$$b; \
		echo "  created $(DIST)/$$b-macos-universal"; \
	done

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
	@echo "hcc build targets:"
	@echo "  make / make native     build for the host machine"
	@echo "  make targets           rustup target add every triple"
	@echo "  make all               build every target in TARGETS"
	@echo "  make <triple>          build one target (e.g. make aarch64-apple-darwin)"
	@echo "  make dist              build all + collect binaries into $(DIST)/"
	@echo "  make macos-universal   lipo arm64 + x86_64 into one macOS binary"
	@echo "  make test              run the test suite"
	@echo "  make clean             cargo clean + remove $(DIST)/"
	@echo ""
	@echo "  foreign-OS targets build with 'cross' (Docker) automatically;"
	@echo "  host-OS targets build with cargo. Override with CROSS=... / CARGO=..."
	@echo "  TARGETS=\"...\"          override the target list"
	@echo ""
	@echo "Configured targets:"
	@for t in $(TARGETS); do echo "    $$t"; done
