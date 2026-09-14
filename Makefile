.PHONY: help all clean test test-loom test-scripts build release \
	release-archive lint fmt \
	check-fmt markdownlint nixie spelling typecheck

APP ?= pg_embedded_setup_unpriv
CARGO ?= cargo
BUILD_JOBS ?=
DIST_DIR ?= dist
RELEASE_BINARIES ?= pg_embedded_setup_unpriv pg_worker
TARGET ?=
UV ?= uv
UV_ENV = UV_CACHE_DIR=.uv-cache UV_TOOL_DIR=.uv-tools
CUPRUM_VERSION ?= 0.1.0
CYCLOPTS_VERSION ?= 4.19.0
TYPOS_CONFIG_BUILDER_VERSION ?= v0.1.1
TYPOS_CONFIG_BUILDER = $(UV_ENV) $(UV) tool run --python 3.14 --from \
	"git+https://github.com/leynos/typos-config-builder.git@$(TYPOS_CONFIG_BUILDER_VERSION)" \
	typos-config-builder
CMD_MOX_VERSION ?= 0.2.0
HYPOTHESIS_VERSION ?= 6.167.1
PYTEST_VERSION ?= 9.0.2
PYYAML_VERSION ?= 6.0.3
SCRIPT_PY_TESTS := scripts/tests/test_release_archive.py \
	scripts/tests/test_release_archive_failures.py \
	scripts/tests/test_release_workflow_contract.py \
	scripts/tests/test_timeout_ordering_contract.py \
	scripts/tests/test_timeout_reading_contract.py \
	scripts/tests/test_timeout_reading_properties.py
SCRIPT_PYTEST = $(UV_ENV) $(UV) run --no-project --python 3.13 \
	--with cmd-mox==$(CMD_MOX_VERSION) \
	--with cuprum==$(CUPRUM_VERSION) \
	--with cyclopts==$(CYCLOPTS_VERSION) \
	--with hypothesis==$(HYPOTHESIS_VERSION) \
	--with pytest==$(PYTEST_VERSION) \
	--with pyyaml==$(PYYAML_VERSION) python -m pytest
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
# Immediate assignment keeps the read-time fatal error while remaining a
# plain variable statement that static Makefile parsers can represent.
VERSION_GUARD := $(error VERSION is empty; set [package].version in Cargo.toml or pass VERSION explicitly)
endif
CLIPPY_FLAGS ?= --all-targets --all-features -- -D warnings
RUSTDOC_FLAGS ?= --cfg docsrs -D warnings
MDLINT ?= markdownlint-cli2
# `make fmt` and `make check-fmt` call mdtablefix directly. `--git` selects the
# Markdown files Git tracks and `--include-untracked` adds the untracked files
# Git does not ignore, so a new document is formatted before it is staged.
# Both modes need mdtablefix 0.6.0 or later; CI pins the version at the
# install-mdtablefix step.
MDTABLEFIX ?= mdtablefix
MDTABLEFIX_SELECT = --git --include-untracked
MDTABLEFIX_RULES = --wrap --renumber --breaks --ellipsis --fences
WHITAKER ?= whitaker
NIXIE ?= nixie
INTERROGATE ?= interrogate
INTERROGATE_EXCLUDES := --exclude .uv-cache --exclude .uv-tools
PY_DOCSTRING_COVERAGE ?= 100

build: ## Build debug binary
	$(CARGO) build $(BUILD_JOBS) --bin "$(APP)"

release: ## Build release binaries
	$(CARGO) build $(BUILD_JOBS) --release $(foreach bin,$(RELEASE_BINARIES),--bin $(bin))

all: check-fmt lint test test-scripts spelling ## Perform all commit gate checks

clean: ## Remove build artefacts
	$(CARGO) clean
	rm -rf "$(DIST_DIR)" .uv-cache .uv-tools

test: ## Run tests with warnings treated as errors
	RUSTFLAGS="-D warnings" $(CARGO) nextest run --all-targets --all-features $(BUILD_JOBS)
	RUSTFLAGS="-D warnings" $(CARGO) nextest run --tests --workspace --no-default-features --features dev-worker $(BUILD_JOBS)

test-loom: ## Run Loom concurrency tests
	$(CARGO) test --features "loom-tests" --lib -- --ignored

test-scripts: ## Run the Python release-tooling tests
	$(SCRIPT_PYTEST) $(SCRIPT_PY_TESTS) -c /dev/null --rootdir=. -p no:cacheprovider

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

lint: ## Run Clippy and the Whitaker Dylint suite with warnings denied
	$(INTERROGATE) --fail-under $(PY_DOCSTRING_COVERAGE) $(INTERROGATE_EXCLUDES) .
	RUSTDOCFLAGS="$(RUSTDOC_FLAGS)" $(CARGO) doc --workspace --no-deps $(BUILD_JOBS)
	$(CARGO) clippy $(CLIPPY_FLAGS)
# --ignore-rust-version: the Whitaker Dylint driver toolchain predates the
# rust-version of some dependencies; the repo toolchain still enforces MSRV.
	RUSTFLAGS="-D warnings" $(WHITAKER) --all -- --all-targets --all-features --ignore-rust-version

typecheck: ## Typecheck the workspace
	$(CARGO) check --workspace --all-targets --all-features $(BUILD_JOBS)

fmt: ## Format Rust and Markdown sources
	$(CARGO) fmt --all
	$(MDTABLEFIX) --in-place $(MDTABLEFIX_SELECT) $(MDTABLEFIX_RULES)
	$(MDLINT) --fix "**/*.md"

check-fmt: ## Verify formatting
	$(CARGO) fmt --all -- --check
	$(MDTABLEFIX) --check $(MDTABLEFIX_SELECT) $(MDTABLEFIX_RULES)

markdownlint: spelling ## Lint Markdown files and enforce spelling
	$(MDLINT) "**/*.md" "#.uv-cache" "#.uv-tools"

spelling: ## Enforce en-GB-oxendict spelling
	$(TYPOS_CONFIG_BUILDER) gate --repository .

nixie: ## Validate Mermaid diagrams
	nixie --no-sandbox

help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | \
	awk 'BEGIN {FS=":"; printf "Available targets:\n"} {printf "  %-20s %s\n", $$1, $$2}'
