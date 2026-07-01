.PHONY: help all clean test build release release-archive lint fmt check-fmt markdownlint nixie typecheck

APP ?= pg_embedded_setup_unpriv
CARGO ?= cargo
BUILD_JOBS ?=
DIST_DIR ?= dist
RELEASE_BINARIES ?= pg_embedded_setup_unpriv pg_worker
TARGET ?=
UV ?= uv
MANIFEST_VERSION := $(strip $(shell awk '\
	/^\[package\]$$/ { in_package = 1; next } \
	/^\[/ { if (in_package) exit; next } \
	in_package && /^version[[:space:]]*=/ { \
		if (match($$0, /"([^"]+)"/)) { \
			print substr($$0, RSTART + 1, RLENGTH - 2); \
			exit; \
		} \
	}' Cargo.toml))
VERSION ?= $(MANIFEST_VERSION)
ifeq ($(strip $(VERSION)),)
$(error VERSION is empty; set [package].version in Cargo.toml or pass VERSION explicitly)
endif
CLIPPY_FLAGS ?= --all-targets --all-features -- -D warnings
RUSTDOC_FLAGS ?= --cfg docsrs -D warnings
MDLINT ?= markdownlint-cli2
NIXIE ?= nixie

build: ## Build debug binary
	$(CARGO) build $(BUILD_JOBS) --bin "$(APP)"

release: ## Build release binaries
	$(CARGO) build $(BUILD_JOBS) --release $(foreach bin,$(RELEASE_BINARIES),--bin $(bin))

all: check-fmt lint test ## Perform all commit gate checks

clean: ## Remove build artifacts
	$(CARGO) clean
	rm -rf "$(DIST_DIR)"

test: ## Run tests with warnings treated as errors
	RUSTFLAGS="-D warnings" $(CARGO) nextest run --all-targets --all-features $(BUILD_JOBS)
	RUSTFLAGS="-D warnings" $(CARGO) nextest run --tests --workspace --no-default-features --features dev-worker $(BUILD_JOBS)

release-archive: ## Package release binaries for cargo-binstall
	@test -n "$(TARGET)" || (echo "TARGET is required" >&2; exit 1)
	@test "$(MANIFEST_VERSION)" = "$(VERSION)" || \
		(echo "VERSION ($(VERSION)) must match Cargo.toml package version ($(MANIFEST_VERSION))" >&2; exit 1)
	$(UV) run --script scripts/release_archive.py "$(TARGET)" \
		--release-version "$(VERSION)" \
		--dist-dir "$(DIST_DIR)" \
		--cargo "$(CARGO)" \
		$(if $(BUILD_JOBS),--build-jobs "$(BUILD_JOBS)") \
		$(foreach bin,$(RELEASE_BINARIES),--binary $(bin))

lint: ## Run Clippy with warnings denied
	RUSTDOCFLAGS="$(RUSTDOC_FLAGS)" $(CARGO) doc --workspace --no-deps $(BUILD_JOBS)
	$(CARGO) clippy $(CLIPPY_FLAGS)

typecheck: ## Typecheck the workspace
	$(CARGO) check --workspace --all-targets --all-features $(BUILD_JOBS)

fmt: ## Format Rust and Markdown sources
	$(CARGO) fmt --all
	mdformat-all

check-fmt: ## Verify formatting
	$(CARGO) fmt --all -- --check

markdownlint: ## Lint Markdown files
	$(MDLINT) "**/*.md"

nixie: ## Validate Mermaid diagrams
	nixie --no-sandbox

help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | \
	awk 'BEGIN {FS=":"; printf "Available targets:\n"} {printf "  %-20s %s\n", $$1, $$2}'
