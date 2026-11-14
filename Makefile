.PHONY: help all clean fetch test test-doc test-loom test-scripts \
	test-serial-group msrv build release \
	release-archive lint lint-clippy lint-whitaker fmt \
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
TYPOS_CONFIG_BUILDER_VERSION ?= v0.1.3
TYPOS_CONFIG_BUILDER = $(UV_ENV) $(UV) tool run --python 3.14 --from \
	"git+https://github.com/leynos/typos-config-builder.git@$(TYPOS_CONFIG_BUILDER_VERSION)" \
	typos-config-builder
CMD_MOX_VERSION ?= 0.2.0
HYPOTHESIS_VERSION ?= 6.167.1
PYTEST_VERSION ?= 9.0.2
PYYAML_VERSION ?= 6.0.3
SCRIPT_PY_TESTS := scripts/tests/test_cluster_sources_loader_contract.py \
	scripts/tests/test_cluster_sources_properties.py \
	scripts/tests/test_cluster_tier_coverage_contract.py \
	scripts/tests/test_markdown_gate_contract.py \
	scripts/tests/test_markdown_gate_e2e.py \
	scripts/tests/test_msrv_check.py \
	scripts/tests/test_nextest_filters_contract.py \
	scripts/tests/test_nextest_filters_properties.py \
	scripts/tests/test_nextest_tier_contract.py \
	scripts/tests/test_release_archive.py \
	scripts/tests/test_release_archive_failures.py \
	scripts/tests/test_release_workflow_contract.py \
	scripts/tests/test_runner_placement.py \
	scripts/tests/test_rustc_wrapper_contract.py \
	scripts/tests/test_serial_group_contract.py \
	scripts/tests/test_serial_inventory_properties.py \
	scripts/tests/test_workflow_reader.py \
	scripts/tests/test_timeout_exactness_contract.py \
	scripts/tests/test_timeout_ordering_contract.py \
	scripts/tests/test_timeout_reading_contract.py \
	scripts/tests/test_timeout_reading_properties.py
# Modules whose examples are collected as doctests alongside the suites.
SCRIPT_PY_DOCTESTS := scripts/msrv_check.py \
	scripts/tests/nextest_filters.py \
	scripts/tests/nextest_serial_group.py \
	scripts/tests/serial_inventory.py \
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
# The local CLI must stay on the version bundled by the pinned
# DavidAnson/markdownlint-cli2-action commit in `.github/workflows/ci.yml`.
# `scripts/tests/test_markdown_gate_contract.py` holds the pairing.
MARKDOWNLINT_CLI2_VERSION ?= 0.23.2
MDLINT ?= npx --yes markdownlint-cli2@$(MARKDOWNLINT_CLI2_VERSION)
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
# Cargo target directories hold build output the repository does not own, and
# this gate scans the whole tree. Interrogate 1.7.0 resolves each `--exclude` to
# an absolute path and skips a file when its path matches "<exclude>*" under
# fnmatch, where `*` also crosses `/`, so a pattern must end in `/*` (a bare
# `target` would also skip `target_utils.py`). `target/*` prunes the root
# directory and `*/target/*` a nested crate's. `CARGO_TARGET_DIR` and
# `CARGO_BUILD_TARGET_DIR` are excluded too, with fnmatch metacharacters in
# their values escaped. Owned Python under a directory literally named `target`
# would be skipped, so the lint recipe fails if Git tracks any.
fnmatch_escape = $(subst ?,[?],$(subst *,[*],$(subst [,[[],$(1))))
INTERROGATE_EXCLUDES := --exclude .uv-cache --exclude .uv-tools \
	--exclude 'target/*' --exclude '*/target/*' \
	$(if $(CARGO_TARGET_DIR),--exclude '$(call fnmatch_escape,$(CARGO_TARGET_DIR))/*') \
	$(if $(CARGO_BUILD_TARGET_DIR),--exclude '$(call fnmatch_escape,$(CARGO_BUILD_TARGET_DIR))/*')
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

# `nixie` and `test-loom` are deliberately outside the local gate. Nixie and
# Merman are not provisioned by local Make, and Loom is opt-in and bounded at
# ten minutes in CI. `AGENTS.md` states when each must be run by hand.
all: check-fmt markdownlint lint test test-scripts spelling test-workflow-contracts ## Perform local commit gates except nixie and test-loom (see AGENTS.md)

clean: ## Remove build artefacts
	$(CARGO) clean
	rm -rf "$(DIST_DIR)" .uv-cache .uv-tools

fetch: ## Download the locked dependencies once, so nested cargo runs offline
	$(CARGO) fetch --locked

test: fetch test-doc ## Run tests with warnings treated as errors
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

# `show-config` builds every test target before it can resolve the filters,
# so this target assigns the same RUSTFLAGS as `test` above: after `make
# test` the build is reused, and CI runs it directly after its test step.
test-serial-group: ## Ask nextest which tests the `serial` group schedules
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }-D warnings $(STANDARD_RUSTFLAGS)" $(SCRIPT_PYTEST) scripts/tests/test_nextest_serial_group_cli.py -c /dev/null --rootdir=. -p no:cacheprovider

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

lint: lint-clippy lint-whitaker ## Run Clippy and the Whitaker Dylint suite with warnings denied

lint-clippy: ## Run docstring coverage, rustdoc and Clippy with warnings denied
	@owned="$$(git ls-files -- ':(glob)target/**/*.py' ':(glob)**/target/**/*.py' 2>/dev/null)"; \
	  if [ -n "$$owned" ]; then \
	    printf '%s\n' "interrogate: tracked Python under a target/ directory is excluded from the gate; move it:" "$$owned" >&2; \
	    exit 1; \
	  fi
	$(INTERROGATE) --fail-under $(PY_DOCSTRING_COVERAGE) $(INTERROGATE_EXCLUDES) .
	RUSTDOCFLAGS="$(RUSTDOC_FLAGS)" RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }$(STANDARD_RUSTFLAGS)" $(CARGO) doc --workspace --no-deps $(BUILD_JOBS)
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }$(STANDARD_RUSTFLAGS)" $(CARGO) clippy $(CLIPPY_FLAGS)

