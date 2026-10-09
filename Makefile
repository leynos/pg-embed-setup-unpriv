.PHONY: help all clean test test-doc test-loom test-scripts msrv build release \
	release-archive lint fmt \
	check-fmt markdownlint nixie spelling typecheck test-workflow-contracts

APP ?= pg_embedded_setup_unpriv
CARGO ?= cargo
BUILD_JOBS ?=
DIST_DIR ?= dist
RELEASE_BINARIES ?= pg_embedded_setup_unpriv pg_worker
TARGET ?=
UV ?= uv
UV_ENV = UV_CACHE_DIR=.uv-cache UV_TOOL_DIR=.uv-tools

# The CV-005 CodeScene contracts live in shared-actions and run from a full
# commit, so a fix is a pin bump. `.github/cv005.toml` holds this repository's
# only parameters.
CV005_CONTRACTS_REF ?= 88977798a5c3bae1549afb99642529488c665276
CV005_CONTRACTS = $(UV_ENV) $(UV) tool run --python 3.13 \
	--from 'git+https://github.com/leynos/shared-actions@$(CV005_CONTRACTS_REF)\#subdirectory=packages/cv005-contracts' \
	cv005-contracts

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
SCRIPT_PY_TESTS := scripts/tests/test_msrv_check.py \
	scripts/tests/test_release_archive.py \
	scripts/tests/test_release_archive_failures.py \
	scripts/tests/test_release_workflow_contract.py \
	scripts/tests/test_runner_placement.py \
	scripts/tests/test_rustc_wrapper_contract.py \
	scripts/tests/test_workflow_reader.py \
	scripts/tests/test_timeout_exactness_contract.py \
	scripts/tests/test_timeout_ordering_contract.py \
	scripts/tests/test_timeout_reading_contract.py \
	scripts/tests/test_timeout_reading_properties.py
# Modules whose examples are collected as doctests alongside the suites.
SCRIPT_PY_DOCTESTS := scripts/msrv_check.py \
	scripts/tests/runner_placement.py \
	scripts/tests/rustc_wrapper.py \
	scripts/tests/workflow_reader.py
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

# The development build standard (concordat rule `rust-build-defaults`):
# the parallel rustc frontend and, on Linux, the mold linker. An assigned
# RUSTFLAGS replaces every `rustflags` table in .cargo/config.toml, so each
# recipe that sets it composes these onto any inherited value (CI's
# setup-rust exports one), except coverage, which stays on LLVM and the
# platform linker.
BUILD_HOST_OS := $(shell uname -s)
STANDARD_RUSTFLAGS := -Zthreads=8$(if $(filter Linux,$(BUILD_HOST_OS)), -Clink-arg=-fuse-ld=mold)

test-workflow-contracts: ## Check the CV-005 CodeScene workflow contracts
	$(CV005_CONTRACTS) check --repository .

build: ## Build debug binary
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }$(STANDARD_RUSTFLAGS)" $(CARGO) build $(BUILD_JOBS) --bin "$(APP)"

release: ## Build release binaries
	RUSTFLAGS="$${RUSTFLAGS-}" $(CARGO) build $(BUILD_JOBS) --release $(foreach bin,$(RELEASE_BINARIES),--bin $(bin))

all: check-fmt lint test test-scripts spelling test-workflow-contracts ## Perform all commit gate checks

clean: ## Remove build artefacts
	$(CARGO) clean
	rm -rf "$(DIST_DIR)" .uv-cache .uv-tools

test: test-doc ## Run tests with warnings treated as errors
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }-D warnings $(STANDARD_RUSTFLAGS)" $(CARGO) nextest run --all-targets --all-features $(BUILD_JOBS)
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }-D warnings $(STANDARD_RUSTFLAGS)" $(CARGO) nextest run --tests --workspace --no-default-features --features dev-worker $(BUILD_JOBS)

# nextest cannot run documentation examples, so `make test` above never
# compiled one: every `# Examples` block in this crate was unchecked prose
# until this target existed.
test-doc: ## Run the documentation examples
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }-D warnings $(STANDARD_RUSTFLAGS)" $(CARGO) test --doc --all-features $(BUILD_JOBS)

test-loom: ## Run Loom concurrency tests
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }$(STANDARD_RUSTFLAGS)" $(CARGO) test --features "loom-tests" --lib -- --ignored

test-scripts: ## Run the Python release-tooling tests
	$(SCRIPT_PYTEST) $(SCRIPT_PY_TESTS) -c /dev/null --rootdir=. -p no:cacheprovider
	$(SCRIPT_PYTEST) --doctest-modules $(SCRIPT_PY_DOCTESTS) -c /dev/null --rootdir=. -p no:cacheprovider

msrv: ## Build at the declared rust-version against a lockfile resolved for it
	$(UV) run --no-project --python 3.13 scripts/msrv_check.py

release-archive: ## Package release binaries for cargo-binstall
	@test -n "$(TARGET)" || (echo "TARGET is required" >&2; exit 1)
	@test "$(MANIFEST_VERSION)" = "$(VERSION)" || \
		(echo "VERSION ($(VERSION)) must match Cargo.toml package version ($(MANIFEST_VERSION))" >&2; exit 1)
	RUSTFLAGS="$${RUSTFLAGS-}" $(UV) run --script scripts/release_archive.py "$(TARGET)" \
		--release-version "$(VERSION)" \
		--dist-dir "$(DIST_DIR)" \
		--cargo "$(CARGO)" \
		$(if $(BUILD_JOBS),--build-jobs "$(BUILD_JOBS)") \
		$(foreach bin,$(RELEASE_BINARIES),--binary $(bin))

lint: ## Run Clippy and the Whitaker Dylint suite with warnings denied
	$(INTERROGATE) --fail-under $(PY_DOCSTRING_COVERAGE) $(INTERROGATE_EXCLUDES) .
	RUSTDOCFLAGS="$(RUSTDOC_FLAGS)" RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }$(STANDARD_RUSTFLAGS)" $(CARGO) doc --workspace --no-deps $(BUILD_JOBS)
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }$(STANDARD_RUSTFLAGS)" $(CARGO) clippy $(CLIPPY_FLAGS)
# --ignore-rust-version: the Whitaker Dylint driver toolchain predates the
# rust-version of some dependencies; the repo toolchain still enforces MSRV.
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }-D warnings $(STANDARD_RUSTFLAGS)" $(WHITAKER) --all -- --all-targets --all-features --ignore-rust-version

typecheck: ## Typecheck the workspace
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }$(STANDARD_RUSTFLAGS)" $(CARGO) check --workspace --all-targets --all-features $(BUILD_JOBS)

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
