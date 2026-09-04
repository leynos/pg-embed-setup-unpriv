# pg_embedded_setup_unpriv developer guide

This guide captures contributor-focused notes for maintaining the library. It
complements the user guide and omits consumer-facing usage details.

Consumer integrations should start with `docs/users-guide.md` for
consumer-facing guidance.

## Test coverage notes

- Unit and behavioural tests assert that `postmaster.pid` disappears after
  `TestCluster` teardown, demonstrating that no orphaned processes remain.
- Behavioural tests driven by `rstest-bdd` exercise both privilege branches to
  guard against regressions in ownership or permission handling.
- Property tests exercise lifecycle invariants that do not depend on a
  particular thread schedule, including repeated cleanup, partial setup
  cleanup, cleanup-mode relationships, and pure bootstrap path preparation.
- Behavioural suites coordinate via a shared lock file on Unix and an atomic
  lock directory on non-Unix platforms, so concurrent test binaries do not
  contend over PostgreSQL setup or cache directories. The non-Unix lock keeps a
  short owner grace window for missing, malformed, or unreadable owner files,
  so a competing process cannot delete a newly created lock while the owner is
  still being recorded.

## Feature coverage in CI

The default feature set keeps Diesel optional for consumers, while `make test`
enables `--all-features` so the Diesel helpers are exercised by smoke tests. CI
also runs a Linux matrix for unprivileged and root execution. The root variant
invokes the test suite under `sudo` so root-only privilege paths execute, while
the unprivileged variant continues to collect coverage.

macOS and Windows CI legs build the production binaries and run the
unprivileged surface tests. These legs deliberately avoid root privilege-drop
coverage, which is a Linux/Unix allowlisted path. macOS root execution fails
fast through the shared privilege-drop support predicate, while Windows follows
the in-process unprivileged path.

## Lint and formatting toolchain

The repository pins `rust-toolchain.toml` to `nightly-2026-04-25` because the
imported `rustfmt.toml` template uses unstable rustfmt options. Use the
Makefile targets rather than invoking Cargo directly so local checks and CI
exercise the same nightly formatter and Clippy policy:

```sh
make check-fmt
```

Run the complete lint gate before committing changes:

```sh
make lint
```

`make lint` runs four tiers in order, each gating the next:

1. `interrogate --fail-under 100 .` — Python docstring coverage at 100%.
2. `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS` set to deny warnings.
3. `cargo clippy --all-targets --all-features -- -D warnings`.
4. `$(WHITAKER) --all -- --all-targets --all-features`, the Whitaker Dylint
   suite, with `RUSTFLAGS="-D warnings"` denying lint warnings. Key lints
   include `module_max_lines` (a 400-line cap per module) and
   `no_expect_outside_tests` (bans `.expect()`/`.unwrap()` outside test-only
   code).

Attribute macros are gone by the time a Dylint lint sees HIR, so
`no_expect_outside_tests` cannot recognize a `#[serial]`-wrapped `#[test]`
function, nor any plain helper called from one, even inside a `#[cfg(test)]`
module. Configuring `additional_test_attributes` does not help, because
`#[serial]` consumes the attribute it would match. Make such a test fallible
and propagate with `?` instead of reaching for `.expect()`; where a fallible
call is only being asserted on, run it as its own statement and assert on the
result, never inside `assert!`. `src/env/tests/thread_helpers.rs` shows the
shape: the guard threads return a `GuardThreadError`, and `join_guard_thread`
re-raises a genuine panic while surfacing an orderly failure.

CI installs the pinned `interrogate==1.7.0` uv tool before running `make lint`.
Keep the Makefile and workflow versions aligned when updating the
docstring-coverage policy.

The Whitaker suite itself is installed by the shared `install-whitaker` action,
pinned to a commit SHA, which resolves a checksum-verified release archive for
the installer version named by `WHITAKER_INSTALLER_VERSION`. The previous
inline step fell back to `cargo install --locked whitaker-installer`, building
the tool from source in CI and verifying nothing.
`tests/whitaker_install_pin.rs` keeps that arrangement in place. Note that the
installer still resolves the lint suite from the tip of the Whitaker
repository, so the lints themselves are not yet pinned; a suite change can turn
this gate red without any commit here.

## Spelling policy

Run `make spelling` to enforce en-GB-oxendict spelling with Typos 1.48.0 and
the companion phrase checker. Typos scans tracked Markdown, including hidden
paths, while the phrase checker scans all tracked UTF-8 text so prohibited
forms such as `hand-written` cannot hide in source comments or tests.

The tracked `typos.toml` is generated from the shared estate dictionary and the
narrow repository policy in `typos.local.toml`; never edit the generated file
by hand. Run `make spelling-config-write` to invoke the exact, commit-pinned
`typos-config-builder`, refresh the untracked shared-dictionary cache only when
its authority is newer, and write deterministic output. Run
`make spelling-config` to verify cache and generated-config drift.