# Whitaker builds with its own installer-managed toolchain, which must not
# inherit this repository's development flags, backend or linker, so the route
# clears them. `RUSTFLAGS` is assigned empty rather than removed: an empty
# assignment is a boundary the Concordat rule can read, and with the variable
# unset Cargo would fall back to the flags in .cargo/config.toml. Dylint reads
# the warning policy from DYLINT_RUSTFLAGS.
WHITAKER_CLEAN_ENV = env -u CARGO_ENCODED_RUSTFLAGS \
	-u CARGO_BUILD_RUSTFLAGS \
	-u CARGO_PROFILE_DEV_CODEGEN_BACKEND -u CARGO_BUILD_TARGET \
	-u CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER \
	-u CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS \
	-u CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER \
	-u CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS \
	-u CARGO_TARGET_X86_64_APPLE_DARWIN_LINKER \
	-u CARGO_TARGET_X86_64_APPLE_DARWIN_RUSTFLAGS -u CFLAGS -u LDFLAGS \
	RUSTFLAGS=

# --lib whitaker_suite: the rolling release now stages a standalone
# `string_continuation_style` library beside the aggregate suite that already
# registers that lint, and installer 0.2.11 could not prune it.
# `--all` would load both and rustc aborts with "duplicate specification of
# lint" before any source compiles (leynos/whitaker#565). Suite-only selection
# covers the same lints; installer 0.2.12 (now pinned) prunes by staged file
# name, so `--all` can be restored in a follow-up once it is shown to work.
# --fail-on-no-libraries keeps a missing suite from passing as a clean lint.
#
# --ignore-rust-version: the Whitaker Dylint driver toolchain predates the
# rust-version of some dependencies; the repo toolchain still enforces MSRV.
lint-whitaker: ## Run the Whitaker Dylint suite with clean driver inputs and warnings denied
	$(WHITAKER_CLEAN_ENV) DYLINT_RUSTFLAGS="-D warnings" $(WHITAKER) --lib whitaker_suite --fail-on-no-libraries -- --all-targets --all-features --ignore-rust-version

typecheck: ## Typecheck the workspace
	RUSTFLAGS="$${RUSTFLAGS:+$$RUSTFLAGS }$(STANDARD_RUSTFLAGS)" $(CARGO) check --workspace --all-targets --all-features $(BUILD_JOBS)

fmt: ## Format Rust and Markdown sources
	$(CARGO) fmt --all
	$(MDTABLEFIX) --in-place $(MDTABLEFIX_SELECT) $(MDTABLEFIX_RULES)
	$(MDLINT) --fix

check-fmt: ## Verify formatting
	$(CARGO) fmt --all -- --check
	$(MDTABLEFIX) --check $(MDTABLEFIX_SELECT) $(MDTABLEFIX_RULES)

markdownlint: spelling ## Lint Markdown files and enforce spelling
	$(MDLINT)

spelling: ## Enforce en-GB-oxendict spelling
	$(TYPOS_CONFIG_BUILDER) gate --repository .

nixie: ## Validate Mermaid diagrams
	nixie --no-sandbox

help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?##' $(MAKEFILE_LIST) | \
	awk 'BEGIN {FS=":"; printf "Available targets:\n"} {printf "  %-20s %s\n", $$1, $$2}'