Repository exceptions belong in `typos.local.toml` as narrow exact or full-line
patterns. Preserve upstream APIs, command-line options, formal terminology and
fixtures without adding broad accepted words that could hide ordinary prose
mistakes.

`make nixie` validates the repository's Mermaid diagrams. CI installs Nixie
1.1.0 and caches Merman CLI 0.7.0, compiling Merman with an isolated Rust
1.95.0 toolchain so the product's pinned nightly toolchain remains unchanged.

## Release process

Tagging a release with `v*` triggers `.github/workflows/release.yml`. The
workflow creates a draft GitHub release, builds native archives for Linux
`x86_64`/`aarch64`, macOS Apple Silicon/Intel, and Windows x86-64, then uploads
`pg-embed-setup-unpriv-{target}-v{version}.tgz` assets containing both
`pg_embedded_setup_unpriv` and `pg_worker`.

The release workflow invokes `scripts/release_archive.py` through `uv run`, so
Python 3.13 and the script dependencies are provisioned explicitly on every
runner. The script builds the selected production binaries, applies the Windows
`.exe` suffix when staging the archive, rejects path-like `target` and
`--binary` values before joining filesystem paths, and writes the shared
`cargo-binstall` `.tgz` layout. `Cargo.toml` exposes matching
`[package.metadata.binstall]` entries so `cargo binstall pg-embed-setup-unpriv`
can install those published assets on the supported host triples.

Pull-request CI also performs a local `cargo-binstall` install-and-run check on
Linux, macOS, and Windows using cargo-binstall 1.19.1. The release workflow
audits published asset URLs with the same pinned cargo-binstall bootstrap
before the draft release is published.

## Lifecycle verification

`proptest` cases run as part of the default unit suite and protect the
schedule-independent lifecycle guarantees. These cover idempotent cleanup,
cleanup after partial setup, dangerous cleanup path rejection, cleanup-mode
relationships, and deterministic bootstrap path preparation.

Loom-based checks are opt-in and only compile when the `loom-tests` feature is
enabled. The Loom tests are marked `#[ignore]`, and `make test` keeps them
dormant: the nextest run uses `--all-features`, while the follow-up
`cargo test` run disables default features (enabling `dev-worker` only). CI
runs the ignored library Loom models explicitly. Run the same suite locally
with:

```sh
make test-loom
```

The Loom models cover the schedule-sensitive lifecycle paths: scoped
environment state, per-template database creation, shared singleton
initialization, and shutdown-hook registration.

The scheduler budget in `src/env/loom_tests.rs` currently uses
`max_threads = 3`, `max_branches = 64`, and `preemption_bound = Some(3)`. The
three bounds jointly constrain the search space so the suite stays tractable.
Changing any of them requires justification, and may need matching CI timeout
adjustments.

Production and Loom both route `ScopedEnv` environment access through the same
guard-aware `EnvLockOps` boundary. `lock_env_mutex` acquires the lock guard,
`ensure_lock_is_clean` verifies the lock before a new outer scope uses it, and
`var_os`, `set_var`, and `remove_var` all receive the held guard so reads,
writes, and removals stay tied to the acquired environment lock. Production's
`StdEnvLock` delegates that single contract to `std::env` while holding
`ENV_LOCK`; Loom swaps in an in-memory fake environment map rather than
mutating the real process environment. This lets the model checker validate
`ScopedEnv` serialization, re-entrant depth tracking, non-empty backup/restore
bookkeeping, spawn-while-held acquisition, asymmetric scope lifetimes, and
panic-path thread-local cleanup. Loom still cannot instrument the actual
`std::env` syscalls used by production; the standard serial environment tests
cover those OS-level mutations.

## Windows shutdown hook

Windows shared-cluster cleanup uses a platform-specific shutdown hook rather
than the POSIX signal path. The hook prepares a kill-on-close Job Object for
the validated postmaster process tree and keeps direct `TerminateProcess`
traversal as the forceful fallback. The root PID from `postmaster.pid` is
verified against the live postmaster identity before any action, and
descendants are revalidated against the current root tree before job assignment
or termination, so a reused PID is not treated as part of the original cluster.

Debug tracing records Job Object preparation, identity mismatches, descendant
validation skips, assignment attempts, and termination attempts with PID and
outcome fields. These logs are intentionally low-level because shutdown hooks
run during process exit and cannot recover interactively.

The process-tree tests are example-driven rather than property-generated: the
tree collector is a finite closure over the snapshot entries, rejects cycles by
bounding ancestor traversal to the snapshot length, and validates both
termination and Job Object assignment decisions against a reused-descendant-PID
case. The serial lock tests cover missing, partial, malformed, and stale owner
states around the grace window.

## Further reading

- `tests/e2e_postgresql_embedded_diesel.rs` – example of combining the helper
  with Diesel-based integration tests while running under `root`.
